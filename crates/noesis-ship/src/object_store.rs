//! Large blob storage over NATS Object Store.
//!
//! Stores snapshots, artifacts, and other large objects with SHA-256 integrity.
//! Generic `ShipObjectStore` plus two specialized: `BeingSnapshots` and `ArtifactStore`.

use crate::connection::ConnectionManager;
use crate::types::{Error, NatsConfig, Result};
use chrono::Utc;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;

// ─── Metadata Types ─────────────────────────────────────────────────────────

/// Metadata returned after storing an object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub name: String,
    pub bucket: String,
    pub size: usize,
    pub sha256: String,
    pub created: String,
    pub description: Option<String>,
}

// ─── Generic Object Store ───────────────────────────────────────────────────

/// Generic object store over a NATS Object Store bucket.
pub struct ShipObjectStore {
    bucket_name: String,
    conn: Arc<Mutex<ConnectionManager>>,
    store: Arc<Mutex<Option<async_nats::jetstream::object_store::ObjectStore>>>,
}

impl ShipObjectStore {
    /// Create a new ShipObjectStore (not yet connected).
    pub fn new(bucket_name: impl Into<String>, config: NatsConfig) -> Self {
        Self {
            bucket_name: bucket_name.into(),
            conn: Arc::new(Mutex::new(ConnectionManager::new(config))),
            store: Arc::new(Mutex::new(None)),
        }
    }

    /// Connect and get-or-create the object store bucket.
    pub async fn connect(&self) -> Result<()> {
        let mut conn = self.conn.lock().await;
        conn.connect().await?;
        let obj_store = conn.ensure_object_store(&self.bucket_name).await?;
        *self.store.lock().await = Some(obj_store);
        Ok(())
    }

    fn store_ref(
        guard: &Option<async_nats::jetstream::object_store::ObjectStore>,
    ) -> Result<&async_nats::jetstream::object_store::ObjectStore> {
        guard.as_ref().ok_or(Error::NotConnected)
    }

    /// Store bytes with a name and optional description.
    pub async fn put(
        &self,
        name: &str,
        data: &[u8],
        description: Option<&str>,
    ) -> Result<ObjectMeta> {
        let sha256 = hex::encode(Sha256::digest(data));

        let guard = self.store.lock().await;
        let store = Self::store_ref(&guard)?;

        let obj_meta = async_nats::jetstream::object_store::ObjectMetadata {
            name: name.to_string(),
            description: description.map(|s| s.to_string()),
            ..Default::default()
        };

        store
            .put(obj_meta, &mut data.to_vec().as_slice())
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?;

        Ok(ObjectMeta {
            name: name.to_string(),
            bucket: self.bucket_name.clone(),
            size: data.len(),
            sha256,
            created: Utc::now().to_rfc3339(),
            description: description.map(|s| s.to_string()),
        })
    }

    /// Store a file by path.
    pub async fn put_file(
        &self,
        name: &str,
        path: &std::path::Path,
        description: Option<&str>,
    ) -> Result<ObjectMeta> {
        let data = tokio::fs::read(path)
            .await
            .map_err(|e| Error::Connection(format!("file read error: {}", e)))?;
        self.put(name, &data, description).await
    }

    /// Get an object's bytes by name.
    pub async fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let guard = self.store.lock().await;
        let store = Self::store_ref(&guard)?;

        match store.get(name).await {
            Ok(mut obj) => {
                // Object implements AsyncRead
                let mut data = Vec::new();
                obj.read_to_end(&mut data)
                    .await
                    .map_err(|e| Error::JetStream(e.to_string()))?;
                Ok(Some(data))
            }
            Err(_) => Ok(None),
        }
    }

    /// Delete an object by name.
    pub async fn delete(&self, name: &str) -> Result<()> {
        let guard = self.store.lock().await;
        let store = Self::store_ref(&guard)?;
        store
            .delete(name)
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?;
        Ok(())
    }

    /// List all objects in the bucket.
    pub async fn list(&self) -> Result<Vec<async_nats::jetstream::object_store::ObjectInfo>> {
        let guard = self.store.lock().await;
        let store = Self::store_ref(&guard)?;
        let mut entries = store
            .list()
            .await
            .map_err(|e| Error::JetStream(e.to_string()))?;
        let mut result = Vec::new();
        while let Some(entry) = entries.next().await {
            if let Ok(info) = entry {
                result.push(info);
            }
        }
        Ok(result)
    }
}

// ─── Being Snapshots ────────────────────────────────────────────────────────

/// Snapshot info for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotInfo {
    pub snapshot_id: String,
    pub being_id: String,
    pub reason: String,
    pub created: String,
    pub size: usize,
}

/// Being snapshot store — stores agent state snapshots.
///
/// Bucket: `snapshots`. ID format: `{being_id}_{YYYYMMDD_HHMMSS}`.
pub struct BeingSnapshots {
    store: ShipObjectStore,
}

impl BeingSnapshots {
    /// Create a new BeingSnapshots store (not yet connected).
    pub fn new(config: NatsConfig) -> Self {
        Self {
            store: ShipObjectStore::new("snapshots", config),
        }
    }

    /// Connect to NATS.
    pub async fn connect(&self) -> Result<()> {
        self.store.connect().await
    }

    /// Take a snapshot of a being's state.
    pub async fn take(
        &self,
        being_id: &str,
        state: &serde_json::Value,
        reason: &str,
    ) -> Result<String> {
        let now = Utc::now();
        let snapshot_id = format!("{}_{}", being_id, now.format("%Y%m%d_%H%M%S"));
        let envelope = serde_json::json!({
            "being_id": being_id,
            "reason": reason,
            "state": state,
            "created": now.to_rfc3339(),
        });
        let data = serde_json::to_vec(&envelope)?;
        self.store.put(&snapshot_id, &data, Some(reason)).await?;
        Ok(snapshot_id)
    }

    /// Restore a being's state from a snapshot.
    pub async fn restore(&self, snapshot_id: &str) -> Result<Option<serde_json::Value>> {
        match self.store.get(snapshot_id).await? {
            Some(data) => {
                let envelope: serde_json::Value = serde_json::from_slice(&data)?;
                Ok(envelope.get("state").cloned())
            }
            None => Ok(None),
        }
    }

    /// Delete a snapshot.
    pub async fn delete(&self, snapshot_id: &str) -> Result<()> {
        self.store.delete(snapshot_id).await
    }
}

// ─── Artifact Store ─────────────────────────────────────────────────────────

/// Artifact store — stores code, docs, logs, and other work artifacts.
///
/// Bucket: `artifacts`.
pub struct ArtifactStore {
    store: ShipObjectStore,
}

impl ArtifactStore {
    /// Create a new ArtifactStore (not yet connected).
    pub fn new(config: NatsConfig) -> Self {
        Self {
            store: ShipObjectStore::new("artifacts", config),
        }
    }

    /// Connect to NATS.
    pub async fn connect(&self) -> Result<()> {
        self.store.connect().await
    }

    /// Store an artifact.
    pub async fn store_artifact(
        &self,
        name: &str,
        data: &[u8],
        artifact_type: &str,
        expedition: Option<&str>,
    ) -> Result<ObjectMeta> {
        let desc = match expedition {
            Some(exp) => format!("type={}, expedition={}", artifact_type, exp),
            None => format!("type={}", artifact_type),
        };
        self.store.put(name, data, Some(&desc)).await
    }

    /// Get an artifact by name.
    pub async fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.store.get(name).await
    }
}

// ─── hex encoding (minimal, no extra dep) ───────────────────────────────────

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_encode() {
        let data = b"hello world";
        let hash = hex::encode(Sha256::digest(data));
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn object_meta_serialization() {
        let meta = ObjectMeta {
            name: "test.json".to_string(),
            bucket: "artifacts".to_string(),
            size: 42,
            sha256: "abc123".to_string(),
            created: "2026-03-19T00:00:00Z".to_string(),
            description: Some("test artifact".to_string()),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: ObjectMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "test.json");
        assert_eq!(back.size, 42);
    }

    #[test]
    fn snapshot_id_format() {
        let being_id = "mini";
        let now = Utc::now();
        let id = format!("{}_{}", being_id, now.format("%Y%m%d_%H%M%S"));
        assert!(id.starts_with("mini_"));
        assert!(id.len() >= 20);
    }

    #[test]
    fn snapshot_info_serialization() {
        let info = SnapshotInfo {
            snapshot_id: "mini_20260319_120000".to_string(),
            being_id: "mini".to_string(),
            reason: "checkpoint".to_string(),
            created: "2026-03-19T12:00:00Z".to_string(),
            size: 1024,
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: SnapshotInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.snapshot_id, "mini_20260319_120000");
    }

    #[test]
    fn artifact_description_format() {
        let desc = format!("type={}, expedition={}", "code", "EX-3293");
        assert_eq!(desc, "type=code, expedition=EX-3293");
    }

    #[test]
    fn artifact_description_no_expedition() {
        let desc = format!("type={}", "log");
        assert_eq!(desc, "type=log");
    }
}
