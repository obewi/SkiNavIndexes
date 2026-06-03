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
const RENDER_SCHEMA_VERSION: i64 = 23;
const PIPELINE_SCHEMA_VERSION: i64 = 1;

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
        } => fetch_sources(&cache_dir, dataset_version, &source_base_url),
        Command::Build {
            cache_dir,
            output_dir,
            dataset_version,
        } => build_from_cache(&cache_dir, &output_dir, dataset_version).map(|summary| {
            eprintln!(
                "built dataset {}: {} resorts, {} runs, {} lifts",
                summary.dataset_version,
                summary.resort_count,
                summary.run_count,
                summary.lift_count
            );
        }),
        Command::Validate { output_dir } => validate_output(&output_dir),
        Command::All {
            cache_dir,
            output_dir,
            dataset_version,
            source_base_url,
            skip_fetch,
        } => {
            if !skip_fetch {
                fetch_sources(&cache_dir, dataset_version.clone(), &source_base_url)?;
            }
            let summary = build_from_cache(&cache_dir, &output_dir, dataset_version)?;
            eprintln!(
                "built dataset {}: {} resorts, {} runs, {} lifts",
                summary.dataset_version,
                summary.resort_count,
                summary.run_count,
                summary.lift_count
            );
            validate_output(&output_dir)
        }
    }
}

fn fetch_sources(
    cache_dir: &Path,
    dataset_version: Option<String>,
    source_base_url: &str,
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

    let metadata = json!({
        "datasetVersion": dataset_version,
        "fetchedAt": Utc::now(),
        "sourceFormat": "openskimap-geojson",
        "layers": layers,
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

    reset_output_dir(output_dir)?;
    let ski_areas = read_feature_collection(&dataset_dir.join("ski_areas.geojson"))?;
    let runs = read_feature_collection(&dataset_dir.join("runs.geojson"))?;
    let lifts = read_feature_collection(&dataset_dir.join("lifts.geojson"))?;

    let generated_at = Utc::now();
    let normalized = normalize_sources(ski_areas, runs, lifts, &dataset_version, generated_at)?;
    write_outputs(output_dir, &normalized)?;

    Ok(BuildSummary {
        dataset_version,
        resort_count: normalized.resorts.len(),
        run_count: normalized.runs.len(),
        lift_count: normalized.lifts.len(),
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
}

#[derive(Debug)]
struct NormalizedDataset {
    dataset_version: String,
    generated_at: DateTime<Utc>,
    resorts: Vec<ResortRecord>,
    runs: Vec<FeatureRecord>,
    lifts: Vec<FeatureRecord>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ResortRecord {
    id: String,
    name: String,
    names: Vec<String>,
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
    #[serde(rename = "artifactManifestPath")]
    artifact_manifest_path: String,
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

        let mut names = collect_names(&feature.properties, &name);
        if names.is_empty() {
            names.push(name.clone());
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
            names,
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
            artifact_manifest_path: format!(
                "packages/resorts/{}/artifact_manifest.json",
                safe_path_id(&id)
            ),
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

    let active_resort_ids = normalized_runs
        .iter()
        .chain(normalized_lifts.iter())
        .flat_map(|record| record.resort_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    resorts.retain(|resort| active_resort_ids.contains(&resort.id));

    apply_feature_bounds_to_resorts(&mut resorts, &normalized_runs, &normalized_lifts);
    compute_resort_hierarchy(&mut resorts, &normalized_runs, &normalized_lifts);

    if resorts.is_empty() {
        bail!("no operating downhill resorts found; check OpenSkiMap schema and source files");
    }

    Ok(NormalizedDataset {
        dataset_version: dataset_version.to_string(),
        generated_at,
        resorts,
        runs: normalized_runs,
        lifts: normalized_lifts,
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

    write_discovery_index(output_dir, dataset)?;
    write_resort_packages(output_dir, dataset, &runs_by_resort, &lifts_by_resort)?;
    write_group_archives(output_dir, dataset)?;
    write_local_app_layout(output_dir, dataset)?;

    let report = json!({
        "datasetVersion": dataset.dataset_version,
        "generatedAt": dataset.generated_at,
        "schemaVersion": PIPELINE_SCHEMA_VERSION,
        "resortCount": dataset.resorts.len(),
        "runCount": dataset.runs.len(),
        "liftCount": dataset.lifts.len(),
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

        let downhill_lines = line_feature_collection(&runs, resort);
        let downhill_polygons = polygon_feature_collection(&runs, resort);
        let centerlines = centerline_feature_collection(&runs, resort);
        let lifts_geojson = lift_feature_collection(&lifts);
        let lift_stations = lift_station_feature_collection(&lifts);
        let hints = run_matching_hints(&centerlines, &lifts_geojson, dataset.generated_at);
        let explore_detail = empty_explore_detail(dataset.generated_at);
        let audit = audit_report(
            resort,
            runs.len(),
            lifts.len(),
            &centerlines,
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
        write_json_pretty(&package_dir.join("lifts.geojson"), &lifts_geojson)?;
        write_json_pretty(&package_dir.join("lift_stations.geojson"), &lift_stations)?;
        write_json_pretty(&package_dir.join("run_matching_hints.json"), &hints)?;
        write_json_pretty(&package_dir.join("explore_detail.json"), &explore_detail)?;
        write_json_pretty(&package_dir.join("audit_report.json"), &audit)?;

        let checksums = checksums_for_dir(&package_dir)?;
        write_json_pretty(&package_dir.join("checksums.json"), &checksums)?;

        let manifest = app_render_manifest(
            resort,
            dataset.generated_at,
            runs.len(),
            lifts.len(),
            &centerlines,
            &downhill_polygons,
            &lift_stations,
        );
        write_json_pretty(&package_dir.join("manifest.json"), &manifest)?;

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
            "files": checksums,
            "stats": {
                "runs": runs.len(),
                "lifts": lifts.len(),
                "centerlines": feature_count(&centerlines),
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
                "artifactManifestPath": format!("../{}/artifact_manifest.json", safe_path_id(child_id)),
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

    let checksums = checksums_for_dir(package_dir)?;
    write_json_pretty(&package_dir.join("checksums.json"), &checksums)?;

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
        "files": checksums,
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
            "lifts.geojson",
            "lift_stations.geojson",
            "run_matching_hints.json",
            "explore_detail.json",
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
            "lifts.geojson",
            "lift_stations.geojson",
            "run_matching_hints.json",
            "explore_detail.json",
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

fn run_matching_hints(centerlines: &Value, _lifts: &Value, generated_at: DateTime<Utc>) -> Value {
    let features = centerlines
        .get("features")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut source_way_to_family = Map::new();
    let mut endpoints = Map::new();
    let mut endpoint_to_sections: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for feature in &features {
        let Some(props) = feature.get("properties").and_then(Value::as_object) else {
            continue;
        };
        let Some(section_id) = first_string(props, &["completion_section_id", "centerline_id"])
        else {
            continue;
        };
        let family = first_string(props, &["completion_family_key", "run_key"])
            .unwrap_or_else(|| section_id.clone());
        if let Some(source_way) = first_string(props, &["source_way_id"]) {
            source_way_to_family.insert(source_way, Value::String(family));
        }
        let start = first_string(props, &["start_endpoint_key"]);
        let end = first_string(props, &["end_endpoint_key"]);
        if let Some(start) = &start {
            endpoint_to_sections
                .entry(start.clone())
                .or_default()
                .push(section_id.clone());
        }
        if let Some(end) = &end {
            endpoint_to_sections
                .entry(end.clone())
                .or_default()
                .push(section_id.clone());
        }
        endpoints.insert(
            section_id,
            json!({
                "startEndpointKey": start,
                "endEndpointKey": end,
                "lengthMeters": null
            }),
        );
    }

    let mut adjacency: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for section_ids in endpoint_to_sections.values() {
        for left in section_ids {
            for right in section_ids {
                if left != right {
                    adjacency
                        .entry(left.clone())
                        .or_default()
                        .insert(right.clone());
                }
            }
        }
    }
    let adjacency_json = adjacency
        .into_iter()
        .map(|(key, values)| {
            (
                key,
                Value::Array(values.into_iter().map(Value::String).collect()),
            )
        })
        .collect::<Map<_, _>>();
    let junctions = endpoint_to_sections
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(endpoint, ids)| json!({"endpointKey": endpoint, "sectionIDs": ids, "kind": "junction"}))
        .collect::<Vec<_>>();

    json!({
        "schemaVersion": RENDER_SCHEMA_VERSION,
        "generatedAt": generated_at,
        "sourceWayToFamilyKey": source_way_to_family,
        "sectionEndpointByID": endpoints,
        "sectionAdjacencyByID": adjacency_json,
        "branchTopologyLinks": [],
        "sharedSectionRoleByID": {},
        "runStructures": [],
        "junctions": junctions,
        "liftAnchorsByLiftID": {}
    })
}

fn empty_explore_detail(generated_at: DateTime<Utc>) -> Value {
    json!({
        "schemaVersion": RENDER_SCHEMA_VERSION,
        "generatedAt": generated_at,
        "processingConfiguration": {
            "profileSampleSpacingMeters": 25.0,
            "demAccuracyMeters": 30.0,
            "smoothingMethod": "median_then_mean"
        },
        "runsBySelectionKey": {},
        "liftsBySelectionKey": {}
    })
}

fn app_render_manifest(
    resort: &ResortRecord,
    generated_at: DateTime<Utc>,
    run_count: usize,
    lift_count: usize,
    centerlines: &Value,
    polygons: &Value,
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
            "lifts": "lifts.geojson",
            "liftStations": "lift_stations.geojson",
            "runMatchingHints": "run_matching_hints.json",
            "exploreDetail": "explore_detail.json"
        },
        "stats": {
            "downhillLineFeatureCount": run_count,
            "downhillCenterlineFeatureCount": feature_count(centerlines),
            "downhillCenterlineLabeledFeatureCount": null,
            "explicitOnewayCenterlineCount": null,
            "inferredOnewayCenterlineCount": null,
            "unknownDirectionCenterlineCount": null,
            "downhillPolygonFeatureCount": feature_count(polygons),
            "liftFeatureCount": lift_count,
            "liftStationFeatureCount": feature_count(lift_stations)
        }
    })
}

fn audit_report(
    resort: &ResortRecord,
    run_count: usize,
    lift_count: usize,
    centerlines: &Value,
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
            "centerlines": feature_count(centerlines),
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

fn collect_names(props: &Map<String, Value>, primary: &str) -> Vec<String> {
    let mut names = BTreeSet::new();
    names.insert(primary.to_string());
    for key in [
        "altName",
        "alt_name",
        "localName",
        "loc_name",
        "shortName",
        "short_name",
    ] {
        if let Some(value) = props.get(key) {
            collect_string_values(value, &mut names);
        }
    }
    if let Some(localized) = props.get("localized").or_else(|| props.get("names")) {
        collect_string_values(localized, &mut names);
    }
    names
        .into_iter()
        .filter(|name| !name.trim().is_empty())
        .collect()
}

fn collect_string_values(value: &Value, names: &mut BTreeSet<String>) {
    match value {
        Value::String(text) => {
            for part in text.split(';') {
                let trimmed = part.trim();
                if !trimmed.is_empty() {
                    names.insert(trimmed.to_string());
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_string_values(item, names);
            }
        }
        Value::Object(object) => {
            for value in object.values() {
                collect_string_values(value, names);
            }
        }
        _ => {}
    }
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
    let center_lat = (bbox[1] + bbox[3]) / 2.0;
    let lat_delta = 500.0 / 111_320.0;
    let lon_delta = 500.0 / (111_320.0 * center_lat.to_radians().cos().abs().max(0.1));
    [
        bbox[0] - lon_delta,
        bbox[1] - lat_delta,
        bbox[2] + lon_delta,
        bbox[3] + lat_delta,
    ]
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

fn checksums_for_dir(path: &Path) -> Result<Value> {
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
        if name == "checksums.json" {
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
        Ok(())
    }
}
