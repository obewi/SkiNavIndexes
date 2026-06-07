use clap::{Parser, Subcommand};
use std::path::PathBuf;

const DEFAULT_OVERPASS_BASE_URL: &str = "https://overpass-api.de/api/";

#[derive(Parser)]
#[command(
    version,
    about = "Build SkiNav indexes and artifacts from OpenSkiMap GeoJSON"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Download OpenSkiMap GeoJSON source layers if they are not already cached.
    Fetch {
        #[arg(long, default_value = "data/raw/openskimap")]
        cache_dir: PathBuf,
        #[arg(long)]
        dataset_version: Option<String>,
        #[arg(long, default_value = "https://tiles.openskimap.org/geojson")]
        source_base_url: String,
        #[arg(long, default_value = DEFAULT_OVERPASS_BASE_URL)]
        overpass_base_url: String,
        #[arg(long)]
        skip_connection_enrichment: bool,
    },
    /// Build indexes, packages, group archives, and release-pack artifacts from cached source files.
    Build {
        #[arg(long, default_value = "data/raw/openskimap")]
        cache_dir: PathBuf,
        #[arg(long, default_value = "output")]
        output_dir: PathBuf,
        #[arg(long)]
        dataset_version: Option<String>,
    },
    /// Validate generated output files.
    Validate {
        #[arg(long, default_value = "output")]
        output_dir: PathBuf,
    },
    /// Fetch missing sources, build outputs, and validate them.
    All {
        #[arg(long, default_value = "data/raw/openskimap")]
        cache_dir: PathBuf,
        #[arg(long, default_value = "output")]
        output_dir: PathBuf,
        #[arg(long)]
        dataset_version: Option<String>,
        #[arg(long, default_value = "https://tiles.openskimap.org/geojson")]
        source_base_url: String,
        #[arg(long, default_value = DEFAULT_OVERPASS_BASE_URL)]
        overpass_base_url: String,
        #[arg(long)]
        skip_connection_enrichment: bool,
        #[arg(long)]
        skip_fetch: bool,
    },
}
