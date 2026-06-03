# Findings: Ski Resort Index Pipeline

## 2026-06-03 - OpenSkiMap GeoJSON Rebuild Decisions

### Authoritative Inputs
- `/Users/eli/Downloads/GeoJSON Integration Strategy.pdf`
- User direction on 2026-06-03: use Rust, update docs after implementation, and treat old project docs as outdated.

### Locked Decisions
- OpenSkiMap GeoJSON layer files are authoritative for this migration:
  - `ski_areas.geojson`
  - `runs.geojson`
  - `lifts.geojson`
- GeoPackage is explicitly not the target for this pass because the strategy document says it omits routing-critical fields such as `runConvention`.
- The legacy Overpass pipeline is superseded and should not be retained as a fallback for the new artifact publisher.
- Local iteration must use a cached raw-source directory so OpenSkiMap is not downloaded repeatedly.
- GitHub Actions publishing is not the first validation path. The first validation path is local generation plus SkiNav on-device/simulator consumption.
- Rust is the durable pipeline language for speed, memory control, strict schemas, reproducible CLI builds, and long-term artifact generation.

### Compatibility Constraint
- Current SkiNav only consumes SkiNavIndexes through `latest.json` and `resorts.json`.
- Rich artifacts are currently local app files under `Documents/graphs` and `Documents/render-bundles`.
- The rebuild therefore needs both:
  - backward-compatible discovery output, and
  - a local artifact installation/testing path for SkiNav before release publishing.

## Key Decisions

### Data Source
- Use **only** `landuse=winter_sports` with `name` tag
- Do NOT use `site=piste` (fragmented geometry)
- Alps bounding box: 5°E to 16°E, 44°N to 48°N

### Hierarchy Detection
- Parent/child via 95% area containment
- Parent = "domain", Child = "resort"

### Bbox Padding Rules
| Size | Area | Padding |
|------|------|---------|
| Small | < 10 km² | +500m |
| Medium | 10-100 km² | +1000m |
| Large | 100-500 km² | +1500m |
| Domain | > 500 km² | +2000-3000m |

### Name Collection
Collect from OSM tags:
- `name` (primary, required)
- `alt_name` (semicolon-separated)
- `name:*` (all language variants)
- `loc_name` and `loc_name:*`
- `short_name` and `short_name:*`

---

## Technical Notes

### Overpass API
- Endpoint: https://overpass-api.de/api/interpreter
- Timeout: 300 seconds
- Output: JSON with geometry (`out tags geom`)

### Geometry Handling
- **Ways (896):** Full `geometry` array with lat/lon coordinates
- **Relations (56):** Only `bounds` available (full `out geom` times out)
- For hierarchy detection: bounds are sufficient for 95% containment
- Normalize script should handle both cases

### Country Detection
- Replaced heuristic lat/lon rules with point-in-polygon lookup using bundled Natural Earth boundaries in `data/alps_countries.geojson`
- This avoids external API rate limits and gives deterministic CI behavior
- Coverage is now 952/952 resorts with country codes
- Countries currently covered in bbox: AT, CH, IT, FR, DE, SI, LI, HR

### Skiable Area Metric
- OSM has no single canonical "skiable area" field
- `landuse=winter_sports` represents resort footprint, not piste surface area
- Best future approach: add a second query for `piste:type=downhill` and compute optional `skiable_area_km2`
- Keep current `area_km2` field for stable compatibility and predictable semantics

### Dependencies
- shapely (polygon operations)
- geojson (geometry handling)
- requests (API calls)

### Edge Cases to Handle
- Multi-polygon relations → take convex hull or largest
- Invalid geometries → skip or fix
- Self-intersecting → buffer(0) fix
- Duplicate names → deduplicate
- Missing geometry → skip with warning

---

## Research Links
- Overpass API: https://overpass-api.de/
- OSM Landuse: https://wiki.openstreetmap.org/wiki/Key:landuse
- Shapely: https://shapely.readthedocs.io/
- Natural Earth countries: https://www.naturalearthdata.com/
