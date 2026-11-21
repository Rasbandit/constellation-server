//! The host's store as the admin API uses it: manifests, uploaded world and player files, and
//! generation statuses, over Postgres or memory. The one place that chooses between the two.

use log::{
    BlobKey, JobState, JobStore, ManifestRecord, ManifestStore, MemLogStore, PgLogStore,
    StoreError, UploadKind, UploadRecord, UploadStore,
};
use std::sync::Arc;
use std::time::Duration;

/// The host's store.
#[derive(Debug, Clone)]
pub(crate) enum Db {
    Pg(Arc<PgLogStore>),
    Mem(Arc<MemLogStore>),
}

/// Run `$call` on whichever store `$db` is.
macro_rules! on {
    ($db:expr, $s:ident => $call:expr) => {
        match $db {
            Db::Pg($s) => $call,
            Db::Mem($s) => $call,
        }
    };
}

impl Db {
    /// Postgres: what is stored here outlives the process.
    pub(crate) const fn is_durable(&self) -> bool {
        matches!(self, Self::Pg(_))
    }

    pub(crate) async fn upload_put(&self, r: &UploadRecord, b: &[u8]) -> Result<(), StoreError> {
        on!(self, s => s.upload_put(r, b).await)
    }

    pub(crate) async fn upload_meta(
        &self,
        kind: UploadKind,
        id: &BlobKey,
    ) -> Result<Option<String>, StoreError> {
        on!(self, s => s.upload_meta(kind, id).await)
    }

    pub(crate) async fn upload_bytes(
        &self,
        kind: UploadKind,
        id: &BlobKey,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        on!(self, s => s.upload_bytes(kind, id).await)
    }

    pub(crate) async fn upload_intact(
        &self,
        kind: UploadKind,
        id: &BlobKey,
    ) -> Result<bool, StoreError> {
        on!(self, s => s.upload_intact(kind, id).await)
    }

    pub(crate) async fn upload_list(&self, kind: UploadKind) -> Result<Vec<String>, StoreError> {
        on!(self, s => s.upload_list(kind).await)
    }

    pub(crate) async fn job_put(
        &self,
        id: &str,
        state: JobState,
        status: &str,
    ) -> Result<(), StoreError> {
        on!(self, s => s.job_put(id, state, status).await)
    }

    pub(crate) async fn job_put_if_absent(
        &self,
        id: &str,
        state: JobState,
        status: &str,
    ) -> Result<bool, StoreError> {
        on!(self, s => s.job_put_if_absent(id, state, status).await)
    }

    pub(crate) async fn job_status(
        &self,
        id: &str,
    ) -> Result<Option<(String, Duration)>, StoreError> {
        on!(self, s => s.job_status(id).await)
    }

    pub(crate) async fn job_touch(&self, id: &str) -> Result<(), StoreError> {
        on!(self, s => s.job_touch(id).await)
    }

    pub(crate) async fn job_fail_all_stale(
        &self,
        older: Duration,
        error: &str,
    ) -> Result<Vec<String>, StoreError> {
        on!(self, s => s.job_fail_all_stale(older, error).await)
    }

    pub(crate) async fn job_fail_stale(
        &self,
        id: &str,
        status: &str,
        older: Duration,
    ) -> Result<bool, StoreError> {
        on!(self, s => s.job_fail_stale(id, status, older).await)
    }

    pub(crate) async fn manifest_put(
        &self,
        r: &ManifestRecord,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        on!(self, s => s.manifest_put(r, bytes).await)
    }

    pub(crate) async fn manifest_record(
        &self,
        id: &BlobKey,
    ) -> Result<Option<ManifestRecord>, StoreError> {
        on!(self, s => s.manifest_record(id).await)
    }

    pub(crate) async fn manifest_bytes(&self, id: &BlobKey) -> Result<Option<Vec<u8>>, StoreError> {
        on!(self, s => s.manifest_bytes(id).await)
    }

    pub(crate) async fn manifest_list(
        &self,
        game: Option<&str>,
        after: Option<&BlobKey>,
        limit: usize,
    ) -> Result<Vec<ManifestRecord>, StoreError> {
        on!(self, s => s.manifest_list(game, after, limit).await)
    }
}
