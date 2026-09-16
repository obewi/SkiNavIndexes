# Per-scope source fingerprints and resilient Overpass enrichment (2026-09-16)

## Mode

- `implement`

## Goal

- Publish one compact, deterministic fingerprint for every resort's canonical
  processing scope; reuse successful Overpass enrichment across dataset runs;
  and expose the fingerprint in SkiNav diagnostics without automatic
  reprocessing.

## Constraints

- Keep the fingerprint fixed-width and cheap to compare: versioned SHA-256,
  first 128 bits, 32 lowercase hexadecimal characters.
- Exclude release dates, dataset versions, generated timestamps, and pack
  layout from the digest; repeat one scope value for every root/descendant
  resort.
- Use a rolling GitHub Release asset for persistent Overpass data, not
  GitHub Actions cache storage.
- Refresh per-station/connection entries after 120 days, pace and retry
  requests, and preserve stale data when the affected result is cached.
- Do not add mobile invalidation or automatic reprocessing in this slice.

## Checklist

- [x] Add producer fingerprints to `latest.json` and `catalog.sqlite`.
- [x] Add persistent Overpass cache, bounded station batches, fallback
  endpoints, retry/backoff, and release persistence.
- [x] Add SkiNav backward-compatible decoding and debug-menu visibility.
- [x] Verify Rust tests/formatting and the SkiNav simulator build; record the
  existing app test-target compiler blocker.

## Stop condition

- Both repositories publish/consume the additive fingerprint contract, the
  producer reuses cached Overpass data safely, and SkiNav shows the digest
  without changing processing decisions.

## Status

- Complete for the producer contract, persistent cache, and SkiNav
  compatibility/debug-only slice. Mobile reprocessing remains intentionally
  deferred.

## App-Owned Render Detail Contract Cleanup (2026-06-06)

## Goal
- Remove redundant app-facing index fields and stop generating app-owned Explore detail and run matching hint artifacts from SkiNavIndexes.

## Plan
- [x] Remove `artifactManifestPath` and `names` from generated `resorts.json` and schema/tests.
- [x] Keep one checksum/manifest source by removing standalone `checksums.json` while retaining `artifact_manifest.json.files`.
- [x] Stop generating, validating, archiving, and local-app linking `explore_detail.json`.
- [x] Stop generating, validating, archiving, and local-app linking `run_matching_hints.json`.
- [x] Update SkiNav to decode/search/install/process the slimmer contract.
- [x] Run focused Rust and SkiNav tests plus diff whitespace checks.

## Review
- `resorts.json` now carries only primary `name` for resort display/search identity; `names` and `artifactManifestPath` are removed from the Rust model and JSON schema.
- Per-package integrity metadata now lives in `artifact_manifest.json.files`; standalone `checksums.json` is no longer written.
- SkiNavIndexes no longer writes or references `run_matching_hints.json` or `explore_detail.json` in package output, local-app bundles, or validation.
- SkiNav keeps matching hints and Explore detail app-owned: release-pack install accepts seed bundles without those files, then app-side graph/render/detail processing regenerates them.
- Verification passed:
  - `PATH=/Users/obewillaert/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH cargo test`
  - `git diff --check`

# Worker A SkiNavIndexes Todo

## Source Pack Locality and Release Contract (2026-09-12)

### Goal

- Keep canonical parent coverage local, cap normal compressed source packs, pin catalog/pack acquisition to one immutable release, and remove the obsolete JSON discovery schema.

### Plan

- [x] Add canonical-root-aware, geographically local pack planning with deterministic size budgets.
- [x] Validate actual compressed pack sizes and complete canonical processing-scope download totals.
- [x] Add immutable release identity to `latest.json` and pin SkiNav asset downloads to it.
- [x] Remove the obsolete JSON discovery schema and update active V1/V2 contract references.
- [x] Restore and preserve the existing lift-station topology changes.
- [x] Run producer tests/build/validation and the app Debug build where the environment permits; the app test target remains blocked by pre-existing Swift 6 actor-isolation fixture errors.

### Acceptance

- [x] Compare the old and candidate layouts on the same cached source snapshot for Ischgl/Sölden, a small standalone resort, and multi-pack parent domains.
- [ ] Verify graph/Explore coverage and ownership parity on a device, plus zero same-release warm asset downloads in the released app.
- [x] Verify immutable-release URL construction with app tests and the producer release contract.

### Local evidence (2026-09-12)

- The old layout produced 10 packs totaling 101.3 MB; its Ischgl/Sölden pack was 11.3 MB.
- The candidate produced 721 geographically bounded packs totaling 94.9 MB; the largest normal pack was 6.45 MB. Ischgl/Sölden share a 5.82 MB pack.
- Les Trois Vallées, Les Portes du Soleil, and Dolomiti Superski each resolve to one local pack in the candidate snapshot.
- `cargo fmt -- --check`, `cargo test --locked`, `cargo run --release -- all --skip-fetch`, and standalone `validate` passed. The cached snapshot lacked the optional station-topology file, so that build intentionally contained no station memberships.
- The SkiNav Debug simulator build passed. The full SkiNav test target still cannot compile because of unrelated pre-existing Swift 6 actor-isolation errors in matcher/routing fixtures.


## Plan

- [x] Map current `src/main.rs` pipeline and preserve existing behavior boundaries.
- [x] Add focused failing tests for source/property sanitization, supported uses, elevation profile preservation, embedded lift stations, spots, and renamed section artifacts.
- [x] Split `src/main.rs` into a thin entrypoint plus separate CLI and pipeline modules.
- [x] Implement the new SkiNavIndexes artifact contract:
  - [x] Fetch/cache `spots.geojson`.
  - [x] Keep spots in normalized and per-resort artifacts.
  - [x] Prune source assignment keys from all final app artifact features, including nested stations and spots.
  - [x] Include `downhill`/`snow_park` line sections and `downhill`/`snow_park`/`playground` polygons while excluding other observed uses for now.
  - [x] Preserve run 3D coordinates and `elevationProfile` on line and section artifacts.
  - [x] Emit `run_sections.geojson` and `connection_sections.geojson`.
  - [x] Remove standalone `lift_stations.geojson` from the required/runtime artifact contract.
- [x] Update manifests, reports, local-app, release-pack paths/counts, README, and task results.
- [x] Run Rust formatting, tests, full cached build, and output validation.
- [x] Run focused SkiNav simulator tests for the new artifact contract and map rendering.
- [x] Build a three-resort local simulator seed with Ischgl, Saas Grund, and a crossing resort.
- [x] Launch SkiNav on the iPhone 17 simulator with the local simulator artifact root.

## Review

- Added a red/green end-to-end Rust fixture covering the new artifact contract.
- Verification:
  - `cargo fmt -- --check` completed with exit 0.
  - `cargo test` passed: 11 tests, 0 failures.
  - `cargo run --release -- fetch --dataset-version 2026-06-03` cached `spots.geojson`.
  - `cargo run --release -- all --dataset-version 2026-06-03 --skip-fetch` built and validated schema 24 output.
  - `cargo run --release -- validate` completed with exit 0.
  - Focused SkiNav `xcodebuild test` suites passed on `iPhone 17, iOS 26.5`.
  - `output/local-simulator-app` contains exactly Ischgl/Samnaun, Saas Grund, and Tannheim-Zöblen-Schattwald.
  - Tannheim-Zöblen-Schattwald contributes road crossings with `dismount=yes` and `dismount=sometimes`.

## Split Run Export and Export-Only Storage (2026-06-07)

## Goal
- Restore the split run export contract and remove duplicate local-app storage from SkiNavIndexes output.

## Plan
- [x] Add failing Rust coverage that expects split run artifacts:
  - `downhill_lines.geojson`
  - `downhill_polygons.geojson`
  - `downhill_centerlines.geojson`
- [x] Keep the useful current data-policy changes: `snow_park` lines/sections, `snow_park`/`playground` polygons, preserved 3D coordinates/elevation profiles, embedded lift stations, spots, and assignment-key pruning.
- [x] Remove `output/local-app` generation, validation assumptions, README references, and generated `latest.json.localArtifactRoot`.
- [x] Update package manifests, artifact manifests, release archive contents, and docs to match export-only storage.
- [x] Run formatting, focused tests, full Rust tests, and output validation.

## Review
- Added red/green Rust coverage for the restored split contract and no-local-app export layout.
- `manifest.json` now points at `downhill_lines.geojson`, `downhill_polygons.geojson`, and `downhill_centerlines.geojson`; combined `runs.geojson` and `run_sections.geojson` are no longer written into resort packages.
- `output/local-app` is no longer generated, and generated/root `latest.json` no longer carries `localArtifactRoot`.
- Verification passed:
  - `cargo test build_pipeline_writes_new_app_artifact_contract`
  - `cargo fmt -- --check`
  - `cargo test`
- `git diff --check`
- `cargo run --release -- all --dataset-version 2026-06-03 --skip-fetch`
- `cargo run --release -- validate`

## SQLite Index V2 — Canonical Release Cutover (2026-09-10)

### Goal

- Make the SQLite catalog and source packs the only generated and consumed resort-index contract.
- Remove the obsolete JSON discovery index, render-package/release-pack generation, and compatibility fallbacks.
- Publish the SQLite assets directly under `output/` so the release root is the client contract.

### Plan

- [x] Make canonical SQLite output write directly to `output/` and reject legacy output paths.
- [x] Remove JSON/package/archive generation, validation, workflow publishing, and Rust modules/tests.
- [x] Make SkiNav resolve the picker, repository regions, and processing scopes from `catalog.sqlite` only.
- [x] Remove SkiNav’s resort cache, JSON decoder, render-bundle installer, and release-pack fallback.
- [x] Run Rust formatting/tests/build/validation and SkiNav focused tests plus local simulator verification.

## SQLite Index V2 — Phase 1: Hierarchy and Ownership Invariants (2026-09-10)

### Goal

- Preserve direct ownership of runs, lifts, spots, and connections on any resort node, including inferred domain/parent nodes.
- Make normalized hierarchy and feature ownership fail-fast and cover the parent-only regression fixture from the V2 brief.
- Superseded by the canonical SQLite release cutover above; V1 is not a supported release layout.

### Plan

- [x] Remove leaf-only filtering from normalized feature assignment.
- [x] Validate resort IDs, parent references, tree cycles, and feature ownership after normalization.
- [x] Emit direct parent-owned features without copying child features into domain packages.
- [x] Add the Domain D / Child A / Child B / Run R parent-only regression.
- [x] Add canonical SQLite catalog and compressed source-pack generation.
- [x] Validate SQLite integrity, pack references, and the complete feature/ownership set.
- [x] Run focused tests, the full Rust test suite, formatting, and diff checks.

### Review

- Normalized run/lift/spot/connection ownership now accepts domain/parent IDs and preserves every matched owner.
- Resort hierarchy is stored only through `parent_id`; children are derived by the SkiNav catalog reader.
- The canonical source packs use WKB geometry BLOBs, binary `elevationProfile` height arrays, normalized feature tables, and ownership join tables.
- Validation runs SQLite quick/integrity/foreign-key checks, validates compressed asset hashes/metadata, checks catalog hierarchy and join completeness, and compares expected versus physical feature/ownership sets.
- Verification passed:
  - `cargo fmt -- --check`
  - `cargo build --release`
  - `cargo test`
  - `cargo test parent_owned_features_survive_hierarchy_normalization`
  - `git diff --check`

## Pipeline Module Refactor (2026-06-07)

## Goal
- Refactor the remaining monolithic Rust pipeline into focused modules without changing the generated artifact contract.

## Plan
- [x] Add a structure regression test that fails while `src/pipeline.rs` remains the large catch-all implementation.
- [x] Split pipeline responsibilities into focused modules for fetch/cache, build orchestration, data models, normalization, artifact output, release packs, validation, geometry helpers, and filesystem/JSON utilities.
- [x] Keep `src/main.rs` and `src/pipeline.rs` as thin entrypoints so the CLI contract stays stable.
- [x] Run formatting, full Rust tests, diff checks, cached release build, and generated-output validation.
- [x] Record the final module map and verification results here.

## Review
- `src/pipeline.rs` is now a 122-line command/orchestration module.
- Pipeline implementation now lives under `src/pipeline/`:
  - `build.rs`
  - `fetch.rs`
  - `geo.rs`
  - `io.rs`
  - `model.rs`
  - `normalize.rs`
  - `output.rs`
  - `release.rs`
  - `validate.rs`
  - `tests.rs`
- Added `tests/pipeline_structure.rs` so the pipeline does not regress back into a large catch-all file.
- Verification passed:
  - `cargo test pipeline_is_split_into_focused_modules` failed before the split with `src/pipeline.rs` at 3861 lines, then passed after the refactor.
  - `cargo fmt`
  - `cargo test`
  - `cargo fmt -- --check`
  - `git diff --check`
  - `cargo run --release -- all --dataset-version 2026-06-03 --skip-fetch`
  - `cargo run --release -- validate`
## Lift Station Topology Enrichment (2026-09-11)

### Goal

- Enrich OSM-backed lift stations with bounded station-to-lift topology, normalize it as a many-to-many relationship, and publish it in source schema v3 for SkiNav's routing and rendering paths.

### Plan

- [x] Add targeted Overpass station topology fetch/cache output and CLI control.
- [x] Normalize station/lift memberships and transfer-station annotations.
- [x] Publish and validate `lift_station_memberships` in SQLite source schema v3.
- [x] Update SkiNav's SQLite loader, station hub links, and transfer-station icon expression.
- [x] Add focused Rust/Swift coverage and update contract documentation.

### Verification

- [x] Run focused Rust tests and formatting.
- [x] Run the smallest available XcodeBuildMCP build/test check.
- [x] Record exact pass/skip/blocker results before closing this task.

Verification: Rust formatting and tests pass. The SkiNav simulator build passes through XcodeBuildMCP; the focused test target remains blocked by pre-existing Swift 6 global-actor errors in unrelated test fixtures before tests execute. The tracked release pointer remains the existing v2 release until a v3 dataset is generated and published with matching SQLite asset hashes.

## Pre-publication V3 Review (2026-09-14)

Mode: review. Scope: origin/main (3dd2ee6) through fe5854e; no production edits or publication.

- [x] Verify remote base and inspect release/catalog compatibility with SkiNav V3.
- [x] Run Rust tests, formatting, and diff checks.
- [x] Review station topology and pack ownership together.
- [x] Build and validate the cached real snapshot in an isolated temporary output directory.
- [x] Record actionable findings and publication readiness.

Initial verification: 19 unit tests plus 1 structure test passed; formatting and diff checks passed. The available 2026-09-10 cache lacks station topology, so cached-release validation cannot establish live enrichment acceptance.

Review result: not ready to publish V3. Two findings:

- P1: `src/pipeline/sqlite.rs:419` writes station memberships only when station and lift are already present in the same pack. In the exact cached snapshot, Winteregg lift `way/150270143` is in pack-0034 while its actual bottom station `node/1631925115` is in pack-0211. A rebuild using symlinked original cached inputs plus that one real membership fails with `SQLite lift station membership set mismatch: expected 1, actual 0`. Reproduction log: `/tmp/skinavindexes-topology-review.log`; fixture cache: `/tmp/skinavindexes-topology-review-cache`. Preserve station/lift membership availability across pack boundaries and add this real-data regression.
- P2: `src/pipeline/fetch.rs:135` ignores Overpass runtime-error remarks in successful HTTP JSON responses. A local HTTP 200 fixture containing a timeout remark and `elements: []` makes `fetch` exit 0 and persist an empty station membership cache. Reject runtime-error/partial responses before cache promotion. Probe cache: `/tmp/skinavindexes-overpass-review-20260914/cache/review`. The reference OverPy implementation separately rejects JSON remarks after accepting HTTP 200: https://python-overpy.readthedocs.io/en/latest/_modules/overpy.html#Overpass.parse_json

Passed: 19 Rust unit tests + 1 structure test, cargo formatting, diff whitespace checks, and current-HEAD release-mode `all --skip-fetch` against the 2026-09-10 real cache. Generated 4,523 resorts and 721 packs; largest compressed pack 6,446,918 bytes, total compressed packs 94,867,108 bytes. Output: `/tmp/skinavindexes-review-20260914`. Its metadata has schemaVersion 3 and releaseTag indexes-2026-09-10, matching the app metadata shape.

Limits: ordinary cached build had no station enrichment; the targeted enriched build fails as described. No complete live Overpass fetch, iOS runtime verification, source fixes, commits, pushes, or release publication. Pushing main does not trigger publication; after fixes, manually dispatch the release workflow with publishing enabled and an unused dataset version.

## V3 Review Fixes (2026-09-14)

Mode: implement. Scope: the two actionable findings from the pre-publication V3 review above.

- [x] Preserve lift-station memberships when topology endpoints have different source-pack owners.
- [x] Reject Overpass runtime-error remarks before enrichment cache promotion.
- [x] Add exact Winteregg cross-pack and simulated timeout regressions.
- [x] Re-run the cached real-data release build and output validation.

Review result: topology normalization now propagates the complete resort-owner set across each connected station/lift component. The existing SQLite pack writer consequently stores both endpoint rows and the membership in every relevant pack while retaining foreign-key and ownership-set validation. Overpass JSON with any `remark` is rejected before either topology or connection enrichment is cached.

Verification: 21 Rust unit tests plus 1 structure test passed; `cargo fmt -- --check` and `git diff --check` passed; the exact cached 2026-09-10 enriched Winteregg reproduction built successfully in release mode and standalone output validation passed. The membership and both endpoint rows were confirmed in `pack-0034.sqlite.gz` and `pack-0211.sqlite.gz`. No commits, pushes, or release publication were performed.
