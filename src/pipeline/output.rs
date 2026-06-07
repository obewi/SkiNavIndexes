use super::*;

pub(super) fn write_outputs(output_dir: &Path, dataset: &NormalizedDataset) -> Result<()> {
    if output_dir.exists() {
        fs::remove_dir_all(output_dir)
            .with_context(|| format!("clearing {}", output_dir.display()))?;
    }
    fs::create_dir_all(output_dir)?;

    let runs_by_resort = records_by_resort(&dataset.runs);
    let lifts_by_resort = records_by_resort(&dataset.lifts);
    let spots_by_resort = records_by_resort(&dataset.spots);
    let connections_by_resort = records_by_resort(&dataset.connections);

    write_discovery_index(output_dir, dataset)?;
    write_resort_packages(
        output_dir,
        dataset,
        &runs_by_resort,
        &lifts_by_resort,
        &spots_by_resort,
        &connections_by_resort,
    )?;
    write_group_archives(output_dir, dataset)?;
    write_release_packs(output_dir, dataset)?;

    let report = json!({
        "datasetVersion": dataset.dataset_version,
        "generatedAt": dataset.generated_at,
        "schemaVersion": PIPELINE_SCHEMA_VERSION,
        "resortCount": dataset.resorts.len(),
        "runCount": dataset.runs.len(),
        "liftCount": dataset.lifts.len(),
        "spotCount": dataset.spots.len(),
        "connectionCount": dataset.connections.len(),
        "warnings": dataset.warnings,
    });
    write_json_pretty(&output_dir.join("build-report.json"), &report)?;
    Ok(())
}

pub(super) fn write_discovery_index(output_dir: &Path, dataset: &NormalizedDataset) -> Result<()> {
    let regions = dataset
        .resorts
        .iter()
        .map(|resort| resort.group_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    let index = json!({
        "version": dataset.dataset_version,
        "datasetVersion": dataset.dataset_version,
        "schemaVersion": PIPELINE_SCHEMA_VERSION,
        "generated_at": dataset.generated_at.to_rfc3339(),
        "total_resorts": dataset.resorts.len(),
        "regions": regions,
        "resorts": dataset.resorts,
    });
    write_json_pretty(&output_dir.join("resorts.json"), &index)?;

    let hash = sha256_file(&output_dir.join("resorts.json"))?;
    let size = fs::metadata(output_dir.join("resorts.json"))?.len();
    let latest = json!({
        "version": dataset.dataset_version,
        "url": format!(
            "https://github.com/obewi/SkiNavIndexes/releases/download/indexes-{}/resorts.json",
            dataset.dataset_version
        ),
        "size": size,
        "hash": format!("sha256:{hash}"),
    });
    write_json_pretty(&output_dir.join("latest.json"), &latest)?;
    Ok(())
}

pub(super) fn write_resort_packages<'a>(
    output_dir: &Path,
    dataset: &NormalizedDataset,
    runs_by_resort: &BTreeMap<String, Vec<&'a FeatureRecord>>,
    lifts_by_resort: &BTreeMap<String, Vec<&'a FeatureRecord>>,
    spots_by_resort: &BTreeMap<String, Vec<&'a FeatureRecord>>,
    connections_by_resort: &BTreeMap<String, Vec<&'a FeatureRecord>>,
) -> Result<()> {
    for resort in &dataset.resorts {
        let package_dir = output_dir
            .join("packages")
            .join("resorts")
            .join(safe_path_id(&resort.id));
        fs::create_dir_all(&package_dir)?;

        if resort.resort_type == "domain" {
            write_domain_package(&package_dir, dataset, resort)?;
            continue;
        }

        let runs = runs_by_resort.get(&resort.id).cloned().unwrap_or_default();
        let lifts = lifts_by_resort.get(&resort.id).cloned().unwrap_or_default();
        let spots = spots_by_resort.get(&resort.id).cloned().unwrap_or_default();
        let connections = connections_by_resort
            .get(&resort.id)
            .cloned()
            .unwrap_or_default();

        let downhill_lines = line_feature_collection(&runs, resort);
        let downhill_polygons = polygon_feature_collection(&runs, resort);
        let downhill_centerlines = centerline_feature_collection(&runs, resort);
        let connection_lines = connection_feature_collection(&connections, resort);
        let connection_sections = connection_section_feature_collection(&connections, resort);
        let lifts_geojson = lift_feature_collection(&lifts);
        let spots_geojson = spot_feature_collection(&spots);
        let lift_station_count = embedded_lift_station_count(&lifts_geojson);
        let audit = audit_report(
            resort,
            runs.len(),
            lifts.len(),
            spots.len(),
            connections.len(),
            &downhill_centerlines,
            &connection_sections,
            lift_station_count,
        );

        write_json_pretty(&package_dir.join("downhill_lines.geojson"), &downhill_lines)?;
        write_json_pretty(
            &package_dir.join("downhill_polygons.geojson"),
            &downhill_polygons,
        )?;
        write_json_pretty(
            &package_dir.join("downhill_centerlines.geojson"),
            &downhill_centerlines,
        )?;
        write_json_pretty(&package_dir.join("connections.geojson"), &connection_lines)?;
        write_json_pretty(
            &package_dir.join("connection_sections.geojson"),
            &connection_sections,
        )?;
        write_json_pretty(&package_dir.join("lifts.geojson"), &lifts_geojson)?;
        write_json_pretty(&package_dir.join("spots.geojson"), &spots_geojson)?;
        write_json_pretty(&package_dir.join("audit_report.json"), &audit)?;

        let manifest = app_render_manifest(
            resort,
            dataset.generated_at,
            runs.len(),
            lifts.len(),
            spots.len(),
            connections.len(),
            &downhill_lines,
            &downhill_centerlines,
            &downhill_polygons,
            &connection_sections,
            &spots_geojson,
            lift_station_count,
        );
        write_json_pretty(&package_dir.join("manifest.json"), &manifest)?;

        let files = file_manifest_for_dir(&package_dir)?;
        let artifact_manifest = json!({
            "schemaVersion": PIPELINE_SCHEMA_VERSION,
            "datasetVersion": dataset.dataset_version,
            "resortId": resort.id,
            "name": resort.name,
            "generatedAt": dataset.generated_at,
            "groupId": resort.group_id,
            "bbox": resort.bbox,
            "isoCodes": resort.iso_codes,
            "countryCodes": resort.country_codes,
            "runConvention": resort.run_convention,
            "statistics": resort.statistics,
            "places": resort.places,
            "files": files,
            "stats": {
                "runs": runs.len(),
                "lifts": lifts.len(),
                "spots": spots.len(),
                "connections": connections.len(),
                "downhillLines": feature_count(&downhill_lines),
                "downhillPolygons": feature_count(&downhill_polygons),
                "downhillCenterlines": feature_count(&downhill_centerlines),
                "connectionSections": feature_count(&connection_sections),
                "liftStations": lift_station_count
            },
            "licenses": [
                {
                    "name": "OpenSkiMap / OpenSkiStats",
                    "url": "https://openskistats.org/"
                },
                {
                    "name": "OpenStreetMap contributors",
                    "url": "https://www.openstreetmap.org/copyright"
                }
            ]
        });
        write_json_pretty(
            &package_dir.join("artifact_manifest.json"),
            &artifact_manifest,
        )?;
    }
    Ok(())
}

pub(super) fn write_domain_package(
    package_dir: &Path,
    dataset: &NormalizedDataset,
    resort: &ResortRecord,
) -> Result<()> {
    let child_artifacts = resort
        .child_ids
        .iter()
        .map(|child_id| {
            json!({
                "id": child_id,
                "renderBundlePath": format!("../{}/manifest.json", safe_path_id(child_id))
            })
        })
        .collect::<Vec<_>>();

    let manifest = json!({
        "schemaVersion": PIPELINE_SCHEMA_VERSION,
        "datasetVersion": dataset.dataset_version,
        "resortId": resort.id,
        "name": resort.name,
        "type": "domain",
        "generatedAt": dataset.generated_at,
        "childIds": resort.child_ids,
        "childArtifacts": child_artifacts,
        "note": "Domain package is reference-only; child resort packages own render and routing source artifacts."
    });
    write_json_pretty(&package_dir.join("manifest.json"), &manifest)?;

    let audit = json!({
        "schemaVersion": PIPELINE_SCHEMA_VERSION,
        "resortId": resort.id,
        "generatedAt": dataset.generated_at,
        "stats": {
            "children": resort.child_ids.len(),
            "runs": 0,
            "lifts": 0
        },
        "issues": ["domain_reference_only"],
        "qualityScore": 100
    });
    write_json_pretty(&package_dir.join("audit_report.json"), &audit)?;

    let files = file_manifest_for_dir(package_dir)?;
    let artifact_manifest = json!({
        "schemaVersion": PIPELINE_SCHEMA_VERSION,
        "datasetVersion": dataset.dataset_version,
        "resortId": resort.id,
        "name": resort.name,
        "type": "domain",
        "generatedAt": dataset.generated_at,
        "groupId": resort.group_id,
        "bbox": resort.bbox,
        "isoCodes": resort.iso_codes,
        "countryCodes": resort.country_codes,
        "runConvention": resort.run_convention,
        "statistics": resort.statistics,
        "places": resort.places,
        "childIds": resort.child_ids,
        "childArtifacts": child_artifacts,
        "files": files,
        "stats": {
            "children": resort.child_ids.len(),
            "runs": 0,
            "lifts": 0,
            "spots": 0,
            "downhillCenterlines": 0,
            "connectionSections": 0,
            "liftStations": 0
        },
        "licenses": [
            {
                "name": "OpenSkiMap / OpenSkiStats",
                "url": "https://openskistats.org/"
            },
            {
                "name": "OpenStreetMap contributors",
                "url": "https://www.openstreetmap.org/copyright"
            }
        ]
    });
    write_json_pretty(
        &package_dir.join("artifact_manifest.json"),
        &artifact_manifest,
    )?;

    Ok(())
}

pub(super) fn records_by_resort(
    records: &[FeatureRecord],
) -> BTreeMap<String, Vec<&FeatureRecord>> {
    let mut grouped: BTreeMap<String, Vec<&FeatureRecord>> = BTreeMap::new();
    for record in records {
        for resort_id in &record.resort_ids {
            grouped.entry(resort_id.clone()).or_default().push(record);
        }
    }
    grouped
}

pub(super) fn write_group_archives(output_dir: &Path, dataset: &NormalizedDataset) -> Result<()> {
    let archive_dir = output_dir.join("groups");
    fs::create_dir_all(&archive_dir)?;
    let mut by_group: BTreeMap<String, Vec<&ResortRecord>> = BTreeMap::new();
    for resort in &dataset.resorts {
        by_group
            .entry(resort.group_id.clone())
            .or_default()
            .push(resort);
    }

    for (group_id, resorts) in by_group {
        let group_manifest = json!({
            "schemaVersion": PIPELINE_SCHEMA_VERSION,
            "datasetVersion": dataset.dataset_version,
            "groupId": group_id,
            "generatedAt": dataset.generated_at,
            "resorts": resorts.iter().map(|resort| {
                json!({
                    "id": resort.id,
                    "name": resort.name,
                    "path": format!("resorts/{}/manifest.json", safe_path_id(&resort.id)),
                    "bbox": resort.bbox,
                    "isoCodes": resort.iso_codes,
                    "countryCodes": resort.country_codes,
                    "parentId": resort.parent_id,
                    "childIds": resort.child_ids,
                })
            }).collect::<Vec<_>>(),
        });
        let staging = archive_dir.join(safe_path_id(&group_id));
        fs::create_dir_all(&staging)?;
        write_json_pretty(&staging.join("manifest.json"), &group_manifest)?;
        for resort in resorts {
            let source = output_dir
                .join("packages")
                .join("resorts")
                .join(safe_path_id(&resort.id));
            let target = staging.join("resorts").join(safe_path_id(&resort.id));
            copy_dir_recursive(&source, &target)?;
        }
        let archive_path = archive_dir.join(format!("{}.tar.gz", safe_path_id(&group_id)));
        create_tar_gz(&staging, &archive_path)?;
        fs::remove_dir_all(&staging)?;
    }
    Ok(())
}

pub(super) fn line_feature_collection(records: &[&FeatureRecord], resort: &ResortRecord) -> Value {
    let features = records
        .iter()
        .filter(|record| geometry_type(&record.geometry).is_some_and(|ty| ty.contains("LineString")))
        .map(|record| {
            let mut props = normalized_run_properties(record, resort);
            props.insert("id".to_string(), Value::String(record.id.clone()));
            json!({"type": "Feature", "id": record.id, "properties": props, "geometry": record.geometry})
        })
        .collect::<Vec<_>>();
    json!({"type": "FeatureCollection", "features": features})
}

pub(super) fn polygon_feature_collection(
    records: &[&FeatureRecord],
    resort: &ResortRecord,
) -> Value {
    let features = records
        .iter()
        .filter(|record| geometry_type(&record.geometry).is_some_and(|ty| ty.contains("Polygon")))
        .map(|record| {
            let mut props = normalized_run_properties(record, resort);
            props.insert("id".to_string(), Value::String(record.id.clone()));
            json!({"type": "Feature", "id": record.id, "properties": props, "geometry": record.geometry})
        })
        .collect::<Vec<_>>();
    json!({"type": "FeatureCollection", "features": features})
}

pub(super) fn centerline_feature_collection(
    records: &[&FeatureRecord],
    resort: &ResortRecord,
) -> Value {
    let mut features = Vec::new();
    for record in records {
        for (index, line) in line_geometries(&record.geometry).into_iter().enumerate() {
            let section_id = format!("{}-{index}", record.id);
            let coordinates = line
                .get("coordinates")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let start_key = coordinates.first().and_then(endpoint_key_from_coord_value);
            let end_key = coordinates.last().and_then(endpoint_key_from_coord_value);
            let mut props = normalized_run_properties(record, resort);
            props.insert("run_key".to_string(), Value::String(record.id.clone()));
            props.insert(
                "completion_family_key".to_string(),
                Value::String(record.id.clone()),
            );
            props.insert(
                "completion_section_id".to_string(),
                Value::String(section_id.clone()),
            );
            props.insert(
                "centerline_id".to_string(),
                Value::String(section_id.clone()),
            );
            props.insert("section_id".to_string(), Value::String(section_id.clone()));
            props.insert(
                "source_way_id".to_string(),
                Value::String(record.id.clone()),
            );
            props.insert(
                "start_endpoint_key".to_string(),
                opt_string_value(start_key),
            );
            props.insert("end_endpoint_key".to_string(), opt_string_value(end_key));
            let oneway_tag = first_string(&record.properties, &["oneway", "direction"])
                .unwrap_or_else(|| "unknown".to_string());
            props.insert(
                "effective_oneway_tag".to_string(),
                Value::String(oneway_tag.clone()),
            );
            props.insert(
                "effective_oneway".to_string(),
                Value::Bool(matches!(oneway_tag.as_str(), "yes" | "true" | "1" | "-1")),
            );
            props.insert(
                "direction_source".to_string(),
                Value::String(
                    if oneway_tag == "unknown" {
                        "none"
                    } else {
                        "openskimap"
                    }
                    .to_string(),
                ),
            );
            features.push(json!({
                "type": "Feature",
                "id": section_id,
                "properties": props,
                "geometry": line
            }));
        }
    }
    json!({"type": "FeatureCollection", "features": features})
}

pub(super) fn connection_feature_collection(
    records: &[&FeatureRecord],
    resort: &ResortRecord,
) -> Value {
    let features = records
        .iter()
        .filter(|record| geometry_type(&record.geometry).is_some_and(|ty| ty.contains("LineString")))
        .map(|record| {
            let mut props = normalized_connection_properties(record, resort);
            props.insert("id".to_string(), Value::String(record.id.clone()));
            json!({"type": "Feature", "id": record.id, "properties": props, "geometry": record.geometry})
        })
        .collect::<Vec<_>>();
    json!({"type": "FeatureCollection", "features": features})
}

pub(super) fn connection_section_feature_collection(
    records: &[&FeatureRecord],
    resort: &ResortRecord,
) -> Value {
    let mut features = Vec::new();
    for record in records {
        for (index, line) in line_geometries(&record.geometry).into_iter().enumerate() {
            let section_id = format!("{}-connection-{index}", record.id);
            let coordinates = line
                .get("coordinates")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let start_key = coordinates.first().and_then(endpoint_key_from_coord_value);
            let end_key = coordinates.last().and_then(endpoint_key_from_coord_value);
            let mut props = normalized_connection_properties(record, resort);
            props.insert("run_key".to_string(), Value::String(record.id.clone()));
            props.insert(
                "completion_family_key".to_string(),
                Value::String(record.id.clone()),
            );
            props.insert(
                "completion_section_id".to_string(),
                Value::String(section_id.clone()),
            );
            props.insert(
                "centerline_id".to_string(),
                Value::String(section_id.clone()),
            );
            props.insert("section_id".to_string(), Value::String(section_id.clone()));
            props.insert(
                "source_way_id".to_string(),
                Value::String(record.id.clone()),
            );
            props.insert(
                "start_endpoint_key".to_string(),
                opt_string_value(start_key),
            );
            props.insert("end_endpoint_key".to_string(), opt_string_value(end_key));
            let oneway_tag = first_string(&record.properties, &["oneway", "direction"])
                .unwrap_or_else(|| "unknown".to_string());
            props.insert(
                "effective_oneway_tag".to_string(),
                Value::String(oneway_tag.clone()),
            );
            props.insert(
                "effective_oneway".to_string(),
                Value::Bool(matches!(oneway_tag.as_str(), "yes" | "true" | "1" | "-1")),
            );
            props.insert(
                "direction_source".to_string(),
                Value::String(
                    if oneway_tag == "unknown" {
                        "none"
                    } else {
                        "connection"
                    }
                    .to_string(),
                ),
            );
            features.push(json!({
                "type": "Feature",
                "id": section_id,
                "properties": props,
                "geometry": line
            }));
        }
    }
    json!({"type": "FeatureCollection", "features": features})
}

pub(super) fn lift_feature_collection(records: &[&FeatureRecord]) -> Value {
    let features = records
        .iter()
        .filter(|record| geometry_type(&record.geometry) == Some("LineString"))
        .map(|record| {
            let mut props = sanitize_app_properties(&record.properties);
            props.insert("id".to_string(), Value::String(record.id.clone()));
            props.insert("run_key".to_string(), Value::String(record.id.clone()));
            props.insert("source_way_id".to_string(), Value::String(record.id.clone()));
            if let Some(lift_type) = first_string(&props, &["liftType", "lift_type", "type"]) {
                props.insert("lift_type".to_string(), Value::String(lift_type));
            }
            if !props.contains_key("oneway_tag") {
                props.insert("oneway_tag".to_string(), Value::String("yes".to_string()));
            }
            json!({"type": "Feature", "id": record.id, "properties": props, "geometry": record.geometry})
        })
        .collect::<Vec<_>>();
    json!({"type": "FeatureCollection", "features": features})
}

pub(super) fn normalized_run_properties(
    record: &FeatureRecord,
    resort: &ResortRecord,
) -> Map<String, Value> {
    let mut props = sanitize_app_properties(&record.properties);
    props.insert(
        "source_way_id".to_string(),
        Value::String(record.id.clone()),
    );
    if has_any_value(&record.properties, &["uses", "activities"], "snow_park") {
        props.insert(
            "piste_type".to_string(),
            Value::String("snow_park".to_string()),
        );
        props.insert(
            "display_type".to_string(),
            Value::String("Terrain Park".to_string()),
        );
    } else if has_any_value(&record.properties, &["uses", "activities"], "playground") {
        props.insert(
            "piste_type".to_string(),
            Value::String("playground".to_string()),
        );
    } else {
        props.insert(
            "piste_type".to_string(),
            Value::String("downhill".to_string()),
        );
    }
    if let Some(name) = first_string(&props, &["name"]) {
        props.insert("run_name".to_string(), Value::String(name));
    }
    if let Some(reference) = first_string(&props, &["ref"]) {
        props.insert("run_ref".to_string(), Value::String(reference));
    }
    if let Some(difficulty) = first_string(&props, &["difficulty", "color"]) {
        props.insert("difficulty".to_string(), Value::String(difficulty));
    }
    if let Some(scale) = first_string(&props, &["difficultyConvention", "difficulty_convention"]) {
        props.insert("difficulty_scale".to_string(), Value::String(scale));
    } else if let Some(convention) = &resort.run_convention {
        props.insert(
            "difficulty_scale".to_string(),
            Value::String(convention.clone()),
        );
    }
    if let Some(country) = resort.country.clone() {
        props.insert("country_code".to_string(), Value::String(country));
    }
    props
}

pub(super) fn normalized_connection_properties(
    record: &FeatureRecord,
    resort: &ResortRecord,
) -> Map<String, Value> {
    let mut props = sanitize_app_properties(&record.properties);
    props.insert("type".to_string(), Value::String("connection".to_string()));
    props.insert(
        "feature_kind".to_string(),
        Value::String("connection".to_string()),
    );
    props.insert(
        "source_way_id".to_string(),
        Value::String(record.id.clone()),
    );
    if let Some(country) = resort.country.clone() {
        props.insert("country_code".to_string(), Value::String(country));
    }
    props
}

pub(super) fn spot_feature_collection(records: &[&FeatureRecord]) -> Value {
    let features = records
        .iter()
        .map(|record| {
            let mut props = sanitize_app_properties(&record.properties);
            props.insert("id".to_string(), Value::String(record.id.clone()));
            json!({"type": "Feature", "id": record.id, "properties": props, "geometry": record.geometry})
        })
        .collect::<Vec<_>>();
    json!({"type": "FeatureCollection", "features": features})
}

pub(super) fn sanitize_app_properties(props: &Map<String, Value>) -> Map<String, Value> {
    let mut props = props.clone();
    prune_assignment_keys_from_map(&mut props);
    props
}

pub(super) fn prune_assignment_keys_from_map(map: &mut Map<String, Value>) {
    for key in ["skiAreas", "skiAreaIds", "ski_area_ids", "ski_area"] {
        map.remove(key);
    }
    for value in map.values_mut() {
        prune_assignment_keys(value);
    }
}

pub(super) fn prune_assignment_keys(value: &mut Value) {
    match value {
        Value::Object(object) => prune_assignment_keys_from_map(object),
        Value::Array(items) => {
            for item in items {
                prune_assignment_keys(item);
            }
        }
        _ => {}
    }
}

pub(super) fn embedded_lift_station_count(lifts_geojson: &Value) -> usize {
    lifts_geojson
        .get("features")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|feature| feature.get("properties"))
        .filter_map(|props| props.get("stations"))
        .filter_map(Value::as_array)
        .map(Vec::len)
        .sum()
}

pub(super) fn app_render_manifest(
    resort: &ResortRecord,
    generated_at: DateTime<Utc>,
    run_count: usize,
    lift_count: usize,
    spot_count: usize,
    connection_count: usize,
    downhill_lines: &Value,
    downhill_centerlines: &Value,
    downhill_polygons: &Value,
    connection_sections: &Value,
    spots: &Value,
    lift_station_count: usize,
) -> Value {
    json!({
        "schemaVersion": RENDER_SCHEMA_VERSION,
        "regionId": resort.id,
        "generatedAt": generated_at,
        "sourceTimestamp": generated_at,
        "files": {
            "downhillLines": "downhill_lines.geojson",
            "downhillCenterlines": "downhill_centerlines.geojson",
            "downhillPolygons": "downhill_polygons.geojson",
            "connections": "connections.geojson",
            "connectionSections": "connection_sections.geojson",
            "lifts": "lifts.geojson",
            "spots": "spots.geojson"
        },
        "stats": {
            "downhillLineFeatureCount": feature_count(downhill_lines),
            "runSourceFeatureCount": run_count,
            "downhillCenterlineFeatureCount": feature_count(downhill_centerlines),
            "downhillCenterlineLabeledFeatureCount": feature_count(downhill_centerlines),
            "explicitOnewayCenterlineCount": count_section_direction(downhill_centerlines, "openskimap", true),
            "inferredOnewayCenterlineCount": count_section_direction(downhill_centerlines, "inferred", true),
            "unknownDirectionCenterlineCount": count_unknown_direction_sections(downhill_centerlines),
            "downhillPolygonFeatureCount": feature_count(downhill_polygons),
            "connectionFeatureCount": connection_count,
            "connectionSectionFeatureCount": feature_count(connection_sections),
            "liftFeatureCount": lift_count,
            "liftStationFeatureCount": lift_station_count,
            "spotFeatureCount": feature_count(spots),
            "spotSourceFeatureCount": spot_count
        }
    })
}

pub(super) fn audit_report(
    resort: &ResortRecord,
    run_count: usize,
    lift_count: usize,
    spot_count: usize,
    connection_count: usize,
    run_sections: &Value,
    connection_sections: &Value,
    lift_station_count: usize,
) -> Value {
    let mut issues = Vec::new();
    if run_count == 0 {
        issues.push("no_downhill_runs");
    }
    if lift_count == 0 {
        issues.push("no_lifts");
    }
    json!({
        "schemaVersion": PIPELINE_SCHEMA_VERSION,
        "resortId": resort.id,
        "generatedAt": Utc::now(),
        "stats": {
            "runs": run_count,
            "lifts": lift_count,
            "spots": spot_count,
            "connections": connection_count,
            "downhillCenterlines": feature_count(run_sections),
            "connectionSections": feature_count(connection_sections),
            "liftStations": lift_station_count
        },
        "issues": issues,
        "qualityScore": if issues.is_empty() { 100 } else { 70 }
    })
}
