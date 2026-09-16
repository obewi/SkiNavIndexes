use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ConnectionConversionSummary {
    pub(super) feature_count: usize,
    pub(super) ignored_count: usize,
}

#[derive(Clone, Debug)]
pub(super) struct SourceFeature {
    pub(super) id: Option<Value>,
    pub(super) properties: Map<String, Value>,
    pub(super) geometry: Value,
}

impl SourceFeature {
    pub(super) fn from_value(value: Value) -> Result<Self> {
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

    pub(super) fn source_id(&self, prefix: &str, index: usize) -> String {
        first_string(&self.properties, &["id", "sourceId", "osmId"])
            .or_else(|| self.id.as_ref().and_then(value_to_string))
            .unwrap_or_else(|| format!("{prefix}:{index}"))
    }
}

#[derive(Debug)]
pub(super) struct BuildSummary {
    pub(super) dataset_version: String,
    pub(super) resort_count: usize,
    pub(super) run_count: usize,
    pub(super) lift_count: usize,
    pub(super) connection_count: usize,
    pub(super) spot_count: usize,
}

#[derive(Debug)]
pub(super) struct NormalizedDataset {
    pub(super) dataset_version: String,
    pub(super) generated_at: DateTime<Utc>,
    pub(super) resorts: Vec<ResortRecord>,
    pub(super) runs: Vec<FeatureRecord>,
    pub(super) lifts: Vec<FeatureRecord>,
    pub(super) spots: Vec<FeatureRecord>,
    pub(super) connections: Vec<FeatureRecord>,
    pub(super) lift_station_memberships: Vec<LiftStationMembership>,
}

/// The per-resort value is only the truncated digest. The contract that makes
/// it comparable is published once in catalog metadata and latest.json.
pub(super) const SOURCE_FINGERPRINT_ALGORITHM: &str = "sha256";
pub(super) const SOURCE_FINGERPRINT_VERSION: i64 = 1;
pub(super) const SOURCE_FINGERPRINT_ENCODING: &str = "lowercase-hex";
pub(super) const SOURCE_FINGERPRINT_TRUNCATION_BITS: i64 = 128;
pub(super) const SOURCE_FINGERPRINT_HEX_LENGTH: usize = 32;

const SOURCE_FINGERPRINT_TRUNCATION_BYTES: usize = SOURCE_FINGERPRINT_TRUNCATION_BITS as usize / 8;
const SOURCE_FINGERPRINT_PREIMAGE_PREFIX: &[u8] = b"skinav/source-fingerprint/v1\0";

/// Build one content fingerprint for each root-plus-descendants processing
/// scope. The preimage is independent of release versions and pack layout.
pub(super) fn source_scope_fingerprints(
    dataset: &NormalizedDataset,
    resort_ids_by_root: &BTreeMap<String, BTreeSet<String>>,
) -> Result<BTreeMap<String, String>> {
    let roots = resort_ids_by_root.iter().collect::<Vec<_>>();
    let mut records_by_root = (0..roots.len())
        .map(|_| Vec::<Vec<u8>>::new())
        .collect::<Vec<_>>();

    let mut root_indexes_by_resort_id = BTreeMap::<&str, BTreeSet<usize>>::new();
    for (root_index, (_, scope_ids)) in roots.iter().enumerate() {
        for resort_id in *scope_ids {
            root_indexes_by_resort_id
                .entry(resort_id.as_str())
                .or_default()
                .insert(root_index);
        }
    }

    for resort in &dataset.resorts {
        let Some(root_indexes) = root_indexes_by_resort_id.get(resort.id.as_str()) else {
            continue;
        };
        let encoded = encode_resort_record(resort);
        for &root_index in root_indexes {
            records_by_root[root_index].push(encoded.clone());
        }
    }

    let mut station_root_indexes_by_id = BTreeMap::<&str, BTreeSet<usize>>::new();
    let mut lift_root_indexes_by_id = BTreeMap::<&str, BTreeSet<usize>>::new();
    for (kind, source_records) in [
        ("run", dataset.runs.as_slice()),
        ("lift", dataset.lifts.as_slice()),
        ("spot", dataset.spots.as_slice()),
        ("connection", dataset.connections.as_slice()),
    ] {
        for record in source_records {
            let affected_root_indexes = record
                .resort_ids
                .iter()
                .filter_map(|resort_id| root_indexes_by_resort_id.get(resort_id.as_str()))
                .flat_map(|root_indexes| root_indexes.iter().copied())
                .collect::<BTreeSet<_>>();

            if kind == "spot" {
                station_root_indexes_by_id
                    .entry(record.id.as_str())
                    .or_default()
                    .extend(affected_root_indexes.iter().copied());
            } else if kind == "lift" {
                lift_root_indexes_by_id
                    .entry(record.id.as_str())
                    .or_default()
                    .extend(affected_root_indexes.iter().copied());
            }

            for root_index in affected_root_indexes {
                let scope_ids = roots[root_index].1;
                records_by_root[root_index].push(encode_feature_record(kind, record, scope_ids)?);
            }
        }
    }

    for membership in &dataset.lift_station_memberships {
        let (Some(station_root_indexes), Some(lift_root_indexes)) = (
            station_root_indexes_by_id.get(membership.station_id.as_str()),
            lift_root_indexes_by_id.get(membership.lift_id.as_str()),
        ) else {
            continue;
        };
        let (smaller, larger) = if station_root_indexes.len() <= lift_root_indexes.len() {
            (station_root_indexes, lift_root_indexes)
        } else {
            (lift_root_indexes, station_root_indexes)
        };
        for &root_index in smaller {
            if larger.contains(&root_index) {
                records_by_root[root_index].push(encode_membership_record(membership));
            }
        }
    }

    let mut fingerprints = BTreeMap::new();
    for ((root_id, scope_ids), mut records) in roots.into_iter().zip(records_by_root) {
        fingerprints.insert(
            root_id.clone(),
            digest_source_fingerprint(root_id, scope_ids, &mut records),
        );
    }
    Ok(fingerprints)
}

// Version 1 encoding: length-prefixed UTF-8/byte fields and big-endian
// IEEE-754 f64 bit patterns. Feature properties use recursively key-sorted,
// compact JSON after the same assignment-property pruning as source packs.
fn encode_resort_record(resort: &ResortRecord) -> Vec<u8> {
    let mut output = Vec::new();
    encode_string(&mut output, "resort");
    encode_string(&mut output, &resort.id);
    encode_string(&mut output, &resort.name);
    encode_optional_string(&mut output, resort.parent_id.as_deref());
    for value in resort.bbox {
        encode_f64(&mut output, value);
    }
    encode_f64(&mut output, resort.area_km2);
    encode_optional_string(&mut output, resort.country.as_deref());
    let mut iso_codes = resort
        .iso_codes
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    iso_codes.sort_unstable();
    encode_count(&mut output, iso_codes.len());
    for iso_code in iso_codes {
        encode_string(&mut output, iso_code);
    }
    for value in resort.center {
        encode_f64(&mut output, value);
    }
    encode_optional_string(&mut output, resort.run_convention.as_deref());
    output
}

fn encode_feature_record(
    kind: &str,
    record: &FeatureRecord,
    scope_ids: &BTreeSet<String>,
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    encode_string(&mut output, kind);
    encode_string(&mut output, &record.id);
    let mut owned_ids = record
        .resort_ids
        .iter()
        .filter(|id| scope_ids.contains(*id))
        .map(String::as_str)
        .collect::<Vec<_>>();
    owned_ids.sort_unstable();
    encode_count(&mut output, owned_ids.len());
    for resort_id in owned_ids {
        encode_string(&mut output, resort_id);
    }
    encode_bytes(&mut output, &geometry_to_wkb(&record.geometry)?);
    let properties = Value::Object(stored_properties(&record.properties));
    let properties = serde_json::to_vec(&canonical_json_value(&properties))?;
    encode_bytes(&mut output, &properties);
    Ok(output)
}

fn encode_membership_record(membership: &LiftStationMembership) -> Vec<u8> {
    let mut output = Vec::new();
    encode_string(&mut output, "liftStationMembership");
    encode_string(&mut output, &membership.station_id);
    encode_string(&mut output, &membership.lift_id);
    encode_optional_string(&mut output, membership.source_node_id.as_deref());
    encode_f64(&mut output, membership.contact[0]);
    encode_f64(&mut output, membership.contact[1]);
    encode_optional_string(&mut output, membership.contact_kind.as_deref());
    output
}

fn encode_string(output: &mut Vec<u8>, value: &str) {
    encode_bytes(output, value.as_bytes());
}

fn encode_bytes(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn encode_optional_string(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            output.push(1);
            encode_string(output, value);
        }
        None => output.push(0),
    }
}

fn encode_f64(output: &mut Vec<u8>, value: f64) {
    output.extend_from_slice(&value.to_bits().to_be_bytes());
}

fn encode_count(output: &mut Vec<u8>, count: usize) {
    output.extend_from_slice(&(count as u64).to_be_bytes());
}

fn canonical_json_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonical_json_value(&object[key]));
            }
            Value::Object(canonical)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical_json_value).collect()),
        _ => value.clone(),
    }
}

fn digest_source_fingerprint(
    root_id: &str,
    scope_ids: &BTreeSet<String>,
    records: &mut [Vec<u8>],
) -> String {
    records.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(SOURCE_FINGERPRINT_PREIMAGE_PREFIX);
    update_u64(&mut hasher, SOURCE_FINGERPRINT_VERSION as u64);
    update_string(&mut hasher, root_id);
    update_u64(&mut hasher, scope_ids.len() as u64);
    for scope_id in scope_ids {
        update_string(&mut hasher, scope_id);
    }
    update_u64(&mut hasher, records.len() as u64);
    for record in records {
        update_bytes(&mut hasher, record);
    }
    let digest = hasher.finalize();
    hex::encode(&digest[..SOURCE_FINGERPRINT_TRUNCATION_BYTES])
}

fn update_string(hasher: &mut Sha256, value: &str) {
    update_bytes(hasher, value.as_bytes());
}

fn update_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn update_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_be_bytes());
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LiftStationTopology {
    pub(super) station_source: String,
    pub(super) members: Vec<LiftStationTopologyMember>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LiftStationTopologyMember {
    pub(super) lift_source: String,
    pub(super) contact_node: String,
    pub(super) coordinate: [f64; 2],
    pub(super) contact_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct LiftStationMembership {
    pub(super) station_id: String,
    pub(super) lift_id: String,
    pub(super) source_node_id: Option<String>,
    pub(super) contact: [f64; 2],
    pub(super) contact_kind: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ResortRecord {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) parent_id: Option<String>,
    pub(super) bbox: [f64; 4],
    pub(super) area_km2: f64,
    pub(super) country: Option<String>,
    pub(super) iso_codes: Vec<String>,
    pub(super) center: [f64; 2],
    pub(super) run_convention: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct FeatureRecord {
    pub(super) id: String,
    pub(super) resort_ids: Vec<String>,
    pub(super) geometry: Value,
    pub(super) properties: Map<String, Value>,
}
