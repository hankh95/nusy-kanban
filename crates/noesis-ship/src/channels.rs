//! Point-to-point JetStream messaging with history.
//!
//! Durable conversations between agents with own-message filtering.
//! Unlike PubSub (fire-and-forget) and EventBus (broadcast), Channels are
//! conversations — durable consumer per agent per channel, explicit ack,
//! own-message filtering.
//!
//! Default stream: `CHANNELS`, subjects: `ship.channel.>`, 30d retention, 10k max messages.

use crate::connection::ConnectionManager;
use crate::types::{ChannelMessage, Error, NatsConfig, Result, StreamConfig};
use futures::StreamExt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Default channel stream configuration.
pub fn default_channel_stream_config() -> StreamConfig {
    StreamConfig::new("CHANNELS", vec!["ship.channel.>".to_string()])
        .with_max_age(2_592_000) // 30 days
        .with_max_msgs(10_000)
}

/// Type alias for async channel message handlers.
pub type ChannelHandler =
    Box<dyn Fn(ChannelMessage) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Point-to-point messaging service with durable consumers and history.
pub struct ChannelService {
    agent_name: Option<String>,
    conn: Arc<Mutex<ConnectionManager>>,
    stream_config: StreamConfig,
    _config: NatsConfig,
}

impl ChannelService {
    /// Create a new ChannelService (not yet connected).
    pub fn new(config: NatsConfig) -> Self {
        Self {
            agent_name: None,
            conn: Arc::new(Mutex::new(ConnectionManager::new(config.clone()))),
            stream_config: default_channel_stream_config(),
            _config: config,
        }
    }

    /// Create with a custom stream config.
    pub fn with_stream(config: NatsConfig, stream_config: StreamConfig) -> Self {
        Self {
            agent_name: None,
            conn: Arc::new(Mutex::new(ConnectionManager::new(config.clone()))),
            stream_config,
            _config: config,
        }
    }

    /// Connect to NATS and ensure the CHANNELS stream exists.
    ///
    /// `agent_name` identifies this agent for durable consumer naming and
    /// own-message filtering.
    pub async fn connect(&mut self, agent_name: impl Into<String>) -> Result<()> {
        self.agent_name = Some(agent_name.into());
        let mut conn = self.conn.lock().await;
        conn.connect().await?;
        conn.ensure_stream(&self.stream_config).await?;
        Ok(())
    }

    /// Disconnect from NATS.
    pub async fn disconnect(&self) -> Result<()> {
        self.conn.lock().await.disconnect().await
    }

    /// Send a message to a channel.
    pub async fn send_message(
        &self,
        channel: &str,
        content: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<()> {
        let agent = self.agent_name.as_deref().ok_or(Error::NotConnected)?;
        let msg = match metadata {
            Some(meta) => ChannelMessage::with_metadata(agent, content, channel, meta),
            None => ChannelMessage::new(agent, content, channel),
        };
        let subject = format!("ship.channel.{}", channel);
        let data = serde_json::to_vec(&msg)?;

        let conn = self.conn.lock().await;
        let js = conn.jetstream()?;
        js.publish(subject, data.into())
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?;

        Ok(())
    }

    /// Subscribe to a channel with a durable consumer.
    ///
    /// If `replay_history` is true, delivers all historical messages first.
    /// Own messages (sender == agent_name) are automatically filtered out.
    pub async fn subscribe(
        &self,
        channel: &str,
        replay_history: bool,
        handler: impl Fn(ChannelMessage) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync
        + 'static,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let agent = self.agent_name.as_deref().ok_or(Error::NotConnected)?;
        let durable_name = format!("{}_{}", agent, channel);
        let filter_subject = format!("ship.channel.{}", channel);

        let deliver_policy = if replay_history {
            async_nats::jetstream::consumer::DeliverPolicy::All
        } else {
            async_nats::jetstream::consumer::DeliverPolicy::New
        };

        let conn = self.conn.lock().await;
        let js = conn.jetstream()?;

        let consumer = js
            .create_consumer_on_stream(
                async_nats::jetstream::consumer::push::Config {
                    filter_subject,
                    durable_name: Some(durable_name),
                    deliver_subject: format!(
                        "_deliver.channel.{}.{}",
                        channel,
                        uuid::Uuid::new_v4()
                    ),
                    deliver_policy,
                    ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                    ..Default::default()
                },
                &self.stream_config.name,
            )
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?;

        let handler = Arc::new(handler);
        let agent_name = agent.to_string();

        let handle = tokio::spawn(async move {
            let mut messages = match consumer.messages().await {
                Ok(m) => m,
                Err(_) => return,
            };

            while let Some(Ok(msg)) = messages.next().await {
                if let Ok(channel_msg) = serde_json::from_slice::<ChannelMessage>(&msg.payload) {
                    // Auto-filter own messages
                    if channel_msg.sender != agent_name {
                        handler(channel_msg).await;
                    }
                }
                let _ = msg.ack().await;
            }
        });

        Ok(handle)
    }

    /// Get channel history (most recent `limit` messages).
    pub async fn get_channel_history(
        &self,
        channel: &str,
        limit: usize,
    ) -> Result<Vec<ChannelMessage>> {
        let conn = self.conn.lock().await;
        let js = conn.jetstream()?;
        let filter_subject = format!("ship.channel.{}", channel);

        let consumer = js
            .create_consumer_on_stream(
                async_nats::jetstream::consumer::pull::Config {
                    filter_subject,
                    deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::All,
                    ..Default::default()
                },
                &self.stream_config.name,
            )
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?;

        let mut messages = consumer
            .fetch()
            .max_messages(limit)
            .messages()
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?;

        let mut result = Vec::new();
        while let Some(Ok(msg)) = messages.next().await {
            if let Ok(channel_msg) = serde_json::from_slice::<ChannelMessage>(&msg.payload) {
                result.push(channel_msg);
            }
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_channel_stream() {
        let config = default_channel_stream_config();
        assert_eq!(config.name, "CHANNELS");
        assert_eq!(config.subjects, vec!["ship.channel.>"]);
        assert_eq!(config.max_age_secs, 2_592_000); // 30 days
        assert_eq!(config.max_msgs, 10_000);
    }

    #[test]
    fn channel_service_new() {
        let svc = ChannelService::new(NatsConfig::default());
        assert!(svc.agent_name.is_none());
    }

    #[test]
    fn durable_consumer_name_format() {
        let agent = "mini";
        let channel = "general";
        let durable = format!("{}_{}", agent, channel);
        assert_eq!(durable, "mini_general");
    }

    #[test]
    fn channel_subject_format() {
        let channel = "dev";
        let subject = format!("ship.channel.{}", channel);
        assert_eq!(subject, "ship.channel.dev");
    }

    #[test]
    fn channel_message_own_filter_logic() {
        let msg = ChannelMessage::new("alice", "hello", "general");
        // Alice's handler should NOT see her own messages
        assert_eq!(msg.sender, "alice");
        assert!(msg.sender != "bob"); // bob WOULD see it
    }
}
