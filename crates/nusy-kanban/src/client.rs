//! NATS client mode — send kanban commands to a remote server.
//!
//! When `--server nats://host:port` is set, the CLI sends NATS requests
//! to `kanban.cmd.{command}` instead of operating on local files.

use std::time::Duration;

/// NATS client for the kanban server.
pub struct NatsClient {
    client: async_nats::Client,
    rt: tokio::runtime::Runtime,
}

/// Error from NATS client operations.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("NATS connection failed: {0}")]
    Connect(String),

    #[error("Request failed: {0}")]
    Request(String),

    #[error("Response deserialization failed: {0}")]
    Deserialize(String),

    #[error("Server returned error: {error} (code: {code})")]
    ServerError { error: String, code: String },

    #[error("Request timed out")]
    Timeout,
}

impl NatsClient {
    /// Connect to a NATS server. Returns None if connection fails (for fallback).
    pub fn connect(url: &str) -> Result<Self, ClientError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| ClientError::Connect(format!("Failed to create runtime: {e}")))?;

        let client = rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async_nats::connect(url))
                .await
                .map_err(|_| ClientError::Timeout)?
                .map_err(|e| ClientError::Connect(e.to_string()))
        })?;

        Ok(NatsClient { client, rt })
    }

    /// Send a request to `kanban.cmd.{command}` and return the parsed JSON response.
    pub fn request(
        &self,
        command: &str,
        payload: &serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let subject = format!("kanban.cmd.{command}");
        let body = serde_json::to_vec(payload)
            .map_err(|e| ClientError::Request(format!("Failed to serialize: {e}")))?;

        let response = self.rt.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(30),
                self.client.request(subject, body.into()),
            )
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|e| ClientError::Request(e.to_string()))
        })?;

        let value: serde_json::Value = serde_json::from_slice(&response.payload)
            .map_err(|e| ClientError::Deserialize(e.to_string()))?;

        // Check for server error response
        if let Some(error) = value.get("error") {
            return Err(ClientError::ServerError {
                error: error.as_str().unwrap_or("unknown").to_string(),
                code: value
                    .get("code")
                    .and_then(|c| c.as_str())
                    .unwrap_or("UNKNOWN")
                    .to_string(),
            });
        }

        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connect_invalid_url_fails() {
        // Connect to a non-existent server should fail or timeout
        let result = NatsClient::connect("nats://127.0.0.1:59999");
        assert!(result.is_err());
    }
}
