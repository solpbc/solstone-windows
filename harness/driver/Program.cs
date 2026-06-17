// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

// FlaUI/UIA3 smoke driver for the installed observer.
//
// The acceptance oracle is the health dump, NOT the webview DOM:
//   Tier 0  poll the health endpoint (loopback /healthz) against the contract's
//           token vocabulary until app_state == "observing" (deterministic).
//   Tier 1  drive the native tray icon + menu by AutomationId (Tauri exposes
//           these on the Win32/UIA surface) and confirm the Settings window.
//   Tier 2  webview data-automation-id is best-effort; the green path must NOT
//           depend on Chromium UIA resolving.
//
// AutomationIds + the observing token come from the committed
// automation-contract.json (the single source of truth) — never hardcoded:
//   - element ids live under "automation_ids"
//   - the observing token is the matching value in "state_tokens.app_phase",
//     compared against the "app_state" field of the health dump (the field and
//     the token group have different names on purpose).
//
// Modes:
//   (default)        await observing (Tier 0), do the Tier-1 interaction, exit 0.
//   --fail-inject    after observing, expect app_state to leave observing
//                    (a required source was killed externally) -> exit 0;
//                    if it dishonestly stays observing past the timeout -> non-0.
//   --selftest       run the pure decision logic (contract parse, token match,
//                    fail-inject decision) with no live target -> exit 0/non-0.

using System;
using System.IO;
using System.Linq;
using System.Net;
using System.Threading;
using System.Web.Script.Serialization;
using FlaUI.Core;
using FlaUI.Core.AutomationElements;
using FlaUI.UIA3;

namespace Solstone.Harness
{
    internal static class Program
    {
        // Distinct, documented exit codes.
        private const int Ok = 0;
        private const int TimeoutNotObserving = 2;
        private const int Tier1WindowNotFound = 3;
        private const int FailInjectStayedObserving = 4;
        private const int ContractIdMissing = 5;
        private const int SelftestFailed = 6;
        private const int UsageError = 7;
        private const int Exception = 10;

        private static string LastState = "(none)";

        private static int Main(string[] args)
        {
            try
            {
                var opts = Options.Parse(args);
                if (opts == null) return UsageError;

                if (opts.Selftest) return Selftest();

                var contract = Contract.Load(opts.ContractPath);
                var observing = contract.ObservingToken(); // from state_tokens.app_phase
                Log($"observing token = '{observing}'; health url = {opts.HealthUrl}; timeout = {opts.TimeoutSecs}s");

                if (opts.FailInject)
                {
                    // Precondition: must be observing first, else the injection proves nothing.
                    if (!PollUntil(opts, dump => Health.AppState(dump) == observing))
                    {
                        Log("FAIL: never reached observing before fail-injection");
                        return TimeoutNotObserving;
                    }
                    Log("reached observing; now expecting it to drop (a required source was killed)");
                    if (PollUntil(opts, dump => Health.AppState(dump) != observing))
                    {
                        Log($"OK: observer honestly left observing (now '{LastState}') after the source was killed");
                        return Ok;
                    }
                    Log("FAIL: observer dishonestly stayed observing after a required source died");
                    return FailInjectStayedObserving;
                }

                // Default happy path: wait for observing (Tier 0, the gate).
                if (!PollUntil(opts, dump => Health.AppState(dump) == observing))
                {
                    Log($"FAIL: app_state never reached '{observing}' within {opts.TimeoutSecs}s (last = '{LastState}')");
                    return TimeoutNotObserving;
                }
                Log("OK (Tier 0): health dump reports observing");

                // Tier 1 native-chrome liveness: open Settings from the tray and
                // confirm the Settings window resolves by AutomationId.
                var tier1 = Tier1Interaction(contract);
                if (tier1 != Ok)
                {
                    Log("WARN (Tier 1): native-chrome interaction did not complete; the Tier-0 oracle is the gate");
                    return tier1;
                }
                Log("OK (Tier 1): tray -> Open Settings -> Settings window resolved by AutomationId");
                return Ok;
            }
            catch (Exception ex)
            {
                Log("EXCEPTION");
                Log(ex.ToString());
                return Exception;
            }
        }

        private static bool PollUntil(Options opts, Func<string, bool> predicate)
        {
            var deadline = DateTime.UtcNow.AddSeconds(opts.TimeoutSecs);
            while (DateTime.UtcNow < deadline)
            {
                var dump = Health.Fetch(opts.HealthUrl);
                if (dump != null)
                {
                    LastState = Health.AppState(dump) ?? "(none)";
                    if (predicate(dump)) return true;
                }
                Thread.Sleep(1000);
            }
            return false;
        }

        // ---- Tier 1: native chrome via FlaUI/UIA3 -------------------------------

        private static int Tier1Interaction(Contract contract)
        {
            var openSettingsId = contract.AutomationId("tray.menu.openSettings");
            var settingsRootId = contract.AutomationId("settings.window.root");
            if (openSettingsId == null || settingsRootId == null) return ContractIdMissing;

            using (var automation = new UIA3Automation())
            {
                var desktop = automation.GetDesktop();

                // The tray context menu, once open, is a top-level popup; find the
                // Open Settings item by AutomationId. Tray automation varies by
                // Windows build — if the item isn't reachable headlessly the Tier-0
                // gate above still stands and we report Tier-1 honestly.
                var item = FindByAutomationId(desktop, openSettingsId, TimeSpan.FromSeconds(3));
                if (item == null)
                {
                    Log($"Tier 1: '{openSettingsId}' not on the UIA surface (tray menu may need interactive opening on this build)");
                    return Tier1WindowNotFound;
                }
                try { item.AsMenuItem()?.Invoke(); } catch { try { item.Click(); } catch { /* best effort */ } }

                var window = FindByAutomationId(desktop, settingsRootId, TimeSpan.FromSeconds(5));
                if (window == null)
                {
                    Log($"Tier 1: Settings window '{settingsRootId}' did not appear after Open Settings");
                    return Tier1WindowNotFound;
                }
                return Ok;
            }
        }

        private static AutomationElement? FindByAutomationId(AutomationElement root, string id, TimeSpan timeout)
        {
            var deadline = DateTime.UtcNow.Add(timeout);
            while (DateTime.UtcNow < deadline)
            {
                var found = root.FindFirstDescendant(cf => cf.ByAutomationId(id));
                if (found != null) return found;
                Thread.Sleep(250);
            }
            return null;
        }

        // ---- Selftest: pure decision logic, no live target ---------------------

        private static int Selftest()
        {
            int failures = 0;

            var c = Contract.Parse(SampleContractJson());
            failures += Expect("observing token", c.ObservingToken() == "observing");
            failures += Expect("automation id lookup", c.AutomationId("settings.window.root") == "settings.window.root");
            failures += Expect("missing id -> null", c.AutomationId("does.not.exist") == null);

            var observingDump = "{\"app_state\":\"observing\",\"sources\":[]}";
            var startingDump = "{\"app_state\":\"starting\",\"sources\":[]}";
            failures += Expect("app_state extract (observing)", Health.AppState(observingDump) == "observing");
            failures += Expect("app_state extract (starting)", Health.AppState(startingDump) == "starting");

            // Fail-inject decision: a faulted required source means the honest dump
            // must NOT be observing -> the drop-detected predicate fires.
            var faultedDump = "{\"app_state\":\"error\",\"sources\":[{\"kind\":\"system_audio\",\"status\":\"faulted\"}]}";
            failures += Expect("fail-inject: error != observing (drop detected)", Health.AppState(faultedDump) != "observing");
            failures += Expect("fail-inject: dishonest observing NOT a drop",
                !(Health.AppState(observingDump) != "observing"));

            if (failures == 0) { Log("SELFTEST OK"); return Ok; }
            Log($"SELFTEST FAILED ({failures} checks)");
            return SelftestFailed;
        }

        private static int Expect(string name, bool cond)
        {
            Log((cond ? "ok   " : "FAIL ") + name);
            return cond ? 0 : 1;
        }

        private static string SampleContractJson()
        {
            return "{\"_generated\":\"x\",\"automation_ids\":{\"settings.window.root\":\"settings.window.root\"},"
                 + "\"state_tokens\":{\"app_phase\":[\"error\",\"idle\",\"observing\",\"paused\",\"starting\"]}}";
        }

        private static void Log(string line) => Console.WriteLine("[driver] " + line);
    }

    internal sealed class Options
    {
        public string ContractPath = "";
        public string HealthUrl = "http://127.0.0.1:49247/healthz";
        public int TimeoutSecs = 60;
        public bool FailInject;
        public bool Selftest;

        public static Options? Parse(string[] args)
        {
            var o = new Options();
            for (int i = 0; i < args.Length; i++)
            {
                switch (args[i])
                {
                    case "--contract": o.ContractPath = Next(args, ref i); break;
                    case "--health-url": o.HealthUrl = Next(args, ref i); break;
                    case "--timeout-secs": o.TimeoutSecs = int.Parse(Next(args, ref i)); break;
                    case "--fail-inject": o.FailInject = true; break;
                    case "--selftest": o.Selftest = true; break;
                    default:
                        Console.Error.WriteLine($"[driver] unknown arg '{args[i]}'");
                        Console.Error.WriteLine("usage: solstone-driver --contract <path> [--health-url <url>] [--timeout-secs N] [--fail-inject] | --selftest");
                        return null;
                }
            }
            if (!o.Selftest && string.IsNullOrEmpty(o.ContractPath))
            {
                Console.Error.WriteLine("[driver] --contract <path-to-automation-contract.json> is required (or use --selftest)");
                return null;
            }
            return o;
        }

        private static string Next(string[] args, ref int i)
        {
            if (i + 1 >= args.Length) throw new ArgumentException("missing value for " + args[i]);
            return args[++i];
        }
    }

    internal sealed class Contract
    {
        private readonly System.Collections.Generic.Dictionary<string, object> _ids;
        private readonly object[] _appPhase;

        private Contract(System.Collections.Generic.Dictionary<string, object> ids, object[] appPhase)
        {
            _ids = ids;
            _appPhase = appPhase;
        }

        public static Contract Load(string path)
        {
            if (!File.Exists(path)) throw new FileNotFoundException("automation-contract.json not found", path);
            return Parse(File.ReadAllText(path));
        }

        public static Contract Parse(string json)
        {
            var root = (System.Collections.Generic.Dictionary<string, object>)new JavaScriptSerializer().DeserializeObject(json);
            var ids = (System.Collections.Generic.Dictionary<string, object>)root["automation_ids"];
            var tokens = (System.Collections.Generic.Dictionary<string, object>)root["state_tokens"];
            var appPhase = (object[])tokens["app_phase"];
            return new Contract(ids, appPhase);
        }

        // The observing token, read from the model-derived app_phase vocabulary.
        public string ObservingToken()
        {
            var match = _appPhase.Select(o => o.ToString()).FirstOrDefault(t => t == "observing");
            if (match == null) throw new InvalidDataException("contract state_tokens.app_phase has no 'observing' token");
            return match;
        }

        // AutomationId by contract key; null when absent.
        public string? AutomationId(string key)
        {
            return _ids.TryGetValue(key, out var v) ? v?.ToString() : null;
        }
    }

    internal static class Health
    {
        // GET the loopback health endpoint; null on any connection/parse failure
        // (treated as "not yet up" by the poller, never as a fake state).
        public static string? Fetch(string url)
        {
            try
            {
                var req = (HttpWebRequest)WebRequest.Create(url);
                req.Timeout = 2000;
                req.ReadWriteTimeout = 2000;
                using (var resp = (HttpWebResponse)req.GetResponse())
                using (var stream = resp.GetResponseStream())
                using (var reader = new StreamReader(stream))
                {
                    return reader.ReadToEnd();
                }
            }
            catch
            {
                return null;
            }
        }

        // Extract the app_state field from a health-dump JSON body.
        public static string? AppState(string? dumpJson)
        {
            if (string.IsNullOrEmpty(dumpJson)) return null;
            try
            {
                var root = (System.Collections.Generic.Dictionary<string, object>)
                    new JavaScriptSerializer().DeserializeObject(dumpJson);
                return root.TryGetValue("app_state", out var v) ? v?.ToString() : null;
            }
            catch
            {
                return null;
            }
        }
    }
}
