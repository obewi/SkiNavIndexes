use super::*;
use flate2::read::GzDecoder;
use flate2::{Compression, write::GzEncoder};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufReader},
};

const SOURCE_SCHEMA_VERSION: i64 = 3;
// The planner estimates compact row payloads. Keeping the estimate at 28 MiB
// leaves room for SQLite pages, indexes, and gzip variance while keeping normal
// downloads below the 12 MiB compressed ceiling on the published snapshot.
pub(super) const SOURCE_PACK_TARGET_BYTES: u64 = 28 * 1024 * 1024;
const SOURCE_PACK_MAX_COMPRESSED_BYTES: u64 = 12 * 1024 * 1024;
const SOURCE_PACK_PARTITION: &str = "adaptive-quadtree";
const SOURCE_PACK_MAX_QUADTREE_DEPTH: u8 = 16;

#[derive(Clone, Copy, Debug)]
struct SpatialBounds {
    west: f64,
    south: f64,
    east: f64,
    north: f64,
}

impl SpatialBounds {
    fn world() -> Self {
        Self {
            west: -180.0,
            south: -90.0,
            east: 180.0,
            north: 90.0,
        }
    }

    fn quadrants(self) -> [Self; 4] {
        let midpoint_lon = (self.west + self.east) / 2.0;
        let midpoint_lat = (self.south + self.north) / 2.0;
        [
            Self {
                west: self.west,
                south: self.south,
                east: midpoint_lon,
                north: midpoint_lat,
            },
            Self {
                west: midpoint_lon,
                south: self.south,
                east: self.east,
                north: midpoint_lat,
            },
            Self {
                west: self.west,
                south: midpoint_lat,
                east: midpoint_lon,
                north: self.north,
            },
            Self {
                west: midpoint_lon,
                south: midpoint_lat,
                east: self.east,
                north: self.north,
            },
        ]
    }

    fn quadrant_index(self, location: [f64; 2]) -> usize {
        let east = location[0] >= (self.west + self.east) / 2.0;
        let north = location[1] >= (self.south + self.north) / 2.0;
        match (north, east) {
            (false, false) => 0,
            (false, true) => 1,
            (true, false) => 2,
            (true, true) => 3,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct SourcePackPlan {
    pub(super) pack_id: String,
    pub(super) resort_ids: Vec<String>,
    pub(super) allows_oversized: bool,
}

#[derive(Clone, Debug)]
struct PackUnit {
    resort_ids: Vec<String>,
    estimated_bytes: u64,
    sort_key: String,
    location: [f64; 2],
    allows_oversized: bool,
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

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct LiftStationMembershipKey {
    station_id: String,
    lift_id: String,
    source_node_id: Option<String>,
    contact_kind: Option<String>,
    contact_lon_bits: u64,
    contact_lat_bits: u64,
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

    let fingerprints_by_resort_id = source_fingerprints_by_resort(dataset)?;
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
    write_catalog(
        &catalog_path,
        dataset,
        &plans,
        &pack_artifacts,
        &fingerprints_by_resort_id,
    )?;
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
        &fingerprints_by_resort_id,
    )?;

    let latest = json!({
        "schemaVersion": SOURCE_SCHEMA_VERSION,
        "datasetVersion": dataset.dataset_version,
        "releaseTag": release_tag_for_dataset(&dataset.dataset_version),
        "sourceFingerprint": {
            "algorithm": SOURCE_FINGERPRINT_ALGORITHM,
            "version": SOURCE_FINGERPRINT_VERSION,
            "truncationBits": SOURCE_FINGERPRINT_TRUNCATION_BITS,
            "encoding": SOURCE_FINGERPRINT_ENCODING,
            "hexLength": SOURCE_FINGERPRINT_HEX_LENGTH
        },
        "packPolicy": {
            "estimatedTargetBytes": SOURCE_PACK_TARGET_BYTES,
            "maxCompressedBytes": SOURCE_PACK_MAX_COMPRESSED_BYTES,
            "partition": SOURCE_PACK_PARTITION,
            "maxQuadtreeDepth": SOURCE_PACK_MAX_QUADTREE_DEPTH
        },
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

pub(super) fn source_fingerprints_by_resort(
    dataset: &NormalizedDataset,
) -> Result<BTreeMap<String, String>> {
    let resorts_by_id = dataset
        .resorts
        .iter()
        .map(|resort| (resort.id.as_str(), resort))
        .collect::<BTreeMap<_, _>>();
    let mut root_by_resort_id = BTreeMap::new();
    let mut resort_ids_by_root = BTreeMap::<String, BTreeSet<String>>::new();
    for resort in &dataset.resorts {
        let root_id = canonical_root_resort_id(resort.id.as_str(), &resorts_by_id)?;
        root_by_resort_id.insert(resort.id.clone(), root_id.clone());
        resort_ids_by_root
            .entry(root_id)
            .or_default()
            .insert(resort.id.clone());
    }

    let fingerprints_by_root = source_scope_fingerprints(dataset, &resort_ids_by_root)?;
    dataset
        .resorts
        .iter()
        .map(|resort| {
            let root_id = root_by_resort_id
                .get(&resort.id)
                .ok_or_else(|| anyhow!("missing processing root for resort {}", resort.id))?;
            let fingerprint = fingerprints_by_root
                .get(root_id)
                .ok_or_else(|| anyhow!("missing processing fingerprint for root {root_id}"))?;
            Ok((resort.id.clone(), fingerprint.clone()))
        })
        .collect()
}

pub(super) fn plan_source_packs(dataset: &NormalizedDataset) -> Result<Vec<SourcePackPlan>> {
    let resort_by_id = dataset
        .resorts
        .iter()
        .map(|resort| (resort.id.as_str(), resort))
        .collect::<BTreeMap<_, _>>();
    let mut members_by_root: BTreeMap<String, Vec<&ResortRecord>> = BTreeMap::new();
    for resort in &dataset.resorts {
        let root_id = canonical_root_resort_id(resort.id.as_str(), &resort_by_id)?;
        members_by_root.entry(root_id).or_default().push(resort);
    }

    let mut fixed_units = Vec::new();
    let mut adaptive_units = Vec::new();
    for (root_id, mut members) in members_by_root {
        members.sort_by(|lhs, rhs| lhs.id.cmp(&rhs.id));
        let root_location = members
            .iter()
            .find(|resort| resort.id == root_id)
            .or_else(|| members.first())
            .map(|resort| resort.center)
            .unwrap_or([0.0, 0.0]);
        let estimates = members
            .iter()
            .map(|resort| estimate_resort_source_bytes(dataset, resort))
            .collect::<Result<Vec<_>>>()?;
        let total_estimate = estimates.iter().copied().fold(0_u64, u64::saturating_add);

        if total_estimate <= SOURCE_PACK_TARGET_BYTES {
            let unit = PackUnit {
                resort_ids: members.iter().map(|resort| resort.id.clone()).collect(),
                estimated_bytes: total_estimate,
                sort_key: root_id.clone(),
                location: root_location,
                allows_oversized: false,
            };
            adaptive_units.push(unit);
            continue;
        }

        let mut current_ids = Vec::new();
        let mut current_estimate = 0_u64;
        let mut part_index = 0_usize;
        for (member, estimate) in members.into_iter().zip(estimates) {
            if !current_ids.is_empty()
                && current_estimate.saturating_add(estimate) > SOURCE_PACK_TARGET_BYTES
            {
                fixed_units.push(PackUnit {
                    resort_ids: std::mem::take(&mut current_ids),
                    estimated_bytes: current_estimate,
                    sort_key: format!("{root_id}#part-{part_index:04}"),
                    location: root_location,
                    allows_oversized: current_estimate > SOURCE_PACK_TARGET_BYTES,
                });
                current_estimate = 0;
                part_index += 1;
            }
            current_ids.push(member.id.clone());
            current_estimate = current_estimate.saturating_add(estimate);
        }
        if !current_ids.is_empty() {
            fixed_units.push(PackUnit {
                resort_ids: current_ids,
                estimated_bytes: current_estimate,
                sort_key: format!("{root_id}#part-{part_index:04}"),
                location: root_location,
                allows_oversized: current_estimate > SOURCE_PACK_TARGET_BYTES,
            });
        }
    }

    let mut adaptive_packs = Vec::new();
    partition_adaptive_units(
        adaptive_units,
        SpatialBounds::world(),
        0,
        &mut adaptive_packs,
    );
    coalesce_adaptive_units(adaptive_packs, &mut fixed_units);

    fixed_units.sort_by(|lhs, rhs| {
        lhs.sort_key
            .cmp(&rhs.sort_key)
            .then_with(|| lhs.resort_ids.cmp(&rhs.resort_ids))
    });

    let plans = fixed_units
        .into_iter()
        .enumerate()
        .map(|(index, unit)| SourcePackPlan {
            pack_id: format!("pack-{:04}", index + 1),
            resort_ids: unit.resort_ids,
            allows_oversized: unit.allows_oversized,
        })
        .collect::<Vec<_>>();
    if plans.is_empty() {
        bail!("cannot generate SQLite source packs without resorts");
    }

    let expected_resort_ids = dataset
        .resorts
        .iter()
        .map(|resort| resort.id.as_str())
        .collect::<BTreeSet<_>>();
    let actual_resort_ids = plans
        .iter()
        .flat_map(|plan| plan.resort_ids.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    let actual_resort_count = plans
        .iter()
        .map(|plan| plan.resort_ids.len())
        .sum::<usize>();
    if actual_resort_count != expected_resort_ids.len() || actual_resort_ids != expected_resort_ids
    {
        bail!("source pack planner does not cover every resort exactly once");
    }
    Ok(plans)
}

fn canonical_root_resort_id(
    resort_id: &str,
    resorts_by_id: &BTreeMap<&str, &ResortRecord>,
) -> Result<String> {
    let mut current_id = resort_id;
    let mut visited = BTreeSet::new();
    while let Some(resort) = resorts_by_id.get(current_id) {
        if !visited.insert(current_id) {
            bail!("resort hierarchy contains a cycle at {current_id}");
        }
        let Some(parent_id) = resort.parent_id.as_deref() else {
            return Ok(current_id.to_string());
        };
        if !resorts_by_id.contains_key(parent_id) {
            bail!("resort {current_id} references missing parent {parent_id}");
        }
        current_id = parent_id;
    }
    bail!("resort hierarchy is missing resort {resort_id}")
}

fn partition_adaptive_units(
    units: Vec<PackUnit>,
    bounds: SpatialBounds,
    depth: u8,
    output: &mut Vec<PackUnit>,
) {
    if units.is_empty() {
        return;
    }

    let total_estimate = units
        .iter()
        .map(|unit| unit.estimated_bytes)
        .fold(0_u64, u64::saturating_add);
    if total_estimate <= SOURCE_PACK_TARGET_BYTES || units.len() == 1 {
        output.push(combine_pack_units(units));
        return;
    }

    if depth >= SOURCE_PACK_MAX_QUADTREE_DEPTH {
        append_size_bounded_units(units, output);
        return;
    }

    let quadrants = bounds.quadrants();
    let mut buckets = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for unit in units {
        let index = bounds.quadrant_index(unit.location);
        buckets[index].push(unit);
    }
    for (index, bucket) in buckets.into_iter().enumerate() {
        partition_adaptive_units(bucket, quadrants[index], depth + 1, output);
    }
}

fn append_size_bounded_units(mut units: Vec<PackUnit>, output: &mut Vec<PackUnit>) {
    units.sort_by(|lhs, rhs| lhs.sort_key.cmp(&rhs.sort_key));
    let mut current = Vec::new();
    let mut current_estimate = 0_u64;
    for unit in units {
        if !current.is_empty()
            && current_estimate.saturating_add(unit.estimated_bytes) > SOURCE_PACK_TARGET_BYTES
        {
            output.push(combine_pack_units(std::mem::take(&mut current)));
            current_estimate = 0;
        }
        current_estimate = current_estimate.saturating_add(unit.estimated_bytes);
        current.push(unit);
    }
    if !current.is_empty() {
        output.push(combine_pack_units(current));
    }
}

fn coalesce_adaptive_units(units: Vec<PackUnit>, output: &mut Vec<PackUnit>) {
    let mut current = Vec::new();
    let mut current_estimate = 0_u64;
    for unit in units {
        if !current.is_empty()
            && current_estimate.saturating_add(unit.estimated_bytes) > SOURCE_PACK_TARGET_BYTES
        {
            output.push(combine_pack_units(std::mem::take(&mut current)));
            current_estimate = 0;
        }
        current_estimate = current_estimate.saturating_add(unit.estimated_bytes);
        current.push(unit);
    }
    if !current.is_empty() {
        output.push(combine_pack_units(current));
    }
}

fn combine_pack_units(mut units: Vec<PackUnit>) -> PackUnit {
    debug_assert!(!units.is_empty());
    units.sort_by(|lhs, rhs| lhs.sort_key.cmp(&rhs.sort_key));
    let mut units = units.into_iter();
    let mut combined = units
        .next()
        .expect("pack unit collection must not be empty");
    for unit in units {
        combined.resort_ids.extend(unit.resort_ids);
        combined.estimated_bytes = combined
            .estimated_bytes
            .saturating_add(unit.estimated_bytes);
        if unit.sort_key < combined.sort_key {
            combined.sort_key = unit.sort_key;
        }
        combined.allows_oversized |= unit.allows_oversized;
    }
    combined.resort_ids.sort();
    combined
}

pub(super) fn estimate_resort_source_bytes(
    dataset: &NormalizedDataset,
    resort: &ResortRecord,
) -> Result<u64> {
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
    let owned_station_ids = dataset
        .spots
        .iter()
        .filter(|record| record.resort_ids.iter().any(|id| id == &resort.id))
        .map(|record| record.id.as_str())
        .collect::<BTreeSet<_>>();
    let owned_lift_ids = dataset
        .lifts
        .iter()
        .filter(|record| record.resort_ids.iter().any(|id| id == &resort.id))
        .map(|record| record.id.as_str())
        .collect::<BTreeSet<_>>();
    size = size.saturating_add(
        dataset
            .lift_station_memberships
            .iter()
            .filter(|membership| {
                owned_station_ids.contains(membership.station_id.as_str())
                    && owned_lift_ids.contains(membership.lift_id.as_str())
            })
            .count() as u64
            * 96,
    );
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
    let station_ids = dataset
        .spots
        .iter()
        .filter(|record| record.resort_ids.iter().any(|id| resort_ids.contains(id)))
        .map(|record| record.id.as_str())
        .collect::<BTreeSet<_>>();
    let lift_ids = dataset
        .lifts
        .iter()
        .filter(|record| record.resort_ids.iter().any(|id| resort_ids.contains(id)))
        .map(|record| record.id.as_str())
        .collect::<BTreeSet<_>>();
    // Topology normalization shares endpoint ownership across connected resort
    // scopes, so both foreign-key targets are intentionally present here.
    for membership in &dataset.lift_station_memberships {
        if station_ids.contains(membership.station_id.as_str())
            && lift_ids.contains(membership.lift_id.as_str())
        {
            insert_lift_station_membership(&transaction, membership)?;
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
        CREATE TABLE lift_station_memberships (
            station_id TEXT NOT NULL REFERENCES spots(id) ON DELETE CASCADE,
            lift_id TEXT NOT NULL REFERENCES lifts(id) ON DELETE CASCADE,
            source_node_id TEXT,
            contact_lon REAL NOT NULL,
            contact_lat REAL NOT NULL,
            contact_kind TEXT,
            PRIMARY KEY (station_id, lift_id)
        );

        CREATE INDEX run_resorts_resort_idx ON run_resorts(resort_id);
        CREATE INDEX lift_resorts_resort_idx ON lift_resorts(resort_id);
        CREATE INDEX spot_resorts_resort_idx ON spot_resorts(resort_id);
        CREATE INDEX connection_resorts_resort_idx ON connection_resorts(resort_id);
        CREATE INDEX lift_station_memberships_lift_idx ON lift_station_memberships(lift_id);
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

fn insert_lift_station_membership(
    transaction: &Transaction<'_>,
    membership: &LiftStationMembership,
) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO lift_station_memberships (station_id, lift_id, source_node_id, contact_lon, contact_lat, contact_kind) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            membership.station_id,
            membership.lift_id,
            membership.source_node_id,
            membership.contact[0],
            membership.contact[1],
            membership.contact_kind,
        ],
    )?;
    Ok(())
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
    fingerprints_by_resort_id: &BTreeMap<String, String>,
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
            (
                "sourceFingerprintAlgorithm",
                SOURCE_FINGERPRINT_ALGORITHM.to_string(),
            ),
            (
                "sourceFingerprintVersion",
                SOURCE_FINGERPRINT_VERSION.to_string(),
            ),
            (
                "sourceFingerprintTruncationBits",
                SOURCE_FINGERPRINT_TRUNCATION_BITS.to_string(),
            ),
            (
                "sourceFingerprintEncoding",
                SOURCE_FINGERPRINT_ENCODING.to_string(),
            ),
            (
                "sourceFingerprintHexLength",
                SOURCE_FINGERPRINT_HEX_LENGTH.to_string(),
            ),
        ],
    )?;

    for resort in &dataset.resorts {
        let source_fingerprint = fingerprints_by_resort_id
            .get(&resort.id)
            .ok_or_else(|| anyhow!("missing source fingerprint for resort {}", resort.id))?;
        transaction.execute(
            "INSERT INTO resorts (id, name, parent_id, bbox_west, bbox_south, bbox_east, bbox_north, center_lon, center_lat, country, run_convention, source_fingerprint) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
                source_fingerprint,
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
            run_convention TEXT,
            source_fingerprint TEXT NOT NULL
                CHECK (
                    length(source_fingerprint) = 32
                    AND source_fingerprint NOT GLOB '*[^0-9a-f]*'
                )
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

fn validate_latest_fingerprint_metadata(value: &Value) -> Result<()> {
    if value.get("algorithm").and_then(Value::as_str) != Some(SOURCE_FINGERPRINT_ALGORITHM)
        || value.get("version").and_then(Value::as_i64) != Some(SOURCE_FINGERPRINT_VERSION)
        || value.get("truncationBits").and_then(Value::as_i64)
            != Some(SOURCE_FINGERPRINT_TRUNCATION_BITS)
        || value.get("encoding").and_then(Value::as_str) != Some(SOURCE_FINGERPRINT_ENCODING)
        || value.get("hexLength").and_then(Value::as_u64)
            != Some(SOURCE_FINGERPRINT_HEX_LENGTH as u64)
    {
        bail!("source latest.json sourceFingerprint metadata mismatch");
    }
    Ok(())
}

fn validate_catalog_fingerprint_metadata(connection: &Connection) -> Result<()> {
    let expected_version = SOURCE_FINGERPRINT_VERSION.to_string();
    let expected_truncation_bits = SOURCE_FINGERPRINT_TRUNCATION_BITS.to_string();
    let expected_hex_length = SOURCE_FINGERPRINT_HEX_LENGTH.to_string();
    for (key, expected) in [
        ("sourceFingerprintAlgorithm", SOURCE_FINGERPRINT_ALGORITHM),
        ("sourceFingerprintVersion", expected_version.as_str()),
        (
            "sourceFingerprintTruncationBits",
            expected_truncation_bits.as_str(),
        ),
        ("sourceFingerprintEncoding", SOURCE_FINGERPRINT_ENCODING),
        ("sourceFingerprintHexLength", expected_hex_length.as_str()),
    ] {
        if metadata_value(connection, key)?.as_deref() != Some(expected) {
            bail!("catalog {key} metadata mismatch");
        }
    }
    Ok(())
}

fn validate_catalog_fingerprints(
    connection: &Connection,
    expected: &BTreeMap<String, String>,
) -> Result<()> {
    let actual = catalog_fingerprint_rows(connection)?;
    if actual
        .values()
        .any(|value| !is_processing_fingerprint(value))
    {
        bail!("catalog contains an invalid processing fingerprint");
    }
    if &actual != expected {
        bail!("catalog processing fingerprints do not match normalized scopes");
    }
    Ok(())
}

fn validate_catalog_fingerprint_shape(connection: &Connection) -> Result<()> {
    let actual = catalog_fingerprint_rows(connection)?;
    if actual
        .values()
        .any(|value| !is_processing_fingerprint(value))
    {
        bail!("catalog contains an invalid processing fingerprint");
    }
    Ok(())
}

fn catalog_fingerprint_rows(connection: &Connection) -> Result<BTreeMap<String, String>> {
    Ok(connection
        .prepare("SELECT id, source_fingerprint FROM resorts ORDER BY id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?)
}

fn is_processing_fingerprint(value: &str) -> bool {
    value.len() == SOURCE_FINGERPRINT_HEX_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
    fingerprints_by_resort_id: &BTreeMap<String, String>,
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
    validate_catalog_fingerprint_metadata(&catalog)?;
    validate_catalog_fingerprints(&catalog, fingerprints_by_resort_id)?;

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
        if artifact.compressed_bytes > SOURCE_PACK_MAX_COMPRESSED_BYTES && !plan.allows_oversized {
            bail!(
                "{} compressed size {} exceeds the {} MiB source-pack ceiling",
                plan.pack_id,
                artifact.compressed_bytes,
                SOURCE_PACK_MAX_COMPRESSED_BYTES / (1024 * 1024)
            );
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
    let expected_memberships = expected_lift_station_memberships(dataset);
    let mut actual_memberships = BTreeSet::new();
    for plan in plans {
        let path = staging.join(format!("{}.sqlite", plan.pack_id));
        let connection = Connection::open(path)?;
        collect_pack_lift_station_memberships(&connection, &mut actual_memberships)?;
    }
    if actual_memberships != expected_memberships {
        bail!(
            "SQLite lift station membership set mismatch: expected {}, actual {}",
            expected_memberships.len(),
            actual_memberships.len()
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
    let release_tag = latest
        .get("releaseTag")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("source latest.json missing releaseTag"))?;
    if release_tag != release_tag_for_dataset(dataset_version) {
        bail!("source latest.json releaseTag does not match datasetVersion");
    }
    let pack_policy = latest
        .get("packPolicy")
        .ok_or_else(|| anyhow!("source latest.json missing packPolicy"))?;
    if pack_policy
        .get("estimatedTargetBytes")
        .and_then(Value::as_u64)
        != Some(SOURCE_PACK_TARGET_BYTES)
        || pack_policy
            .get("maxCompressedBytes")
            .and_then(Value::as_u64)
            != Some(SOURCE_PACK_MAX_COMPRESSED_BYTES)
        || pack_policy.get("partition").and_then(Value::as_str) != Some(SOURCE_PACK_PARTITION)
        || pack_policy.get("maxQuadtreeDepth").and_then(Value::as_u64)
            != Some(SOURCE_PACK_MAX_QUADTREE_DEPTH as u64)
    {
        bail!("source latest.json packPolicy mismatch");
    }
    validate_latest_fingerprint_metadata(
        latest
            .get("sourceFingerprint")
            .ok_or_else(|| anyhow!("source latest.json missing sourceFingerprint"))?,
    )?;
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
    validate_catalog_fingerprint_metadata(&catalog)?;
    validate_catalog_fingerprint_shape(&catalog)?;

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
        if artifact.compressed_bytes > SOURCE_PACK_MAX_COMPRESSED_BYTES {
            let resort_count: i64 = catalog.query_row(
                "SELECT COUNT(DISTINCT resort_id) FROM resort_packs WHERE pack_id = ?1",
                params![artifact.pack_id],
                |row| row.get(0),
            )?;
            if resort_count != 1 {
                bail!(
                    "source pack {} compressed size {} exceeds the {} MiB source-pack ceiling",
                    artifact.pack_id,
                    artifact.compressed_bytes,
                    SOURCE_PACK_MAX_COMPRESSED_BYTES / (1024 * 1024)
                );
            }
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

fn release_tag_for_dataset(dataset_version: &str) -> String {
    format!("indexes-{dataset_version}")
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
            bail!("V3 source pack {table} contains an unowned feature");
        }
    }
    let has_orphan: i64 = connection.query_row(
        "SELECT EXISTS(
            SELECT 1
            FROM lift_station_memberships membership
            LEFT JOIN spots station ON station.id = membership.station_id
            LEFT JOIN lifts lift ON lift.id = membership.lift_id
            WHERE station.id IS NULL OR lift.id IS NULL
        )",
        [],
        |row| row.get(0),
    )?;
    if has_orphan != 0 {
        bail!("V3 source pack lift_station_memberships contains an orphan");
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
                bail!("V3 catalog has invalid parent for resort {id}");
            }
        }
        let mut path = BTreeSet::new();
        let mut current = Some(id.as_str());
        while let Some(current_id) = current {
            if !path.insert(current_id) {
                bail!("V3 catalog hierarchy contains a cycle at {current_id}");
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

fn expected_lift_station_memberships(
    dataset: &NormalizedDataset,
) -> BTreeSet<LiftStationMembershipKey> {
    dataset
        .lift_station_memberships
        .iter()
        .map(lift_station_membership_key)
        .collect()
}

fn lift_station_membership_key(membership: &LiftStationMembership) -> LiftStationMembershipKey {
    LiftStationMembershipKey {
        station_id: membership.station_id.clone(),
        lift_id: membership.lift_id.clone(),
        source_node_id: membership.source_node_id.clone(),
        contact_kind: membership.contact_kind.clone(),
        contact_lon_bits: membership.contact[0].to_bits(),
        contact_lat_bits: membership.contact[1].to_bits(),
    }
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

fn collect_pack_lift_station_memberships(
    connection: &Connection,
    output: &mut BTreeSet<LiftStationMembershipKey>,
) -> Result<()> {
    let mut statement = connection.prepare(
        "SELECT station_id, lift_id, source_node_id, contact_lon, contact_lat, contact_kind FROM lift_station_memberships",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(LiftStationMembershipKey {
            station_id: row.get(0)?,
            lift_id: row.get(1)?,
            source_node_id: row.get(2)?,
            contact_kind: row.get(5)?,
            contact_lon_bits: row.get::<_, f64>(3)?.to_bits(),
            contact_lat_bits: row.get::<_, f64>(4)?.to_bits(),
        })
    })?;
    for row in rows {
        output.insert(row?);
    }
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

pub(super) fn geometry_to_wkb(geometry: &Value) -> Result<Vec<u8>> {
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
