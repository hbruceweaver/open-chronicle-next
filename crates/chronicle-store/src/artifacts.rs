use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use chronicle_domain::{
    DerivedArtifactRevision, EventEnvelope, EventPayload, ObservationContent,
    ScreenshotLifecycleAction,
};

use crate::checksum::canonical_json;
use crate::{
    CanonicalJournal, FaultInjector, FaultPoint, LockManager, ManagedRoot, Projector, Result,
    StoreError, StoreGeneration,
};

#[derive(Clone, Debug)]
pub struct ArtifactStore {
    root: ManagedRoot,
    locks: LockManager,
    projector: Projector,
}

impl ArtifactStore {
    pub fn new(root: ManagedRoot, projector: Projector) -> Self {
        Self {
            locks: LockManager::new(root.clone(), Duration::from_secs(1)),
            root,
            projector,
        }
    }

    pub fn write_revision(
        &self,
        revision: &DerivedArtifactRevision,
        faults: FaultInjector,
    ) -> Result<()> {
        revision.validate().map_err(|reason| {
            StoreError::Contract(chronicle_domain::ContractError::Validation(reason))
        })?;
        let generation = StoreGeneration::load(&self.root)?;
        if revision.store_generation != generation.generation {
            return Err(StoreError::StaleGeneration {
                expected: revision.store_generation,
                actual: generation.generation,
            });
        }
        let shared = self.locks.shared_request()?;
        generation.ensure_current(&self.root)?;
        let _artifact = shared.artifact(revision.artifact_id.as_str())?;
        let directory = format!("derived/{}", revision.artifact_id);
        self.root.ensure_directory(&directory)?;
        let relative = format!("{directory}/{}.json", revision.revision_id);
        let bytes = canonical_json(revision)?;
        if self.root.exists(&relative)? {
            if self.root.read(&relative)? != bytes {
                return Err(StoreError::StableIdConflict {
                    id: revision.revision_id.to_string(),
                });
            }
            self.root.sync_directory(&directory)?;
            faults.check(FaultPoint::AfterArtifactDirectorySync)?;
            return self.projector.project_artifact(revision, faults);
        }
        let current = current_revision(&self.root, revision.artifact_id.as_str())?;
        if current.as_deref()
            != revision
                .expected_prior_revision_id
                .as_ref()
                .map(|id| id.as_str())
        {
            return Err(StoreError::ArtifactConflict);
        }
        if self.root.exists(&relative)? {
            return Err(StoreError::ArtifactConflict);
        } else {
            self.root
                .atomic_write_with_boundary(&relative, &bytes, || {
                    faults.check(FaultPoint::AfterArtifactRename)
                })?;
            faults.check(FaultPoint::AfterArtifactDirectorySync)?;
        }
        self.projector.project_artifact(revision, faults)
    }

    pub fn scan_all(&self) -> Result<Vec<DerivedArtifactRevision>> {
        scan_artifact_revisions(&self.root)
    }
}

pub fn scan_artifact_revisions(root: &ManagedRoot) -> Result<Vec<DerivedArtifactRevision>> {
    let mut revisions = Vec::new();
    for artifact_id in directory_names(root.path().join("derived"))? {
        let directory = format!("derived/{artifact_id}");
        for name in root.list_file_names(&directory)? {
            if !name.ends_with(".json") {
                continue;
            }
            let relative = format!("{directory}/{name}");
            let bytes = root.read(&relative)?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
            let revision = DerivedArtifactRevision::parse(text)?;
            if revision.artifact_id.as_str() != artifact_id {
                return Err(StoreError::ArtifactConflict);
            }
            revisions.push(revision);
        }
    }
    order_artifact_chains(revisions)
}

#[derive(Clone, Debug)]
pub struct ScreenshotStore {
    root: ManagedRoot,
    journal: CanonicalJournal,
    projector: Projector,
    locks: LockManager,
    generation: StoreGeneration,
}

impl ScreenshotStore {
    pub fn new(root: ManagedRoot, journal: CanonicalJournal, projector: Projector) -> Result<Self> {
        let generation = StoreGeneration::load(&root)?;
        Ok(Self {
            locks: LockManager::new(root.clone(), Duration::from_secs(1)),
            root,
            journal,
            projector,
            generation,
        })
    }

    pub fn retain(
        &self,
        observation: &EventEnvelope,
        encoded_image: &[u8],
        completion: &EventEnvelope,
        faults: FaultInjector,
    ) -> Result<()> {
        let _shared = self.locks.shared_request()?;
        self.generation.ensure_current(&self.root)?;
        if encoded_image.is_empty() || encoded_image.len() > 4 * 1024 * 1024 {
            return Err(StoreError::InvalidPath(
                "encoded screenshot must be between 1 byte and 4 MiB".to_owned(),
            ));
        }
        let image = match &observation.payload {
            EventPayload::ObservationAttempt(attempt) => match &attempt.content {
                ObservationContent::Captured(content) => content.image.as_ref(),
                _ => None,
            },
            _ => None,
        }
        .ok_or_else(|| {
            StoreError::InvalidPath("observation has no pending image intent".to_owned())
        })?;
        let lifecycle = match &completion.payload {
            EventPayload::ScreenshotLifecycle(lifecycle)
                if lifecycle.action == ScreenshotLifecycleAction::WriteCompleted
                    && lifecycle.artifact_id == image.artifact_id
                    && lifecycle.source_event_id == observation.event_id =>
            {
                lifecycle
            }
            _ => {
                return Err(StoreError::InvalidPath(
                    "completion does not close the observation image intent".to_owned(),
                ));
            }
        };
        let final_path = derived_screenshot_path(observation, image.artifact_id.as_str());
        if image.managed_relative_path.as_str() != final_path {
            return Err(StoreError::InvalidPath(
                "image path does not match the managed screenshot derivation".to_owned(),
            ));
        }
        let parent = final_path
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .ok_or_else(|| StoreError::InvalidPath(final_path.to_owned()))?;
        let provisional = format!("{parent}/.{}.provisional", lifecycle.artifact_id);
        if self.root.exists(&final_path)? || self.root.exists(&provisional)? {
            return Err(StoreError::StableIdConflict {
                id: lifecycle.artifact_id.to_string(),
            });
        }
        self.root.atomic_write(&provisional, encoded_image)?;
        faults.check(FaultPoint::AfterProvisionalImageSync)?;
        let observation_record = match self.journal.append_event(observation, faults) {
            Ok(record) => record,
            Err(error) => {
                let _ = self.root.unlink(&provisional);
                return Err(error);
            }
        };
        faults.check(FaultPoint::AfterObservationAppend)?;
        self.root
            .rename_with_boundary(&provisional, &final_path, || {
                faults.check(FaultPoint::AfterImagePromotion)
            })?;
        faults.check(FaultPoint::AfterImagePromotionDirectorySync)?;
        let lifecycle_record = self.journal.append_event(completion, faults)?;
        faults.check(FaultPoint::AfterLifecycleCompletion)?;
        self.projector
            .project_record(&observation_record, FaultInjector::none())?;
        self.projector
            .project_record(&lifecycle_record, FaultInjector::none())?;
        Ok(())
    }

    pub fn delete(
        &self,
        request: &EventEnvelope,
        completion: &EventEnvelope,
        faults: FaultInjector,
    ) -> Result<()> {
        let _shared = self.locks.shared_request()?;
        self.generation.ensure_current(&self.root)?;
        let requested = match &request.payload {
            EventPayload::ScreenshotLifecycle(lifecycle)
                if lifecycle.action == ScreenshotLifecycleAction::DeleteRequested =>
            {
                lifecycle
            }
            _ => return Err(StoreError::InvalidPath("missing delete request".to_owned())),
        };
        let completed = match &completion.payload {
            EventPayload::ScreenshotLifecycle(lifecycle)
                if lifecycle.action == ScreenshotLifecycleAction::DeleteCompleted
                    && lifecycle.artifact_id == requested.artifact_id
                    && lifecycle.source_event_id == requested.source_event_id
                    && lifecycle.deletion_cause == requested.deletion_cause =>
            {
                lifecycle
            }
            _ => {
                return Err(StoreError::InvalidPath(
                    "invalid delete completion".to_owned(),
                ));
            }
        };
        let managed_relative_path = self.resolve_screenshot_path(
            requested.artifact_id.as_str(),
            requested.source_event_id.as_str(),
        )?;
        let request_record = self.journal.append_event(request, faults)?;
        self.projector
            .project_record(&request_record, FaultInjector::none())?;
        faults.check(FaultPoint::AfterDeleteRequest)?;
        match self.root.unlink_with_boundary(&managed_relative_path, || {
            faults.check(FaultPoint::AfterImageUnlink)
        }) {
            Ok(()) => {}
            Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = managed_relative_path
                    .rsplit_once('/')
                    .map(|(parent, _)| parent)
                    .ok_or_else(|| StoreError::InvalidPath(managed_relative_path.clone()))?;
                self.root.sync_directory(parent)?;
            }
            Err(error) => return Err(error),
        }
        faults.check(FaultPoint::AfterImageUnlinkDirectorySync)?;
        let completion_record = self.journal.append_event(completion, faults)?;
        faults.check(FaultPoint::AfterDeleteCompletion)?;
        self.projector
            .project_record(&completion_record, FaultInjector::none())?;
        let _ = completed;
        Ok(())
    }

    fn resolve_screenshot_path(&self, artifact_id: &str, source_event_id: &str) -> Result<String> {
        let records = self.journal.scan_all(crate::JournalFamily::Events, false)?;
        for record in records.records {
            if record.stable_id() != source_event_id {
                continue;
            }
            let event = EventEnvelope::parse(
                std::str::from_utf8(record.body_bytes())
                    .map_err(|error| StoreError::InvalidPath(error.to_string()))?,
            )?;
            if let EventPayload::ObservationAttempt(attempt) = &event.payload
                && let ObservationContent::Captured(content) = &attempt.content
                && let Some(image) = &content.image
                && image.artifact_id.as_str() == artifact_id
            {
                let derived = derived_screenshot_path(&event, artifact_id);
                if image.managed_relative_path.as_str() != derived {
                    return Err(StoreError::InvalidPath(
                        "canonical image path violates screenshot derivation".to_owned(),
                    ));
                }
                return Ok(derived);
            }
        }
        Err(StoreError::InvalidPath(
            "screenshot source observation was not found".to_owned(),
        ))
    }
}

pub(crate) fn derived_screenshot_path(event: &EventEnvelope, artifact_id: &str) -> String {
    format!(
        "screenshots/{}/{}.heic",
        event.recorded_at.format("%Y-%m-%d"),
        artifact_id
    )
}

fn current_revision(root: &ManagedRoot, artifact_id: &str) -> Result<Option<String>> {
    let directory = format!("derived/{artifact_id}");
    if !root.path().join(&directory).exists() {
        return Ok(None);
    }
    let mut revisions = Vec::new();
    for name in root.list_file_names(&directory)? {
        if name.ends_with(".json") {
            let relative = format!("{directory}/{name}");
            let bytes = root.read(&relative)?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|error| StoreError::InvalidPath(error.to_string()))?;
            revisions.push(DerivedArtifactRevision::parse(text)?);
        }
    }
    Ok(order_artifact_chains(revisions)?
        .last()
        .map(|revision| revision.revision_id.to_string()))
}

fn order_artifact_chains(
    revisions: Vec<DerivedArtifactRevision>,
) -> Result<Vec<DerivedArtifactRevision>> {
    let mut groups = BTreeMap::<String, Vec<DerivedArtifactRevision>>::new();
    for revision in revisions {
        groups
            .entry(revision.artifact_id.to_string())
            .or_default()
            .push(revision);
    }
    let mut ordered = Vec::new();
    for (_artifact_id, group) in groups {
        let mut by_id = HashMap::<String, DerivedArtifactRevision>::new();
        let mut child_by_prior = HashMap::<String, String>::new();
        let mut roots = Vec::new();
        for revision in &group {
            if by_id
                .insert(revision.revision_id.to_string(), revision.clone())
                .is_some()
            {
                return Err(StoreError::ArtifactConflict);
            }
            if let Some(prior) = &revision.prior_revision_id {
                if child_by_prior
                    .insert(prior.to_string(), revision.revision_id.to_string())
                    .is_some()
                {
                    return Err(StoreError::ArtifactConflict);
                }
            } else {
                roots.push(revision.revision_id.to_string());
            }
        }
        if roots.len() != 1 {
            return Err(StoreError::ArtifactConflict);
        }
        for revision in &group {
            if let Some(prior) = &revision.prior_revision_id
                && !by_id.contains_key(prior.as_str())
            {
                return Err(StoreError::ArtifactConflict);
            }
        }
        let mut next = roots.pop();
        let mut emitted = 0_usize;
        while let Some(revision_id) = next {
            let revision = by_id
                .remove(&revision_id)
                .ok_or(StoreError::ArtifactConflict)?;
            next = child_by_prior.get(&revision_id).cloned();
            ordered.push(revision);
            emitted += 1;
        }
        if emitted != group.len() || !by_id.is_empty() {
            return Err(StoreError::ArtifactConflict);
        }
    }
    Ok(ordered)
}

fn directory_names(path: impl AsRef<std::path::Path>) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name().into_string().map_err(|_| {
            StoreError::InvalidPath("artifact directory name is not valid UTF-8".to_owned())
        })?;
        names.push(name);
    }
    names.sort();
    Ok(names)
}
