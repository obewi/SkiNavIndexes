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
- Group archives: 627
- Generated output: 1.6 GB
- Source cache: 1.0 GB

## Known Follow-Up Work

- Peak Rust build RSS is still high because the first implementation keeps normalized run/lift geometry in memory while writing packages.
- The current client consumes remote `latest.json` and `resorts.json`; per-resort release artifact downloading is future work.
- Archive grouping should be revisited because some group archives are much larger than others.
