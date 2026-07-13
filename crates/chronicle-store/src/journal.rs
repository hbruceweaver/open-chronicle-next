use std::io::{Read, Seek, SeekFrom, Write};

use chronicle_domain::{
    ChunkRevision, DeviceId, EventEnvelope, EventId, EventKind, EventPayload, EvidenceSource,
    GapReason, RecordingGap,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::checksum::{canonical_json, checksum_bytes};
use crate::{FaultInjector, FaultPoint, ManagedRoot, Result, StoreError};

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
}

impl CanonicalJournal {
    pub const fn new(root: ManagedRoot) -> Self {
        Self { root }
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
        let directory = self.root.path().join(family.directory());
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
        for shard in shards {
            let report = self.scan_shard(family, &shard, repair_partial)?;
            verified_through = report.verified_through;
            if report.health.partial_tail_bytes > 0 {
                health = report.health.clone();
            }
            records.extend(report.records);
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
        if let Some(record) = self.find_stable_id(family, stable_id)? {
            if record.checksum() != checksum {
                return Err(StoreError::StableIdConflict {
                    id: stable_id.to_owned(),
                });
            }
            let relative = format!("{}/{}", family.directory(), record.shard());
            self.root
                .open_file(&relative, false, false, false)?
                .sync_all()?;
            self.root.sync_directory(family.directory())?;
            return Ok(record);
        }
        let date = timestamp.format("%Y-%m-%d").to_string();
        let shard = self.active_shard(family, &date)?;
        let relative = format!("{}/{shard}", family.directory());
        let existed = self.root.exists(&relative)?;
        let mut file = self.root.open_file(&relative, true, true, false)?;
        let start_offset = file.seek(SeekFrom::End(0))?;
        file.write_all(&line)?;
        faults.check(FaultPoint::AfterJournalAppend)?;
        file.sync_all()?;
        faults.check(FaultPoint::AfterJournalSync)?;
        if !existed {
            self.root.sync_directory(family.directory())?;
        }
        let end_offset = start_offset
            .checked_add(u64::try_from(line.len()).map_err(|_| {
                StoreError::InvalidPath("journal line exceeds supported length".to_owned())
            })?)
            .ok_or_else(|| StoreError::InvalidPath("journal offset overflow".to_owned()))?;
        Ok(VerifiedRecord {
            family,
            shard,
            start_offset,
            end_offset,
            stable_id: stable_id.to_owned(),
            checksum,
            body_bytes,
        })
    }

    fn active_shard(&self, family: JournalFamily, date: &str) -> Result<String> {
        let mut candidates = Vec::new();
        for entry in std::fs::read_dir(self.root.path().join(family.directory()))? {
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|_| {
                StoreError::InvalidPath("journal shard name is not valid UTF-8".to_owned())
            })?;
            if name.starts_with(date) && name.ends_with(".jsonl") {
                validate_shard_name(&name)?;
                candidates.push(name);
            }
        }
        candidates.sort();
        match candidates.as_slice() {
            [] => Ok(format!("{date}.jsonl")),
            [one] => Ok(one.clone()),
            _ => Err(StoreError::RepairIncomplete(format!(
                "multiple active shards exist for {date}"
            ))),
        }
    }

    fn find_stable_id(
        &self,
        family: JournalFamily,
        stable_id: &str,
    ) -> Result<Option<VerifiedRecord>> {
        Ok(self
            .scan_all(family, false)?
            .records
            .into_iter()
            .find(|record| record.stable_id() == stable_id))
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
