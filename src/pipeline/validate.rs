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
            "downhill_polygons.geojson",
            "downhill_centerlines.geojson",
            "connections.geojson",
            "connection_sections.geojson",
            "lifts.geojson",
            "spots.geojson",
            "audit_report.json",
        ] {
            validate_geojson_or_json_exists(&packages.join(safe_path_id(id)).join(file))?;
        }
    }
    Ok(())
}

pub(super) fn validate_geojson_or_json_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("missing {}", path.display());
    }
    let _: Value = read_json(path).with_context(|| format!("validating {}", path.display()))?;
    Ok(())
}
