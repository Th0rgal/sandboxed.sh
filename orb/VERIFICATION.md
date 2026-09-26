Orb cron acceptance verification — 2026-09-20

Started in a separate clone at verified remote branch
`feat/orb-creation-theme-polish`, SHA
`3efa640772d8fc19f93438f58d7fd64edb448251`. Disk and inode checks completed before
cloning. No predecessor/native session was resumed or invented. No live cron,
mission, service, deployment, or merge operation was performed.

| Acceptance | Evidence |
| --- | --- |
| One summary trigger; shared schedule editor; compact day/date layout | Browser tests check equal Name/Runs/Stops-after widths, 240px popover bounds, days and datetime overflow, and Escape focus. Separate 375px viewport test passes. |
| Shared create/edit form | `CronForm` is used by project creation and `ControllerSettingsPanel`; skills, repeat, advanced fields, validation and discard share one implementation. |
| Real Hermes mapping | `cronSchema.ts` maps object schedules, repeat counters, model/provider/reasoning/workdir, and `context_from: self` continuity. Read/list/update/action paths use the same projection. Real fixture provenance is in `tests/fixtures/README.md`. |
| Root expansion/cache/reveal | Browser tests prime the empty root cache, collapse it, create a folder, verify refreshed children and expansion; creation opens the returned cron and expands its project. |
| Row focus/menu keyboard | Browser tests check focus-within visibility, first-item focus, ArrowDown navigation and Escape restoration. Unit tests cover menu behavior. |
| Cursor audit | Pointer for selects/links/actions and default for disabled fields; browser assertions cover select, link and disabled input. |
| Draft protection | Tests cover tabs, view unmount, creation dialog dismissal, discard, rejected saves, dropped API fields and transferring unsaved creation overrides into the created job's edit draft. |
| Workspace/format/backend | pnpm 10.17.1 pinned; isolated package workspace and esbuild allowlist documented. Root and standalone Orb rustfmt checks pass. Rust 1.98.1 compiles the backend in this checkout. |

Passed commands:

- `pnpm install --frozen-lockfile` (esbuild rebuild completed with the explicit allowlist)
- `pnpm build`
- `pnpm test` — 17 tests
- `pnpm test:browser` — 5 Chromium tests, including light/dark, compact viewport and saved defaults
- `cargo +stable fmt --all --check`
- `cargo +stable fmt --manifest-path orb/src-tauri/Cargo.toml -- --check`
- `cargo +stable check --locked --bin sandboxed-sh -j 2`
- `cargo +stable build --locked --bin sandboxed-sh -j 2`
- `cargo +stable test --locked --lib project_cron_bindings_are_scoped_and_idempotent -j 2` — 1 targeted test
- `tests/fixtures/verify_api_patch.py` with isolated Hermes source — real create/update handler transport and storage verified

Backend compilation reports six existing dead-code warnings. The targeted Rust
test reports one dead-code warning. No compilation or test failures remain.

Screenshots use the real Orb components with all API traffic intercepted by the
local test harness; they do not show production state:

- [Create, light](../screenshots/orb-cron-create-light.png)
- [Create, dark](../screenshots/orb-cron-create-dark.png)
- [Edit, light](../screenshots/orb-cron-edit-light.png)
- [Edit, dark](../screenshots/orb-cron-edit-dark.png)

Release dependency: Hermes production source at `882881de` stores the advanced
fields but its REST API filters them. The companion
`../patches/hermes/orb-cron-api-fields.patch` exposes those fields and has been
verified on an isolated copy of that exact source. It is not deployed. Orb also
reports dropped fields from an older API and preserves the requested overrides
as a draft. The sandboxed.sh Hermes submodule pointer is unchanged.

Follow-up after inspecting both uploaded Cursor references:

- Both files were accessible under predecessor context c68142d2. Inspected the
  full-window and sidebar images: muted dark surfaces, compact folder hierarchy,
  rounded active rows, trailing cloud glyphs and contextual row actions.
- Native schedule normalization now covers primary-controller reads, updates
  and actions as well as additional project crons; the form no longer calls
  `.trim()` directly on a potentially structured schedule.
- Real Hermes snapshot defaults are preserved and shown separately from explicit
  model/provider overrides. A generated snapshot fixture covers this behavior.
- Dialogs and schedule popovers share stacked focus ownership. Only the top
  layer handles Escape, Tab/Shift+Tab remain within it, and focus returns to its
  opener. Global app shortcuts defer to the active focus scope. Menu actions
  restore their trigger before opening dialogs.
- Committed component tests cover nested dialogs/popovers and keyboard behavior,
  in addition to the intercepted browser integration tests.

Follow-up validation: `pnpm build`, all 17 component/unit tests, and all 5 browser tests passed. Native Mac integration has not been run in this Linux checkout.

Additional review fixes — 2026-09-20:

- Project cron listing propagates upstream outages, authentication failures and
  malformed records. Only an explicit Hermes deleted-job response is skipped.
  The UI retains cached jobs, displays the error and offers retry; upstream
  authentication failures do not clear the Orb login.
- Creation defaults to `project:<slug>` and explains the canonical conversation
  destination. Missing bindings produce actionable validation; local output
  requires an explicit choice. Additional cron ownership remains separate from
  the canonical controller.
- At widths up to 720px the sidebar toggle opens a drawer, dismissible with the
  toggle, backdrop or Escape. Browser coverage uses the actual App at 375px.
- Passed: 18 unit/component tests, 8 Chromium browser tests, production frontend
  build, 6 targeted Rust tests (`cargo +stable test --locked --lib project_cron
  -j 2`), backend check and build. Root rustfmt and the separate
  `orb/src-tauri/Cargo.toml` rustfmt check both passed. The backend compile also
  verifies the `ProjectsStore::project_cron_ids` lifetime fix.
- Rust tests exercise the HTTP adapter against a local Axum server, real Hermes
  record fixtures, exclusive project/job ownership, and delivery validation.
  These supplement the browser's intercepted API tests.
- [Narrow sidebar drawer](../screenshots/orb-narrow-drawer.png). Updated form
  screenshots reflect the visible delivery destination and explanation.

No deployment or native Mac integration was performed.

Consolidated sidebar, streaming and project chooser checkpoint:

- Sidebar rows are 32px, with 5px selection radii, 4px group separation,
  #141415 dark tint at 96% opacity, and quiet trailing cloud icons without
  redundant workspace text. Actual App browser assertions verify row height.
- The project chooser has search, Recents, current selection, bounded scrolling,
  keyboard navigation, focus restoration and outside-click dismissal. Supported
  actions open separate project creation or the existing machine picker.
  Creation keeps the existing PUT endpoint and validates names/IDs, existing
  IDs and backend failures. The form explains hosted project file location;
  no filesystem path is fabricated.
- The transcript follows the producer's cumulative response bubble across tool
  boundaries, preserves event/sequence metadata, reconciles held overlap by
  identity/tool boundaries, and handles Unicode scalar offsets and canonical
  native bubbles. Genuine repeated messages remain distinct.
- Stable keyed rendering and reused work groups preserve expanded work/tool
  state. See benchmarks/README.md and raw before/after results for initial
  render, replay, update timing and DOM mutation counts.
- Passed 29 unit/component tests, all 11 browser scenarios, and the production
  frontend build. Browser tests use port 1431. The earlier backend checks and
  six Rust tests remain valid; this checkpoint changes no backend Rust.
- Composer voice code is untouched. Native validation belongs to the local
  coordinator; no production deployment or live mission mutation occurred.

Final reconnect verification: tool row keys use tool_call_id, so a coalesced
history replacement preserves expanded work and tool details even when the
stored snapshot moves relative to tools. The actual DOM regression passes.
Final production frontend build, all 29 unit/component tests and all 11 browser
tests passed. Screenshots were refreshed from this final build's components.

Launch and capability follow-up: see [LAUNCH-VERIFICATION.md](LAUNCH-VERIFICATION.md).
Passed 37 unit/component tests, 22 browser tests and the frontend build. Compact
unsupported-cron state, preserved launch drafts, visible initial prompts/status,
remote selection guards and startup timing artifacts are committed. Backend
structured-harness support is an explicitly separate integration dependency.
