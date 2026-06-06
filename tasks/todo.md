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
