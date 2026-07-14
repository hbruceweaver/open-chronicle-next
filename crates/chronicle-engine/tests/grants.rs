mod common;

use std::error::Error;

use chronicle_domain::{
    ClientId, ContentClass, DisclosureGrant, DisclosureLimits, GrantId, GrantState, GrantTimeScope,
    ReceiptId, UtcRange,
};
use chronicle_engine::SharedService;
use chronicle_store::{LockManager, StoreGeneration};
use chrono::{DateTime, Utc};

fn at(value: &str) -> DateTime<Utc> {
    value.parse().expect("valid UTC fixture timestamp")
}

#[test]
fn grant_receipts_are_durable_reopenable_and_client_bound() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, _projector) = common::store()?;
    let service = SharedService::open(root.clone(), sqlite.clone())?;
    let grant = DisclosureGrant {
        schema_version: "1.0".to_owned(),
        grant_id: GrantId::new("durable-grant")?,
        client_id: ClientId::new("claude")?,
        receipt_id: ReceiptId::new("durable-receipt")?,
        time_scope: GrantTimeScope::Absolute {
            range: UtcRange {
                start: at("2026-07-13T09:00:00Z"),
                end: at("2026-07-13T10:00:00Z"),
            },
        },
        content_classes: vec![ContentClass::Metadata],
        created_at: at("2026-07-13T08:00:00Z"),
        expires_at: at("2026-07-14T08:00:00Z"),
        state: GrantState::Active,
        limits: DisclosureLimits {
            max_page_items: 10,
            max_response_bytes: 16 * 1024,
            max_cumulative_bytes: 64 * 1024,
        },
        disclosed_bytes: 0,
        store_generation: 1,
    };
    service.install_grant(grant.clone())?;
    drop(service);

    let reopened = SharedService::open(root, sqlite)?;
    assert_eq!(reopened.grant(&grant.grant_id)?, grant);
    assert!(
        reopened
            .grant_receipt_bytes()?
            .windows("durable-grant".len())
            .any(|window| window == b"durable-grant")
    );
    Ok(())
}

#[test]
fn grant_mutation_waits_for_reset_then_rejects_the_old_generation() -> Result<(), Box<dyn Error>> {
    let (_temporary, root, sqlite, _projector) = common::store()?;
    let service = SharedService::open(root.clone(), sqlite)?;
    let locks = LockManager::new(root.clone(), std::time::Duration::from_secs(2));
    let maintenance = locks.exclusive_maintenance()?;
    let install_service = service.clone();
    let install = std::thread::spawn(move || {
        let grant = DisclosureGrant {
            schema_version: "1.0".to_owned(),
            grant_id: GrantId::new("racing-grant").expect("grant ID"),
            client_id: ClientId::new("claude").expect("client ID"),
            receipt_id: ReceiptId::new("racing-receipt").expect("receipt ID"),
            time_scope: GrantTimeScope::Absolute {
                range: UtcRange {
                    start: at("2026-07-13T09:00:00Z"),
                    end: at("2026-07-13T10:00:00Z"),
                },
            },
            content_classes: vec![ContentClass::Metadata],
            created_at: at("2026-07-13T08:00:00Z"),
            expires_at: at("2026-07-14T08:00:00Z"),
            state: GrantState::Active,
            limits: DisclosureLimits {
                max_page_items: 10,
                max_response_bytes: 16 * 1024,
                max_cumulative_bytes: 64 * 1024,
            },
            disclosed_bytes: 0,
            store_generation: 1,
        };
        install_service.install_grant(grant)
    });
    let generation = StoreGeneration::load(&root)?;
    let advanced = generation.increment(&root)?;
    assert_eq!(advanced.generation, 2);
    drop(maintenance);
    let error = install
        .join()
        .map_err(|_| "grant install thread panicked")?;
    assert!(matches!(
        error,
        Err(chronicle_engine::SharedServiceError::StaleGeneration {
            expected: 1,
            actual: 2
        })
    ));
    assert!(matches!(
        service.revoke_grant(&GrantId::new("racing-grant")?, at("2026-07-13T09:01:00Z")),
        Err(chronicle_engine::SharedServiceError::StaleGeneration {
            expected: 1,
            actual: 2
        })
    ));
    assert!(
        !service
            .grant_receipt_bytes()?
            .windows("racing-grant".len())
            .any(|window| window == b"racing-grant")
    );
    Ok(())
}
