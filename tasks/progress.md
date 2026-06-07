# Progress

## 2026-06-03

- Rebuilt SkiNavIndexes as a Rust OpenSkiMap GeoJSON pipeline.
- Cached the 2026-06-03 OpenSkiMap source snapshot locally and reused it for rebuilds.
- Removed redundant `rendering_features.geojson` and per-package raw `runs.geojson`.
- Changed domain packages to reference child artifacts instead of duplicating runs and lifts.
- Reduced generated output from 5.6 GB to 1.6 GB.
- Added local-app artifacts for SkiNav simulator/device validation.
- Updated SkiNav to accept string IDs and DEBUG local artifacts.
- Added a GitHub Actions release workflow for manual full builds and optional release publication.
- Added an experimental balanced release-pack layout for testing: oversized groups split into part assets and tiny groups combine into small-group packs.
- Switched repository URLs from the old owner to `obewi/SkiNavIndexes`.
- Removed legacy Python/Overpass scripts, requirements, stale schema, tracked generated `output/resorts.json`, and tracked `.DS_Store` files.

## Current Metrics

- Resorts: 4,494
- Domain records: 79
- Runs: 96,152
- Lifts: 23,690
- Assigned connections: 2,639
- Group archives: 627
- Generated output: 1.6 GB
- Source cache: 1.0 GB

## 2026-06-04

- Added `connections.geojson` source enrichment for OpenSkiMap connection features.
- Confirmed cached OpenSkiMap `2026-06-03` does not contain GeoJSON `type=connection`; fetched fallback raw OSM `piste:type=connection` data from Overpass.
- Cached 3,058 Overpass connection features in `data/raw/openskimap/2026-06-03/connections.geojson`; 3 malformed/non-linear elements were ignored during conversion.
- Built and validated the enriched 2026-06-03 output with 2,639 confidently assigned leaf-resort connections.
- Verified real Dolomiti connection `way/49436042` is packaged in Alta Badia leaf resort `41ca531357e0d2a532b8ab94e3e9fe74ddbe88c4`, not in Dolomiti Superski domain `480f0abbee27a7e26a20a29d9bf947db63bef9a9`.
- Verified Alta Badia connection files are present in `output/groups/IT-BL.tar.gz` and `output/release-packs/IT-BL.part-002-of-007.tar.gz`.

## 2026-06-06

- Split the Rust binary entrypoint into thin `src/main.rs`, `src/cli.rs`, and `src/pipeline.rs` modules.
- Added `spots.geojson` to the required OpenSkiMap source cache contract and normalized/assigned spots into leaf resort packages.
- Replaced runtime run artifacts with `runs.geojson` and `run_sections.geojson`; replaced `connection_centerlines.geojson` with `connection_sections.geojson`.
- Removed standalone `lift_stations.geojson` from the runtime artifact contract; sanitized embedded lift station data remains in `lifts.geojson`.
- Pruned assignment-only `skiAreas`/`skiAreaIds`/`ski_area_ids`/`ski_area` properties from final app artifacts, including nested station and spot objects.
- Preserved run 3D coordinates and `elevationProfile` on run and section artifacts.
- Expanded app-supported run use filtering to include `snow_park` lines/sections and `snow_park`/`playground` polygons while documenting `sled` and `skitour` exclusion for now.
- Added focused Rust coverage for snow park/playground inclusion, elevation preservation, embedded station sanitization, no standalone lift station artifact, spots with `dismount=yes|sometimes|no`, and assignment-key pruning.
- Cached OpenSkiMap `spots.geojson` for dataset `2026-06-03`; the raw cached file is 125,039,856 bytes with SHA-256 `e483fbca8e16bcb7d45387e8ee90259f2cf4f8fab70aeb4a8195b7ac6d7721e2`.
- Rebuilt and validated schema 24 output from cache: 4,503 resorts, 97,571 supported runs, 23,690 lifts, 2,630 connections, and 28,232 assigned spots.
- Created `output/local-simulator-app` with exactly three local-app resorts: Silvretta Arena Ischgl/Samnaun, Saas Grund, and Tannheim-Zöblen-Schattwald.
- Selected Tannheim-Zöblen-Schattwald as the crossing validation resort; its local spots include 11 `dismount=yes` crossings and 5 `dismount=sometimes` crossings.
- Verified the three local simulator bundles have schema 24 manifests, no standalone `lift_stations.geojson`, no remaining assignment-only ski-area keys in app artifacts, and preserved `elevationProfile` data in runs and run sections.
- Launched SkiNav on the `iPhone 17` iOS 26.5 simulator with `SKINAV_USE_LOCAL_ARTIFACTS=1` and `SKINAV_LOCAL_ARTIFACT_ROOT=/Users/eli/projects/personal/SkiNavIndexes/output/local-simulator-app`.

## Known Follow-Up Work

- Peak Rust build RSS is still high because the first implementation keeps normalized run/lift geometry in memory while writing packages.
- The current client consumes remote `latest.json` and `resorts.json`; per-resort release artifact downloading is future work.
- Archive grouping should be revisited because some group archives are much larger than others.

## 2026-06-07

- Restored split run export artifacts: `downhill_lines.geojson`, `downhill_polygons.geojson`, and `downhill_centerlines.geojson`.
- Kept the current supported-use policy: `downhill`/`snow_park` lines and centerlines, plus `downhill`/`snow_park`/`playground` polygons.
- Removed duplicate `output/local-app` generation from the build output; release/export storage now lives in packages, groups, and release packs only.
- Removed `localArtifactRoot` from generated `latest.json` metadata.
