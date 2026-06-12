//! Key-value state management over NATS KV.
//!
//! Provides `KvStore` for generic key-value access, plus specialized stores:
//! `BeingRegistry`, `ShipConfig`, and `HealthMetrics`.

use crate::connection::ConnectionManager;
use crate::types::{Error, KvBucketConfig, NatsConfig, Result};
use chrono::Utc;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

// ─── Generic KV Store ───────────────────────────────────────────────────────

/// Generic key-value store over a NATS KV bucket.
pub struct KvStore {
    bucket_config: KvBucketConfig,
    conn: Arc<Mutex<ConnectionManager>>,
    store: Arc<Mutex<Option<async_nats::jetstream::kv::Store>>>,
}

impl KvStore {
    /// Create a new KvStore (not yet connected).
    pub fn new(bucket_config: KvBucketConfig, config: NatsConfig) -> Self {
        Self {
            bucket_config,
            conn: Arc::new(Mutex::new(ConnectionManager::new(config))),
            store: Arc::new(Mutex::new(None)),
        }
    }

    /// Connect and get-or-create the KV bucket.
    pub async fn connect(&self) -> Result<()> {
        let mut conn = self.conn.lock().await;
        conn.connect().await?;
        let kv = conn.ensure_kv_bucket(&self.bucket_config).await?;
        *self.store.lock().await = Some(kv);
        Ok(())
    }

    fn store_ref(
        guard: &Option<async_nats::jetstream::kv::Store>,
    ) -> Result<&async_nats::jetstream::kv::Store> {
        guard.as_ref().ok_or(Error::NotConnected)
    }

    /// Put a JSON value at a key.
    pub async fn put(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        let data = serde_json::to_vec(value)?;
        let guard = self.store.lock().await;
        let store = Self::store_ref(&guard)?;
        store
            .put(key, data.into())
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?;
        Ok(())
    }

    /// Get a JSON value by key. Returns None if not found.
    pub async fn get(&self, key: &str) -> Result<Option<serde_json::Value>> {
        let guard = self.store.lock().await;
        let store = Self::store_ref(&guard)?;
        // KV get returns Option<Bytes> directly
        match store.get(key).await {
            Ok(Some(bytes)) => {
                let value = serde_json::from_slice(&bytes)?;
                Ok(Some(value))
            }
            Ok(None) => Ok(None),
            Err(_) => Ok(None),
        }
    }

    /// Delete a key.
    pub async fn delete(&self, key: &str) -> Result<()> {
        let guard = self.store.lock().await;
        let store = Self::store_ref(&guard)?;
        store
            .delete(key)
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?;
        Ok(())
    }

    /// List all keys in the bucket.
    pub async fn keys(&self) -> Result<Vec<String>> {
        let guard = self.store.lock().await;
        let store = Self::store_ref(&guard)?;
        let mut keys_stream = store
            .keys()
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?;
        let mut keys = Vec::new();
        while let Some(key) = keys_stream.next().await {
            if let Ok(k) = key {
                keys.push(k);
            }
        }
        Ok(keys)
    }

    /// Watch for changes. Returns a stream of (key, value) updates.
    pub async fn watch(&self) -> Result<impl futures::Stream<Item = (String, serde_json::Value)>> {
        let guard = self.store.lock().await;
        let store = Self::store_ref(&guard)?;
        let watcher = store
            .watch_all()
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?;

        Ok(watcher.filter_map(|entry| async move {
            let entry = entry.ok()?;
            let value = serde_json::from_slice(&entry.value).ok()?;
            Some((entry.key, value))
        }))
    }
}

// ─── Being Registry ─────────────────────────────────────────────────────────

/// Being status states.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BeingState {
    Idle,
    Working,
    Blocked,
    Offline,
}

/// Status record for a being in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeingStatus {
    pub being_id: String,
    pub state: BeingState,
    pub current_task: Option<String>,
    pub last_heartbeat: String,
    pub uptime_seconds: u64,
    pub memory_mb: Option<f64>,
    pub capabilities: Vec<String>,
    pub metadata: Option<serde_json::Value>,
}

/// Being registry — tracks who's online and their status.
///
/// Bucket: `beings_status`, TTL: 300s (5 min), history: 5.
pub struct BeingRegistry {
    store: KvStore,
}

impl BeingRegistry {
    /// Create a new BeingRegistry (not yet connected).
    pub fn new(config: NatsConfig) -> Self {
        let bucket_config = KvBucketConfig::new("beings_status")
            .with_ttl_secs(300)
            .with_history(5);
        Self {
            store: KvStore::new(bucket_config, config),
        }
    }

    /// Connect to NATS.
    pub async fn connect(&self) -> Result<()> {
        self.store.connect().await
    }

    /// Register a being with initial capabilities.
    pub async fn register(&self, being_id: &str, capabilities: Vec<String>) -> Result<()> {
        let status = BeingStatus {
            being_id: being_id.to_string(),
            state: BeingState::Idle,
            current_task: None,
            last_heartbeat: Utc::now().to_rfc3339(),
            uptime_seconds: 0,
            memory_mb: None,
            capabilities,
            metadata: None,
        };
        let value = serde_json::to_value(&status)?;
        self.store.put(being_id, &value).await
    }

    /// Update a being's status.
    pub async fn update_status(
        &self,
        being_id: &str,
        state: BeingState,
        current_task: Option<String>,
    ) -> Result<()> {
        if let Some(mut status) = self.get_status(being_id).await? {
            status.state = state;
            status.current_task = current_task;
            status.last_heartbeat = Utc::now().to_rfc3339();
            let value = serde_json::to_value(&status)?;
            self.store.put(being_id, &value).await
        } else {
            Err(Error::KeyNotFound(being_id.to_string()))
        }
    }

    /// Send a heartbeat for a being.
    pub async fn heartbeat(&self, being_id: &str) -> Result<()> {
        if let Some(mut status) = self.get_status(being_id).await? {
            status.last_heartbeat = Utc::now().to_rfc3339();
            let value = serde_json::to_value(&status)?;
            self.store.put(being_id, &value).await
        } else {
            Err(Error::KeyNotFound(being_id.to_string()))
        }
    }

    /// Unregister a being.
    pub async fn unregister(&self, being_id: &str) -> Result<()> {
        self.store.delete(being_id).await
    }

    /// Get a being's current status.
    pub async fn get_status(&self, being_id: &str) -> Result<Option<BeingStatus>> {
        match self.store.get(being_id).await? {
            Some(value) => {
                let status: BeingStatus = serde_json::from_value(value)?;
                Ok(Some(status))
            }
            None => Ok(None),
        }
    }

    /// Get all currently online beings (those with recent heartbeats).
    pub async fn get_online(&self) -> Result<Vec<BeingStatus>> {
        let keys = self.store.keys().await?;
        let mut online = Vec::new();
        for key in keys {
            if let Some(status) = self.get_status(&key).await? {
                online.push(status);
            }
        }
        Ok(online)
    }
}

// ─── Ship Config ────────────────────────────────────────────────────────────

/// Ship-wide configuration store.
///
/// Bucket: `ship_config`, no TTL, history: 5.
pub struct ShipConfig {
    store: KvStore,
}

impl ShipConfig {
    /// Create a new ShipConfig (not yet connected).
    pub fn new(config: NatsConfig) -> Self {
        let bucket_config = KvBucketConfig::new("ship_config").with_history(5);
        Self {
            store: KvStore::new(bucket_config, config),
        }
    }

    /// Connect to NATS.
    pub async fn connect(&self) -> Result<()> {
        self.store.connect().await
    }

    /// Get a config value, returning default if not found.
    pub async fn get(&self, key: &str, default: serde_json::Value) -> Result<serde_json::Value> {
        Ok(self.store.get(key).await?.unwrap_or(default))
    }

    /// Set a config value.
    pub async fn set(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        self.store.put(key, value).await
    }

    /// Get all config keys.
    pub async fn keys(&self) -> Result<Vec<String>> {
        self.store.keys().await
    }
}

// ─── Health Metrics ─────────────────────────────────────────────────────────

/// Health metrics store with TTL-based expiry.
///
/// Bucket: `ship_health`, TTL: 120s, history: 5.
pub struct HealthMetrics {
    store: KvStore,
}

impl HealthMetrics {
    /// Create a new HealthMetrics store (not yet connected).
    pub fn new(config: NatsConfig) -> Self {
        let bucket_config = KvBucketConfig::new("ship_health")
            .with_ttl_secs(120)
            .with_history(5);
        Self {
            store: KvStore::new(bucket_config, config),
        }
    }

    /// Connect to NATS.
    pub async fn connect(&self) -> Result<()> {
        self.store.connect().await
    }

    /// Report metrics from a source.
    pub async fn report(&self, source: &str, metrics: &serde_json::Value) -> Result<()> {
        self.store.put(source, metrics).await
    }

    /// Get metrics for a source.
    pub async fn get(&self, source: &str) -> Result<Option<serde_json::Value>> {
        self.store.get(source).await
    }

    /// Get all reported metrics.
    pub async fn get_all(&self) -> Result<Vec<(String, serde_json::Value)>> {
        let keys = self.store.keys().await?;
        let mut all = Vec::new();
        for key in keys {
            if let Some(value) = self.store.get(&key).await? {
                all.push((key, value));
            }
        }
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn being_state_serialization() {
        let idle = serde_json::to_string(&BeingState::Idle).unwrap();
        assert_eq!(idle, "\"idle\"");
        let working: BeingState = serde_json::from_str("\"working\"").unwrap();
        assert_eq!(working, BeingState::Working);
    }

    #[test]
    fn being_status_roundtrip() {
        let status = BeingStatus {
            being_id: "mini".to_string(),
            state: BeingState::Working,
            current_task: Some("EX-3292".to_string()),
            last_heartbeat: Utc::now().to_rfc3339(),
            uptime_seconds: 3600,
            memory_mb: Some(1024.0),
            capabilities: vec!["code".to_string(), "test".to_string()],
            metadata: Some(json!({"version": "0.14.0"})),
        };
        let json_str = serde_json::to_string(&status).unwrap();
        let deserialized: BeingStatus = serde_json::from_str(&json_str).unwrap();
        assert_eq!(deserialized.being_id, "mini");
        assert_eq!(deserialized.state, BeingState::Working);
        assert_eq!(deserialized.current_task.as_deref(), Some("EX-3292"));
        assert_eq!(deserialized.capabilities.len(), 2);
    }

    #[test]
    fn kv_bucket_config_beings_status() {
        let config = KvBucketConfig::new("beings_status")
            .with_ttl_secs(300)
            .with_history(5);
        assert_eq!(config.bucket, "beings_status");
        assert_eq!(config.ttl, Some(std::time::Duration::from_secs(300)));
        assert_eq!(config.history, 5);
    }

    #[test]
    fn kv_bucket_config_ship_config() {
        let config = KvBucketConfig::new("ship_config").with_history(5);
        assert!(config.ttl.is_none());
    }

    #[test]
    fn kv_bucket_config_health_metrics() {
        let config = KvBucketConfig::new("ship_health")
            .with_ttl_secs(120)
            .with_history(5);
        assert_eq!(config.ttl, Some(std::time::Duration::from_secs(120)));
    }

    #[test]
    fn being_state_all_variants() {
        let states = vec![
            BeingState::Idle,
            BeingState::Working,
            BeingState::Blocked,
            BeingState::Offline,
        ];
        for state in states {
            let json = serde_json::to_string(&state).unwrap();
            let back: BeingState = serde_json::from_str(&json).unwrap();
            assert_eq!(back, state);
        }
    }
}
