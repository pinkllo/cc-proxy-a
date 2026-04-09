use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use uuid::Uuid;

use crate::convert::stream::{StreamObserver, StreamSummary};
use crate::error::ProxyError;
use crate::history::PersistedRequestLog;
use crate::types::claude::Usage;

const MAX_RECENT_REQUESTS: usize = 50;
const MAX_RECENT_ERRORS: usize = 20;

#[derive(Clone)]
pub struct StatsCollector {
    inner: Arc<Mutex<StatsState>>,
}

#[derive(Clone)]
pub struct RequestTicket {
    pub request_id: String,
    pub started_at: Instant,
    pub started_at_epoch_ms: u64,
    pub model: String,
    pub upstream_model: String,
    pub stream: bool,
}

#[derive(Clone)]
pub struct RequestCompletion {
    pub success: bool,
    pub http_status: u16,
    pub upstream_status: Option<u16>,
    pub usage: Usage,
    pub stop_reason: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct StatsSnapshot {
    pub started_at_epoch_secs: u64,
    pub total_requests: u64,
    pub active_requests: u64,
    pub streaming_requests: u64,
    pub non_streaming_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_prompt_tokens: u64,
    /// Sum of upstream API's raw prompt_tokens (before cc-proxy adjustment).
    /// This is what the upstream actually bills for.
    pub total_upstream_input_tokens: u64,
    pub cache_hit_ratio: f64,
    pub status_counts: BTreeMap<u16, u64>,
    pub recent_requests: Vec<RequestRecord>,
    pub recent_errors: Vec<ErrorRecord>,
}

#[derive(Clone, Serialize)]
pub struct RequestRecord {
    pub request_id: String,
    pub started_at_epoch_ms: u64,
    pub latency_ms: u64,
    pub model: String,
    pub upstream_model: String,
    pub stream: bool,
    pub success: bool,
    pub status: u16,
    pub upstream_status: Option<u16>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub total_prompt_tokens: u32,
    pub cache_hit_ratio: f64,
    pub stop_reason: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct RequestRecordDetail {
    pub request_id: String,
    pub started_at_epoch_ms: u64,
    pub latency_ms: u64,
    pub model: String,
    pub upstream_model: String,
    pub stream: bool,
    pub success: bool,
    pub status: u16,
    pub upstream_status: Option<u16>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: u32,
    pub total_prompt_tokens: u32,
    pub cache_hit_ratio: f64,
    pub stop_reason: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct ErrorRecord {
    pub request_id: String,
    pub timestamp_epoch_ms: u64,
    pub model: String,
    pub upstream_model: String,
    pub stream: bool,
    pub status: u16,
    pub upstream_status: Option<u16>,
    pub error_code: Option<String>,
    pub message: String,
}

struct StatsState {
    started_at_epoch_secs: u64,
    total_requests: u64,
    active_requests: u64,
    streaming_requests: u64,
    non_streaming_requests: u64,
    successful_requests: u64,
    failed_requests: u64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_cache_read_tokens: u64,
    total_upstream_input_tokens: u64,
    status_counts: BTreeMap<u16, u64>,
    recent_requests: VecDeque<RequestRecord>,
    recent_errors: VecDeque<ErrorRecord>,
}

impl StatsCollector {
    pub fn new(seed_logs: &[PersistedRequestLog]) -> Self {
        let mut state = StatsState::new();
        for log in seed_logs {
            state.apply_log(log);
        }
        Self {
            inner: Arc::new(Mutex::new(state)),
        }
    }

    pub fn begin_request(
        &self,
        model: String,
        upstream_model: String,
        stream: bool,
    ) -> RequestTicket {
        let mut state = self.lock_state();
        state.total_requests += 1;
        state.active_requests += 1;
        if stream {
            state.streaming_requests += 1;
        } else {
            state.non_streaming_requests += 1;
        }
        RequestTicket {
            request_id: Uuid::new_v4().to_string(),
            started_at: Instant::now(),
            started_at_epoch_ms: unix_epoch_ms(),
            model,
            upstream_model,
            stream,
        }
    }

    pub fn finish(&self, ticket: RequestTicket, completion: RequestCompletion) {
        let mut state = self.lock_state();
        state.active_requests = state.active_requests.saturating_sub(1);
        state.apply_completion(&ticket, &completion);
    }

    pub fn finish_error(&self, ticket: RequestTicket, error: &ProxyError) {
        self.finish(ticket, RequestCompletion::from_proxy_error(error));
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        let state = self.lock_state();
        StatsSnapshot {
            started_at_epoch_secs: state.started_at_epoch_secs,
            total_requests: state.total_requests,
            active_requests: state.active_requests,
            streaming_requests: state.streaming_requests,
            non_streaming_requests: state.non_streaming_requests,
            successful_requests: state.successful_requests,
            failed_requests: state.failed_requests,
            total_input_tokens: state.total_input_tokens,
            total_output_tokens: state.total_output_tokens,
            total_cache_read_tokens: state.total_cache_read_tokens,
            total_prompt_tokens: state.total_input_tokens + state.total_cache_read_tokens,
            total_upstream_input_tokens: state.total_upstream_input_tokens,
            cache_hit_ratio: ratio(
                state.total_cache_read_tokens,
                state.total_input_tokens + state.total_cache_read_tokens,
            ),
            status_counts: state.status_counts.clone(),
            recent_requests: state.recent_requests.iter().cloned().collect(),
            recent_errors: state.recent_errors.iter().cloned().collect(),
        }
    }

    pub fn find_request(&self, request_id: &str) -> Option<RequestRecordDetail> {
        self.lock_state()
            .recent_requests
            .iter()
            .find(|record| record.request_id == request_id)
            .map(|record| RequestRecordDetail {
                request_id: record.request_id.clone(),
                started_at_epoch_ms: record.started_at_epoch_ms,
                latency_ms: record.latency_ms,
                model: record.model.clone(),
                upstream_model: record.upstream_model.clone(),
                stream: record.stream,
                success: record.success,
                status: record.status,
                upstream_status: record.upstream_status,
                input_tokens: record.input_tokens,
                output_tokens: record.output_tokens,
                cache_read_input_tokens: record.cache_read_input_tokens,
                total_prompt_tokens: record.total_prompt_tokens,
                cache_hit_ratio: record.cache_hit_ratio,
                stop_reason: record.stop_reason.clone(),
                error_code: record.error_code.clone(),
                error_message: record.error_message.clone(),
            })
    }

    pub fn stream_observer(&self, ticket: RequestTicket) -> StreamObserver {
        let stats = self.clone();
        let shared_ticket = Arc::new(Mutex::new(Some(ticket)));
        Arc::new(move |summary: StreamSummary| {
            let mut guard = shared_ticket
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(ticket) = guard.take() else {
                return;
            };
            let completion = if summary.had_error {
                RequestCompletion::stream_error(
                    summary.usage,
                    summary.stop_reason,
                    summary.error_message,
                )
            } else {
                RequestCompletion::success(200, summary.usage, Some(summary.stop_reason))
            };
            stats.finish(ticket, completion);
        })
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, StatsState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl RequestCompletion {
    pub fn success(http_status: u16, usage: Usage, stop_reason: Option<String>) -> Self {
        Self {
            success: true,
            http_status,
            upstream_status: None,
            usage,
            stop_reason,
            error_code: None,
            error_message: None,
        }
    }

    pub fn from_proxy_error(error: &ProxyError) -> Self {
        Self {
            success: false,
            http_status: error.status_code().as_u16(),
            upstream_status: error.upstream_status(),
            usage: Usage::default(),
            stop_reason: None,
            error_code: error.upstream_error_code().map(str::to_string),
            error_message: Some(error.message_text()),
        }
    }

    pub fn stream_error(usage: Usage, stop_reason: String, error_message: Option<String>) -> Self {
        Self {
            success: false,
            http_status: 200,
            upstream_status: None,
            usage,
            stop_reason: Some(stop_reason),
            error_code: None,
            error_message,
        }
    }
}

impl StatsState {
    fn new() -> Self {
        Self {
            started_at_epoch_secs: unix_epoch_secs(),
            total_requests: 0,
            active_requests: 0,
            streaming_requests: 0,
            non_streaming_requests: 0,
            successful_requests: 0,
            failed_requests: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            total_upstream_input_tokens: 0,
            status_counts: BTreeMap::new(),
            recent_requests: VecDeque::with_capacity(MAX_RECENT_REQUESTS),
            recent_errors: VecDeque::with_capacity(MAX_RECENT_ERRORS),
        }
    }

    fn apply_log(&mut self, log: &PersistedRequestLog) {
        *self.status_counts.entry(log.status).or_default() += 1;
        self.total_requests += 1;
        if log.stream {
            self.streaming_requests += 1;
        } else {
            self.non_streaming_requests += 1;
        }
        self.total_input_tokens += u64::from(log.usage.input_tokens);
        self.total_output_tokens += u64::from(log.usage.output_tokens);
        self.total_cache_read_tokens += u64::from(log.usage.cache_read_input_tokens.unwrap_or(0));
        self.total_upstream_input_tokens +=
            u64::from(log.usage.upstream_input_tokens.unwrap_or(
                log.usage.input_tokens + log.usage.cache_read_input_tokens.unwrap_or(0),
            ));
        if log.success {
            self.successful_requests += 1;
        } else {
            self.failed_requests += 1;
            self.push_error(error_from_log(log));
        }
        self.push_request(record_from_log(log));
    }

    fn apply_completion(&mut self, ticket: &RequestTicket, completion: &RequestCompletion) {
        *self
            .status_counts
            .entry(completion.http_status)
            .or_default() += 1;
        self.total_input_tokens += u64::from(completion.usage.input_tokens);
        self.total_output_tokens += u64::from(completion.usage.output_tokens);
        self.total_cache_read_tokens +=
            u64::from(completion.usage.cache_read_input_tokens.unwrap_or(0));
        self.total_upstream_input_tokens +=
            u64::from(completion.usage.upstream_input_tokens.unwrap_or(
                completion.usage.input_tokens
                    + completion.usage.cache_read_input_tokens.unwrap_or(0),
            ));
        if completion.success {
            self.successful_requests += 1;
        } else {
            self.failed_requests += 1;
            self.push_error(error_from_completion(ticket, completion));
        }
        self.push_request(record_from_completion(ticket, completion));
    }

    fn push_request(&mut self, record: RequestRecord) {
        self.recent_requests.push_front(record);
        if self.recent_requests.len() > MAX_RECENT_REQUESTS {
            self.recent_requests.pop_back();
        }
    }

    fn push_error(&mut self, record: ErrorRecord) {
        self.recent_errors.push_front(record);
        if self.recent_errors.len() > MAX_RECENT_ERRORS {
            self.recent_errors.pop_back();
        }
    }
}

fn record_from_log(log: &PersistedRequestLog) -> RequestRecord {
    RequestRecord {
        request_id: log.request_id.clone(),
        started_at_epoch_ms: log.started_at_epoch_ms,
        latency_ms: log.latency_ms,
        model: log.original_model.clone(),
        upstream_model: log.upstream_model.clone(),
        stream: log.stream,
        success: log.success,
        status: log.status,
        upstream_status: log.upstream_status,
        input_tokens: log.usage.input_tokens,
        output_tokens: log.usage.output_tokens,
        cache_read_input_tokens: log.usage.cache_read_input_tokens.unwrap_or(0),
        total_prompt_tokens: total_prompt_tokens(&log.usage),
        cache_hit_ratio: ratio(
            u64::from(log.usage.cache_read_input_tokens.unwrap_or(0)),
            u64::from(total_prompt_tokens(&log.usage)),
        ),
        stop_reason: log.stop_reason.clone(),
        error_code: log.error_code.clone(),
        error_message: log.error_message.clone(),
    }
}

fn record_from_completion(ticket: &RequestTicket, completion: &RequestCompletion) -> RequestRecord {
    RequestRecord {
        request_id: ticket.request_id.clone(),
        started_at_epoch_ms: ticket.started_at_epoch_ms,
        latency_ms: ticket.started_at.elapsed().as_millis() as u64,
        model: ticket.model.clone(),
        upstream_model: ticket.upstream_model.clone(),
        stream: ticket.stream,
        success: completion.success,
        status: completion.http_status,
        upstream_status: completion.upstream_status,
        input_tokens: completion.usage.input_tokens,
        output_tokens: completion.usage.output_tokens,
        cache_read_input_tokens: completion.usage.cache_read_input_tokens.unwrap_or(0),
        total_prompt_tokens: total_prompt_tokens(&completion.usage),
        cache_hit_ratio: ratio(
            u64::from(completion.usage.cache_read_input_tokens.unwrap_or(0)),
            u64::from(total_prompt_tokens(&completion.usage)),
        ),
        stop_reason: completion.stop_reason.clone(),
        error_code: completion.error_code.clone(),
        error_message: completion.error_message.clone(),
    }
}

fn error_from_log(log: &PersistedRequestLog) -> ErrorRecord {
    ErrorRecord {
        request_id: log.request_id.clone(),
        timestamp_epoch_ms: log.completed_at_epoch_ms,
        model: log.original_model.clone(),
        upstream_model: log.upstream_model.clone(),
        stream: log.stream,
        status: log.status,
        upstream_status: log.upstream_status,
        error_code: log.error_code.clone(),
        message: log
            .error_message
            .clone()
            .unwrap_or_else(|| "Unknown error".into()),
    }
}

fn error_from_completion(ticket: &RequestTicket, completion: &RequestCompletion) -> ErrorRecord {
    ErrorRecord {
        request_id: ticket.request_id.clone(),
        timestamp_epoch_ms: unix_epoch_ms(),
        model: ticket.model.clone(),
        upstream_model: ticket.upstream_model.clone(),
        stream: ticket.stream,
        status: completion.http_status,
        upstream_status: completion.upstream_status,
        error_code: completion.error_code.clone(),
        message: completion
            .error_message
            .clone()
            .unwrap_or_else(|| "Unknown error".into()),
    }
}

fn total_prompt_tokens(usage: &Usage) -> u32 {
    usage.input_tokens + usage.cache_read_input_tokens.unwrap_or(0)
}

fn unix_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn unix_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn ratio(hit: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        hit as f64 / total as f64
    }
}
