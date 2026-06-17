# Lifecycle matrix

How the observer behaves across launch, session, and power events. The single
rule underneath all of it: **state is computed and earned, never asserted.** The
shell is a pure renderer of the engine's honest `HealthDump`.

## Launch surfaces (two different things, never conflated)

| Surface | Mechanism | Used for |
|---|---|---|
| **Production** | per-user login/startup item into interactive Session 1 (autostart plugin / Velopack first-run hook) | normal operation |
| **Test** | low-privilege scheduled task (`LogonType=Interactive`) into Session 1 | the FlaUI smoke only |

## Single instance

A per-user named mutex in the `Local\` namespace = per interactive session =
"one observer per session". A second launch surfaces Settings on the first
instance and exits.

## Session / power events

| Event | Engine response | Phase |
|---|---|---|
| Operator pause | stop sources, hold | `Paused` |
| Session locked | pause (`SessionLocked`) | `Paused` |
| Session unlocked | resume | recomputed |
| Display changed | re-acquire screen source | recomputed |
| System suspending | pause (`SystemSuspending`) | `Paused` |
| System resumed | resume | recomputed |
| Required source faulted | feed to backoff/breaker; cannot claim observing | `Error` |
| No microphone present | first-class `NoInputDevice` (not a fault; not required) | unaffected |

## Phase computation

`Observing` is reachable only when the engine is ready, a run is requested, no
pause is in effect, and every *required* source (screen + system audio) is
`Active`. The microphone is best-effort: a machine with no mic
(`NoInputDevice`) still observes.

## Update status (two layers)

- **Durable `ReconciledUpdateStatus`** (persisted): last-known-available +
  last-check-outcome. The tray/UI badge reads this — earned from the Velopack
  callback.
- **Transient `UpdateActivity`** (checking/downloading/installing): never
  restored from disk.
