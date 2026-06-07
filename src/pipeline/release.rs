use super::*;

pub(super) fn write_release_packs(output_dir: &Path, dataset: &NormalizedDataset) -> Result<()> {
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

pub(super) fn write_release_pack_group(
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

pub(super) fn release_group_inputs(
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

pub(super) fn plan_release_packs(groups: Vec<ReleaseGroupInput>) -> Vec<ReleasePackPlan> {
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

pub(super) fn single_group_pack(group: ReleaseGroupInput) -> ReleasePackPlan {
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

pub(super) fn split_large_group(group: ReleaseGroupInput) -> Vec<ReleasePackPlan> {
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

pub(super) fn pack_small_groups(mut groups: Vec<ReleaseGroupInput>) -> Vec<ReleasePackPlan> {
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

pub(super) fn small_groups_pack(
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
