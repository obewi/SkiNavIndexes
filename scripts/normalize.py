#!/usr/bin/env python3
"""Normalize Overpass API output into ski resort index format.

Flat model: All ski areas are equal, linked via site_relation_ids and contained_area_ids.
Supports both landuse=winter_sports polygons and site=piste relations.
"""

import argparse
import json
import math
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple, Union

from shapely.geometry import Point, Polygon, MultiPolygon, box, shape, mapping
from shapely.ops import unary_union
from shapely.prepared import prep
from shapely.validation import make_valid


def collect_name_variants(tags: dict[str, Any]) -> list[str]:
    """Collect all name variants from OSM tags."""
    names = []

    if "name" in tags:
        names.append(tags["name"])

    if "alt_name" in tags:
        alt_names = [n.strip() for n in tags["alt_name"].split(";") if n.strip()]
        names.extend(alt_names)

    for key, value in tags.items():
        if key.startswith("name:") and value:
            names.append(value)

    if "loc_name" in tags:
        names.append(tags["loc_name"])
    for key, value in tags.items():
        if key.startswith("loc_name:") and value:
            names.append(value)

    if "short_name" in tags:
        names.append(tags["short_name"])
    for key, value in tags.items():
        if key.startswith("short_name:") and value:
            names.append(value)

    seen = set()
    deduplicated = []
    for name in names:
        name = name.strip()
        if name and name not in seen and len(name) <= 200:
            seen.add(name)
            deduplicated.append(name)

    return deduplicated


def geometry_to_polygon(geometry: list[dict]) -> Optional[Polygon]:
    """Convert Overpass geometry array to Shapely Polygon."""
    if not geometry or len(geometry) < 3:
        return None

    coords = [(p["lon"], p["lat"]) for p in geometry]
    if coords[0] != coords[-1]:
        coords.append(coords[0])

    try:
        poly = Polygon(coords)
        if not poly.is_valid:
            fixed = make_valid(poly)
            if isinstance(fixed, (Polygon, MultiPolygon)):
                if isinstance(fixed, MultiPolygon):
                    poly = max(fixed.geoms, key=lambda g: g.area)
                else:
                    poly = fixed
            else:
                return None
        if poly.is_empty:
            return None
        return poly
    except Exception:
        return None


def bounds_to_polygon(bounds: dict) -> Polygon:
    """Convert Overpass bounds to Shapely Polygon (box)."""
    return box(
        bounds["minlon"],
        bounds["minlat"],
        bounds["maxlon"],
        bounds["maxlat"],
    )


def calculate_area_km2(polygon: Polygon) -> float:
    """Calculate approximate area in km² using Haversine-based method."""
    bounds = polygon.bounds
    center_lat = (bounds[1] + bounds[3]) / 2

    lat_km = 111.0
    lon_km = 111.0 * math.cos(math.radians(center_lat))

    minx, miny, maxx, maxy = bounds
    width_km = (maxx - minx) * lon_km
    height_km = (maxy - miny) * lat_km

    bbox_area = width_km * height_km
    poly_ratio = (
        polygon.area / ((maxx - minx) * (maxy - miny))
        if (maxx - minx) * (maxy - miny) > 0
        else 1
    )

    return round(bbox_area * poly_ratio, 2)


def get_padding_meters(area_km2: float) -> float:
    """Get bbox padding based on size category."""
    if area_km2 < 10:
        return 500
    elif area_km2 < 100:
        return 1000
    elif area_km2 < 500:
        return 1500
    else:
        return 2500


def apply_padding(
    bounds: tuple[float, float, float, float], padding_m: float, center_lat: float
) -> list[float]:
    """Apply padding to bounding box in degrees."""
    lat_deg = padding_m / 111000
    lon_deg = padding_m / (111000 * math.cos(math.radians(center_lat)))

    west, south, east, north = bounds
    return [
        round(west - lon_deg, 6),
        round(south - lat_deg, 6),
        round(east + lon_deg, 6),
        round(north + lat_deg, 6),
    ]


def get_country_code(
    lat: float, lon: float, country_index: Optional[List[Dict[str, Any]]]
) -> Optional[str]:
    """Get ISO country code from coordinates using polygon containment."""
    if not country_index:
        return None

    point = Point(lon, lat)

    for entry in country_index:
        if entry["prepared"].contains(point) or entry["prepared"].covers(point):
            return entry["iso_a2"]

    nearest_code = None
    nearest_distance = float("inf")
    for entry in country_index:
        distance = entry["geometry"].distance(point)
        if distance < nearest_distance:
            nearest_distance = distance
            nearest_code = entry["iso_a2"]

    if nearest_distance <= 0.02:
        return nearest_code

    return None


def build_country_index() -> Optional[List[Dict[str, Any]]]:
    """Build country polygon index from bundled Natural Earth boundaries."""
    data_path = Path(__file__).parent.parent / "data" / "alps_countries.geojson"
    if not data_path.exists():
        print(f"Warning: Country boundary file not found: {data_path}", file=sys.stderr)
        return None

    try:
        with open(data_path) as f:
            geojson = json.load(f)

        index = []
        iso_a3_to_a2 = {
            "FRA": "FR",
            "DEU": "DE",
            "AUT": "AT",
            "ITA": "IT",
            "CHE": "CH",
            "SVN": "SI",
            "LIE": "LI",
            "HRV": "HR",
            "DE": "DE",
            "AT": "AT",
            "IT": "IT",
            "CH": "CH",
            "SI": "SI",
            "LI": "LI",
            "HR": "HR",
        }

        for feature in geojson.get("features", []):
            props = feature.get("properties", {})
            iso_a2 = props.get("ISO_A2")
            if not iso_a2 or iso_a2 == "-99":
                iso_a2 = iso_a3_to_a2.get(props.get("ADM0_A3"), None)
            geometry_data = feature.get("geometry")
            if not iso_a2 or not geometry_data:
                continue

            geometry = shape(geometry_data)
            if not geometry.is_valid:
                geometry = make_valid(geometry)
            if geometry.is_empty:
                continue

            index.append(
                {
                    "iso_a2": iso_a2,
                    "geometry": geometry,
                    "prepared": prep(geometry),
                }
            )

        if not index:
            print("Warning: No country polygons loaded", file=sys.stderr)
            return None

        return index
    except Exception as exc:
        print(f"Warning: Could not build country index: {exc}", file=sys.stderr)
        return None


def parse_overpass_output(
    data: dict, country_index: Optional[List[Dict[str, Any]]]
) -> Tuple[List[Dict], Dict[int, Polygon]]:
    """Parse Overpass JSON into resort objects.
    
    Returns:
        tuple: (list of resort dicts, dict mapping relation IDs to their hull polygons)
    """
    resorts = []
    relation_hulls: dict[int, Polygon] = {}
    
    # First pass: collect all elements by type
    ways_by_id: dict[int, dict] = {}
    relations_by_id: dict[int, dict] = {}
    member_geometries: dict[int, list[Polygon]] = {}  # relation_id -> list of member polygons
    
    for element in data.get("elements", []):
        if element["type"] == "way":
            ways_by_id[element["id"]] = element
        elif element["type"] == "relation":
            relations_by_id[element["id"]] = element
    
    # Process landuse=winter_sports ways
    for way_id, way in ways_by_id.items():
        tags = way.get("tags", {})
        if tags.get("landuse") != "winter_sports":
            continue
            
        name = tags.get("name")
        if not name:
            continue
        
        polygon = geometry_to_polygon(way.get("geometry", []))
        if polygon is None or polygon.is_empty:
            continue
        
        area_km2 = calculate_area_km2(polygon)
        bounds = polygon.bounds
        center_lat = polygon.centroid.y
        center_lon = polygon.centroid.x
        
        padding = get_padding_meters(area_km2)
        bbox_center_lat = (bounds[1] + bounds[3]) / 2
        bbox = apply_padding(bounds, padding, bbox_center_lat)
        
        names = collect_name_variants(tags)
        country = get_country_code(center_lat, center_lon, country_index)
        
        resorts.append({
            "id": way_id,
            "name": name,
            "names": names,
            "geometry": mapping(polygon),
            "bbox": bbox,
            "area_km2": area_km2,
            "country": country,
            "site_relation_ids": [],
            "_polygon": polygon,
            "_centroid": polygon.centroid,
        })
    
    # Process site=piste relations
    for rel_id, relation in relations_by_id.items():
        tags = relation.get("tags", {})
        if tags.get("site") != "piste":
            continue
            
        name = tags.get("name")
        if not name:
            continue
        
        # Collect member way geometries
        member_polys = []
        member_way_ids = []
        
        for member in relation.get("members", []):
            if member["type"] == "way":
                member_way_id = member["ref"]
                member_way_ids.append(member_way_id)
                
                # Look up member way geometry
                if member_way_id in ways_by_id:
                    member_way = ways_by_id[member_way_id]
                    if "geometry" in member_way:
                        member_poly = geometry_to_polygon(member_way["geometry"])
                        if member_poly and not member_poly.is_empty:
                            member_polys.append(member_poly)
        
        if not member_polys:
            # Fall back to bounds if available
            if "bounds" in relation:
                hull = bounds_to_polygon(relation["bounds"])
            else:
                continue
        else:
            # Create convex hull from all member geometries
            try:
                union = unary_union(member_polys)
                hull = union.convex_hull
            except Exception:
                continue
        
        if hull is None or hull.is_empty:
            continue
        
        relation_hulls[rel_id] = hull
        
        area_km2 = calculate_area_km2(hull)
        bounds = hull.bounds
        center_lat = hull.centroid.y
        center_lon = hull.centroid.x
        
        padding = get_padding_meters(area_km2)
        bbox_center_lat = (bounds[1] + bounds[3]) / 2
        bbox = apply_padding(bounds, padding, bbox_center_lat)
        
        names = collect_name_variants(tags)
        country = get_country_code(center_lat, center_lon, country_index)
        
        resorts.append({
            "id": -rel_id,  # Negative ID to distinguish relations
            "name": name,
            "names": names,
            "geometry": mapping(hull),
            "bbox": bbox,
            "area_km2": area_km2,
            "country": country,
            "contained_area_ids": [],  # Will be populated in linking step
            "_polygon": hull,
            "_is_relation": True,
            "_member_way_ids": member_way_ids,
        })
    
    # Also process landuse=winter_sports relations (existing behavior)
    # But skip if already processed as site=piste (some relations have both tags)
    for rel_id, relation in relations_by_id.items():
        tags = relation.get("tags", {})
        if tags.get("landuse") != "winter_sports":
            continue
        
        # Skip if already processed as site=piste
        if tags.get("site") == "piste":
            continue
            
        name = tags.get("name")
        if not name:
            continue
        
        # Use bounds for landuse relations
        if "bounds" not in relation:
            continue
            
        polygon = bounds_to_polygon(relation["bounds"])
        if polygon.is_empty:
            continue
        
        area_km2 = calculate_area_km2(polygon)
        bounds = polygon.bounds
        center_lat = polygon.centroid.y
        center_lon = polygon.centroid.x
        
        padding = get_padding_meters(area_km2)
        bbox_center_lat = (bounds[1] + bounds[3]) / 2
        bbox = apply_padding(bounds, padding, bbox_center_lat)
        
        names = collect_name_variants(tags)
        country = get_country_code(center_lat, center_lon, country_index)
        
        resorts.append({
            "id": -rel_id,
            "name": name,
            "names": names,
            "geometry": None,  # No exact geometry for landuse relations
            "bbox": bbox,
            "area_km2": area_km2,
            "country": country,
            "site_relation_ids": [],
            "_polygon": polygon,
            "_centroid": polygon.centroid,
        })
    
    return resorts, relation_hulls


def link_resorts(resorts: list[dict], relation_hulls: dict[int, Polygon]) -> list[dict]:
    """Link landuse ways to site=piste relations via spatial overlap."""
    
    # Build prepared polygons for relations
    prepared_hulls = {rel_id: prep(hull) for rel_id, hull in relation_hulls.items()}
    
    for resort in resorts:
        # Skip relations
        if resort.get("_is_relation"):
            continue
        
        centroid = resort.get("_centroid")
        if centroid is None:
            continue
        
        # Check which relations contain this resort's centroid
        for rel_id, prepared in prepared_hulls.items():
            if prepared.contains(centroid) or prepared.covers(centroid):
                resort["site_relation_ids"].append(-rel_id)
    
    # Populate contained_area_ids for relations
    resort_by_id = {r["id"]: r for r in resorts}
    for resort in resorts:
        if not resort.get("_is_relation"):
            continue
        
        rel_id = -resort["id"]  # Convert back to positive
        
        # Find all landuse ways that have this relation in their site_relation_ids
        for other in resorts:
            if other.get("_is_relation"):
                continue
            if -rel_id in other.get("site_relation_ids", []):
                resort["contained_area_ids"].append(other["id"])
    
    return resorts


def normalize(input_path: str, output_path: str) -> dict:
    """Main normalization function."""
    with open(input_path) as f:
        data = json.load(f)

    print(
        f"Loaded {len(data.get('elements', []))} elements from Overpass",
        file=sys.stderr,
    )

    print("Building country index...", file=sys.stderr)
    country_index = build_country_index()

    print("Parsing elements...", file=sys.stderr)
    resorts, relation_hulls = parse_overpass_output(data, country_index)
    print(f"Parsed {len(resorts)} valid ski areas", file=sys.stderr)
    print(f"  - {sum(1 for r in resorts if not r.get('_is_relation'))} landuse ways", file=sys.stderr)
    print(f"  - {sum(1 for r in resorts if r.get('_is_relation'))} site relations", file=sys.stderr)

    print("Linking ski areas...", file=sys.stderr)
    resorts = link_resorts(resorts, relation_hulls)
    
    linked_count = sum(1 for r in resorts if r.get("site_relation_ids") or r.get("contained_area_ids"))
    print(f"Linked {linked_count} ski areas", file=sys.stderr)

    # Deduplicate: Remove landuse ways that are contained in site=piste relations
    # Keep only site=piste relations and standalone landuse ways
    original_count = len(resorts)
    resorts = [r for r in resorts if not r.get("site_relation_ids")]
    removed_count = original_count - len(resorts)
    if removed_count > 0:
        print(f"Removed {removed_count} landuse ways contained in site=piste relations", file=sys.stderr)

    # Clean up internal fields
    for resort in resorts:
        resort.pop("_polygon", None)
        resort.pop("_centroid", None)
        resort.pop("_is_relation", None)
        resort.pop("_member_way_ids", None)
        
        # Clean up empty lists
        if resort.get("site_relation_ids") == [] or resort.get("site_relation_ids") is None:
            resort.pop("site_relation_ids", None)
        if resort.get("contained_area_ids") == [] or resort.get("contained_area_ids") is None:
            resort.pop("contained_area_ids", None)
        if resort.get("geometry") is None:
            resort.pop("geometry", None)

    output = {
        "version": datetime.now(timezone.utc).strftime("%Y-%m-%d"),
        "generated_at": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "total_resorts": len(resorts),
        "regions": ["alps"],
        "resorts": resorts,
    }

    Path(output_path).parent.mkdir(parents=True, exist_ok=True)
    with open(output_path, "w") as f:
        json.dump(output, f, indent=2, ensure_ascii=False)

    print(f"Wrote {len(resorts)} ski areas to {output_path}", file=sys.stderr)
    return output


def main():
    parser = argparse.ArgumentParser(
        description="Normalize Overpass output to resort index"
    )
    parser.add_argument("input", help="Input JSON file from Overpass API")
    parser.add_argument("output", help="Output resorts.json file")
    args = parser.parse_args()

    normalize(args.input, args.output)


if __name__ == "__main__":
    main()
