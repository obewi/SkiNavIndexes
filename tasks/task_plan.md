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
- [x] Experimental balanced release packs split very large groups and combine very small groups.

## Validation Requirements

- [x] `cargo test`
- [x] `cargo run --release -- validate`
- [x] Focused SkiNav integration tests for discovery decoding and local artifact loading.
- [ ] GitHub Actions release workflow dry run on pushed branch.
- [ ] SkiNav integration check against pushed/released `latest.json` and `resorts.json`.

## Follow-Up Branches

- Incremental source snapshot/change detection: compare current and previous OpenSkiMap source manifests, skip rebuild when unchanged, and report changed areas.
- Archive packing strategy: test balanced `output/release-packs` artifacts against client download behavior before promoting to mainline release automation.
- Client artifact lifecycle: version downloaded generated artifacts per resort, atomically promote verified versions, and prune older non-current versions through the downloaded-region lifecycle.

---

# OpenSkiMap Connection Enrichment Implementation Plan

> **For agentic workers:** Implement task-by-task and keep this checklist current. Do not start implementation until Eli approves this plan.

**Goal:** Add connection source features to the SkiNavIndexes dataset, using OpenSkiMap GeoJSON directly when it contains `type=connection` features and falling back to a global Overpass query for raw OSM `piste:type=connection` when it does not.

**Architecture:** Keep connection data as a first-class cached source layer so `build --skip-fetch` stays deterministic. `fetch` decides whether OpenSkiMap is already fixed by scanning cached OpenSkiMap GeoJSON for features whose properties have `type=connection`; only if absent does it POST a global Overpass QL query for raw OSM `piste:type=connection` to `https://overpass-api.de/api/interpreter` with a SkiNavIndexes user agent, convert the Overpass JSON response to OpenSkiMap-compatible GeoJSON, and cache it as `connections.geojson`. `build` reads the cached connection layer, assigns connection features to resort packages by spatial overlap/snapping, and emits connection artifacts plus manifest/report counts.

**Current source check:** The cached `data/raw/openskimap/2026-06-03/*.geojson` files contain neither OpenSkiMap `type=connection` features nor raw `piste:type=connection` tags, so the fallback path is needed for the current snapshot.

**Documentation check:** `ctx7` resolved Overpass API docs to `/websites/wiki_openstreetmap_wiki_overpass_api` and returned current Overpass QL examples. The installed `ctx7` CLI rejected `docs ... --research` with `unknown option '--research'`, so implementation should proceed from the resolved docs plus the explicit endpoint and user-agent requirements in this task.

## Files

- Modify `src/main.rs`: CLI options, source fetch/cache logic, OpenSkiMap connection detection, Overpass JSON fetch/convert helpers, connection normalization, resort assignment, package output, validation, and tests.
- Modify `README.md`: document the connection source behavior, Overpass fallback, cache file, user agent, and deterministic `build --skip-fetch` contract.
- Modify `tasks/findings.md`: replace the old "Overpass is intentionally not a fallback" finding with the new narrow fallback rule for missing upstream connections.
- Modify `tasks/progress.md`: record the connection-enrichment implementation and validation results after execution.

## Task 1: Source Contract and CLI Shape

- [x] Add a `connections.geojson` cache contract under `data/raw/openskimap/<dataset-version>/`.
- [x] Keep `LAYER_FILES` limited to OpenSkiMap's three upstream files, and add a separate `CONNECTIONS_FILE` constant so source metadata can distinguish upstream OpenSkiMap layers from derived/fallback enrichment.
- [x] Add fetch/all CLI options:
  - `--overpass-base-url`, default `https://overpass-api.de/api/`
  - `--skip-connection-enrichment` only for controlled debugging; default must enrich connections
- [x] Keep `build` network-free. If `connections.geojson` is missing and OpenSkiMap has no `type=connection` features, fail with a clear "run fetch first" error rather than querying Overpass during build.

## Task 2: OpenSkiMap Detection First

- [x] Add `openskimap_has_connections(dataset_dir: &Path) -> Result<bool>` that scans the cached OpenSkiMap source files for features whose properties contain exact `type = "connection"`.
- [x] Add `write_connections_from_openskimap(dataset_dir: &Path)` that extracts those OpenSkiMap features into `connections.geojson` when upstream is fixed.
- [x] Preserve source IDs and source metadata. If the feature has `sources`, keep it; otherwise preserve GeoJSON `id` and use the same `source_id` fallback style as runs/lifts.
- [x] Unit test that exact OpenSkiMap GeoJSON `type=connection` is detected, while unrelated OpenSkiMap feature types and missing tags are ignored.

## Task 3: Overpass Fallback Fetch

- [x] Build an HTTP client with a required user agent, e.g. `SkiNavIndexes/0.1 (connection enrichment; contact: github.com/obewi/SkiNavIndexes)`.
- [x] POST to `overpass_base_url.trim_end_matches('/') + "/interpreter"` using form body field `data`.
- [x] Use this Overpass QL shape:

```text
[out:json][timeout:900];
(
  way["piste:type"="connection"](-90,-180,90,180);
  relation["piste:type"="connection"](-90,-180,90,180);
);
out body geom;
```

- [x] Convert Overpass `way` elements with `geometry` arrays to GeoJSON `LineString` features with OpenSkiMap-compatible `type=connection` in properties while preserving raw OSM tags, including `piste:type=connection`.
- [x] Convert Overpass `relation` elements with member geometries to `MultiLineString` features when possible, also setting OpenSkiMap-compatible `type=connection`.
- [x] Ignore non-linear or geometry-less elements with warnings in `source_metadata.json`; do not silently count them as usable connections.
- [x] Cache the converted FeatureCollection at `connections.geojson` and validate it like other GeoJSON layers.
- [x] Record the Overpass URL, query hash, fetched timestamp, feature count, ignored-element count, and SHA-256 in `source_metadata.json`.
- [x] Unit test Overpass JSON conversion with one way, one relation, and one ignored malformed element. No unit test should hit the real Overpass endpoint.

## Task 4: Build-Time Connection Normalization

- [x] Extend `NormalizedDataset` with `connections: Vec<FeatureRecord>`.
- [x] Read `connections.geojson` during `build_from_cache`.
- [x] Normalize only line or multiline connection geometries with OpenSkiMap GeoJSON `type=connection`; accept preserved raw `piste:type=connection` only as a fallback marker for converted Overpass fixtures.
- [x] Keep connection status filtering conservative: skip only explicit non-operating status values when present.
- [x] Assign each connection to the correct resort package before any package, group archive, or release-pack generation:
  - First pass: use explicit `skiAreas` associations when OpenSkiMap supplies them, filtered to known resort IDs.
  - Source-ID pass: for Overpass-derived connections, preserve `osm_type/osm_id` and `source = way/<id>` or `relation/<id>` metadata, then check whether any existing OpenSkiMap run/lift feature references the same OSM source in its `sources` array; if so, inherit that feature's resort IDs.
  - Candidate filtering: use padded resort/run/lift bboxes only to reduce the search space; bbox overlap alone must not be the final assignment because it can put a connection in the wrong tarball.
  - Network pass: snap connection endpoints and nearby line vertices to existing run/lift endpoints or segments within a small fixed meter threshold, then inherit the matched resort IDs. Score candidates by endpoint matches first, segment proximity second, and reject weak matches where the connection merely passes through a broad resort bbox.
  - Resort-area fallback: if the ski-area polygon/point geometry is usable, use it only to break ties between otherwise plausible network candidates, not as standalone proof.
  - Multi-resort handling: if a connection legitimately bridges two resort networks, include it in both resort packages so both generated tarballs contain the routing edge.
  - Orphan handling: if no explicit or network assignment is found, skip the connection and emit a warning with the source ID instead of placing it in a broad bbox match.
- [x] Make assigned connections contribute to package-level graph/routing source counts, but do not let connection-only areas create new discovery resorts.
- [x] Add tests for explicit `skiAreas` assignment, OSM source-ID assignment, network proximity assignment, multi-resort bridge duplication, false-positive bbox rejection, tie-breaking, and orphan warning behavior using tiny synthetic fixtures.

## Task 5: Package Output and Manifests

- [x] Emit per-resort `connections.geojson` for leaf resort packages.
- [x] Emit per-resort `connection_centerlines.geojson` with the same endpoint metadata shape as downhill centerlines:
  - `type = "connection"`
  - preserved `piste:type = "connection"` when the source came from Overpass
  - `feature_kind = "connection"`
  - `run_key`, `completion_family_key`, `completion_section_id`, `centerline_id`
  - `start_endpoint_key`, `end_endpoint_key`
  - direction fields using the current oneway logic
- [x] Add connection file paths and counts to `manifest.json`, `artifact_manifest.json`, `audit_report.json`, local-app render bundle manifests, and graph metadata.
- [x] Verify group archives and release packs include connections indirectly by copying the resort package files; do not implement a separate archive-level connection grouping path that can drift from resort membership.
- [x] Do not merge connections into `downhill_lines.geojson` or `downhill_polygons.geojson`; keep current visual piste artifacts backward-compatible.
- [x] If downstream SkiNav graph loading needs one combined centerline file, add that as an explicit compatibility artifact instead of changing the meaning of existing downhill render files.

## Task 6: Validation and Release Safety

- [x] Run `cargo fmt`.
- [x] Run `cargo test`.
- [x] Run `cargo run -- --help` and confirm new fetch/all options are visible.
- [x] Run `cargo run --release -- fetch --dataset-version 2026-06-03` once to populate `connections.geojson` through OpenSkiMap detection or Overpass fallback.
- [x] Run `cargo run --release -- all --dataset-version 2026-06-03 --skip-fetch`.
- [x] Run `cargo run --release -- validate`.
- [x] Inspect `output/build-report.json` for `connectionCount`, skipped connection warnings, and unchanged resort/run/lift counts except where connection assignment expands package source counts.
- [x] Inspect at least one resort package containing connections and verify `connections.geojson`, `connection_centerlines.geojson`, manifests, checksums, local-app files, group archives, and release packs include the new files.

## Task 7: Documentation and Findings

- [x] Update `README.md` Data Source and One-Download Cache Policy sections with the connection enrichment behavior.
- [x] Document that Overpass is only used during `fetch` when OpenSkiMap lacks GeoJSON `type=connection`; it is never used by `build`.
- [x] Document the endpoint, user-agent requirement, and public endpoint caveat.
- [x] Update `tasks/findings.md` to remove the stale blanket "Overpass is intentionally not a fallback" rule and replace it with the narrow connection fallback rule.
- [x] Update `tasks/progress.md` with final counts and validation commands after implementation.

## Approval Checkpoint

- [x] Eli approves this plan before implementation starts.
