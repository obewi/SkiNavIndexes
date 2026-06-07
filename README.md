# SkiNav Indexes

Rust CLI for building SkiNav discovery indexes and offline resort artifacts from cached OpenSkiMap GeoJSON snapshots.

The current pipeline is intentionally source-specific: it consumes OpenSkiMap GeoJSON layer files, builds backward-compatible discovery output for SkiNav.

## Data Source

The authoritative upstream inputs are the OpenSkiMap GeoJSON layers at:

- `https://tiles.openskimap.org/geojson/lifts.geojson`
- `https://tiles.openskimap.org/geojson/ski_areas.geojson`
- `https://tiles.openskimap.org/geojson/runs.geojson`
- `https://tiles.openskimap.org/geojson/spots.geojson`

The CLI caches these files under `data/raw/openskimap/<dataset-version>/` by default. Treat the cache as the normal development path: fetch once for a dataset version, then rebuild and validate from the local files.

Connection features are cached as an enrichment layer at:

- `data/raw/openskimap/<dataset-version>/connections.geojson`

OpenSkiMap GeoJSON is checked first. When OpenSkiMap contains connection features, they are identified by `properties.type = "connection"` and copied into `connections.geojson`. When OpenSkiMap does not yet contain those features, `fetch` uses a narrow Overpass fallback for raw OSM `piste:type=connection` ways and relations. The default Overpass base URL is `https://overpass-api.de/api/`, and requests use the SkiNavIndexes user agent configured in the CLI. Overpass is only used by `fetch`; `build` and `all --skip-fetch` never query the network.

## Commands

Run commands through Cargo during development:

```bash
cargo run -- <command> [options]
```

Use release mode for real OpenSkiMap snapshots. The runs layer is roughly 1 GB, and dev-mode builds are only appropriate for CLI smoke checks or tiny fixtures:

```bash
cargo run --release -- <command> [options]
```

Installed binary name:

```bash
skinav-indexes <command> [options]
```

Available commands:

```bash
# Download OpenSkiMap GeoJSON layers only when missing from the cache.
cargo run --release -- fetch

# Build all generated outputs from cached source files.
cargo run --release -- build

# Validate generated output files.
cargo run --release -- validate

# Fetch missing sources, build outputs, then validate.
cargo run --release -- all

# Rebuild and validate from the local cache without any network fetch.
cargo run --release -- all --skip-fetch
```

Useful options:

```bash
# Pin a cache namespace for a specific source snapshot or local test batch.
cargo run --release -- fetch --dataset-version 2026-06-03
cargo run --release -- build --dataset-version 2026-06-03
cargo run --release -- all --dataset-version 2026-06-03 --skip-fetch

# Use non-default directories for isolated experiments.
cargo run --release -- build --cache-dir data/raw/openskimap --output-dir output

# Point at a compatible OpenSkiMap GeoJSON base URL.
cargo run --release -- fetch --source-base-url https://tiles.openskimap.org/geojson

# Point connection enrichment at a different Overpass API base URL.
cargo run --release -- fetch --overpass-base-url https://overpass-api.de/api/
```

## One-Download Cache Policy

The intended workflow is:

1. Run `cargo run --release -- fetch --dataset-version <version>` once for the dataset version. This downloads missing OpenSkiMap layers, including `spots.geojson`, and creates `connections.geojson` from OpenSkiMap `type=connection` features or the Overpass fallback.
2. Re-run `cargo run --release -- build --dataset-version <version>` as often as needed.
3. Re-run `cargo run --release -- validate` after builds.
4. Use `cargo run --release -- all --dataset-version <version> --skip-fetch` when you want the full local build and validation path without touching the network.

Do not delete `data/raw/openskimap/<dataset-version>/` just to force a rebuild. Delete or replace cached source files only when intentionally moving to a new upstream snapshot. If `spots.geojson` or `connections.geojson` is missing, run `fetch` for that dataset version before building; the build step will fail rather than silently changing the source contract or querying Overpass.

## Output Layout

Generated artifacts are written below `output/` by default:

```text
output/
├── resorts.json
├── latest.json
├── build-report.json
├── packages/
│   └── resorts/
│       └── <resort-id>/
│           ├── manifest.json
│           ├── artifact_manifest.json
│           ├── lifts.geojson
│           ├── downhill_lines.geojson
│           ├── downhill_polygons.geojson
│           ├── downhill_centerlines.geojson
│           ├── connections.geojson
│           ├── connection_sections.geojson
│           ├── spots.geojson
│           └── audit_report.json
├── groups/
│   └── <group>.tar.gz
├── release-packs/
│   ├── manifest.json
│   └── <balanced-pack>.tar.gz
```

Key files:

- `latest.json` at the repository root is the stable app entrypoint. It is tracked and should point at the current published `resorts.json` release asset.
- `output/resorts.json` is the backward-compatible discovery index consumed by the current SkiNav app.
- `output/latest.json` is the generated release-candidate metadata for the build output.
- `output/build-report.json` records dataset version, generated timestamp, source counts, assigned spot counts, and warnings.
- `output/packages/resorts/<id>/...` contains the per-resort files used for richer offline graph and render workflows. Leaf resort packages own render artifacts; domain packages are lightweight metadata packages that reference child resort artifacts instead of duplicating child runs and lifts.
- `output/groups/<group>.tar.gz` bundles resort packages by logical ISO-derived group for debugging and inspection.
- `output/release-packs/...` is the release distribution layout. Large logical groups are split into part archives and tiny groups are combined into balanced packs. `output/release-packs/manifest.json` maps every release asset back to group and resort IDs.

Per-resort run artifacts are sanitized app-renderable exports, not raw OpenSkiMap copies. `downhill_lines.geojson` and `downhill_centerlines.geojson` include `downhill` and `snow_park` line features; `downhill_polygons.geojson` includes `downhill`, `snow_park`, and `playground` polygon features. The observed `nordic`, `hike`, `fatbike`, `sleigh`, `ice_skate`, `sled`, and `skitour` uses are intentionally excluded from app artifacts until routing and rendering policy exists for them.

Run line and section artifacts preserve OpenSkiMap 3D coordinates and `elevationProfile` values (`heights`, `resolution`, and `targetResolution`) when present. Assignment-only source properties (`skiAreas`, `skiAreaIds`, `ski_area_ids`, and `ski_area`) are pruned from final app artifacts, including nested lift stations and spots; resort membership remains represented by package path and manifests.

Lift station records are embedded in `lifts.geojson` via sanitized `properties.stations`; standalone `lift_stations.geojson` is no longer part of the runtime artifact contract. `spots.geojson` contains all assigned OpenSkiMap spots. Crossing spots preserve `spotType = "crossing"` and `dismount = "yes" | "sometimes" | "no"` so SkiNav can render road-crossing policy without another source-contract change.

The pipeline intentionally does not generate `rendering_features.geojson` or `output/local-app`: those duplicate per-layer export artifacts that already live inside resort packages and release packs.

## GitHub Workflow

`.github/workflows/release-indexes.yml` is the release automation surface for `obewi/SkiNavIndexes`.

Pull requests run Rust smoke checks only: build, tests, and CLI help. Manual dispatch runs the real release path:

1. Resolve the dataset version, defaulting to the current UTC date.
2. Restore Cargo and `data/raw/openskimap/<dataset-version>/` caches when available.
3. Fetch missing OpenSkiMap source layers once for that dataset version.
4. Build and validate the generated output.
5. Upload generated index artifacts and balanced release-pack artifacts.
6. Optionally create or update a GitHub release when `publish_release` is enabled.

The public release contract is `latest.json`, `resorts.json`, `build-report.json`, `release-pack-manifest.json`, and the tarballs named in that manifest.

For a dry run from a pushed branch:

```bash
gh workflow run release-indexes.yml \
  --ref feature/geojson-rust-indexes \
  -f dataset_version=2026-06-03 \
  -f publish_release=false
```

For a release:

```bash
gh workflow run release-indexes.yml \
  --ref main \
  -f dataset_version=2026-06-03 \
  -f publish_release=true \
  -f release_tag=indexes-2026-06-03
```

## Development Checks

Lightweight checks that do not download GeoJSON:

```bash
cargo test
cargo run -- --help
cargo run --release -- all --skip-fetch
```

`cargo run --release -- all --skip-fetch` requires cached source files to already exist for the selected dataset version.

## License

OpenSkiMap combines OpenStreetMap and Skimap.org data. Preserve upstream attribution and license requirements when publishing derived artifacts.
