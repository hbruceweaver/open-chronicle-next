CREATE INDEX IF NOT EXISTS retention_state_health
  ON retention_state(state, expires_at, artifact_id);
