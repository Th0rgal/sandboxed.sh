# Orb routing settings

Settings now has Client and Routing pages. Client retains the existing connection,
local harness, appearance and execution links. Routing uses the connected backend's
existing `/api/model-routing` endpoints; it does not introduce another routing store
or change direct-to-provider local harness behavior.

Routing provides chain CRUD, ordered provider/model entries, default selection,
strip-thinking configuration, credential resolution and explicit inference tests.
The catalog includes unconnected providers and preserves custom/legacy model IDs.
Provider Health distinguishes individual accounts; Recent Fallback Events supports
chain, provider and reason filters. Health/events refresh every ten seconds while
visible. Server changes clear previous data, and late responses are discarded.
Editing survives polling and failed saves. Navigation and deletion use Orb dialogs;
no automatic inference test or cooldown reset occurs when opening the page.

Validation on 23 September 2026:

- The full Orb suite passed: 344 tests before the final server-label regression
  test was added. All nine focused routing tests pass, including that final test.
- TypeScript/Vite build and the debug macOS Orb.app bundle build passed.
- Orb's local browser client successfully loaded real production chains, account
  health and fallback events.
- Through the Orb UI, created `orb-routing-validation-20260923`, renamed it and
  resolved its Z.AI account. The production API confirmed the persisted change.
- The browser automation stalled on the first native deletion confirmation.
  Deletion and discard confirmations were replaced with existing Orb Dialog UI,
  and confirmation/cancellation were regression tested. The temporary chain was
  removed through the authenticated API; absence was verified. Existing chains
  and the default chain were not changed. No real inference probe was sent.

The rebuilt bundle is `orb/src-tauri/target/debug/bundle/macos/Orb.app`.
An already-running bundled application picks up the frontend on its next launch;
the local Vite client updates immediately. No server deployment is needed.
