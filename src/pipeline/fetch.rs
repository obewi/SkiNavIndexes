use super::*;

pub(super) const OVERPASS_CACHE_DIRECTORY: &str = ".overpass";
pub(super) const OVERPASS_STATION_CACHE_FILE: &str = "lift_station_topology_cache.json";
pub(super) const OVERPASS_CONNECTION_CACHE_FILE: &str = "connections_cache.json";
const OVERPASS_CACHE_SCHEMA_VERSION: u8 = 1;
#[cfg(test)]
const OVERPASS_CACHE_STALE_AFTER_DAYS: i64 = 120;
const OVERPASS_DEFAULT_ENDPOINT: &str = "https://overpass-api.de/api/interpreter";
#[cfg(test)]
const OVERPASS_FALLBACK_ENDPOINTS: [&str; 3] = [
    "https://maps.mail.ru/osm/tools/overpass/api/interpreter",
    "https://overpass.private.coffee/api/interpreter",
    "https://overpass.osm.jp/api/interpreter",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedStationTopology {
    fetched_at: i64,
    members: Vec<LiftStationTopologyMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistentStationTopologyCache {
    schema_version: u8,
    stations: BTreeMap<String, CachedStationTopology>,
}

impl Default for PersistentStationTopologyCache {
    fn default() -> Self {
        Self {
            schema_version: OVERPASS_CACHE_SCHEMA_VERSION,
            stations: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub(super) struct OverpassResponse {
    pub(super) endpoint: String,
    pub(super) value: Value,
}

#[derive(Debug, Default)]
pub(super) struct OverpassPacer {
    last_request: Option<std::time::Instant>,
}

impl OverpassPacer {
    fn wait_with_interval(&mut self, interval: Duration, sleep: &mut dyn FnMut(Duration)) {
        if let Some(last_request) = self.last_request {
            let elapsed = last_request.elapsed();
            if elapsed < interval {
                sleep(interval - elapsed);
            }
        }
        self.last_request = Some(std::time::Instant::now());
    }
}

pub(super) fn overpass_cache_dir(dataset_dir: &Path) -> PathBuf {
    dataset_dir
        .parent()
        .unwrap_or(dataset_dir)
        .join(OVERPASS_CACHE_DIRECTORY)
}

pub(super) fn overpass_station_cache_path(dataset_dir: &Path) -> PathBuf {
    overpass_cache_dir(dataset_dir).join(OVERPASS_STATION_CACHE_FILE)
}

pub(super) fn overpass_connection_cache_path(dataset_dir: &Path) -> PathBuf {
    overpass_cache_dir(dataset_dir).join(OVERPASS_CONNECTION_CACHE_FILE)
}

pub(super) fn overpass_interpreter_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return OVERPASS_DEFAULT_ENDPOINT.to_string();
    }
    if trimmed.ends_with("/interpreter") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/interpreter")
    }
}

#[cfg(test)]
pub(super) fn overpass_endpoints(preferred_base_url: &str) -> Vec<String> {
    let preferred = overpass_interpreter_url(preferred_base_url);
    let public_endpoints = std::iter::once(OVERPASS_DEFAULT_ENDPOINT)
        .chain(OVERPASS_FALLBACK_ENDPOINTS)
        .collect::<Vec<_>>();

    if public_endpoints
        .iter()
        .any(|endpoint| *endpoint == preferred)
    {
        let mut endpoints = vec![preferred.clone()];
        endpoints.extend(
            public_endpoints
                .into_iter()
                .filter(|endpoint| *endpoint != preferred)
                .map(str::to_string),
        );
        endpoints
    } else {
        vec![preferred]
    }
}

fn overpass_endpoints_with_config(
    config: &OverpassConfig,
    preferred_base_url: Option<&str>,
) -> Vec<String> {
    let mut endpoints = Vec::new();
    for endpoint in &config.endpoints {
        let endpoint = overpass_interpreter_url(endpoint);
        if !endpoints.contains(&endpoint) {
            endpoints.push(endpoint);
        }
    }

    if let Some(preferred_base_url) = preferred_base_url {
        let preferred = overpass_interpreter_url(preferred_base_url);
        endpoints.retain(|endpoint| endpoint != &preferred);
        endpoints.insert(0, preferred);
    }

    endpoints
}

#[cfg(test)]
pub(super) fn overpass_request_with_fallback(
    client: &Client,
    preferred_base_url: &str,
    query: &str,
    operation: &str,
    pacer: &mut OverpassPacer,
    sleep: &mut dyn FnMut(Duration),
) -> Result<OverpassResponse> {
    let mut config = OverpassConfig::default();
    config.endpoints = overpass_endpoints(preferred_base_url);
    overpass_request_with_fallback_with_config(
        client, &config, None, query, operation, pacer, sleep,
    )
}

pub(super) fn overpass_request_with_fallback_with_config(
    client: &Client,
    config: &OverpassConfig,
    preferred_base_url: Option<&str>,
    query: &str,
    operation: &str,
    pacer: &mut OverpassPacer,
    sleep: &mut dyn FnMut(Duration),
) -> Result<OverpassResponse> {
    let mut errors = Vec::new();
    let endpoints = overpass_endpoints_with_config(config, preferred_base_url);
    if endpoints.is_empty() {
        bail!("Overpass endpoint configuration is empty");
    }

    for round in 0..config.max_retry_rounds {
        let mut transient_failure = false;
        let mut rate_limit_delay = None;

        for endpoint in endpoints.iter().cloned() {
            pacer.wait_with_interval(config.request_interval(), sleep);
            eprintln!("Querying Overpass {operation}: {endpoint}");
            let response = match client.post(&endpoint).form(&[("data", query)]).send() {
                Ok(response) => response,
                Err(error) => {
                    transient_failure = true;
                    errors.push(format!("{endpoint}: {error}"));
                    continue;
                }
            };

            let status = response.status();
            if status.is_success() {
                let body = match response.text() {
                    Ok(body) => body,
                    Err(error) => {
                        transient_failure = true;
                        errors.push(format!("{endpoint}: reading response body: {error}"));
                        continue;
                    }
                };
                match parse_overpass_json(&body) {
                    Ok(value) if value.get("elements").is_some_and(Value::is_array) => {
                        return Ok(OverpassResponse { endpoint, value });
                    }
                    Ok(_) => errors.push(format!("{endpoint}: response missing elements array")),
                    Err(error) => errors.push(format!("{endpoint}: {error}")),
                }
                continue;
            }

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                transient_failure = true;
                let delay = overpass_retry_after(&response).unwrap_or(config.rate_limit_delay());
                rate_limit_delay =
                    Some(rate_limit_delay.map_or(delay, |current: Duration| current.max(delay)));
                errors.push(format!("{endpoint}: HTTP {status}"));
                continue;
            }

            if status.is_client_error() {
                bail!("Overpass {operation} failed at {endpoint}: HTTP {status}");
            }

            if status.is_server_error()
                || status == reqwest::StatusCode::REQUEST_TIMEOUT
                || status == reqwest::StatusCode::TOO_EARLY
            {
                transient_failure = true;
            }
            errors.push(format!("{endpoint}: HTTP {status}"));
        }

        if !transient_failure || round + 1 == config.max_retry_rounds {
            break;
        }

        let delay = rate_limit_delay.unwrap_or(config.retry_delay());
        eprintln!(
            "All Overpass endpoints failed for {operation}; retrying after {} seconds",
            delay.as_secs()
        );
        sleep(delay);
    }

    bail!(
        "Overpass {operation} failed across configured endpoints: {}",
        errors.join("; ")
    )
}

fn overpass_retry_after(response: &reqwest::blocking::Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
pub(super) fn overpass_cache_entry_is_fresh(fetched_at: i64, now: i64) -> bool {
    overpass_cache_entry_is_fresh_after_days(fetched_at, now, OVERPASS_CACHE_STALE_AFTER_DAYS)
}

fn overpass_cache_entry_is_fresh_after_days(
    fetched_at: i64,
    now: i64,
    stale_after_days: i64,
) -> bool {
    now.saturating_sub(fetched_at) < stale_after_days.max(0).saturating_mul(24 * 60 * 60)
}

fn read_station_topology_cache(path: &Path) -> Result<PersistentStationTopologyCache> {
    let value = read_json(path)?;
    let cache: PersistentStationTopologyCache =
        serde_json::from_value(value).with_context(|| {
            format!(
                "parsing persistent station topology cache {}",
                path.display()
            )
        })?;
    if cache.schema_version != OVERPASS_CACHE_SCHEMA_VERSION {
        bail!(
            "unsupported persistent station topology cache version {} in {}",
            cache.schema_version,
            path.display()
        );
    }
    Ok(cache)
}

fn write_station_topology_cache(path: &Path, cache: &PersistentStationTopologyCache) -> Result<()> {
    write_json_atomically(path, &serde_json::to_value(cache)?)
}

fn read_connection_cache(path: &Path) -> Result<(i64, Value)> {
    let value = read_json(path)?;
    let schema_version = value
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("connection cache missing schemaVersion"))?;
    if schema_version != u64::from(OVERPASS_CACHE_SCHEMA_VERSION) {
        bail!("unsupported connection cache version {schema_version}");
    }
    let fetched_at = value
        .get("fetchedAt")
        .cloned()
        .ok_or_else(|| anyhow!("connection cache missing fetchedAt"))?
        .as_i64()
        .ok_or_else(|| anyhow!("connection cache fetchedAt is not an integer"))?;
    let data = value
        .get("data")
        .cloned()
        .ok_or_else(|| anyhow!("connection cache missing data"))?;
    validate_geojson_value(&data)?;
    Ok((fetched_at, data))
}

fn write_connection_cache(path: &Path, fetched_at: i64, data: &Value) -> Result<()> {
    write_json_atomically(
        path,
        &json!({
            "schemaVersion": OVERPASS_CACHE_SCHEMA_VERSION,
            "fetchedAt": fetched_at,
            "data": data,
        }),
    )
}

pub(super) fn write_json_atomically(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating cache directory {}", parent.display()))?;
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!("{extension}.part"))
        .unwrap_or_else(|| "part".to_string());
    let temp = path.with_extension(extension);
    write_json_pretty(&temp, value)?;
    fs::rename(&temp, path)
        .with_context(|| format!("moving {} to {}", temp.display(), path.display()))?;
    Ok(())
}

fn validate_geojson_value(value: &Value) -> Result<()> {
    if value.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
        bail!("expected GeoJSON FeatureCollection");
    }
    if !value.get("features").is_some_and(Value::is_array) {
        bail!("expected features array");
    }
    Ok(())
}

fn file_modified_at_with_stale_days(path: &Path, stale_after_days: i64) -> i64 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_else(|| {
            Utc::now().timestamp()
                - stale_after_days
                    .max(0)
                    .saturating_add(1)
                    .saturating_mul(24 * 60 * 60)
        })
}

pub(super) fn fetch_sources_with_config(
    cache_dir: &Path,
    dataset_version: Option<String>,
    source_base_url: &str,
    overpass_base_url: Option<&str>,
    overpass_config: &OverpassConfig,
    skip_connection_enrichment: bool,
    skip_station_topology_enrichment: bool,
) -> Result<()> {
    overpass_config.validate()?;
    let dataset_version =
        dataset_version.unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
    let dataset_dir = cache_dir.join(&dataset_version);
    fs::create_dir_all(&dataset_dir)
        .with_context(|| format!("creating source cache {}", dataset_dir.display()))?;

    let client = Client::builder()
        .timeout(overpass_config.request_timeout())
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
        fetch_or_extract_connections_with_config(
            &dataset_dir,
            overpass_base_url,
            overpass_config,
            &client,
        )?
    };
    let station_topology_source = if skip_station_topology_enrichment {
        json!({
            "name": LIFT_STATION_TOPOLOGY_FILE,
            "status": "skipped",
            "reason": "skip_station_topology_enrichment"
        })
    } else {
        fetch_or_extract_lift_station_topology_with_config(
            &dataset_dir,
            overpass_base_url,
            overpass_config,
            &client,
        )?
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

#[cfg(test)]
pub(super) fn fetch_or_extract_lift_station_topology(
    dataset_dir: &Path,
    overpass_base_url: &str,
    client: &Client,
) -> Result<Value> {
    let mut config = OverpassConfig::default();
    config.endpoints = overpass_endpoints(overpass_base_url);
    fetch_or_extract_lift_station_topology_with_config(
        dataset_dir,
        Some(overpass_base_url),
        &config,
        client,
    )
}

pub(super) fn fetch_or_extract_lift_station_topology_with_config(
    dataset_dir: &Path,
    overpass_base_url: Option<&str>,
    overpass_config: &OverpassConfig,
    client: &Client,
) -> Result<Value> {
    let target = dataset_dir.join(LIFT_STATION_TOPOLOGY_FILE);
    let station_sources = station_source_ids_from_spots(dataset_dir)?;
    let cache_path = overpass_station_cache_path(dataset_dir);
    let mut cache = if cache_path.exists() {
        match read_station_topology_cache(&cache_path) {
            Ok(cache) => cache,
            Err(error) => {
                eprintln!(
                    "Ignoring invalid persistent station topology cache {}: {error:#}",
                    cache_path.display()
                );
                PersistentStationTopologyCache::default()
            }
        }
    } else {
        PersistentStationTopologyCache::default()
    };

    if target.exists() {
        let fetched_at =
            file_modified_at_with_stale_days(&target, overpass_config.cache_stale_after_days);
        for topology in read_lift_station_topology(&target)? {
            let should_replace = cache
                .stations
                .get(&topology.station_source)
                .is_none_or(|entry| entry.fetched_at < fetched_at);
            if should_replace {
                cache.stations.insert(
                    topology.station_source,
                    CachedStationTopology {
                        fetched_at,
                        members: topology.members,
                    },
                );
            }
        }
        write_station_topology_cache(&cache_path, &cache)?;
    }

    let now = Utc::now().timestamp();
    let missing_station_count = station_sources
        .iter()
        .filter(|source| !cache.stations.contains_key(*source))
        .count();
    let stale_station_count = station_sources
        .iter()
        .filter(|source| {
            cache.stations.get(*source).is_some_and(|entry| {
                !overpass_cache_entry_is_fresh_after_days(
                    entry.fetched_at,
                    now,
                    overpass_config.cache_stale_after_days,
                )
            })
        })
        .count();
    let fresh_station_count = station_sources.len() - missing_station_count - stale_station_count;
    let refresh_sources = station_sources
        .iter()
        .filter(|source| {
            cache.stations.get(*source).is_none_or(|entry| {
                !overpass_cache_entry_is_fresh_after_days(
                    entry.fetched_at,
                    now,
                    overpass_config.cache_stale_after_days,
                )
            })
        })
        .cloned()
        .collect::<Vec<_>>();

    let mut pacer = OverpassPacer::default();
    let mut sleep = |duration| std::thread::sleep(duration);
    let mut preferred_endpoint = overpass_base_url.map(str::to_string);
    let mut query_hashes = Vec::new();
    let mut query_count = 0;
    let mut refreshed_station_count = 0;
    let mut stale_fallback_station_count = 0;
    let mut refresh_blocked = false;
    for batch in refresh_sources.chunks(overpass_config.station_batch_size) {
        if refresh_blocked {
            if batch
                .iter()
                .any(|source| !cache.stations.contains_key(source))
            {
                bail!(
                    "Overpass station topology refresh was unavailable and {} station(s) have no cached result",
                    batch.len()
                );
            }
            stale_fallback_station_count += batch.len();
            continue;
        }

        let query = overpass_lift_station_topology_query(batch);
        query_hashes.push(sha256_text(query.as_str()));
        query_count += 1;
        match overpass_request_with_fallback_with_config(
            client,
            overpass_config,
            preferred_endpoint.as_deref(),
            &query,
            "station topology",
            &mut pacer,
            &mut sleep,
        ) {
            Ok(response) => {
                let (topologies, _) =
                    overpass_json_to_lift_station_topology(&response.value, batch)?;
                if topologies.len() != batch.len() {
                    bail!(
                        "Overpass station topology response returned {} of {} requested stations",
                        topologies.len(),
                        batch.len()
                    );
                }
                for topology in topologies {
                    cache.stations.insert(
                        topology.station_source,
                        CachedStationTopology {
                            fetched_at: now,
                            members: topology.members,
                        },
                    );
                }
                write_station_topology_cache(&cache_path, &cache)?;
                refreshed_station_count += batch.len();
                preferred_endpoint = Some(response.endpoint);
            }
            Err(error) => {
                if batch
                    .iter()
                    .any(|source| !cache.stations.contains_key(source))
                {
                    return Err(error).with_context(|| {
                        format!(
                            "querying station topology for {} station source IDs",
                            batch.len()
                        )
                    });
                }
                eprintln!(
                    "Using stale station topology cache for {} station source IDs: {error:#}",
                    batch.len()
                );
                stale_fallback_station_count += batch.len();
                refresh_blocked = true;
            }
        }
    }

    let topologies = station_sources
        .iter()
        .map(|station_source| {
            let members = cache
                .stations
                .get(station_source)
                .ok_or_else(|| anyhow!("station topology cache missing {station_source}"))?
                .members
                .clone();
            Ok(LiftStationTopology {
                station_source: station_source.clone(),
                members,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let summary = topology_summary(&topologies);
    let value = serde_json::to_value(&topologies)?;
    write_json_atomically(&target, &value)?;
    let status = if query_count == 0 {
        "cached"
    } else if refreshed_station_count == 0 {
        "stale-cache"
    } else if stale_fallback_station_count > 0 {
        "overpass-with-stale-fallback"
    } else {
        "overpass"
    };
    let metadata_url = if query_count > 0 {
        preferred_endpoint.as_deref().map(overpass_interpreter_url)
    } else {
        None
    };
    let url = metadata_url
        .clone()
        .map(Value::String)
        .unwrap_or(Value::Null);
    Ok(json!({
        "name": LIFT_STATION_TOPOLOGY_FILE,
        "status": status,
        "url": url,
        "querySha256": if query_hashes.is_empty() {
            Value::Null
        } else {
            Value::String(sha256_text(&query_hashes.join("\n")))
        },
        "fetchedAt": Utc::now(),
        "stationCount": summary.station_count,
        "featureCount": summary.membership_count,
        "freshStationCount": fresh_station_count,
        "staleStationCount": stale_station_count,
        "missingStationCount": missing_station_count,
        "refreshedStationCount": refreshed_station_count,
        "staleFallbackStationCount": stale_fallback_station_count,
        "queryCount": query_count,
        "persistentCachePath": cache_path,
        "metadata": file_metadata(LIFT_STATION_TOPOLOGY_FILE, &target, metadata_url)?
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
pub(super) fn fetch_or_extract_connections_with_config(
    dataset_dir: &Path,
    overpass_base_url: Option<&str>,
    overpass_config: &OverpassConfig,
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

    let persistent_cache_path = overpass_connection_cache_path(dataset_dir);
    let persistent_cache = if persistent_cache_path.exists() {
        match read_connection_cache(&persistent_cache_path) {
            Ok(cache) => Some(cache),
            Err(error) => {
                eprintln!(
                    "Ignoring invalid persistent connection cache {}: {error:#}",
                    persistent_cache_path.display()
                );
                None
            }
        }
    } else {
        None
    };
    let now = Utc::now().timestamp();
    if let Some((fetched_at, connections)) = &persistent_cache {
        if overpass_cache_entry_is_fresh_after_days(
            *fetched_at,
            now,
            overpass_config.cache_stale_after_days,
        ) {
            write_json_atomically(&target, connections)?;
            let metadata = file_metadata(CONNECTIONS_FILE, &target, None)?;
            return Ok(json!({
                "name": CONNECTIONS_FILE,
                "status": "cached",
                "source": "persistent-overpass-cache",
                "fetchedAt": fetched_at,
                "persistentCachePath": persistent_cache_path,
                "metadata": metadata
            }));
        }
    }

    let query = overpass_connection_query();
    let mut pacer = OverpassPacer::default();
    let mut sleep = |duration| std::thread::sleep(duration);
    let response = match overpass_request_with_fallback_with_config(
        client,
        overpass_config,
        overpass_base_url,
        &query,
        "connection enrichment",
        &mut pacer,
        &mut sleep,
    ) {
        Ok(response) => response,
        Err(error) => {
            if let Some((fetched_at, connections)) = &persistent_cache {
                eprintln!("Using stale connection cache after Overpass failure: {error:#}");
                write_json_atomically(&target, connections)?;
                let metadata = file_metadata(CONNECTIONS_FILE, &target, None)?;
                return Ok(json!({
                    "name": CONNECTIONS_FILE,
                    "status": "stale-cache",
                    "source": "persistent-overpass-cache",
                    "error": error.to_string(),
                    "fetchedAt": fetched_at,
                    "persistentCachePath": persistent_cache_path,
                    "metadata": metadata
                }));
            }
            return Err(error).context("querying Overpass connection enrichment");
        }
    };
    let (connections, summary) = overpass_json_to_connection_geojson(&response.value)?;
    let endpoint = response.endpoint;
    write_connection_cache(&persistent_cache_path, now, &connections)?;
    write_json_atomically(&target, &connections)?;
    Ok(json!({
        "name": CONNECTIONS_FILE,
        "status": "overpass",
        "url": endpoint.clone(),
        "querySha256": sha256_text(query.as_str()),
        "fetchedAt": Utc::now(),
        "featureCount": summary.feature_count,
        "ignoredElementCount": summary.ignored_count,
        "persistentCachePath": persistent_cache_path,
        "metadata": file_metadata(CONNECTIONS_FILE, &target, Some(endpoint))?
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
