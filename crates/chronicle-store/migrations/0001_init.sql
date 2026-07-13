PRAGMA user_version = 1;

CREATE TABLE IF NOT EXISTS schema_versions (
  component TEXT PRIMARY KEY,
  version INTEGER NOT NULL,
  build_id TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS projection_cursors (
  family TEXT NOT NULL,
  shard TEXT NOT NULL,
  byte_offset INTEGER NOT NULL CHECK (byte_offset >= 0),
  PRIMARY KEY (family, shard)
) STRICT;

CREATE TABLE IF NOT EXISTS events (
  event_id TEXT PRIMARY KEY,
  checksum TEXT NOT NULL,
  kind TEXT NOT NULL,
  recorded_at TEXT NOT NULL,
  body_json TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS observations (
  event_id TEXT PRIMARY KEY REFERENCES events(event_id) ON DELETE CASCADE,
  attempt_status TEXT NOT NULL,
  evidence_state TEXT NOT NULL,
  presence_state TEXT NOT NULL,
  ocr_state TEXT NOT NULL,
  application_bundle_id TEXT,
  process_name TEXT,
  window_title TEXT,
  authorized_domain TEXT,
  content_hash TEXT,
  ocr_text TEXT
) STRICT;

CREATE VIRTUAL TABLE IF NOT EXISTS ocr_fts USING fts5(event_id UNINDEXED, text);

CREATE TABLE IF NOT EXISTS screenshot_lifecycle (
  artifact_id TEXT PRIMARY KEY,
  source_event_id TEXT NOT NULL,
  last_event_id TEXT NOT NULL REFERENCES events(event_id),
  state TEXT NOT NULL,
  deletion_cause TEXT,
  requested_at TEXT,
  completed_at TEXT
) STRICT;

CREATE TABLE IF NOT EXISTS chunk_revisions (
  revision_id TEXT PRIMARY KEY,
  chunk_id TEXT NOT NULL,
  prior_revision_id TEXT,
  checksum TEXT NOT NULL,
  window_start TEXT NOT NULL,
  window_end TEXT NOT NULL,
  generated_at TEXT NOT NULL,
  input_digest TEXT NOT NULL,
  body_json TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS current_chunks (
  chunk_id TEXT PRIMARY KEY,
  revision_id TEXT NOT NULL REFERENCES chunk_revisions(revision_id)
) STRICT;

CREATE TABLE IF NOT EXISTS chunk_evidence_refs (
  revision_id TEXT NOT NULL REFERENCES chunk_revisions(revision_id) ON DELETE CASCADE,
  event_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  PRIMARY KEY (revision_id, event_id)
) STRICT;

CREATE TABLE IF NOT EXISTS chunk_dimensions (
  revision_id TEXT NOT NULL REFERENCES chunk_revisions(revision_id) ON DELETE CASCADE,
  dimension TEXT NOT NULL,
  dimension_key TEXT NOT NULL,
  label TEXT NOT NULL,
  estimated_seconds INTEGER NOT NULL,
  PRIMARY KEY (revision_id, dimension, dimension_key)
) STRICT;

CREATE TABLE IF NOT EXISTS chunk_transitions (
  revision_id TEXT NOT NULL REFERENCES chunk_revisions(revision_id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL,
  at TEXT NOT NULL,
  from_key TEXT,
  to_key TEXT NOT NULL,
  supporting_event_id TEXT NOT NULL,
  PRIMARY KEY (revision_id, ordinal)
) STRICT;

CREATE TABLE IF NOT EXISTS artifact_revisions (
  revision_id TEXT PRIMARY KEY,
  artifact_id TEXT NOT NULL,
  prior_revision_id TEXT,
  created_at TEXT NOT NULL,
  status TEXT NOT NULL,
  checksum TEXT NOT NULL,
  body_json TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS current_artifacts (
  artifact_id TEXT PRIMARY KEY,
  revision_id TEXT NOT NULL REFERENCES artifact_revisions(revision_id)
) STRICT;

CREATE TABLE IF NOT EXISTS artifact_evidence_refs (
  revision_id TEXT NOT NULL REFERENCES artifact_revisions(revision_id) ON DELETE CASCADE,
  evidence_kind TEXT NOT NULL,
  evidence_id TEXT NOT NULL,
  PRIMARY KEY (revision_id, evidence_kind, evidence_id)
) STRICT;

CREATE TABLE IF NOT EXISTS health_snapshots (
  component TEXT PRIMARY KEY,
  state TEXT NOT NULL,
  detail TEXT,
  updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS retention_state (
  artifact_id TEXT PRIMARY KEY,
  state TEXT NOT NULL,
  updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS store_generation (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  generation INTEGER NOT NULL,
  epoch_id TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS aggregation_watermark (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  through_utc TEXT,
  revision_id TEXT
) STRICT;

CREATE TABLE IF NOT EXISTS registration_receipts (
  receipt_id TEXT PRIMARY KEY,
  client_id TEXT NOT NULL,
  receipt_json TEXT NOT NULL,
  updated_at TEXT NOT NULL
) STRICT;
