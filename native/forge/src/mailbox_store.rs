//! Forge mailbox adapter for the shared conductor-native SQLite store.

use super::queue::PreparedMessage;
use anyhow::Result;
use conductor_native::a2a_store::Store as NativeStore;
pub use conductor_native::a2a_store::{
    registered_identity, validate_identity, validate_message_id, PendingOutbound, Preview,
};
use serde_json::Value;
use std::path::Path;

pub struct Store {
    inner: NativeStore,
}

impl Store {
    pub fn initialize(root: &Path, name: &str) -> Result<Self> {
        Ok(Self {
            inner: NativeStore::initialize(root, name)?,
        })
    }

    pub fn open(root: &Path, name: &str, writable: bool) -> Result<Option<Self>> {
        Ok(NativeStore::open(root, name, writable)?.map(|inner| Self { inner }))
    }

    pub fn required(root: &Path, name: &str, writable: bool) -> Result<Self> {
        Ok(Self {
            inner: NativeStore::required(root, name, writable)?,
        })
    }

    pub fn require_enqueue_schema(&self) -> Result<()> {
        self.inner.require_enqueue_schema()
    }

    pub fn record_inbound(
        &mut self,
        id: &str,
        sender: &str,
        recipient: &str,
        body: &str,
        data_json: Option<&str>,
    ) -> Result<()> {
        self.inner.record_inbound(
            id,
            sender,
            recipient,
            body,
            data_json,
            &crate::instant::isoformat_millis_utc(crate::instant::now()),
        )
    }

    pub fn next_outbound(
        &self,
        recipient: Option<&str>,
        excluded: &[String],
    ) -> Result<Option<PendingOutbound>> {
        self.inner.next_outbound(recipient, excluded)
    }

    pub fn has_outbound(&self, recipient: Option<&str>) -> Result<bool> {
        self.inner.has_outbound(recipient)
    }

    pub fn mark_outbound(&mut self, id: &str, status: &str, reason: Option<&str>) -> Result<()> {
        let now = crate::instant::isoformat_millis_utc(crate::instant::now());
        let received_at = (status == "delivered").then_some(now.as_str());
        self.inner
            .mark_outbound(id, status, reason, &now, received_at)
    }

    pub fn outbound_receipt(&self, id: &str) -> Result<Value> {
        self.inner.outbound_receipt(id)
    }

    pub fn enqueue(&mut self, message: &PreparedMessage, reason: &str) -> Result<Value> {
        self.inner.enqueue(message, reason)
    }

    pub fn previews(
        &self,
        unread: bool,
        limit: usize,
        chars: usize,
    ) -> Result<(Vec<Preview>, i64, i64)> {
        self.inner.previews(unread, limit, chars)
    }

    pub fn message(&mut self, id: &str, direction: &str, max_bytes: usize) -> Result<Value> {
        self.inner.message(id, direction, max_bytes)
    }

    pub fn mark_read(&mut self, id: &str) -> Result<Value> {
        self.inner.mark_read(
            id,
            &crate::instant::isoformat_millis_utc(crate::instant::now()),
        )
    }

    pub fn history(&self, id: Option<&str>, limit: usize) -> Result<Value> {
        self.inner.history(id, limit)
    }
}
