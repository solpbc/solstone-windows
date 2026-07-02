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
//   --skip-tier1     run only the deterministic health/render gate.
//   --tier1-only     run only the bounded advisory FlaUI/UIA native-chrome pass.
//   --open-journal-only
//                    invoke tray -> Open Journal for the live journal-window harness.
//                    This is only a trigger; the PowerShell/mock transcript decides.
//   --selftest       run the pure decision logic (contract parse, token match,
//                    fail-inject decision) with no live target -> exit 0/non-0.

using System;
using System.IO;
using System.Linq;
using System.Net;
using System.Threading;
using System.Threading.Tasks;
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
        private const int ViewNotRendered = 11;

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

                if (opts.OpenJournalOnly)
                {
                    if (OpenJournalTrigger(contract))
                    {
                        Log("OK (trigger only): tray -> Open Journal invoked");
                    }
                    else
                    {
                        Log("WARN (trigger only): tray -> Open Journal was not reachable; transcript gate will decide");
                    }
                    return Ok;
                }

                if (opts.Tier1Only)
                {
                    var tier1 = Tier1InteractionAdvisory(contract, opts.Tier1TimeoutSecs);
                    if (tier1 == Ok)
                    {
                        Log("OK (Tier 1 advisory): tray -> Open Settings -> Settings window resolved by AutomationId");
                    }
                    else
                    {
                        Log("WARN (Tier 1 advisory): native-chrome interaction did not complete");
                    }
                    // UIA can leave COM helper threads behind after an advisory
                    // timeout; tier1-only is a wrapper mode, so terminate hard.
                    Environment.Exit(Ok);
                    return Ok;
                }

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

                // Default happy path: wait for observing (Tier 0).
                if (!PollUntil(opts, dump => Health.AppState(dump) == observing))
                {
                    Log($"FAIL: app_state never reached '{observing}' within {opts.TimeoutSecs}s (last = '{LastState}')");
                    return TimeoutNotObserving;
                }
                Log("OK (Tier 0): health dump reports observing");

                // Tier R render gate (LOAD-BEARING): the view opened via --open-view
                // must report it rendered OUR UI -- the per-view beacon
                // (`views.<label>` == rendered) on /healthz, which only our own
                // renderer can fire and only after stamping the contract window root.
                // A webview that loads the dev-server error page or a blank window
                // never fires it, so this catches the 0.2.0 custom-protocol class that
                // the old "window resolved" Tier-1 missed. Launch the app with
                // `--open-view <view>` so the view actually opens.
                var rendered = contract.RenderedToken();
                if (!PollUntil(opts, dump => Health.ViewState(dump, opts.RenderView) == rendered))
                {
                    Log($"FAIL (Tier R): view '{opts.RenderView}' never reached '{rendered}' within {opts.TimeoutSecs}s -- the webview did not load our UI (is the app launched with --open-view {opts.RenderView}? is --features custom-protocol set?)");
                    return ViewNotRendered;
                }
                Log($"OK (Tier R): view '{opts.RenderView}' rendered our UI ('{rendered}')");

                // Tier 1 native-chrome (ADVISORY): --open-view already opened the view
                // deterministically, so a tray-menu miss here is informational, never
                // fatal. The gate is Tier 0 (observing) + Tier R (our UI rendered).
                if (opts.SkipTier1)
                {
                    Log("SKIP (Tier 1 advisory): native-chrome interaction is separate from the gate");
                }
                else if (Tier1InteractionAdvisory(contract, opts.Tier1TimeoutSecs) == Ok)
                {
                    Log("OK (Tier 1): tray -> Open Settings -> Settings window resolved by AutomationId");
                }
                else
                {
                    Log("WARN (Tier 1, advisory): native-chrome interaction did not complete; Tier-0 + Tier-R are the gate");
                }
                return Ok;
            }
            catch (Exception ex)
            {
                Log("EXCEPTION");
                Log(ex.ToString());
                return Exception;
            }
        }

        private static int Tier1InteractionAdvisory(Contract contract, int timeoutSecs)
        {
            var boundedTimeout = Math.Max(5, timeoutSecs);
            var task = Task.Run(() => Tier1Interaction(contract));
            try
            {
                if (!task.Wait(TimeSpan.FromSeconds(boundedTimeout)))
                {
                    Log($"Tier 1: timed out after {boundedTimeout}s");
                    return Tier1WindowNotFound;
                }
                return task.Result;
            }
            catch (AggregateException ex)
            {
                Log("Tier 1: exception during UIA interaction");
                Log(ex.GetBaseException().ToString());
                return Tier1WindowNotFound;
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
            var settingsRootId = contract.AutomationId("settings.window.root");
            if (settingsRootId == null) return ContractIdMissing;

            using (var automation = new UIA3Automation())
            {
                var desktop = automation.GetDesktop();

                if (!InvokeTrayMenuItem(contract, desktop, "tray.menu.openSettings", "Open Settings"))
                {
                    return Tier1WindowNotFound;
                }

                var window = FindByAutomationId(desktop, settingsRootId, TimeSpan.FromSeconds(5));
                if (window == null)
                {
                    Log($"Tier 1: Settings window '{settingsRootId}' did not appear after Open Settings");
                    return Tier1WindowNotFound;
                }
                return Ok;
            }
        }

        private static bool OpenJournalTrigger(Contract contract)
        {
            using (var automation = new UIA3Automation())
            {
                return InvokeTrayMenuItem(
                    contract,
                    automation.GetDesktop(),
                    "tray.menu.openJournal",
                    "Open Journal");
            }
        }

        private static bool InvokeTrayMenuItem(Contract contract, AutomationElement desktop, string contractKey, string label)
        {
            var id = contract.AutomationId(contractKey);
            if (id == null)
            {
                Log($"Tier 1: contract key '{contractKey}' is missing");
                return false;
            }

            // The tray context menu, once open, is a top-level popup; find the
            // menu item by AutomationId. Tray automation varies by Windows build -
            // if the item isn't reachable headlessly the caller reports honestly.
            var item = FindByAutomationId(desktop, id, TimeSpan.FromSeconds(3));
            if (item == null)
            {
                Log($"Tier 1: '{id}' not on the UIA surface (tray menu may need interactive opening on this build)");
                return false;
            }

            try { item.AsMenuItem()?.Invoke(); return true; }
            catch { try { item.Click(); return true; } catch { Log($"Tier 1: could not invoke '{label}'"); return false; } }
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
            failures += Expect("journal tray id lookup", c.AutomationId("tray.menu.openJournal") == "tray.menu.openJournal");
            failures += Expect("missing id -> null", c.AutomationId("does.not.exist") == null);

            var observingDump = "{\"app_state\":\"observing\",\"sources\":[]}";
            var startingDump = "{\"app_state\":\"starting\",\"sources\":[]}";
            failures += Expect("app_state extract (observing)", Health.AppState(observingDump) == "observing");
            failures += Expect("app_state extract (starting)", Health.AppState(startingDump) == "starting");

            // Tier-R render-gate logic: the rendered token + views extraction.
            failures += Expect("rendered token", c.RenderedToken() == "rendered");
            var renderedDump = "{\"app_state\":\"observing\",\"views\":{\"settings\":\"rendered\"}}";
            var pendingDump = "{\"app_state\":\"observing\",\"views\":{\"settings\":\"pending\"}}";
            var noViewsDump = "{\"app_state\":\"observing\"}";
            failures += Expect("view state (rendered)", Health.ViewState(renderedDump, "settings") == "rendered");
            failures += Expect("view state (pending)", Health.ViewState(pendingDump, "settings") == "pending");
            failures += Expect("view state (no views map -> null)", Health.ViewState(noViewsDump, "settings") == null);
            failures += Expect("view state (absent key -> null)", Health.ViewState(renderedDump, "about") == null);

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
            return "{\"_generated\":\"x\",\"automation_ids\":{\"settings.window.root\":\"settings.window.root\","
                 + "\"tray.menu.openJournal\":\"tray.menu.openJournal\"},"
                 + "\"state_tokens\":{\"app_phase\":[\"error\",\"idle\",\"observing\",\"paused\",\"starting\"],"
                 + "\"view_render_state\":[\"pending\",\"rendered\"]}}";
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
        public bool SkipTier1;
        public bool Tier1Only;
        public bool OpenJournalOnly;
        public int Tier1TimeoutSecs = 15;
        public string RenderView = "settings";

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
                    case "--skip-tier1": o.SkipTier1 = true; break;
                    case "--tier1-only": o.Tier1Only = true; break;
                    case "--open-journal-only": o.OpenJournalOnly = true; break;
                    case "--tier1-timeout-secs": o.Tier1TimeoutSecs = int.Parse(Next(args, ref i)); break;
                    case "--render-view": o.RenderView = Next(args, ref i); break;
                    default:
                        Console.Error.WriteLine($"[driver] unknown arg '{args[i]}'");
                        Console.Error.WriteLine("usage: solstone-driver --contract <path> [--health-url <url>] [--timeout-secs N] [--render-view settings|about] [--skip-tier1] [--tier1-only] [--open-journal-only] [--tier1-timeout-secs N] [--fail-inject] | --selftest");
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
        private readonly object[] _viewRenderState;

        private Contract(System.Collections.Generic.Dictionary<string, object> ids, object[] appPhase, object[] viewRenderState)
        {
            _ids = ids;
            _appPhase = appPhase;
            _viewRenderState = viewRenderState;
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
            var viewRenderState = (object[])tokens["view_render_state"];
            return new Contract(ids, appPhase, viewRenderState);
        }

        // The observing token, read from the model-derived app_phase vocabulary.
        public string ObservingToken()
        {
            var match = _appPhase.Select(o => o.ToString()).FirstOrDefault(t => t == "observing");
            if (match == null) throw new InvalidDataException("contract state_tokens.app_phase has no 'observing' token");
            return match;
        }

        // The "rendered" token, read from the model-derived view_render_state vocabulary.
        public string RenderedToken()
        {
            var match = _viewRenderState.Select(o => o.ToString()).FirstOrDefault(t => t == "rendered");
            if (match == null) throw new InvalidDataException("contract state_tokens.view_render_state has no 'rendered' token");
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

        // Extract the views.<view> render-state token from a health-dump JSON body
        // (null when absent/unparseable -> the poller treats it as "not yet rendered").
        public static string? ViewState(string? dumpJson, string view)
        {
            if (string.IsNullOrEmpty(dumpJson)) return null;
            try
            {
                var root = (System.Collections.Generic.Dictionary<string, object>)
                    new JavaScriptSerializer().DeserializeObject(dumpJson);
                if (!root.TryGetValue("views", out var v) ||
                    !(v is System.Collections.Generic.Dictionary<string, object> views))
                    return null;
                return views.TryGetValue(view, out var s) ? s?.ToString() : null;
            }
            catch
            {
                return null;
            }
        }
    }
}
