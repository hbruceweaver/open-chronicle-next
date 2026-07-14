CREATE TABLE IF NOT EXISTS projection_identity (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  instance_id TEXT NOT NULL
) STRICT;
