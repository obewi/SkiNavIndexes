# Ski Resort Index

Automated pipeline that extracts ski resort data from OpenStreetMap, normalizes it into a compact JSON format, and publishes it via GitHub Releases with monthly updates.

## Quick Start

### Download Latest Index

```bash
# Get latest version info
curl -s https://raw.githubusercontent.com/OrbitalExplorer/SkiNavIndexes/main/latest.json

# Download resorts.json
curl -LO $(curl -s https://raw.githubusercontent.com/OrbitalExplorer/SkiNavIndexes/main/latest.json | jq -r .url)
```

### Manual Generation

```bash
# Install dependencies
pip install -r requirements.txt

# Run Overpass query
curl -X POST https://overpass-api.de/api/interpreter \
  -H 'Content-Type: application/x-www-form-urlencoded' \
  --data-urlencode "data=$(cat queries/winter_sports.overpassql)" \
  -o raw_data.json

# Normalize and validate
python scripts/normalize.py raw_data.json output/resorts.json
python scripts/validate.py output/resorts.json
```

## Output Format

```json
{
  "version": "2026-02-01",
  "generated_at": "2026-02-01T00:00:00Z",
  "total_resorts": 952,
  "regions": ["alps"],
  "resorts": [
    {
      "id": 123456,
      "name": "Val Gardena",
      "names": ["Val Gardena", "Gröden", "ヴァル・ガルデーナ"],
      "type": "resort",
      "parent_id": 789012,
      "parent_name": "Dolomiti Superski",
      "bbox": [10.28, 46.51, 11.82, 46.78],
      "area_km2": 175.3,
      "country": "IT"
    }
  ]
}
```

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `id` | int | OSM way/relation ID |
| `name` | string | Primary display name |
| `names` | string[] | All searchable name variants (multilingual) |
| `type` | string | `"domain"` (parent) or `"resort"` (leaf) |
| `parent_id` | int\|null | OSM ID of parent domain |
| `parent_name` | string\|null | Name of parent domain |
| `bbox` | number[4] | [west, south, east, north] with padding |
| `area_km2` | number | Approximate area in km² |
| `country` | string\|null | ISO 3166-1 alpha-2 code |

## Project Structure

```
SkiNavIndexes/
├── .github/workflows/
│   └── update-resorts.yml    # Monthly cron workflow
├── queries/
│   └── winter_sports.overpassql
├── scripts/
│   ├── normalize.py          # Overpass → resorts.json
│   └── validate.py           # Schema validation
├── schemas/
│   └── resort.json           # JSON Schema
├── output/
│   └── resorts.json          # Generated output
├── requirements.txt
└── latest.json               # Version pointer
```

## Data Source

Uses OpenStreetMap `landuse=winter_sports` features with a `name` tag.

**Current coverage:** Alps region (Austria, Switzerland, Italy, France, Germany, Slovenia)

## Automation

GitHub Actions runs monthly on the 1st at 00:00 UTC:
1. Fetches data from Overpass API
2. Normalizes and validates
3. Creates GitHub Release if changes detected
4. Updates `latest.json`

Trigger manually via `workflow_dispatch`.

## License

Data sourced from OpenStreetMap, licensed under ODbL.
