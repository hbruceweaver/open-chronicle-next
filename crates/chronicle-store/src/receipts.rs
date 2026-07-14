use std::collections::HashSet;
use std::time::Duration as StdDuration;

use chronicle_domain::{
    ArtifactId, ArtifactRevisionId, ClientId, DisclosureGrant, GrantId, GrantState,
    McpHealthSummary, QueryOperationKind, RequestId,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    FaultInjector, FaultPoint, GrantReceiptGuard, LockManager, ManagedRoot, Result,
    SharedStoreGuard, StoreError, StoreGeneration,
};

const RECEIPT_PATH: &str = "receipts/disclosure-grants.json";
const MAX_ACTIVE_CURSORS: usize = 512;
const MAX_DERIVED_WRITE_RECEIPTS: usize = 4_096;
const CURSOR_LIFETIME_SECONDS: i64 = 60 * 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DisclosureCursorReceipt {
    token: String,
    grant_id: GrantId,
    client_id: ClientId,
    operation: QueryOperationKind,
    scope_digest: String,
    position: String,
    expires_at: DateTime<Utc>,
    store_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DerivedWriteReceipt {
    request_id: RequestId,
    grant_id: GrantId,
    client_id: ClientId,
    artifact_id: ArtifactId,
    revision_id: ArtifactRevisionId,
    store_generation: u64,
    committed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DisclosureReceiptDocument {
    schema_version: String,
    updated_at: DateTime<Utc>,
    grants: Vec<DisclosureGrant>,
    cursors: Vec<DisclosureCursorReceipt>,
    #[serde(default)]
    derived_writes: Vec<DerivedWriteReceipt>,
}

impl DisclosureReceiptDocument {
    fn empty(now: DateTime<Utc>) -> Self {
        Self {
            schema_version: "1.0".to_owned(),
            updated_at: now,
            grants: Vec::new(),
            cursors: Vec::new(),
            derived_writes: Vec::new(),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != "1.0" {
            return Err(StoreError::InvalidPath(
                "unsupported disclosure receipt version".to_owned(),
            ));
        }
        for grant in &self.grants {
            grant.validate().map_err(StoreError::InvalidPath)?;
        }
        let mut grant_ids = HashSet::new();
        let mut receipt_ids = HashSet::new();
        for grant in &self.grants {
            if !grant_ids.insert(&grant.grant_id) || !receipt_ids.insert(&grant.receipt_id) {
                return Err(StoreError::InvalidPath(
                    "disclosure receipt contains duplicate grant or receipt IDs".to_owned(),
                ));
            }
        }
        let mut cursor_tokens = HashSet::new();
        for cursor in &self.cursors {
            if cursor.token.is_empty()
                || cursor.scope_digest.is_empty()
                || cursor.position.is_empty()
                || cursor.store_generation == 0
                || !cursor_tokens.insert(&cursor.token)
            {
                return Err(StoreError::InvalidPath(
                    "disclosure receipt contains an invalid cursor".to_owned(),
                ));
            }
        }
        if self.derived_writes.len() > MAX_DERIVED_WRITE_RECEIPTS {
            return Err(StoreError::InvalidPath(
                "derived-write receipt limit exceeded".to_owned(),
            ));
        }
        let mut write_request_ids = HashSet::new();
        for write in &self.derived_writes {
            if write.store_generation == 0 || !write_request_ids.insert(&write.request_id) {
                return Err(StoreError::InvalidPath(
                    "disclosure receipt contains an invalid derived-write receipt".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct GrantReceiptStore {
    root: ManagedRoot,
    locks: LockManager,
}

impl GrantReceiptStore {
    pub fn new(root: ManagedRoot, timeout: StdDuration) -> Self {
        Self {
            locks: LockManager::new(root.clone(), timeout),
            root,
        }
    }

    pub fn install(&self, grant: DisclosureGrant, now: DateTime<Utc>) -> Result<()> {
        grant.validate().map_err(StoreError::InvalidPath)?;
        if grant.state != GrantState::Active || grant.disclosed_bytes != 0 {
            return Err(StoreError::InvalidPath(
                "new grants must be active with zero disclosed bytes".to_owned(),
            ));
        }
        let _store_guard = self.locks.shared_request()?;
        let generation = StoreGeneration::load(&self.root)?;
        if grant.store_generation != generation.generation {
            return Err(StoreError::StaleGeneration {
                expected: grant.store_generation,
                actual: generation.generation,
            });
        }
        let _guard = self.locks.grant_receipts()?;
        let mut document = self.load(now)?;
        if document.grants.iter().any(|existing| {
            existing.grant_id == grant.grant_id || existing.receipt_id == grant.receipt_id
        }) {
            return Err(StoreError::GrantAlreadyExists);
        }
        document.grants.push(grant);
        document
            .grants
            .sort_by(|left, right| left.grant_id.cmp(&right.grant_id));
        self.persist(&mut document, now)
    }

    pub fn revoke(
        &self,
        grant_id: &GrantId,
        expected_generation: u64,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let _store_guard = self.locks.shared_request()?;
        let generation = StoreGeneration::load(&self.root)?;
        if generation.generation != expected_generation {
            return Err(StoreError::StaleGeneration {
                expected: expected_generation,
                actual: generation.generation,
            });
        }
        let _guard = self.locks.grant_receipts()?;
        let mut document = self.load(now)?;
        let grant = document
            .grants
            .iter_mut()
            .find(|grant| &grant.grant_id == grant_id)
            .ok_or(StoreError::GrantNotFound)?;
        grant.state = GrantState::Revoked;
        document
            .cursors
            .retain(|cursor| &cursor.grant_id != grant_id);
        document
            .derived_writes
            .retain(|write| &write.grant_id != grant_id);
        self.persist(&mut document, now)
    }

    pub fn grant(&self, grant_id: &GrantId) -> Result<DisclosureGrant> {
        let _guard = self.locks.grant_receipts()?;
        let document = self.load(Utc::now())?;
        document
            .grants
            .into_iter()
            .find(|grant| &grant.grant_id == grant_id)
            .ok_or(StoreError::GrantNotFound)
    }

    pub fn receipt_bytes(&self) -> Result<Vec<u8>> {
        let _guard = self.locks.grant_receipts()?;
        if self.root.exists(RECEIPT_PATH)? {
            self.root.read(RECEIPT_PATH)
        } else {
            Ok(Vec::new())
        }
    }

    pub fn begin_query(
        &self,
        grant_id: &GrantId,
        client_id: &ClientId,
        store_generation: u64,
        now: DateTime<Utc>,
    ) -> Result<GrantQuerySession> {
        let store_guard = self.locks.shared_request()?;
        let generation = StoreGeneration::load(&self.root)?;
        if generation.generation != store_generation {
            return Err(StoreError::StaleGeneration {
                expected: store_generation,
                actual: generation.generation,
            });
        }
        let guard = self.locks.grant_receipts()?;
        let mut document = self.load(now)?;
        document.cursors.retain(|cursor| {
            cursor.expires_at > now && cursor.store_generation == store_generation
        });
        document
            .derived_writes
            .retain(|write| write.store_generation == store_generation);
        let index = document
            .grants
            .iter()
            .position(|grant| &grant.grant_id == grant_id)
            .ok_or(StoreError::GrantNotFound)?;
        let grant = &mut document.grants[index];
        if &grant.client_id != client_id {
            return Err(StoreError::GrantClientMismatch);
        }
        if grant.store_generation != store_generation {
            return Err(StoreError::StaleGeneration {
                expected: grant.store_generation,
                actual: store_generation,
            });
        }
        if !grant.is_active_at(now) {
            if grant.state == GrantState::Active && now >= grant.expires_at {
                grant.state = GrantState::Expired;
                self.persist(&mut document, now)?;
            }
            return Err(StoreError::GrantInactive);
        }
        let original_disclosed_bytes = grant.disclosed_bytes;
        Ok(GrantQuerySession {
            root: self.root.clone(),
            _guard: guard,
            _store_guard: store_guard,
            document,
            grant_index: index,
            original_disclosed_bytes,
            now,
        })
    }

    pub fn health_summary(
        &self,
        now: DateTime<Utc>,
        current_generation: u64,
    ) -> Result<McpHealthSummary> {
        let _guard = self.locks.grant_receipts()?;
        let document = self.load(now)?;
        let mut summary = McpHealthSummary {
            active_grants: 0,
            revoked_grants: 0,
            expired_grants: 0,
            exhausted_grants: 0,
            stale_generation_grants: 0,
        };
        for grant in document.grants {
            if grant.store_generation != current_generation {
                summary.stale_generation_grants = summary.stale_generation_grants.saturating_add(1);
                continue;
            }
            if grant.disclosed_bytes >= grant.limits.max_cumulative_bytes {
                summary.exhausted_grants = summary.exhausted_grants.saturating_add(1);
            }
            match grant.state {
                GrantState::Active if grant.is_active_at(now) => {
                    summary.active_grants = summary.active_grants.saturating_add(1);
                }
                GrantState::Revoked => {
                    summary.revoked_grants = summary.revoked_grants.saturating_add(1);
                }
                GrantState::Expired | GrantState::Active => {
                    summary.expired_grants = summary.expired_grants.saturating_add(1);
                }
            }
        }
        Ok(summary)
    }

    fn load(&self, now: DateTime<Utc>) -> Result<DisclosureReceiptDocument> {
        if !self.root.exists(RECEIPT_PATH)? {
            return Ok(DisclosureReceiptDocument::empty(now));
        }
        let document: DisclosureReceiptDocument =
            serde_json::from_slice(&self.root.read(RECEIPT_PATH)?)?;
        document.validate()?;
        Ok(document)
    }

    fn persist(&self, document: &mut DisclosureReceiptDocument, now: DateTime<Utc>) -> Result<()> {
        document.updated_at = now;
        document.validate()?;
        self.root
            .atomic_write(RECEIPT_PATH, &serde_json::to_vec(document)?)
    }
}

#[derive(Debug)]
pub struct GrantQuerySession {
    root: ManagedRoot,
    _guard: GrantReceiptGuard,
    _store_guard: SharedStoreGuard,
    document: DisclosureReceiptDocument,
    grant_index: usize,
    original_disclosed_bytes: u64,
    now: DateTime<Utc>,
}

impl GrantQuerySession {
    pub fn grant(&self) -> &DisclosureGrant {
        &self.document.grants[self.grant_index]
    }

    pub fn resolve_cursor(
        &self,
        token: &str,
        operation: QueryOperationKind,
        scope_digest: &str,
    ) -> Result<String> {
        let cursor = self
            .document
            .cursors
            .iter()
            .find(|cursor| cursor.token == token)
            .ok_or(StoreError::CursorNotFound)?;
        let grant = self.grant();
        if cursor.grant_id != grant.grant_id
            || cursor.client_id != grant.client_id
            || cursor.operation != operation
            || cursor.scope_digest != scope_digest
            || cursor.store_generation != grant.store_generation
            || cursor.expires_at <= self.now
        {
            return Err(StoreError::CursorScopeMismatch);
        }
        Ok(cursor.position.clone())
    }

    pub fn stage_cursor(
        &mut self,
        operation: QueryOperationKind,
        scope_digest: String,
        position: String,
    ) -> Result<String> {
        if self.document.cursors.len() >= MAX_ACTIVE_CURSORS {
            return Err(StoreError::InvalidPath(
                "active disclosure cursor limit reached".to_owned(),
            ));
        }
        let grant = self.grant().clone();
        let token = format!("cursor-{}", Uuid::now_v7());
        let cursor_expiry = self.now + Duration::seconds(CURSOR_LIFETIME_SECONDS);
        self.document.cursors.push(DisclosureCursorReceipt {
            token: token.clone(),
            grant_id: grant.grant_id,
            client_id: grant.client_id,
            operation,
            scope_digest,
            position,
            expires_at: cursor_expiry.min(grant.expires_at),
            store_generation: grant.store_generation,
        });
        Ok(token)
    }

    pub fn stage_disclosed_bytes(&mut self, response_bytes: u64) -> Result<()> {
        let grant = &mut self.document.grants[self.grant_index];
        if response_bytes > grant.limits.max_response_bytes {
            return Err(StoreError::DisclosureByteLimit);
        }
        let disclosed = self
            .original_disclosed_bytes
            .checked_add(response_bytes)
            .ok_or(StoreError::DisclosureByteLimit)?;
        if disclosed > grant.limits.max_cumulative_bytes {
            return Err(StoreError::DisclosureByteLimit);
        }
        grant.disclosed_bytes = disclosed;
        Ok(())
    }

    /// Returns true only for an already committed retry of the exact same
    /// derived write. Reusing a request ID for different content fails closed.
    pub fn derived_write_committed(
        &self,
        request_id: &RequestId,
        artifact_id: &ArtifactId,
        revision_id: &ArtifactRevisionId,
    ) -> Result<bool> {
        let Some(receipt) = self
            .document
            .derived_writes
            .iter()
            .find(|receipt| &receipt.request_id == request_id)
        else {
            return Ok(false);
        };
        let grant = self.grant();
        if receipt.grant_id != grant.grant_id
            || receipt.client_id != grant.client_id
            || receipt.store_generation != grant.store_generation
            || &receipt.artifact_id != artifact_id
            || &receipt.revision_id != revision_id
        {
            return Err(StoreError::StableIdConflict {
                id: request_id.to_string(),
            });
        }
        Ok(true)
    }

    pub fn repair_receipt_durability(&self, faults: FaultInjector) -> Result<()> {
        self.root.sync_directory("receipts")?;
        faults.check(FaultPoint::AfterArtifactDirectorySync)
    }

    pub fn stage_derived_write(
        &mut self,
        request_id: RequestId,
        artifact_id: ArtifactId,
        revision_id: ArtifactRevisionId,
    ) -> Result<()> {
        if self.document.derived_writes.len() >= MAX_DERIVED_WRITE_RECEIPTS {
            return Err(StoreError::InvalidPath(
                "derived-write receipt limit reached".to_owned(),
            ));
        }
        if self
            .document
            .derived_writes
            .iter()
            .any(|receipt| receipt.request_id == request_id)
        {
            return Err(StoreError::StableIdConflict {
                id: request_id.to_string(),
            });
        }
        let grant = self.grant().clone();
        self.document.derived_writes.push(DerivedWriteReceipt {
            request_id,
            grant_id: grant.grant_id,
            client_id: grant.client_id,
            artifact_id,
            revision_id,
            store_generation: grant.store_generation,
            committed_at: self.now,
        });
        self.document
            .derived_writes
            .sort_by(|left, right| left.request_id.cmp(&right.request_id));
        Ok(())
    }

    pub fn commit(self) -> Result<DisclosureGrant> {
        self.commit_with_faults(FaultInjector::none())
    }

    pub fn commit_with_faults(mut self, faults: FaultInjector) -> Result<DisclosureGrant> {
        self.document.updated_at = self.now;
        self.document.validate()?;
        faults.check(FaultPoint::BeforeTransactionCommit)?;
        self.root.atomic_write_with_boundary(
            RECEIPT_PATH,
            &serde_json::to_vec(&self.document)?,
            || faults.check(FaultPoint::AfterArtifactRename),
        )?;
        Ok(self.document.grants[self.grant_index].clone())
    }
}
