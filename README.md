# SkiNav Indexes

Rust CLI for building SkiNav discovery indexes and offline resort artifacts from cached OpenSkiMap GeoJSON snapshots.

The current pipeline is intentionally source-specific: it consumes OpenSkiMap GeoJSON layer files, builds backward-compatible discovery output for SkiNav, and emits richer per-resort packages for local device and simulator testing before any release publishing path is trusted.

## Data Source

The authoritative upstream inputs are the OpenSkiMap GeoJSON layers at:

- `https://tiles.openskimap.org/geojson/lifts.geojson`
- `https://tiles.openskimap.org/geojson/ski_areas.geojson`
- `https://tiles.openskimap.org/geojson/runs.geojson`

The CLI caches these files under `data/raw/openskimap/<dataset-version>/` by default. Treat the cache as the normal development path: fetch once for a dataset version, then rebuild and validate from the local files.

The owner constraint is to avoid repeated upstream downloads. In day-to-day development, use `--skip-fetch` once the day's source files are cached.

The strategy PDF refers to `tiles.skimap.org`; this implementation and these docs use the current CLI default requested for this repository: `https://tiles.openskimap.org/geojson`.

## Non-Goals

- No Overpass fallback. If OpenSkiMap is unavailable or missing required fields, the pipeline should fail visibly instead of silently reverting to Overpass.
- No GeoPackage geometry source for this pass. The pipeline uses the separate GeoJSON layers because they preserve routing and rendering fields needed by SkiNav.
- No CSV geometry source. CSV snapshots can be useful for audits, but they are not routing or rendering inputs.
- No on-device Overpass dependency. Local app testing should consume generated files from this repository.
- No binary `SKIGRAPH` generation in this first pass. `output/local-app/graphs/` contains graph metadata with `status: "not-built"` until the shared SkiNav graph builder contract owns binary graph creation.
- No one-asset-per-resort release layout. Resort packages are grouped into ISO-derived archives to stay compatible with GitHub release asset limits.

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
```

## One-Download Cache Policy

The intended workflow is:

1. Run `cargo run --release -- fetch --dataset-version <version>` once for the dataset version.
2. Re-run `cargo run --release -- build --dataset-version <version>` as often as needed.
3. Re-run `cargo run --release -- validate` after builds.
4. Use `cargo run --release -- all --dataset-version <version> --skip-fetch` when you want the full local build and validation path without touching the network.

Do not delete `data/raw/openskimap/<dataset-version>/` just to force a rebuild. Delete or replace cached source files only when intentionally moving to a new upstream snapshot.

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
│           ├── lift_stations.geojson
│           ├── downhill_lines.geojson
│           ├── downhill_polygons.geojson
│           ├── downhill_centerlines.geojson
│           ├── run_matching_hints.json
│           ├── explore_detail.json
│           ├── audit_report.json
│           └── checksums.json
├── groups/
│   └── <group>.tar.gz
└── local-app/
    ├── resorts.json
    ├── latest.json
    ├── manifest.json
    ├── graphs/
    └── render-bundles/
```

Key files:

- `latest.json` at the repository root is the stable app entrypoint. It is tracked and should point at the current published `resorts.json` release asset.
- `output/resorts.json` is the backward-compatible discovery index consumed by the current SkiNav app.
- `output/latest.json` is the generated release-candidate metadata for the build output.
- `output/build-report.json` records dataset version, generated timestamp, source counts, and warnings.
- `output/packages/resorts/<id>/...` contains the per-resort files used for richer offline graph and render workflows. Leaf resort packages own render artifacts; domain packages are lightweight metadata packages that reference child resort artifacts instead of duplicating child runs and lifts.
- `output/groups/<group>.tar.gz` bundles resort packages by group into release-sized archives.
- `output/local-app/...` is the local seeding surface for device and simulator validation. It includes `resorts.json`, `latest.json`, `manifest.json`, `render-bundles/<resort-id>/...`, and `graphs/*.graph.meta.json`.

The pipeline intentionally does not generate `rendering_features.geojson`: that file duplicated `downhill_lines.geojson`, `downhill_polygons.geojson`, and `lifts.geojson`, and SkiNav does not consume it. Per-resort raw `runs.geojson` is also omitted because the cached OpenSkiMap source snapshot is the canonical raw input and SkiNav local artifacts do not read package raw-run files.

The 2026-06-03 full cached build produced:

- `4,494` resorts, including `79` domain records.
- `96,152` downhill runs and `23,690` lifts from the OpenSkiMap source layers.
- `627` group archives.
- `1.6 GB` generated `output/`, down from `5.6 GB` before removing duplicate files and domain payload duplication.
- `output/local-app/` is about `1.3 GB` when measured by itself. In the combined `output/` tree it adds little extra disk usage on APFS because local-app render files are hard links to package files when supported.

Generated raw and output artifacts are intentionally ignored by Git. Commit source, schemas, code, plans, workflow files, and the root `latest.json` entrypoint; regenerate `output/*` locally or in GitHub Actions.

## Local App Testing

Local/on-device validation comes before GitHub Actions or release publishing.

1. Fetch once for the dataset version:

   ```bash
   cargo run --release -- fetch --dataset-version 2026-06-03
   ```

2. Rebuild and validate from cache:

   ```bash
   cargo run --release -- all --dataset-version 2026-06-03 --skip-fetch
   ```

3. Seed SkiNav from `output/local-app/`.

   The generated local app directory mirrors the SkiNav document layout used for offline testing:

   ```text
   output/local-app/resorts.json
   output/local-app/latest.json
   output/local-app/manifest.json
   output/local-app/graphs/
   output/local-app/render-bundles/
   ```

4. Run SkiNav against those local files on the simulator or device and inspect routing/rendering behavior before considering any release publication.

The first-pass local app layout supplies render artifacts and graph metadata only. `output/local-app/manifest.json` has `containsBinaryGraphs: false`; binary graph generation is deferred to the shared SkiNav graph builder contract.

## GitHub Workflow

`.github/workflows/release-indexes.yml` is the release automation surface for `obewi/SkiNavIndexes`.

Pull requests run Rust smoke checks only: build, tests, and CLI help. Manual dispatch runs the real release path:

1. Resolve the dataset version, defaulting to the current UTC date.
2. Restore Cargo and `data/raw/openskimap/<dataset-version>/` caches when available.
3. Fetch missing OpenSkiMap source layers once for that dataset version.
4. Build and validate the generated output.
5. Upload generated index and archive artifacts.
6. Optionally create or update a GitHub release when `publish_release` is enabled.

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
