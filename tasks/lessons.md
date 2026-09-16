# Lessons

# 2026-09-16

- A per-resort source identity should be a fixed-size, versioned digest of
  normalized semantic content, independent of release timestamps and pack
  layout. Compute it once per canonical processing scope and repeat it for
  descendants so comparisons stay cheap and storage stays bounded.

## 2026-09-10

- A format migration is incomplete while the old producer or consumer remains reachable. When SQLite becomes the source of truth, remove the V1 generator, cache, decoder, and fallback in the same change; do not leave V2 under a nested candidate directory.

## 2026-06-04

- For OpenSkiMap GeoJSON, detect connection features with `properties.type = "connection"`. Reserve raw `piste:type = "connection"` for OSM/Overpass queries and preserve it only as source metadata after conversion.
- Assign connection features to resort packages before building group archives or release packs. Use explicit `skiAreas` and network proximity; bbox overlap is only a candidate filter because archive placement follows resort package membership.

## 2026-06-06

- Keep render-bundle `stats` strict, but only include fields that are part of the actual generated artifact contract. Do not make app-only diagnostic counters optional to hide a mismatch; remove them or pipe them through deliberately.
