# Findings

## Source Format

- OpenSkiMap GeoJSON is the source of truth for this pipeline.
- Overpass is intentionally not a fallback.
- The GeoJSON `skiAreas` association on run/lift features is required for correct resort assignment.

## Artifact Shape

- `rendering_features.geojson` had no SkiNav consumer and duplicated split render layers, so it was removed.
- Per-package raw `runs.geojson` was removed; the dated OpenSkiMap cache is the raw source snapshot.
- Parent domains such as Dolomiti Superski should not own copied child run/lift artifacts. They are reference-only packages.

## Release Shape

- `latest.json` is the stable tracked entrypoint for SkiNav clients.
- Generated `output/*` is ignored by Git and recreated locally or in GitHub Actions.
- Release builds should publish `resorts.json`, `latest.json`, build report, release-pack manifest, and the balanced release packs from the workflow output.
- `output/local-app` should not be published as a release asset. It is a local testing seed and duplicates the render-package data.

## SkiNav Client

- Current SkiNav clients remotely consume only `latest.json` and `resorts.json`.
- DEBUG local artifacts can seed `resorts.json`, `latest.json`, and render bundles before GitHub release publication.
- Binary graph generation remains separate from this first Rust index release.
