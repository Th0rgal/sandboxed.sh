#!/usr/bin/env bash
# Run from any directory; requires installed pnpm dependencies and Chromium.
set -euo pipefail
cd "$(dirname "$0")/.."
trap 'rm -f src/_benchmarkBefore.tsx' EXIT
git show 9a7cb32b:orb/src/Transcript.tsx > src/_benchmarkBefore.tsx
ORB_BENCHMARK_BASELINE=1 pnpm exec playwright test transcript.browser.spec.ts -g 'long transcript'
cp test-results/transcript-before.json benchmarks/transcript-before.json
rm src/_benchmarkBefore.tsx
pnpm exec playwright test transcript.browser.spec.ts -g 'long transcript'
cp test-results/transcript-after.json benchmarks/transcript-after.json
