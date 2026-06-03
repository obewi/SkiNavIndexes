# Current Task Plan: Rust OpenSkiMap Index Pipeline

## Authoritative Inputs

- `https://tiles.openskimap.org/geojson/lifts.geojson`
- `https://tiles.openskimap.org/geojson/ski_areas.geojson`
- `https://tiles.openskimap.org/geojson/runs.geojson`

## Current Implementation

- [x] Rust CLI with `fetch`, `build`, `validate`, and `all` commands.
- [x] One-download-per-dataset cache under `data/raw/openskimap/<dataset-version>/`.
- [x] Discovery index generation for current SkiNav clients.
- [x] Per-resort packages for leaf resorts.
- [x] Reference-only domain packages so parent areas do not duplicate child artifacts.
- [x] Group archive generation.
- [x] Local app seed layout for simulator/device validation.
- [x] Release workflow for manual GitHub Actions builds and optional GitHub Release publication.
- [x] Generated output cleaned before rebuild to prevent stale deleted files from lingering.
- [x] Generated `output/*` ignored by Git; root `latest.json` remains the tracked app entrypoint.

## Validation Requirements

- [x] `cargo test`
- [x] `cargo run --release -- validate`
- [x] Focused SkiNav integration tests for discovery decoding and local artifact loading.
- [ ] GitHub Actions release workflow dry run on pushed branch.
- [ ] SkiNav integration check against pushed/released `latest.json` and `resorts.json`.

## Follow-Up Branches

- Incremental source snapshot/change detection: compare current and previous OpenSkiMap source manifests, skip rebuild when unchanged, and report changed areas.
- Archive packing strategy: rebalance oversized and tiny group archives into client-friendly release chunks.
- Client artifact lifecycle: version downloaded generated artifacts per resort, atomically promote verified versions, and prune older non-current versions through the downloaded-region lifecycle.
