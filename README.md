# SkiNav Indexes

Rust CLI for building the SkiNav SQLite catalog and compressed source packs from cached OpenSkiMap GeoJSON snapshots.

The pipeline consumes OpenSkiMap GeoJSON layer files and emits one canonical SQLite release for SkiNav. The generated release root is the only client-facing output contract.

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
├── latest.json
├── catalog.sqlite.gz
└── pack-<number>.sqlite.gz
```

Key files:

- `latest.json` is the stable app entrypoint. It identifies the schema, dataset, and verified `catalog.sqlite.gz` metadata.
- `catalog.sqlite.gz` contains resort hierarchy, names, ISO metadata, source-pack references, and per-resort source statistics.
- `pack-*.sqlite.gz` contains normalized source feature tables and explicit ownership join tables. A resort can reference more than one pack, and a feature can be owned by more than one resort.

There is no generated discovery JSON, per-resort package tree, group archive, release-pack tarball, or nested `output/v2/` candidate. `latest.json` is metadata only; resort search and future catalog statistics come from SQLite.

The catalog tables are `metadata`, `resorts`, `resort_iso_codes`, `packs`, `resort_packs`, and `resort_source_stats`. Source packs contain `runs`, `lifts`, `spots`, and `connections` plus explicit ownership join tables. Geometry is stored as little-endian WKB, elevation profiles as little-endian `f64` sequences, and normalized feature properties remain available as row-level JSON for future app features. Redundant resort-assignment properties are omitted because ownership is stored in the join tables.

The pipeline intentionally does not generate app-owned render bundles or local simulator artifacts. SkiNav installs source packs from this release and creates its own render/graph artifacts locally.

## GitHub Workflow

`.github/workflows/release-indexes.yml` is the release automation surface for `obewi/SkiNavIndexes`.

Pull requests run Rust smoke checks only: build, tests, and CLI help. Manual dispatch runs the real release path:

1. Resolve the dataset version, defaulting to the current UTC date.
2. Fetch the OpenSkiMap source layers for that dataset version on the ephemeral runner.
3. Build and validate the generated output.
4. When `publish_release` is enabled, upload the canonical SQLite catalog and source-pack assets.
5. When `publish_release` is enabled, create or update the dataset's GitHub release.

The workflow does not use `actions/cache` or `actions/upload-artifact`; generated data is deleted in the final cleanup step. GitHub still retains workflow logs and run metadata according to the repository's Actions retention settings, and the published release assets are intentionally stored as GitHub Release assets for SkiNav to download.

The public release contract is the root-level `latest.json`, `catalog.sqlite.gz`, and `pack-*.sqlite.gz` assets.

The build writes that release contract directly under `output/`:

```text
output/
├── latest.json
├── catalog.sqlite.gz
└── pack-<number>.sqlite.gz
```

The catalog contains hierarchy, ISO metadata, pack references, and per-resort source statistics. Each compressed source pack contains normalized feature tables and ownership join tables. There is no V1 discovery index or archive-generation step.

For a dry run from a pushed branch:

```bash
gh workflow run release-indexes.yml \
  --ref <pushed-branch> \
  -f dataset_version=2026-06-03 \
  -f publish_release=false
```

For a release:

```bash
gh workflow run release-indexes.yml \
  --ref main \
  -f dataset_version=2026-06-03 \
  -f publish_release=true
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
