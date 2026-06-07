use super::*;

pub(super) fn normalize_sources(
    ski_areas: Vec<SourceFeature>,
    runs: Vec<SourceFeature>,
    lifts: Vec<SourceFeature>,
    spots: Vec<SourceFeature>,
    connections: Vec<SourceFeature>,
    dataset_version: &str,
    generated_at: DateTime<Utc>,
) -> Result<NormalizedDataset> {
    let mut warnings = Vec::new();
    let mut resorts = Vec::new();

    for (index, feature) in ski_areas.into_iter().enumerate() {
        let id = feature.source_id("ski-area", index);
        let Some(name) = first_string(&feature.properties, &["name", "title"]) else {
            warnings.push(format!("ski area {id} skipped: missing name"));
            continue;
        };
        let status = first_string(&feature.properties, &["status"]).unwrap_or_default();
        if !status.is_empty() && status != "operating" {
            continue;
        }
        if !has_any_value(&feature.properties, &["activities"], "downhill") {
            warnings.push(format!(
                "ski area {id} skipped: activities does not include downhill"
            ));
            continue;
        }

        let geometry_bbox = bbox_from_geometry(&feature.geometry);
        let bbox = geometry_bbox.unwrap_or([0.0, 0.0, 0.0, 0.0]);
        let center = bbox_center(bbox);
        let iso_codes = iso_codes_from_places(feature.properties.get("places"));
        let country_codes = country_codes_from_places(feature.properties.get("places"));
        let country = country_codes.first().cloned();
        let group_id = iso_codes
            .first()
            .cloned()
            .or_else(|| country.clone())
            .unwrap_or_else(|| "ZZ".to_string());

        resorts.push(ResortRecord {
            id: id.clone(),
            name,
            resort_type: "resort".to_string(),
            parent_id: None,
            parent_name: None,
            bbox,
            area_km2: area_km2_from_bbox(bbox),
            country,
            iso_codes,
            country_codes,
            group_id,
            center,
            child_ids: Vec::new(),
            run_convention: first_string(&feature.properties, &["runConvention", "run_convention"]),
            places: feature
                .properties
                .get("places")
                .cloned()
                .unwrap_or(Value::Null),
            statistics: feature
                .properties
                .get("statistics")
                .cloned()
                .unwrap_or(Value::Null),
        });
    }

    let resort_ids: BTreeSet<String> = resorts.iter().map(|resort| resort.id.clone()).collect();
    let mut normalized_runs = Vec::new();
    let mut normalized_lifts = Vec::new();
    let mut spot_candidates = Vec::new();
    let mut connection_candidates = Vec::new();

    for (index, feature) in runs.into_iter().enumerate() {
        let id = feature.source_id("run", index);
        if !is_supported_run_feature(&feature) {
            continue;
        }
        let status = first_string(&feature.properties, &["status"]).unwrap_or_default();
        if !status.is_empty() && status != "operating" {
            continue;
        }
        let associated = ski_area_ids(&feature.properties)
            .into_iter()
            .filter(|id| resort_ids.contains(id))
            .collect::<Vec<_>>();
        if associated.is_empty() {
            warnings.push(format!(
                "run {id} skipped: no matching skiAreas association"
            ));
            continue;
        }
        normalized_runs.push(FeatureRecord {
            id,
            resort_ids: associated,
            geometry: feature.geometry,
            properties: feature.properties,
        });
    }

    for (index, feature) in lifts.into_iter().enumerate() {
        let id = feature.source_id("lift", index);
        let status = first_string(&feature.properties, &["status"]).unwrap_or_default();
        if !status.is_empty() && status != "operating" {
            continue;
        }
        let associated = ski_area_ids(&feature.properties)
            .into_iter()
            .filter(|id| resort_ids.contains(id))
            .collect::<Vec<_>>();
        if associated.is_empty() {
            warnings.push(format!(
                "lift {id} skipped: no matching skiAreas association"
            ));
            continue;
        }
        normalized_lifts.push(FeatureRecord {
            id,
            resort_ids: associated,
            geometry: feature.geometry,
            properties: feature.properties,
        });
    }

    for (index, feature) in spots.into_iter().enumerate() {
        let id = feature.source_id("spot", index);
        let status = first_string(&feature.properties, &["status"]).unwrap_or_default();
        if explicit_non_operating_status(&status) {
            continue;
        }
        spot_candidates.push(FeatureRecord {
            id,
            resort_ids: Vec::new(),
            geometry: feature.geometry,
            properties: feature.properties,
        });
    }

    for (index, feature) in connections.into_iter().enumerate() {
        let id = feature.source_id("connection", index);
        if !is_openskimap_connection(&feature.properties)
            && !has_any_value(&feature.properties, &["piste:type"], "connection")
        {
            continue;
        }
        if !geometry_type(&feature.geometry).is_some_and(|ty| ty.contains("LineString")) {
            warnings.push(format!("connection {id} skipped: geometry is not linear"));
            continue;
        }
        let status = first_string(&feature.properties, &["status"]).unwrap_or_default();
        if explicit_non_operating_status(&status) {
            continue;
        }
        connection_candidates.push(FeatureRecord {
            id,
            resort_ids: Vec::new(),
            geometry: feature.geometry,
            properties: feature.properties,
        });
    }

    let active_resort_ids = normalized_runs
        .iter()
        .chain(normalized_lifts.iter())
        .flat_map(|record| record.resort_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    resorts.retain(|resort| active_resort_ids.contains(&resort.id));

    apply_feature_bounds_to_resorts(&mut resorts, &normalized_runs, &normalized_lifts);
    compute_resort_hierarchy(&mut resorts, &normalized_runs, &normalized_lifts);
    let normalized_connections = assign_connections_to_leaf_resorts(
        connection_candidates,
        &resorts,
        &normalized_runs,
        &normalized_lifts,
        &mut warnings,
    );
    let normalized_spots = assign_spots_to_leaf_resorts(spot_candidates, &resorts, &mut warnings);

    if resorts.is_empty() {
        bail!("no operating downhill resorts found; check OpenSkiMap schema and source files");
    }

    Ok(NormalizedDataset {
        dataset_version: dataset_version.to_string(),
        generated_at,
        resorts,
        runs: normalized_runs,
        lifts: normalized_lifts,
        spots: normalized_spots,
        connections: normalized_connections,
        warnings,
    })
}

pub(super) fn apply_feature_bounds_to_resorts(
    resorts: &mut [ResortRecord],
    runs: &[FeatureRecord],
    lifts: &[FeatureRecord],
) {
    let mut bounds_by_resort: HashMap<String, [f64; 4]> = HashMap::new();
    for record in runs.iter().chain(lifts.iter()) {
        if let Some(bbox) = bbox_from_geometry(&record.geometry) {
            for resort_id in &record.resort_ids {
                bounds_by_resort
                    .entry(resort_id.clone())
                    .and_modify(|existing| *existing = merge_bbox(*existing, bbox))
                    .or_insert(bbox);
            }
        }
    }
    for resort in resorts {
        if invalid_or_point_bbox(resort.bbox) {
            if let Some(bbox) = bounds_by_resort.get(&resort.id) {
                resort.bbox = padded_bbox(*bbox);
                resort.center = bbox_center(resort.bbox);
                resort.area_km2 = area_km2_from_bbox(resort.bbox);
            }
        } else if let Some(feature_bbox) = bounds_by_resort.get(&resort.id) {
            resort.bbox = padded_bbox(merge_bbox(resort.bbox, *feature_bbox));
            resort.center = bbox_center(resort.bbox);
            resort.area_km2 = area_km2_from_bbox(resort.bbox);
        }
    }
}

pub(super) fn compute_resort_hierarchy(
    resorts: &mut [ResortRecord],
    runs: &[FeatureRecord],
    lifts: &[FeatureRecord],
) {
    let mut graph: HashMap<String, BTreeSet<String>> = HashMap::new();
    for record in runs.iter().chain(lifts.iter()) {
        for left in &record.resort_ids {
            graph.entry(left.clone()).or_default();
            for right in &record.resort_ids {
                if left != right {
                    graph.entry(left.clone()).or_default().insert(right.clone());
                }
            }
        }
    }

    let resort_index = resorts
        .iter()
        .enumerate()
        .map(|(index, resort)| (resort.id.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut visited = BTreeSet::new();
    for resort in resorts.to_vec() {
        if visited.contains(&resort.id) {
            continue;
        }
        let mut stack = vec![resort.id.clone()];
        let mut component = Vec::new();
        visited.insert(resort.id.clone());
        while let Some(current) = stack.pop() {
            component.push(current.clone());
            for neighbor in graph.get(&current).into_iter().flatten() {
                if visited.insert(neighbor.clone()) {
                    stack.push(neighbor.clone());
                }
            }
        }
        if component.len() < 2 {
            continue;
        }
        component.sort_by(|left, right| {
            let left_area = resort_index
                .get(left)
                .map(|i| resorts[*i].area_km2)
                .unwrap_or(0.0);
            let right_area = resort_index
                .get(right)
                .map(|i| resorts[*i].area_km2)
                .unwrap_or(0.0);
            right_area
                .partial_cmp(&left_area)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let parent_id = component[0].clone();
        let parent_name = resort_index
            .get(&parent_id)
            .map(|i| resorts[*i].name.clone())
            .unwrap_or_else(|| parent_id.clone());
        let child_ids = component.iter().skip(1).cloned().collect::<Vec<_>>();
        if let Some(parent_index) = resort_index.get(&parent_id).copied() {
            resorts[parent_index].resort_type = "domain".to_string();
            resorts[parent_index].child_ids = child_ids.clone();
        }
        for child_id in child_ids {
            if let Some(child_index) = resort_index.get(&child_id).copied() {
                resorts[child_index].parent_id = Some(parent_id.clone());
                resorts[child_index].parent_name = Some(parent_name.clone());
            }
        }
    }
}

pub(super) fn assign_connections_to_leaf_resorts(
    connections: Vec<FeatureRecord>,
    resorts: &[ResortRecord],
    runs: &[FeatureRecord],
    lifts: &[FeatureRecord],
    warnings: &mut Vec<String>,
) -> Vec<FeatureRecord> {
    let leaf_resort_ids = resorts
        .iter()
        .filter(|resort| resort.resort_type != "domain")
        .map(|resort| resort.id.clone())
        .collect::<BTreeSet<_>>();
    let source_index = source_resort_index(runs, lifts, &leaf_resort_ids);
    let network_index = build_network_match_index(runs, lifts, &leaf_resort_ids);
    let mut assigned = Vec::new();

    for mut connection in connections {
        let mut resort_ids = ski_area_ids(&connection.properties)
            .into_iter()
            .filter(|id| leaf_resort_ids.contains(id))
            .collect::<BTreeSet<_>>();

        if resort_ids.is_empty() {
            for source_key in source_keys_from_properties(&connection.properties) {
                if let Some(ids) = source_index.get(&source_key) {
                    resort_ids.extend(ids.iter().cloned());
                }
            }
        }

        if resort_ids.is_empty() {
            resort_ids.extend(network_resort_matches(&connection, &network_index));
        }

        if resort_ids.is_empty() {
            warnings.push(format!(
                "connection {} skipped: no confident leaf resort assignment",
                connection.id
            ));
            continue;
        }

        connection.resort_ids = resort_ids.into_iter().collect();
        assigned.push(connection);
    }
    assigned
}

pub(super) fn assign_spots_to_leaf_resorts(
    spots: Vec<FeatureRecord>,
    resorts: &[ResortRecord],
    warnings: &mut Vec<String>,
) -> Vec<FeatureRecord> {
    let leaf_resorts = resorts
        .iter()
        .filter(|resort| resort.resort_type != "domain")
        .collect::<Vec<_>>();
    let leaf_resort_ids = leaf_resorts
        .iter()
        .map(|resort| resort.id.clone())
        .collect::<BTreeSet<_>>();
    let mut assigned = Vec::new();

    for mut spot in spots {
        let mut resort_ids = ski_area_ids(&spot.properties)
            .into_iter()
            .filter(|id| leaf_resort_ids.contains(id))
            .collect::<BTreeSet<_>>();

        if resort_ids.is_empty() {
            if let Some(point) = point_geometry_lon_lat(&spot.geometry) {
                resort_ids.extend(
                    leaf_resorts
                        .iter()
                        .filter(|resort| {
                            point_in_bbox(point, padded_bbox_meters(resort.bbox, 150.0))
                        })
                        .map(|resort| resort.id.clone()),
                );
            }
        }

        if resort_ids.is_empty() {
            warnings.push(format!(
                "spot {} skipped: no confident leaf resort assignment",
                spot.id
            ));
            continue;
        }

        spot.resort_ids = resort_ids.into_iter().collect();
        assigned.push(spot);
    }

    assigned
}

pub(super) fn is_supported_run_feature(feature: &SourceFeature) -> bool {
    let Some(geometry_type) = geometry_type(&feature.geometry) else {
        return false;
    };
    if geometry_type.contains("LineString") {
        return has_any_value(&feature.properties, &["uses", "activities"], "downhill")
            || has_any_value(&feature.properties, &["uses", "activities"], "snow_park");
    }
    if geometry_type.contains("Polygon") {
        return has_any_value(&feature.properties, &["uses", "activities"], "downhill")
            || has_any_value(&feature.properties, &["uses", "activities"], "snow_park")
            || has_any_value(&feature.properties, &["uses", "activities"], "playground");
    }
    false
}

pub(super) fn point_geometry_lon_lat(geometry: &Value) -> Option<[f64; 2]> {
    if geometry_type(geometry) != Some("Point") {
        return None;
    }
    let coords = geometry.get("coordinates")?.as_array()?;
    Some([coords.first()?.as_f64()?, coords.get(1)?.as_f64()?])
}

pub(super) fn point_in_bbox(point: [f64; 2], bbox: [f64; 4]) -> bool {
    point[0] >= bbox[0] && point[0] <= bbox[2] && point[1] >= bbox[1] && point[1] <= bbox[3]
}

pub(super) fn source_resort_index(
    runs: &[FeatureRecord],
    lifts: &[FeatureRecord],
    leaf_resort_ids: &BTreeSet<String>,
) -> HashMap<String, BTreeSet<String>> {
    let mut index: HashMap<String, BTreeSet<String>> = HashMap::new();
    for record in runs.iter().chain(lifts.iter()) {
        let resort_ids = record
            .resort_ids
            .iter()
            .filter(|id| leaf_resort_ids.contains(*id))
            .cloned()
            .collect::<BTreeSet<_>>();
        if resort_ids.is_empty() {
            continue;
        }
        for key in source_keys_from_properties(&record.properties) {
            index
                .entry(key)
                .or_default()
                .extend(resort_ids.iter().cloned());
        }
    }
    index
}

#[derive(Debug)]
pub(super) struct NetworkMatchFeature {
    resort_ids: Vec<String>,
    bbox: [f64; 4],
    endpoints: Vec<[f64; 2]>,
    points: Vec<[f64; 2]>,
}

#[derive(Debug)]
pub(super) struct NetworkMatchIndex {
    features: Vec<NetworkMatchFeature>,
    buckets: HashMap<(i32, i32), Vec<usize>>,
}

pub(super) fn build_network_match_index(
    runs: &[FeatureRecord],
    lifts: &[FeatureRecord],
    leaf_resort_ids: &BTreeSet<String>,
) -> NetworkMatchIndex {
    let features = runs
        .iter()
        .chain(lifts.iter())
        .filter_map(|record| {
            let resort_ids = record
                .resort_ids
                .iter()
                .filter(|id| leaf_resort_ids.contains(*id))
                .cloned()
                .collect::<Vec<_>>();
            if resort_ids.is_empty() {
                return None;
            }
            let bbox = bbox_from_geometry(&record.geometry)?;
            let points = geometry_points(&record.geometry);
            if points.len() < 2 {
                return None;
            }
            let endpoints = geometry_endpoints(&record.geometry);
            Some(NetworkMatchFeature {
                resort_ids,
                bbox,
                endpoints,
                points,
            })
        })
        .collect::<Vec<_>>();
    let mut buckets: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (index, feature) in features.iter().enumerate() {
        for cell in bbox_cells(padded_bbox_meters(feature.bbox, 50.0)) {
            buckets.entry(cell).or_default().push(index);
        }
    }
    NetworkMatchIndex { features, buckets }
}

pub(super) fn network_resort_matches(
    connection: &FeatureRecord,
    network_index: &NetworkMatchIndex,
) -> BTreeSet<String> {
    let Some(connection_bbox) = bbox_from_geometry(&connection.geometry) else {
        return BTreeSet::new();
    };
    let search_bbox = padded_bbox_meters(connection_bbox, CONNECTION_SEARCH_PADDING_METERS);
    let connection_points = geometry_points(&connection.geometry);
    let connection_endpoints = geometry_endpoints(&connection.geometry);
    if connection_points.len() < 2 {
        return BTreeSet::new();
    }

    let mut scores: HashMap<String, i32> = HashMap::new();
    for candidate_index in network_index.candidate_indices(search_bbox) {
        let candidate = &network_index.features[candidate_index];
        if !bbox_intersects(search_bbox, padded_bbox_meters(candidate.bbox, 50.0)) {
            continue;
        }
        let endpoint_score = endpoint_match_score(&connection_endpoints, &candidate.endpoints);
        let segment_score = segment_match_score(&connection_points, &candidate.points);
        let score = endpoint_score * 100 + segment_score * 10;
        if score == 0 {
            continue;
        }
        for resort_id in &candidate.resort_ids {
            scores
                .entry(resort_id.clone())
                .and_modify(|existing| *existing = (*existing).max(score))
                .or_insert(score);
        }
    }

    let Some(best_score) = scores.values().copied().max() else {
        return BTreeSet::new();
    };
    let threshold = if best_score >= 100 { 100 } else { 20 };
    if best_score < threshold {
        return BTreeSet::new();
    }
    scores
        .into_iter()
        .filter(|(_, score)| *score >= threshold && *score >= best_score - 10)
        .map(|(resort_id, _)| resort_id)
        .collect()
}

impl NetworkMatchIndex {
    fn candidate_indices(&self, bbox: [f64; 4]) -> Vec<usize> {
        let mut indices = BTreeSet::new();
        for cell in bbox_cells(bbox) {
            if let Some(bucket) = self.buckets.get(&cell) {
                indices.extend(bucket.iter().copied());
            }
        }
        indices.into_iter().collect()
    }
}

pub(super) fn bbox_cells(bbox: [f64; 4]) -> Vec<(i32, i32)> {
    let min_x = bucket_coord(bbox[0]);
    let max_x = bucket_coord(bbox[2]);
    let min_y = bucket_coord(bbox[1]);
    let max_y = bucket_coord(bbox[3]);
    let mut cells = Vec::new();
    for x in min_x..=max_x {
        for y in min_y..=max_y {
            cells.push((x, y));
        }
    }
    cells
}

pub(super) fn bucket_coord(value: f64) -> i32 {
    (value / NETWORK_BUCKET_DEGREES).floor() as i32
}

pub(super) fn endpoint_match_score(left: &[[f64; 2]], right: &[[f64; 2]]) -> i32 {
    let mut matches = 0;
    for left_point in left {
        if right.iter().any(|right_point| {
            haversine_meters(*left_point, *right_point) <= CONNECTION_ENDPOINT_MATCH_METERS
        }) {
            matches += 1;
        }
    }
    matches
}

pub(super) fn segment_match_score(left_points: &[[f64; 2]], right_points: &[[f64; 2]]) -> i32 {
    let mut matches = 0;
    for point in left_points {
        if min_distance_to_polyline_meters(*point, right_points) <= CONNECTION_SEGMENT_MATCH_METERS
        {
            matches += 1;
        }
    }
    matches.min(3)
}

pub(super) fn source_keys_from_properties(props: &Map<String, Value>) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    if let Some(sources) = props.get("sources").and_then(Value::as_array) {
        for source in sources {
            if let Some(id) = source.get("id").and_then(Value::as_str) {
                keys.insert(id.to_string());
            } else if let Some(id) = value_to_string(source) {
                keys.insert(id);
            }
        }
    }
    if let Some(source) = first_string(props, &["source"]) {
        if source.contains('/') {
            keys.insert(source);
        }
    }
    if let (Some(osm_type), Some(osm_id)) = (
        first_string(props, &["osm_type"]),
        first_string(props, &["osm_id"]),
    ) {
        keys.insert(format!("{osm_type}/{osm_id}"));
    }
    keys
}

pub(super) fn has_any_value(props: &Map<String, Value>, keys: &[&str], needle: &str) -> bool {
    keys.iter()
        .filter_map(|key| props.get(*key))
        .any(|value| value_contains_string(value, needle))
}

pub(super) fn value_contains_string(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text.eq_ignore_ascii_case(needle),
        Value::Array(items) => items.iter().any(|item| value_contains_string(item, needle)),
        Value::Object(object) => object
            .values()
            .any(|item| value_contains_string(item, needle)),
        _ => false,
    }
}
