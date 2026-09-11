# Findings

## Source Format

- OpenSkiMap GeoJSON is the source of truth for this pipeline.
- Overpass is only a narrow `fetch`-time fallback for missing OpenSkiMap GeoJSON connection features. OpenSkiMap uses `properties.type = "connection"`; raw OSM/Overpass uses `piste:type=connection`.
- The GeoJSON `skiAreas` association on run/lift features is required for correct resort assignment.
- Connection features are assigned directly to normalized resort ownership rows; no package, group archive, or release-pack propagation step exists anymore.

## Artifact Shape

- The generated release contains only the SQLite catalog and compressed source packs; app-owned render bundles are created by SkiNav after source-pack installation.
- The catalog stores hierarchy, metadata, pack references, and per-resort source statistics. Source packs store normalized runs, lifts, spots, connections, geometry, elevation profiles, and explicit ownership joins.
- Parent-owned or explicitly multi-owned features remain attached to every normalized owner. Parent domains with no direct source ownership remain valid catalog rows without copied child data.

## Release Shape

- `latest.json` is the stable tracked entrypoint for SkiNav clients.
- Generated `output/*` is ignored by Git and recreated locally or in GitHub Actions.
- Release builds publish only the root-level `latest.json`, `catalog.sqlite.gz`, and `pack-*.sqlite.gz` assets.
- The pipeline does not generate local app artifacts; SkiNav downloads source packs and creates its app-owned artifacts locally.

## SQLite V2 Migration

- Parent/domain resort IDs are valid direct owners for every normalized feature kind; leaf-only assignment is no longer a storage invariant.
- The root-level SQLite catalog owns hierarchy, metadata, pack references, and source statistics; source packs own normalized feature rows and explicit ownership join rows.
- Resort-assignment properties are not part of the source-pack payload contract: `skiAreas`-style nested features duplicate the catalog/join ownership data and are omitted from `properties_json`.
- V2 pack validation compares feature/ownership sets rather than aggregate counts because a feature may be duplicated physically across packs.
- Cross-pack feature duplication remains an intentional download-efficiency tradeoff for shared ownership; after assignment-property compaction it is a small minority of the payload.

## SkiNav Client

- Current SkiNav clients resolve the picker and processing scopes from `latest.json` plus `catalog.sqlite.gz`; source packs are the only feature-data input.
- DEBUG local artifacts use the same root-level SQLite release layout before GitHub publication.
- Binary graph generation remains separate from this first Rust index release.
