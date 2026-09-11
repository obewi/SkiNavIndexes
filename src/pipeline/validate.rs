use super::*;

pub(super) fn validate_geojson_file(path: &Path) -> Result<()> {
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

pub(super) fn validate_output(output_dir: &Path) -> Result<()> {
    validate_source_output(output_dir)
}
