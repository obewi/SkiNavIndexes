use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use flate2::{Compression, write::GzEncoder};
use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, File},
    io::{BufReader, ErrorKind, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use tar::Builder;
use walkdir::WalkDir;

const LAYER_FILES: [&str; 3] = ["ski_areas.geojson", "runs.geojson", "lifts.geojson"];
const CONNECTIONS_FILE: &str = "connections.geojson";
const RENDER_SCHEMA_VERSION: i64 = 23;
const PIPELINE_SCHEMA_VERSION: i64 = 1;
const RELEASE_PACK_TARGET_BYTES: u64 = 24 * 1024 * 1024;
const RELEASE_PACK_SMALL_GROUP_BYTES: u64 = 1 * 1024 * 1024;
const RELEASE_PACK_LARGE_GROUP_BYTES: u64 = 24 * 1024 * 1024;
const DEFAULT_OVERPASS_BASE_URL: &str = "https://overpass-api.de/api/";
const CONNECTION_ENDPOINT_MATCH_METERS: f64 = 60.0;
const CONNECTION_SEGMENT_MATCH_METERS: f64 = 35.0;
const CONNECTION_SEARCH_PADDING_METERS: f64 = 300.0;
const NETWORK_BUCKET_DEGREES: f64 = 0.02;

#[derive(Parser)]
#[command(
    version,
    about = "Build SkiNav indexes and artifacts from OpenSkiMap GeoJSON"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download OpenSkiMap GeoJSON source layers if they are not already cached.
    Fetch {
        #[arg(long, default_value = "data/raw/openskimap")]
        cache_dir: PathBuf,
        #[arg(long)]
        dataset_version: Option<String>,
        #[arg(long, default_value = "https://tiles.openskimap.org/geojson")]
        source_base_url: String,
        #[arg(long, default_value = DEFAULT_OVERPASS_BASE_URL)]
        overpass_base_url: String,
        #[arg(long)]
        skip_connection_enrichment: bool,
    },
    /// Build indexes, packages, group archives, and local-app artifacts from cached source files.
    Build {
        #[arg(long, default_value = "data/raw/openskimap")]
        cache_dir: PathBuf,
        #[arg(long, default_value = "output")]
        output_dir: PathBuf,
        #[arg(long)]
        dataset_version: Option<String>,
    },
    /// Validate generated output files.
    Validate {
        #[arg(long, default_value = "output")]
        output_dir: PathBuf,
    },
    /// Fetch missing sources, build outputs, and validate them.
    All {
        #[arg(long, default_value = "data/raw/openskimap")]
        cache_dir: PathBuf,
        #[arg(long, default_value = "output")]
        output_dir: PathBuf,
        #[arg(long)]
        dataset_version: Option<String>,
        #[arg(long, default_value = "https://tiles.openskimap.org/geojson")]
        source_base_url: String,
        #[arg(long, default_value = DEFAULT_OVERPASS_BASE_URL)]
        overpass_base_url: String,
        #[arg(long)]
        skip_connection_enrichment: bool,
        #[arg(long)]
        skip_fetch: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Fetch {
            cache_dir,
            dataset_version,
            source_base_url,
            overpass_base_url,
            skip_connection_enrichment,
        } => fetch_sources(
            &cache_dir,
            dataset_version,
            &source_base_url,
            &overpass_base_url,
            skip_connection_enrichment,
        ),
        Command::Build {
            cache_dir,
            output_dir,
            dataset_version,
        } => build_from_cache(&cache_dir, &output_dir, dataset_version).map(|summary| {
            eprintln!(
                "built dataset {}: {} resorts, {} runs, {} lifts, {} connections",
                summary.dataset_version,
                summary.resort_count,
                summary.run_count,
                summary.lift_count,
                summary.connection_count
            );
        }),
        Command::Validate { output_dir } => validate_output(&output_dir),
        Command::All {
            cache_dir,
            output_dir,
            dataset_version,
            source_base_url,
            overpass_base_url,
            skip_connection_enrichment,
            skip_fetch,
        } => {
            if !skip_fetch {
                fetch_sources(
                    &cache_dir,
                    dataset_version.clone(),
                    &source_base_url,
                    &overpass_base_url,
                    skip_connection_enrichment,
                )?;
            }
            let summary = build_from_cache(&cache_dir, &output_dir, dataset_version)?;
            eprintln!(
                "built dataset {}: {} resorts, {} runs, {} lifts, {} connections",
                summary.dataset_version,
                summary.resort_count,
                summary.run_count,
                summary.lift_count,
                summary.connection_count
            );
            validate_output(&output_dir)
        }
    }
}

fn fetch_sources(
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

fn validate_geojson_file(path: &Path) -> Result<()> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let value: Value = serde_json::from_reader(reader)?;
    if value.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
        bail!("expected GeoJSON FeatureCollection");
    }
    if !value.get("features").is_some_and(Value::is_array) {
        bail!("expected features array");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConnectionConversionSummary {
    feature_count: usize,
    ignored_count: usize,
}

fn fetch_or_extract_connections(
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

fn openskimap_has_connections(dataset_dir: &Path) -> Result<bool> {
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

fn write_connections_from_openskimap(dataset_dir: &Path, target: &Path) -> Result<usize> {
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

fn is_openskimap_connection(props: &Map<String, Value>) -> bool {
    first_string(props, &["type"]).is_some_and(|value| value.eq_ignore_ascii_case("connection"))
}

fn overpass_connection_query() -> String {
    r#"[out:json][timeout:900];
(
  way["piste:type"="connection"](-90,-180,90,180);
  relation["piste:type"="connection"](-90,-180,90,180);
);
out body geom;"#
        .to_string()
}

fn overpass_json_to_connection_geojson(
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

fn overpass_element_to_connection_feature(element: &Value) -> Option<Value> {
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

fn overpass_geometry_coordinates(geometry: &Value) -> Option<Vec<Value>> {
    let points = geometry.as_array()?;
    let mut coordinates = Vec::new();
    for point in points {
        let lon = point.get("lon").and_then(Value::as_f64)?;
        let lat = point.get("lat").and_then(Value::as_f64)?;
        coordinates.push(json!([lon, lat]));
    }
    Some(coordinates)
}

fn reset_output_dir(output_dir: &Path) -> Result<()> {
    fs::create_dir_all(output_dir)
        .with_context(|| format!("creating generated output {}", output_dir.display()))?;
    for entry in
        fs::read_dir(output_dir).with_context(|| format!("reading {}", output_dir.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", output_dir.display()))?;
        remove_generated_path(&entry.path())
            .with_context(|| format!("removing generated path {}", entry.path().display()))?;
    }
    File::create(output_dir.join(".gitkeep"))
        .with_context(|| format!("creating {}", output_dir.join(".gitkeep").display()))?;
    Ok(())
}

fn remove_generated_path(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };

    if metadata.is_dir() {
        for _ in 0..5 {
            for entry in
                fs::read_dir(path).with_context(|| format!("reading {}", path.display()))?
            {
                let entry =
                    entry.with_context(|| format!("reading entry in {}", path.display()))?;
                remove_generated_path(&entry.path())?;
            }
            match fs::remove_dir(path) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
                Err(error) if error.raw_os_error() == Some(66) => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("removing directory {}", path.display()));
                }
            }
        }
        fs::remove_dir(path).with_context(|| format!("removing directory {}", path.display()))?;
        Ok(())
    } else {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("removing file {}", path.display())),
        }
    }
}

fn build_from_cache(
    cache_dir: &Path,
    output_dir: &Path,
    dataset_version: Option<String>,
) -> Result<BuildSummary> {
    let dataset_dir = resolve_dataset_dir(cache_dir, dataset_version)?;
    let dataset_version = dataset_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid dataset directory {}", dataset_dir.display()))?
        .to_string();

    for layer in LAYER_FILES {
        let path = dataset_dir.join(layer);
        if !path.exists() {
            bail!("missing cached source layer {}", path.display());
        }
    }
    let connections_path = dataset_dir.join(CONNECTIONS_FILE);
    if !connections_path.exists() {
        if openskimap_has_connections(&dataset_dir)? {
            bail!(
                "missing cached connection layer {}; run fetch first to extract OpenSkiMap type=connection features",
                connections_path.display()
            );
        }
        bail!(
            "missing cached connection layer {}; run fetch first to query Overpass connection enrichment",
            connections_path.display()
        );
    }

    reset_output_dir(output_dir)?;
    let ski_areas = read_feature_collection(&dataset_dir.join("ski_areas.geojson"))?;
    let runs = read_feature_collection(&dataset_dir.join("runs.geojson"))?;
    let lifts = read_feature_collection(&dataset_dir.join("lifts.geojson"))?;
    let connections = read_feature_collection(&connections_path)?;

    let generated_at = Utc::now();
    let normalized = normalize_sources(
        ski_areas,
        runs,
        lifts,
        connections,
        &dataset_version,
        generated_at,
    )?;
    write_outputs(output_dir, &normalized)?;

    Ok(BuildSummary {
        dataset_version,
        resort_count: normalized.resorts.len(),
        run_count: normalized.runs.len(),
        lift_count: normalized.lifts.len(),
        connection_count: normalized.connections.len(),
    })
}

fn resolve_dataset_dir(cache_dir: &Path, dataset_version: Option<String>) -> Result<PathBuf> {
    if let Some(version) = dataset_version {
        return Ok(cache_dir.join(version));
    }

    let mut candidates = Vec::new();
    if cache_dir.exists() {
        for entry in fs::read_dir(cache_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let path = entry.path();
                if LAYER_FILES.iter().all(|layer| path.join(layer).exists()) {
                    candidates.push(path);
                }
            }
        }
    }
    candidates.sort();
    candidates.pop().ok_or_else(|| {
        anyhow!(
            "no cached dataset with all required layers under {}",
            cache_dir.display()
        )
    })
}

fn read_feature_collection(path: &Path) -> Result<Vec<SourceFeature>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut root: Value =
        serde_json::from_reader(reader).with_context(|| format!("parsing {}", path.display()))?;
    if root.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
        bail!("{} is not a FeatureCollection", path.display());
    }
    let features = root
        .get_mut("features")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("{} missing features array", path.display()))?;
    let features = std::mem::take(features);

    features
        .into_iter()
        .map(SourceFeature::from_value)
        .collect::<Result<Vec<_>>>()
}

#[derive(Clone, Debug)]
struct SourceFeature {
    id: Option<Value>,
    properties: Map<String, Value>,
    geometry: Value,
}

impl SourceFeature {
    fn from_value(value: Value) -> Result<Self> {
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("feature must be an object"))?;
        let properties = object
            .get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let geometry = object
            .get("geometry")
            .cloned()
            .ok_or_else(|| anyhow!("feature missing geometry"))?;
        Ok(Self {
            id: object.get("id").cloned(),
            properties,
            geometry,
        })
    }

    fn source_id(&self, prefix: &str, index: usize) -> String {
        first_string(&self.properties, &["id", "sourceId", "osmId"])
            .or_else(|| self.id.as_ref().and_then(value_to_string))
            .unwrap_or_else(|| format!("{prefix}:{index}"))
    }
}

#[derive(Debug)]
struct BuildSummary {
    dataset_version: String,
    resort_count: usize,
    run_count: usize,
    lift_count: usize,
    connection_count: usize,
}

#[derive(Debug)]
struct NormalizedDataset {
    dataset_version: String,
    generated_at: DateTime<Utc>,
    resorts: Vec<ResortRecord>,
    runs: Vec<FeatureRecord>,
    lifts: Vec<FeatureRecord>,
    connections: Vec<FeatureRecord>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ResortRecord {
    id: String,
    name: String,
    #[serde(rename = "type")]
    resort_type: String,
    #[serde(rename = "parent_id")]
    parent_id: Option<String>,
    #[serde(rename = "parent_name")]
    parent_name: Option<String>,
    bbox: [f64; 4],
    #[serde(rename = "area_km2")]
    area_km2: f64,
    country: Option<String>,
    #[serde(rename = "isoCodes")]
    iso_codes: Vec<String>,
    #[serde(rename = "countryCodes")]
    country_codes: Vec<String>,
    #[serde(rename = "groupId")]
    group_id: String,
    center: [f64; 2],
    #[serde(rename = "childIds")]
    child_ids: Vec<String>,
    #[serde(skip)]
    run_convention: Option<String>,
    #[serde(skip)]
    places: Value,
    #[serde(skip)]
    statistics: Value,
}

#[derive(Debug, Clone)]
struct FeatureRecord {
    id: String,
    resort_ids: Vec<String>,
    geometry: Value,
    properties: Map<String, Value>,
}

fn normalize_sources(
    ski_areas: Vec<SourceFeature>,
    runs: Vec<SourceFeature>,
    lifts: Vec<SourceFeature>,
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
    let mut connection_candidates = Vec::new();

    for (index, feature) in runs.into_iter().enumerate() {
        let id = feature.source_id("run", index);
        if !has_any_value(&feature.properties, &["uses", "activities"], "downhill") {
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

    if resorts.is_empty() {
        bail!("no operating downhill resorts found; check OpenSkiMap schema and source files");
    }

    Ok(NormalizedDataset {
        dataset_version: dataset_version.to_string(),
        generated_at,
        resorts,
        runs: normalized_runs,
        lifts: normalized_lifts,
        connections: normalized_connections,
        warnings,
    })
}

fn write_outputs(output_dir: &Path, dataset: &NormalizedDataset) -> Result<()> {
    if output_dir.exists() {
        fs::remove_dir_all(output_dir)
            .with_context(|| format!("clearing {}", output_dir.display()))?;
    }
    fs::create_dir_all(output_dir)?;

    let runs_by_resort = records_by_resort(&dataset.runs);
    let lifts_by_resort = records_by_resort(&dataset.lifts);
    let connections_by_resort = records_by_resort(&dataset.connections);

    write_discovery_index(output_dir, dataset)?;
    write_resort_packages(
        output_dir,
        dataset,
        &runs_by_resort,
        &lifts_by_resort,
        &connections_by_resort,
    )?;
    write_group_archives(output_dir, dataset)?;
    write_release_packs(output_dir, dataset)?;
    write_local_app_layout(output_dir, dataset)?;

    let report = json!({
        "datasetVersion": dataset.dataset_version,
        "generatedAt": dataset.generated_at,
        "schemaVersion": PIPELINE_SCHEMA_VERSION,
        "resortCount": dataset.resorts.len(),
        "runCount": dataset.runs.len(),
        "liftCount": dataset.lifts.len(),
        "connectionCount": dataset.connections.len(),
        "warnings": dataset.warnings,
    });
    write_json_pretty(&output_dir.join("build-report.json"), &report)?;
    Ok(())
}

fn write_discovery_index(output_dir: &Path, dataset: &NormalizedDataset) -> Result<()> {
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
        "localArtifactRoot": "local-app",
    });
    write_json_pretty(&output_dir.join("latest.json"), &latest)?;
    Ok(())
}

fn write_resort_packages<'a>(
    output_dir: &Path,
    dataset: &NormalizedDataset,
    runs_by_resort: &BTreeMap<String, Vec<&'a FeatureRecord>>,
    lifts_by_resort: &BTreeMap<String, Vec<&'a FeatureRecord>>,
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
        let connections = connections_by_resort
            .get(&resort.id)
            .cloned()
            .unwrap_or_default();

        let downhill_lines = line_feature_collection(&runs, resort);
        let downhill_polygons = polygon_feature_collection(&runs, resort);
        let centerlines = centerline_feature_collection(&runs, resort);
        let connection_lines = connection_feature_collection(&connections, resort);
        let connection_centerlines = connection_centerline_feature_collection(&connections, resort);
        let lifts_geojson = lift_feature_collection(&lifts);
        let lift_stations = lift_station_feature_collection(&lifts);
        let audit = audit_report(
            resort,
            runs.len(),
            lifts.len(),
            connections.len(),
            &centerlines,
            &connection_centerlines,
            &lift_stations,
        );

        write_json_pretty(&package_dir.join("downhill_lines.geojson"), &downhill_lines)?;
        write_json_pretty(
            &package_dir.join("downhill_polygons.geojson"),
            &downhill_polygons,
        )?;
        write_json_pretty(
            &package_dir.join("downhill_centerlines.geojson"),
            &centerlines,
        )?;
        write_json_pretty(&package_dir.join("connections.geojson"), &connection_lines)?;
        write_json_pretty(
            &package_dir.join("connection_centerlines.geojson"),
            &connection_centerlines,
        )?;
        write_json_pretty(&package_dir.join("lifts.geojson"), &lifts_geojson)?;
        write_json_pretty(&package_dir.join("lift_stations.geojson"), &lift_stations)?;
        write_json_pretty(&package_dir.join("audit_report.json"), &audit)?;

        let manifest = app_render_manifest(
            resort,
            dataset.generated_at,
            runs.len(),
            lifts.len(),
            connections.len(),
            &centerlines,
            &downhill_polygons,
            &connection_centerlines,
            &lift_stations,
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
                "connections": connections.len(),
                "centerlines": feature_count(&centerlines),
                "connectionCenterlines": feature_count(&connection_centerlines),
                "liftStations": feature_count(&lift_stations)
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

fn write_domain_package(
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
            "centerlines": 0,
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

fn records_by_resort(records: &[FeatureRecord]) -> BTreeMap<String, Vec<&FeatureRecord>> {
    let mut grouped: BTreeMap<String, Vec<&FeatureRecord>> = BTreeMap::new();
    for record in records {
        for resort_id in &record.resort_ids {
            grouped.entry(resort_id.clone()).or_default().push(record);
        }
    }
    grouped
}

fn write_group_archives(output_dir: &Path, dataset: &NormalizedDataset) -> Result<()> {
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReleaseResortInput {
    id: String,
    estimated_size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReleaseGroupInput {
    group_id: String,
    resorts: Vec<ReleaseResortInput>,
    estimated_size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReleasePackGroup {
    group_id: String,
    part_index: Option<usize>,
    part_count: Option<usize>,
    resort_ids: Vec<String>,
    estimated_size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReleasePackPlan {
    asset_name: String,
    archive_type: &'static str,
    groups: Vec<ReleasePackGroup>,
    estimated_size_bytes: u64,
}

fn write_release_packs(output_dir: &Path, dataset: &NormalizedDataset) -> Result<()> {
    let release_dir = output_dir.join("release-packs");
    fs::create_dir_all(&release_dir)?;

    let group_inputs = release_group_inputs(output_dir, dataset)?;
    let plans = plan_release_packs(group_inputs);
    let resort_lookup = dataset
        .resorts
        .iter()
        .map(|resort| (resort.id.as_str(), resort))
        .collect::<HashMap<_, _>>();

    let mut assets = Vec::new();
    for plan in &plans {
        let staging = release_dir.join(format!(
            ".staging-{}",
            plan.asset_name.trim_end_matches(".tar.gz")
        ));
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(&staging)?;

        let archive_manifest = json!({
            "schemaVersion": PIPELINE_SCHEMA_VERSION,
            "datasetVersion": dataset.dataset_version,
            "generatedAt": dataset.generated_at,
            "assetName": plan.asset_name,
            "archiveType": plan.archive_type,
            "estimatedSizeBytes": plan.estimated_size_bytes,
            "groups": plan.groups.iter().map(|group| {
                json!({
                    "groupId": group.group_id,
                    "partIndex": group.part_index,
                    "partCount": group.part_count,
                    "estimatedSizeBytes": group.estimated_size_bytes,
                    "resortIds": group.resort_ids,
                })
            }).collect::<Vec<_>>(),
        });
        write_json_pretty(&staging.join("manifest.json"), &archive_manifest)?;

        for group in &plan.groups {
            write_release_pack_group(output_dir, &staging, group, &resort_lookup)?;
        }

        let archive_path = release_dir.join(&plan.asset_name);
        create_tar_gz(&staging, &archive_path)?;
        fs::remove_dir_all(&staging)?;

        assets.push(json!({
            "assetName": plan.asset_name,
            "archiveType": plan.archive_type,
            "size": fs::metadata(&archive_path)?.len(),
            "estimatedSizeBytes": plan.estimated_size_bytes,
            "hash": format!("sha256:{}", sha256_file(&archive_path)?),
            "groups": plan.groups.iter().map(|group| {
                json!({
                    "groupId": group.group_id,
                    "partIndex": group.part_index,
                    "partCount": group.part_count,
                    "resortCount": group.resort_ids.len(),
                    "resortIds": group.resort_ids,
                })
            }).collect::<Vec<_>>(),
        }));
    }

    let manifest = json!({
        "schemaVersion": PIPELINE_SCHEMA_VERSION,
        "datasetVersion": dataset.dataset_version,
        "generatedAt": dataset.generated_at,
        "strategy": {
            "targetBytes": RELEASE_PACK_TARGET_BYTES,
            "smallGroupBytes": RELEASE_PACK_SMALL_GROUP_BYTES,
            "largeGroupBytes": RELEASE_PACK_LARGE_GROUP_BYTES,
            "description": "Large logical groups are split by resort package and tiny groups are combined into balanced release packs."
        },
        "assetBaseUrl": format!(
            "https://github.com/obewi/SkiNavIndexes/releases/download/indexes-{}",
            dataset.dataset_version
        ),
        "assetCount": assets.len(),
        "assets": assets,
    });
    write_json_pretty(&release_dir.join("manifest.json"), &manifest)?;
    Ok(())
}

fn write_release_pack_group(
    output_dir: &Path,
    staging: &Path,
    group: &ReleasePackGroup,
    resort_lookup: &HashMap<&str, &ResortRecord>,
) -> Result<()> {
    let group_dir = staging.join("groups").join(safe_path_id(&group.group_id));
    fs::create_dir_all(group_dir.join("resorts"))?;
    let resorts = group
        .resort_ids
        .iter()
        .map(|resort_id| {
            resort_lookup
                .get(resort_id.as_str())
                .ok_or_else(|| anyhow!("release pack references unknown resort {resort_id}"))
        })
        .collect::<Result<Vec<_>>>()?;

    let group_manifest = json!({
        "schemaVersion": PIPELINE_SCHEMA_VERSION,
        "groupId": group.group_id,
        "partIndex": group.part_index,
        "partCount": group.part_count,
        "estimatedSizeBytes": group.estimated_size_bytes,
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
    write_json_pretty(&group_dir.join("manifest.json"), &group_manifest)?;

    for resort in resorts {
        let source = output_dir
            .join("packages")
            .join("resorts")
            .join(safe_path_id(&resort.id));
        let target = group_dir.join("resorts").join(safe_path_id(&resort.id));
        copy_dir_recursive(&source, &target)?;
    }

    Ok(())
}

fn release_group_inputs(
    output_dir: &Path,
    dataset: &NormalizedDataset,
) -> Result<Vec<ReleaseGroupInput>> {
    let mut by_group: BTreeMap<String, Vec<ReleaseResortInput>> = BTreeMap::new();
    for resort in &dataset.resorts {
        let package_dir = output_dir
            .join("packages")
            .join("resorts")
            .join(safe_path_id(&resort.id));
        let estimated_size_bytes = directory_size(&package_dir)
            .with_context(|| format!("sizing package {}", package_dir.display()))?;
        by_group
            .entry(resort.group_id.clone())
            .or_default()
            .push(ReleaseResortInput {
                id: resort.id.clone(),
                estimated_size_bytes,
            });
    }

    Ok(by_group
        .into_iter()
        .map(|(group_id, mut resorts)| -> Result<ReleaseGroupInput> {
            resorts.sort_by(|left, right| left.id.cmp(&right.id));
            let group_archive = output_dir
                .join("groups")
                .join(format!("{}.tar.gz", safe_path_id(&group_id)));
            let estimated_size_bytes = if group_archive.exists() {
                fs::metadata(&group_archive)?.len()
            } else {
                resorts
                    .iter()
                    .map(|resort| resort.estimated_size_bytes)
                    .sum()
            };
            Ok(ReleaseGroupInput {
                group_id,
                resorts,
                estimated_size_bytes,
            })
        })
        .collect::<Result<Vec<_>>>()?)
}

fn plan_release_packs(groups: Vec<ReleaseGroupInput>) -> Vec<ReleasePackPlan> {
    let mut packs = Vec::new();
    let mut small_groups = Vec::new();

    for group in groups {
        if group.estimated_size_bytes > RELEASE_PACK_LARGE_GROUP_BYTES && group.resorts.len() > 1 {
            packs.extend(split_large_group(group));
        } else if group.estimated_size_bytes < RELEASE_PACK_SMALL_GROUP_BYTES {
            small_groups.push(group);
        } else {
            packs.push(single_group_pack(group));
        }
    }

    packs.extend(pack_small_groups(small_groups));
    packs.sort_by(|left, right| left.asset_name.cmp(&right.asset_name));
    packs
}

fn single_group_pack(group: ReleaseGroupInput) -> ReleasePackPlan {
    ReleasePackPlan {
        asset_name: format!("{}.tar.gz", safe_path_id(&group.group_id)),
        archive_type: "group",
        estimated_size_bytes: group.estimated_size_bytes,
        groups: vec![ReleasePackGroup {
            group_id: group.group_id,
            part_index: None,
            part_count: None,
            estimated_size_bytes: group.estimated_size_bytes,
            resort_ids: group.resorts.into_iter().map(|resort| resort.id).collect(),
        }],
    }
}

fn split_large_group(group: ReleaseGroupInput) -> Vec<ReleasePackPlan> {
    let mut resorts = group.resorts;
    resorts.sort_by(|left, right| {
        right
            .estimated_size_bytes
            .cmp(&left.estimated_size_bytes)
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut parts: Vec<Vec<ReleaseResortInput>> = Vec::new();
    let mut part_sizes: Vec<u64> = Vec::new();
    for resort in resorts {
        let target_index = part_sizes
            .iter()
            .enumerate()
            .find(|(_, size)| **size + resort.estimated_size_bytes <= RELEASE_PACK_TARGET_BYTES)
            .map(|(index, _)| index);
        match target_index {
            Some(index) => {
                part_sizes[index] += resort.estimated_size_bytes;
                parts[index].push(resort);
            }
            None => {
                part_sizes.push(resort.estimated_size_bytes);
                parts.push(vec![resort]);
            }
        }
    }

    let part_count = parts.len();
    parts
        .into_iter()
        .enumerate()
        .map(|(index, mut resorts)| {
            resorts.sort_by(|left, right| left.id.cmp(&right.id));
            let estimated_size_bytes = resorts
                .iter()
                .map(|resort| resort.estimated_size_bytes)
                .sum();
            let part_index = index + 1;
            ReleasePackPlan {
                asset_name: format!(
                    "{}.part-{:03}-of-{:03}.tar.gz",
                    safe_path_id(&group.group_id),
                    part_index,
                    part_count
                ),
                archive_type: "group-part",
                estimated_size_bytes,
                groups: vec![ReleasePackGroup {
                    group_id: group.group_id.clone(),
                    part_index: Some(part_index),
                    part_count: Some(part_count),
                    estimated_size_bytes,
                    resort_ids: resorts.into_iter().map(|resort| resort.id).collect(),
                }],
            }
        })
        .collect()
}

fn pack_small_groups(mut groups: Vec<ReleaseGroupInput>) -> Vec<ReleasePackPlan> {
    groups.sort_by(|left, right| left.group_id.cmp(&right.group_id));
    let mut packs = Vec::new();
    let mut current_groups = Vec::new();
    let mut current_size = 0;
    let mut pack_index = 1;

    for group in groups {
        if !current_groups.is_empty()
            && current_size + group.estimated_size_bytes > RELEASE_PACK_TARGET_BYTES
        {
            packs.push(small_groups_pack(pack_index, current_groups, current_size));
            pack_index += 1;
            current_groups = Vec::new();
            current_size = 0;
        }
        current_size += group.estimated_size_bytes;
        current_groups.push(group);
    }

    if !current_groups.is_empty() {
        packs.push(small_groups_pack(pack_index, current_groups, current_size));
    }

    packs
}

fn small_groups_pack(
    pack_index: usize,
    groups: Vec<ReleaseGroupInput>,
    estimated_size_bytes: u64,
) -> ReleasePackPlan {
    ReleasePackPlan {
        asset_name: format!("small-groups-{pack_index:03}.tar.gz"),
        archive_type: "small-groups",
        estimated_size_bytes,
        groups: groups
            .into_iter()
            .map(|group| ReleasePackGroup {
                group_id: group.group_id,
                part_index: None,
                part_count: None,
                estimated_size_bytes: group.estimated_size_bytes,
                resort_ids: group.resorts.into_iter().map(|resort| resort.id).collect(),
            })
            .collect(),
    }
}

fn write_local_app_layout(output_dir: &Path, dataset: &NormalizedDataset) -> Result<()> {
    let local_app = output_dir.join("local-app");
    fs::create_dir_all(local_app.join("render-bundles"))?;
    fs::create_dir_all(local_app.join("graphs"))?;
    fs::copy(
        output_dir.join("resorts.json"),
        local_app.join("resorts.json"),
    )?;
    fs::copy(
        output_dir.join("latest.json"),
        local_app.join("latest.json"),
    )?;

    for resort in &dataset.resorts {
        let package_dir = output_dir
            .join("packages")
            .join("resorts")
            .join(safe_path_id(&resort.id));
        if resort.resort_type == "domain" {
            let graph_meta = json!({
                "schemaVersion": 6,
                "regionId": resort.id,
                "generatedAt": dataset.generated_at,
                "source": "SkiNavIndexes Rust OpenSkiMap GeoJSON pipeline",
                "status": "domain-reference-only",
                "childIds": resort.child_ids,
                "note": "Domain resorts reference child render bundles and do not duplicate child run/lift artifacts."
            });
            write_json_pretty(
                &local_app
                    .join("graphs")
                    .join(format!("{}.graph.meta.json", resort.id)),
                &graph_meta,
            )?;
            continue;
        }

        let render_target = local_app.join("render-bundles").join(&resort.id);
        fs::create_dir_all(&render_target)?;
        for file in [
            "manifest.json",
            "downhill_lines.geojson",
            "downhill_centerlines.geojson",
            "downhill_polygons.geojson",
            "connections.geojson",
            "connection_centerlines.geojson",
            "lifts.geojson",
            "lift_stations.geojson",
        ] {
            hard_link_or_copy(&package_dir.join(file), &render_target.join(file))
                .with_context(|| format!("linking local-app render file {file}"))?;
        }

        let graph_meta = json!({
            "schemaVersion": 6,
            "regionId": resort.id,
            "generatedAt": dataset.generated_at,
            "source": "SkiNavIndexes Rust OpenSkiMap GeoJSON pipeline",
            "status": "not-built",
            "note": "This first-pass local-app layout supplies render artifacts. Binary SKIGRAPH generation is intentionally deferred to the shared SkiNav graph builder contract."
        });
        write_json_pretty(
            &local_app
                .join("graphs")
                .join(format!("{}.graph.meta.json", resort.id)),
            &graph_meta,
        )?;
    }

    let manifest = json!({
        "schemaVersion": PIPELINE_SCHEMA_VERSION,
        "datasetVersion": dataset.dataset_version,
        "generatedAt": dataset.generated_at,
        "layout": "SkiNav Documents seed",
        "containsBinaryGraphs": false,
        "resorts": dataset.resorts.iter().map(|resort| {
            json!({
                "id": resort.id,
                "name": resort.name,
                "type": resort.resort_type,
                "renderBundlePath": if resort.resort_type == "domain" {
                    Value::Null
                } else {
                    Value::String(format!("render-bundles/{}", resort.id))
                },
                "graphPath": format!("graphs/{}.graph", resort.id),
                "graphMetadataPath": format!("graphs/{}.graph.meta.json", resort.id),
                "childIds": resort.child_ids,
            })
        }).collect::<Vec<_>>()
    });
    write_json_pretty(&local_app.join("manifest.json"), &manifest)?;
    Ok(())
}

fn validate_output(output_dir: &Path) -> Result<()> {
    let resorts_path = output_dir.join("resorts.json");
    let latest_path = output_dir.join("latest.json");
    validate_geojson_or_json_exists(&resorts_path)?;
    validate_geojson_or_json_exists(&latest_path)?;

    let index: Value = read_json(&resorts_path)?;
    let resorts = index
        .get("resorts")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("resorts.json missing resorts array"))?;
    let total = index
        .get("total_resorts")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("resorts.json missing total_resorts"))?;
    if total as usize != resorts.len() {
        bail!(
            "total_resorts mismatch: declared {total}, actual {}",
            resorts.len()
        );
    }

    let packages = output_dir.join("packages").join("resorts");
    if !packages.exists() {
        bail!("missing packages/resorts output");
    }
    let release_pack_manifest = output_dir.join("release-packs").join("manifest.json");
    validate_geojson_or_json_exists(&release_pack_manifest)?;

    for resort in resorts {
        let id = resort
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("resort id must be a string"))?;
        let manifest = packages.join(safe_path_id(id)).join("manifest.json");
        validate_geojson_or_json_exists(&manifest)?;
        let artifact_manifest = packages
            .join(safe_path_id(id))
            .join("artifact_manifest.json");
        validate_geojson_or_json_exists(&artifact_manifest)?;
        let is_domain = resort.get("type").and_then(Value::as_str) == Some("domain");
        if is_domain {
            continue;
        }
        for file in [
            "downhill_lines.geojson",
            "downhill_centerlines.geojson",
            "downhill_polygons.geojson",
            "connections.geojson",
            "connection_centerlines.geojson",
            "lifts.geojson",
            "lift_stations.geojson",
            "audit_report.json",
        ] {
            validate_geojson_or_json_exists(&packages.join(safe_path_id(id)).join(file))?;
        }
    }
    Ok(())
}

fn validate_geojson_or_json_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("missing {}", path.display());
    }
    let _: Value = read_json(path).with_context(|| format!("validating {}", path.display()))?;
    Ok(())
}

fn line_feature_collection(records: &[&FeatureRecord], resort: &ResortRecord) -> Value {
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

fn polygon_feature_collection(records: &[&FeatureRecord], resort: &ResortRecord) -> Value {
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

fn centerline_feature_collection(records: &[&FeatureRecord], resort: &ResortRecord) -> Value {
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

fn connection_feature_collection(records: &[&FeatureRecord], resort: &ResortRecord) -> Value {
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

fn connection_centerline_feature_collection(
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

fn lift_feature_collection(records: &[&FeatureRecord]) -> Value {
    let features = records
        .iter()
        .filter(|record| geometry_type(&record.geometry) == Some("LineString"))
        .map(|record| {
            let mut props = record.properties.clone();
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

fn lift_station_feature_collection(records: &[&FeatureRecord]) -> Value {
    let mut features = Vec::new();
    for record in records {
        if let Some(stations) = record.properties.get("stations").and_then(Value::as_array) {
            for (index, station) in stations.iter().enumerate() {
                if let Some(point) = station_point_geometry(station) {
                    let station_type =
                        first_string_from_value(station, &["position", "type", "stationType"])
                            .unwrap_or_else(|| "unknown".to_string());
                    let lift_type = first_string(&record.properties, &["liftType", "lift_type"]);
                    features.push(json!({
                        "type": "Feature",
                        "id": format!("{}:station:{index}", record.id),
                        "properties": {
                            "id": format!("{}:station:{index}", record.id),
                            "lift_id": record.id,
                            "station_type": station_type,
                            "lift_type": lift_type,
                        },
                        "geometry": point
                    }));
                }
            }
        }
        if features.iter().all(|feature| {
            feature
                .get("properties")
                .and_then(|props| props.get("lift_id"))
                .and_then(Value::as_str)
                != Some(record.id.as_str())
        }) {
            let endpoints = line_endpoints(&record.geometry);
            for (station_type, coord) in [("bottom", endpoints.0), ("top", endpoints.1)] {
                if let Some(coord) = coord {
                    features.push(json!({
                        "type": "Feature",
                        "id": format!("{}:{station_type}", record.id),
                        "properties": {
                            "id": format!("{}:{station_type}", record.id),
                            "lift_id": record.id,
                            "station_type": station_type,
                            "lift_type": first_string(&record.properties, &["liftType", "lift_type"]),
                        },
                        "geometry": {"type": "Point", "coordinates": coord}
                    }));
                }
            }
        }
    }
    json!({"type": "FeatureCollection", "features": features})
}

fn normalized_run_properties(record: &FeatureRecord, resort: &ResortRecord) -> Map<String, Value> {
    let mut props = record.properties.clone();
    props.insert(
        "source_way_id".to_string(),
        Value::String(record.id.clone()),
    );
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

fn normalized_connection_properties(
    record: &FeatureRecord,
    resort: &ResortRecord,
) -> Map<String, Value> {
    let mut props = record.properties.clone();
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

fn app_render_manifest(
    resort: &ResortRecord,
    generated_at: DateTime<Utc>,
    run_count: usize,
    lift_count: usize,
    connection_count: usize,
    centerlines: &Value,
    polygons: &Value,
    connection_centerlines: &Value,
    lift_stations: &Value,
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
            "connectionCenterlines": "connection_centerlines.geojson",
            "lifts": "lifts.geojson",
            "liftStations": "lift_stations.geojson"
        },
        "stats": {
            "downhillLineFeatureCount": run_count,
            "downhillCenterlineFeatureCount": feature_count(centerlines),
            "downhillCenterlineLabeledFeatureCount": null,
            "explicitOnewayCenterlineCount": null,
            "inferredOnewayCenterlineCount": null,
            "unknownDirectionCenterlineCount": null,
            "downhillPolygonFeatureCount": feature_count(polygons),
            "connectionFeatureCount": connection_count,
            "connectionCenterlineFeatureCount": feature_count(connection_centerlines),
            "liftFeatureCount": lift_count,
            "liftStationFeatureCount": feature_count(lift_stations)
        }
    })
}

fn audit_report(
    resort: &ResortRecord,
    run_count: usize,
    lift_count: usize,
    connection_count: usize,
    centerlines: &Value,
    connection_centerlines: &Value,
    lift_stations: &Value,
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
            "connections": connection_count,
            "centerlines": feature_count(centerlines),
            "connectionCenterlines": feature_count(connection_centerlines),
            "liftStations": feature_count(lift_stations)
        },
        "issues": issues,
        "qualityScore": if issues.is_empty() { 100 } else { 70 }
    })
}

fn apply_feature_bounds_to_resorts(
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

fn compute_resort_hierarchy(
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

fn assign_connections_to_leaf_resorts(
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

fn source_resort_index(
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
struct NetworkMatchFeature {
    resort_ids: Vec<String>,
    bbox: [f64; 4],
    endpoints: Vec<[f64; 2]>,
    points: Vec<[f64; 2]>,
}

#[derive(Debug)]
struct NetworkMatchIndex {
    features: Vec<NetworkMatchFeature>,
    buckets: HashMap<(i32, i32), Vec<usize>>,
}

fn build_network_match_index(
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

fn network_resort_matches(
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

fn bbox_cells(bbox: [f64; 4]) -> Vec<(i32, i32)> {
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

fn bucket_coord(value: f64) -> i32 {
    (value / NETWORK_BUCKET_DEGREES).floor() as i32
}

fn endpoint_match_score(left: &[[f64; 2]], right: &[[f64; 2]]) -> i32 {
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

fn segment_match_score(left_points: &[[f64; 2]], right_points: &[[f64; 2]]) -> i32 {
    let mut matches = 0;
    for point in left_points {
        if min_distance_to_polyline_meters(*point, right_points) <= CONNECTION_SEGMENT_MATCH_METERS
        {
            matches += 1;
        }
    }
    matches.min(3)
}

fn source_keys_from_properties(props: &Map<String, Value>) -> BTreeSet<String> {
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

fn has_any_value(props: &Map<String, Value>, keys: &[&str], needle: &str) -> bool {
    keys.iter()
        .filter_map(|key| props.get(*key))
        .any(|value| value_contains_string(value, needle))
}

fn value_contains_string(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text.eq_ignore_ascii_case(needle),
        Value::Array(items) => items.iter().any(|item| value_contains_string(item, needle)),
        Value::Object(object) => object
            .values()
            .any(|item| value_contains_string(item, needle)),
        _ => false,
    }
}

fn ski_area_ids(props: &Map<String, Value>) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for key in ["skiAreas", "skiAreaIds", "ski_area_ids", "ski_area"] {
        if let Some(value) = props.get(key) {
            collect_ids(value, &mut ids);
        }
    }
    ids.into_iter().collect()
}

fn collect_ids(value: &Value, ids: &mut BTreeSet<String>) {
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

fn iso_codes_from_places(value: Option<&Value>) -> Vec<String> {
    let mut codes = BTreeSet::new();
    collect_place_codes(value, &mut codes, &["iso3166_2", "iso3166-2", "isoCode"]);
    codes.into_iter().collect()
}

fn country_codes_from_places(value: Option<&Value>) -> Vec<String> {
    let mut codes = BTreeSet::new();
    collect_place_codes(
        value,
        &mut codes,
        &["iso3166_1Alpha2", "iso3166-1Alpha2", "countryCode"],
    );
    codes.into_iter().collect()
}

fn collect_place_codes(value: Option<&Value>, codes: &mut BTreeSet<String>, keys: &[&str]) {
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

fn bbox_from_geometry(geometry: &Value) -> Option<[f64; 4]> {
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

fn scan_coordinates(value: &Value, callback: &mut impl FnMut(f64, f64)) {
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

fn bbox_center(bbox: [f64; 4]) -> [f64; 2] {
    [(bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0]
}

fn merge_bbox(left: [f64; 4], right: [f64; 4]) -> [f64; 4] {
    [
        left[0].min(right[0]),
        left[1].min(right[1]),
        left[2].max(right[2]),
        left[3].max(right[3]),
    ]
}

fn padded_bbox(bbox: [f64; 4]) -> [f64; 4] {
    padded_bbox_meters(bbox, 500.0)
}

fn padded_bbox_meters(bbox: [f64; 4], meters: f64) -> [f64; 4] {
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

fn bbox_intersects(left: [f64; 4], right: [f64; 4]) -> bool {
    left[0] <= right[2] && left[2] >= right[0] && left[1] <= right[3] && left[3] >= right[1]
}

fn invalid_or_point_bbox(bbox: [f64; 4]) -> bool {
    bbox[0] == 0.0 && bbox[1] == 0.0 && bbox[2] == 0.0 && bbox[3] == 0.0
        || (bbox[0] - bbox[2]).abs() < f64::EPSILON
        || (bbox[1] - bbox[3]).abs() < f64::EPSILON
}

fn area_km2_from_bbox(bbox: [f64; 4]) -> f64 {
    let center_lat = (bbox[1] + bbox[3]) / 2.0;
    let width = (bbox[2] - bbox[0]).abs() * 111.0 * center_lat.to_radians().cos().abs();
    let height = (bbox[3] - bbox[1]).abs() * 111.0;
    (width * height * 100.0).round() / 100.0
}

fn line_geometries(geometry: &Value) -> Vec<Value> {
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

fn line_endpoints(geometry: &Value) -> (Option<Value>, Option<Value>) {
    let first_line = line_geometries(geometry).into_iter().next();
    let Some(line) = first_line else {
        return (None, None);
    };
    let coords = line.get("coordinates").and_then(Value::as_array);
    let start = coords.and_then(|items| items.first()).cloned();
    let end = coords.and_then(|items| items.last()).cloned();
    (start, end)
}

fn geometry_points(geometry: &Value) -> Vec<[f64; 2]> {
    let mut points = Vec::new();
    scan_coordinates(
        geometry.get("coordinates").unwrap_or(&Value::Null),
        &mut |lon, lat| {
            points.push([lon, lat]);
        },
    );
    points
}

fn geometry_endpoints(geometry: &Value) -> Vec<[f64; 2]> {
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

fn point_from_coord_value(value: &Value) -> Option<[f64; 2]> {
    let items = value.as_array()?;
    Some([items.first()?.as_f64()?, items.get(1)?.as_f64()?])
}

fn haversine_meters(left: [f64; 2], right: [f64; 2]) -> f64 {
    let radius = 6_371_000.0;
    let lat1 = left[1].to_radians();
    let lat2 = right[1].to_radians();
    let dlat = (right[1] - left[1]).to_radians();
    let dlon = (right[0] - left[0]).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * radius * h.sqrt().asin()
}

fn min_distance_to_polyline_meters(point: [f64; 2], line: &[[f64; 2]]) -> f64 {
    if line.len() < 2 {
        return f64::INFINITY;
    }
    line.windows(2)
        .map(|segment| point_segment_distance_meters(point, segment[0], segment[1]))
        .fold(f64::INFINITY, f64::min)
}

fn point_segment_distance_meters(point: [f64; 2], start: [f64; 2], end: [f64; 2]) -> f64 {
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

fn station_point_geometry(value: &Value) -> Option<Value> {
    if value
        .get("geometry")
        .and_then(|geometry| geometry.get("type"))
        .and_then(Value::as_str)
        == Some("Point")
    {
        return value.get("geometry").cloned();
    }
    if let Some(coords) = value.get("coordinates").and_then(Value::as_array) {
        if coords.len() >= 2 && coords[0].is_number() && coords[1].is_number() {
            return Some(json!({"type": "Point", "coordinates": coords}));
        }
    }
    let lon = first_number_from_value(value, &["lon", "lng", "longitude"])?;
    let lat = first_number_from_value(value, &["lat", "latitude"])?;
    Some(json!({"type": "Point", "coordinates": [lon, lat]}))
}

fn geometry_type(geometry: &Value) -> Option<&str> {
    geometry.get("type").and_then(Value::as_str)
}

fn endpoint_key_from_coord_value(value: &Value) -> Option<String> {
    let items = value.as_array()?;
    let lon = items.first()?.as_f64()?;
    let lat = items.get(1)?.as_f64()?;
    Some(format!("{lat:.6},{lon:.6}"))
}

fn first_string(props: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| props.get(*key))
        .filter_map(value_to_string)
        .map(|text| text.trim().to_string())
        .find(|text| !text.is_empty())
}

fn first_string_from_value(value: &Value, keys: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    first_string(object, keys)
}

fn first_number_from_value(value: &Value, keys: &[&str]) -> Option<f64> {
    let object = value.as_object()?;
    keys.iter()
        .filter_map(|key| object.get(*key))
        .find_map(Value::as_f64)
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn explicit_non_operating_status(status: &str) -> bool {
    !status.is_empty()
        && !matches!(
            status.to_ascii_lowercase().as_str(),
            "operating" | "open" | "active"
        )
}

fn opt_string_value(value: Option<String>) -> Value {
    value.map(Value::String).unwrap_or(Value::Null)
}

fn safe_path_id(id: &str) -> String {
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

fn feature_count(collection: &Value) -> usize {
    collection
        .get("features")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

fn read_json(path: &Path) -> Result<Value> {
    let file = File::open(path)?;
    Ok(serde_json::from_reader(BufReader::new(file))?)
}

fn write_json_pretty(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn sha256_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

fn file_metadata(layer: &str, path: &Path, source_url: Option<String>) -> Result<Value> {
    let metadata = fs::metadata(path)?;
    Ok(json!({
        "name": layer,
        "path": path,
        "sourceUrl": source_url,
        "sizeBytes": metadata.len(),
        "sha256": sha256_file(path)?,
    }))
}

fn file_manifest_for_dir(path: &Path) -> Result<Value> {
    let mut files = Map::new();
    for entry in WalkDir::new(path).min_depth(1).max_depth(1) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let file_path = entry.path();
        let Some(name) = file_path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name == "artifact_manifest.json" {
            continue;
        }
        let size = fs::metadata(file_path)?.len();
        files.insert(
            name.to_string(),
            json!({"sizeBytes": size, "sha256": sha256_file(file_path)?}),
        );
    }
    Ok(Value::Object(files))
}

fn directory_size(path: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in WalkDir::new(path).min_depth(1) {
        let entry = entry?;
        if entry.file_type().is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
    if target.exists() {
        fs::remove_dir_all(target)?;
    }
    fs::create_dir_all(target)?;
    for entry in WalkDir::new(source).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&destination)?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &destination)?;
        }
    }
    Ok(())
}

fn hard_link_or_copy(source: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    if target.exists() {
        fs::remove_file(target)?;
    }
    match fs::hard_link(source, target) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(source, target)?;
            Ok(())
        }
    }
}

fn create_tar_gz(source_dir: &Path, archive_path: &Path) -> Result<()> {
    let file = File::create(archive_path)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = Builder::new(encoder);
    archive.append_dir_all(".", source_dir)?;
    archive.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
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
    fn build_pipeline_writes_local_app_layout() -> Result<()> {
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

        let output = TempDir::new()?;
        let summary =
            build_from_cache(cache.path(), output.path(), Some("2026-06-03".to_string()))?;
        assert_eq!(summary.resort_count, 1);
        assert!(output.path().join("resorts.json").exists());
        assert!(
            output
                .path()
                .join("packages/resorts/area-1/manifest.json")
                .exists()
        );
        assert!(
            output
                .path()
                .join("local-app/render-bundles/area-1/manifest.json")
                .exists()
        );
        validate_output(output.path())?;
        let resorts_json: Value = read_json(&output.path().join("resorts.json"))?;
        let first_resort = resorts_json
            .get("resorts")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(Value::as_object)
            .expect("first resort object");
        assert!(!first_resort.contains_key("names"));
        assert!(!first_resort.contains_key("artifactManifestPath"));

        let package_dir = output.path().join("packages/resorts/area-1");
        assert!(package_dir.join("artifact_manifest.json").exists());
        assert!(!package_dir.join("checksums.json").exists());
        assert!(!package_dir.join("run_matching_hints.json").exists());
        assert!(!package_dir.join("explore_detail.json").exists());

        let manifest: Value = read_json(&package_dir.join("manifest.json"))?;
        let files = manifest
            .get("files")
            .and_then(Value::as_object)
            .expect("render manifest files");
        assert!(!files.contains_key("runMatchingHints"));
        assert!(!files.contains_key("exploreDetail"));

        let local_render_bundle = output.path().join("local-app/render-bundles/area-1");
        assert!(!local_render_bundle.join("run_matching_hints.json").exists());
        assert!(!local_render_bundle.join("explore_detail.json").exists());
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

    fn domain_and_leaf_resorts(domain_id: &str, leaf_id: &str) -> Vec<ResortRecord> {
        vec![
            test_resort(domain_id, "Domain", "domain", None),
            test_resort(leaf_id, "Leaf", "resort", Some(domain_id)),
        ]
    }

    fn test_resort(
        id: &str,
        name: &str,
        resort_type: &str,
        parent_id: Option<&str>,
    ) -> ResortRecord {
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
}
