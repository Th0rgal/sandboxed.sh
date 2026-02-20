# Capability Matrix

This repository keeps a versioned capability matrix at `capabilities/matrix.v1.json` to prevent backend/client feature drift.

## What is enforced

- The matrix must exist and be valid JSON with `version: 1`.
- Each surface (`api`, `web`, `ios`) must have at least one `supported` capability row.
- Each `supported` row must provide one or more evidence entries:
  - `path`: repo-relative file path
  - `pattern`: exact string that must exist in that file
- Each `deferred` row must include a human-readable `note`.

CI runs `scripts/check-capability-matrix.sh` on every push/PR.

## Updating the matrix during rollouts

1. Update backend contract code.
2. Update web and iOS consumers (or mark deferred explicitly).
3. Edit `capabilities/matrix.v1.json`:
   - Keep status as `supported` when the surface ships the capability.
   - Set status to `deferred` with a concrete note if not shipped yet.
   - Update evidence patterns to match current contract code.
4. Run `scripts/check-capability-matrix.sh` locally.
5. Include matrix updates in the same PR as contract changes.

## Deferred support policy

Use `deferred` only when support is intentionally delayed for a surface. The note should describe what is missing and where parity will be tracked.
