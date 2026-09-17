use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OverpassConfig {
    pub(super) endpoints: Vec<String>,
    pub(super) station_batch_size: usize,
    pub(super) request_interval_seconds: u64,
    pub(super) max_retry_rounds: usize,
    pub(super) retry_delay_seconds: u64,
    pub(super) rate_limit_delay_seconds: u64,
    pub(super) request_timeout_seconds: u64,
    pub(super) cache_stale_after_days: i64,
}

impl Default for OverpassConfig {
    fn default() -> Self {
        Self {
            endpoints: vec![
                "https://overpass-api.de/api/interpreter".to_owned(),
                "https://maps.mail.ru/osm/tools/overpass/api/interpreter".to_owned(),
                "https://overpass.private.coffee/api/interpreter".to_owned(),
                "https://overpass.osm.jp/api/interpreter".to_owned(),
            ],
            station_batch_size: 5_000,
            request_interval_seconds: 2,
            max_retry_rounds: 2,
            retry_delay_seconds: 5,
            rate_limit_delay_seconds: 30,
            request_timeout_seconds: 600,
            cache_stale_after_days: 120,
        }
    }
}

impl OverpassConfig {
    pub(super) fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read Overpass config at {}", path.display()))?;
        let config: Self = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse Overpass config at {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub(super) fn validate(&self) -> Result<()> {
        if self.endpoints.is_empty()
            || self
                .endpoints
                .iter()
                .any(|endpoint| endpoint.trim().is_empty())
        {
            bail!("Overpass config must contain at least one non-empty endpoint");
        }
        if self.station_batch_size == 0 {
            bail!("Overpass config stationBatchSize must be greater than zero");
        }
        if self.max_retry_rounds == 0 {
            bail!("Overpass config maxRetryRounds must be greater than zero");
        }
        if self.request_timeout_seconds == 0 {
            bail!("Overpass config requestTimeoutSeconds must be greater than zero");
        }
        if self.cache_stale_after_days < 0 {
            bail!("Overpass config cacheStaleAfterDays must not be negative");
        }
        Ok(())
    }

    pub(super) fn request_interval(&self) -> Duration {
        Duration::from_secs(self.request_interval_seconds)
    }

    pub(super) fn retry_delay(&self) -> Duration {
        Duration::from_secs(self.retry_delay_seconds)
    }

    pub(super) fn rate_limit_delay(&self) -> Duration {
        Duration::from_secs(self.rate_limit_delay_seconds)
    }

    pub(super) fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.request_timeout_seconds)
    }
}
