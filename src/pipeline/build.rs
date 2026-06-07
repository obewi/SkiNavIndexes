use super::*;

pub(super) fn build_from_cache(
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
    let spots = read_feature_collection(&dataset_dir.join("spots.geojson"))?;
    let connections = read_feature_collection(&connections_path)?;

    let generated_at = Utc::now();
    let normalized = normalize_sources(
        ski_areas,
        runs,
        lifts,
        spots,
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
        spot_count: normalized.spots.len(),
    })
}

pub(super) fn resolve_dataset_dir(
    cache_dir: &Path,
    dataset_version: Option<String>,
) -> Result<PathBuf> {
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
