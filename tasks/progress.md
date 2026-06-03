# Progress Log: Ski Resort Index Pipeline

## Session: 2026-06-03 - Rust OpenSkiMap GeoJSON Rebuild

### Scope
- Rebuild SkiNavIndexes from the legacy Python/Overpass monthly indexer into a Rust OpenSkiMap GeoJSON artifact pipeline.
- Coordinate with SkiNav so locally generated artifacts can be tested on device before any GitHub Actions/release publishing.

### Current Status
- User approved Rust as the implementation language.
- User clarified the old project docs are not authoritative.
- Authoritative migration input is `/Users/eli/Downloads/GeoJSON Integration Strategy.pdf` plus the current plan.
- Legacy Overpass and GeoPackage-preferred notes are now marked superseded.
- README and workflow docs now describe the Rust/OpenSkiMap GeoJSON CLI, one-download local cache policy, generated output layout, local app testing path, and explicit non-goals.
- GitHub workflow surface is now a Rust CLI smoke-check path only; local generation plus on-device/simulator SkiNav validation remains the first acceptance path.
- Implemented the Rust CLI with `fetch`, `build`, `validate`, and `all` commands.
- Downloaded the 2026-06-03 OpenSkiMap source snapshot once into `data/raw/openskimap/2026-06-03/`: `ski_areas.geojson`, `runs.geojson`, and `lifts.geojson`.
- Final release-mode cached build completed from local files only:
  - Resorts: 4,494
  - Runs: 96,152
  - Lifts: 23,690
  - Groups: 627
  - Output size: 5.6 GB
  - Local app seed size: 1.7 GB
- `cargo test` and `cargo run --release -- validate` passed.
- SkiNav focused tests passed for local artifact loading and string OpenSkiMap IDs.
- Size-reduction follow-up removes redundant `rendering_features.geojson`, omits per-resort raw `runs.geojson`, writes domain packages as child-reference metadata, and hard-links local-app render files to package files when possible.
- After the size reduction rebuild, generated `output/` dropped from 5.6 GB to 1.6 GB. `output/local-app/` is about 1.3 GB when measured by itself, but adds little extra disk usage inside the combined `output/` tree on APFS because the render files are hard links to package files.

### Constraints
- Download OpenSkiMap GeoJSON layers once and process repeatedly from the local cache.
- Use GeoJSON layer files, not GeoPackage.
- Do not push to GitHub just to test the rebuilt indexer.
- Update README/docs after implementation reflects the new reality.

### Follow-Up Performance Note
- The compact release build is usable, but peak RSS is still high because the first full implementation keeps normalized run/lift geometry in memory while writing packages.
- A future hardening pass should stream/spool runs and lifts by resort instead of retaining all feature `serde_json::Value` payloads at once.

## Session: 2026-02-06

### Completed
- [x] Phase 1: Overpass Query - 952 elements (896 ways, 56 relations)
- [x] Phase 2: Normalization Script - hierarchy detection, bbox padding, multilingual names
- [x] Phase 3: Validation Script - schema + semantic validation
- [x] Phase 4: JSON Schema - draft-07 compliant
- [x] Phase 5: GitHub Actions - monthly cron workflow
- [x] Phase 6: Documentation - README, requirements.txt, latest.json
- [x] Phase 7: Data quality improvements - polygon country detection + skiable-area feasibility

### Statistics
- Total resorts: 952
- Domains (parents): 27
- Child resorts: 140
- With country codes: 952 (100%)
- With multiple names: 72

### Country Distribution
- CH: 279
- AT: 239
- FR: 187
- IT: 180
- DE: 46
- SI: 18
- LI: 1
- HR: 2

---

## Test Results
| Test | Result | Notes |
|------|--------|-------|
| Overpass query | PASS | 952 elements, 1.5MB |
| Normalization | PASS | All edge cases handled |
| Validation (valid) | PASS | 0 errors |
| Validation (broken) | PASS | Catches errors correctly |
| YAML syntax | PASS | Workflow valid |
| End-to-end | PASS | Full pipeline works |
