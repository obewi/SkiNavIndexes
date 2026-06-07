use super::*;

pub(super) fn ski_area_ids(props: &Map<String, Value>) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for key in ["skiAreas", "skiAreaIds", "ski_area_ids", "ski_area"] {
        if let Some(value) = props.get(key) {
            collect_ids(value, &mut ids);
        }
    }
    ids.into_iter().collect()
}

pub(super) fn collect_ids(value: &Value, ids: &mut BTreeSet<String>) {
    match value {
        Value::String(text) => {
            if !text.trim().is_empty() {
                ids.insert(text.trim().to_string());
            }
        }
        Value::Number(_) => {
            if let Some(text) = value_to_string(value) {
                ids.insert(text);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_ids(item, ids);
            }
        }
        Value::Object(object) => {
            if let Some(id) = first_string(object, &["id", "skiAreaId", "ski_area_id"]) {
                ids.insert(id);
            }
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                if let Some(id) = first_string(properties, &["id", "skiAreaId", "ski_area_id"]) {
                    ids.insert(id);
                }
            }
        }
        _ => {}
    }
}

pub(super) fn iso_codes_from_places(value: Option<&Value>) -> Vec<String> {
    let mut codes = BTreeSet::new();
    collect_place_codes(value, &mut codes, &["iso3166_2", "iso3166-2", "isoCode"]);
    codes.into_iter().collect()
}

pub(super) fn country_codes_from_places(value: Option<&Value>) -> Vec<String> {
    let mut codes = BTreeSet::new();
    collect_place_codes(
        value,
        &mut codes,
        &["iso3166_1Alpha2", "iso3166-1Alpha2", "countryCode"],
    );
    codes.into_iter().collect()
}

pub(super) fn collect_place_codes(
    value: Option<&Value>,
    codes: &mut BTreeSet<String>,
    keys: &[&str],
) {
    match value {
        Some(Value::Array(items)) => {
            for item in items {
                collect_place_codes(Some(item), codes, keys);
            }
        }
        Some(Value::Object(object)) => {
            for key in keys {
                if let Some(code) = object.get(*key).and_then(Value::as_str) {
                    if !code.trim().is_empty() {
                        codes.insert(code.trim().to_string());
                    }
                }
            }
        }
        _ => {}
    }
}

pub(super) fn bbox_from_geometry(geometry: &Value) -> Option<[f64; 4]> {
    let mut bbox: Option<[f64; 4]> = None;
    scan_coordinates(geometry.get("coordinates")?, &mut |lon, lat| {
        bbox = Some(match bbox {
            Some(existing) => [
                existing[0].min(lon),
                existing[1].min(lat),
                existing[2].max(lon),
                existing[3].max(lat),
            ],
            None => [lon, lat, lon, lat],
        });
    });
    bbox
}

pub(super) fn scan_coordinates(value: &Value, callback: &mut impl FnMut(f64, f64)) {
    if let Some(items) = value.as_array() {
        if items.len() >= 2 && items[0].is_number() && items[1].is_number() {
            if let (Some(lon), Some(lat)) = (items[0].as_f64(), items[1].as_f64()) {
                callback(lon, lat);
            }
        } else {
            for item in items {
                scan_coordinates(item, callback);
            }
        }
    }
}

pub(super) fn bbox_center(bbox: [f64; 4]) -> [f64; 2] {
    [(bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0]
}

pub(super) fn merge_bbox(left: [f64; 4], right: [f64; 4]) -> [f64; 4] {
    [
        left[0].min(right[0]),
        left[1].min(right[1]),
        left[2].max(right[2]),
        left[3].max(right[3]),
    ]
}

pub(super) fn padded_bbox(bbox: [f64; 4]) -> [f64; 4] {
    padded_bbox_meters(bbox, 500.0)
}

pub(super) fn padded_bbox_meters(bbox: [f64; 4], meters: f64) -> [f64; 4] {
    let center_lat = (bbox[1] + bbox[3]) / 2.0;
    let lat_delta = meters / 111_320.0;
    let lon_delta = meters / (111_320.0 * center_lat.to_radians().cos().abs().max(0.1));
    [
        bbox[0] - lon_delta,
        bbox[1] - lat_delta,
        bbox[2] + lon_delta,
        bbox[3] + lat_delta,
    ]
}

pub(super) fn bbox_intersects(left: [f64; 4], right: [f64; 4]) -> bool {
    left[0] <= right[2] && left[2] >= right[0] && left[1] <= right[3] && left[3] >= right[1]
}

pub(super) fn invalid_or_point_bbox(bbox: [f64; 4]) -> bool {
    bbox[0] == 0.0 && bbox[1] == 0.0 && bbox[2] == 0.0 && bbox[3] == 0.0
        || (bbox[0] - bbox[2]).abs() < f64::EPSILON
        || (bbox[1] - bbox[3]).abs() < f64::EPSILON
}

pub(super) fn area_km2_from_bbox(bbox: [f64; 4]) -> f64 {
    let center_lat = (bbox[1] + bbox[3]) / 2.0;
    let width = (bbox[2] - bbox[0]).abs() * 111.0 * center_lat.to_radians().cos().abs();
    let height = (bbox[3] - bbox[1]).abs() * 111.0;
    (width * height * 100.0).round() / 100.0
}

pub(super) fn line_geometries(geometry: &Value) -> Vec<Value> {
    match geometry_type(geometry) {
        Some("LineString") => vec![geometry.clone()],
        Some("MultiLineString") => geometry
            .get("coordinates")
            .and_then(Value::as_array)
            .map(|lines| {
                lines
                    .iter()
                    .map(|line| json!({"type": "LineString", "coordinates": line}))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

pub(super) fn geometry_points(geometry: &Value) -> Vec<[f64; 2]> {
    let mut points = Vec::new();
    scan_coordinates(
        geometry.get("coordinates").unwrap_or(&Value::Null),
        &mut |lon, lat| {
            points.push([lon, lat]);
        },
    );
    points
}

pub(super) fn geometry_endpoints(geometry: &Value) -> Vec<[f64; 2]> {
    let mut endpoints = Vec::new();
    for line in line_geometries(geometry) {
        let Some(coords) = line.get("coordinates").and_then(Value::as_array) else {
            continue;
        };
        for coord in [coords.first(), coords.last()].into_iter().flatten() {
            if let Some(point) = point_from_coord_value(coord) {
                endpoints.push(point);
            }
        }
    }
    endpoints
}

pub(super) fn point_from_coord_value(value: &Value) -> Option<[f64; 2]> {
    let items = value.as_array()?;
    Some([items.first()?.as_f64()?, items.get(1)?.as_f64()?])
}

pub(super) fn haversine_meters(left: [f64; 2], right: [f64; 2]) -> f64 {
    let radius = 6_371_000.0;
    let lat1 = left[1].to_radians();
    let lat2 = right[1].to_radians();
    let dlat = (right[1] - left[1]).to_radians();
    let dlon = (right[0] - left[0]).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * radius * h.sqrt().asin()
}

pub(super) fn min_distance_to_polyline_meters(point: [f64; 2], line: &[[f64; 2]]) -> f64 {
    if line.len() < 2 {
        return f64::INFINITY;
    }
    line.windows(2)
        .map(|segment| point_segment_distance_meters(point, segment[0], segment[1]))
        .fold(f64::INFINITY, f64::min)
}

pub(super) fn point_segment_distance_meters(
    point: [f64; 2],
    start: [f64; 2],
    end: [f64; 2],
) -> f64 {
    let lat = ((point[1] + start[1] + end[1]) / 3.0).to_radians();
    let meters_per_degree_lon = 111_320.0 * lat.cos().abs().max(0.1);
    let to_xy = |coord: [f64; 2]| [coord[0] * meters_per_degree_lon, coord[1] * 111_320.0];
    let p = to_xy(point);
    let a = to_xy(start);
    let b = to_xy(end);
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [p[0] - a[0], p[1] - a[1]];
    let ab_len2 = ab[0] * ab[0] + ab[1] * ab[1];
    if ab_len2 == 0.0 {
        return ((p[0] - a[0]).powi(2) + (p[1] - a[1]).powi(2)).sqrt();
    }
    let t = ((ap[0] * ab[0] + ap[1] * ab[1]) / ab_len2).clamp(0.0, 1.0);
    let projection = [a[0] + t * ab[0], a[1] + t * ab[1]];
    ((p[0] - projection[0]).powi(2) + (p[1] - projection[1]).powi(2)).sqrt()
}

pub(super) fn geometry_type(geometry: &Value) -> Option<&str> {
    geometry.get("type").and_then(Value::as_str)
}

pub(super) fn endpoint_key_from_coord_value(value: &Value) -> Option<String> {
    let items = value.as_array()?;
    let lon = items.first()?.as_f64()?;
    let lat = items.get(1)?.as_f64()?;
    Some(format!("{lat:.6},{lon:.6}"))
}

pub(super) fn first_string(props: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| props.get(*key))
        .filter_map(value_to_string)
        .map(|text| text.trim().to_string())
        .find(|text| !text.is_empty())
}

pub(super) fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

pub(super) fn explicit_non_operating_status(status: &str) -> bool {
    !status.is_empty()
        && !matches!(
            status.to_ascii_lowercase().as_str(),
            "operating" | "open" | "active"
        )
}

pub(super) fn opt_string_value(value: Option<String>) -> Value {
    value.map(Value::String).unwrap_or(Value::Null)
}

pub(super) fn safe_path_id(id: &str) -> String {
    id.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

pub(super) fn feature_count(collection: &Value) -> usize {
    collection
        .get("features")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

pub(super) fn count_section_direction(
    collection: &Value,
    direction_source: &str,
    effective_oneway: bool,
) -> usize {
    collection
        .get("features")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|feature| {
            let Some(properties) = feature.get("properties").and_then(Value::as_object) else {
                return false;
            };
            properties
                .get("direction_source")
                .and_then(Value::as_str)
                .is_some_and(|source| source == direction_source)
                && properties
                    .get("effective_oneway")
                    .and_then(Value::as_bool)
                    .is_some_and(|oneway| oneway == effective_oneway)
        })
        .count()
}

pub(super) fn count_unknown_direction_sections(collection: &Value) -> usize {
    collection
        .get("features")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|feature| {
            feature
                .get("properties")
                .and_then(Value::as_object)
                .and_then(|properties| properties.get("direction_source"))
                .and_then(Value::as_str)
                == Some("none")
        })
        .count()
}
