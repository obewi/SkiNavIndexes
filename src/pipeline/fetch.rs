use super::*;

pub(super) fn fetch_sources(
    cache_dir: &Path,
    dataset_version: Option<String>,
    source_base_url: &str,
    overpass_base_url: &str,
    skip_connection_enrichment: bool,
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

    let metadata = json!({
        "datasetVersion": dataset_version,
        "fetchedAt": Utc::now(),
        "sourceFormat": "openskimap-geojson",
        "layers": layers,
        "connectionEnrichment": connection_source,
    });
    write_json_pretty(&dataset_dir.join("source_metadata.json"), &metadata)?;
    Ok(())
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
    let overpass: Value = serde_json::from_str(&body).context("parsing Overpass response")?;
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
