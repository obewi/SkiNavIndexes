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
    pub(super) warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ResortRecord {
    pub(super) id: String,
    pub(super) name: String,
    #[serde(rename = "type")]
    pub(super) resort_type: String,
    #[serde(rename = "parent_id")]
    pub(super) parent_id: Option<String>,
    #[serde(rename = "parent_name")]
    pub(super) parent_name: Option<String>,
    pub(super) bbox: [f64; 4],
    #[serde(rename = "area_km2")]
    pub(super) area_km2: f64,
    pub(super) country: Option<String>,
    #[serde(rename = "isoCodes")]
    pub(super) iso_codes: Vec<String>,
    #[serde(rename = "countryCodes")]
    pub(super) country_codes: Vec<String>,
    #[serde(rename = "groupId")]
    pub(super) group_id: String,
    pub(super) center: [f64; 2],
    #[serde(rename = "childIds")]
    pub(super) child_ids: Vec<String>,
    #[serde(skip)]
    pub(super) run_convention: Option<String>,
    #[serde(skip)]
    pub(super) places: Value,
    #[serde(skip)]
    pub(super) statistics: Value,
}

#[derive(Debug, Clone)]
pub(super) struct FeatureRecord {
    pub(super) id: String,
    pub(super) resort_ids: Vec<String>,
    pub(super) geometry: Value,
    pub(super) properties: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReleaseResortInput {
    pub(super) id: String,
    pub(super) estimated_size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReleaseGroupInput {
    pub(super) group_id: String,
    pub(super) resorts: Vec<ReleaseResortInput>,
    pub(super) estimated_size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReleasePackGroup {
    pub(super) group_id: String,
    pub(super) part_index: Option<usize>,
    pub(super) part_count: Option<usize>,
    pub(super) resort_ids: Vec<String>,
    pub(super) estimated_size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReleasePackPlan {
    pub(super) asset_name: String,
    pub(super) archive_type: &'static str,
    pub(super) groups: Vec<ReleasePackGroup>,
    pub(super) estimated_size_bytes: u64,
}
