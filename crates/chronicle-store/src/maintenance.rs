use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::checksum::{canonical_json, checksum_bytes};
use crate::{LockManager, ManagedRoot, Result, SqliteStore, StoreError, StoreGeneration};

const MAINTENANCE_SCHEMA_VERSION: u32 = 1;
const EVIDENCE_DELETION_RECEIPT: &str = "receipts/evidence-deletion.json";
const EVIDENCE_DELETION_COMPLETION_PROOF: &str = "receipts/evidence-deletion-completion.json";
const MAX_INVENTORY_FILES: usize = 100_000;
const MANAGED_EVIDENCE_TREES: &[(&str, MaintenanceFileClass)] = &[
    ("evidence/events", MaintenanceFileClass::EventJournal),
    ("aggregates/chunks", MaintenanceFileClass::ChunkJournal),
    ("derived", MaintenanceFileClass::DerivedArtifact),
    ("screenshots", MaintenanceFileClass::Screenshot),
    ("diagnostics", MaintenanceFileClass::Diagnostic),
];

thread_local! {
    static MAINTENANCE_FENCE_BYPASS: Cell<bool> = const { Cell::new(false) };
}

struct MaintenanceFenceBypassGuard {
    previous: bool,
}

impl Drop for MaintenanceFenceBypassGuard {
    fn drop(&mut self) {
        MAINTENANCE_FENCE_BYPASS.set(self.previous);
    }
}

pub(crate) fn ensure_normal_store_access(root: &ManagedRoot) -> Result<()> {
    if MAINTENANCE_FENCE_BYPASS.get() || !root.exists(EVIDENCE_DELETION_RECEIPT)? {
        return Ok(());
    }
    let receipt: EvidenceDeletionReceipt =
        serde_json::from_slice(&root.read(EVIDENCE_DELETION_RECEIPT)?)?;
    validate_receipt(&receipt)?;
    match receipt.state {
        EvidenceDeletionState::Prepared => Ok(()),
        EvidenceDeletionState::Complete if completion_proof_matches(root, &receipt)? => Ok(()),
        EvidenceDeletionState::CommitIntent
        | EvidenceDeletionState::Deleting
        | EvidenceDeletionState::PartialFailure
        | EvidenceDeletionState::Complete => Err(StoreError::MaintenanceInProgress),
    }
}

fn with_maintenance_fence_bypass<T>(action: impl FnOnce() -> Result<T>) -> Result<T> {
    let previous = MAINTENANCE_FENCE_BYPASS.replace(true);
    let _guard = MaintenanceFenceBypassGuard { previous };
    action()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaintenanceFileClass {
    EventJournal,
    ChunkJournal,
    DerivedArtifact,
    Screenshot,
    Diagnostic,
    Export,
    Projection,
    JournalIndex,
    Configuration,
    RegistrationReceipt,
    DisclosureGrantReceipt,
    OperationalReceipt,
    StoreGeneration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceFile {
    pub relative_path: String,
    pub class: MaintenanceFileClass,
    pub bytes: u64,
    pub checksum: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceInventory {
    pub schema_version: u32,
    pub store_generation: u64,
    pub files: Vec<MaintenanceFile>,
    pub total_bytes: u64,
    pub digest: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceDeletionOptions {
    pub preserve_exports: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceDeletionPreview {
    pub schema_version: u32,
    pub operation_id: String,
    pub prepared_at: DateTime<Utc>,
    pub store_generation: u64,
    pub next_store_generation: u64,
    pub options: EvidenceDeletionOptions,
    pub deletion: MaintenanceInventory,
    pub preserved: MaintenanceInventory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceDeletionConfirmation {
    operation_id: String,
    store_generation: u64,
    inventory_digest: String,
    confirmed: bool,
}

impl EvidenceDeletionConfirmation {
    pub fn confirmed(preview: &EvidenceDeletionPreview) -> Self {
        Self {
            operation_id: preview.operation_id.clone(),
            store_generation: preview.store_generation,
            inventory_digest: preview.deletion.digest.clone(),
            confirmed: true,
        }
    }

    pub fn unconfirmed(preview: &EvidenceDeletionPreview) -> Self {
        Self {
            operation_id: preview.operation_id.clone(),
            store_generation: preview.store_generation,
            inventory_digest: preview.deletion.digest.clone(),
            confirmed: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceDeletionState {
    Prepared,
    CommitIntent,
    Deleting,
    PartialFailure,
    Complete,
}

impl EvidenceDeletionState {
    pub const fn destructive_commit_started(self) -> bool {
        !matches!(self, Self::Prepared)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceDeletionReceipt {
    pub schema_version: u32,
    pub preview: EvidenceDeletionPreview,
    pub state: EvidenceDeletionState,
    pub committed_generation: Option<u64>,
    pub deleted_relative_paths: Vec<String>,
    pub last_error_code: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct EvidenceDeletionCompletionProof {
    schema_version: u32,
    operation_id: String,
    committed_generation: u64,
    receipt_digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceDeletionResult {
    pub receipt: EvidenceDeletionReceipt,
    pub deleted: MaintenanceInventory,
    pub remaining_evidence: MaintenanceInventory,
    pub preserved: MaintenanceInventory,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactoryResetInventory {
    pub schema_version: u32,
    pub prepared_at: DateTime<Utc>,
    pub removal: MaintenanceInventory,
    pub external_copies_outside_control: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceFaultPoint {
    AfterCommitIntent,
    AfterGenerationIncrement,
    AfterFileDeletion,
    BeforeCompletion,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MaintenanceFaultInjector {
    point: Option<MaintenanceFaultPoint>,
    occurrence: usize,
}

impl MaintenanceFaultInjector {
    pub const fn at(point: MaintenanceFaultPoint) -> Self {
        Self {
            point: Some(point),
            occurrence: 0,
        }
    }

    pub const fn at_occurrence(point: MaintenanceFaultPoint, occurrence: usize) -> Self {
        Self {
            point: Some(point),
            occurrence,
        }
    }

    pub const fn none() -> Self {
        Self {
            point: None,
            occurrence: 0,
        }
    }

    fn check(self, point: MaintenanceFaultPoint, occurrence: usize) -> Result<()> {
        if self.point == Some(point) && self.occurrence == occurrence {
            return Err(StoreError::InvalidPath(format!(
                "injected maintenance boundary: {point:?}"
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct MaintenanceStore {
    root: ManagedRoot,
    locks: LockManager,
}

impl MaintenanceStore {
    pub fn open(root: ManagedRoot, lock_timeout: Duration) -> Result<Self> {
        StoreGeneration::initialize(&root)?;
        Ok(Self {
            locks: LockManager::new(root.clone(), lock_timeout),
            root,
        })
    }

    pub fn evidence_inventory(
        &self,
        options: EvidenceDeletionOptions,
    ) -> Result<MaintenanceInventory> {
        // Inventory promises one coherent point-in-time partition. A shared
        // request guard would still allow another trusted writer to append
        // while the individual files are being hashed.
        let _exclusive = self.locks.exclusive_maintenance()?;
        let generation = StoreGeneration::load(&self.root)?;
        deletion_inventory(&self.root, generation.generation, options, true)
    }

    pub fn prepare_evidence_deletion(
        &self,
        options: EvidenceDeletionOptions,
        prepared_at: DateTime<Utc>,
    ) -> Result<EvidenceDeletionPreview> {
        let _exclusive = self.locks.exclusive_maintenance()?;
        if let Some(receipt) = self.load_receipt()?
            && receipt.state.destructive_commit_started()
            && receipt.state != EvidenceDeletionState::Complete
        {
            return Err(StoreError::InvalidPath(
                "an evidence deletion is already committed and must be resumed".to_owned(),
            ));
        }
        let generation = StoreGeneration::load(&self.root)?;
        let next_store_generation = generation
            .generation
            .checked_add(1)
            .ok_or_else(|| StoreError::InvalidPath("store generation overflow".to_owned()))?;
        let preview = EvidenceDeletionPreview {
            schema_version: MAINTENANCE_SCHEMA_VERSION,
            operation_id: Uuid::now_v7().to_string(),
            prepared_at,
            store_generation: generation.generation,
            next_store_generation,
            options,
            deletion: deletion_inventory(&self.root, generation.generation, options, true)?,
            preserved: preserved_inventory(&self.root, generation.generation, options)?,
        };
        let receipt = EvidenceDeletionReceipt {
            schema_version: MAINTENANCE_SCHEMA_VERSION,
            preview: preview.clone(),
            state: EvidenceDeletionState::Prepared,
            committed_generation: None,
            deleted_relative_paths: Vec::new(),
            last_error_code: None,
            completed_at: None,
        };
        self.persist_receipt(&receipt)?;
        Ok(preview)
    }

    pub fn evidence_deletion_receipt(&self) -> Result<Option<EvidenceDeletionReceipt>> {
        self.load_receipt()
    }

    pub fn finalize_evidence_deletion(
        &self,
        confirmation: EvidenceDeletionConfirmation,
        completed_at: DateTime<Utc>,
        faults: MaintenanceFaultInjector,
    ) -> Result<EvidenceDeletionResult> {
        if !confirmation.confirmed {
            return Err(StoreError::InvalidPath(
                "evidence deletion requires explicit confirmation".to_owned(),
            ));
        }
        let _capture_owner = self.locks.try_capture_owner()?;
        let _exclusive = self.locks.exclusive_maintenance()?;
        let mut receipt = self.load_receipt()?.ok_or_else(|| {
            StoreError::InvalidPath("evidence deletion has no prepared receipt".to_owned())
        })?;
        if receipt.preview.operation_id != confirmation.operation_id
            || receipt.preview.store_generation != confirmation.store_generation
            || receipt.preview.deletion.digest != confirmation.inventory_digest
        {
            return Err(StoreError::InvalidPath(
                "evidence deletion confirmation does not match its preview".to_owned(),
            ));
        }
        self.drive_evidence_deletion(&mut receipt, completed_at, faults)
    }

    pub fn resume_evidence_deletion(
        &self,
        completed_at: DateTime<Utc>,
        faults: MaintenanceFaultInjector,
    ) -> Result<EvidenceDeletionResult> {
        let _capture_owner = self.locks.try_capture_owner()?;
        let _exclusive = self.locks.exclusive_maintenance()?;
        let mut receipt = self.load_receipt()?.ok_or_else(|| {
            StoreError::InvalidPath("evidence deletion has no durable receipt".to_owned())
        })?;
        if !receipt.state.destructive_commit_started() {
            return Err(StoreError::InvalidPath(
                "prepared evidence deletion still requires confirmation".to_owned(),
            ));
        }
        self.drive_evidence_deletion(&mut receipt, completed_at, faults)
    }

    pub fn factory_reset_inventory(
        &self,
        prepared_at: DateTime<Utc>,
    ) -> Result<FactoryResetInventory> {
        let _exclusive = self.locks.exclusive_maintenance()?;
        let generation = StoreGeneration::load(&self.root)?;
        Ok(FactoryResetInventory {
            schema_version: MAINTENANCE_SCHEMA_VERSION,
            prepared_at,
            removal: factory_inventory(&self.root, generation.generation)?,
            external_copies_outside_control: true,
        })
    }

    fn drive_evidence_deletion(
        &self,
        receipt: &mut EvidenceDeletionReceipt,
        completed_at: DateTime<Utc>,
        faults: MaintenanceFaultInjector,
    ) -> Result<EvidenceDeletionResult> {
        validate_receipt(receipt)?;
        if receipt.state == EvidenceDeletionState::Complete {
            return self.completed_result(receipt.clone());
        }
        if receipt.state == EvidenceDeletionState::Prepared {
            let current = StoreGeneration::load(&self.root)?;
            if current.generation != receipt.preview.store_generation {
                return Err(StoreError::StaleGeneration {
                    expected: receipt.preview.store_generation,
                    actual: current.generation,
                });
            }
            let current_inventory = deletion_inventory(
                &self.root,
                current.generation,
                receipt.preview.options,
                true,
            )?;
            if current_inventory != receipt.preview.deletion {
                return Err(StoreError::InvalidPath(
                    "evidence deletion preview is stale".to_owned(),
                ));
            }
            receipt.state = EvidenceDeletionState::CommitIntent;
            receipt.last_error_code = None;
            self.persist_receipt(receipt)?;
            faults.check(MaintenanceFaultPoint::AfterCommitIntent, 0)?;
        }

        if receipt.state == EvidenceDeletionState::CommitIntent {
            let current = StoreGeneration::load(&self.root)?;
            if current.generation == receipt.preview.store_generation {
                let next = with_maintenance_fence_bypass(|| current.increment(&self.root))?;
                if next.generation != receipt.preview.next_store_generation {
                    return Err(StoreError::StaleGeneration {
                        expected: receipt.preview.next_store_generation,
                        actual: next.generation,
                    });
                }
                faults.check(MaintenanceFaultPoint::AfterGenerationIncrement, 0)?;
            } else if current.generation != receipt.preview.next_store_generation {
                return Err(StoreError::StaleGeneration {
                    expected: receipt.preview.next_store_generation,
                    actual: current.generation,
                });
            }
            receipt.state = EvidenceDeletionState::Deleting;
            receipt.committed_generation = Some(receipt.preview.next_store_generation);
            receipt.last_error_code = None;
            self.persist_receipt(receipt)?;
        }

        if matches!(
            receipt.state,
            EvidenceDeletionState::Deleting | EvidenceDeletionState::PartialFailure
        ) {
            let current = StoreGeneration::load(&self.root)?;
            if current.generation != receipt.preview.next_store_generation {
                return Err(StoreError::StaleGeneration {
                    expected: receipt.preview.next_store_generation,
                    actual: current.generation,
                });
            }
            receipt.state = EvidenceDeletionState::Deleting;
            receipt.last_error_code = None;
            self.persist_receipt(receipt)?;
            for (occurrence, item) in receipt
                .preview
                .deletion
                .files
                .clone()
                .into_iter()
                .enumerate()
            {
                if receipt
                    .deleted_relative_paths
                    .iter()
                    .any(|path| path == &item.relative_path)
                {
                    continue;
                }
                if self.root.exists(&item.relative_path)? {
                    let current_item = inventory_file(&self.root, &item.relative_path, item.class)?;
                    if current_item != item {
                        receipt.state = EvidenceDeletionState::PartialFailure;
                        receipt.last_error_code =
                            Some("managed-file-changed-after-commit".to_owned());
                        self.persist_receipt(receipt)?;
                        return Err(StoreError::InvalidPath(
                            "a committed managed file changed before deletion".to_owned(),
                        ));
                    }
                    self.root.unlink(&item.relative_path)?;
                }
                if !receipt
                    .deleted_relative_paths
                    .iter()
                    .any(|path| path == &item.relative_path)
                {
                    receipt
                        .deleted_relative_paths
                        .push(item.relative_path.clone());
                    receipt.deleted_relative_paths.sort();
                    self.persist_receipt(receipt)?;
                }
                faults.check(MaintenanceFaultPoint::AfterFileDeletion, occurrence)?;
            }
            with_maintenance_fence_bypass(|| {
                let fresh = SqliteStore::open(self.root.clone())?;
                fresh.checkpoint()?;
                ensure_empty_projection(&fresh)?;
                Ok(())
            })?;
            let remaining = deletion_inventory(
                &self.root,
                current.generation,
                receipt.preview.options,
                false,
            )?;
            if !remaining.files.is_empty() {
                receipt.state = EvidenceDeletionState::PartialFailure;
                receipt.last_error_code = Some("managed-evidence-remains".to_owned());
                self.persist_receipt(receipt)?;
                let paths = remaining
                    .files
                    .iter()
                    .map(|item| item.relative_path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(StoreError::InvalidPath(format!(
                    "managed evidence remains after deletion: {paths}"
                )));
            }
            ensure_complete_deleted_path_coverage(receipt)?;
            faults.check(MaintenanceFaultPoint::BeforeCompletion, 0)?;
            receipt.state = EvidenceDeletionState::Complete;
            receipt.completed_at = Some(completed_at);
            receipt.last_error_code = None;
            self.persist_receipt(receipt)?;
        }
        self.completed_result(receipt.clone())
    }

    fn completed_result(&self, receipt: EvidenceDeletionReceipt) -> Result<EvidenceDeletionResult> {
        validate_receipt(&receipt)?;
        let generation = StoreGeneration::load(&self.root)?;
        if generation.generation != receipt.preview.next_store_generation {
            return Err(StoreError::StaleGeneration {
                expected: receipt.preview.next_store_generation,
                actual: generation.generation,
            });
        }
        if receipt.state != EvidenceDeletionState::Complete {
            return Err(StoreError::InvalidPath(
                "evidence deletion receipt is not complete".to_owned(),
            ));
        }
        ensure_complete_deleted_path_coverage(&receipt)?;
        let remaining_evidence = deletion_inventory(
            &self.root,
            generation.generation,
            receipt.preview.options,
            false,
        )?;
        if !remaining_evidence.files.is_empty() {
            return Err(StoreError::InvalidPath(
                "completed evidence deletion still has managed evidence".to_owned(),
            ));
        }
        with_maintenance_fence_bypass(|| {
            let sqlite = SqliteStore::open(self.root.clone())?;
            ensure_empty_projection(&sqlite)
        })?;
        self.persist_completion_proof(&receipt)?;
        let preserved =
            preserved_inventory(&self.root, generation.generation, receipt.preview.options)?;
        Ok(EvidenceDeletionResult {
            deleted: receipt.preview.deletion.clone(),
            remaining_evidence,
            preserved,
            receipt,
        })
    }

    fn load_receipt(&self) -> Result<Option<EvidenceDeletionReceipt>> {
        if !self.root.exists(EVIDENCE_DELETION_RECEIPT)? {
            return Ok(None);
        }
        let receipt: EvidenceDeletionReceipt =
            serde_json::from_slice(&self.root.read(EVIDENCE_DELETION_RECEIPT)?)?;
        validate_receipt(&receipt)?;
        Ok(Some(receipt))
    }

    fn persist_receipt(&self, receipt: &EvidenceDeletionReceipt) -> Result<()> {
        validate_receipt(receipt)?;
        self.root
            .atomic_write(EVIDENCE_DELETION_RECEIPT, &canonical_json(receipt)?)
    }

    fn persist_completion_proof(&self, receipt: &EvidenceDeletionReceipt) -> Result<()> {
        let proof = completion_proof(receipt)?;
        self.root
            .atomic_write(EVIDENCE_DELETION_COMPLETION_PROOF, &canonical_json(&proof)?)
    }
}

fn completion_proof(receipt: &EvidenceDeletionReceipt) -> Result<EvidenceDeletionCompletionProof> {
    if receipt.state != EvidenceDeletionState::Complete {
        return Err(StoreError::InvalidPath(
            "completion proof requires a complete evidence deletion".to_owned(),
        ));
    }
    Ok(EvidenceDeletionCompletionProof {
        schema_version: MAINTENANCE_SCHEMA_VERSION,
        operation_id: receipt.preview.operation_id.clone(),
        committed_generation: receipt.preview.next_store_generation,
        receipt_digest: checksum_bytes(&canonical_json(receipt)?),
    })
}

fn completion_proof_matches(root: &ManagedRoot, receipt: &EvidenceDeletionReceipt) -> Result<bool> {
    if !root.exists(EVIDENCE_DELETION_COMPLETION_PROOF)? {
        return Ok(false);
    }
    let proof: EvidenceDeletionCompletionProof =
        serde_json::from_slice(&root.read(EVIDENCE_DELETION_COMPLETION_PROOF)?)?;
    if proof != completion_proof(receipt)? {
        return Ok(false);
    }
    Ok(StoreGeneration::load(root)?.generation == proof.committed_generation)
}

fn validate_receipt(receipt: &EvidenceDeletionReceipt) -> Result<()> {
    if receipt.schema_version != MAINTENANCE_SCHEMA_VERSION
        || receipt.preview.schema_version != MAINTENANCE_SCHEMA_VERSION
        || receipt.preview.operation_id.is_empty()
        || receipt.preview.next_store_generation
            != receipt
                .preview
                .store_generation
                .checked_add(1)
                .ok_or_else(|| StoreError::InvalidPath("store generation overflow".to_owned()))?
    {
        return Err(StoreError::InvalidPath(
            "invalid evidence deletion receipt".to_owned(),
        ));
    }
    validate_inventory(&receipt.preview.deletion, receipt.preview.store_generation)?;
    validate_inventory(&receipt.preview.preserved, receipt.preview.store_generation)?;
    if receipt
        .preview
        .deletion
        .files
        .iter()
        .any(|item| !valid_deletion_item(item, receipt.preview.options))
        || receipt
            .preview
            .preserved
            .files
            .iter()
            .any(|item| !valid_preserved_item(item, receipt.preview.options))
    {
        return Err(StoreError::InvalidPath(
            "evidence deletion receipt contains an invalid file classification".to_owned(),
        ));
    }
    let deletion_paths = receipt
        .preview
        .deletion
        .files
        .iter()
        .map(|item| item.relative_path.as_str())
        .collect::<BTreeSet<_>>();
    if receipt
        .preview
        .preserved
        .files
        .iter()
        .any(|item| deletion_paths.contains(item.relative_path.as_str()))
    {
        return Err(StoreError::InvalidPath(
            "evidence deletion receipt deletes a preserved path".to_owned(),
        ));
    }
    if receipt.state == EvidenceDeletionState::Prepared
        && (receipt.committed_generation.is_some() || !receipt.deleted_relative_paths.is_empty())
    {
        return Err(StoreError::InvalidPath(
            "prepared evidence deletion receipt contains committed state".to_owned(),
        ));
    }
    if matches!(
        receipt.state,
        EvidenceDeletionState::Deleting
            | EvidenceDeletionState::PartialFailure
            | EvidenceDeletionState::Complete
    ) && receipt.committed_generation != Some(receipt.preview.next_store_generation)
    {
        return Err(StoreError::InvalidPath(
            "committed evidence deletion receipt has no generation proof".to_owned(),
        ));
    }
    if receipt.state == EvidenceDeletionState::Complete && receipt.completed_at.is_none() {
        return Err(StoreError::InvalidPath(
            "completed evidence deletion receipt has no completion time".to_owned(),
        ));
    }
    let preview_paths = receipt
        .preview
        .deletion
        .files
        .iter()
        .map(|item| item.relative_path.as_str())
        .collect::<BTreeSet<_>>();
    let deleted_paths = receipt
        .deleted_relative_paths
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if deleted_paths.len() != receipt.deleted_relative_paths.len()
        || !deleted_paths.is_subset(&preview_paths)
        || receipt.state == EvidenceDeletionState::Complete && deleted_paths != preview_paths
    {
        return Err(StoreError::InvalidPath(
            "evidence deletion receipt contains invalid deleted paths".to_owned(),
        ));
    }
    Ok(())
}

fn validate_inventory(inventory: &MaintenanceInventory, generation: u64) -> Result<()> {
    if inventory.schema_version != MAINTENANCE_SCHEMA_VERSION
        || inventory.store_generation != generation
        || inventory.files.len() > MAX_INVENTORY_FILES
        || inventory.files.iter().any(|item| {
            !valid_relative_path(&item.relative_path)
                || item.checksum.len() != 64
                || !item
                    .checksum
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        || inventory
            .files
            .windows(2)
            .any(|pair| pair[0].relative_path >= pair[1].relative_path)
    {
        return Err(StoreError::InvalidPath(
            "invalid maintenance inventory".to_owned(),
        ));
    }
    let total_bytes = inventory.files.iter().try_fold(0_u64, |total, item| {
        total
            .checked_add(item.bytes)
            .ok_or_else(|| StoreError::InvalidPath("inventory byte count overflow".to_owned()))
    })?;
    if total_bytes != inventory.total_bytes
        || checksum_bytes(&canonical_json(&inventory.files)?) != inventory.digest
    {
        return Err(StoreError::InvalidPath(
            "maintenance inventory digest or byte count is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn valid_deletion_item(item: &MaintenanceFile, options: EvidenceDeletionOptions) -> bool {
    match item.class {
        MaintenanceFileClass::EventJournal => under(&item.relative_path, "evidence/events"),
        MaintenanceFileClass::ChunkJournal => under(&item.relative_path, "aggregates/chunks"),
        MaintenanceFileClass::DerivedArtifact => under(&item.relative_path, "derived"),
        MaintenanceFileClass::Screenshot => under(&item.relative_path, "screenshots"),
        MaintenanceFileClass::Diagnostic => under(&item.relative_path, "diagnostics"),
        MaintenanceFileClass::Export => {
            !options.preserve_exports && under(&item.relative_path, "exports")
        }
        MaintenanceFileClass::Projection => projection_path(&item.relative_path),
        MaintenanceFileClass::JournalIndex => {
            item.relative_path.starts_with("receipts/journal-")
                && !item.relative_path["receipts/".len()..].contains('/')
                && item.bytes > 0
        }
        MaintenanceFileClass::Configuration
        | MaintenanceFileClass::RegistrationReceipt
        | MaintenanceFileClass::DisclosureGrantReceipt
        | MaintenanceFileClass::OperationalReceipt
        | MaintenanceFileClass::StoreGeneration => false,
    }
}

fn valid_preserved_item(item: &MaintenanceFile, options: EvidenceDeletionOptions) -> bool {
    match item.class {
        MaintenanceFileClass::Configuration => item.relative_path == "config.json",
        MaintenanceFileClass::StoreGeneration => item.relative_path == "store-generation",
        MaintenanceFileClass::RegistrationReceipt => {
            item.relative_path == "receipts/agent-registrations.json"
        }
        MaintenanceFileClass::DisclosureGrantReceipt => {
            item.relative_path == "receipts/disclosure-grants.json"
        }
        MaintenanceFileClass::OperationalReceipt => {
            under(&item.relative_path, "receipts")
                && !item.relative_path.starts_with("receipts/journal-")
        }
        MaintenanceFileClass::Export => {
            options.preserve_exports && under(&item.relative_path, "exports")
        }
        MaintenanceFileClass::JournalIndex => {
            item.relative_path.starts_with("receipts/journal-")
                && !item.relative_path["receipts/".len()..].contains('/')
                && item.bytes == 0
        }
        MaintenanceFileClass::EventJournal
        | MaintenanceFileClass::ChunkJournal
        | MaintenanceFileClass::DerivedArtifact
        | MaintenanceFileClass::Screenshot
        | MaintenanceFileClass::Diagnostic
        | MaintenanceFileClass::Projection => false,
    }
}

fn valid_relative_path(relative: &str) -> bool {
    !relative.is_empty()
        && !relative.contains('\\')
        && !Path::new(relative).is_absolute()
        && Path::new(relative)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn under(relative: &str, directory: &str) -> bool {
    relative
        .strip_prefix(directory)
        .is_some_and(|suffix| suffix.starts_with('/') && suffix.len() > 1)
}

fn projection_path(relative: &str) -> bool {
    matches!(
        relative,
        "index.sqlite3" | "index.sqlite3-wal" | "index.sqlite3-shm" | "index.sqlite3-journal"
    ) || relative.starts_with("index.rebuild-") && !relative.contains('/')
}

fn deletion_inventory(
    root: &ManagedRoot,
    generation: u64,
    options: EvidenceDeletionOptions,
    include_projection: bool,
) -> Result<MaintenanceInventory> {
    let mut files = Vec::new();
    for (tree, class) in MANAGED_EVIDENCE_TREES {
        collect_tree(root, tree, *class, &mut files)?;
    }
    if !options.preserve_exports {
        collect_tree(root, "exports", MaintenanceFileClass::Export, &mut files)?;
    }
    collect_journal_receipts(root, &mut files)?;
    if include_projection {
        collect_projection_files(root, &mut files)?;
    }
    build_inventory(generation, files)
}

fn preserved_inventory(
    root: &ManagedRoot,
    generation: u64,
    options: EvidenceDeletionOptions,
) -> Result<MaintenanceInventory> {
    let mut files = Vec::new();
    collect_optional(
        root,
        "config.json",
        MaintenanceFileClass::Configuration,
        &mut files,
    )?;
    collect_optional(
        root,
        "store-generation",
        MaintenanceFileClass::StoreGeneration,
        &mut files,
    )?;
    collect_nonjournal_receipts(root, &mut files)?;
    collect_empty_journal_receipts(root, &mut files)?;
    if options.preserve_exports {
        collect_tree(root, "exports", MaintenanceFileClass::Export, &mut files)?;
    }
    build_inventory(generation, files)
}

fn factory_inventory(root: &ManagedRoot, generation: u64) -> Result<MaintenanceInventory> {
    let mut files = Vec::new();
    for (tree, class) in MANAGED_EVIDENCE_TREES {
        collect_tree(root, tree, *class, &mut files)?;
    }
    collect_tree(root, "exports", MaintenanceFileClass::Export, &mut files)?;
    collect_all_receipts(root, &mut files)?;
    collect_optional(
        root,
        "config.json",
        MaintenanceFileClass::Configuration,
        &mut files,
    )?;
    collect_optional(
        root,
        "store-generation",
        MaintenanceFileClass::StoreGeneration,
        &mut files,
    )?;
    collect_projection_files(root, &mut files)?;
    build_inventory(generation, files)
}

fn collect_projection_files(root: &ManagedRoot, files: &mut Vec<MaintenanceFile>) -> Result<()> {
    for entry in fs::read_dir(root.path())? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            StoreError::InvalidPath("managed root file name is not UTF-8".to_owned())
        })?;
        let projection = matches!(
            name.as_str(),
            "index.sqlite3" | "index.sqlite3-wal" | "index.sqlite3-shm" | "index.sqlite3-journal"
        ) || name.starts_with("index.rebuild-");
        if projection {
            collect_optional(root, &name, MaintenanceFileClass::Projection, files)?;
        }
    }
    Ok(())
}

fn collect_journal_receipts(root: &ManagedRoot, files: &mut Vec<MaintenanceFile>) -> Result<()> {
    for entry in fs::read_dir(root.path().join("receipts"))? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| StoreError::InvalidPath("receipt file name is not UTF-8".to_owned()))?;
        if name.starts_with("journal-") {
            let relative = format!("receipts/{name}");
            let item = inventory_file(root, &relative, MaintenanceFileClass::JournalIndex)?;
            // Opening a fresh projection scans the empty canonical journal and
            // durably recreates a zero-byte index. That file contains no
            // evidence and is safe to retain as empty operational state.
            if item.bytes > 0 || !name.ends_with("-index.jsonl") {
                files.push(item);
                if files.len() > MAX_INVENTORY_FILES {
                    return Err(StoreError::InvalidPath(
                        "maintenance inventory file limit exceeded".to_owned(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn collect_empty_journal_receipts(
    root: &ManagedRoot,
    files: &mut Vec<MaintenanceFile>,
) -> Result<()> {
    for entry in fs::read_dir(root.path().join("receipts"))? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StoreError::InvalidPath(
                "receipt inventory contains a non-regular object".to_owned(),
            ));
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| StoreError::InvalidPath("receipt file name is not UTF-8".to_owned()))?;
        if name.starts_with("journal-") && name.ends_with("-index.jsonl") {
            let relative = format!("receipts/{name}");
            let item = inventory_file(root, &relative, MaintenanceFileClass::JournalIndex)?;
            if item.bytes == 0 {
                push_inventory_file(files, item)?;
            }
        }
    }
    Ok(())
}

fn collect_all_receipts(root: &ManagedRoot, files: &mut Vec<MaintenanceFile>) -> Result<()> {
    for entry in fs::read_dir(root.path().join("receipts"))? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StoreError::InvalidPath(
                "receipt inventory contains a non-regular object".to_owned(),
            ));
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| StoreError::InvalidPath("receipt file name is not UTF-8".to_owned()))?;
        let class = if name.starts_with("journal-") {
            MaintenanceFileClass::JournalIndex
        } else {
            receipt_class(&name)
        };
        collect_optional(root, &format!("receipts/{name}"), class, files)?;
    }
    Ok(())
}

fn collect_nonjournal_receipts(root: &ManagedRoot, files: &mut Vec<MaintenanceFile>) -> Result<()> {
    for entry in fs::read_dir(root.path().join("receipts"))? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StoreError::InvalidPath(
                "receipt inventory contains a non-regular object".to_owned(),
            ));
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| StoreError::InvalidPath("receipt file name is not UTF-8".to_owned()))?;
        if name.starts_with("journal-") {
            continue;
        }
        let class = receipt_class(&name);
        collect_optional(root, &format!("receipts/{name}"), class, files)?;
    }
    Ok(())
}

fn collect_tree(
    root: &ManagedRoot,
    relative: &str,
    class: MaintenanceFileClass,
    files: &mut Vec<MaintenanceFile>,
) -> Result<()> {
    let path = root.path().join(relative);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StoreError::InvalidPath(
            "managed inventory tree is not a regular directory".to_owned(),
        ));
    }
    collect_tree_at(root, &path, class, files)
}

fn collect_tree_at(
    root: &ManagedRoot,
    path: &Path,
    class: MaintenanceFileClass,
    files: &mut Vec<MaintenanceFile>,
) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            return Err(StoreError::InvalidPath(
                "managed inventory contains a symbolic link".to_owned(),
            ));
        }
        if metadata.is_dir() {
            collect_tree_at(root, &entry.path(), class, files)?;
        } else if metadata.is_file() {
            let relative = entry
                .path()
                .strip_prefix(root.path())
                .map_err(|_| {
                    StoreError::InvalidPath("managed inventory escaped its root".to_owned())
                })?
                .to_str()
                .ok_or_else(|| {
                    StoreError::InvalidPath("managed inventory path is not UTF-8".to_owned())
                })?
                .to_owned();
            files.push(inventory_file(root, &relative, class)?);
            if files.len() > MAX_INVENTORY_FILES {
                return Err(StoreError::InvalidPath(
                    "maintenance inventory file limit exceeded".to_owned(),
                ));
            }
        } else {
            return Err(StoreError::InvalidPath(
                "managed inventory contains a non-regular object".to_owned(),
            ));
        }
    }
    Ok(())
}

fn collect_optional(
    root: &ManagedRoot,
    relative: &str,
    class: MaintenanceFileClass,
    files: &mut Vec<MaintenanceFile>,
) -> Result<()> {
    match inventory_file(root, relative, class) {
        Ok(item) => {
            files.push(item);
            if files.len() > MAX_INVENTORY_FILES {
                return Err(StoreError::InvalidPath(
                    "maintenance inventory file limit exceeded".to_owned(),
                ));
            }
            Ok(())
        }
        Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn inventory_file(
    root: &ManagedRoot,
    relative: &str,
    class: MaintenanceFileClass,
) -> Result<MaintenanceFile> {
    let mut file = root.open_file(relative, false, false, false)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(StoreError::InvalidPath(
            "maintenance inventory contains a non-file".to_owned(),
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let after = file.metadata()?;
    if before.len() != after.len() || before.modified()? != after.modified()? {
        return Err(StoreError::InvalidPath(
            "managed file changed during maintenance inventory".to_owned(),
        ));
    }
    let digest = hasher.finalize();
    let mut checksum = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(checksum, "{byte:02x}");
    }
    Ok(MaintenanceFile {
        relative_path: relative.to_owned(),
        class,
        bytes: after.len(),
        checksum,
    })
}

fn build_inventory(
    generation: u64,
    mut files: Vec<MaintenanceFile>,
) -> Result<MaintenanceInventory> {
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if files
        .windows(2)
        .any(|pair| pair[0].relative_path == pair[1].relative_path)
    {
        return Err(StoreError::InvalidPath(
            "maintenance inventory contains duplicate paths".to_owned(),
        ));
    }
    let total_bytes = files.iter().try_fold(0_u64, |total, item| {
        total
            .checked_add(item.bytes)
            .ok_or_else(|| StoreError::InvalidPath("inventory byte count overflow".to_owned()))
    })?;
    let digest = checksum_bytes(&canonical_json(&files)?);
    Ok(MaintenanceInventory {
        schema_version: MAINTENANCE_SCHEMA_VERSION,
        store_generation: generation,
        files,
        total_bytes,
        digest,
    })
}

fn receipt_class(name: &str) -> MaintenanceFileClass {
    match name {
        "agent-registrations.json" => MaintenanceFileClass::RegistrationReceipt,
        "disclosure-grants.json" => MaintenanceFileClass::DisclosureGrantReceipt,
        _ => MaintenanceFileClass::OperationalReceipt,
    }
}

fn push_inventory_file(files: &mut Vec<MaintenanceFile>, item: MaintenanceFile) -> Result<()> {
    files.push(item);
    if files.len() > MAX_INVENTORY_FILES {
        return Err(StoreError::InvalidPath(
            "maintenance inventory file limit exceeded".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_complete_deleted_path_coverage(receipt: &EvidenceDeletionReceipt) -> Result<()> {
    let preview_paths = receipt
        .preview
        .deletion
        .files
        .iter()
        .map(|item| item.relative_path.as_str())
        .collect::<BTreeSet<_>>();
    let deleted_paths = receipt
        .deleted_relative_paths
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if preview_paths != deleted_paths {
        return Err(StoreError::InvalidPath(
            "evidence deletion receipt does not cover every committed path".to_owned(),
        ));
    }
    Ok(())
}

fn ensure_empty_projection(sqlite: &SqliteStore) -> Result<()> {
    let snapshot = sqlite.snapshot_ids()?;
    if !snapshot.event_ids.is_empty()
        || !snapshot.chunk_revision_ids.is_empty()
        || !snapshot.current_chunks.is_empty()
        || !snapshot.artifact_revision_ids.is_empty()
        || !snapshot.current_artifacts.is_empty()
        || !snapshot.screenshot_lifecycle.is_empty()
    {
        return Err(StoreError::InvalidPath(
            "evidence deletion did not recreate an empty projection".to_owned(),
        ));
    }
    Ok(())
}
