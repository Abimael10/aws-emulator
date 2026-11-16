// SPDX-License-Identifier: CC0-1.0

//! `DisableDomainTransferLock` over awsJson1_1: the EPP status it clears and the
//! operation record it appends.
//!
//! The rule pinned here is the operation's own documentation: it "removes the transfer
//! lock on the domain (specifically the `clientTransferProhibited` status) to allow
//! domain transfers", and "[s]uccessful submission returns an operation ID that you can
//! use to track the progress and completion of the action". So the lock a registration
//! carries is gone from the status list afterwards, and the id the call returns resolves
//! through `GetOperationDetail`.
//!
//! Every test drives the real wire path through the shared harness in
//! `tests/all/domain_lifecycle.rs`.

use crate::domain_lifecycle::{register, Emulator};
use serde_json::{json, Value};

/// The `StatusList` `GetDomainDetail` reports for `name`.
async fn status_list(emulator: &Emulator, name: &str) -> Vec<String> {
    let (status, body) = emulator
        .call("GetDomainDetail", &json!({ "DomainName": name }))
        .await;
    assert_eq!(
        status, 200,
        "case {name:?}: expected GetDomainDetail to answer 200, got {status} {body}"
    );
    body["StatusList"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn disabling_the_transfer_lock_clears_the_status_and_is_tracked() {
    // Rows differ along the TLD, so the registrar of record differs with them; the lock
    // a registration carries, and the call that removes it, are the same either way.
    let emulator = Emulator::new();

    for name in [
        "sim-probe-lock.com",
        "sim-probe-lock.io",
        "sim-probe-lock.co.uk",
    ] {
        register(&emulator, name).await;
        assert!(
            status_list(&emulator, name)
                .await
                .contains(&"clientTransferProhibited".to_string()),
            "case {name:?}: a registration must carry the transfer lock, got {:?}",
            status_list(&emulator, name).await
        );

        let (status, body) = emulator
            .call("DisableDomainTransferLock", &json!({ "DomainName": name }))
            .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected DisableDomainTransferLock to answer 200, got \
             {status} {body}"
        );
        let operation_id = body["OperationId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {name:?}: expected an OperationId, got {body}")
            })
            .to_string();

        let after = status_list(&emulator, name).await;
        assert!(
            !after.contains(&"clientTransferProhibited".to_string()),
            "case {name:?}: expected the transfer lock to be gone, got {after:?}"
        );

        // The id the call returned is the record that tracks it.
        let (status, detail): (u16, Value) = emulator
            .call(
                "GetOperationDetail",
                &json!({ "OperationId": operation_id }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected GetOperationDetail to resolve {operation_id}, got \
             {status} {detail}"
        );
        assert_eq!(
            (detail["DomainName"].as_str(), detail["Type"].as_str()),
            (Some(name), Some("DOMAIN_LOCK")),
            "case {name:?}: expected a DOMAIN_LOCK record for this domain, got {detail}"
        );

        // Asked again, the lock is already off: the end state is the same, so the call
        // is served and tracked again rather than refused.
        let (status, body) = emulator
            .call("DisableDomainTransferLock", &json!({ "DomainName": name }))
            .await;
        assert_eq!(
            status, 200,
            "case {name:?} asked twice: expected 200, got {status} {body}"
        );
        assert!(
            body["OperationId"].as_str().is_some_and(|id| id != operation_id),
            "case {name:?} asked twice: expected a new OperationId, got {body}"
        );
        let after = status_list(&emulator, name).await;
        assert!(
            !after.contains(&"clientTransferProhibited".to_string()),
            "case {name:?} asked twice: expected the lock to stay off, got {after:?}"
        );
    }
}
