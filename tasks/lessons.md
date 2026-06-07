# Lessons

## 2026-06-04

- For OpenSkiMap GeoJSON, detect connection features with `properties.type = "connection"`. Reserve raw `piste:type = "connection"` for OSM/Overpass queries and preserve it only as source metadata after conversion.
- Assign connection features to resort packages before building group archives or release packs. Use explicit `skiAreas` and network proximity; bbox overlap is only a candidate filter because archive placement follows resort package membership.

## 2026-06-06

- Keep render-bundle `stats` strict, but only include fields that are part of the actual generated artifact contract. Do not make app-only diagnostic counters optional to hide a mismatch; remove them or pipe them through deliberately.
