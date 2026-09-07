# Complete-source transport capacity evidence

Measured 2026-09-07 in an isolated development mission, from sandboxed.sh base
`8302c6dd3dd87eacbc4c04b32d307231e39e4632` (PR890 merged; no PR889 changes).
Sources are pristine pinned checkouts of `lfglabs-dev/lido-srv3-proof-closure`.
Both recursive pins are included: `lido-core` at
`17005714f151e5502c559932319a3f2f74ac2436`, and its nested
`foundry/lib/forge-std` at `ffa2ee0d921b4163b7abd0f1122df93ead205805`.

The originally supplied RESERVE1 pin
`2e4b6a50f68077d65a3d716522f9818fafd50073` was unavailable: direct `git fetch
origin <sha>` returned `not our ref`, and GitHub's commit API returned HTTP 422.
The current PR244 head below was fetched and measured instead. PR245 and PR246
heads were pinned at the same observation. These are transport fixtures, not
new Lido proof commits or claims about proof completion.

| Fixture | Pinned head | Files | Decoded bytes | Wrapper JSON bytes | Gzip bytes |
| --- | --- | ---: | ---: | ---: | ---: |
| reserve-current | `f27b39d8f12b1d8b5947c7f664ebfc33e0183019` | 2,075 | 21,265,880 | 28,748,779 | 6,084,229 |
| alloc2 | `2bb0a7dc7bf009333ba925b2a3a2e02503297b76` | 1,551 | 17,130,069 | 23,126,604 | 5,729,317 |
| alloc1 | `8691c7881a863715ab5ec9b39631ab41243e7c91` | 1,423 | 15,309,625 | 20,675,025 | 5,282,749 |
| combined | `b6c9ad041fa8b7ee4a098b5b21ff3009026175f2` | 2,382 | 25,322,173 | 34,213,451 | 7,285,251 |

The combined fixture retains every unique `(original path, bytes, executable
mode)` from the three snapshots. Identical shared entries occur once; differing
versions at the same path are retained under `fixture-variants/<label>/`.
It is a conservative capacity fixture, not a semantic merge. Every tracked
source and receipt is retained, including binary files, logs, and nested
submodules. Staging uses `git add --force -A` because inherited ignore rules
otherwise hide already-tracked receipt logs. An explicit union assertion
checks that no source version was omitted. The actual Lido checkouts stay
pristine. No proof code is modified or compiled.

## Bounded limits

Complete decoded source grows from 16 MiB to **32 MiB** at both sender and
receiver. The combined fixture leaves 8,232,259 bytes (about 7.85 MiB) of decoded
headroom; its 2,382 files leave 1,714 slots under the unchanged 4,096 file cap.
The core gzip decoder grows from 32 MiB to **64 MiB**: the combined JSON already
exceeds the old cap, and 32 MiB of raw bytes alone needs about 42.67 MiB in
base64. This is bounded headroom for the current deliverable, not a guarantee
for arbitrary future growth.

Both existing **50 MiB HTTP body caps remain unchanged** and now share a
constant. Core reads gzip through `Bytes`; the decoder reads at most 64 MiB + 1
and rejects expansion over 64 MiB. `RemoteNodeClient::submit_job` forwards plain
JSON via reqwest, and the node's `Json<SubmitJobRequest>` extractor has the
50 MiB cap. Tests exercise both extractors over loopback HTTP with a real
32 MiB recursive wrapper request and the serialized node envelope. Each
route rejects body cap + 1. No proxy configuration has been changed; the
intended production route still needs a rollout canary.

Overlay defaults remain 1 MiB / 256 operations. Full snapshots retain the
4,096-file ceiling. Existing sender byte/file and receiver byte overrides,
path restrictions, recursive pin/index checks, missing-submodule failures,
symlink and replacement-ref protections, privacy rules, receipt identity and
resume semantics remain intact. See [operator limits and coordinated
rollout](REMOTE_NODES.md#remote-lean-build-wrapper).

Bundle entry and deletion paths must be canonical relative paths: no empty,
`.` or `..` components, including leading/trailing slashes and repeated
separators. The receiver rejects the entire bundle before any writes or
deletions when a path violates this contract. Paths are never normalized;
manifest and operation digests continue to identify literal paths. This closes
the crafted `./Root.lean` / `Root.lean` destination alias; normal Git sender
paths are unaffected. Build-directory path handling is unchanged.

## Placement capacity contract

Complete bundles retain protocol v4. Receivers now advertise effective decoded
file-byte limits in `source_bundle_capacity` (`complete_bytes`, `overlay_bytes`),
including positive operator overrides. Core measures actual base64-decoded file
contents before dispatch and excludes insufficient receivers in automatic and
explicit placement. Auto re-probing retains the same constraint. With no capable
node, placement returns 503 before submission; accepted jobs and receipt reuse
keep their existing behavior. Fleet/tool output carries the same capability.

Legacy v4 nodes without this field retain compatibility through 16 MiB complete
(and v3 overlays through 1 MiB); larger bundles require explicit capacity.
Private lower overrides on legacy nodes remain unknowable and require updating
those receivers to advertise them. Updated lower ceilings are enforced for all
bundle sizes. Old backends ignore the new heartbeat field and must be updated
before enabling larger bundles. The 32/64/50 MiB limits above are unchanged. Decoded-byte advertisements do not
replace the wire bound: core counts the exact serialized node job, including
lease and JSON escaping, before tentative admission and dispatch. Above 50 MiB
it returns 503, including when a raised source override would otherwise admit
a 38 MiB bundle. Counting uses a non-allocating writer, not an approximate ratio.

## Reproduction and receipts

From this transport checkout (Git credentials are used only for the authorized
clone/submodule initialization; captured requests contain dummy capabilities):

```bash
git clone https://github.com/lfglabs-dev/lido-srv3-proof-closure.git ../lido-fixture
git -C ../lido-fixture fetch origin f27b39d8f12b1d8b5947c7f664ebfc33e0183019
git -C ../lido-fixture fetch origin 2bb0a7dc7bf009333ba925b2a3a2e02503297b76
git -C ../lido-fixture fetch origin 8691c7881a863715ab5ec9b39631ab41243e7c91
python3 scripts/measure_remote_source_capacity.py ../lido-fixture ../lido-capacity
python3 scripts/test_remote_lean_build_source.py -v
cargo fmt --all --check
cargo test --locked --lib api::remote_build::tests:: -- --test-threads=2
cargo test --locked --lib node::lean::tests:: -- --test-threads=2
# Repeat for reserve-current, alloc2, alloc1, combined:
REMOTE_BUILD_MEASURED_FIXTURE=../lido-capacity/combined.request.gz \
  cargo test --locked --lib measured_complete_fixture -- --ignored --test-threads=1
```

Use debug Rust builds only. The fixture script writes `sizes.json`, every
`*.files.json` (path, bytes, executable mode, SHA-256), actual `*.request.gz`
captures and `union-provenance.json`. Captures are local source evidence and
are not checked into this transport repository. File paths, decoded bytes,
modes and both digests are verified against the recursive checkouts. The
manual Rust tests pass each captured request through the actual API decoder
and receiver validator, then materialize and compare every byte and mode
without source fetching. Run the permission-based wrapper regression as an
unprivileged user; root bypasses its chmod-based failure fixture.

The offline CI suite includes exact 32 MiB sender/API/receiver acceptance,
32 MiB + 1 rejection, explicit lower sender/receiver overrides, a raised
sender override, exact 64 MiB valid JSON and gzip expansion max + 1 rejection,
and byte/mode/manifest tamper rejection. Existing recursive-submodule and
full/overlay fidelity regressions remain in the required CI suite.

Manifest / operations digests for the measured captures:

- reserve-current: `29d2ebcea8a18aa1a26ccef32241872a229555cc059a52b16fd48afb0cf425e1` / `8755a4d7a258b6cc69eff78f7debb57d00a8d91bbe8d447752408b89534969de`
- alloc2: `85263f0ee092971dc46aabcf04c4cbebeb5be804478ce6d01051cd0045d82433` / `87a048184bf9459365950a3947b15fe8240207da76c69a133d1d0b2b8dc4916c`
- alloc1: `02a61d583e976f76535656e6a840101387543ad6df035c9180d73466dc7e5aca` / `b881aed46a63c5059ac63ed1790d36928317eaa9d087fe9e5580f6df690c1374`
- combined: `895328fdae836f7189a79ef8e1ef1a76bc4c2f64c98b4a14fef5333736f45028` / `364f62fecf4d295a1c81cbd331e7ab4943fe7653b914da04c503b1f4c8f02bb1`
