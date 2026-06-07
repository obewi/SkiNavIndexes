use super::*;
use tempfile::TempDir;

#[test]
fn ski_area_ids_accept_strings_and_objects() {
    let props = Map::from_iter([(
        "skiAreas".to_string(),
        json!([
            "area-a",
            {"id": "area-b"},
            {"type": "Feature", "properties": {"id": "area-c"}},
            42
        ]),
    )]);
    assert_eq!(
        ski_area_ids(&props),
        vec!["42", "area-a", "area-b", "area-c"]
    );
}

#[test]
fn bbox_scans_nested_geojson_coordinates() {
    let geometry = json!({
        "type": "MultiLineString",
        "coordinates": [
            [[10.0, 46.0], [10.2, 46.3]],
            [[9.9, 45.8], [10.1, 46.1]]
        ]
    });
    assert_eq!(bbox_from_geometry(&geometry), Some([9.9, 45.8, 10.2, 46.3]));
}

#[test]
fn openskimap_connection_detection_uses_geojson_type_property() -> Result<()> {
    let cache = TempDir::new()?;
    let dataset_dir = cache.path().join("2026-06-04");
    fs::create_dir_all(&dataset_dir)?;
    write_json_pretty(
        &dataset_dir.join("runs.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "properties": {"id": "run-1", "type": "run", "piste:type": "connection"},
                    "geometry": {"type": "LineString", "coordinates": [[10.0, 46.0], [10.1, 46.1]]}
                }
            ]
        }),
    )?;
    write_json_pretty(
        &dataset_dir.join("lifts.geojson"),
        &json!({"type": "FeatureCollection", "features": []}),
    )?;
    write_json_pretty(
        &dataset_dir.join("ski_areas.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "properties": {"id": "connection-1", "type": "connection"},
                "geometry": {"type": "LineString", "coordinates": [[10.0, 46.0], [10.1, 46.1]]}
            }]
        }),
    )?;

    assert!(openskimap_has_connections(&dataset_dir)?);
    Ok(())
}

#[test]
fn overpass_way_conversion_preserves_raw_piste_type_and_adds_openskimap_type() -> Result<()> {
    let (collection, summary) = overpass_json_to_connection_geojson(&json!({
        "elements": [
            {
                "type": "way",
                "id": 49436042,
                "geometry": [
                    {"lat": 46.5593027, "lon": 11.9532744},
                    {"lat": 46.5594386, "lon": 11.9534193}
                ],
                "tags": {
                    "name": "Armentarola",
                    "piste:type": "connection"
                }
            },
            {"type": "way", "id": 1, "tags": {"piste:type": "connection"}}
        ]
    }))?;

    assert_eq!(summary.feature_count, 1);
    assert_eq!(summary.ignored_count, 1);
    let feature = collection
        .get("features")
        .and_then(Value::as_array)
        .and_then(|features| features.first())
        .expect("converted feature");
    assert_eq!(
        feature.get("id").and_then(Value::as_str),
        Some("way/49436042")
    );
    let props = feature
        .get("properties")
        .and_then(Value::as_object)
        .unwrap();
    assert_eq!(
        props.get("type").and_then(Value::as_str),
        Some("connection")
    );
    assert_eq!(
        props.get("piste:type").and_then(Value::as_str),
        Some("connection")
    );
    assert_eq!(
        props.get("osm_id").and_then(Value::as_str),
        Some("49436042")
    );
    Ok(())
}

#[test]
fn real_dolomiti_connection_is_packaged_with_leaf_resort_not_domain() -> Result<()> {
    let cache = TempDir::new()?;
    let dataset_dir = cache.path().join("2026-06-04");
    fs::create_dir_all(&dataset_dir)?;
    let dolomiti_id = "480f0abbee27a7e26a20a29d9bf947db63bef9a9";
    let alta_badia_id = "41ca531357e0d2a532b8ab94e3e9fe74ddbe88c4";

    write_json_pretty(
        &dataset_dir.join("ski_areas.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "properties": {
                        "id": dolomiti_id,
                        "name": "Dolomiti Superski",
                        "status": "operating",
                        "activities": ["downhill"],
                        "places": [{"iso3166_2": "IT-BL", "iso3166_1Alpha2": "IT"}]
                    },
                    "geometry": {"type": "Polygon", "coordinates": [[[11.8, 46.4], [12.1, 46.4], [12.1, 46.7], [11.8, 46.7], [11.8, 46.4]]]}
                },
                {
                    "type": "Feature",
                    "properties": {
                        "id": alta_badia_id,
                        "name": "Alta Badia",
                        "status": "operating",
                        "activities": ["downhill"],
                        "places": [{"iso3166_2": "IT-BZ", "iso3166_1Alpha2": "IT"}]
                    },
                    "geometry": {"type": "Polygon", "coordinates": [[[11.9, 46.5], [12.0, 46.5], [12.0, 46.6], [11.9, 46.6], [11.9, 46.5]]]}
                }
            ]
        }),
    )?;
    write_json_pretty(
        &dataset_dir.join("runs.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "properties": {
                    "id": "armentarola-run",
                    "name": "Armentarola",
                    "uses": ["downhill"],
                    "status": "operating",
                    "sources": [{"id": "way/49436042", "type": "openstreetmap"}],
                    "skiAreas": [dolomiti_id, alta_badia_id]
                },
                "geometry": {"type": "LineString", "coordinates": [[11.9532744, 46.5593027], [11.9534193, 46.5594386]]}
            }]
        }),
    )?;
    write_json_pretty(
        &dataset_dir.join("lifts.geojson"),
        &json!({"type": "FeatureCollection", "features": []}),
    )?;
    write_json_pretty(
        &dataset_dir.join("connections.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "id": "way/49436042",
                "properties": {
                    "id": "way/49436042",
                    "name": "Armentarola",
                    "type": "connection",
                    "piste:type": "connection",
                    "osm_type": "way",
                    "osm_id": "49436042",
                    "sources": [{"id": "way/49436042", "type": "openstreetmap"}]
                },
                "geometry": {"type": "LineString", "coordinates": [[11.9532744, 46.5593027], [11.9534193, 46.5594386]]}
            }]
        }),
    )?;
    write_json_pretty(
        &dataset_dir.join("spots.geojson"),
        &json!({"type": "FeatureCollection", "features": []}),
    )?;

    let output = TempDir::new()?;
    build_from_cache(cache.path(), output.path(), Some("2026-06-04".to_string()))?;

    let leaf_connections = read_json(
        &output
            .path()
            .join("packages/resorts")
            .join(alta_badia_id)
            .join("connections.geojson"),
    )?;
    assert_eq!(feature_count(&leaf_connections), 1);
    let parent_connections = output
        .path()
        .join("packages/resorts")
        .join(dolomiti_id)
        .join("connections.geojson");
    assert!(!parent_connections.exists());
    let resorts = read_json(&output.path().join("resorts.json"))?;
    let alta_badia = resorts
        .get("resorts")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .find(|resort| resort.get("id").and_then(Value::as_str) == Some(alta_badia_id))
        .expect("Alta Badia resort");
    assert_eq!(
        alta_badia.get("parent_id").and_then(Value::as_str),
        Some(dolomiti_id)
    );
    validate_output(output.path())?;
    Ok(())
}

#[test]
fn connection_assignment_uses_explicit_leaf_ski_area() {
    let mut warnings = Vec::new();
    let leaf_id = "leaf-a".to_string();
    let connections = vec![connection_record(
        "explicit-connection",
        json!({
            "id": "explicit-connection",
            "type": "connection",
            "skiAreas": ["domain-a", "leaf-a"]
        }),
        json!({"type": "LineString", "coordinates": [[10.0, 46.0], [10.01, 46.01]]}),
    )];
    let assigned = assign_connections_to_leaf_resorts(
        connections,
        &domain_and_leaf_resorts("domain-a", &leaf_id),
        &[],
        &[],
        &mut warnings,
    );

    assert_eq!(assigned.len(), 1);
    assert_eq!(assigned[0].resort_ids, vec![leaf_id]);
    assert!(warnings.is_empty());
}

#[test]
fn connection_assignment_uses_network_proximity_and_rejects_bbox_only_matches() {
    let leaf_id = "leaf-a".to_string();
    let resorts = domain_and_leaf_resorts("domain-a", &leaf_id);
    let run = feature_record(
        "run-a",
        vec![leaf_id.clone()],
        json!({"id": "run-a", "uses": ["downhill"]}),
        json!({"type": "LineString", "coordinates": [[10.0, 46.0], [10.01, 46.0]]}),
    );

    let mut warnings = Vec::new();
    let assigned = assign_connections_to_leaf_resorts(
        vec![connection_record(
            "network-connection",
            json!({"id": "network-connection", "type": "connection"}),
            json!({"type": "LineString", "coordinates": [[10.01, 46.0], [10.02, 46.0]]}),
        )],
        &resorts,
        std::slice::from_ref(&run),
        &[],
        &mut warnings,
    );

    assert_eq!(assigned.len(), 1);
    assert_eq!(assigned[0].resort_ids, vec![leaf_id.clone()]);
    assert!(warnings.is_empty());

    let mut warnings = Vec::new();
    let rejected = assign_connections_to_leaf_resorts(
        vec![connection_record(
            "bbox-only-connection",
            json!({"id": "bbox-only-connection", "type": "connection"}),
            json!({"type": "LineString", "coordinates": [[10.0, 46.002], [10.01, 46.002]]}),
        )],
        &resorts,
        &[run],
        &[],
        &mut warnings,
    );

    assert!(rejected.is_empty());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("bbox-only-connection"));
}

#[test]
fn connection_assignment_duplicates_real_bridge_between_leaf_resorts() {
    let left_id = "alta-badia".to_string();
    let right_id = "sellaronda".to_string();
    let mut resorts = domain_and_leaf_resorts("dolomiti", &left_id);
    resorts.push(test_resort(
        &right_id,
        "Sellaronda",
        "resort",
        Some("dolomiti"),
    ));
    let left_run = feature_record(
        "left-run",
        vec![left_id.clone()],
        json!({"id": "left-run", "uses": ["downhill"]}),
        json!({"type": "LineString", "coordinates": [[11.9532744, 46.5593027], [11.9534193, 46.5594386]]}),
    );
    let right_run = feature_record(
        "right-run",
        vec![right_id.clone()],
        json!({"id": "right-run", "uses": ["downhill"]}),
        json!({"type": "LineString", "coordinates": [[11.9534193, 46.5594386], [11.95355, 46.55955]]}),
    );
    let connection = connection_record(
        "way/49436042",
        json!({
            "id": "way/49436042",
            "name": "Armentarola",
            "type": "connection",
            "piste:type": "connection"
        }),
        json!({"type": "LineString", "coordinates": [[11.9532744, 46.5593027], [11.9534193, 46.5594386]]}),
    );

    let mut warnings = Vec::new();
    let assigned = assign_connections_to_leaf_resorts(
        vec![connection],
        &resorts,
        &[left_run, right_run],
        &[],
        &mut warnings,
    );

    assert_eq!(assigned.len(), 1);
    assert_eq!(assigned[0].resort_ids, vec![left_id, right_id]);
    assert!(warnings.is_empty());
}

#[test]
fn build_pipeline_writes_export_layout_without_local_app() -> Result<()> {
    let cache = TempDir::new()?;
    let dataset_dir = cache.path().join("2026-06-03");
    fs::create_dir_all(&dataset_dir)?;
    write_json_pretty(
        &dataset_dir.join("ski_areas.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "properties": {
                    "id": "area-1",
                    "name": "Demo",
                    "status": "operating",
                    "activities": ["downhill"],
                    "runConvention": "europe",
                    "places": [{"iso3166_2": "AT-7", "iso3166_1Alpha2": "AT"}]
                },
                "geometry": {"type": "Point", "coordinates": [10.0, 46.0]}
            }]
        }),
    )?;
    write_json_pretty(
        &dataset_dir.join("runs.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "properties": {
                    "id": "run-1",
                    "name": "Blue One",
                    "difficulty": "easy",
                    "uses": ["downhill"],
                    "status": "operating",
                    "skiAreas": ["area-1"]
                },
                "geometry": {"type": "LineString", "coordinates": [[10.0, 46.1], [10.1, 46.0]]}
            }]
        }),
    )?;
    write_json_pretty(
        &dataset_dir.join("lifts.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "properties": {
                    "id": "lift-1",
                    "name": "Lift",
                    "liftType": "chair_lift",
                    "status": "operating",
                    "skiAreas": ["area-1"]
                },
                "geometry": {"type": "LineString", "coordinates": [[10.1, 46.0], [10.0, 46.1]]}
            }]
        }),
    )?;
    write_json_pretty(
        &dataset_dir.join("connections.geojson"),
        &json!({"type": "FeatureCollection", "features": []}),
    )?;
    write_json_pretty(
        &dataset_dir.join("spots.geojson"),
        &json!({"type": "FeatureCollection", "features": []}),
    )?;

    let output = TempDir::new()?;
    let summary = build_from_cache(cache.path(), output.path(), Some("2026-06-03".to_string()))?;
    assert_eq!(summary.resort_count, 1);
    assert!(output.path().join("resorts.json").exists());
    assert!(
        output
            .path()
            .join("packages/resorts/area-1/manifest.json")
            .exists()
    );
    assert!(!output.path().join("local-app").exists());
    validate_output(output.path())?;
    assert!(output.path().join("release-packs/manifest.json").exists());
    Ok(())
}

#[test]
fn release_pack_planner_splits_large_groups_and_combines_small_groups() {
    let groups = vec![
        ReleaseGroupInput {
            group_id: "AT-7".to_string(),
            resorts: vec![
                ReleaseResortInput {
                    id: "large-a".to_string(),
                    estimated_size_bytes: 18 * 1024 * 1024,
                },
                ReleaseResortInput {
                    id: "large-b".to_string(),
                    estimated_size_bytes: 18 * 1024 * 1024,
                },
            ],
            estimated_size_bytes: 36 * 1024 * 1024,
        },
        ReleaseGroupInput {
            group_id: "BE-A".to_string(),
            resorts: vec![ReleaseResortInput {
                id: "small-a".to_string(),
                estimated_size_bytes: 128 * 1024,
            }],
            estimated_size_bytes: 128 * 1024,
        },
        ReleaseGroupInput {
            group_id: "BE-B".to_string(),
            resorts: vec![ReleaseResortInput {
                id: "small-b".to_string(),
                estimated_size_bytes: 128 * 1024,
            }],
            estimated_size_bytes: 128 * 1024,
        },
        ReleaseGroupInput {
            group_id: "FR-73".to_string(),
            resorts: vec![ReleaseResortInput {
                id: "medium-a".to_string(),
                estimated_size_bytes: 2 * 1024 * 1024,
            }],
            estimated_size_bytes: 2 * 1024 * 1024,
        },
    ];

    let packs = plan_release_packs(groups);
    let asset_names = packs
        .iter()
        .map(|pack| pack.asset_name.as_str())
        .collect::<Vec<_>>();

    assert!(asset_names.contains(&"AT-7.part-001-of-002.tar.gz"));
    assert!(asset_names.contains(&"AT-7.part-002-of-002.tar.gz"));
    assert!(asset_names.contains(&"FR-73.tar.gz"));
    assert!(asset_names.contains(&"small-groups-001.tar.gz"));

    let small_pack = packs
        .iter()
        .find(|pack| pack.asset_name == "small-groups-001.tar.gz")
        .expect("small groups pack");
    assert_eq!(small_pack.archive_type, "small-groups");
    assert_eq!(small_pack.groups.len(), 2);
}

#[test]
fn build_pipeline_writes_new_app_artifact_contract() -> Result<()> {
    let cache = TempDir::new()?;
    let dataset_dir = cache.path().join("2026-06-06");
    fs::create_dir_all(&dataset_dir)?;
    write_json_pretty(
        &dataset_dir.join("ski_areas.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "properties": {
                    "id": "area-1",
                    "name": "Demo",
                    "status": "operating",
                    "activities": ["downhill"],
                    "runConvention": "europe",
                    "places": [{"iso3166_2": "AT-7", "iso3166_1Alpha2": "AT"}]
                },
                "geometry": {"type": "Polygon", "coordinates": [[[10.0, 46.0], [10.2, 46.0], [10.2, 46.2], [10.0, 46.2], [10.0, 46.0]]]}
            }]
        }),
    )?;
    write_json_pretty(
        &dataset_dir.join("runs.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "properties": {
                        "id": "downhill-line",
                        "name": "Blue One",
                        "difficulty": "easy",
                        "uses": ["downhill"],
                        "status": "operating",
                        "skiAreas": ["area-1"],
                        "skiAreaIds": ["area-1"],
                        "elevationProfile": {
                            "heights": [2100.0, 2080.0],
                            "resolution": 25.0,
                            "targetResolution": 25.0
                        }
                    },
                    "geometry": {"type": "LineString", "coordinates": [[10.0, 46.1, 2100.0], [10.1, 46.0, 2080.0]]}
                },
                {
                    "type": "Feature",
                    "properties": {
                        "id": "park-line",
                        "name": "Jump Line",
                        "uses": ["snow_park"],
                        "status": "operating",
                        "skiAreas": ["area-1"],
                        "elevationProfile": {
                            "heights": [2050.0, 2040.0],
                            "resolution": 10.0,
                            "targetResolution": 10.0
                        }
                    },
                    "geometry": {"type": "LineString", "coordinates": [[10.02, 46.1, 2050.0], [10.03, 46.09, 2040.0]]}
                },
                {
                    "type": "Feature",
                    "properties": {
                        "id": "park-polygon",
                        "uses": ["snow_park"],
                        "status": "operating",
                        "skiAreas": ["area-1"]
                    },
                    "geometry": {"type": "Polygon", "coordinates": [[[10.03, 46.11], [10.04, 46.11], [10.04, 46.12], [10.03, 46.12], [10.03, 46.11]]]}
                },
                {
                    "type": "Feature",
                    "properties": {
                        "id": "playground-polygon",
                        "uses": ["playground"],
                        "status": "operating",
                        "skiAreas": ["area-1"]
                    },
                    "geometry": {"type": "Polygon", "coordinates": [[[10.05, 46.11], [10.06, 46.11], [10.06, 46.12], [10.05, 46.12], [10.05, 46.11]]]}
                },
                {
                    "type": "Feature",
                    "properties": {"id": "sled-line", "uses": ["sled"], "status": "operating", "skiAreas": ["area-1"]},
                    "geometry": {"type": "LineString", "coordinates": [[10.0, 46.08], [10.01, 46.08]]}
                },
                {
                    "type": "Feature",
                    "properties": {"id": "skitour-line", "uses": ["skitour"], "status": "operating", "skiAreas": ["area-1"]},
                    "geometry": {"type": "LineString", "coordinates": [[10.0, 46.07], [10.01, 46.07]]}
                }
            ]
        }),
    )?;
    write_json_pretty(
        &dataset_dir.join("lifts.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "properties": {
                    "id": "lift-1",
                    "name": "Lift",
                    "liftType": "chair_lift",
                    "status": "operating",
                    "skiAreas": ["area-1"],
                    "stations": [
                        {
                            "type": "Feature",
                            "properties": {"id": "station-bottom", "skiAreas": ["area-1"], "position": "bottom"},
                            "geometry": {"type": "Point", "coordinates": [10.1, 46.0, 2080.0]}
                        }
                    ]
                },
                "geometry": {"type": "LineString", "coordinates": [[10.1, 46.0, 2080.0], [10.0, 46.1, 2100.0]]}
            }]
        }),
    )?;
    write_json_pretty(
        &dataset_dir.join("connections.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "id": "connection-1",
                "properties": {
                    "id": "connection-1",
                    "type": "connection",
                    "piste:type": "connection",
                    "skiAreas": ["area-1"]
                },
                "geometry": {"type": "LineString", "coordinates": [[10.1, 46.0], [10.11, 46.0]]}
            }]
        }),
    )?;
    write_json_pretty(
        &dataset_dir.join("spots.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [
                {"type": "Feature", "properties": {"id": "crossing-yes", "spotType": "crossing", "dismount": "yes", "skiAreas": ["area-1"]}, "geometry": {"type": "Point", "coordinates": [10.01, 46.01]}},
                {"type": "Feature", "properties": {"id": "crossing-sometimes", "spotType": "crossing", "dismount": "sometimes", "skiAreas": ["area-1"]}, "geometry": {"type": "Point", "coordinates": [10.02, 46.01]}},
                {"type": "Feature", "properties": {"id": "crossing-no", "spotType": "crossing", "dismount": "no", "skiAreas": ["area-1"]}, "geometry": {"type": "Point", "coordinates": [10.03, 46.01]}}
            ]
        }),
    )?;

    let output = TempDir::new()?;
    let summary = build_from_cache(cache.path(), output.path(), Some("2026-06-06".to_string()))?;
    assert_eq!(summary.resort_count, 1);
    assert_eq!(summary.run_count, 4);

    let package = output.path().join("packages/resorts/area-1");
    assert!(package.join("downhill_lines.geojson").exists());
    assert!(package.join("downhill_polygons.geojson").exists());
    assert!(package.join("downhill_centerlines.geojson").exists());
    assert!(package.join("connection_sections.geojson").exists());
    assert!(package.join("spots.geojson").exists());
    assert!(!package.join("runs.geojson").exists());
    assert!(!package.join("run_sections.geojson").exists());
    assert!(!package.join("lift_stations.geojson").exists());
    assert!(!package.join("run_matching_hints.json").exists());
    assert!(!package.join("explore_detail.json").exists());
    assert!(!package.join("checksums.json").exists());

    let downhill_lines = read_json(&package.join("downhill_lines.geojson"))?;
    let line_ids = feature_ids(&downhill_lines);
    assert!(line_ids.contains("downhill-line"));
    assert!(line_ids.contains("park-line"));
    assert!(!line_ids.contains("park-polygon"));
    assert!(!line_ids.contains("playground-polygon"));
    assert!(!line_ids.contains("sled-line"));
    assert!(!line_ids.contains("skitour-line"));

    let downhill_polygons = read_json(&package.join("downhill_polygons.geojson"))?;
    let polygon_ids = feature_ids(&downhill_polygons);
    assert!(!polygon_ids.contains("downhill-line"));
    assert!(!polygon_ids.contains("park-line"));
    assert!(polygon_ids.contains("park-polygon"));
    assert!(polygon_ids.contains("playground-polygon"));
    assert!(
        feature_by_id(&downhill_lines, "downhill-line")
            .and_then(|feature| feature.get("geometry"))
            .and_then(|geometry| geometry.get("coordinates"))
            .and_then(Value::as_array)
            .and_then(|coords| coords.first())
            .and_then(Value::as_array)
            .is_some_and(|coord| coord.len() == 3)
    );
    assert_eq!(
        feature_by_id(&downhill_lines, "downhill-line")
            .and_then(|feature| feature.get("properties"))
            .and_then(|props| props.get("elevationProfile"))
            .and_then(|profile| profile.get("targetResolution"))
            .and_then(Value::as_f64),
        Some(25.0)
    );

    let downhill_centerlines = read_json(&package.join("downhill_centerlines.geojson"))?;
    assert_eq!(
        feature_by_id(&downhill_centerlines, "downhill-line-0")
            .and_then(|feature| feature.get("properties"))
            .and_then(|props| props.get("elevationProfile"))
            .and_then(|profile| profile.get("heights"))
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(2)
    );

    let lifts = read_json(&package.join("lifts.geojson"))?;
    assert_eq!(
        lifts
            .get("features")
            .and_then(Value::as_array)
            .and_then(|features| features.first())
            .and_then(|feature| feature.get("properties"))
            .and_then(|props| props.get("stations"))
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );

    let spots = read_json(&package.join("spots.geojson"))?;
    assert_eq!(feature_count(&spots), 3);
    let dismount_values = spots
        .get("features")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter_map(|feature| {
            feature
                .get("properties")
                .and_then(|props| props.get("dismount"))
                .and_then(Value::as_str)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(dismount_values, BTreeSet::from(["no", "sometimes", "yes"]));

    for file in [
        "downhill_lines.geojson",
        "downhill_polygons.geojson",
        "downhill_centerlines.geojson",
        "lifts.geojson",
        "connections.geojson",
        "connection_sections.geojson",
        "spots.geojson",
    ] {
        assert_no_assignment_keys(&read_json(&package.join(file))?);
    }

    let manifest = read_json(&package.join("manifest.json"))?;
    assert_eq!(
        manifest
            .get("files")
            .and_then(|files| files.get("downhillLines"))
            .and_then(Value::as_str),
        Some("downhill_lines.geojson")
    );
    assert_eq!(
        manifest
            .get("files")
            .and_then(|files| files.get("downhillPolygons"))
            .and_then(Value::as_str),
        Some("downhill_polygons.geojson")
    );
    assert_eq!(
        manifest
            .get("files")
            .and_then(|files| files.get("downhillCenterlines"))
            .and_then(Value::as_str),
        Some("downhill_centerlines.geojson")
    );
    assert_eq!(
        manifest
            .get("files")
            .and_then(|files| files.get("spots"))
            .and_then(Value::as_str),
        Some("spots.geojson")
    );
    assert!(
        !manifest
            .get("files")
            .and_then(Value::as_object)
            .is_some_and(|files| {
                files.contains_key("runMatchingHints") || files.contains_key("exploreDetail")
            })
    );
    assert_eq!(
        manifest
            .get("stats")
            .and_then(|stats| stats.get("spotFeatureCount"))
            .and_then(Value::as_u64),
        Some(3)
    );
    assert_eq!(
        manifest
            .get("stats")
            .and_then(|stats| stats.get("downhillPolygonFeatureCount"))
            .and_then(Value::as_u64),
        Some(2)
    );
    let latest = read_json(&output.path().join("latest.json"))?;
    assert!(
        !latest
            .as_object()
            .unwrap()
            .contains_key("localArtifactRoot")
    );
    assert!(!output.path().join("local-app").exists());
    assert!(output.path().join("release-packs/manifest.json").exists());
    validate_output(output.path())?;
    Ok(())
}

fn domain_and_leaf_resorts(domain_id: &str, leaf_id: &str) -> Vec<ResortRecord> {
    vec![
        test_resort(domain_id, "Domain", "domain", None),
        test_resort(leaf_id, "Leaf", "resort", Some(domain_id)),
    ]
}

fn test_resort(id: &str, name: &str, resort_type: &str, parent_id: Option<&str>) -> ResortRecord {
    ResortRecord {
        id: id.to_string(),
        name: name.to_string(),
        resort_type: resort_type.to_string(),
        parent_id: parent_id.map(str::to_string),
        parent_name: parent_id.map(|_| "Domain".to_string()),
        bbox: [10.0, 46.0, 10.02, 46.02],
        area_km2: 1.0,
        country: Some("IT".to_string()),
        iso_codes: vec!["IT-BZ".to_string()],
        country_codes: vec!["IT".to_string()],
        group_id: "IT-BZ".to_string(),
        center: [10.01, 46.01],
        child_ids: Vec::new(),
        run_convention: Some("europe".to_string()),
        places: Value::Null,
        statistics: Value::Null,
    }
}

fn connection_record(id: &str, properties: Value, geometry: Value) -> FeatureRecord {
    feature_record(id, Vec::new(), properties, geometry)
}

fn feature_record(
    id: &str,
    resort_ids: Vec<String>,
    properties: Value,
    geometry: Value,
) -> FeatureRecord {
    FeatureRecord {
        id: id.to_string(),
        resort_ids,
        properties: properties.as_object().cloned().unwrap_or_default(),
        geometry,
    }
}

fn feature_ids(collection: &Value) -> BTreeSet<&str> {
    collection
        .get("features")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|feature| feature.get("id").and_then(Value::as_str))
        .collect()
}

fn feature_by_id<'a>(collection: &'a Value, id: &str) -> Option<&'a Value> {
    collection
        .get("features")
        .and_then(Value::as_array)?
        .iter()
        .find(|feature| feature.get("id").and_then(Value::as_str) == Some(id))
}

fn assert_no_assignment_keys(value: &Value) {
    match value {
        Value::Object(object) => {
            for key in ["skiAreas", "skiAreaIds", "ski_area_ids", "ski_area"] {
                assert!(
                    !object.contains_key(key),
                    "found assignment key {key} in {value:#}"
                );
            }
            for child in object.values() {
                assert_no_assignment_keys(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                assert_no_assignment_keys(item);
            }
        }
        _ => {}
    }
}
