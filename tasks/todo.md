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
