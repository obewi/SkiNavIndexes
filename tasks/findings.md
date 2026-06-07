# Findings

## Source Format

- OpenSkiMap GeoJSON is the source of truth for this pipeline.
- Overpass is only a narrow `fetch`-time fallback for missing OpenSkiMap GeoJSON connection features. OpenSkiMap uses `properties.type = "connection"`; raw OSM/Overpass uses `piste:type=connection`.
- The GeoJSON `skiAreas` association on run/lift features is required for correct resort assignment.
- Connection features must be assigned to leaf resort packages before archives are built; group archives and release packs should inherit them by copying resort packages.

## Artifact Shape

- `rendering_features.geojson` had no SkiNav consumer and duplicated split render layers, so it was removed.
- Per-package raw `runs.geojson` was removed; the dated OpenSkiMap cache is the raw source snapshot.
- Parent domains such as Dolomiti Superski should not own copied child run/lift artifacts. They are reference-only packages.
- Parent domains such as Dolomiti Superski should not own copied child connection artifacts either. Connections belong in the lowest matching leaf resort package, with multi-resort bridge connections duplicated into each matching leaf package.

## Release Shape

- `latest.json` is the stable tracked entrypoint for SkiNav clients.
- Generated `output/*` is ignored by Git and recreated locally or in GitHub Actions.
- Release builds should publish `resorts.json`, `latest.json`, build report, release-pack manifest, and the balanced release packs from the workflow output.
- `output/local-app` should not be generated or published as a release asset. It duplicates the render-package data already present in resort packages and release packs.

## SkiNav Client

- Current SkiNav clients remotely consume only `latest.json` and `resorts.json`.
- DEBUG local artifacts can seed `resorts.json`, `latest.json`, and render bundles before GitHub release publication.
- Binary graph generation remains separate from this first Rust index release.
