// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the precondition a DOMAIN-SCOPED `ListInstanceTypeDetails` read
//! carries: the retrieve-AZ flag.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`]
//! and the listing helpers from [`super::instance_type_catalog`], which owns this
//! operation's suite; this one adds no second copy of either.
//!
//! The rule under test: `ListInstanceTypeDetails$DomainName` scopes the read to a domain,
//! and the ONLY thing that scope decides is which Availability Zones the entries report —
//! `RetrieveAZs` is "an optional parameter that specifies the Availability Zones for the
//! domain" and the catalog itself is the named release's, the same scoped or unscoped
//! (`smithy_query(query="operation",operation="ListInstanceTypeDetails")`). Naming a
//! domain is therefore asking for that domain's instance-type Availability Zone
//! information, which the answer carries only when the flag is enabled, so a scoped read
//! that does not enable it is refused with the modelled `ValidationException` (HTTP 400)
//! and the service's fixed sentence for the flag.
//!
//! So the assertions span SETS of inputs: a table of scoped reads that do not enable the
//! flag — the flag absent and sent false, beside each other optional member, and naming a
//! domain that exists and one that does not, so the refusal is the precondition's and not
//! a resource lookup's; the same reads with the flag enabled, which are answered with the
//! zones; and every UNSCOPED read, which names no domain and is answered whatever the
//! flag says.

use serde_json::json;

use super::domain_lifecycle::{error_code, Emulator, RECORDED_NOT_FOUND_STATUS};
use super::instance_type_catalog::list;

/// The engine release every read below is taken for.
const VERSION: &str = "OpenSearch_2.11";

/// This suite's OWN statement of the refusal text: the fixed sentence, with no variable
/// part. Restated here rather than imported from the service, so changing it in the
/// service fails this suite instead of being asserted against itself.
const EXPECTED_MESSAGE: &str =
    "Retrieve AZ flag need to be enabled to get Instance Type AZ information.";

/// The query members a scoped read is probed beside: none, the other filter, a page size,
/// a paging token, and the flag sent explicitly false.
const BESIDE: [&str; 5] = [
    "",
    "instanceType=m5.large.search",
    "maxResults=5",
    "nextToken=m5.large.search",
    "retrieveAZs=false",
];

#[tokio::test]
async fn a_domain_scoped_read_without_the_flag_is_refused() {
    let emulator = Emulator::new();
    let held = "az-scope-domain";
    let created = emulator.create(json!({ "DomainName": held })).await;
    assert_eq!(
        created.status, 200,
        "case create {held}: expected HTTP 200, got {} {:?}",
        created.status, created.body
    );

    // A domain the account holds and one it does not: the precondition is judged before
    // the scope is resolved, so both are refused the same way.
    for domain in [held, "az-scope-absent"] {
        for beside in BESIDE {
            let query = if beside.is_empty() {
                format!("domainName={domain}")
            } else {
                format!("domainName={domain}&{beside}")
            };
            let answer = list(&emulator, VERSION, &query).await;
            assert_eq!(
                answer.status, 400,
                "case {query}: expected HTTP 400, got {} {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                error_code(&answer),
                "ValidationException",
                "case {query}: expected the modelled validation refusal, got {:?}",
                answer.body
            );
            let actual = answer.body["message"].as_str().unwrap_or_default();
            assert_eq!(
                actual, EXPECTED_MESSAGE,
                "case {query}: expected the sentence naming the flag, got {actual:?}"
            );
            assert!(
                answer.body["InstanceTypeDetails"].is_null(),
                "case {query}: a refused read carries no listing, got {:?}",
                answer.body["InstanceTypeDetails"]
            );
        }
    }
}

#[tokio::test]
async fn a_domain_scoped_read_with_the_flag_is_answered_with_the_zones() {
    let emulator = Emulator::new();
    let held = "az-scope-served";
    let created = emulator.create(json!({ "DomainName": held })).await;
    assert_eq!(
        created.status, 200,
        "case create {held}: expected HTTP 200, got {} {:?}",
        created.status, created.body
    );

    for beside in BESIDE {
        // One member is sent once: the row that sends the flag false is this row's same
        // read with the flag enabled rather than a second value for it.
        let beside = if beside.starts_with("retrieveAZs") {
            ""
        } else {
            beside
        };
        let query = if beside.is_empty() {
            format!("domainName={held}&retrieveAZs=true")
        } else {
            format!("domainName={held}&retrieveAZs=true&{beside}")
        };
        let answer = list(&emulator, VERSION, &query).await;
        assert_eq!(
            answer.status, 200,
            "case {query}: expected the scoped read to be answered, got {} {:?}",
            answer.status, answer.body
        );
        let entries = answer.body["InstanceTypeDetails"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            !entries.is_empty(),
            "case {query}: expected entries to report zones for, got {:?}",
            answer.body
        );
        for entry in &entries {
            assert!(
                entry["AvailabilityZones"].is_array(),
                "case {query}: expected the AZ information the read asked for, got {entry:?}"
            );
        }
    }

    // The scope still has to resolve: with the flag enabled, a domain the account holds
    // none of is the modelled not-found refusal.
    let answer = list(
        &emulator,
        VERSION,
        "domainName=az-scope-absent&retrieveAZs=true",
    )
    .await;
    assert_eq!(
        answer.status, RECORDED_NOT_FOUND_STATUS,
        "case an absent domain with the flag: expected HTTP {RECORDED_NOT_FOUND_STATUS}, \
         got {} {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        error_code(&answer),
        "ResourceNotFoundException",
        "case an absent domain with the flag: expected the modelled not-found refusal, \
         got {:?}",
        answer.body
    );
}

#[tokio::test]
async fn an_unscoped_read_is_answered_whatever_the_flag_says() {
    let emulator = Emulator::new();

    // Naming no domain asks for no domain's zones, so nothing is preconditioned: every
    // other member, with the flag absent, false and true.
    for beside in BESIDE {
        for flag in ["", "retrieveAZs=true"] {
            // One member is sent once, as above.
            if beside.starts_with("retrieveAZs") && !flag.is_empty() {
                continue;
            }
            let query = [beside, flag]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<&str>>()
                .join("&");
            let answer = list(&emulator, VERSION, &query).await;
            assert_eq!(
                answer.status, 200,
                "case {query:?}: expected an unscoped read to be answered, got {} {:?}",
                answer.status, answer.body
            );
        }
    }
}
