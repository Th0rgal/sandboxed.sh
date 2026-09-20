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
- `pnpm test` — 11 tests
- `pnpm test:browser` — 3 Chromium tests, including light/dark and compact viewport
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
