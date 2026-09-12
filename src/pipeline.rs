mod build;
mod fetch;
mod geo;
mod io;
mod model;
mod normalize;
mod sqlite;
mod validate;

#[cfg(test)]
mod tests;

use crate::cli::{Cli, Command};
use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, File},
    io::{BufReader, ErrorKind, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use build::*;
use fetch::*;
use geo::*;
use io::*;
use model::*;
use normalize::*;
use sqlite::*;
use validate::*;

const LAYER_FILES: [&str; 4] = [
    "ski_areas.geojson",
    "runs.geojson",
    "lifts.geojson",
    "spots.geojson",
];
const CONNECTIONS_FILE: &str = "connections.geojson";
const LIFT_STATION_TOPOLOGY_FILE: &str = "lift_station_topology.json";
const LIFT_STATION_TOPOLOGY_BATCH_SIZE: usize = 200;
const CONNECTION_ENDPOINT_MATCH_METERS: f64 = 60.0;
const CONNECTION_SEGMENT_MATCH_METERS: f64 = 35.0;
const CONNECTION_SEARCH_PADDING_METERS: f64 = 300.0;
const NETWORK_BUCKET_DEGREES: f64 = 0.02;

pub fn run() -> Result<()> {
    let cli = <Cli as clap::Parser>::parse();
    match cli.command {
        Command::Fetch {
            cache_dir,
            dataset_version,
            source_base_url,
            overpass_base_url,
            skip_connection_enrichment,
            skip_station_topology_enrichment,
        } => fetch_sources(
            &cache_dir,
            dataset_version,
            &source_base_url,
            &overpass_base_url,
            skip_connection_enrichment,
            skip_station_topology_enrichment,
        ),
        Command::Build {
            cache_dir,
            output_dir,
            dataset_version,
        } => build_from_cache(&cache_dir, &output_dir, dataset_version).map(|summary| {
            eprintln!(
                "built dataset {}: {} resorts, {} runs, {} lifts, {} connections, {} spots",
                summary.dataset_version,
                summary.resort_count,
                summary.run_count,
                summary.lift_count,
                summary.connection_count,
                summary.spot_count
            );
        }),
        Command::Validate { output_dir } => validate_output(&output_dir),
        Command::All {
            cache_dir,
            output_dir,
            dataset_version,
            source_base_url,
            overpass_base_url,
            skip_connection_enrichment,
            skip_station_topology_enrichment,
            skip_fetch,
        } => {
            if !skip_fetch {
                fetch_sources(
                    &cache_dir,
                    dataset_version.clone(),
                    &source_base_url,
                    &overpass_base_url,
                    skip_connection_enrichment,
                    skip_station_topology_enrichment,
                )?;
            }
            let summary = build_from_cache(&cache_dir, &output_dir, dataset_version)?;
            eprintln!(
                "built dataset {}: {} resorts, {} runs, {} lifts, {} connections, {} spots",
                summary.dataset_version,
                summary.resort_count,
                summary.run_count,
                summary.lift_count,
                summary.connection_count,
                summary.spot_count
            );
            validate_output(&output_dir)
        }
    }
}
