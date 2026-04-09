use std::sync::{Arc, RwLock};

use crate::client::UpstreamClient;
use crate::config::ProxyConfig;
use crate::error::ProxyError;

#[derive(Clone)]
pub struct RuntimeHandle {
    inner: Arc<RwLock<RuntimeState>>,
}

#[derive(Clone)]
pub struct RuntimeSnapshot {
    pub config: ProxyConfig,
    pub client: UpstreamClient,
}

struct RuntimeState {
    config: ProxyConfig,
    client: UpstreamClient,
}

impl RuntimeHandle {
    pub fn new(config: ProxyConfig) -> Result<Self, ProxyError> {
        let client = UpstreamClient::new(&config)?;
        Ok(Self {
            inner: Arc::new(RwLock::new(RuntimeState { config, client })),
        })
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        let state = self.read_state();
        RuntimeSnapshot {
            config: state.config.clone(),
            client: state.client.clone(),
        }
    }

    pub fn current_auth_key(&self) -> Option<String> {
        self.read_state().config.anthropic_api_key.clone()
    }

    pub fn update_config(&self, config: ProxyConfig) -> Result<(), ProxyError> {
        let client = UpstreamClient::new(&config)?;
        let mut state = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.config = config;
        state.client = client;
        Ok(())
    }

    fn read_state(&self) -> std::sync::RwLockReadGuard<'_, RuntimeState> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
