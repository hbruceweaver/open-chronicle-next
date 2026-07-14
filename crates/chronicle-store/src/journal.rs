use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use chronicle_domain::{
    ChunkRevision, DeviceId, EventEnvelope, EventId, EventKind, EventPayload, EvidenceSource,
    GapReason, ObservationContent, RecordingGap,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::checksum::{canonical_json, checksum_bytes};
use crate::{FaultInjector, FaultPoint, LockManager, ManagedRoot, Result, SqliteStore, StoreError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JournalFamily {
    Events,
    Chunks,
}

impl JournalFamily {
    pub const fn directory(self) -> &'static str {
        match self {
            Self::Events => "evidence/events",
            Self::Chunks => "aggregates/chunks",
        }
    }

    pub const fn cursor_name(self) -> &'static str {
        match self {
            Self::Events => "events",
            Self::Chunks => "chunks",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct JournalEnvelope {
    body: Value,
    checksum: String,
}

const REPAIR_CONFIRMATION: &str = "I UNDERSTAND THIS QUARANTINES UNVERIFIED EVIDENCE";

#[derive(Clone, Debug)]
pub struct RepairConfirmation(());

impl RepairConfirmation {
    pub fn confirm(phrase: &str) -> Result<Self> {
        if phrase == REPAIR_CONFIRMATION {
            Ok(Self(()))
        } else {
            Err(StoreError::RepairNotConfirmed)
        }
    }

    pub const fn required_phrase() -> &'static str {
        REPAIR_CONFIRMATION
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairReport {
    pub family: String,
    pub original_shard: String,
    pub archived_original: String,
    pub quarantined_bytes: String,
    pub successor_shard: String,
    pub verified_prefix_bytes: u64,
    pub quarantined_byte_count: u64,
    pub original_checksum: String,
    pub quarantine_checksum: String,
    pub repair_event_id: EventId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RepairReceipt {
    schema_version: u32,
    confirmed: bool,
    completed: bool,
    repaired_at: DateTime<Utc>,
    report: RepairReport,
}

#[derive(Clone, Debug)]
pub struct VerifiedRecord {
    family: JournalFamily,
    shard: String,
    start_offset: u64,
    end_offset: u64,
    stable_id: String,
    checksum: String,
    body_bytes: Vec<u8>,
}

impl VerifiedRecord {
    pub fn family(&self) -> JournalFamily {
        self.family
    }

    pub fn shard(&self) -> &str {
        &self.shard
    }

    pub fn start_offset(&self) -> u64 {
        self.start_offset
    }

    pub fn end_offset(&self) -> u64 {
        self.end_offset
    }

    pub fn stable_id(&self) -> &str {
        &self.stable_id
    }

    pub fn checksum(&self) -> &str {
        &self.checksum
    }

    pub fn body_bytes(&self) -> &[u8] {
        &self.body_bytes
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanHealth {
    pub partial_tail_bytes: u64,
    pub diagnostic_copy: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ScanReport {
    pub records: Vec<VerifiedRecord>,
    pub verified_through: u64,
    pub health: ScanHealth,
}

#[derive(Clone, Debug)]
pub struct CanonicalJournal {
    root: ManagedRoot,
    index: Arc<Mutex<JournalIndex>>,
}

#[derive(Clone, Debug)]
struct JournalIndexEntry {
    family: JournalFamily,
    shard: String,
    start_offset: u64,
    end_offset: u64,
    stable_id: String,
    checksum: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JournalManifestEntry {
    shard: String,
    start_offset: u64,
    end_offset: u64,
    stable_id: String,
    checksum: String,
}

impl From<&JournalIndexEntry> for JournalManifestEntry {
    fn from(entry: &JournalIndexEntry) -> Self {
        Self {
            shard: entry.shard.clone(),
            start_offset: entry.start_offset,
            end_offset: entry.end_offset,
            stable_id: entry.stable_id.clone(),
            checksum: entry.checksum.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PendingJournalMutation {
    schema_version: u32,
    shard: String,
    prior_size: u64,
    requires_full_scan: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ManifestIdentity {
    inode: u64,
    length: u64,
}

impl From<&VerifiedRecord> for JournalIndexEntry {
    fn from(record: &VerifiedRecord) -> Self {
        Self {
            family: record.family,
            shard: record.shard.clone(),
            start_offset: record.start_offset,
            end_offset: record.end_offset,
            stable_id: record.stable_id.clone(),
            checksum: record.checksum.clone(),
        }
    }
}

#[derive(Debug, Default)]
struct JournalIndex {
    loaded_families: HashSet<JournalFamily>,
    shard_sizes: HashMap<(JournalFamily, String), u64>,
    records: HashMap<(JournalFamily, String), JournalIndexEntry>,
    manifested_records: HashSet<(JournalFamily, String)>,
    active_shards: HashMap<(JournalFamily, String), String>,
    manifest_identities: HashMap<JournalFamily, ManifestIdentity>,
    full_scan_count: HashMap<JournalFamily, u64>,
    directory_enumeration_count: HashMap<JournalFamily, u64>,
    image_intent_owners: HashMap<String, String>,
}

fn image_intent_identity(
    family: JournalFamily,
    body_bytes: &[u8],
) -> Result<Option<(String, String)>> {
    if family != JournalFamily::Events {
        return Ok(None);
    }
    let event = EventEnvelope::parse(
        std::str::from_utf8(body_bytes)
            .map_err(|error| StoreError::InvalidPath(error.to_string()))?,
    )?;
    let artifact_id = match &event.payload {
        EventPayload::ObservationAttempt(attempt) => match &attempt.content {
            ObservationContent::Captured(content) => content
                .image
                .as_ref()
                .map(|image| image.artifact_id.to_string()),
            ObservationContent::Unchanged(_)
            | ObservationContent::Protected(_)
            | ObservationContent::NoEvidence(_) => None,
        },
        EventPayload::RecordingGap(_) | EventPayload::ScreenshotLifecycle(_) => None,
    };
    Ok(artifact_id.map(|artifact_id| (artifact_id, event.event_id.to_string())))
}

fn register_image_intent(index: &mut JournalIndex, record: &VerifiedRecord) -> Result<()> {
    let Some((artifact_id, owner_event_id)) =
        image_intent_identity(record.family, record.body_bytes())?
    else {
        return Ok(());
    };
    if let Some(existing_owner) = index.image_intent_owners.get(&artifact_id)
        && existing_owner != &owner_event_id
    {
        return Err(StoreError::StableIdConflict { id: artifact_id });
    }
    index
        .image_intent_owners
        .insert(artifact_id, owner_event_id);
    Ok(())
}

fn shared_journal_index(root: &ManagedRoot) -> Arc<Mutex<JournalIndex>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<JournalIndex>>>>> = OnceLock::new();
    let registry = REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry.lock().unwrap_or_else(|poison| poison.into_inner());
    let key = root.path().to_path_buf();
    if let Some(index) = registry.get(&key).and_then(Weak::upgrade) {
        return index;
    }
    let index = Arc::new(Mutex::new(JournalIndex::default()));
    registry.insert(key, Arc::downgrade(&index));
    index
}

impl CanonicalJournal {
    pub fn new(root: ManagedRoot) -> Self {
        let index = shared_journal_index(&root);
        Self { root, index }
    }

    pub fn append_event(
        &self,
        event: &EventEnvelope,
        faults: FaultInjector,
    ) -> Result<VerifiedRecord> {
        event.validate().map_err(|reason| {
            StoreError::Contract(chronicle_domain::ContractError::Validation(reason))
        })?;
        self.append(
            JournalFamily::Events,
            event.event_id.as_str(),
            event.recorded_at,
            event,
            faults,
        )
    }

    pub fn append_chunk(
        &self,
        chunk: &ChunkRevision,
        faults: FaultInjector,
    ) -> Result<VerifiedRecord> {
        chunk.validate().map_err(|reason| {
            StoreError::Contract(chronicle_domain::ContractError::Validation(reason))
        })?;
        self.append(
            JournalFamily::Chunks,
            chunk.revision_id.as_str(),
            chunk.generated_at,
            chunk,
            faults,
        )
    }

    pub fn scan_all(&self, family: JournalFamily, repair_partial: bool) -> Result<ScanReport> {
        let _writer =
            LockManager::new(self.root.clone(), Duration::from_secs(1)).journal(family)?;
        self.scan_all_locked(family, repair_partial)
    }

    pub(crate) fn image_intent_owner(&self, artifact_id: &str) -> Result<Option<String>> {
        let _writer = LockManager::new(self.root.clone(), Duration::from_secs(1))
            .journal(JournalFamily::Events)?;
        self.refresh_index(JournalFamily::Events)?;
        Ok(self
            .index
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .image_intent_owners
            .get(artifact_id)
            .cloned())
    }

    /// Returns canonical records that are durable beyond the projection cursor.
    /// This path enumerates shard metadata but reads only bytes after each
    /// durable cursor; it never clones or scans the historical record index.
    pub fn unprojected_records(
        &self,
        family: JournalFamily,
        sqlite: &SqliteStore,
    ) -> Result<Vec<VerifiedRecord>> {
        let _writer =
            LockManager::new(self.root.clone(), Duration::from_secs(1)).journal(family)?;
        let mut shards = Vec::new();
        for entry in std::fs::read_dir(self.root.path().join(family.directory()))? {
            let name = entry?.file_name().into_string().map_err(|_| {
                StoreError::InvalidPath("journal shard name is not valid UTF-8".to_owned())
            })?;
            if name.ends_with(".jsonl") {
                validate_shard_name(&name)?;
                shards.push(name);
            }
        }
        shards.sort();
        reject_multiple_active_shards(&shards)?;
        let cursors = sqlite.projection_cursors(family)?;
        let mut records = Vec::new();
        for shard in shards {
            let cursor = cursors.get(&shard).copied().unwrap_or_default();
            let relative = format!("{}/{shard}", family.directory());
            let size = self
                .root
                .open_file(&relative, false, false, false)?
                .metadata()?
                .len();
            if cursor > size {
                return Err(StoreError::SqliteIdentity(format!(
                    "projection cursor exceeds canonical shard {shard}"
                )));
            }
            if cursor < size {
                records.extend(self.scan_shard_tail(family, &shard, cursor)?);
            }
        }
        Ok(records)
    }

    fn scan_shard_tail(
        &self,
        family: JournalFamily,
        shard: &str,
        start_offset: u64,
    ) -> Result<Vec<VerifiedRecord>> {
        let relative = format!("{}/{shard}", family.directory());
        let mut file = self.root.open_file(&relative, false, false, false)?;
        file.seek(SeekFrom::Start(start_offset))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
            return Err(StoreError::RepairIncomplete(format!(
                "journal shard {shard} has an incomplete unprojected tail"
            )));
        }
        let mut offset = start_offset;
        let mut records = Vec::new();
        for complete_line in bytes.split_inclusive(|byte| *byte == b'\n') {
            if complete_line.is_empty() {
                continue;
            }
            let line = &complete_line[..complete_line.len().saturating_sub(1)];
            if line.is_empty() {
                return Err(StoreError::CorruptRecord {
                    shard: shard.to_owned(),
                    offset,
                    reason: "empty complete line".to_owned(),
                });
            }
            let end_offset = offset
                .checked_add(u64::try_from(complete_line.len()).map_err(|_| {
                    StoreError::InvalidPath("journal tail exceeds supported length".to_owned())
                })?)
                .ok_or_else(|| StoreError::InvalidPath("journal offset overflow".to_owned()))?;
            records.push(parse_verified_line(
                family, shard, offset, end_offset, line,
            )?);
            offset = end_offset;
        }
        Ok(records)
    }

    fn scan_all_locked(&self, family: JournalFamily, repair_partial: bool) -> Result<ScanReport> {
        let directory = self.root.path().join(family.directory());
        {
            let mut index = self
                .index
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let count = index.directory_enumeration_count.entry(family).or_default();
            *count = count.saturating_add(1);
        }
        let mut shards = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|_| {
                StoreError::InvalidPath("journal shard name is not valid UTF-8".to_owned())
            })?;
            if name.ends_with(".jsonl") {
                shards.push(name);
            }
        }
        shards.sort();
        reject_multiple_active_shards(&shards)?;
        let mut records = Vec::new();
        let mut health = ScanHealth::default();
        let mut verified_through = 0;
        for shard in &shards {
            let report = self.scan_shard(family, shard, repair_partial)?;
            verified_through = report.verified_through;
            if report.health.partial_tail_bytes > 0 {
                health = report.health.clone();
            }
            records.extend(report.records);
        }
        {
            let mut index = self
                .index
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if index.loaded_families.insert(family) {
                let scans = index.full_scan_count.entry(family).or_default();
                *scans = scans.saturating_add(1);
            }
            index
                .records
                .retain(|(entry_family, _), _| *entry_family != family);
            index
                .shard_sizes
                .retain(|(entry_family, _), _| *entry_family != family);
            index
                .active_shards
                .retain(|(entry_family, _), _| *entry_family != family);
            if family == JournalFamily::Events {
                index.image_intent_owners.clear();
            }
            for record in &records {
                register_image_intent(&mut index, record)?;
                index.records.insert(
                    (family, record.stable_id.clone()),
                    JournalIndexEntry::from(record),
                );
            }
            for shard in &shards {
                let size =
                    std::fs::metadata(self.root.path().join(family.directory()).join(shard))?.len();
                index.shard_sizes.insert((family, shard.clone()), size);
                index
                    .active_shards
                    .insert((family, shard[..10].to_owned()), shard.clone());
            }
            // The manifest is a disposable acceleration structure. Canonical
            // replay remains available if its rewrite cannot be persisted.
            let _ = self.rebuild_manifest(family, &mut index);
        }
        if let Some(pending) = self.read_pending_mutation(family)? {
            if pending.schema_version != 1 {
                return Err(StoreError::RepairIncomplete(
                    "unsupported pending journal mutation version".to_owned(),
                ));
            }
            let relative = format!("{}/{}", family.directory(), pending.shard);
            if self.root.exists(&relative)? {
                let file = self.root.open_file(&relative, false, false, false)?;
                if file.metadata()?.len() < pending.prior_size {
                    return Err(StoreError::RepairIncomplete(
                        "pending journal mutation regressed its shard size".to_owned(),
                    ));
                }
                file.sync_all()?;
            }
            self.root.sync_directory(family.directory())?;
            self.clear_pending_mutation(family)?;
        }
        Ok(ScanReport {
            records,
            verified_through,
            health,
        })
    }

    pub fn scan_shard(
        &self,
        family: JournalFamily,
        shard: &str,
        repair_partial: bool,
    ) -> Result<ScanReport> {
        validate_shard_name(shard)?;
        let relative = format!("{}/{shard}", family.directory());
        let mut file = self.root.open_file(&relative, false, false, false)?;
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let last_newline = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        let mut offset = 0_u64;
        let mut records = Vec::new();
        for complete_line in bytes[..last_newline].split_inclusive(|byte| *byte == b'\n') {
            let line = &complete_line[..complete_line.len().saturating_sub(1)];
            if line.is_empty() {
                return Err(StoreError::CorruptRecord {
                    shard: shard.to_owned(),
                    offset,
                    reason: "empty complete line".to_owned(),
                });
            }
            let start = offset;
            offset += u64::try_from(line.len() + 1).map_err(|_| StoreError::CorruptRecord {
                shard: shard.to_owned(),
                offset: start,
                reason: "line length exceeds u64".to_owned(),
            })?;
            records.push(parse_verified_line(family, shard, start, offset, line)?);
        }
        let partial = &bytes[last_newline..];
        let mut health = ScanHealth::default();
        if !partial.is_empty() {
            health.partial_tail_bytes = u64::try_from(partial.len()).unwrap_or(u64::MAX);
            if repair_partial {
                let diagnostic = format!(
                    "diagnostics/partial-tail-{}-{}.bin",
                    shard.trim_end_matches(".jsonl"),
                    Uuid::now_v7()
                );
                self.root.atomic_write(&diagnostic, partial)?;
                file.set_len(u64::try_from(last_newline).map_err(|_| {
                    StoreError::InvalidPath("journal exceeds supported length".to_owned())
                })?)?;
                file.sync_all()?;
                self.root.sync_directory(family.directory())?;
                health.diagnostic_copy = Some(diagnostic);
            }
        }
        Ok(ScanReport {
            records,
            verified_through: u64::try_from(last_newline).unwrap_or(u64::MAX),
            health,
        })
    }

    pub(crate) fn repair_corrupt_shard(
        &self,
        family: JournalFamily,
        shard: &str,
        device_id: DeviceId,
        _confirmation: RepairConfirmation,
        faults: FaultInjector,
    ) -> Result<RepairReport> {
        validate_shard_name(shard)?;
        let _writer =
            LockManager::new(self.root.clone(), Duration::from_secs(1)).journal(family)?;
        let relative = format!("{}/{shard}", family.directory());
        let family_name = family.cursor_name();
        let pending = find_repair_receipt(&self.root, family_name, shard)?;
        if pending.as_ref().is_some_and(|receipt| receipt.completed) {
            if self.root.exists(&relative)? {
                return Err(StoreError::RepairIncomplete(
                    "completed repair still has an active original shard".to_owned(),
                ));
            }
            return Ok(pending
                .ok_or_else(|| StoreError::RepairIncomplete("repair receipt vanished".to_owned()))?
                .report);
        }
        let prior_size = if self.root.exists(&relative)? {
            self.root
                .open_file(&relative, false, false, false)?
                .metadata()?
                .len()
        } else {
            0
        };
        self.write_pending_mutation(
            family,
            &PendingJournalMutation {
                schema_version: 1,
                shard: shard.to_owned(),
                prior_size,
                requires_full_scan: true,
            },
        )?;

        let (mut receipt, receipt_relative, bytes) = if let Some(receipt) = pending {
            let bytes = self.root.read(&receipt.report.archived_original)?;
            let operation_id = receipt_operation_id(&receipt.report.successor_shard)?.to_owned();
            (
                receipt,
                format!(
                    "diagnostics/journal-repair-{}-{}.json",
                    family_name, operation_id
                ),
                bytes,
            )
        } else {
            let bytes = self.root.read(&relative)?;
            let corrupt_offset = locate_first_corrupt_line(family, shard, &bytes)?;
            let quarantine = &bytes[corrupt_offset..];
            if quarantine.is_empty() {
                return Err(StoreError::RepairIncomplete(
                    "no corrupt complete line was found".to_owned(),
                ));
            }
            let repaired_at = Utc::now();
            let original_checksum = checksum_bytes(&bytes);
            let operation_id = original_checksum[..24].to_owned();
            let archive_directory = format!("diagnostics/corrupt-shards/{family_name}");
            self.root.ensure_directory(&archive_directory)?;
            let archived_original = format!("{archive_directory}/{shard}.{operation_id}.sealed");
            let quarantined_bytes =
                format!("{archive_directory}/{shard}.{operation_id}.quarantine");
            self.root.atomic_write(&archived_original, &bytes)?;
            self.root.atomic_write(&quarantined_bytes, quarantine)?;
            let repair_event = make_repair_event(device_id.clone(), repaired_at, &bytes, shard)?;
            let report = RepairReport {
                family: family_name.to_owned(),
                original_shard: shard.to_owned(),
                archived_original,
                quarantined_bytes,
                successor_shard: format!("{}.repair-{operation_id}.jsonl", &shard[..10]),
                verified_prefix_bytes: u64::try_from(corrupt_offset).map_err(|_| {
                    StoreError::RepairIncomplete("verified prefix exceeds u64".to_owned())
                })?,
                quarantined_byte_count: u64::try_from(quarantine.len()).map_err(|_| {
                    StoreError::RepairIncomplete("quarantine exceeds u64".to_owned())
                })?,
                original_checksum,
                quarantine_checksum: checksum_bytes(quarantine),
                repair_event_id: repair_event.event_id,
            };
            let receipt = RepairReceipt {
                schema_version: 1,
                confirmed: true,
                completed: false,
                repaired_at,
                report,
            };
            let receipt_relative = format!(
                "diagnostics/journal-repair-{}-{}.json",
                family_name, operation_id
            );
            self.root
                .atomic_write(&receipt_relative, &canonical_json(&receipt)?)?;
            faults.check(FaultPoint::AfterRepairArchive)?;
            (receipt, receipt_relative, bytes)
        };

        if checksum_bytes(&bytes) != receipt.report.original_checksum {
            return Err(StoreError::RepairIncomplete(
                "archived original checksum changed".to_owned(),
            ));
        }
        let corrupt_offset =
            usize::try_from(receipt.report.verified_prefix_bytes).map_err(|_| {
                StoreError::RepairIncomplete("verified prefix exceeds usize".to_owned())
            })?;
        if corrupt_offset > bytes.len() {
            return Err(StoreError::RepairIncomplete(
                "verified prefix exceeds archived shard".to_owned(),
            ));
        }
        let repair_event = make_repair_event(device_id, receipt.repaired_at, &bytes, shard)?;
        if repair_event.event_id != receipt.report.repair_event_id {
            return Err(StoreError::RepairIncomplete(
                "repair event identity changed".to_owned(),
            ));
        }
        let mut successor_bytes = bytes[..corrupt_offset].to_vec();
        if family == JournalFamily::Events {
            successor_bytes.extend(encode_line(&repair_event)?.0);
        }
        let successor_relative =
            format!("{}/{}", family.directory(), receipt.report.successor_shard);
        if self.root.exists(&successor_relative)? {
            if self.root.read(&successor_relative)? != successor_bytes {
                return Err(StoreError::RepairIncomplete(
                    "successor shard bytes changed".to_owned(),
                ));
            }
        } else {
            self.root
                .atomic_write(&successor_relative, &successor_bytes)?;
        }
        faults.check(FaultPoint::AfterRepairSuccessor)?;
        if self.root.exists(&relative)? {
            if checksum_bytes(&self.root.read(&relative)?) != receipt.report.original_checksum {
                return Err(StoreError::RepairIncomplete(
                    "active original changed during repair".to_owned(),
                ));
            }
            self.root.unlink(&relative)?;
        }
        faults.check(FaultPoint::AfterRepairOriginalUnlink)?;
        if family == JournalFamily::Chunks {
            self.append_event(&repair_event, FaultInjector::none())?;
        }
        faults.check(FaultPoint::AfterRepairMarker)?;
        self.scan_all_locked(family, false)?;
        receipt.completed = true;
        self.root
            .atomic_write(&receipt_relative, &canonical_json(&receipt)?)?;
        Ok(receipt.report)
    }

    fn append<T: Serialize>(
        &self,
        family: JournalFamily,
        stable_id: &str,
        timestamp: DateTime<Utc>,
        body: &T,
        faults: FaultInjector,
    ) -> Result<VerifiedRecord> {
        let (line, body_bytes, checksum) = encode_line(body)?;
        let _writer =
            LockManager::new(self.root.clone(), Duration::from_secs(1)).journal(family)?;
        self.refresh_index(family)?;
        let mut index = self
            .index
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let date = timestamp.format("%Y-%m-%d").to_string();
        let shard = index
            .active_shards
            .get(&(family, date.clone()))
            .cloned()
            .unwrap_or_else(|| format!("{date}.jsonl"));
        self.refresh_changed_shard(family, &shard, &mut index)?;
        let image_intent = image_intent_identity(family, &body_bytes)?;
        if let Some((artifact_id, owner_event_id)) = &image_intent
            && let Some(existing_owner) = index.image_intent_owners.get(artifact_id)
            && existing_owner != owner_event_id
        {
            return Err(StoreError::StableIdConflict {
                id: artifact_id.clone(),
            });
        }
        if let Some(entry) = index.records.get(&(family, stable_id.to_owned())).cloned() {
            if entry.checksum != checksum {
                return Err(StoreError::StableIdConflict {
                    id: stable_id.to_owned(),
                });
            }
            let relative = format!("{}/{}", family.directory(), entry.shard);
            self.root
                .open_file(&relative, false, false, false)?
                .sync_all()?;
            self.root.sync_directory(family.directory())?;
            return self.read_indexed_record(&entry);
        }
        let relative = format!("{}/{shard}", family.directory());
        let existed = self.root.exists(&relative)?;
        let mut file = self.root.open_file(&relative, true, true, false)?;
        let start_offset = file.seek(SeekFrom::End(0))?;
        self.write_pending_mutation(
            family,
            &PendingJournalMutation {
                schema_version: 1,
                shard: shard.clone(),
                prior_size: start_offset,
                requires_full_scan: false,
            },
        )?;
        let end_offset = start_offset
            .checked_add(u64::try_from(line.len()).map_err(|_| {
                StoreError::InvalidPath("journal line exceeds supported length".to_owned())
            })?)
            .ok_or_else(|| StoreError::InvalidPath("journal offset overflow".to_owned()))?;
        let record = VerifiedRecord {
            family,
            shard: shard.clone(),
            start_offset,
            end_offset,
            stable_id: stable_id.to_owned(),
            checksum,
            body_bytes,
        };
        file.write_all(&line)?;
        index.records.insert(
            (family, stable_id.to_owned()),
            JournalIndexEntry::from(&record),
        );
        if let Some((artifact_id, owner_event_id)) = image_intent {
            index
                .image_intent_owners
                .insert(artifact_id, owner_event_id);
        }
        index
            .shard_sizes
            .insert((family, shard.clone()), end_offset);
        index.active_shards.insert((family, date), shard.clone());
        faults.check(FaultPoint::AfterJournalAppend)?;
        file.sync_all()?;
        faults.check(FaultPoint::AfterJournalSync)?;
        if !existed {
            self.root.sync_directory(family.directory())?;
        }
        let entry = index
            .records
            .get(&(family, stable_id.to_owned()))
            .cloned()
            .ok_or_else(|| StoreError::InvalidPath("journal index entry vanished".to_owned()))?;
        let manifest_result = faults
            .check(FaultPoint::BeforeJournalManifestUpdate)
            .and_then(|()| self.append_manifest_entry(family, &entry, &mut index));
        if manifest_result.is_ok() {
            let _ = self.clear_pending_mutation(family);
        }
        Ok(record)
    }

    fn refresh_index(&self, family: JournalFamily) -> Result<()> {
        let loaded = self
            .index
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .loaded_families
            .contains(&family);
        if !loaded {
            self.scan_all_locked(family, false)?;
        }
        let manifest_requires_scan = {
            let mut index = self
                .index
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            self.refresh_manifest(family, &mut index)?
        };
        if manifest_requires_scan {
            self.scan_all_locked(family, false)?;
        }
        if let Some(pending) = self.read_pending_mutation(family)? {
            if pending.schema_version != 1 {
                return Err(StoreError::RepairIncomplete(
                    "unsupported pending journal mutation version".to_owned(),
                ));
            }
            if pending.requires_full_scan {
                self.scan_all_locked(family, false)?;
            } else {
                let mut index = self
                    .index
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                // A prior writer may have cached the canonical size before its
                // disposable manifest update failed. Force the named-shard
                // pass so every unmanifested record is durably re-indexed.
                index.shard_sizes.remove(&(family, pending.shard.clone()));
                self.refresh_changed_shard(family, &pending.shard, &mut index)?;
                let relative = format!("{}/{}", family.directory(), pending.shard);
                self.root
                    .open_file(&relative, false, false, false)?
                    .sync_all()?;
                let actual_size = self
                    .root
                    .open_file(&relative, false, false, false)?
                    .metadata()?
                    .len();
                if actual_size < pending.prior_size {
                    return Err(StoreError::RepairIncomplete(
                        "pending journal mutation regressed its shard size".to_owned(),
                    ));
                }
                self.root.sync_directory(family.directory())?;
                self.clear_pending_mutation(family)?;
            }
        }
        Ok(())
    }

    fn manifest_path(family: JournalFamily) -> String {
        format!("receipts/journal-{}-index.jsonl", family.cursor_name())
    }

    fn pending_path(family: JournalFamily) -> String {
        format!("receipts/journal-{}-pending.json", family.cursor_name())
    }

    fn rebuild_manifest(&self, family: JournalFamily, index: &mut JournalIndex) -> Result<()> {
        let mut entries = index
            .records
            .iter()
            .filter(|((entry_family, _), _)| *entry_family == family)
            .map(|(_, entry)| entry.clone())
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            left.shard
                .cmp(&right.shard)
                .then(left.start_offset.cmp(&right.start_offset))
        });
        let mut bytes = Vec::new();
        for entry in &entries {
            bytes.extend(canonical_json(&JournalManifestEntry::from(entry))?);
            bytes.push(b'\n');
        }
        let path = Self::manifest_path(family);
        if !self.root.exists(&path)? || self.root.read(&path)? != bytes {
            self.root.atomic_write(&path, &bytes)?;
        }
        let metadata = self
            .root
            .open_file(&path, false, false, false)?
            .metadata()?;
        index.manifest_identities.insert(
            family,
            ManifestIdentity {
                inode: metadata.ino(),
                length: metadata.len(),
            },
        );
        index
            .manifested_records
            .retain(|(entry_family, _)| *entry_family != family);
        index
            .manifested_records
            .extend(entries.into_iter().map(|entry| (family, entry.stable_id)));
        Ok(())
    }

    fn refresh_manifest(&self, family: JournalFamily, index: &mut JournalIndex) -> Result<bool> {
        let path = Self::manifest_path(family);
        if !self.root.exists(&path)? {
            return Ok(true);
        }
        let mut file = self.root.open_file(&path, false, false, false)?;
        let metadata = file.metadata()?;
        let actual = ManifestIdentity {
            inode: metadata.ino(),
            length: metadata.len(),
        };
        let Some(prior) = index.manifest_identities.get(&family).copied() else {
            return Ok(true);
        };
        if actual == prior {
            return Ok(false);
        }
        if actual.inode != prior.inode || actual.length < prior.length {
            return Ok(true);
        }
        file.seek(SeekFrom::Start(prior.length))?;
        let mut tail = Vec::new();
        file.read_to_end(&mut tail)?;
        if !tail.is_empty() && tail.last() != Some(&b'\n') {
            return Ok(true);
        }
        for line in tail
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let Ok(entry) = serde_json::from_slice::<JournalManifestEntry>(line) else {
                return Ok(true);
            };
            if self.integrate_manifest_entry(family, entry, index).is_err() {
                return Ok(true);
            }
        }
        index.manifest_identities.insert(family, actual);
        Ok(false)
    }

    fn integrate_manifest_entry(
        &self,
        family: JournalFamily,
        entry: JournalManifestEntry,
        index: &mut JournalIndex,
    ) -> Result<()> {
        validate_shard_name(&entry.shard)?;
        if entry.start_offset >= entry.end_offset {
            return Err(StoreError::RepairIncomplete(
                "journal index manifest has invalid offsets".to_owned(),
            ));
        }
        let indexed = JournalIndexEntry {
            family,
            shard: entry.shard,
            start_offset: entry.start_offset,
            end_offset: entry.end_offset,
            stable_id: entry.stable_id,
            checksum: entry.checksum,
        };
        let verified = self.read_indexed_record(&indexed)?;
        let key = (family, indexed.stable_id.clone());
        if let Some(existing) = index.records.get(&key)
            && (existing.checksum != indexed.checksum
                || existing.shard != indexed.shard
                || existing.start_offset != indexed.start_offset
                || existing.end_offset != indexed.end_offset)
        {
            return Err(StoreError::StableIdConflict {
                id: indexed.stable_id,
            });
        }
        let date = indexed.shard[..10].to_owned();
        if let Some(active) = index.active_shards.get(&(family, date.clone()))
            && active != &indexed.shard
        {
            return Err(StoreError::RepairIncomplete(format!(
                "multiple active shards exist for {date}"
            )));
        }
        index
            .shard_sizes
            .entry((family, indexed.shard.clone()))
            .and_modify(|size| *size = (*size).max(indexed.end_offset))
            .or_insert(indexed.end_offset);
        index
            .active_shards
            .insert((family, date), indexed.shard.clone());
        index.manifested_records.insert(key.clone());
        register_image_intent(index, &verified)?;
        index
            .records
            .insert(key, JournalIndexEntry::from(&verified));
        Ok(())
    }

    fn refresh_changed_shard(
        &self,
        family: JournalFamily,
        shard: &str,
        index: &mut JournalIndex,
    ) -> Result<()> {
        validate_shard_name(shard)?;
        let relative = format!("{}/{shard}", family.directory());
        if !self.root.exists(&relative)? {
            return Ok(());
        }
        let size = self
            .root
            .open_file(&relative, false, false, false)?
            .metadata()?
            .len();
        if index.shard_sizes.get(&(family, shard.to_owned())).copied() == Some(size) {
            return Ok(());
        }
        let report = self.scan_shard(family, shard, false)?;
        if report.health.partial_tail_bytes != 0 {
            return Err(StoreError::RepairIncomplete(format!(
                "journal shard {shard} has an incomplete external tail"
            )));
        }
        let removed_stable_ids = index
            .records
            .iter()
            .filter(|((entry_family, _), entry)| *entry_family == family && entry.shard == shard)
            .map(|((_, stable_id), _)| stable_id.clone())
            .collect::<HashSet<_>>();
        index
            .records
            .retain(|(entry_family, _), entry| *entry_family != family || entry.shard != shard);
        if family == JournalFamily::Events {
            index
                .image_intent_owners
                .retain(|_, owner| !removed_stable_ids.contains(owner));
        }
        for record in &report.records {
            register_image_intent(index, record)?;
            let entry = JournalIndexEntry::from(record);
            let key = (family, entry.stable_id.clone());
            if let Some(existing) = index.records.get(&key)
                && (existing.checksum != entry.checksum
                    || existing.shard != entry.shard
                    || existing.start_offset != entry.start_offset)
            {
                return Err(StoreError::StableIdConflict {
                    id: entry.stable_id,
                });
            }
            index.records.insert(key.clone(), entry.clone());
            if !index.manifested_records.contains(&key) {
                self.append_manifest_entry(family, &entry, index)?;
            }
        }
        index.shard_sizes.insert((family, shard.to_owned()), size);
        let date = shard[..10].to_owned();
        if let Some(active) = index.active_shards.get(&(family, date.clone()))
            && active != shard
        {
            return Err(StoreError::RepairIncomplete(format!(
                "multiple active shards exist for {date}"
            )));
        }
        index.active_shards.insert((family, date), shard.to_owned());
        Ok(())
    }

    fn append_manifest_entry(
        &self,
        family: JournalFamily,
        entry: &JournalIndexEntry,
        index: &mut JournalIndex,
    ) -> Result<()> {
        let key = (family, entry.stable_id.clone());
        if index.manifested_records.contains(&key) {
            return Ok(());
        }
        let path = Self::manifest_path(family);
        let existed = self.root.exists(&path)?;
        let mut file = self.root.open_file(&path, true, true, false)?;
        let mut line = canonical_json(&JournalManifestEntry::from(entry))?;
        line.push(b'\n');
        file.write_all(&line)?;
        file.sync_all()?;
        if !existed {
            self.root.sync_directory("receipts")?;
        }
        let metadata = file.metadata()?;
        index.manifest_identities.insert(
            family,
            ManifestIdentity {
                inode: metadata.ino(),
                length: metadata.len(),
            },
        );
        index.manifested_records.insert(key);
        Ok(())
    }

    fn write_pending_mutation(
        &self,
        family: JournalFamily,
        pending: &PendingJournalMutation,
    ) -> Result<()> {
        self.root
            .atomic_write(&Self::pending_path(family), &canonical_json(pending)?)
    }

    fn read_pending_mutation(
        &self,
        family: JournalFamily,
    ) -> Result<Option<PendingJournalMutation>> {
        let path = Self::pending_path(family);
        if !self.root.exists(&path)? {
            return Ok(None);
        }
        let pending = serde_json::from_slice(&self.root.read(&path)?)?;
        Ok(Some(pending))
    }

    fn clear_pending_mutation(&self, family: JournalFamily) -> Result<()> {
        let path = Self::pending_path(family);
        match self.root.unlink(&path) {
            Ok(()) => Ok(()),
            Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn read_indexed_record(&self, entry: &JournalIndexEntry) -> Result<VerifiedRecord> {
        let relative = format!("{}/{}", entry.family.directory(), entry.shard);
        let mut file = self.root.open_file(&relative, false, false, false)?;
        file.seek(SeekFrom::Start(entry.start_offset))?;
        let length = entry.end_offset.saturating_sub(entry.start_offset);
        let mut line = vec![
            0_u8;
            usize::try_from(length).map_err(|_| {
                StoreError::InvalidPath("journal record length exceeds memory bounds".to_owned())
            })?
        ];
        file.read_exact(&mut line)?;
        if line.pop() != Some(b'\n') {
            return Err(StoreError::CorruptRecord {
                shard: entry.shard.clone(),
                offset: entry.start_offset,
                reason: "indexed journal record is not newline terminated".to_owned(),
            });
        }
        let record = parse_verified_line(
            entry.family,
            &entry.shard,
            entry.start_offset,
            entry.end_offset,
            &line,
        )?;
        if record.stable_id != entry.stable_id || record.checksum != entry.checksum {
            return Err(StoreError::StableIdConflict {
                id: entry.stable_id.clone(),
            });
        }
        Ok(record)
    }

    pub fn index_full_scan_count(&self, family: JournalFamily) -> u64 {
        self.index
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .full_scan_count
            .get(&family)
            .copied()
            .unwrap_or_default()
    }

    pub fn directory_enumeration_count(&self, family: JournalFamily) -> u64 {
        self.index
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .directory_enumeration_count
            .get(&family)
            .copied()
            .unwrap_or_default()
    }
}

fn parse_verified_line(
    family: JournalFamily,
    shard: &str,
    start_offset: u64,
    end_offset: u64,
    line: &[u8],
) -> Result<VerifiedRecord> {
    let envelope: JournalEnvelope =
        serde_json::from_slice(line).map_err(|error| StoreError::CorruptRecord {
            shard: shard.to_owned(),
            offset: start_offset,
            reason: error.to_string(),
        })?;
    let body_bytes = canonical_json(&envelope.body)?;
    let actual = checksum_bytes(&body_bytes);
    if actual != envelope.checksum {
        return Err(StoreError::CorruptRecord {
            shard: shard.to_owned(),
            offset: start_offset,
            reason: "body checksum mismatch".to_owned(),
        });
    }
    let body_text =
        std::str::from_utf8(&body_bytes).map_err(|error| StoreError::CorruptRecord {
            shard: shard.to_owned(),
            offset: start_offset,
            reason: error.to_string(),
        })?;
    let stable_id = match family {
        JournalFamily::Events => EventEnvelope::parse(body_text)
            .map_err(|error| StoreError::CorruptRecord {
                shard: shard.to_owned(),
                offset: start_offset,
                reason: error.to_string(),
            })?
            .event_id
            .to_string(),
        JournalFamily::Chunks => ChunkRevision::parse(body_text)
            .map_err(|error| StoreError::CorruptRecord {
                shard: shard.to_owned(),
                offset: start_offset,
                reason: error.to_string(),
            })?
            .revision_id
            .to_string(),
    };
    Ok(VerifiedRecord {
        family,
        shard: shard.to_owned(),
        start_offset,
        end_offset,
        stable_id,
        checksum: envelope.checksum,
        body_bytes,
    })
}

fn validate_shard_name(shard: &str) -> Result<()> {
    let valid_date = shard.len() >= 16
        && shard[..10].bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 4 | 7) {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        });
    let suffix = shard.get(10..).unwrap_or_default();
    let valid_suffix = suffix == ".jsonl"
        || suffix
            .strip_prefix(".repair-")
            .and_then(|value| value.strip_suffix(".jsonl"))
            .is_some_and(|identifier| {
                !identifier.is_empty()
                    && identifier
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            });
    let valid = valid_date && valid_suffix;
    if valid {
        Ok(())
    } else {
        Err(StoreError::InvalidPath(shard.to_owned()))
    }
}

fn reject_multiple_active_shards(shards: &[String]) -> Result<()> {
    let mut prior_date: Option<&str> = None;
    for shard in shards {
        validate_shard_name(shard)?;
        let date = &shard[..10];
        if prior_date == Some(date) {
            return Err(StoreError::RepairIncomplete(format!(
                "multiple active shards exist for {date}"
            )));
        }
        prior_date = Some(date);
    }
    Ok(())
}

fn locate_first_corrupt_line(family: JournalFamily, shard: &str, bytes: &[u8]) -> Result<usize> {
    let last_newline = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let mut offset = 0_usize;
    for complete_line in bytes[..last_newline].split_inclusive(|byte| *byte == b'\n') {
        let line = &complete_line[..complete_line.len().saturating_sub(1)];
        let end = offset
            .checked_add(complete_line.len())
            .ok_or_else(|| StoreError::RepairIncomplete("journal offset overflow".to_owned()))?;
        if parse_verified_line(
            family,
            shard,
            u64::try_from(offset).unwrap_or(u64::MAX),
            u64::try_from(end).unwrap_or(u64::MAX),
            line,
        )
        .is_err()
        {
            return Ok(offset);
        }
        offset = end;
    }
    Err(StoreError::RepairIncomplete(
        "shard has no corrupt complete canonical line".to_owned(),
    ))
}

fn encode_line<T: Serialize>(body: &T) -> Result<(Vec<u8>, Vec<u8>, String)> {
    let body_value = serde_json::to_value(body)?;
    let body_bytes = canonical_json(&body_value)?;
    let checksum = checksum_bytes(&body_bytes);
    let envelope = JournalEnvelope {
        body: body_value,
        checksum: checksum.clone(),
    };
    let mut line = canonical_json(&envelope)?;
    line.push(b'\n');
    Ok((line, body_bytes, checksum))
}

fn make_repair_event(
    device_id: DeviceId,
    repaired_at: DateTime<Utc>,
    original_bytes: &[u8],
    shard: &str,
) -> Result<EventEnvelope> {
    let digest = checksum_bytes(original_bytes);
    let event_id = EventId::new(format!("journal-repair-{}", &digest[..32]))
        .map_err(|error| StoreError::RepairIncomplete(error.to_string()))?;
    let event = EventEnvelope {
        schema_version: chronicle_domain::CONTRACT_VERSION.to_owned(),
        event_id,
        device_id,
        scheduled_at: None,
        observed_at: repaired_at,
        recorded_at: repaired_at,
        display_timezone: "UTC".to_owned(),
        source: EvidenceSource {
            adapter: "journal-repair".to_owned(),
            version: format!("1:{shard}"),
        },
        kind: EventKind::RecordingGap,
        payload: EventPayload::RecordingGap(RecordingGap {
            start: repaired_at - chrono::Duration::milliseconds(1),
            end: repaired_at,
            reason: GapReason::StorageOutage,
        }),
    };
    event.validate().map_err(|reason| {
        StoreError::Contract(chronicle_domain::ContractError::Validation(reason))
    })?;
    Ok(event)
}

fn find_repair_receipt(
    root: &ManagedRoot,
    family_name: &str,
    shard: &str,
) -> Result<Option<RepairReceipt>> {
    let prefix = format!("journal-repair-{family_name}-");
    let mut matched = None;
    for entry in std::fs::read_dir(root.path().join("diagnostics"))? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|_| {
            StoreError::InvalidPath("repair receipt name is not valid UTF-8".to_owned())
        })?;
        if !name.starts_with(&prefix) || !name.ends_with(".json") {
            continue;
        }
        let receipt: RepairReceipt =
            serde_json::from_slice(&root.read(&format!("diagnostics/{name}"))?)?;
        if receipt.report.original_shard == shard && receipt.report.family == family_name {
            if matched.is_some() {
                return Err(StoreError::RepairIncomplete(
                    "multiple repair receipts match one shard".to_owned(),
                ));
            }
            matched = Some(receipt);
        }
    }
    Ok(matched)
}

fn receipt_operation_id(successor_shard: &str) -> Result<&str> {
    successor_shard
        .strip_suffix(".jsonl")
        .and_then(|value| {
            value
                .rsplit_once(".repair-")
                .map(|(_, identifier)| identifier)
        })
        .filter(|identifier| !identifier.is_empty())
        .ok_or_else(|| {
            StoreError::RepairIncomplete("repair successor has invalid identity".to_owned())
        })
}
