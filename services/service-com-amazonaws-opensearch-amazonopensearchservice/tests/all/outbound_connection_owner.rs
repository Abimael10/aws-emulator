// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for whose domain the LOCAL side of an outbound cross-cluster
//! connection can be: `CreateOutboundConnection`.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`]
//! and the connection paths from [`super::cross_cluster_connections`], which owns this
//! operation's suite; this one adds no second copy of either.
//!
//! The rule under test: the LOCAL domain of an outbound connection is the source the
//! CALLER connects FROM, so its `AWSDomainInformation$OwnerId` is the caller's own
//! account — a reference naming any other account names no domain this caller holds, and
//! the request is refused with the 400 `ValidationException` and the service's fixed
//! sentence. The REMOTE domain is the other half of a cross-account connection, so its
//! owner is not held to the caller's account
//! (`smithy_query(query="operation",operation="CreateOutboundConnection")`).
//!
//! So the assertions span SETS of inputs: a table of foreign owner ids — another
//! 12-digit account, one differing from the caller's in a single digit, and a third of
//! the same shape — each beside every Region a reference can name, refused and storing
//! nothing; the caller's own account named explicitly and omitted, which are accepted and
//! recorded in full; and the same foreign ids on the REMOTE reference, which are accepted
//! and recorded as given.

use serde_json::{json, Value};

use super::cross_cluster_connections::{create_connection, domain_info, ACCOUNT, REGION};
use super::domain_lifecycle::{error_code, Emulator, PREFIX};

/// This suite's OWN statement of the refusal text: the fixed sentence, with no variable
/// part. Restated here rather than imported from the service, so changing it in the
/// service fails this suite instead of being asserted against itself.
const EXPECTED_MESSAGE: &str = "OwnerId for local domain should be the same as caller account id.";

/// The owner ids that are not the caller's account: another account of the same shape,
/// one differing from the caller's in a single digit, and a third of that shape.
const FOREIGN_OWNERS: [&str; 3] = ["210987654321", "000000000001", "999999999999"];

/// A domain the account holds, created so a refusal is never a missing source domain.
async fn created_domain(emulator: &Emulator, name: &str) {
    let answer = emulator.create(json!({ "DomainName": name })).await;
    assert_eq!(
        answer.status, 200,
        "case create {name}: expected HTTP 200, got {} {:?}",
        answer.status, answer.body
    );
}

/// `DescribeOutboundConnections`: `POST /opensearch/cc/outboundConnection/search`.
async fn describe_connections(emulator: &Emulator) -> Value {
    let answer = emulator
        .call(
            "POST",
            &format!("{PREFIX}/opensearch/cc/outboundConnection/search"),
            Some(json!({})),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case the outbound report: expected HTTP 200, got {} {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// The `ConnectionId`s the outbound report carries.
fn reported_ids(report: &Value) -> Vec<String> {
    report["Connections"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|connection| connection["ConnectionId"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn a_local_domain_owned_by_another_account_is_refused() {
    let emulator = Emulator::new();
    created_domain(&emulator, "owner-source").await;
    created_domain(&emulator, "owner-destination").await;

    for (index, owner) in FOREIGN_OWNERS.into_iter().enumerate() {
        // Beside each Region the reference can name: the caller's own, another, and none
        // — the owner is what the rule is about, whatever the Region says.
        for (position, region) in [None, Some(REGION), Some("eu-west-1")]
            .into_iter()
            .enumerate()
        {
            let alias = format!("ForeignOwner{index}-{position}");
            let case = format!("CreateOutboundConnection(OwnerId={owner}, Region={region:?})");
            let answer = create_connection(
                &emulator,
                json!({
                    "ConnectionAlias": alias,
                    "LocalDomainInfo": domain_info("owner-source", Some(owner), region),
                    "RemoteDomainInfo": domain_info("owner-destination", None, None),
                }),
            )
            .await;
            assert_eq!(
                answer.status, 400,
                "case {case}: expected HTTP 400, got {} {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                error_code(&answer),
                "ValidationException",
                "case {case}: expected the modelled validation refusal, got {:?}",
                answer.body
            );
            let actual = answer.body["message"].as_str().unwrap_or_default();
            assert_eq!(
                actual, EXPECTED_MESSAGE,
                "case {case}: expected the sentence naming the caller's account, got {actual:?}"
            );
            assert!(
                answer.body["ConnectionId"].is_null(),
                "case {case}: a refused create mints no connection, got {:?}",
                answer.body
            );
        }
    }

    // None of them stored anything: the report holds no connection at all.
    let reported = reported_ids(&describe_connections(&emulator).await);
    assert!(
        reported.is_empty(),
        "case the stored set: a refused create stores no connection, got {reported:?}"
    );
}

#[tokio::test]
async fn the_callers_own_account_is_accepted_named_or_omitted() {
    let emulator = Emulator::new();
    created_domain(&emulator, "owner-own-source").await;
    created_domain(&emulator, "owner-own-destination").await;

    let mut minted: Vec<String> = Vec::new();
    for (index, owner) in [None, Some(ACCOUNT)].into_iter().enumerate() {
        let alias = format!("OwnAccount{index}");
        let case = format!("CreateOutboundConnection(OwnerId={owner:?})");
        let answer = create_connection(
            &emulator,
            json!({
                "ConnectionAlias": alias,
                "LocalDomainInfo": domain_info("owner-own-source", owner, None),
                "RemoteDomainInfo": domain_info("owner-own-destination", None, None),
            }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected the caller's own account to be accepted, got {} {:?}",
            answer.status, answer.body
        );
        // Either way the record names the account in full, which is the caller's.
        assert_eq!(
            answer.body["LocalDomainInfo"],
            domain_info("owner-own-source", Some(ACCOUNT), Some(REGION)),
            "case {case}: expected the local reference to be recorded in full, got {:?}",
            answer.body["LocalDomainInfo"]
        );
        minted.push(
            answer.body["ConnectionId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: expected a ConnectionId"))
                .to_owned(),
        );
    }

    // Both are in the stored set, which is what the report answers from.
    let reported = reported_ids(&describe_connections(&emulator).await);
    for id in &minted {
        assert!(
            reported.contains(id),
            "case the stored set: expected the report to carry {id}, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_remote_domain_in_another_account_is_accepted_and_recorded_as_given() {
    let emulator = Emulator::new();
    created_domain(&emulator, "owner-remote-source").await;
    created_domain(&emulator, "owner-remote-destination").await;

    for (index, owner) in FOREIGN_OWNERS.into_iter().enumerate() {
        let alias = format!("ForeignRemote{index}");
        let case = format!("CreateOutboundConnection(RemoteDomainInfo.OwnerId={owner})");
        let answer = create_connection(
            &emulator,
            json!({
                "ConnectionAlias": alias,
                "LocalDomainInfo": domain_info("owner-remote-source", None, None),
                "RemoteDomainInfo": domain_info("owner-remote-destination", Some(owner), None),
            }),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected the other half of the connection to name its own \
             account, got {} {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.body["RemoteDomainInfo"],
            domain_info("owner-remote-destination", Some(owner), Some(REGION)),
            "case {case}: expected the remote reference to be recorded as given, got {:?}",
            answer.body["RemoteDomainInfo"]
        );
    }
}
