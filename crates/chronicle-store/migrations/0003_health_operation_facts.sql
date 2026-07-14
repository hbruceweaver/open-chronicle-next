CREATE TABLE IF NOT EXISTS health_operation_facts (
  fact_type TEXT NOT NULL CHECK (fact_type IN (
    'scheduled-attempt',
    'successful-capture',
    'successful-ocr',
    'event-projected',
    'chunk-projected'
  )),
  stable_id TEXT NOT NULL,
  occurred_at TEXT NOT NULL,
  occurred_epoch INTEGER NOT NULL,
  occurred_subsec_nanos INTEGER NOT NULL
    CHECK (occurred_subsec_nanos >= 0 AND occurred_subsec_nanos < 1000000000),
  PRIMARY KEY (fact_type, stable_id)
) STRICT;

CREATE INDEX IF NOT EXISTS health_operation_facts_latest
  ON health_operation_facts(
    fact_type, occurred_epoch DESC, occurred_subsec_nanos DESC, stable_id DESC);
