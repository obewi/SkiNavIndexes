use super::*;
use flate2::read::GzDecoder;
use flate2::{Compression, write::GzEncoder};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufReader},
};

const SOURCE_SCHEMA_VERSION: i64 = 2;
// The planner estimates compact row payloads; SQLite pages and indexes add roughly
// 25% on disk, so this target keeps packs useful without leaving a tiny tail pack.
const SOURCE_PACK_TARGET_BYTES: u64 = 28 * 1024 * 1024;

#[derive(Clone, Debug)]
struct SourcePackPlan {
    pack_id: String,
    resort_ids: Vec<String>,
}

#[derive(Clone, Debug)]
struct PackArtifact {
    pack_id: String,
    asset: String,
    compressed_bytes: u64,
    uncompressed_bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct OwnershipKey {
    feature_kind: &'static str,
    feature_id: String,
    resort_id: String,
}

#[derive(Debug)]
struct FeatureValues {
    source_way_id: Option<String>,
    name: Option<String>,
    reference: Option<String>,
    difficulty: Option<String>,
    piste_type: Option<String>,
    grooming: Option<String>,
    status: Option<String>,
    oneway: Option<String>,
    tunnel: i64,
    gladed: i64,
    lit: i64,
    geometry_wkb: Vec<u8>,
    elevation_profile: Option<Vec<u8>>,
    elevation_resolution: Option<f64>,
    elevation_target_resolution: Option<f64>,
    properties_json: String,
}

pub(super) fn write_source_outputs(output_dir: &Path, dataset: &NormalizedDataset) -> Result<()> {
    fs::create_dir_all(output_dir)?;
    let staging = output_dir.join(".staging");
    if staging.exists() {
        fs::remove_dir_all(&staging).with_context(|| format!("clearing {}", staging.display()))?;
    }
    fs::create_dir_all(&staging)?;

    let plans = plan_source_packs(dataset)?;
    let mut pack_artifacts = Vec::with_capacity(plans.len());
    for plan in &plans {
        let sqlite_path = staging.join(format!("{}.sqlite", plan.pack_id));
        write_source_pack(&sqlite_path, dataset, plan)?;

        let asset = format!("{}.sqlite.gz", plan.pack_id);
        let compressed_path = output_dir.join(&asset);
        gzip_file(&sqlite_path, &compressed_path)?;
        pack_artifacts.push(PackArtifact {
            pack_id: plan.pack_id.clone(),
            asset,
            compressed_bytes: fs::metadata(&compressed_path)?.len(),
            uncompressed_bytes: fs::metadata(&sqlite_path)?.len(),
            sha256: sha256_file(&compressed_path)?,
        });
    }

    let catalog_path = staging.join("catalog.sqlite");
    write_catalog(&catalog_path, dataset, &plans, &pack_artifacts)?;
    let catalog_asset = "catalog.sqlite.gz";
    let catalog_compressed_path = output_dir.join(catalog_asset);
    gzip_file(&catalog_path, &catalog_compressed_path)?;
    let catalog_artifact = PackArtifact {
        pack_id: "catalog".to_string(),
        asset: catalog_asset.to_string(),
        compressed_bytes: fs::metadata(&catalog_compressed_path)?.len(),
        uncompressed_bytes: fs::metadata(&catalog_path)?.len(),
        sha256: sha256_file(&catalog_compressed_path)?,
    };

    validate_source_release(
        output_dir,
        &staging,
        dataset,
        &plans,
        &pack_artifacts,
        &catalog_artifact,
    )?;

    let latest = json!({
        "schemaVersion": SOURCE_SCHEMA_VERSION,
        "datasetVersion": dataset.dataset_version,
        "catalog": {
            "asset": catalog_artifact.asset,
            "compressedBytes": catalog_artifact.compressed_bytes,
            "uncompressedBytes": catalog_artifact.uncompressed_bytes,
            "sha256": catalog_artifact.sha256
        }
    });
    write_json_pretty(&output_dir.join("latest.json"), &latest)?;
    fs::remove_dir_all(&staging)?;
    Ok(())
}

fn plan_source_packs(dataset: &NormalizedDataset) -> Result<Vec<SourcePackPlan>> {
    let mut by_group: BTreeMap<&str, Vec<&ResortRecord>> = BTreeMap::new();
    for resort in &dataset.resorts {
        by_group
            .entry(resort.pack_group_hint.as_str())
            .or_default()
            .push(resort);
    }

    let mut plans = Vec::new();
    let mut current_resort_ids = Vec::new();
    let mut current_size = 0_u64;
    for resorts in by_group.values() {
        for resort in resorts {
            let estimate = estimate_resort_source_bytes(dataset, resort)?;
            if !current_resort_ids.is_empty()
                && current_size.saturating_add(estimate) > SOURCE_PACK_TARGET_BYTES
            {
                plans.push(SourcePackPlan {
                    pack_id: format!("pack-{:04}", plans.len() + 1),
                    resort_ids: std::mem::take(&mut current_resort_ids),
                });
                current_size = 0;
            }
            current_resort_ids.push(resort.id.clone());
            current_size = current_size.saturating_add(estimate);
        }
    }
    if !current_resort_ids.is_empty() {
        plans.push(SourcePackPlan {
            pack_id: format!("pack-{:04}", plans.len() + 1),
            resort_ids: current_resort_ids,
        });
    }
    if plans.is_empty() {
        bail!("cannot generate SQLite source packs without resorts");
    }
    Ok(plans)
}

fn estimate_resort_source_bytes(dataset: &NormalizedDataset, resort: &ResortRecord) -> Result<u64> {
    let mut size = 2048_u64;
    for record in dataset
        .runs
        .iter()
        .chain(dataset.lifts.iter())
        .chain(dataset.spots.iter())
        .chain(dataset.connections.iter())
        .filter(|record| record.resort_ids.iter().any(|id| id == &resort.id))
    {
        size = size
            .saturating_add(geometry_to_wkb(&record.geometry)?.len() as u64)
            .saturating_add(
                serde_json::to_vec(&stored_properties(&record.properties))?.len() as u64,
            )
            .saturating_add(256);
    }
    Ok(size)
}

fn write_source_pack(
    path: &Path,
    dataset: &NormalizedDataset,
    plan: &SourcePackPlan,
) -> Result<()> {
    let mut connection = Connection::open(path)?;
    configure_database(&connection)?;
    create_source_schema(&connection)?;
    let transaction = connection.transaction()?;
    insert_metadata(
        &transaction,
        &[
            ("schemaVersion", SOURCE_SCHEMA_VERSION.to_string()),
            ("datasetVersion", dataset.dataset_version.clone()),
            ("packId", plan.pack_id.clone()),
            ("geometryEncoding", "wkb-little-endian".to_string()),
            ("elevationProfileEncoding", "f64le".to_string()),
        ],
    )?;

    let resort_ids = plan.resort_ids.iter().collect::<BTreeSet<_>>();
    for record in &dataset.runs {
        if record.resort_ids.iter().any(|id| resort_ids.contains(id)) {
            insert_run(&transaction, record)?;
        }
    }
    for record in &dataset.lifts {
        if record.resort_ids.iter().any(|id| resort_ids.contains(id)) {
            insert_lift(&transaction, record)?;
        }
    }
    for record in &dataset.spots {
        if record.resort_ids.iter().any(|id| resort_ids.contains(id)) {
            insert_spot(&transaction, record)?;
        }
    }
    for record in &dataset.connections {
        if record.resort_ids.iter().any(|id| resort_ids.contains(id)) {
            insert_connection(&transaction, record)?;
        }
    }
    transaction.commit()?;
    Ok(())
}

fn configure_database(connection: &Connection) -> Result<()> {
    connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")?;
    let journal_mode: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    if journal_mode.eq_ignore_ascii_case("wal") {
        bail!("SQLite source database unexpectedly uses WAL mode");
    }
    Ok(())
}

fn create_source_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
        CREATE TABLE metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE runs (
            id TEXT PRIMARY KEY,
            source_way_id TEXT,
            name TEXT,
            ref TEXT,
            difficulty TEXT,
            piste_type TEXT,
            grooming TEXT,
            status TEXT,
            oneway TEXT,
            tunnel INTEGER NOT NULL,
            gladed INTEGER NOT NULL,
            lit INTEGER NOT NULL,
            geometry_wkb BLOB NOT NULL,
            elevation_profile BLOB,
            elevation_resolution REAL,
            elevation_target_resolution REAL,
            properties_json TEXT NOT NULL
        );

        CREATE TABLE lifts (
            id TEXT PRIMARY KEY,
            source_way_id TEXT,
            name TEXT,
            ref TEXT,
            lift_type TEXT,
            status TEXT,
            oneway TEXT,
            tunnel INTEGER NOT NULL,
            gladed INTEGER NOT NULL,
            lit INTEGER NOT NULL,
            geometry_wkb BLOB NOT NULL,
            properties_json TEXT NOT NULL
        );

        CREATE TABLE spots (
            id TEXT PRIMARY KEY,
            source_way_id TEXT,
            name TEXT,
            spot_type TEXT,
            dismount TEXT,
            geometry_wkb BLOB NOT NULL,
            properties_json TEXT NOT NULL
        );

        CREATE TABLE connections (
            id TEXT PRIMARY KEY,
            source_way_id TEXT,
            name TEXT,
            ref TEXT,
            piste_type TEXT,
            status TEXT,
            oneway TEXT,
            tunnel INTEGER NOT NULL,
            gladed INTEGER NOT NULL,
            lit INTEGER NOT NULL,
            geometry_wkb BLOB NOT NULL,
            properties_json TEXT NOT NULL
        );

        CREATE TABLE run_resorts (
            run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
            resort_id TEXT NOT NULL,
            PRIMARY KEY (run_id, resort_id)
        );
        CREATE TABLE lift_resorts (
            lift_id TEXT NOT NULL REFERENCES lifts(id) ON DELETE CASCADE,
            resort_id TEXT NOT NULL,
            PRIMARY KEY (lift_id, resort_id)
        );
        CREATE TABLE spot_resorts (
            spot_id TEXT NOT NULL REFERENCES spots(id) ON DELETE CASCADE,
            resort_id TEXT NOT NULL,
            PRIMARY KEY (spot_id, resort_id)
        );
        CREATE TABLE connection_resorts (
            connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
            resort_id TEXT NOT NULL,
            PRIMARY KEY (connection_id, resort_id)
        );

        CREATE INDEX run_resorts_resort_idx ON run_resorts(resort_id);
        CREATE INDEX lift_resorts_resort_idx ON lift_resorts(resort_id);
        CREATE INDEX spot_resorts_resort_idx ON spot_resorts(resort_id);
        CREATE INDEX connection_resorts_resort_idx ON connection_resorts(resort_id);
        "#,
    )?;
    Ok(())
}

fn insert_run(transaction: &Transaction<'_>, record: &FeatureRecord) -> Result<()> {
    let values = feature_values(record)?;
    transaction.execute(
        "INSERT OR IGNORE INTO runs (id, source_way_id, name, ref, difficulty, piste_type, grooming, status, oneway, tunnel, gladed, lit, geometry_wkb, elevation_profile, elevation_resolution, elevation_target_resolution, properties_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            record.id,
            values.source_way_id.as_deref(),
            values.name.as_deref(),
            values.reference.as_deref(),
            values.difficulty.as_deref(),
            values.piste_type.as_deref(),
            values.grooming.as_deref(),
            values.status.as_deref(),
            values.oneway.as_deref(),
            values.tunnel,
            values.gladed,
            values.lit,
            values.geometry_wkb,
            values.elevation_profile,
            values.elevation_resolution,
            values.elevation_target_resolution,
            values.properties_json,
        ],
    )?;
    verify_feature_payload(transaction, "runs", &record.id, &values)?;
    insert_ownership(
        transaction,
        "run_resorts",
        "run_id",
        &record.id,
        &record.resort_ids,
    )
}

fn insert_lift(transaction: &Transaction<'_>, record: &FeatureRecord) -> Result<()> {
    let values = feature_values(record)?;
    let lift_type = property_string(&record.properties, &["liftType", "lift_type"]);
    transaction.execute(
        "INSERT OR IGNORE INTO lifts (id, source_way_id, name, ref, lift_type, status, oneway, tunnel, gladed, lit, geometry_wkb, properties_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            record.id,
            values.source_way_id.as_deref(),
            values.name.as_deref(),
            values.reference.as_deref(),
            lift_type.as_deref(),
            values.status.as_deref(),
            values.oneway.as_deref(),
            values.tunnel,
            values.gladed,
            values.lit,
            values.geometry_wkb,
            values.properties_json,
        ],
    )?;
    verify_feature_payload(transaction, "lifts", &record.id, &values)?;
    insert_ownership(
        transaction,
        "lift_resorts",
        "lift_id",
        &record.id,
        &record.resort_ids,
    )
}

fn insert_spot(transaction: &Transaction<'_>, record: &FeatureRecord) -> Result<()> {
    let values = feature_values(record)?;
    let spot_type = property_string(&record.properties, &["spotType", "spot_type"]);
    let dismount = property_string(&record.properties, &["dismount"]);
    transaction.execute(
        "INSERT OR IGNORE INTO spots (id, source_way_id, name, spot_type, dismount, geometry_wkb, properties_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            record.id,
            values.source_way_id.as_deref(),
            values.name.as_deref(),
            spot_type.as_deref(),
            dismount.as_deref(),
            values.geometry_wkb,
            values.properties_json,
        ],
    )?;
    verify_feature_payload(transaction, "spots", &record.id, &values)?;
    insert_ownership(
        transaction,
        "spot_resorts",
        "spot_id",
        &record.id,
        &record.resort_ids,
    )
}

fn insert_connection(transaction: &Transaction<'_>, record: &FeatureRecord) -> Result<()> {
    let values = feature_values(record)?;
    transaction.execute(
        "INSERT OR IGNORE INTO connections (id, source_way_id, name, ref, piste_type, status, oneway, tunnel, gladed, lit, geometry_wkb, properties_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            record.id,
            values.source_way_id.as_deref(),
            values.name.as_deref(),
            values.reference.as_deref(),
            values.piste_type.as_deref(),
            values.status.as_deref(),
            values.oneway.as_deref(),
            values.tunnel,
            values.gladed,
            values.lit,
            values.geometry_wkb,
            values.properties_json,
        ],
    )?;
    verify_feature_payload(transaction, "connections", &record.id, &values)?;
    insert_ownership(
        transaction,
        "connection_resorts",
        "connection_id",
        &record.id,
        &record.resort_ids,
    )
}

fn verify_feature_payload(
    transaction: &Transaction<'_>,
    table: &str,
    id: &str,
    values: &FeatureValues,
) -> Result<()> {
    let sql = format!("SELECT geometry_wkb, properties_json FROM {table} WHERE id = ?1");
    let (geometry_wkb, properties_json): (Vec<u8>, String) =
        transaction.query_row(&sql, params![id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    if geometry_wkb != values.geometry_wkb {
        bail!("conflicting geometry for {table} feature {id}");
    }
    let existing_properties: Value = serde_json::from_str(&properties_json)?;
    let current_properties: Value = serde_json::from_str(&values.properties_json)?;
    if existing_properties != current_properties {
        bail!("conflicting properties for {table} feature {id}");
    }
    Ok(())
}

fn insert_ownership(
    transaction: &Transaction<'_>,
    table: &str,
    id_column: &str,
    feature_id: &str,
    resort_ids: &[String],
) -> Result<()> {
    let sql = format!("INSERT OR IGNORE INTO {table} ({id_column}, resort_id) VALUES (?1, ?2)");
    for resort_id in resort_ids {
        transaction.execute(&sql, params![feature_id, resort_id])?;
    }
    Ok(())
}

fn feature_values(record: &FeatureRecord) -> Result<FeatureValues> {
    let (elevation_profile, elevation_resolution, elevation_target_resolution) =
        elevation_profile_values(&record.properties)?;
    Ok(FeatureValues {
        source_way_id: source_way_id(&record.properties),
        name: property_string(&record.properties, &["name", "title"]),
        reference: property_string(&record.properties, &["ref"]),
        difficulty: property_string(&record.properties, &["difficulty"]),
        piste_type: property_string(&record.properties, &["piste:type", "pisteType"]),
        grooming: property_string(&record.properties, &["grooming"]),
        status: property_string(&record.properties, &["status"]),
        oneway: property_string(&record.properties, &["oneway", "oneWay"]),
        tunnel: property_flag(&record.properties, "tunnel"),
        gladed: property_flag(&record.properties, "gladed"),
        lit: property_flag(&record.properties, "lit"),
        geometry_wkb: geometry_to_wkb(&record.geometry)?,
        elevation_profile,
        elevation_resolution,
        elevation_target_resolution,
        properties_json: serde_json::to_string(&stored_properties(&record.properties))?,
    })
}

fn source_way_id(properties: &Map<String, Value>) -> Option<String> {
    property_string(properties, &["sourceWayId", "source_way_id"])
        .or_else(|| source_keys_from_properties(properties).into_iter().next())
}

fn property_string(properties: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| properties.get(*key).and_then(value_to_string))
}

fn property_flag(properties: &Map<String, Value>, key: &str) -> i64 {
    match properties.get(key) {
        Some(Value::Bool(value)) => i64::from(*value),
        Some(Value::Number(value)) => i64::from(value.as_i64().unwrap_or_default() != 0),
        Some(Value::String(value)) => i64::from(matches!(
            value.to_ascii_lowercase().as_str(),
            "yes" | "true" | "1"
        )),
        _ => 0,
    }
}

pub(super) fn stored_properties(properties: &Map<String, Value>) -> Map<String, Value> {
    properties
        .iter()
        .filter_map(|(key, value)| {
            (!is_assignment_property(key)).then(|| (key.clone(), stored_value(value)))
        })
        .collect()
}

fn stored_value(value: &Value) -> Value {
    match value {
        Value::Object(properties) => Value::Object(
            properties
                .iter()
                .filter_map(|(key, value)| {
                    (!is_assignment_property(key)).then(|| (key.clone(), stored_value(value)))
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(stored_value).collect()),
        _ => value.clone(),
    }
}

fn is_assignment_property(key: &str) -> bool {
    matches!(key, "skiAreas" | "skiAreaIds" | "ski_area_ids" | "ski_area")
}

fn elevation_profile_values(
    properties: &Map<String, Value>,
) -> Result<(Option<Vec<u8>>, Option<f64>, Option<f64>)> {
    let Some(profile) = properties
        .get("elevationProfile")
        .and_then(Value::as_object)
    else {
        return Ok((None, None, None));
    };
    let Some(heights) = profile.get("heights").and_then(Value::as_array) else {
        return Ok((None, None, None));
    };
    let mut bytes = Vec::with_capacity(heights.len() * 8);
    for height in heights {
        let height = height
            .as_f64()
            .ok_or_else(|| anyhow!("elevationProfile heights must be numeric"))?;
        bytes.extend_from_slice(&height.to_le_bytes());
    }
    Ok((
        Some(bytes),
        profile.get("resolution").and_then(Value::as_f64),
        profile.get("targetResolution").and_then(Value::as_f64),
    ))
}

fn write_catalog(
    path: &Path,
    dataset: &NormalizedDataset,
    plans: &[SourcePackPlan],
    pack_artifacts: &[PackArtifact],
) -> Result<()> {
    let mut connection = Connection::open(path)?;
    configure_database(&connection)?;
    create_catalog_schema(&connection)?;
    let transaction = connection.transaction()?;
    insert_metadata(
        &transaction,
        &[
            ("schemaVersion", SOURCE_SCHEMA_VERSION.to_string()),
            ("datasetVersion", dataset.dataset_version.clone()),
            ("generatedAt", dataset.generated_at.to_rfc3339()),
            ("packCount", plans.len().to_string()),
        ],
    )?;

    for resort in &dataset.resorts {
        transaction.execute(
            "INSERT INTO resorts (id, name, parent_id, bbox_west, bbox_south, bbox_east, bbox_north, center_lon, center_lat, country, run_convention) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                resort.id,
                resort.name,
                resort.parent_id,
                resort.bbox[0],
                resort.bbox[1],
                resort.bbox[2],
                resort.bbox[3],
                resort.center[0],
                resort.center[1],
                resort.country,
                resort.run_convention,
            ],
        )?;
        for iso_code in &resort.iso_codes {
            transaction.execute(
                "INSERT INTO resort_iso_codes (resort_id, iso_code) VALUES (?1, ?2)",
                params![resort.id, iso_code],
            )?;
        }
        transaction.execute(
            "INSERT INTO resort_source_stats (resort_id, run_count, lift_count, spot_count, connection_count) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                resort.id,
                count_owned(&dataset.runs, &resort.id) as i64,
                count_owned(&dataset.lifts, &resort.id) as i64,
                count_owned(&dataset.spots, &resort.id) as i64,
                count_owned(&dataset.connections, &resort.id) as i64,
            ],
        )?;
    }

    for artifact in pack_artifacts {
        transaction.execute(
            "INSERT INTO packs (pack_id, asset, compressed_bytes, uncompressed_bytes, sha256) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                artifact.pack_id,
                artifact.asset,
                artifact.compressed_bytes as i64,
                artifact.uncompressed_bytes as i64,
                artifact.sha256,
            ],
        )?;
    }
    for plan in plans {
        for resort_id in &plan.resort_ids {
            transaction.execute(
                "INSERT INTO resort_packs (resort_id, pack_id) VALUES (?1, ?2)",
                params![resort_id, plan.pack_id],
            )?;
        }
    }
    transaction.commit()?;
    Ok(())
}

fn create_catalog_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        r#"
        CREATE TABLE metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE resorts (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            parent_id TEXT REFERENCES resorts(id) DEFERRABLE INITIALLY DEFERRED,
            bbox_west REAL NOT NULL,
            bbox_south REAL NOT NULL,
            bbox_east REAL NOT NULL,
            bbox_north REAL NOT NULL,
            center_lon REAL NOT NULL,
            center_lat REAL NOT NULL,
            country TEXT,
            run_convention TEXT
        );
        CREATE TABLE resort_iso_codes (
            resort_id TEXT NOT NULL REFERENCES resorts(id) ON DELETE CASCADE,
            iso_code TEXT NOT NULL,
            PRIMARY KEY (resort_id, iso_code)
        );
        CREATE TABLE packs (
            pack_id TEXT PRIMARY KEY,
            asset TEXT NOT NULL UNIQUE,
            compressed_bytes INTEGER NOT NULL,
            uncompressed_bytes INTEGER NOT NULL,
            sha256 TEXT NOT NULL
        );
        CREATE TABLE resort_packs (
            resort_id TEXT NOT NULL REFERENCES resorts(id) ON DELETE CASCADE,
            pack_id TEXT NOT NULL REFERENCES packs(pack_id) ON DELETE CASCADE,
            PRIMARY KEY (resort_id, pack_id)
        );
        CREATE TABLE resort_source_stats (
            resort_id TEXT PRIMARY KEY REFERENCES resorts(id) ON DELETE CASCADE,
            run_count INTEGER NOT NULL,
            lift_count INTEGER NOT NULL,
            spot_count INTEGER NOT NULL,
            connection_count INTEGER NOT NULL
        );
        CREATE INDEX resorts_parent_idx ON resorts(parent_id);
        CREATE INDEX resort_packs_pack_idx ON resort_packs(pack_id);
        "#,
    )?;
    Ok(())
}

fn insert_metadata(transaction: &Transaction<'_>, entries: &[(&str, String)]) -> Result<()> {
    for (key, value) in entries {
        transaction.execute(
            "INSERT INTO metadata (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
    }
    Ok(())
}

fn count_owned(records: &[FeatureRecord], resort_id: &str) -> usize {
    records
        .iter()
        .filter(|record| record.resort_ids.iter().any(|id| id == resort_id))
        .count()
}

fn validate_source_release(
    output_dir: &Path,
    staging: &Path,
    dataset: &NormalizedDataset,
    plans: &[SourcePackPlan],
    pack_artifacts: &[PackArtifact],
    catalog_artifact: &PackArtifact,
) -> Result<()> {
    let catalog_path = staging.join("catalog.sqlite");
    validate_database(&catalog_path)?;
    let catalog = Connection::open(&catalog_path)?;
    configure_database(&catalog)?;
    if metadata_value(&catalog, "schemaVersion")?.as_deref()
        != Some(&SOURCE_SCHEMA_VERSION.to_string())
    {
        bail!("catalog schemaVersion mismatch");
    }
    if metadata_value(&catalog, "datasetVersion")?.as_deref()
        != Some(dataset.dataset_version.as_str())
    {
        bail!("catalog datasetVersion mismatch");
    }

    let expected_pack_ids = plans
        .iter()
        .map(|plan| plan.pack_id.as_str())
        .collect::<BTreeSet<_>>();
    let actual_pack_ids = catalog
        .prepare("SELECT pack_id FROM packs ORDER BY pack_id")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    if actual_pack_ids
        != expected_pack_ids
            .iter()
            .map(|pack_id| (*pack_id).to_string())
            .collect::<BTreeSet<_>>()
    {
        bail!("catalog pack set does not match generated source packs");
    }

    for resort in &dataset.resorts {
        let count: i64 = catalog.query_row(
            "SELECT COUNT(*) FROM resort_packs WHERE resort_id = ?1",
            params![resort.id],
            |row| row.get(0),
        )?;
        if count == 0 {
            bail!("resort {} has no catalog pack reference", resort.id);
        }
    }

    for (plan, artifact) in plans.iter().zip(pack_artifacts) {
        let path = staging.join(format!("{}.sqlite", plan.pack_id));
        validate_database(&path)?;
        let pack = Connection::open(&path)?;
        configure_database(&pack)?;
        if metadata_value(&pack, "schemaVersion")?.as_deref()
            != Some(&SOURCE_SCHEMA_VERSION.to_string())
        {
            bail!("{} schemaVersion mismatch", plan.pack_id);
        }
        if metadata_value(&pack, "datasetVersion")?.as_deref()
            != Some(dataset.dataset_version.as_str())
        {
            bail!("{} datasetVersion mismatch", plan.pack_id);
        }
        if metadata_value(&pack, "packId")?.as_deref() != Some(plan.pack_id.as_str()) {
            bail!("{} packId mismatch", plan.pack_id);
        }

        let compressed_path = output_dir.join(&artifact.asset);
        if fs::metadata(&compressed_path)?.len() != artifact.compressed_bytes
            || sha256_file(&compressed_path)? != artifact.sha256
        {
            bail!("{} compressed asset metadata mismatch", artifact.asset);
        }
    }
    let catalog_compressed_path = output_dir.join(&catalog_artifact.asset);
    if fs::metadata(&catalog_compressed_path)?.len() != catalog_artifact.compressed_bytes
        || sha256_file(&catalog_compressed_path)? != catalog_artifact.sha256
    {
        bail!("catalog compressed asset metadata mismatch");
    }

    let expected = expected_ownership(dataset);
    let mut actual = BTreeSet::new();
    for plan in plans {
        let path = staging.join(format!("{}.sqlite", plan.pack_id));
        let connection = Connection::open(path)?;
        collect_pack_ownership(&connection, &mut actual)?;
    }
    if actual != expected {
        bail!(
            "SQLite source ownership set mismatch: expected {}, actual {}",
            expected.len(),
            actual.len()
        );
    }
    Ok(())
}

pub(super) fn validate_source_output(output_dir: &Path) -> Result<()> {
    if !output_dir.is_dir() {
        bail!("missing source output directory {}", output_dir.display());
    }
    for entry in fs::read_dir(output_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let allowed = name == "latest.json"
            || name == "catalog.sqlite.gz"
            || (name.starts_with("pack-") && name.ends_with(".sqlite.gz"));
        if !allowed {
            bail!("unexpected file in canonical source output: {name}");
        }
    }

    let latest: Value = read_json(&output_dir.join("latest.json"))?;
    if latest.get("schemaVersion").and_then(Value::as_i64) != Some(SOURCE_SCHEMA_VERSION) {
        bail!("source latest.json schemaVersion mismatch");
    }
    let dataset_version = latest
        .get("datasetVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("source latest.json missing datasetVersion"))?;
    let catalog_metadata = latest
        .get("catalog")
        .ok_or_else(|| anyhow!("source latest.json missing catalog metadata"))?;
    let catalog_asset = catalog_metadata
        .get("asset")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("source latest.json missing catalog asset"))?;
    if catalog_asset != "catalog.sqlite.gz" {
        bail!("source latest.json references non-canonical catalog asset {catalog_asset}");
    }
    let catalog_compressed_path = asset_path(output_dir, catalog_asset)?;
    let catalog_hash = catalog_metadata
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("source latest.json missing catalog sha256"))?;
    if sha256_file(&catalog_compressed_path)? != catalog_hash {
        bail!("source catalog sha256 mismatch");
    }

    let validation_dir = output_dir.join(format!(".validate-{}", std::process::id()));
    if validation_dir.exists() {
        fs::remove_dir_all(&validation_dir)?;
    }
    fs::create_dir_all(&validation_dir)?;
    let catalog_path = validation_dir.join("catalog.sqlite");
    gunzip_file(&catalog_compressed_path, &catalog_path)?;
    validate_database(&catalog_path)?;
    let catalog = Connection::open(&catalog_path)?;
    configure_database(&catalog)?;
    if metadata_value(&catalog, "schemaVersion")?.as_deref()
        != Some(&SOURCE_SCHEMA_VERSION.to_string())
    {
        bail!("source catalog schemaVersion mismatch");
    }
    if metadata_value(&catalog, "datasetVersion")?.as_deref() != Some(dataset_version) {
        bail!("source catalog datasetVersion mismatch");
    }

    let catalog_uncompressed_bytes = fs::metadata(&catalog_path)?.len();
    if catalog_metadata
        .get("compressedBytes")
        .and_then(Value::as_u64)
        != Some(fs::metadata(&catalog_compressed_path)?.len())
        || catalog_metadata
            .get("uncompressedBytes")
            .and_then(Value::as_u64)
            != Some(catalog_uncompressed_bytes)
    {
        bail!("source catalog byte metadata mismatch");
    }

    let mut statement = catalog.prepare(
        "SELECT pack_id, asset, compressed_bytes, uncompressed_bytes, sha256 FROM packs ORDER BY pack_id",
    )?;
    let pack_rows = statement
        .query_map([], |row| {
            Ok(PackArtifact {
                pack_id: row.get(0)?,
                asset: row.get(1)?,
                compressed_bytes: row.get::<_, i64>(2)? as u64,
                uncompressed_bytes: row.get::<_, i64>(3)? as u64,
                sha256: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if pack_rows.is_empty() {
        bail!("source catalog contains no source packs");
    }

    for artifact in &pack_rows {
        let compressed_path = asset_path(output_dir, &artifact.asset)?;
        if !artifact.asset.starts_with("pack-") || !artifact.asset.ends_with(".sqlite.gz") {
            bail!(
                "source catalog references non-canonical pack asset {}",
                artifact.asset
            );
        }
        if fs::metadata(&compressed_path)?.len() != artifact.compressed_bytes
            || sha256_file(&compressed_path)? != artifact.sha256
        {
            bail!("source pack {} asset metadata mismatch", artifact.pack_id);
        }
        let sqlite_path = validation_dir.join(format!("{}.sqlite", artifact.pack_id));
        gunzip_file(&compressed_path, &sqlite_path)?;
        if fs::metadata(&sqlite_path)?.len() != artifact.uncompressed_bytes {
            bail!(
                "source pack {} uncompressed byte metadata mismatch",
                artifact.pack_id
            );
        }
        validate_database(&sqlite_path)?;
        let pack = Connection::open(&sqlite_path)?;
        configure_database(&pack)?;
        if metadata_value(&pack, "schemaVersion")?.as_deref()
            != Some(&SOURCE_SCHEMA_VERSION.to_string())
            || metadata_value(&pack, "datasetVersion")?.as_deref() != Some(dataset_version)
            || metadata_value(&pack, "packId")?.as_deref() != Some(artifact.pack_id.as_str())
        {
            bail!("source pack {} metadata mismatch", artifact.pack_id);
        }
        validate_join_completeness(&pack)?;
    }

    validate_catalog_hierarchy(&catalog)?;
    let resort_count: i64 =
        catalog.query_row("SELECT COUNT(*) FROM resorts", [], |row| row.get(0))?;
    let stats_count: i64 =
        catalog.query_row("SELECT COUNT(*) FROM resort_source_stats", [], |row| {
            row.get(0)
        })?;
    if resort_count != stats_count {
        bail!("source resort_source_stats does not cover every resort");
    }
    fs::remove_dir_all(&validation_dir)?;
    Ok(())
}

fn asset_path(output_dir: &Path, asset: &str) -> Result<PathBuf> {
    let path = Path::new(asset);
    if path.components().count() != 1
        || path.file_name().and_then(|name| name.to_str()) != Some(asset)
    {
        bail!("invalid source asset path {asset}");
    }
    let path = output_dir.join(path);
    if !path.exists() {
        bail!("missing source asset {}", path.display());
    }
    Ok(path)
}

fn gunzip_file(source: &Path, destination: &Path) -> Result<()> {
    let input = File::open(source).with_context(|| format!("opening {}", source.display()))?;
    let mut decoder = GzDecoder::new(input);
    let mut output =
        File::create(destination).with_context(|| format!("creating {}", destination.display()))?;
    io::copy(&mut decoder, &mut output)?;
    output.sync_all()?;
    Ok(())
}

fn validate_join_completeness(connection: &Connection) -> Result<()> {
    for (table, join_table, id_column) in [
        ("runs", "run_resorts", "run_id"),
        ("lifts", "lift_resorts", "lift_id"),
        ("spots", "spot_resorts", "spot_id"),
        ("connections", "connection_resorts", "connection_id"),
    ] {
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM {table} feature LEFT JOIN {join_table} ownership ON ownership.{id_column} = feature.id WHERE ownership.{id_column} IS NULL)"
        );
        let has_orphan: i64 = connection.query_row(&sql, [], |row| row.get(0))?;
        if has_orphan != 0 {
            bail!("V2 source pack {table} contains an unowned feature");
        }
    }
    Ok(())
}

fn validate_catalog_hierarchy(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("SELECT id, parent_id FROM resorts")?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let ids = rows
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<BTreeSet<_>>();
    for (id, parent_id) in &rows {
        if let Some(parent_id) = parent_id {
            if parent_id == id || !ids.contains(parent_id.as_str()) {
                bail!("V2 catalog has invalid parent for resort {id}");
            }
        }
        let mut path = BTreeSet::new();
        let mut current = Some(id.as_str());
        while let Some(current_id) = current {
            if !path.insert(current_id) {
                bail!("V2 catalog hierarchy contains a cycle at {current_id}");
            }
            current = rows
                .iter()
                .find(|(candidate_id, _)| candidate_id == current_id)
                .and_then(|(_, parent)| parent.as_deref());
        }
    }
    Ok(())
}

fn validate_database(path: &Path) -> Result<()> {
    let connection = Connection::open(path)?;
    configure_database(&connection)?;
    let quick_check: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if quick_check != "ok" {
        bail!(
            "{} failed PRAGMA quick_check: {quick_check}",
            path.display()
        );
    }
    let integrity_check: String =
        connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity_check != "ok" {
        bail!(
            "{} failed PRAGMA integrity_check: {integrity_check}",
            path.display()
        );
    }
    let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
    let mut rows = statement.query([])?;
    if rows.next()?.is_some() {
        bail!("{} failed PRAGMA foreign_key_check", path.display());
    }
    Ok(())
}

fn metadata_value(connection: &Connection, key: &str) -> Result<Option<String>> {
    Ok(connection
        .query_row(
            "SELECT value FROM metadata WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?)
}

fn expected_ownership(dataset: &NormalizedDataset) -> BTreeSet<OwnershipKey> {
    let mut expected = BTreeSet::new();
    add_ownership("run", &dataset.runs, &mut expected);
    add_ownership("lift", &dataset.lifts, &mut expected);
    add_ownership("spot", &dataset.spots, &mut expected);
    add_ownership("connection", &dataset.connections, &mut expected);
    expected
}

fn add_ownership(
    feature_kind: &'static str,
    records: &[FeatureRecord],
    output: &mut BTreeSet<OwnershipKey>,
) {
    for record in records {
        for resort_id in &record.resort_ids {
            output.insert(OwnershipKey {
                feature_kind,
                feature_id: record.id.clone(),
                resort_id: resort_id.clone(),
            });
        }
    }
}

fn collect_pack_ownership(
    connection: &Connection,
    output: &mut BTreeSet<OwnershipKey>,
) -> Result<()> {
    collect_join_ownership(connection, "run", "run_resorts", "run_id", output)?;
    collect_join_ownership(connection, "lift", "lift_resorts", "lift_id", output)?;
    collect_join_ownership(connection, "spot", "spot_resorts", "spot_id", output)?;
    collect_join_ownership(
        connection,
        "connection",
        "connection_resorts",
        "connection_id",
        output,
    )?;
    Ok(())
}

fn collect_join_ownership(
    connection: &Connection,
    feature_kind: &'static str,
    table: &str,
    id_column: &str,
    output: &mut BTreeSet<OwnershipKey>,
) -> Result<()> {
    let sql = format!("SELECT {id_column}, resort_id FROM {table}");
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (feature_id, resort_id) = row?;
        output.insert(OwnershipKey {
            feature_kind,
            feature_id,
            resort_id,
        });
    }
    Ok(())
}

fn gzip_file(source: &Path, destination: &Path) -> Result<()> {
    let input = File::open(source).with_context(|| format!("opening {}", source.display()))?;
    let output =
        File::create(destination).with_context(|| format!("creating {}", destination.display()))?;
    let mut reader = BufReader::new(input);
    let mut encoder = GzEncoder::new(output, Compression::default());
    io::copy(&mut reader, &mut encoder)?;
    encoder.finish()?.sync_all()?;
    Ok(())
}

fn geometry_to_wkb(geometry: &Value) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    write_wkb_geometry(&mut output, geometry)?;
    Ok(output)
}

fn write_wkb_geometry(output: &mut Vec<u8>, geometry: &Value) -> Result<()> {
    let geometry_type = geometry
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("geometry is missing a type"))?;
    let coordinates = geometry.get("coordinates");
    let dimension = coordinates.and_then(coordinate_dimension).unwrap_or(2);
    let type_code = wkb_type_code(geometry_type)? + if dimension >= 3 { 1000 } else { 0 };
    output.push(1);
    output.extend_from_slice(&type_code.to_le_bytes());

    match geometry_type {
        "Point" => write_wkb_position(
            output,
            coordinates.ok_or_else(|| anyhow!("Point is missing coordinates"))?,
            dimension,
        )?,
        "LineString" => write_wkb_linestring(
            output,
            coordinates.ok_or_else(|| anyhow!("LineString is missing coordinates"))?,
            dimension,
        )?,
        "Polygon" => write_wkb_polygon(
            output,
            coordinates.ok_or_else(|| anyhow!("Polygon is missing coordinates"))?,
            dimension,
        )?,
        "MultiPoint" => write_wkb_multi_point(
            output,
            coordinates.ok_or_else(|| anyhow!("MultiPoint is missing coordinates"))?,
            dimension,
        )?,
        "MultiLineString" => write_wkb_multi_line_string(
            output,
            coordinates.ok_or_else(|| anyhow!("MultiLineString is missing coordinates"))?,
            dimension,
        )?,
        "MultiPolygon" => write_wkb_multi_polygon(
            output,
            coordinates.ok_or_else(|| anyhow!("MultiPolygon is missing coordinates"))?,
            dimension,
        )?,
        "GeometryCollection" => {
            let geometries = geometry
                .get("geometries")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("GeometryCollection is missing geometries"))?;
            output.extend_from_slice(&(geometries.len() as u32).to_le_bytes());
            for child in geometries {
                write_wkb_geometry(output, child)?;
            }
        }
        other => bail!("unsupported GeoJSON geometry type {other}"),
    }
    Ok(())
}

fn wkb_type_code(geometry_type: &str) -> Result<u32> {
    match geometry_type {
        "Point" => Ok(1),
        "LineString" => Ok(2),
        "Polygon" => Ok(3),
        "MultiPoint" => Ok(4),
        "MultiLineString" => Ok(5),
        "MultiPolygon" => Ok(6),
        "GeometryCollection" => Ok(7),
        other => bail!("unsupported GeoJSON geometry type {other}"),
    }
}

fn coordinate_dimension(value: &Value) -> Option<usize> {
    let values = value.as_array()?;
    if values.len() >= 2 && values.first().is_some_and(Value::is_number) {
        return Some(values.len());
    }
    values.iter().find_map(coordinate_dimension)
}

fn write_wkb_position(output: &mut Vec<u8>, value: &Value, dimension: usize) -> Result<()> {
    let coordinates = value
        .as_array()
        .ok_or_else(|| anyhow!("coordinate is not an array"))?;
    let longitude = coordinates
        .first()
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("coordinate is missing longitude"))?;
    let latitude = coordinates
        .get(1)
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("coordinate is missing latitude"))?;
    output.extend_from_slice(&longitude.to_le_bytes());
    output.extend_from_slice(&latitude.to_le_bytes());
    if dimension >= 3 {
        let elevation = coordinates
            .get(2)
            .and_then(Value::as_f64)
            .ok_or_else(|| anyhow!("3D coordinate is missing elevation"))?;
        output.extend_from_slice(&elevation.to_le_bytes());
    }
    Ok(())
}

fn write_wkb_linestring(output: &mut Vec<u8>, value: &Value, dimension: usize) -> Result<()> {
    let points = value
        .as_array()
        .ok_or_else(|| anyhow!("LineString coordinates are not an array"))?;
    output.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for point in points {
        write_wkb_position(output, point, dimension)?;
    }
    Ok(())
}

fn write_wkb_polygon(output: &mut Vec<u8>, value: &Value, dimension: usize) -> Result<()> {
    let rings = value
        .as_array()
        .ok_or_else(|| anyhow!("Polygon coordinates are not an array"))?;
    output.extend_from_slice(&(rings.len() as u32).to_le_bytes());
    for ring in rings {
        write_wkb_linestring(output, ring, dimension)?;
    }
    Ok(())
}

fn write_wkb_multi_point(output: &mut Vec<u8>, value: &Value, dimension: usize) -> Result<()> {
    let points = value
        .as_array()
        .ok_or_else(|| anyhow!("MultiPoint coordinates are not an array"))?;
    output.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for point in points {
        output.push(1);
        let type_code = 1_u32 + if dimension >= 3 { 1000 } else { 0 };
        output.extend_from_slice(&type_code.to_le_bytes());
        write_wkb_position(output, point, dimension)?;
    }
    Ok(())
}

fn write_wkb_multi_line_string(
    output: &mut Vec<u8>,
    value: &Value,
    dimension: usize,
) -> Result<()> {
    let lines = value
        .as_array()
        .ok_or_else(|| anyhow!("MultiLineString coordinates are not an array"))?;
    output.extend_from_slice(&(lines.len() as u32).to_le_bytes());
    for line in lines {
        output.push(1);
        let type_code = 2_u32 + if dimension >= 3 { 1000 } else { 0 };
        output.extend_from_slice(&type_code.to_le_bytes());
        write_wkb_linestring(output, line, dimension)?;
    }
    Ok(())
}

fn write_wkb_multi_polygon(output: &mut Vec<u8>, value: &Value, dimension: usize) -> Result<()> {
    let polygons = value
        .as_array()
        .ok_or_else(|| anyhow!("MultiPolygon coordinates are not an array"))?;
    output.extend_from_slice(&(polygons.len() as u32).to_le_bytes());
    for polygon in polygons {
        output.push(1);
        let type_code = 3_u32 + if dimension >= 3 { 1000 } else { 0 };
        output.extend_from_slice(&type_code.to_le_bytes());
        write_wkb_polygon(output, polygon, dimension)?;
    }
    Ok(())
}
