mod common;

use std::error::Error;

use chronicle_store::StoreQueries;
use chrono::{DateTime, Duration, Utc};

#[test]
fn pending_backlog_over_one_hundred_thousand_advances_in_bounded_batches()
-> Result<(), Box<dyn Error>> {
    let (_temporary, _root, sqlite, _projector) = common::store()?;
    let start: DateTime<Utc> = "2025-01-01T00:00:00Z".parse()?;
    let mut connection = sqlite.connection()?;
    let transaction = connection.transaction()?;
    {
        let mut insert = transaction.prepare(
            "INSERT INTO aggregation_pending_buckets(
               device_id, bucket_start, bucket_start_epoch,
               finalization_cadence_seconds, generation_at)
             VALUES('backlog-device', ?1, ?2, 30, NULL)",
        )?;
        for index in 0..100_005_i64 {
            let at = start + Duration::seconds(index * 300);
            insert.execute(rusqlite::params![at.to_rfc3339(), at.timestamp()])?;
        }
    }
    transaction.commit()?;

    let queries = StoreQueries::new(sqlite.clone());
    let now: DateTime<Utc> = "2026-01-01T00:00:00Z".parse()?;
    let mut processed = 0_usize;
    let mut batches = 0_usize;
    loop {
        let (batch, has_more) = queries.due_aggregation_bucket_batch(now, 30)?;
        assert!(batch.len() <= 1_024);
        if batch.is_empty() {
            assert!(!has_more);
            break;
        }
        processed += batch.len();
        batches += 1;
        let last = batch.last().ok_or("batch disappeared")?;
        sqlite.connection()?.execute(
            "DELETE FROM aggregation_pending_buckets
             WHERE device_id='backlog-device' AND bucket_start <= ?1",
            [last.bucket_start.to_rfc3339()],
        )?;
        if !has_more {
            break;
        }
    }
    assert_eq!(processed, 100_005);
    assert!(batches > 90);
    assert!(queries.pending_aggregation_buckets()?.is_empty());

    let connection = sqlite.connection()?;
    let pending_plan: String = connection.query_row(
        "EXPLAIN QUERY PLAN
         SELECT device_id, bucket_start FROM aggregation_pending_buckets
         WHERE bucket_start_epoch + 330 <= ?1
         ORDER BY bucket_start_epoch, device_id LIMIT 1025",
        [now.timestamp()],
        |row| row.get(3),
    )?;
    assert!(pending_plan.contains("aggregation_pending_due_order"));
    let membership_plan: String = connection.query_row(
        "EXPLAIN QUERY PLAN
         SELECT events.body_json
         FROM aggregation_bucket_events membership
         JOIN events ON events.event_id=membership.event_id
         WHERE membership.device_id=?1 AND membership.bucket_start=?2",
        rusqlite::params!["backlog-device", start.to_rfc3339()],
        |row| row.get(3),
    )?;
    assert!(membership_plan.contains("aggregation_bucket_events"));
    Ok(())
}
