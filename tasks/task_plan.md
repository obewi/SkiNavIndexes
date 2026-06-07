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

---

# Artifact Simplification, Terrain Parks, Elevation, Spots, and SkiNav Rendering Plan

> **For future compacted context:** This is the current plan for the cross-repo SkiNavIndexes and SkiNav work discussed on 2026-06-06. It intentionally supersedes older assumptions that only `downhill` runs matter and that `lift_stations.geojson` must remain a standalone runtime file. No backward compatibility or migration path is required because this artifact shape is not in production.

**Goal:** Refactor the Rust indexer into multiple files, reduce duplicated artifact storage, preserve OpenSkiMap elevation and spot data, add terrain park/spot/polygon rendering support in SkiNav, and fix the installed-artifact pipeline so map rendering and direction inference use the data OpenSkiMap already exports.

**High-level architecture:** Keep the OpenSkiMap source files as the broad truth layer during fetch/build, but emit lean per-resort app artifacts with repeated source-only assignment data removed. Use source `skiAreas` only for assigning features to leaf resort packages; do not copy it into final run/lift/spot/connection artifacts. Keep all OpenSkiMap spots in the dataset so SkiNav can add more spot rendering later, while implementing road-crossing rendering first.

## Confirmed Source Facts

- Raw cached OpenSkiMap snapshot inspected: `data/raw/openskimap/2026-06-03`.
- `runs.geojson` contains `227796` run features.
- Every raw run feature has `skiAreas`; this is assignment/provenance data and is too large to repeat in app artifacts.
- `213873` run features have `elevationProfile`.
- Run `elevationProfile` key set is exactly:
  - `heights`
  - `resolution`
  - `targetResolution`
- Run coordinates are already 3D in many cases: `[longitude, latitude, elevation]`.
- Run `uses` values and notable counts:
  - `downhill`: `108344`
  - `nordic`: `96401`
  - `hike`: `10060`
  - `skitour`: `6307`
  - `sled`: `4233`
  - `connection`: `2594`
  - `snow_park`: `1520`
  - `fatbike`: `1379`
  - `sleigh`: `664`
  - `ice_skate`: `433`
  - `playground`: `316`
- `snow_park` appears in `uses`, not as a separate `piste:type` field in the sampled raw run.
- Raw polygon counts observed:
  - all polygon or multipolygon run features: `13923`
  - polygon features with `downhill`: `12243`
  - polygon features with `snow_park`: `487`
  - polygon features with `playground`: `176`
  - polygon features with `sled`: `676`
- `lifts.geojson` contains `33049` lift features.
- Every raw lift feature has `skiAreas`.
- `16036` raw lift features already embed station objects in `properties.stations`.
- Raw lift features do not have `elevationProfile`.
- Embedded lift station point geometries can be 3D and have their own nested `skiAreas`, which should also be pruned in final app artifacts.
- OpenSkiMap exposes `spots.geojson` at `https://tiles.openskimap.org/geojson/spots.geojson`; it responded `HTTP 200` with the existing SkiNavIndexes user agent and was about 33 MB on 2026-06-06.
- OpenSkiMap's processor turns OSM spots with `piste:dismount=yes|no|sometimes` into crossing spots:
  - `spotType = "crossing"`
  - `dismount = "yes" | "no" | "sometimes"`
- OpenSkiMap's frontend legend labels:
  - `dismount=yes`: Road Crossing, Ski removal required.
  - `dismount=sometimes`: Road Crossing, Ski removal sometimes required.
  - `dismount=no`: crossing exists, no ski removal required; keep in data even if not prominently rendered.

## Rust Refactor

- [ ] Split `src/main.rs` into focused Rust modules without changing behavior first:
  - `cli.rs`: clap command definitions and dispatch.
  - `fetch.rs`: OpenSkiMap and Overpass fetching.
  - `source.rs`: GeoJSON loading, source feature parsing, and source metadata.
  - `normalize.rs`: run, lift, connection, spot, and resort normalization.
  - `resorts.rs`: resort hierarchy, leaf/domain assignment, feature-to-resort assignment, grouping.
  - `artifacts.rs`: per-resort app artifact generation.
  - `release.rs`: group archives, release packs, and local-app copy layout.
  - `geo.rs`: bbox, endpoint, line/polygon, distance, and coordinate helpers.
  - `validation.rs`: output validation.
  - `main.rs`: thin entrypoint only.
- [ ] Keep all current tests passing after the split before making behavior changes.
- [ ] Add focused unit tests near each extracted module instead of leaving all tests at the bottom of `main.rs`.

## New App Artifact Contract

- [ ] Bump the render/package schema version after the artifact shape changes.
- [ ] No compatibility/migration support is needed. Remove old required files instead of maintaining dual paths.
- [ ] Replace the confusing `centerline` terminology where possible:
  - Current `downhill_centerlines.geojson` is not a computed geometric centerline.
  - It is an exploded per-line section artifact.
  - Rename internally and, if practical, externally to `run_sections.geojson`.
- [ ] Recommended output shape:
  - `runs.geojson`: sanitized original app-renderable line and polygon features, preserving elevation where available.
  - `run_sections.geojson`: derived per-line section/topology features with minimal duplicated properties.
  - `lifts.geojson`: sanitized lift lines with embedded sanitized station data.
  - `spots.geojson`: all assigned OpenSkiMap spots, sanitized.
  - `connections.geojson`: sanitized connection features.
  - `connection_sections.geojson`: derived connection section/topology features with minimal duplicated properties.
- [ ] Remove standalone `lift_stations.geojson`; station data should come from embedded `lifts.geojson` stations.
- [ ] Consider removing separate `downhill_lines.geojson` and `downhill_polygons.geojson` by combining both geometry types into `runs.geojson`, while preserving a clear way for SkiNav map layers to source line and polygon layers from the same GeoJSON.

## Property Pruning and Storage Reduction

- [ ] Add a shared Rust app-artifact property sanitizer.
- [ ] Strip these assignment/provenance keys from final per-feature artifacts:
  - `skiAreas`
  - `skiAreaIds`
  - `ski_area_ids`
  - `ski_area`
- [ ] Apply the sanitizer to:
  - run line features
  - run polygon features
  - run section features
  - lift features
  - nested lift station feature objects inside `lifts.properties.stations`
  - spot features
  - connection features
  - connection section features
- [ ] Keep resort membership at package/manifest/tree level:
  - package path
  - `manifest.json.resortId`
  - `artifact_manifest.json`
  - domain `childIds`
- [ ] Do not duplicate the full source property payload in section artifacts. Keep only section-specific/topology fields plus a compact source reference:
  - `source_way_id`
  - `run_key` or `selection_key`
  - `completion_family_key`
  - `completion_section_id`
  - endpoint keys
  - direction fields
  - display-critical labels/difficulty/type
  - elevation/profile data if needed for downstream processing

## Uses Filtering

- [ ] Replace the current Rust "must contain downhill" run filter with explicit app-supported categories.
- [ ] Include for run lines/sections:
  - `downhill`
  - `snow_park`
- [ ] Include for run polygons:
  - `downhill`
  - `snow_park`
  - `playground`
- [ ] Keep `connection` handled by the connection artifact path, not by runs.
- [ ] Do not include by default:
  - `nordic`
  - `hike`
  - `fatbike`
  - `sleigh`
  - `ice_skate`
- [ ] Investigate before adding:
  - `sled`
  - `skitour`
- [ ] Add a test that standalone `snow_park` features are included even when `uses` does not contain `downhill`.
- [ ] Add a test that standalone `playground` polygons are included in polygon output.
- [ ] Add a test documenting that `sled` and `skitour` are intentionally excluded until a routing/rendering policy is chosen.

## Terrain Park Support

- [ ] Normalize `uses` to an app piste type:
  - if `uses` contains `snow_park`, app piste type should be `snow_park`.
  - display label should be `Terrain Park`.
- [ ] Preserve raw `uses` in sanitized output unless storage measurements show it is too expensive; it is useful for future rendering decisions.
- [ ] In SkiNav, update `IndexArtifactSkiDataLoader` so it does not default every non-downhill feature to `downhill`.
- [ ] In SkiNav map styling, render `snow_park` lines and polygons distinctly and correctly.
- [ ] In SkiNav Explore/detail UI, show terrain parks as `Terrain Park`.
- [ ] Add Rust and Swift tests for `snow_park` line, section, polygon, loader, render bundle, and map style coverage.

## Elevation Preservation and DEM Fallback Fix

- [ ] Preserve 3D coordinates from OpenSkiMap run and lift geometries.
- [ ] Preserve run `elevationProfile` on run line and section artifacts:
  - `heights`
  - `resolution`
  - `targetResolution`
- [ ] Do not add elevation profile to polygons unless it is already present and needed; polygons do not currently care.
- [ ] SkiNav should use elevation in this order:
  - 3D coordinate elevation if present.
  - `elevationProfile.heights` interpolation if 3D coordinates are absent or incomplete.
  - DEM fallback only when neither source has enough coverage.
- [ ] Fix the current symptom where Kappl appears to trigger DEM fallback for every run.
- [ ] Add a focused SkiNav test where installed-artifact runs with 3D coordinates and/or `elevationProfile.heights` do not call the DEM sampler during direction inference.
- [ ] Add a real/generated Kappl validation check after implementation: process Kappl and verify fallback logs are not emitted for every run when source elevations exist.

## Lift Station Optimization

- [ ] Keep embedded lift station data in `lifts.geojson`; do not emit standalone `lift_stations.geojson`.
- [ ] Sanitize embedded station properties, especially nested `skiAreas`.
- [ ] In SkiNav, load station records from `lifts.properties.stations`.
- [ ] Use embedded station elevation/position to enrich lift endpoint elevations.
- [ ] Update map source descriptors/layers to stop requiring a separate `ski-lift-stations` source if stations can be rendered from the lift artifact or a generated in-memory source.
- [ ] Add tests proving embedded lift stations are decoded, used for endpoint elevation enrichment, and rendered where expected.

## Spots and Road Crossings

- [ ] Fetch `spots.geojson` alongside:
  - `ski_areas.geojson`
  - `runs.geojson`
  - `lifts.geojson`
  - `connections.geojson`
- [ ] Keep all OpenSkiMap spots in final per-resort data, not only crossings, so future spot types can be shown without changing the source contract again.
- [ ] Use source `skiAreas` for assignment, then prune it from emitted spot features.
- [ ] Assign spots to leaf resort packages with the same hierarchy rules as other features:
  - explicit source `skiAreas` first
  - source relations if available
  - spatial fallback only if needed and safe
- [ ] Emit sanitized `spots.geojson` per resort.
- [ ] Include spot counts in manifests, build report, audit report, local-app layout, group archives, and release packs.
- [ ] In SkiNav, load all spots and model at least:
  - `spotType`
  - `dismount`
  - point geometry
  - name/source metadata if available
- [ ] Render road crossings first:
  - `spotType = "crossing"` and `dismount = "yes"`: Road Crossing, ski removal required.
  - `spotType = "crossing"` and `dismount = "sometimes"`: Road Crossing, ski removal sometimes required.
  - `spotType = "crossing"` and `dismount = "no"`: keep in data; render subtly or debug-only unless product decision changes.
- [ ] Add tests for all three `dismount` values.
- [ ] Add map legend support in SkiNav for required and sometimes-required road crossings.

## Downhill Polygon Loss Fix

- [ ] Fix SkiNav `IndexArtifactSkiDataLoader`: it currently loads only line artifacts and hardcodes `isArea = false`.
- [ ] Load polygon features from the new combined `runs.geojson` or from polygon artifact if the final shape stays split.
- [ ] Decode polygon and multipolygon geometry into `OSMPiste` with `isArea = true`.
- [ ] Preserve `pisteType` from `uses`, `piste_type`, or source `type`.
- [ ] Ensure `SkiRenderBundleService` receives `isArea` pistes so `downhill_polygons.geojson` is non-empty.
- [ ] Keep/extend current SkiNav expectation that renderable polygon types are:
  - `downhill`
  - `snow_park`
  - `playground`
- [ ] Add an installed-artifact loader regression test proving polygon artifacts survive from source package to render bundle.
- [ ] Add a map validation check that polygon fill/outline layers render after local package processing.

## Verification Plan

- [ ] Rust:
  - `cargo fmt`
  - `cargo test`
  - `cargo run --release -- fetch --dataset-version 2026-06-03`
  - `cargo run --release -- all --dataset-version 2026-06-03 --skip-fetch`
  - `cargo run --release -- validate`
- [ ] Structured artifact inspections:
  - no final feature properties contain `skiAreas` or equivalent assignment keys
  - `snow_park` line/section/polygon features exist
  - standalone `playground` polygons exist
  - run elevation profiles keep `heights`, `resolution`, `targetResolution`
  - lift stations are embedded and standalone station artifact is gone
  - all assigned spots are emitted
  - road crossings include `yes`, `sometimes`, and `no` dismount values when present in source
- [ ] SkiNav focused tests:
  - `IndexArtifactSkiDataLoaderTests`
  - `PisteDirectionInferenceServiceTests`
  - `SkiRenderBundleServiceTests`
  - `MapLibreMapViewTests`
  - any new spot/crossing loader and map layer tests
- [ ] Use XcodeBuildMCP or normal Xcode test commands for SkiNav verification after implementation.
- [ ] Manual/local runtime validation:
  - install generated local app artifacts
  - process Kappl
  - confirm elevation fallback is not triggered for every run
  - confirm downhill polygons render
  - confirm terrain parks render as terrain parks
  - confirm road crossings render/legend correctly

## Documentation Updates

- [ ] Update `README.md` with the new artifact contract.
- [ ] Document that `skiAreas` is source-only assignment data and is intentionally pruned from app artifacts.
- [ ] Document the `uses` policy and the excluded-but-observed values.
- [ ] Document elevation preservation and DEM fallback order.
- [ ] Document `spots.geojson` and road crossing semantics.
- [ ] Document that no compatibility/migration path is required for this pre-production artifact change.
