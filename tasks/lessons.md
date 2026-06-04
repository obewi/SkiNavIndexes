# Lessons

## 2026-06-04

- For OpenSkiMap GeoJSON, detect connection features with `properties.type = "connection"`. Reserve raw `piste:type = "connection"` for OSM/Overpass queries and preserve it only as source metadata after conversion.
- Assign connection features to resort packages before building group archives or release packs. Use explicit `skiAreas` and network proximity; bbox overlap is only a candidate filter because archive placement follows resort package membership.
