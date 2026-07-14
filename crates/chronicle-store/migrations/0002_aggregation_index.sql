BEGIN IMMEDIATE;

CREATE TABLE IF NOT EXISTS aggregation_pending_buckets (
  device_id TEXT NOT NULL,
  bucket_start TEXT NOT NULL,
  bucket_start_epoch INTEGER NOT NULL,
  finalization_cadence_seconds INTEGER NOT NULL CHECK (finalization_cadence_seconds IN (30, 60)),
  generation_at TEXT,
  PRIMARY KEY (device_id, bucket_start)
) STRICT;

CREATE TABLE IF NOT EXISTS aggregation_bucket_events (
  device_id TEXT NOT NULL,
  bucket_start TEXT NOT NULL,
  event_id TEXT NOT NULL REFERENCES events(event_id) ON DELETE CASCADE,
  PRIMARY KEY (device_id, bucket_start, event_id)
) STRICT;

CREATE INDEX IF NOT EXISTS aggregation_pending_due_order
  ON aggregation_pending_buckets(bucket_start_epoch, device_id);

CREATE TABLE IF NOT EXISTS aggregation_build_state (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  aggregator_version TEXT NOT NULL,
  store_generation INTEGER NOT NULL CHECK (store_generation > 0)
) STRICT;

INSERT INTO aggregation_pending_buckets(
  device_id, bucket_start, bucket_start_epoch,
  finalization_cadence_seconds, generation_at)
SELECT
  json_extract(body_json, '$.device_id'),
  strftime('%Y-%m-%dT%H:%M:%S+00:00',
           (unixepoch(json_extract(body_json, '$.scheduled_at')) / 300) * 300,
           'unixepoch'),
  (unixepoch(json_extract(body_json, '$.scheduled_at')) / 300) * 300,
  json_extract(body_json, '$.payload.data.cadence_seconds'),
  NULL
FROM events
WHERE kind='observation-attempt'
ON CONFLICT(device_id, bucket_start) DO UPDATE SET
  finalization_cadence_seconds=max(
    aggregation_pending_buckets.finalization_cadence_seconds,
    excluded.finalization_cadence_seconds);

INSERT OR IGNORE INTO aggregation_bucket_events(device_id, bucket_start, event_id)
SELECT
  json_extract(body_json, '$.device_id'),
  strftime('%Y-%m-%dT%H:%M:%S+00:00',
           (unixepoch(json_extract(body_json, '$.scheduled_at')) / 300) * 300,
           'unixepoch'),
  event_id
FROM events
WHERE kind='observation-attempt';

WITH RECURSIVE gap_buckets(event_id, device_id, bucket_epoch, end_epoch) AS (
  SELECT
    event_id,
    json_extract(body_json, '$.device_id'),
    (unixepoch(json_extract(body_json, '$.payload.data.start')) / 300) * 300,
    unixepoch(json_extract(body_json, '$.payload.data.end'))
  FROM events
  WHERE kind='recording-gap'
  UNION ALL
  SELECT event_id, device_id, bucket_epoch + 300, end_epoch
  FROM gap_buckets
  WHERE bucket_epoch + 300 < end_epoch
)
INSERT INTO aggregation_pending_buckets(
  device_id, bucket_start, bucket_start_epoch,
  finalization_cadence_seconds, generation_at)
SELECT
  device_id,
  strftime('%Y-%m-%dT%H:%M:%S+00:00', bucket_epoch, 'unixepoch'),
  bucket_epoch,
  30,
  NULL
FROM gap_buckets
WHERE true
ON CONFLICT(device_id, bucket_start) DO NOTHING;

WITH RECURSIVE gap_buckets(event_id, device_id, bucket_epoch, end_epoch) AS (
  SELECT
    event_id,
    json_extract(body_json, '$.device_id'),
    (unixepoch(json_extract(body_json, '$.payload.data.start')) / 300) * 300,
    unixepoch(json_extract(body_json, '$.payload.data.end'))
  FROM events
  WHERE kind='recording-gap'
  UNION ALL
  SELECT event_id, device_id, bucket_epoch + 300, end_epoch
  FROM gap_buckets
  WHERE bucket_epoch + 300 < end_epoch
)
INSERT OR IGNORE INTO aggregation_bucket_events(device_id, bucket_start, event_id)
SELECT
  device_id,
  strftime('%Y-%m-%dT%H:%M:%S+00:00', bucket_epoch, 'unixepoch'),
  event_id
FROM gap_buckets;

PRAGMA user_version = 2;
COMMIT;
