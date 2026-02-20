#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MATRIX_FILE="${ROOT_DIR}/capabilities/matrix.v1.json"

if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required for capability checks" >&2
  exit 1
fi

if [[ ! -f "${MATRIX_FILE}" ]]; then
  echo "error: capability matrix not found at ${MATRIX_FILE}" >&2
  exit 1
fi

if ! jq -e '
  .version == 1 and
  (.capabilities | type == "array") and
  (.capabilities | length > 0)
' "${MATRIX_FILE}" >/dev/null; then
  echo "error: capability matrix must have version=1 and a non-empty capabilities array" >&2
  exit 1
fi

for surface in api web ios; do
  supported_count="$(jq -r --arg s "${surface}" '
    [.capabilities[] | select(.[$s].status == "supported")] | length
  ' "${MATRIX_FILE}")"
  if [[ "${supported_count}" -lt 1 ]]; then
    echo "error: ${surface} must have at least one supported capability check" >&2
    exit 1
  fi
done

while IFS=$'\t' read -r cap_id surface status; do
  if [[ "${status}" != "supported" && "${status}" != "deferred" ]]; then
    echo "error: ${cap_id}/${surface} has invalid status '${status}'" >&2
    exit 1
  fi

  if [[ "${status}" == "deferred" ]]; then
    note="$(jq -r --arg id "${cap_id}" --arg s "${surface}" '
      (.capabilities[] | select(.id == $id) | .[$s].note) // ""
    ' "${MATRIX_FILE}")"
    if [[ -z "${note}" ]]; then
      echo "error: ${cap_id}/${surface} is deferred but missing note" >&2
      exit 1
    fi
    continue
  fi

  evidence_len="$(jq -r --arg id "${cap_id}" --arg s "${surface}" '
    (.capabilities[] | select(.id == $id) | .[$s].evidence // []) | length
  ' "${MATRIX_FILE}")"

  if [[ "${evidence_len}" -lt 1 ]]; then
    echo "error: ${cap_id}/${surface} is supported but has no evidence entries" >&2
    exit 1
  fi

  while IFS= read -r evidence_json; do
    rel_path="$(jq -r '.path // ""' <<<"${evidence_json}")"
    pattern="$(jq -r '.pattern // ""' <<<"${evidence_json}")"
    if [[ -z "${rel_path}" ]]; then
      echo "error: ${cap_id}/${surface} evidence entry is missing path" >&2
      exit 1
    fi
    if [[ -z "${pattern}" ]]; then
      echo "error: ${cap_id}/${surface} evidence pattern must be non-empty" >&2
      exit 1
    fi
    abs_path="${ROOT_DIR}/${rel_path}"
    if [[ ! -f "${abs_path}" ]]; then
      echo "error: ${cap_id}/${surface} evidence path missing: ${rel_path}" >&2
      exit 1
    fi
    if ! grep -Fq -- "${pattern}" "${abs_path}"; then
      echo "error: ${cap_id}/${surface} evidence pattern not found in ${rel_path}" >&2
      exit 1
    fi
  done < <(jq -c --arg id "${cap_id}" --arg s "${surface}" '
    .capabilities[]
    | select(.id == $id)
    | .[$s].evidence[]
    | {path, pattern}
  ' "${MATRIX_FILE}")
done < <(jq -r '
  .capabilities[]
  | .id as $id
  | ["api", "web", "ios"][] as $s
  | [$id, $s, (.[$s].status // "")]
  | @tsv
' "${MATRIX_FILE}")

echo "Capability matrix checks passed."
