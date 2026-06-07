use super::*;

pub(super) fn reset_output_dir(output_dir: &Path) -> Result<()> {
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

pub(super) fn remove_generated_path(path: &Path) -> Result<()> {
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

pub(super) fn read_feature_collection(path: &Path) -> Result<Vec<SourceFeature>> {
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

pub(super) fn read_json(path: &Path) -> Result<Value> {
    let file = File::open(path)?;
    Ok(serde_json::from_reader(BufReader::new(file))?)
}

pub(super) fn write_json_pretty(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

pub(super) fn sha256_file(path: &Path) -> Result<String> {
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

pub(super) fn sha256_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

pub(super) fn file_metadata(layer: &str, path: &Path, source_url: Option<String>) -> Result<Value> {
    let metadata = fs::metadata(path)?;
    Ok(json!({
        "name": layer,
        "path": path,
        "sourceUrl": source_url,
        "sizeBytes": metadata.len(),
        "sha256": sha256_file(path)?,
    }))
}

pub(super) fn file_manifest_for_dir(path: &Path) -> Result<Value> {
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

pub(super) fn directory_size(path: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in WalkDir::new(path).min_depth(1) {
        let entry = entry?;
        if entry.file_type().is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

pub(super) fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
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

pub(super) fn create_tar_gz(source_dir: &Path, archive_path: &Path) -> Result<()> {
    let file = File::create(archive_path)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = Builder::new(encoder);
    archive.append_dir_all(".", source_dir)?;
    archive.finish()?;
    Ok(())
}
