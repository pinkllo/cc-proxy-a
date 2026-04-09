use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{ModelPricing, ProxyConfig};
use crate::error::ProxyError;
use crate::types::claude::Usage;

#[derive(Clone)]
pub struct HistoryStore {
    inner: Arc<Mutex<HistoryState>>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PersistedRequestLog {
    pub request_id: String,
    pub started_at_epoch_ms: u64,
    pub completed_at_epoch_ms: u64,
    pub latency_ms: u64,
    pub original_model: String,
    pub upstream_model: String,
    pub stream: bool,
    pub success: bool,
    pub status: u16,
    pub upstream_status: Option<u16>,
    pub usage: Usage,
    pub stop_reason: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub claude_request: Value,
    pub openai_request: Value,
    pub response_payload: Option<Value>,
}

#[derive(Clone, Serialize)]
pub struct CostSummary {
    pub total_estimated_cost_usd: f64,
    pub today_estimated_cost_usd: f64,
    pub priced_requests: u64,
    pub unpriced_requests: u64,
}

struct HistoryState {
    path: PathBuf,
    logs: Vec<PersistedRequestLog>,
}

impl HistoryStore {
    pub fn load() -> Result<(Self, Vec<PersistedRequestLog>), ProxyError> {
        let path = history_file_path();
        ensure_parent_dir(&path)?;
        let logs = load_logs(&path)?;
        let store = Self {
            inner: Arc::new(Mutex::new(HistoryState {
                path,
                logs: logs.clone(),
            })),
        };
        Ok((store, logs))
    }

    pub fn append(&self, entry: PersistedRequestLog) -> Result<(), ProxyError> {
        let mut state = self.lock_state();
        append_log_line(&state.path, &entry)?;
        state.logs.push(entry);
        Ok(())
    }

    pub fn recent_logs(&self, limit: usize) -> Vec<PersistedRequestLog> {
        self.lock_state()
            .logs
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn find(&self, request_id: &str) -> Option<PersistedRequestLog> {
        self.lock_state()
            .logs
            .iter()
            .rev()
            .find(|log| log.request_id == request_id)
            .cloned()
    }

    pub fn total_log_count(&self) -> usize {
        self.lock_state().logs.len()
    }

    pub fn cost_summary(
        &self,
        pricing: &std::collections::HashMap<String, ModelPricing>,
    ) -> CostSummary {
        let today_start = current_day_start_epoch_ms();
        let mut total = 0.0;
        let mut today = 0.0;
        let mut priced = 0_u64;
        let mut unpriced = 0_u64;

        for log in &self.lock_state().logs {
            match estimate_cost(log, pricing) {
                Some(cost) => {
                    priced += 1;
                    total += cost;
                    if log.started_at_epoch_ms >= today_start {
                        today += cost;
                    }
                }
                None => unpriced += 1,
            }
        }

        CostSummary {
            total_estimated_cost_usd: round_cost(total),
            today_estimated_cost_usd: round_cost(today),
            priced_requests: priced,
            unpriced_requests: unpriced,
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, HistoryState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub fn estimate_cost(
    log: &PersistedRequestLog,
    pricing: &std::collections::HashMap<String, ModelPricing>,
) -> Option<f64> {
    let model_pricing = pricing.get(&log.upstream_model)?;
    let upstream_cached = log.usage.cache_read_input_tokens.unwrap_or(0) as f64;
    let output_tokens = log.usage.output_tokens as f64;
    let cache_price = model_pricing
        .cache_read_cost_per_million
        .unwrap_or(model_pricing.input_cost_per_million);

    // Use upstream's raw prompt_tokens for cost estimation when available,
    // because the upstream API bills based on their own token count,
    // not our adjusted (tiktoken-compressed) input_tokens.
    let upstream_total_input = log
        .usage
        .upstream_input_tokens
        .unwrap_or(log.usage.input_tokens + log.usage.cache_read_input_tokens.unwrap_or(0))
        as f64;

    // Upstream bills: (total - cached) at full input rate + cached at cache rate
    let non_cached_input = (upstream_total_input - upstream_cached).max(0.0);
    let cost = (non_cached_input * model_pricing.input_cost_per_million
        + upstream_cached * cache_price
        + output_tokens * model_pricing.output_cost_per_million)
        / 1_000_000.0;
    Some(round_cost(cost))
}

fn history_file_path() -> PathBuf {
    ProxyConfig::default_config_path()
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("history")
        .join("requests.jsonl")
}

fn ensure_parent_dir(path: &PathBuf) -> Result<(), ProxyError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(parent)
        .map_err(|error| ProxyError::Internal(format!("Failed to create history dir: {error}")))
}

fn load_logs(path: &PathBuf) -> Result<Vec<PersistedRequestLog>, ProxyError> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let file = fs::File::open(path)
        .map_err(|error| ProxyError::Internal(format!("Failed to read history file: {error}")))?;
    let reader = BufReader::new(file);
    let mut logs = Vec::new();

    for (index, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| {
            ProxyError::Internal(format!(
                "Failed to read history line {}: {error}",
                index + 1
            ))
        })?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<PersistedRequestLog>(&line) {
            Ok(log) => logs.push(log),
            Err(error) => {
                tracing::warn!(
                    "Skipping invalid history entry at line {}: {}",
                    index + 1,
                    error
                );
            }
        }
    }

    Ok(logs)
}

fn append_log_line(path: &PathBuf, entry: &PersistedRequestLog) -> Result<(), ProxyError> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| ProxyError::Internal(format!("Failed to open history file: {error}")))?;
    let line = serde_json::to_string(entry).map_err(|error| {
        ProxyError::Internal(format!("Failed to serialize history entry: {error}"))
    })?;
    writeln!(file, "{line}")
        .map_err(|error| ProxyError::Internal(format!("Failed to append history entry: {error}")))
}

fn current_day_start_epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let day = 24 * 60 * 60;
    (now - now % day) * 1000
}

fn round_cost(value: f64) -> f64 {
    (value * 100_000.0).round() / 100_000.0
}
