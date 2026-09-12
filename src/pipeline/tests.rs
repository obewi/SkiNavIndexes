use super::*;
use flate2::read::GzDecoder;
use rusqlite::{Connection, OptionalExtension};
use std::{
    fs::File,
    io::{Read, Write, copy},
    net::TcpListener,
};
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
fn stored_properties_prune_nested_assignment_properties() {
    let props = Map::from_iter([
        ("skiAreas".to_string(), json!(["area-a"])),
        (
            "stations".to_string(),
            json!([{
                "properties": {
                    "id": "station-a",
                    "skiAreas": [],
                    "skiAreaIds": ["area-a"],
                    "ski_area_ids": ["area-a"],
                    "ski_area": "area-a"
                }
            }]),
        ),
    ]);

    let stored = stored_properties(&props);
    assert!(stored.get("skiAreas").is_none());
    let station_properties = stored["stations"][0]["properties"]
        .as_object()
        .expect("station properties");
    assert_eq!(station_properties.get("id"), Some(&json!("station-a")));
    for key in ["skiAreas", "skiAreaIds", "ski_area_ids", "ski_area"] {
        assert!(
            station_properties.get(key).is_none(),
            "nested assignment-only property was persisted: {key}"
        );
    }
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
fn parent_owned_features_survive_hierarchy_normalization_and_sqlite_output() -> Result<()> {
    let domain_id = "domain-d";
    let child_a_id = "child-a";
    let child_b_id = "child-b";
    let ski_areas = vec![
        source_feature(
            json!({
                "id": domain_id,
                "name": "Domain D",
                "status": "operating",
                "activities": ["downhill"]
            }),
            json!({
                "type": "Polygon",
                "coordinates": [[[10.0, 46.0], [10.2, 46.0], [10.2, 46.2], [10.0, 46.2], [10.0, 46.0]]]
            }),
        ),
        source_feature(
            json!({
                "id": child_a_id,
                "name": "Child A",
                "status": "operating",
                "activities": ["downhill"]
            }),
            json!({
                "type": "Polygon",
                "coordinates": [[[10.02, 46.02], [10.08, 46.02], [10.08, 46.08], [10.02, 46.08], [10.02, 46.02]]]
            }),
        ),
        source_feature(
            json!({
                "id": child_b_id,
                "name": "Child B",
                "status": "operating",
                "activities": ["downhill"]
            }),
            json!({
                "type": "Polygon",
                "coordinates": [[[10.12, 46.12], [10.18, 46.12], [10.18, 46.18], [10.12, 46.18], [10.12, 46.12]]]
            }),
        ),
    ];
    let runs = vec![
        source_feature(
            json!({
                "id": "parent-run",
                "uses": ["downhill"],
                "status": "operating",
                "skiAreas": [domain_id],
                "elevationProfile": {
                    "heights": [100.0, 90.0],
                    "resolution": 10.0,
                    "targetResolution": 5.0
                }
            }),
            json!({"type": "LineString", "coordinates": [[10.04, 46.04], [10.05, 46.05]]}),
        ),
        source_feature(
            json!({
                "id": "child-a-run",
                "uses": ["downhill"],
                "status": "operating",
                "skiAreas": [domain_id, child_a_id]
            }),
            json!({"type": "LineString", "coordinates": [[10.06, 46.06], [10.07, 46.07]]}),
        ),
        source_feature(
            json!({
                "id": "child-b-run",
                "uses": ["downhill"],
                "status": "operating",
                "skiAreas": [domain_id, child_b_id]
            }),
            json!({"type": "LineString", "coordinates": [[10.14, 46.14], [10.15, 46.15]]}),
        ),
    ];
    let spots = vec![source_feature(
        json!({"id": "parent-spot", "skiAreas": [domain_id]}),
        json!({"type": "Point", "coordinates": [10.05, 46.05]}),
    )];
    let connections = vec![source_feature(
        json!({
            "id": "parent-connection",
            "type": "connection",
            "piste:type": "connection",
            "skiAreas": [domain_id]
        }),
        json!({"type": "LineString", "coordinates": [[10.05, 46.05], [10.06, 46.06]]}),
    )];

    let dataset = normalize_sources_with_topology(
        ski_areas,
        runs,
        Vec::new(),
        spots,
        connections,
        Vec::new(),
        "2026-09-10",
        Utc::now(),
    )?;
    assert_eq!(
        dataset
            .resorts
            .iter()
            .filter(|resort| resort.parent_id.as_deref() == Some(domain_id))
            .map(|resort| resort.id.clone())
            .collect::<Vec<_>>(),
        vec![child_a_id.to_string(), child_b_id.to_string()]
    );
    assert_eq!(
        dataset
            .runs
            .iter()
            .find(|run| run.id == "parent-run")
            .expect("parent-owned run")
            .resort_ids,
        vec![domain_id.to_string()]
    );
    assert_eq!(
        dataset
            .spots
            .iter()
            .find(|spot| spot.id == "parent-spot")
            .expect("parent-owned spot")
            .resort_ids,
        vec![domain_id.to_string()]
    );
    assert_eq!(
        dataset
            .connections
            .iter()
            .find(|connection| connection.id == "parent-connection")
            .expect("parent-owned connection")
            .resort_ids,
        vec![domain_id.to_string()]
    );

    let output = TempDir::new()?;
    write_source_outputs(output.path(), &dataset)?;
    validate_output(output.path())?;
    assert_canonical_output(output.path())?;

    let catalog_dir = unpack_gzip_asset(output.path(), "catalog.sqlite.gz")?;
    let catalog = Connection::open(catalog_dir.path().join("catalog.sqlite"))?;
    assert_eq!(
        catalog.query_row(
            "SELECT parent_id FROM resorts WHERE id = ?1",
            [child_a_id],
            |row| row.get::<_, Option<String>>(0)
        )?,
        Some(domain_id.to_string())
    );
    assert_eq!(
        catalog.query_row(
            "SELECT run_count FROM resort_source_stats WHERE resort_id = ?1",
            [domain_id],
            |row| row.get::<_, i64>(0)
        )?,
        3
    );

    let pack_name = fs::read_dir(output.path())?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| name.starts_with("pack-") && name.ends_with(".sqlite.gz"))
        .expect("source pack");
    let pack_dir = unpack_gzip_asset(output.path(), &pack_name)?;
    let pack = Connection::open(pack_dir.path().join(pack_name.trim_end_matches(".gz")))?;
    let parent_properties: String = pack.query_row(
        "SELECT properties_json FROM runs WHERE id = ?1",
        ["parent-run"],
        |row| row.get(0),
    )?;
    let parent_properties: Value = serde_json::from_str(&parent_properties)?;
    for key in ["skiAreas", "skiAreaIds", "ski_area_ids", "ski_area"] {
        assert!(
            parent_properties.get(key).is_none(),
            "assignment-only property was persisted: {key}"
        );
    }
    assert_eq!(parent_properties.get("uses"), Some(&json!(["downhill"])));
    let parent_run = pack
        .query_row(
            "SELECT typeof(geometry_wkb), length(geometry_wkb), typeof(elevation_profile), length(elevation_profile), elevation_resolution, elevation_target_resolution FROM runs WHERE id = ?1",
            ["parent-run"],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, f64>(5)?,
                ))
            },
        )
        .optional()?;
    assert_eq!(
        parent_run,
        Some(("blob".to_string(), 41, "blob".to_string(), 16, 10.0, 5.0))
    );
    Ok(())
}

#[test]
fn source_pack_planner_keeps_hierarchy_local_and_combines_nearby_standalone_resorts() -> Result<()>
{
    let mut domain = test_resort("domain", "Domain", "domain", None);
    domain.center = [10.0, 46.0];
    let mut child = test_resort("child", "Child", "resort", Some("domain"));
    child.center = [10.2, 46.2];
    let mut nearby_a = test_resort("nearby-a", "Nearby A", "resort", None);
    nearby_a.center = [10.4, 46.4];
    let mut nearby_b = test_resort("nearby-b", "Nearby B", "resort", None);
    nearby_b.center = [11.0, 46.8];

    let dataset = NormalizedDataset {
        dataset_version: "2026-09-10".to_string(),
        generated_at: Utc::now(),
        resorts: vec![domain, child, nearby_a, nearby_b],
        runs: Vec::new(),
        lifts: Vec::new(),
        spots: Vec::new(),
        connections: Vec::new(),
        lift_station_memberships: Vec::new(),
    };

    let plans = plan_source_packs(&dataset)?;
    let hierarchy_plan = plans
        .iter()
        .find(|plan| plan.resort_ids.iter().any(|id| id == "domain"))
        .expect("hierarchy pack");
    assert_eq!(hierarchy_plan.resort_ids, vec!["child", "domain"]);
    assert!(!hierarchy_plan.resort_ids.iter().any(|id| id == "nearby-a"));

    let nearby_plan = plans
        .iter()
        .find(|plan| plan.resort_ids.iter().any(|id| id == "nearby-a"))
        .expect("nearby standalone pack");
    assert!(nearby_plan.resort_ids.iter().any(|id| id == "nearby-b"));
    Ok(())
}

#[test]
fn source_pack_planner_does_not_combine_standalone_resorts_across_cells() -> Result<()> {
    let mut west = test_resort("west", "West", "resort", None);
    west.center = [10.0, 46.0];
    let mut east = test_resort("east", "East", "resort", None);
    east.center = [13.0, 46.0];
    let dataset = NormalizedDataset {
        dataset_version: "2026-09-10".to_string(),
        generated_at: Utc::now(),
        resorts: vec![west, east],
        runs: Vec::new(),
        lifts: Vec::new(),
        spots: Vec::new(),
        connections: Vec::new(),
        lift_station_memberships: Vec::new(),
    };

    let plans = plan_source_packs(&dataset)?;
    assert_eq!(plans.len(), 2);
    assert!(plans.iter().all(|plan| plan.resort_ids.len() == 1));
    Ok(())
}

#[test]
fn source_pack_planner_splits_an_oversized_hierarchy_without_mixing_roots() -> Result<()> {
    let domain = test_resort("domain", "Domain", "domain", None);
    let child = test_resort("child", "Child", "resort", Some("domain"));
    let payload = "x".repeat(9 * 1024 * 1024);
    let feature_properties = Map::from_iter([(String::from("payload"), Value::String(payload))]);
    let dataset = NormalizedDataset {
        dataset_version: "2026-09-10".to_string(),
        generated_at: Utc::now(),
        resorts: vec![domain, child],
        runs: vec![
            FeatureRecord {
                id: "domain-run".to_string(),
                resort_ids: vec!["domain".to_string()],
                geometry: json!({"type": "LineString", "coordinates": [[10.0, 46.0], [10.01, 46.01]]}),
                properties: feature_properties.clone(),
            },
            FeatureRecord {
                id: "child-run".to_string(),
                resort_ids: vec!["child".to_string()],
                geometry: json!({"type": "LineString", "coordinates": [[10.1, 46.1], [10.11, 46.11]]}),
                properties: feature_properties,
            },
        ],
        lifts: Vec::new(),
        spots: Vec::new(),
        connections: Vec::new(),
        lift_station_memberships: Vec::new(),
    };

    let plans = plan_source_packs(&dataset)?;
    assert_eq!(plans.len(), 2);
    assert!(plans.iter().all(|plan| plan.resort_ids.len() == 1));
    assert!(plans.iter().all(|plan| !plan.allows_oversized));
    Ok(())
}

#[test]
fn normalized_validation_rejects_orphan_feature_ownership() {
    let dataset = NormalizedDataset {
        dataset_version: "2026-09-10".to_string(),
        generated_at: Utc::now(),
        resorts: vec![test_resort("known", "Known", "resort", None)],
        runs: vec![feature_record(
            "orphan-run",
            vec!["missing".to_string()],
            json!({"uses": ["downhill"]}),
            json!({"type": "LineString", "coordinates": [[10.0, 46.0], [10.01, 46.01]]}),
        )],
        lifts: Vec::new(),
        spots: Vec::new(),
        connections: Vec::new(),
        lift_station_memberships: Vec::new(),
    };

    let error = validate_normalized_dataset(&dataset).expect_err("orphan ownership must fail");
    assert!(
        error
            .to_string()
            .contains("references missing resort missing")
    );
}

#[test]
fn normalized_validation_rejects_hierarchy_cycles() {
    let left = test_resort("left", "Left", "resort", Some("right"));
    let right = test_resort("right", "Right", "resort", Some("left"));
    let dataset = NormalizedDataset {
        dataset_version: "2026-09-10".to_string(),
        generated_at: Utc::now(),
        resorts: vec![left, right],
        runs: Vec::new(),
        lifts: Vec::new(),
        spots: Vec::new(),
        connections: Vec::new(),
        lift_station_memberships: Vec::new(),
    };

    let error = validate_normalized_dataset(&dataset).expect_err("cycle must fail");
    assert!(error.to_string().contains("hierarchy contains a cycle"));
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
            "features": [{
                "type": "Feature",
                "properties": {"id": "run-1", "type": "run", "piste:type": "connection"},
                "geometry": {"type": "LineString", "coordinates": [[10.0, 46.0], [10.1, 46.1]]}
            }]
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
                "tags": {"name": "Armentarola", "piste:type": "connection"}
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
fn station_topology_query_is_bounded_to_station_source_ids() {
    let query =
        overpass_lift_station_topology_query(&["node/20".to_string(), "way/10".to_string()]);

    assert!(query.contains("node(id:20)"));
    assert!(query.contains("way(id:10)"));
    assert!(query.contains("way(bn.station_nodes)[\"aerialway\"]"));
    assert!(query.contains(".station_sources"));
    assert!(!query.contains("-90,-180,90,180"));
}

#[test]
fn station_topology_fetch_rejects_runtime_error_before_cache_promotion() -> Result<()> {
    let cache = TempDir::new()?;
    write_json_pretty(
        &cache.path().join("spots.geojson"),
        &json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "properties": {
                    "id": "station-1",
                    "spotType": "lift_station",
                    "sources": [{"id": "node/20", "type": "openstreetmap"}]
                },
                "geometry": {"type": "Point", "coordinates": [10.0, 46.0]}
            }]
        }),
    )?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let server = std::thread::spawn(move || -> std::io::Result<()> {
        let (mut stream, _) = listener.accept()?;
        let mut request = [0_u8; 8192];
        let _ = stream.read(&mut request)?;
        let body = r#"{"remark":"runtime error: Query timed out","elements":[]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )?;
        stream.flush()?;
        Ok(())
    });

    let client = Client::builder().build()?;
    let result =
        fetch_or_extract_lift_station_topology(cache.path(), &format!("http://{address}"), &client);
    let error = result.expect_err("runtime-error response must fail the fetch");
    assert!(
        error
            .chain()
            .any(|cause| cause.to_string().contains("remark")),
        "unexpected fetch error: {error:?}"
    );
    assert!(!cache.path().join(LIFT_STATION_TOPOLOGY_FILE).exists());
    server.join().expect("Overpass fixture server thread")?;
    Ok(())
}

#[test]
fn station_topology_conversion_preserves_way_and_node_contacts() -> Result<()> {
    let station_sources = vec!["way/10".to_string(), "node/20".to_string()];
    let (topologies, summary) = overpass_json_to_lift_station_topology(
        &json!({
            "elements": [
                {
                    "type": "way",
                    "id": 10,
                    "nodes": [100, 101],
                    "geometry": [
                        {"lon": 10.0, "lat": 46.0},
                        {"lon": 10.01, "lat": 46.01}
                    ],
                    "tags": {"aerialway": "station"}
                },
                {
                    "type": "node",
                    "id": 20,
                    "lon": 10.2,
                    "lat": 46.2,
                    "tags": {"aerialway": "station"}
                },
                {
                    "type": "way",
                    "id": 200,
                    "nodes": [101, 102],
                    "geometry": [
                        {"lon": 10.01, "lat": 46.01},
                        {"lon": 10.1, "lat": 46.1}
                    ],
                    "tags": {"aerialway": "chair_lift"}
                },
                {
                    "type": "way",
                    "id": 201,
                    "nodes": [20, 202],
                    "geometry": [
                        {"lon": 10.2, "lat": 46.2},
                        {"lon": 10.3, "lat": 46.3}
                    ],
                    "tags": {"aerialway": "gondola"}
                },
                {
                    "type": "way",
                    "id": 300,
                    "nodes": [101, 103],
                    "geometry": [
                        {"lon": 10.01, "lat": 46.01},
                        {"lon": 10.4, "lat": 46.4}
                    ],
                    "tags": {"aerialway": "station"}
                }
            ]
        }),
        &station_sources,
    )?;

    assert_eq!(summary.station_count, 2);
    assert_eq!(summary.membership_count, 2);
    assert_eq!(topologies[0].station_source, "node/20");
    assert_eq!(topologies[0].members[0].lift_source, "way/201");
    assert_eq!(topologies[0].members[0].contact_node, "node/20");
    assert_eq!(
        topologies[0].members[0].contact_kind.as_deref(),
        Some("station-node")
    );
    assert_eq!(topologies[0].members[0].coordinate, [10.2, 46.2]);
    assert_eq!(topologies[1].station_source, "way/10");
    assert_eq!(topologies[1].members[0].lift_source, "way/200");
    assert_eq!(topologies[1].members[0].contact_node, "node/101");
    assert_eq!(
        topologies[1].members[0].contact_kind.as_deref(),
        Some("station-way-boundary-node")
    );
    Ok(())
}

#[test]
fn plan_du_fou_station_way_maps_to_both_actual_lifts() -> Result<()> {
    let station_sources = vec!["way/1455909804".to_string()];
    let (topologies, summary) = overpass_json_to_lift_station_topology(
        &json!({
            "elements": [
                {
                    "type": "way",
                    "id": 1455909804,
                    "nodes": [
                        13352507308u64, 13352507314u64, 13352507309u64, 13352507316u64,
                        13352507310u64, 13352507311u64, 13352507315u64, 13352507312u64,
                        13352507313u64, 13352507308u64
                    ],
                    "geometry": [
                        {"lon": 7.2946226, "lat": 46.1400933},
                        {"lon": 7.294607, "lat": 46.1400573},
                        {"lon": 7.2945925, "lat": 46.1400241},
                        {"lon": 7.2942686, "lat": 46.1400916},
                        {"lon": 7.2942496, "lat": 46.1400956},
                        {"lon": 7.2939667, "lat": 46.140264},
                        {"lon": 7.2940008, "lat": 46.1402915},
                        {"lon": 7.2940324, "lat": 46.140317},
                        {"lon": 7.2942924, "lat": 46.1401621},
                        {"lon": 7.2946226, "lat": 46.1400933}
                    ],
                    "tags": {"aerialway": "station", "name": "Plan-du-Fou"}
                },
                {
                    "type": "way",
                    "id": 249809635,
                    "nodes": [9097312385u64, 13352507314u64],
                    "geometry": [
                        {"lon": 7.3157572, "lat": 46.1353029},
                        {"lon": 7.294607, "lat": 46.1400573}
                    ],
                    "tags": {"aerialway": "gondola", "name": "Siviez"}
                },
                {
                    "type": "way",
                    "id": 761801233,
                    "nodes": [13352507317u64, 13352507315u64],
                    "geometry": [
                        {"lon": 7.2793111, "lat": 46.1487755},
                        {"lon": 7.2940008, "lat": 46.1402915}
                    ],
                    "tags": {"aerialway": "gondola", "name": "Prarion-Plan-du-Fou"}
                }
            ]
        }),
        &station_sources,
    )?;

    assert_eq!(summary.station_count, 1);
    assert_eq!(summary.membership_count, 2);
    assert_eq!(topologies[0].station_source, "way/1455909804");
    assert_eq!(
        topologies[0]
            .members
            .iter()
            .map(|member| member.lift_source.as_str())
            .collect::<Vec<_>>(),
        vec!["way/249809635", "way/761801233"]
    );
    assert_eq!(topologies[0].members[0].contact_node, "node/13352507314");
    assert_eq!(topologies[0].members[1].contact_node, "node/13352507315");
    Ok(())
}

#[test]
fn winteregg_station_topology_survives_cross_pack_release_layout() -> Result<()> {
    let murren_id = "d9ac8983541d7960d801802ce64751267338b5e4";
    let grindelwald_id = "17b19c745f7d69545421bdb985f0b981abfc70ae";
    let ski_areas = vec![
        source_feature(
            json!({
                "id": murren_id,
                "name": "Mürren/Schilthorn",
                "status": "operating",
                "activities": ["downhill"]
            }),
            json!({"type": "Point", "coordinates": [7.8820089, 46.5676365]}),
        ),
        source_feature(
            json!({
                "id": grindelwald_id,
                "name": "Grindelwald - Männlichen (Schlittelpiste)",
                "status": "operating",
                "activities": ["downhill"]
            }),
            json!({"type": "Point", "coordinates": [7.9664529, 46.6171299]}),
        ),
    ];
    let lifts = vec![
        source_feature(
            json!({
                "id": "01c94a1646c80ab59a94035582af3e2b619ba6ad",
                "name": "Winteregg",
                "skiAreas": [murren_id],
                "sources": [{"id": "way/150270143", "type": "openstreetmap"}]
            }),
            json!({
                "type": "LineString",
                "coordinates": [[7.8957272, 46.5815, 1582.4], [7.8877671, 46.5707924, 1939.0]]
            }),
        ),
        source_feature(
            json!({
                "id": "grindelwald-support-lift",
                "name": "Grindelwald support lift",
                "skiAreas": [grindelwald_id],
                "sources": [{"id": "way/208370459", "type": "openstreetmap"}]
            }),
            json!({
                "type": "LineString",
                "coordinates": [[7.96, 46.61], [7.97, 46.62]]
            }),
        ),
    ];
    let spots = vec![source_feature(
        json!({
            "id": "85a01cfe97437f6e15935756a3882d3601cfb7b5",
            "name": "Winteregg (Talst. Maulerhubel)",
            "spotType": "lift_station",
            "position": "bottom",
            "skiAreas": [grindelwald_id],
            "sources": [{"id": "node/1631925115", "type": "openstreetmap"}]
        }),
        json!({"type": "Point", "coordinates": [7.8957272, 46.5815, 1582.4]}),
    )];
    let mut dataset = normalize_sources_with_topology(
        ski_areas,
        Vec::new(),
        lifts,
        spots,
        Vec::new(),
        vec![LiftStationTopology {
            station_source: "node/1631925115".to_string(),
            members: vec![LiftStationTopologyMember {
                lift_source: "way/150270143".to_string(),
                contact_node: "node/1631925115".to_string(),
                coordinate: [7.8957272, 46.5815],
                contact_kind: Some("station-node".to_string()),
            }],
        }],
        "2026-09-10",
        Utc::now(),
    )?;

    let station = dataset
        .spots
        .iter()
        .find(|spot| spot.id == "85a01cfe97437f6e15935756a3882d3601cfb7b5")
        .expect("Winteregg station");
    let lift = dataset
        .lifts
        .iter()
        .find(|lift| lift.id == "01c94a1646c80ab59a94035582af3e2b619ba6ad")
        .expect("Winteregg lift");
    assert_eq!(station.resort_ids, vec![grindelwald_id, murren_id]);
    assert_eq!(lift.resort_ids, vec![grindelwald_id, murren_id]);

    for resort in &mut dataset.resorts {
        resort.center = if resort.id == murren_id {
            [10.0, 46.0]
        } else {
            [14.0, 46.0]
        };
    }
    let plans = plan_source_packs(&dataset)?;
    assert_eq!(plans.len(), 2);

    let output = TempDir::new()?;
    write_source_outputs(output.path(), &dataset)?;
    validate_output(output.path())?;
    let pack_count = fs::read_dir(output.path())?
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with("pack-") && name.ends_with(".sqlite.gz")
        })
        .count();
    assert_eq!(pack_count, 2);
    Ok(())
}

#[test]
fn station_topology_normalizes_many_to_many_and_persists_transfer_annotation() -> Result<()> {
    let ski_areas = vec![source_feature(
        json!({
            "id": "area-1",
            "name": "Demo",
            "status": "operating",
            "activities": ["downhill"]
        }),
        json!({"type": "Point", "coordinates": [10.0, 46.0]}),
    )];
    let lifts = vec![
        source_feature(
            json!({
                "id": "lift-a",
                "skiAreas": ["area-1"],
                "sources": [{"id": "way/200", "type": "openstreetmap"}]
            }),
            json!({"type": "LineString", "coordinates": [[10.01, 46.01], [10.1, 46.1]]}),
        ),
        source_feature(
            json!({
                "id": "lift-b",
                "skiAreas": ["area-1"],
                "sources": [{"id": "way/201", "type": "openstreetmap"}]
            }),
            json!({"type": "LineString", "coordinates": [[10.01, 46.01], [10.2, 46.2]]}),
        ),
    ];
    let spots = vec![source_feature(
        json!({
            "id": "station-1",
            "spotType": "lift_station",
            "position": "top",
            "skiAreas": ["area-1"],
            "sources": [{"id": "way/10", "type": "openstreetmap"}]
        }),
        json!({"type": "Point", "coordinates": [10.01, 46.01, 2400.0]}),
    )];
    let dataset = normalize_sources_with_topology(
        ski_areas,
        Vec::new(),
        lifts,
        spots,
        Vec::new(),
        vec![LiftStationTopology {
            station_source: "way/10".to_string(),
            members: vec![
                LiftStationTopologyMember {
                    lift_source: "way/200".to_string(),
                    contact_node: "node/101".to_string(),
                    coordinate: [10.01, 46.01],
                    contact_kind: Some("station-way-boundary-node".to_string()),
                },
                LiftStationTopologyMember {
                    lift_source: "way/201".to_string(),
                    contact_node: "node/101".to_string(),
                    coordinate: [10.01, 46.01],
                    contact_kind: Some("station-way-boundary-node".to_string()),
                },
            ],
        }],
        "2026-09-11",
        Utc::now(),
    )?;

    assert_eq!(dataset.lift_station_memberships.len(), 2);
    let station = dataset
        .spots
        .iter()
        .find(|spot| spot.id == "station-1")
        .expect("normalized station");
    assert_eq!(
        station.properties.get("isTransferStation"),
        Some(&json!(true))
    );
    assert_eq!(station.properties.get("is_transfer"), Some(&json!(true)));
    assert_eq!(
        station.properties.get("connectedLiftCount"),
        Some(&json!(2))
    );

    let output = TempDir::new()?;
    write_source_outputs(output.path(), &dataset)?;
    validate_output(output.path())?;
    let pack_name = fs::read_dir(output.path())?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| name.starts_with("pack-") && name.ends_with(".sqlite.gz"))
        .expect("source pack");
    let pack_dir = unpack_gzip_asset(output.path(), &pack_name)?;
    let pack = Connection::open(pack_dir.path().join(pack_name.trim_end_matches(".gz")))?;
    assert_eq!(
        pack.query_row("SELECT COUNT(*) FROM lift_station_memberships", [], |row| {
            row.get::<_, i64>(0)
        })?,
        2
    );
    Ok(())
}

#[test]
fn connection_assignment_preserves_explicit_parent_and_child_ownership() {
    let domain_id = "domain-a".to_string();
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
    let mut warnings = Vec::new();
    let assigned = assign_connections_to_resorts(
        connections,
        &domain_and_leaf_resorts("domain-a", &leaf_id),
        &[],
        &[],
        &mut warnings,
    );

    assert_eq!(assigned.len(), 1);
    assert_eq!(assigned[0].resort_ids, vec![domain_id, leaf_id]);
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
    let assigned = assign_connections_to_resorts(
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
    let rejected = assign_connections_to_resorts(
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
    let assigned = assign_connections_to_resorts(
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
fn build_pipeline_writes_only_canonical_sqlite_output() -> Result<()> {
    let cache = TempDir::new()?;
    let dataset_dir = cache.path().join("2026-09-10");
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
                "geometry": {"type": "LineString", "coordinates": [[10.0, 46.0], [10.1, 46.0]]}
            }]
        }),
    )?;
    for filename in ["lifts.geojson", "connections.geojson", "spots.geojson"] {
        write_json_pretty(
            &dataset_dir.join(filename),
            &json!({"type": "FeatureCollection", "features": []}),
        )?;
    }

    let output = TempDir::new()?;
    let summary = build_from_cache(cache.path(), output.path(), Some("2026-09-10".to_string()))?;
    assert_eq!(summary.resort_count, 1);
    assert_canonical_output(output.path())?;
    validate_output(output.path())?;
    let latest: Value = read_json(&output.path().join("latest.json"))?;
    assert_eq!(
        latest.get("releaseTag").and_then(Value::as_str),
        Some("indexes-2026-09-10")
    );
    assert_eq!(
        latest
            .get("packPolicy")
            .and_then(|policy| policy.get("maxCompressedBytes"))
            .and_then(Value::as_u64),
        Some(8 * 1024 * 1024)
    );
    Ok(())
}

fn assert_canonical_output(output: &Path) -> Result<()> {
    assert!(output.join("latest.json").exists());
    assert!(output.join("catalog.sqlite.gz").exists());
    assert!(fs::read_dir(output)?.filter_map(Result::ok).any(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        name.starts_with("pack-") && name.ends_with(".sqlite.gz")
    }));
    for legacy in ["resorts.json", "packages", "groups", "release-packs", "v2"] {
        assert!(
            !output.join(legacy).exists(),
            "legacy output exists: {legacy}"
        );
    }
    Ok(())
}

fn unpack_gzip_asset(output: &Path, asset: &str) -> Result<TempDir> {
    let unpacked = TempDir::new()?;
    let input = File::open(output.join(asset))?;
    let mut decoder = GzDecoder::new(input);
    let destination = unpacked.path().join(asset.trim_end_matches(".gz"));
    let mut file = File::create(&destination)?;
    copy(&mut decoder, &mut file)?;
    Ok(unpacked)
}

fn domain_and_leaf_resorts(domain_id: &str, leaf_id: &str) -> Vec<ResortRecord> {
    vec![
        test_resort(domain_id, "Domain", "domain", None),
        test_resort(leaf_id, "Leaf", "resort", Some(domain_id)),
    ]
}

fn test_resort(id: &str, name: &str, _resort_type: &str, parent_id: Option<&str>) -> ResortRecord {
    ResortRecord {
        id: id.to_string(),
        name: name.to_string(),
        parent_id: parent_id.map(str::to_string),
        bbox: [10.0, 46.0, 10.02, 46.02],
        area_km2: 1.0,
        country: Some("IT".to_string()),
        iso_codes: vec!["IT-BZ".to_string()],
        center: [10.01, 46.01],
        run_convention: Some("europe".to_string()),
    }
}

fn source_feature(properties: Value, geometry: Value) -> SourceFeature {
    SourceFeature {
        id: None,
        properties: properties.as_object().cloned().unwrap_or_default(),
        geometry,
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
