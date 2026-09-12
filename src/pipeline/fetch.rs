use super::*;

pub(super) fn fetch_sources(
    cache_dir: &Path,
    dataset_version: Option<String>,
    source_base_url: &str,
    overpass_base_url: &str,
    skip_connection_enrichment: bool,
    skip_station_topology_enrichment: bool,
) -> Result<()> {
    let dataset_version =
        dataset_version.unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
    let dataset_dir = cache_dir.join(&dataset_version);
    fs::create_dir_all(&dataset_dir)
        .with_context(|| format!("creating source cache {}", dataset_dir.display()))?;

    let client = Client::builder()
        .timeout(Duration::from_secs(600))
        .user_agent("SkiNavIndexes/0.1 (OpenSkiMap GeoJSON cache)")
        .build()
        .context("building HTTP client")?;

    let mut layers = Vec::new();
    for layer in LAYER_FILES {
        let target = dataset_dir.join(layer);
        if target.exists() {
            layers.push(file_metadata(layer, &target, None)?);
            eprintln!("cached {layer}: {}", target.display());
            continue;
        }

        let url = format!("{}/{}", source_base_url.trim_end_matches('/'), layer);
        eprintln!("downloading once: {url}");
        let mut response = client
            .get(&url)
            .send()
            .with_context(|| format!("downloading {url}"))?;
        if !response.status().is_success() {
            bail!("download failed for {url}: HTTP {}", response.status());
        }

        let temp = target.with_extension("geojson.part");
        let mut out =
            File::create(&temp).with_context(|| format!("creating {}", temp.display()))?;
        response
            .copy_to(&mut out)
            .with_context(|| format!("writing {}", temp.display()))?;
        drop(out);
        validate_geojson_file(&temp).with_context(|| format!("validating {}", temp.display()))?;
        fs::rename(&temp, &target)
            .with_context(|| format!("moving {} to {}", temp.display(), target.display()))?;
        layers.push(file_metadata(layer, &target, Some(url))?);
    }

    let connection_source = if skip_connection_enrichment {
        json!({
            "name": CONNECTIONS_FILE,
            "status": "skipped",
            "reason": "skip_connection_enrichment"
        })
    } else {
        fetch_or_extract_connections(&dataset_dir, overpass_base_url, &client)?
    };
    let station_topology_source = if skip_station_topology_enrichment {
        json!({
            "name": LIFT_STATION_TOPOLOGY_FILE,
            "status": "skipped",
            "reason": "skip_station_topology_enrichment"
        })
    } else {
        fetch_or_extract_lift_station_topology(&dataset_dir, overpass_base_url, &client)?
    };

    let metadata = json!({
        "datasetVersion": dataset_version,
        "fetchedAt": Utc::now(),
        "sourceFormat": "openskimap-geojson",
        "layers": layers,
        "connectionEnrichment": connection_source,
        "liftStationTopologyEnrichment": station_topology_source,
    });
    write_json_pretty(&dataset_dir.join("source_metadata.json"), &metadata)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LiftStationTopologyConversionSummary {
    pub(super) station_count: usize,
    pub(super) membership_count: usize,
}

pub(super) fn fetch_or_extract_lift_station_topology(
    dataset_dir: &Path,
    overpass_base_url: &str,
    client: &Client,
) -> Result<Value> {
    let target = dataset_dir.join(LIFT_STATION_TOPOLOGY_FILE);
    if target.exists() {
        let topologies = read_lift_station_topology(&target)?;
        let summary = topology_summary(&topologies);
        return Ok(json!({
            "name": LIFT_STATION_TOPOLOGY_FILE,
            "status": "cached",
            "stationCount": summary.station_count,
            "featureCount": summary.membership_count,
            "metadata": file_metadata(LIFT_STATION_TOPOLOGY_FILE, &target, None)?
        }));
    }

    let station_sources = station_source_ids_from_spots(dataset_dir)?;
    let mut memberships: BTreeMap<(String, String), LiftStationTopologyMember> = BTreeMap::new();
    let mut query_hashes = Vec::new();
    for batch in station_sources.chunks(LIFT_STATION_TOPOLOGY_BATCH_SIZE) {
        let query = overpass_lift_station_topology_query(batch);
        query_hashes.push(sha256_text(query.as_str()));
        let url = format!("{}/interpreter", overpass_base_url.trim_end_matches('/'));
        eprintln!(
            "Querying bounded lift-station topology for {} station source IDs: {url}",
            batch.len()
        );
        let response = client
            .post(&url)
            .form(&[("data", query.as_str())])
            .send()
            .with_context(|| format!("querying Overpass station topology {url}"))?;
        if !response.status().is_success() {
            bail!(
                "Overpass station topology query failed: HTTP {}",
                response.status()
            );
        }
        let body = response
            .text()
            .context("reading Overpass station topology response body")?;
        let overpass =
            parse_overpass_json(&body).context("parsing Overpass station topology response")?;
        let (topologies, _) = overpass_json_to_lift_station_topology(&overpass, batch)?;
        for topology in topologies {
            for member in topology.members {
                let key = (topology.station_source.clone(), member.lift_source.clone());
                if let Some(existing) = memberships.get(&key) {
                    if existing != &member {
                        bail!(
                            "conflicting station topology contact for {} -> {}",
                            key.0,
                            key.1
                        );
                    }
                } else {
                    memberships.insert(key, member);
                }
            }
        }
    }

    let topologies = station_sources
        .into_iter()
        .map(|station_source| LiftStationTopology {
            station_source: station_source.clone(),
            members: memberships
                .iter()
                .filter(|((source, _), _)| source == &station_source)
                .map(|(_, member)| member.clone())
                .collect(),
        })
        .collect::<Vec<_>>();
    let summary = topology_summary(&topologies);
    let value = serde_json::to_value(&topologies)?;
    let temp = target.with_extension("json.part");
    write_json_pretty(&temp, &value)?;
    fs::rename(&temp, &target)
        .with_context(|| format!("moving {} to {}", temp.display(), target.display()))?;

    let url = format!("{}/interpreter", overpass_base_url.trim_end_matches('/'));
    Ok(json!({
        "name": LIFT_STATION_TOPOLOGY_FILE,
        "status": "overpass",
        "url": url,
        "querySha256": sha256_text(&query_hashes.join("\n")),
        "fetchedAt": Utc::now(),
        "stationCount": summary.station_count,
        "featureCount": summary.membership_count,
        "metadata": file_metadata(LIFT_STATION_TOPOLOGY_FILE, &target, Some(url))?
    }))
}

pub(super) fn station_source_ids_from_spots(dataset_dir: &Path) -> Result<Vec<String>> {
    let spots_path = dataset_dir.join("spots.geojson");
    let mut station_sources = BTreeSet::new();
    for feature in read_feature_collection(&spots_path)? {
        let is_station = first_string(&feature.properties, &["spotType", "spot_type"])
            .is_some_and(|spot_type| spot_type.eq_ignore_ascii_case("lift_station"));
        if !is_station {
            continue;
        }
        station_sources.extend(
            source_keys_from_properties(&feature.properties)
                .into_iter()
                .filter(|source| parse_osm_source(source).is_some()),
        );
    }
    Ok(station_sources.into_iter().collect())
}

pub(super) fn overpass_lift_station_topology_query(station_sources: &[String]) -> String {
    let mut node_ids = BTreeSet::new();
    let mut way_ids = BTreeSet::new();
    for source in station_sources {
        match parse_osm_source(source) {
            Some(("node", id)) => {
                node_ids.insert(id);
            }
            Some(("way", id)) => {
                way_ids.insert(id);
            }
            _ => {}
        }
    }

    let mut query = String::from("[out:json][timeout:900];\n(\n");
    if !node_ids.is_empty() {
        query.push_str("  node(id:");
        query.push_str(
            &node_ids
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(","),
        );
        query.push_str(");\n");
    }
    if !way_ids.is_empty() {
        query.push_str("  way(id:");
        query.push_str(
            &way_ids
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(","),
        );
        query.push_str(");\n");
    }
    query.push_str(")->.station_sources;\n");
    query.push_str("(\n  .station_sources;\n  >;\n)->.station_nodes;\n");
    query.push_str("(\n  .station_sources;\n  way(bn.station_nodes)[\"aerialway\"];\n);\n");
    query.push_str("out body geom;");
    query
}

pub(super) fn overpass_json_to_lift_station_topology(
    root: &Value,
    station_sources: &[String],
) -> Result<(
    Vec<LiftStationTopology>,
    LiftStationTopologyConversionSummary,
)> {
    let elements = root
        .get("elements")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Overpass response missing elements array"))?;

    let mut node_locations: HashMap<String, [f64; 2]> = HashMap::new();
    let mut way_nodes: HashMap<String, Vec<String>> = HashMap::new();
    let mut lift_ways: Vec<(String, Vec<String>)> = Vec::new();

    for element in elements {
        let Some(object) = element.as_object() else {
            continue;
        };
        let Some(source) = overpass_source_key(object) else {
            continue;
        };
        match object.get("type").and_then(Value::as_str) {
            Some("node") => {
                if let (Some(lon), Some(lat)) = (
                    object.get("lon").and_then(Value::as_f64),
                    object.get("lat").and_then(Value::as_f64),
                ) {
                    node_locations.insert(source, [lon, lat]);
                }
            }
            Some("way") => {
                let nodes = overpass_way_node_sources(object);
                if let Some(geometry) = object.get("geometry").and_then(Value::as_array) {
                    for (node_source, point) in nodes.iter().zip(geometry) {
                        if let (Some(lon), Some(lat)) = (
                            point.get("lon").and_then(Value::as_f64),
                            point.get("lat").and_then(Value::as_f64),
                        ) {
                            node_locations.insert(node_source.clone(), [lon, lat]);
                        }
                    }
                }
                way_nodes.insert(source.clone(), nodes.clone());
                let aerialway = object
                    .get("tags")
                    .and_then(Value::as_object)
                    .and_then(|tags| tags.get("aerialway"))
                    .and_then(Value::as_str)
                    .map(str::to_ascii_lowercase);
                if aerialway
                    .as_deref()
                    .is_some_and(|value| !matches!(value, "station" | "pylon"))
                {
                    lift_ways.push((source, nodes));
                }
            }
            _ => {}
        }
    }

    let mut topologies = Vec::with_capacity(station_sources.len());
    for station_source in station_sources {
        let Some((station_kind, _)) = parse_osm_source(station_source) else {
            continue;
        };
        let station_nodes = match station_kind {
            "node" => vec![station_source.clone()],
            "way" => way_nodes.get(station_source).cloned().unwrap_or_default(),
            _ => Vec::new(),
        };
        let station_node_set = station_nodes.iter().cloned().collect::<BTreeSet<_>>();
        let mut members = Vec::new();
        for (lift_source, lift_nodes) in &lift_ways {
            let Some(contact_node) = lift_nodes
                .iter()
                .find(|node_source| station_node_set.contains(*node_source))
            else {
                continue;
            };
            let Some(coordinate) = node_locations.get(contact_node).copied() else {
                continue;
            };
            members.push(LiftStationTopologyMember {
                lift_source: lift_source.clone(),
                contact_node: contact_node.clone(),
                coordinate,
                contact_kind: Some(if station_kind == "node" {
                    "station-node".to_string()
                } else {
                    "station-way-boundary-node".to_string()
                }),
            });
        }
        members.sort_by(|left, right| {
            left.lift_source
                .cmp(&right.lift_source)
                .then_with(|| left.contact_node.cmp(&right.contact_node))
        });
        members.dedup_by(|left, right| left.lift_source == right.lift_source);
        topologies.push(LiftStationTopology {
            station_source: station_source.clone(),
            members,
        });
    }

    topologies.sort_by(|left, right| left.station_source.cmp(&right.station_source));
    let summary = topology_summary(&topologies);
    Ok((topologies, summary))
}

fn read_lift_station_topology(path: &Path) -> Result<Vec<LiftStationTopology>> {
    let value = read_json(path)?;
    serde_json::from_value(value)
        .with_context(|| format!("parsing lift station topology {}", path.display()))
}

fn topology_summary(topologies: &[LiftStationTopology]) -> LiftStationTopologyConversionSummary {
    LiftStationTopologyConversionSummary {
        station_count: topologies.len(),
        membership_count: topologies
            .iter()
            .map(|topology| topology.members.len())
            .sum(),
    }
}

fn parse_osm_source(source: &str) -> Option<(&str, u64)> {
    let (kind, id) = source.split_once('/')?;
    if !matches!(kind, "node" | "way") {
        return None;
    }
    Some((kind, id.parse().ok()?))
}

fn overpass_source_key(object: &Map<String, Value>) -> Option<String> {
    let kind = object.get("type").and_then(Value::as_str)?;
    let id = object.get("id").and_then(value_to_string)?;
    Some(format!("{kind}/{id}"))
}

fn overpass_way_node_sources(object: &Map<String, Value>) -> Vec<String> {
    object
        .get("nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(value_to_string)
        .map(|id| format!("node/{id}"))
        .collect()
}
pub(super) fn fetch_or_extract_connections(
    dataset_dir: &Path,
    overpass_base_url: &str,
    client: &Client,
) -> Result<Value> {
    let target = dataset_dir.join(CONNECTIONS_FILE);
    if target.exists() {
        return file_metadata(CONNECTIONS_FILE, &target, None).map(|metadata| {
            json!({
                "name": CONNECTIONS_FILE,
                "status": "cached",
                "metadata": metadata
            })
        });
    }

    if openskimap_has_connections(dataset_dir)? {
        let count = write_connections_from_openskimap(dataset_dir, &target)?;
        let metadata = file_metadata(CONNECTIONS_FILE, &target, None)?;
        return Ok(json!({
            "name": CONNECTIONS_FILE,
            "status": "openskimap",
            "featureCount": count,
            "metadata": metadata
        }));
    }

    let query = overpass_connection_query();
    let url = format!("{}/interpreter", overpass_base_url.trim_end_matches('/'));
    eprintln!("OpenSkiMap has no type=connection features; querying Overpass: {url}");
    let response = client
        .post(&url)
        .form(&[("data", query.as_str())])
        .send()
        .with_context(|| format!("querying Overpass {url}"))?;
    if !response.status().is_success() {
        bail!(
            "Overpass connection query failed: HTTP {}",
            response.status()
        );
    }
    let body = response.text().context("reading Overpass response body")?;
    let overpass = parse_overpass_json(&body).context("parsing Overpass response")?;
    let (connections, summary) = overpass_json_to_connection_geojson(&overpass)?;
    let temp = target.with_extension("geojson.part");
    write_json_pretty(&temp, &connections)?;
    validate_geojson_file(&temp)?;
    fs::rename(&temp, &target)
        .with_context(|| format!("moving {} to {}", temp.display(), target.display()))?;
    Ok(json!({
        "name": CONNECTIONS_FILE,
        "status": "overpass",
        "url": url,
        "querySha256": sha256_text(query.as_str()),
        "fetchedAt": Utc::now(),
        "featureCount": summary.feature_count,
        "ignoredElementCount": summary.ignored_count,
        "metadata": file_metadata(CONNECTIONS_FILE, &target, Some(url))?
    }))
}

pub(super) fn parse_overpass_json(body: &str) -> Result<Value> {
    let overpass: Value = serde_json::from_str(body).context("decoding Overpass JSON")?;
    if let Some(remark) = overpass.get("remark") {
        let remark = remark
            .as_str()
            .map(str::trim)
            .filter(|remark| !remark.is_empty())
            .unwrap_or("non-string or empty remark");
        bail!("Overpass response contains a remark: {remark}");
    }
    Ok(overpass)
}

pub(super) fn openskimap_has_connections(dataset_dir: &Path) -> Result<bool> {
    for layer in LAYER_FILES {
        let path = dataset_dir.join(layer);
        if !path.exists() {
            continue;
        }
        let features = read_feature_collection(&path)?;
        if features
            .iter()
            .any(|feature| is_openskimap_connection(&feature.properties))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn write_connections_from_openskimap(
    dataset_dir: &Path,
    target: &Path,
) -> Result<usize> {
    let mut features = Vec::new();
    for layer in LAYER_FILES {
        let path = dataset_dir.join(layer);
        if !path.exists() {
            continue;
        }
        for (index, feature) in read_feature_collection(&path)?.into_iter().enumerate() {
            if !is_openskimap_connection(&feature.properties) {
                continue;
            }
            let id = feature.source_id("connection", index);
            let mut props = feature.properties;
            props
                .entry("id".to_string())
                .or_insert_with(|| Value::String(id.clone()));
            props.insert("sourceLayer".to_string(), Value::String(layer.to_string()));
            features.push(json!({
                "type": "Feature",
                "id": id,
                "properties": props,
                "geometry": feature.geometry
            }));
        }
    }
    let count = features.len();
    write_json_pretty(
        target,
        &json!({"type": "FeatureCollection", "features": features}),
    )?;
    validate_geojson_file(target)?;
    Ok(count)
}

pub(super) fn is_openskimap_connection(props: &Map<String, Value>) -> bool {
    first_string(props, &["type"]).is_some_and(|value| value.eq_ignore_ascii_case("connection"))
}

pub(super) fn overpass_connection_query() -> String {
    r#"[out:json][timeout:900];
(
  way["piste:type"="connection"](-90,-180,90,180);
  relation["piste:type"="connection"](-90,-180,90,180);
);
out body geom;"#
        .to_string()
}

pub(super) fn overpass_json_to_connection_geojson(
    root: &Value,
) -> Result<(Value, ConnectionConversionSummary)> {
    let elements = root
        .get("elements")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Overpass response missing elements array"))?;
    let mut features = Vec::new();
    let mut ignored_count = 0;
    for element in elements {
        match overpass_element_to_connection_feature(element) {
            Some(feature) => features.push(feature),
            None => ignored_count += 1,
        }
    }
    let summary = ConnectionConversionSummary {
        feature_count: features.len(),
        ignored_count,
    };
    Ok((
        json!({"type": "FeatureCollection", "features": features}),
        summary,
    ))
}

pub(super) fn overpass_element_to_connection_feature(element: &Value) -> Option<Value> {
    let object = element.as_object()?;
    let element_type = object.get("type").and_then(Value::as_str)?;
    let id_value = object.get("id")?;
    let osm_id = value_to_string(id_value)?;
    let feature_id = format!("{element_type}/{osm_id}");
    let mut props = object
        .get("tags")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if !value_contains_string(props.get("piste:type")?, "connection") {
        return None;
    }
    props.insert("id".to_string(), Value::String(feature_id.clone()));
    props.insert("type".to_string(), Value::String("connection".to_string()));
    props.insert(
        "osm_type".to_string(),
        Value::String(element_type.to_string()),
    );
    props.insert("osm_id".to_string(), Value::String(osm_id));
    props.insert(
        "sources".to_string(),
        json!([{"id": feature_id, "type": "openstreetmap"}]),
    );

    let geometry = match element_type {
        "way" => {
            let coordinates = overpass_geometry_coordinates(object.get("geometry")?)?;
            if coordinates.len() < 2 {
                return None;
            }
            json!({"type": "LineString", "coordinates": coordinates})
        }
        "relation" => {
            let mut lines = Vec::new();
            for member in object
                .get("members")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(coordinates) = overpass_geometry_coordinates(member.get("geometry")?) {
                    if coordinates.len() >= 2 {
                        lines.push(Value::Array(coordinates));
                    }
                }
            }
            if lines.is_empty() {
                return None;
            }
            json!({"type": "MultiLineString", "coordinates": lines})
        }
        _ => return None,
    };

    Some(json!({
        "type": "Feature",
        "id": feature_id,
        "properties": props,
        "geometry": geometry
    }))
}

pub(super) fn overpass_geometry_coordinates(geometry: &Value) -> Option<Vec<Value>> {
    let points = geometry.as_array()?;
    let mut coordinates = Vec::new();
    for point in points {
        let lon = point.get("lon").and_then(Value::as_f64)?;
        let lat = point.get("lat").and_then(Value::as_f64)?;
        coordinates.push(json!([lon, lat]));
    }
    Some(coordinates)
}
