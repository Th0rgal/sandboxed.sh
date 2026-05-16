# iOS Dashboard — UX Blocking Audit & Improvement Backlog

## TL;DR

The app is well-engineered at the plumbing layer (off-main JSON decode,
mission disk cache, race guards, parallel cold-start) but the **screen layer
defaults to a centered spinner over a blank view whenever data is in flight**.
Skeleton primitives (`ShimmerCard`, `ShimmerRow`) exist in the codebase and
are essentially never used. The five highest-impact freezes are:

1. **Mission switch always blanks the chat** — `switchToMission` ignores the
   on-disk cache that `loadMission` uses. Fix: mirror the cache-first path.
   (`ControlView.swift:2474-2541`)
2. **Mission load is two serial RTTs** — `getMission` then
   `getMissionEventsWithMeta`. Fix: `async let` both.
   (`ControlView.swift:1661-1697`)
3. **BackendAgentService walks 5 backends serially** for configs and agents on
   every New Mission tap. Fix: two `withTaskGroup`s.
   (`BackendAgentService.swift:51-96`)
4. **Send-message has no in-flight state** — the optimistic bubble looks
   identical to a confirmed one; slow network = re-taps and confusion.
   (`ControlView.swift:2245-2290`)
5. **Files / Workspaces / History tabs flash to full-screen spinner** on every
   open, with no cache and no skeleton.

Lower-tier but real: markdown re-parsed on every body call (#9), event replay
synchronous on main actor (#4), upload reads whole files into RAM with no
progress (#6), terminal ANSI parsed inline on each line (#19), desktop frames
converted on main actor (#20), FIDO approve has no in-flight feedback (#23a),
and several arbitrary timer-based latencies (#18, #24).

30 numbered findings below; quick-fix backlog grouped by approach in section
"Improvement backlog"; concrete patch sketches for the top items in the
appendix.

---

Audit scope: every screen in `SandboxedDashboard/` for points where slow network
or main-thread work makes the UI feel frozen, blank, or unresponsive. Findings
are grounded in specific file:line citations so each item is independently
verifiable.

App stack: SwiftUI + `@Observable`, single `APIService.shared` (15 s request
timeout, 30 s SSE inactivity timeout, off-main JSON decode via `Task.detached`),
SSE for live mission events, WebSockets for terminal & desktop stream. Five
visible tabs (Control / Terminal / Files / History / Settings), plus several
sheets.

The codebase already does some good things — see "What's already good"
at the bottom — but the dominant pattern is **full-screen spinner over blank
view while waiting for the network**. Skeleton primitives (`ShimmerCard`,
`ShimmerRow` in `LoadingView.swift:48-84`) exist but are essentially **never
used**; every screen falls back to `LoadingView(message:)` which is a centered
`ProgressView` over an empty layout.

---

## High-severity blockers (visible freezes on common flows)

### 1. Mission switch always shows a blank spinner — even when cached
**Where:** `Views/Control/ControlView.swift:2474-2541` (`switchToMission`)

`loadMission` (line 1636) correctly checks the on-disk cache and renders the
cached mission instantly (line 1648-1651) before fetching fresh events, **but
the parallel `switchToMission` path used by the mission switcher, running-bar
chip, and worker peeks does not**. It just sets `isLoading = true` (line 2489)
and replaces the chat with `LoadingView("Loading conversation...")`
(line 675-676) until both `getMission` and `getMissionEventsWithMeta` return
serially.

On a slow link this is 2–30 s of blank screen for content the device already
has on disk.

**Fix:** Mirror `loadMission`'s cache-first path in `switchToMission`. Call
`loadCachedMissionData(id)` and `applyViewingMissionWithEvents` immediately,
then fan-out the refresh in the background.

---

### 2. Mission load fetches metadata and events serially
**Where:** `ControlView.swift:1661-1697`, `ControlView.swift:2510-2534`

```swift
let mission = try await api.getMission(id: id)        // RTT 1
…
let result = try await api.getMissionEventsWithMeta(…) // RTT 2
```

These two requests share only `id`; the events fetch does not need the metadata
response. On a 300 ms-RTT cellular link this is ~600 ms of mandatory latency.

**Fix:** `async let metaTask = api.getMission(id:)` plus
`async let eventsTask = api.getMissionEventsWithMeta(…)`, then `await` both. Same
applies to `switchToMission`. The child-mission fetch (line 1716) is already
backgrounded — good.

---

### 3. Backend/agent loading walks every backend serially
**Where:** `Services/BackendAgentService.swift:51-96`

```swift
for backend in backends {
    let config = try await api.getBackendConfig(backendId: backend.id)
    …
}
for backendId in enabled {
    let agents = try await api.listBackendAgents(backendId: backendId)
    …
}
```

With 5 backends and a 200 ms RTT, that's 2 s on first New-Mission tap or first
Settings open — `NewMissionSheet:224-232` and `SettingsView:188-196` both block
the entire agent picker behind a "Loading agents…" spinner during this time.

**Fix:** Two `withTaskGroup`s — one for configs, one for agents — to fan out.
A single `async let` per backend would also work but the count is variable.

---

### 4. Historic event replay runs synchronously on the main actor
**Where:** `ControlView.swift:1445-1501` (`applyViewingMissionWithEvents`)

```swift
messages.removeAll()
for event in orderedEvents {
    …
    handleStreamEvent(type: event.eventType, data: data, isHistoricalReplay: true)
}
recomputeGroupedItems()
```

`handleStreamEvent` mutates `messages` (an `@State`). On a 5 k-event mission
this is a multi-frame stall on the main actor while the user stares at a
half-rendered chat. (`recomputeGroupedItems` is correctly deferred to the end —
good — but the per-event mutations still observe.)

**Fix:** Move replay off the main actor. Build `[ChatMessage]` and grouped items
in a `Task.detached` (events + their handler can be pure), then do one bulk
assignment back on the main actor. Alternatively: batch in chunks of ~100 with
`Task.yield()` between batches so the runloop can paint a skeleton.

---

### 5. File browsing wipes the list on every navigation
**Where:** `Views/Files/FilesView.swift:59-73, 418-445`

Every `navigateTo()` (line 447) clears the screen to `LoadingView("Loading
files...")`. There's no per-path cache, so going up one level and back round-trips
even though the parent directory was just rendered. The race-condition guard
(`fetchingPath`) is correct but doesn't help with the blank-screen feel.

**Fix:**
- LRU-cache directory listings (path → entries, 60 s TTL).
- Keep the previous listing visible with `.opacity(0.5)` + a subtle progress
  pill until the new listing arrives — same pattern as cached mission load in
  Control.
- Show a `ShimmerRow` skeleton list (the primitive already exists in
  `LoadingView.swift:48`).

---

### 6. File upload reads the whole file into memory and uploads serially
**Where:** `FilesView.swift:499-525`, `APIService.swift:447-492`

```swift
let data = try Data(contentsOf: url)              // whole file in RAM
let _ = try await api.uploadFile(data: data, …)   // one file at a time
```

A 200 MB video will OOM-kill the app. Multi-file imports block the picker for
the duration. There's no progress UI either — the user has no idea anything is
happening.

**Fix:**
- Use `URLSession.shared.uploadTask(with:fromFile:)` with the file URL directly
  (streams from disk). The current MIME-multipart helper would need to write a
  temp file with the boundary framing, or switch to a single-file PUT.
- Fan out concurrent uploads with `withTaskGroup` (cap at 3-4 in flight).
- Add a per-upload progress bar via `URLSessionTaskDelegate` or
  `URLSession.uploadTask(_:from:).progress`.

---

### 7. Workspaces tab uses legacy completion-handler API and a full-screen spinner
**Where:** `Views/Workspaces/WorkspacesView.swift:21-25, 102-117`

`isLoading = true` shows a bare `ProgressView` on the whole screen on every
appear. Creating a workspace dismisses the sheet and re-fetches the entire
list (line 92-95) — no optimistic insert. The completion-handler API
(`APIService.listWorkspaces(completion:)`, line 534-543) wraps an async call in
a `Task` then bounces back to main with `DispatchQueue.main.async` (line 107) —
unnecessary indirection.

**Fix:**
- Switch to the async variant.
- Show 3-4 `ShimmerCard`s during initial load.
- Optimistically prepend the new workspace on create; reconcile when the API
  returns.
- Share the cache with `WorkspaceState.shared.workspaces` so opening the tab
  while another tab already loaded shows data immediately.

---

### 8. HistoryView loads every mission, no pagination, no server-side search
**Where:** `Views/History/HistoryView.swift:260-279, 42-60`

`listMissions()` fetches the whole list (no `limit`/`offset` exposed on the
endpoint here), then `filteredMissions` filters client-side by `searchText`.
The backend exposes `/api/control/missions/search` (`APIService.swift:286-295`),
but the UI doesn't call it. On accounts with thousands of missions this is a
multi-MB download per cold-open of History, while the user stares at
`LoadingView("Loading history...")`.

**Fix:**
- Paginate: initial page 50, fetch more on scroll-near-bottom.
- Use `searchMissions(query:)` for the search bar (debounced 300 ms).
- Cache the first page in memory; on tab open render immediately, refresh
  silently in the background.

---

### 9. Markdown re-parsed on every view recomposition
**Where:** `Views/Components/MarkdownView.swift:20-21`

```swift
var body: some View {
    let blocks = MarkdownParser.parse(content)   // runs on every body call
    …
}
```

`MarkdownParser.parse` is a non-trivial line scanner. SwiftUI re-runs `body`
frequently — every scroll tick, every observed property change, every keyboard
appearance. For a chat with dozens of assistant messages each containing several
markdown blocks plus a `MarkdownInlineText` that calls
`AttributedString(markdown:)` per paragraph, this is real main-thread CPU during
scroll.

**Fix:** Pre-parse once when a `ChatMessage` is constructed (or in the
detached-replay refactor in #4), store `[MarkdownBlock]` on the message.
Memoize `AttributedString` per paragraph as well.

---

### 10. SSE reconnect fallback can fetch the entire history
**Where:** `ControlView.swift:1820-1888` (`resumeMissionAfterReconnect`)

When the delta-resume path returns `.unsupported`, `.noCursor`, or `.failed`,
the code reloads the latest page (`getMissionEventsWithMeta(... limit: ...,
latest: true)`). On a 50 k-event mission, even the "latest" tail can be
megabytes. The chat freezes during the catch-up.

**Fix:**
- Cap the fallback fetch to a small window (e.g., 200 events) and surface a
  "Load earlier" affordance for older content.
- During catch-up, keep the existing in-memory transcript visible and overlay a
  thin "Catching up…" pill (the connection banner at line 642-660 already shows
  for disconnected; this would be a parallel "syncing" state).

---

## Medium-severity issues

### 11. Send button has no in-flight indicator
**Where:** `ControlView.swift:2245-2290`

Optimistic insert exists (good), but the inserted bubble is indistinguishable
from a confirmed one — no opacity dim, no ⏳, no disabled state on the send
button. On a slow link the user re-taps or doubts the send.

**Fix:** Add `isPending: Bool` to `ChatMessage` for the temp message; render at
70% opacity with a tiny spinner until the SSE confirmation (or the
`sendMessage` API response) clears it.

---

### 12. Queue sheet loads after opening
**Where:** `ControlView.swift:2304-2310`, sheet binding `:569-584`

Sheet opens to potentially-stale `queuedItems` while `loadQueueItems()` fires.

**Fix:** Refresh on a 5 s timer when queue length > 0 so the sheet open is
instantaneous. Also `loadQueueItems` could be invoked when `queueLength`
transitions above 0 (cheap idempotent prefetch).

---

### 13. Slash command catalog lazy-loaded on first "/"
**Where:** `ControlView.swift:1106-1118, 1231-1237`

First `/` keypress fires `/api/library/builtin-commands` and the popover stays
empty for up to 15 s.

**Fix:** Pre-fetch on cold start alongside the other context fetches (line
413-417). The payload is tiny.

---

### 14. Mission switcher loads recent list AFTER opening
**Where:** `ControlView.swift:288-294`

```swift
Task { await loadRecentMissions() }
showMissionSwitcher = true
```

Sheet opens, list is empty for a beat.

**Fix:** Keep `recentMissions` warm via a low-frequency background poller (or
hydrate from `listMissions()` already cached in History tab — share the cache).

---

### 15. NewMissionSheet loads agents then providers serially
**Where:** `Views/Control/NewMissionSheet.swift:443-483`

`await BackendAgentService.loadBackendsAndAgents()` then
`await api.listProviders()` — serial, both block the form.

**Fix:** `async let` both. The form's model picker depends on neither in
parallel.

---

### 16. ContentView auth check shows full-screen "Connecting…"
**Where:** `ContentView.swift:19-23, 46-67`

Cold open on a slow link: 0-15 s of "Connecting…" with no progress, no retry.
Health check failure pushes user to LoginView without explanation.

**Fix:** Show the prior tab content opportunistically (rooted in saved JWT) and
silently re-check health in the background; degrade only on confirmed 401 or
network failure with retry CTA.

---

### 17. Image cards each fire their own request, no shared cache
**Where:** `ControlView.swift:3406-3596` (`SharedFileCardView`)

Every image in a message starts an independent `URLSession.shared.data(for:)`
on `.task`. Three images in a message = three sequential RTTs from the same
host, no `URLCache` warmup, no shared `NSCache`/`URLSession` configured for
images, no thumbnail variant requested.

**Fix:**
- Centralize image fetching in a small `ImageLoader` actor with an `NSCache`
  keyed by URL+token.
- Request a thumbnail variant if backend supports it; fall back to full.
- Render a blurred-hash or color-block placeholder rather than a centered
  spinner over `Theme.backgroundSecondary` (line 3411-3413, 3426-3428).

---

### 18. Terminal connect has a hardcoded 0.5 s "connected" delay
**Where:** `Views/Terminal/TerminalView.swift:286-294`

```swift
DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
    if state.connectionStatus == .connecting {
        state.connectionStatus = .connected
        …
    }
}
```

Arbitrary delay before declaring "Connected" — adds latency on fast networks
and hides true connection failures on slow ones. Workspace switch adds another
0.3 s for nothing (line 52).

**Fix:** Mark connected on the first WebSocket message receive (same pattern
DesktopStreamService uses — line 162-165). Eliminate the 0.3 s
`DispatchQueue.main.asyncAfter` on workspace switch; rely on connection-state
transitions.

---

### 19. Terminal ANSI parsing runs eagerly on every line
**Where:** `Services/TerminalState.swift:65-73`

```swift
init(text: String, type: LineType) {
    …
    if type == .output {
        self.attributedText = ANSIParser.parse(text)
    }
}
```

A `cat` of a large file pumps thousands of lines through the WebSocket; each
constructs a `TerminalLine` on `MainActor` (the `receiveMessage` continuation
is wrapped in `Task { @MainActor in … }`) and parses ANSI inline. Burst output
freezes the input field.

**Fix:** Coalesce inbound text into a debounced flush (e.g., every 16 ms), parse
ANSI in a background actor, then bulk-append parsed lines on the main actor.

---

### 20. Desktop stream redraws CGImage→CVPixelBuffer on the main actor
**Where:** `Services/DesktopStreamService.swift:268-353` (`feedFrameToPipLayer`)

Per-frame at up to 30 fps the service allocates a CVPixelBuffer, locks its base
address, draws the CGImage through a CGContext, and creates a CMSampleBuffer —
all on the main actor (`@MainActor` class, called from `handleMessage`).

**Fix:** Move the conversion to a serial background queue or
`DispatchQueue(label:)`; only `layer.enqueue(sample)` strictly needs to be on
the main actor (and even that depends on the layer's setup).

---

### 21. Mission cache JSON encode runs on the main actor
**Where:** `ControlView.swift:1335-1352`

```swift
// Encode on @MainActor (CachedMissionData transitively contains
// non-Sendable types) …
let encoded = try? JSONEncoder().encode(cacheData)
Task.detached(priority: .utility) { … }
```

The write is detached (good), but the encode itself can be tens of MB for big
missions and runs synchronously on the main thread, blocking input and
animations.

**Fix:** Make `CachedMissionData` and the event types `Sendable` so the encode
can move to the detached task as well. If full sendability is hard, copy the
needed fields into a `Sendable` snapshot struct first.

---

### 22. Running-missions poller fires every 3 s with no backoff
**Where:** `ControlView.swift:2464-2472`

A 3 s tick is reasonable when active, but it keeps firing while the app is in
the background (the task isn't tied to scenePhase) and during sustained network
failures. With a 15 s request timeout each failed poll holds a URLSession slot.

**Fix:**
- Suspend the poller while `scenePhase != .active` (use `.onChange(of:
  scenePhase)`).
- Apply 1.5×/2× backoff on consecutive failures, reset on success.

---

### 23. AutomationsView: load before render, full-screen spinner
**Where:** `Views/Control/AutomationsView.swift:28-30, 136-147`

Single fetch under `LoadingView` until response arrives. Small data, but
visible flash on a slow link.

**Fix:** Cache last response per `missionId`; on retry show stale-while-revalidate.

---

### 23a. FIDO approval buttons have no in-flight feedback
**Where:** `Views/FidoApproval/FidoApprovalOverlay.swift:79-94`,
`Services/FidoApprovalState.swift:83-100`

Tapping **Approve** fires `Task { await fidoState.approve(request.id) }`. The
request is only removed from `pendingRequests` after `api.fidoRespond(…)`
returns — up to 15 s. During that window the overlay stays exactly as it was,
with both buttons still tappable, and the user has no idea whether the tap
registered. Re-taps will fire duplicate `fidoRespond` calls.

**Fix:** Add `inFlightRequestIds: Set<String>` to `FidoApprovalState`; toggle on
entry/exit of `approve`/`deny`. In the overlay, disable both buttons and swap
the icon for a spinner when `request.id ∈ inFlightRequestIds`. Bonus: the
"Auto-approve SSH for 5 min" chip has the same issue.

---

### 24. SetupSheet adds a fake 500 ms "success" delay
**Where:** `ContentView.swift:200-202`

```swift
try? await Task.sleep(for: .milliseconds(500))
onComplete()
```

Pure cosmetic latency.

**Fix:** Delete the sleep; the haptic + checkmark already convey success.

---

## Lower-severity / code-quality

### 25. Toolbar status pill conflates "not connected" with "off"
**Where:** `TerminalView.swift:79-114`

The pill says "Off" in any non-`.connected` state — including `.connecting`,
`.error`. A user who taps **Connect** sees "Off" until the 0.5 s delay
(see #18) declares success, then nothing in between.

**Fix:** Add explicit `connecting`/`error` visual states; surface the error in
the pill rather than only in the overlay banner.

---

### 26. `currentFrame` swapped on every WebSocket message
**Where:** `DesktopStreamService.swift:181-191`

Each JPEG frame creates a new `UIImage(data: data)` and sets `currentFrame`,
which re-renders the SwiftUI tree. At 30 fps with a multi-MB image this is
heavy.

**Fix:** Throttle visible updates (e.g., display-link driven) and defer JPEG
decoding to a background task; consider switching to streaming HEVC/JPEG via a
dedicated `AVSampleBufferDisplayLayer` (already half-set-up for PiP).

---

### 27. UserDefaults used as a primary cache
**Where:** `APIService.swift:69-72, 80-82`, several
`UserDefaults.standard.bool/set` callsites

`baseURL` and `jwt_token` live in `UserDefaults` (synchronous access, written
on every change). Frequent draft saves (`ControlView.swift:460-468`) are
debounced — good — but the underlying defaults DB still holds the prefs across
the whole app and any large mission-cache blob would be wasted there. (The
"legacy mission cache" migration at `SandboxedDashboardApp.swift:12-17` confirms
this was a real problem in the past — comment notes "multi-KB-to-multi-MB JSON
payload held resident by cfprefsd").

**Fix:** Migrate the JWT to Keychain (Keychain reads can be slow too — cache
in-memory after first read). Confirm no large blobs slipped back into defaults.

---

### 28. Sheets dismiss into full reloads
**Where:** `WorkspacesView.swift:85-95`, several
`onCancel:`/`onComplete:` paths

Closing the New Workspace or New Mission sheet immediately re-fires the
parent's load function instead of merging the returned item.

**Fix:** Pass the created entity back and merge into the local list optimistically.

---

### 29. No empty-state-while-loading distinction
**Where:** every screen that uses `LoadingView`

`LoadingView` is centered text + spinner over a transparent background; on
sub-second responses it's a visible flash. Several screens (`HistoryView:131`,
`FilesView:59`, `WorkspacesView:21`) sit in the loading branch for >50 ms,
which is enough for the eye to catch the flash.

**Fix:** Show the screen scaffold (nav title, filter pills, search bar) with
`ShimmerRow` placeholders for content. Only show `LoadingView` when there is no
prior scaffold to render. This is a one-time refactor that benefits four
screens.

---

### 30. Mission switcher backend search debounces the fetch but not the input
**Where:** `ControlView.swift:4706-4744`

The 250 ms debounce timer fires the API call, but the previous result can land
after a newer query — race window. Also there's no cancellation when the user
clears the search box quickly.

**Fix:** Cancel the in-flight task on every text change; only kick off after
the debounce. Guard updates with the normalized query string (the code already
does this on line 4728 — good — but still leaves the request in flight).

---

## Improvement backlog (grouped by approach)

### A. Skeletons & cached UX
1. Use existing `ShimmerCard`/`ShimmerRow` on **History**, **Files**,
   **Workspaces**, and **Mission detail** instead of `LoadingView`.
2. **Files**: keep previous listing visible during navigation; LRU cache by
   path.
3. **History**: paginate + stale-while-revalidate; share the in-memory mission
   list with Control's `recentMissions`.
4. **Control / Mission switch (#1)**: mirror the cached-first path that
   `loadMission` already implements.
5. **Image cards (#17)**: shared cache; blurhash/color-block placeholder.
6. **Markdown (#9)**: parse once at message construction; memoize attributed
   strings.

### B. Optimistic UI
1. **Send-message bubble (#11)**: render at 70 % opacity with mini-spinner
   until confirmed.
2. **Queue actions** (already optimistic — good).
3. **Workspace / Folder create (#7, #28)**: prepend on success, reconcile on
   response.
4. **Mission delete** (already optimistic — good).
5. **Mission switch**: render cached transcript immediately while metadata
   refresh resolves.

### C. Parallelization
1. **BackendAgentService (#3)**: `withTaskGroup` over backends.
2. **Mission load (#2)**: `async let` metadata + events.
3. **Mission switch (#2)**: same.
4. **NewMissionSheet (#15)**: `async let` agents + providers.
5. **File upload (#6)**: streamed uploads + concurrent (3-4) for batches.
6. **Cold-start (already parallel)** — keep, but add slash-command catalog
   into the fan-out (#13).

### D. Off-main-actor work
1. **Event replay (#4)**: build messages on a detached task; bulk-assign.
2. **Markdown parse (#9)**: same as A6 — at construction, not in `body`.
3. **JSON encode for mission cache (#21)**: detached.
4. **Desktop frame conversion (#20)**: detached.
5. **Terminal ANSI parse (#19)**: detached, debounced flush.
6. **JPEG decode for desktop frames (#26)**: detached + throttle.

### E. Smarter network shape
1. **History pagination (#8)** + use `searchMissions` server-side.
2. **SSE reconnect cap (#10)** — don't refetch entire tail.
3. **Image thumbnails (#17)** — request smaller variants.
4. **Running-missions poller (#22)** — suspend in background, backoff on error.
5. **Prefetch slash commands, recent missions, queue** on cold start (#12-14).

### F. Cosmetic latency to delete
1. `SetupSheet` 500 ms success sleep (#24).
2. Terminal 0.5 s "connected" timer (#18) and 0.3 s workspace-switch delay.
3. `DispatchQueue.main.asyncAfter` callsites in ControlView lines 935, 2171,
   4035 (assorted scroll/animation timers — audit each for necessity).

### G. Cross-cutting plumbing
1. Introduce a tiny `Cached<T>(ttl:)` wrapper to standardize the
   stale-while-revalidate pattern (BackendAgentService already implements this
   manually — generalize).
2. Centralize per-screen "scaffold + skeleton" so every list view has a
   consistent loading experience.
3. Add a "loading state" enum (`idle | loading(cached: …) | loaded | error`)
   per data source so views can render the right scaffolding without
   ad-hoc booleans (`isLoading`, `isLoadingEarlier`, `isLoadingHistory`,
   `isLoadingImage`, `isLoadingAgents`, `isLoadingConnection`, …).

---

## What's already good (don't undo)

- `APIService` runs JSON decoding off-main via `Task.detached`
  (`APIService.swift:825-839`) — keeps large payloads from hitching the UI.
- Cold-start `async let` fan-out in ControlView (`:411-417`).
- File-based mission cache + LRU eviction + atomic writes
  (`ControlView.swift:1335-1418`).
- Race-condition guards on every `await` boundary (`fetchingMissionId`,
  `fetchingPath`, connection IDs in `DesktopStreamService.connectionId`).
- 15 s request timeout + 30 s SSE inactivity timeout — short enough to surface
  failures.
- SSE delta-resume with `X-Max-Sequence` header (`ControlView.swift:1796-1828`).
- Optimistic queue add/remove/clear (`ControlView.swift:2312-2344`).
- Workspace/terminal/mission state are `@MainActor @Observable` singletons —
  good for cross-tab sharing.
- 30 s TTL cache in `BackendAgentService` for repeat New-Mission opens.

---

## Suggested priority order

1. **#1, #2, #4** — mission load/switch (biggest perceived freeze).
2. **#3** — backend/agent parallel fetch (felt on every New Mission tap).
3. **#11** — send-message in-flight state (visible to every user every send).
4. **#5, #7, #8** — skeleton & caching pass on Files / Workspaces / History.
5. **#6** — file upload streaming + progress (correctness, not just polish).
6. **#9** — markdown caching (scroll smoothness across all chat views).
7. **#10, #17, #19, #20, #21** — main-actor offloads & per-frame work.
8. **#22-30** — cleanup, deletions, plumbing.

Item-level estimates: 1, 2, 3, 11, 13, 14, 15, 24 are <1 hour each; the
skeleton refactor across 4 screens is ~half a day; the streaming-upload work
(#6) and the off-main event replay (#4) are the only multi-day items.

---

## Appendix — concrete patch sketches for the top issues

These are illustrative, not press-Save-ready code. Types and helper names match
the current codebase.

### Patch for #1 (mission switch cache-first)

```swift
// ControlView.swift — switchToMission, around line 2483-2541
private func switchToMission(id: String) async {
    guard id != viewingMissionId else { return }
    let previousViewingMission = viewingMission
    let previousViewingId = viewingMissionId
    viewingMissionId = id
    fetchingMissionId = id
    childMissions = []

    // 1. Render cached transcript instantly (same as loadMission)
    let hasCache: Bool
    if let cached = loadCachedMissionData(id) {
        applyViewingMissionWithEvents(cached.mission, events: cached.events)
        hasCache = true
    } else {
        hasCache = false
        isLoading = true
    }

    // Keep run-state hydration from runningMissions (existing logic)

    do {
        // 2. Fan out metadata + events in parallel
        async let meta = api.getMission(id: id)
        async let evts = api.getMissionEventsWithMeta(id: id, types: historyEventTypes)
        let mission = try await meta
        let result = (try? await evts) ?? .init(events: [], maxSequence: nil)
        guard fetchingMissionId == id else { return }

        if currentMission?.id == mission.id { currentMission = mission }

        if !result.events.isEmpty {
            applyViewingMissionWithEvents(mission, events: result.events)
            if let max = result.maxSequence, max > 0 { missionMaxSeq[id] = max }
            cacheMissionWithEvents(mission, events: result.events)
        } else if !hasCache {
            removeMissionFromCache(mission.id)
            applyViewingMission(mission)
        }
        isLoading = false
        // background workers fetch unchanged
    } catch {
        guard fetchingMissionId == id else { return }
        if !hasCache {
            isLoading = false
            // existing fallback to previousViewingMission
        }
    }
}
```

### Patch for #3 (parallel backend/agent load)

```swift
// BackendAgentService.swift — fetchBackendsAndAgents
private static func fetchBackendsAndAgents() async -> BackendAgentData {
    let backends = (try? await api.listBackends()) ?? Backend.defaults

    // Parallel config fetch
    let enabledArr = await withTaskGroup(of: (String, Bool).self) { group in
        for b in backends {
            group.addTask {
                let enabled = (try? await api.getBackendConfig(backendId: b.id).isEnabled) ?? true
                return (b.id, enabled)
            }
        }
        var out: [(String, Bool)] = []
        for await pair in group { out.append(pair) }
        return out
    }
    let enabled = Set(enabledArr.filter(\.1).map(\.0))

    // Parallel agent fetch
    let agentsArr = await withTaskGroup(of: (String, [BackendAgent]).self) { group in
        for id in enabled {
            group.addTask {
                let list = (try? await api.listBackendAgents(backendId: id))
                    ?? (id == "amp" ? [
                        BackendAgent(id: "smart", name: "Smart Mode"),
                        BackendAgent(id: "rush", name: "Rush Mode")
                    ] : [])
                return (id, list)
            }
        }
        var out: [(String, [BackendAgent])] = []
        for await pair in group { out.append(pair) }
        return out
    }
    return BackendAgentData(
        backends: backends,
        enabledBackendIds: enabled,
        backendAgents: Dictionary(uniqueKeysWithValues: agentsArr)
    )
}
```

### Patch for #11 (send-message in-flight state)

```swift
// ChatMessage.swift
struct ChatMessage: Identifiable {
    let id: String
    let type: MessageType
    let content: String
    var isPending: Bool = false   // <-- new
    let timestamp: Date
    // …
}

// ControlView.swift — sendMessage
let tempMessage = ChatMessage(id: tempId, type: .user, content: content, isPending: true)
messages.append(tempMessage)
// …
do {
    let (messageId, queued) = try await api.sendMessage(content: content)
    if let i = messages.firstIndex(where: { $0.id == tempId }) {
        messages[i] = ChatMessage(id: messageId, type: .user, content: content,
                                  isPending: false, timestamp: messages[i].timestamp)
    }
    // …
} catch { /* remove tempId, error haptic */ }

// In the bubble view:
.opacity(message.isPending ? 0.55 : 1)
.overlay(alignment: .bottomTrailing) {
    if message.isPending {
        ProgressView().scaleEffect(0.5).padding(6)
    }
}
```

### Patch for #5 (Files cache + stale-while-revalidate)

```swift
// FilesView.swift
@State private var pathCache: [String: [FileEntry]] = [:]  // simple LRU later

private func loadDirectory() async {
    let pathToLoad = currentPath
    fetchingPath = pathToLoad
    if let cached = pathCache[pathToLoad] {
        entries = cached
        isLoading = false          // render immediately
    } else {
        isLoading = entries.isEmpty   // only show spinner if nothing to show
    }
    do {
        let fresh = try await api.listDirectory(path: pathToLoad)
        guard fetchingPath == pathToLoad else { return }
        entries = fresh
        pathCache[pathToLoad] = fresh
    } catch {
        guard fetchingPath == pathToLoad else { return }
        if pathCache[pathToLoad] == nil { errorMessage = error.localizedDescription }
    }
    if fetchingPath == pathToLoad { isLoading = false }
}
```

### Patch for #18 (Terminal: drop the 0.5 s fake-connect timer)

```swift
// TerminalView.swift — replace the DispatchQueue.main.asyncAfter at line 286.
state.webSocketTask?.resume()
receiveMessages()
sendResize(cols: 80, rows: 24)
// Move state transitions into receiveMessages:
case .success(let message):
    Task { @MainActor in
        if state.connectionStatus != .connected {
            state.connectionStatus = .connected
            state.isConnecting = false
            state.appendLine(TerminalLine(text: "Connected.", type: .system))
        }
        self.handleOutput(/* … */)
    }
```

### Patch for #19 (Terminal ANSI parse: batch + off-main)

```swift
// TerminalState.swift
actor ANSIParserActor {
    func parse(_ text: String) -> AttributedString { ANSIParser.parse(text) }
}
private let ansiActor = ANSIParserActor()
private var pendingChunks: [String] = []
private var flushTask: Task<Void, Never>?

func appendOutputChunk(_ text: String) {
    pendingChunks.append(text)
    flushTask?.cancel()
    flushTask = Task { @MainActor in
        try? await Task.sleep(nanoseconds: 16_000_000) // 16 ms coalesce
        let batch = pendingChunks.joined()
        pendingChunks.removeAll()
        // parse off-main
        let parsed = await ansiActor.parse(batch)
        // append in one shot
        terminalOutput.append(TerminalLine(parsed: parsed, type: .output))
    }
}
```

