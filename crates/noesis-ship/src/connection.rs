//! NATS connection management.
//!
//! Centralized connection handling — one `ConnectionManager` per service,
//! shared by all primitives. Manages the NATS client, JetStream context,
//! and helpers for creating streams, KV buckets, and object stores.

use crate::types::{Error, KvBucketConfig, NatsConfig, Result, StreamConfig};
use std::time::Duration;

/// Centralized NATS connection manager.
///
/// Owns the NATS client and JetStream context. All primitives borrow from this.
/// Not Clone — it owns the connection lifecycle.
pub struct ConnectionManager {
    config: NatsConfig,
    client: Option<async_nats::Client>,
    jetstream: Option<async_nats::jetstream::Context>,
}

impl ConnectionManager {
    /// Create a new ConnectionManager (not yet connected).
    pub fn new(config: NatsConfig) -> Self {
        Self {
            config,
            client: None,
            jetstream: None,
        }
    }

    /// Connect to the NATS server with timeout.
    ///
    /// Creates the NATS client and JetStream context. If already connected,
    /// this is a no-op.
    pub async fn connect(&mut self) -> Result<()> {
        if self.client.is_some() {
            return Ok(());
        }

        let client = tokio::time::timeout(
            self.config.connect_timeout,
            async_nats::connect(&self.config.url),
        )
        .await
        .map_err(|_| Error::Timeout(self.config.connect_timeout))?
        .map_err(|e| Error::Connection(e.to_string()))?;

        let jetstream = async_nats::jetstream::new(client.clone());

        tracing::info!(url = %self.config.url, "connected to NATS");

        self.client = Some(client);
        self.jetstream = Some(jetstream);
        Ok(())
    }

    /// Disconnect from the NATS server, draining pending messages.
    pub async fn disconnect(&mut self) -> Result<()> {
        if let Some(client) = self.client.take() {
            client
                .drain()
                .await
                .map_err(|e| Error::Connection(e.to_string()))?;
            tracing::info!("disconnected from NATS");
        }
        self.jetstream = None;
        Ok(())
    }

    /// Whether the connection is active.
    pub fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    /// Get a reference to the NATS client.
    ///
    /// Returns [`Error::NotConnected`] if not connected.
    pub fn client(&self) -> Result<&async_nats::Client> {
        self.client.as_ref().ok_or(Error::NotConnected)
    }

    /// Get a reference to the JetStream context.
    ///
    /// Returns [`Error::NotConnected`] if not connected.
    pub fn jetstream(&self) -> Result<&async_nats::jetstream::Context> {
        self.jetstream.as_ref().ok_or(Error::NotConnected)
    }

    /// The config this manager was created with.
    pub fn config(&self) -> &NatsConfig {
        &self.config
    }

    /// Ensure a JetStream stream exists, creating it if necessary.
    ///
    /// If the stream already exists, this is a no-op (returns Ok).
    pub async fn ensure_stream(
        &self,
        config: &StreamConfig,
    ) -> Result<async_nats::jetstream::stream::Stream> {
        let js = self.jetstream()?;

        let max_age = Duration::from_secs(config.max_age_secs);
        let storage = match config.storage.as_str() {
            "memory" => async_nats::jetstream::stream::StorageType::Memory,
            _ => async_nats::jetstream::stream::StorageType::File,
        };

        let stream_config = async_nats::jetstream::stream::Config {
            name: config.name.clone(),
            subjects: config.subjects.clone(),
            max_age,
            max_messages: config.max_msgs,
            storage,
            ..Default::default()
        };

        js.get_or_create_stream(stream_config)
            .await
            .map_err(|e| Error::JetStream(e.to_string()))
    }

    /// Ensure a NATS KV bucket exists, creating it if necessary.
    pub async fn ensure_kv_bucket(
        &self,
        config: &KvBucketConfig,
    ) -> Result<async_nats::jetstream::kv::Store> {
        let js = self.jetstream()?;

        let kv_config = async_nats::jetstream::kv::Config {
            bucket: config.bucket.clone(),
            history: config.history as i64,
            max_age: config.ttl.unwrap_or_default(),
            ..Default::default()
        };

        match js.get_key_value(&config.bucket).await {
            Ok(store) => Ok(store),
            Err(_) => js
                .create_key_value(kv_config)
                .await
                .map_err(|e| Error::JetStream(e.to_string())),
        }
    }

    /// Ensure a NATS Object Store bucket exists, creating it if necessary.
    pub async fn ensure_object_store(
        &self,
        bucket: &str,
    ) -> Result<async_nats::jetstream::object_store::ObjectStore> {
        let js = self.jetstream()?;

        let obj_config = async_nats::jetstream::object_store::Config {
            bucket: bucket.to_string(),
            ..Default::default()
        };

        match js.get_object_store(bucket).await {
            Ok(store) => Ok(store),
            Err(_) => js
                .create_object_store(obj_config)
                .await
                .map_err(|e| Error::JetStream(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_not_connected() {
        let config = NatsConfig::default();
        let conn = ConnectionManager::new(config);
        assert!(!conn.is_connected());
    }

    #[test]
    fn client_returns_not_connected_before_connect() {
        let conn = ConnectionManager::new(NatsConfig::default());
        let result = conn.client();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::NotConnected));
    }

    #[test]
    fn jetstream_returns_not_connected_before_connect() {
        let conn = ConnectionManager::new(NatsConfig::default());
        let result = conn.jetstream();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::NotConnected));
    }

    #[test]
    fn config_accessible() {
        let config = NatsConfig::new("nats://192.168.8.110:4222");
        let conn = ConnectionManager::new(config);
        assert_eq!(conn.config().url, "nats://192.168.8.110:4222");
    }
}
