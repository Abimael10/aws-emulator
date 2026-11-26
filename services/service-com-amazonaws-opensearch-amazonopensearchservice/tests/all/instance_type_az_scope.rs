// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the precondition a `ListInstanceTypeDetails` read that asks for
//! instance-type Availability Zone information carries: the retrieve-AZ flag.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The harness comes from [`super::domain_lifecycle`]
//! and the listing helpers from [`super::instance_type_catalog`], which owns this
//! operation's suite; this one adds no second copy of either.
//!
//! The rule under test: two members of this read carry information the service has to
//! resolve against Availability Zones — `DomainName`, which scopes the read to a domain
//! and whose ONLY effect is which Region's zones the entries report, and `InstanceType`,
//! the hardware those zones are asked about ("an optional parameter that lists
//! information for a given instance type") — while `RetrieveAZs` is "an optional
//! parameter that specifies the Availability Zones for the domain"
//! (`smithy_query(query="operation",operation="ListInstanceTypeDetails")`), and the
//! catalog itself is the named release's, the same narrowed, scoped or neither. Naming
//! either member is therefore asking for instance-type Availability Zone information,
//! which the answer carries only when the flag is enabled, so a read that names one
//! without enabling it is refused with the modelled `ValidationException` (HTTP 400) and
//! the service's fixed sentence for the flag. The model states no interaction between
//! these members, so that is recorded behaviour.
//!
//! So the assertions span SETS of inputs: a table of reads that ask for that information
//! and do not enable the flag — each AZ-information member alone and both together,
//! naming a domain that exists and one that does not and an instance type the catalog
//! publishes and one it does not, with the flag absent and sent false, and beside each
//! other optional member — so the refusal is the precondition's and neither a resource
//! lookup's nor the value's; the same reads with the flag enabled, which are answered
//! with the zones; and every read that asks for no such information, which is answered
//! whatever the flag says.

use serde_json::json;

use super::domain_lifecycle::{error_code, Emulator, RECORDED_NOT_FOUND_STATUS};
use super::instance_type_catalog::list;

/// The engine release every read below is taken for.
const VERSION: &str = "OpenSearch_2.11";

/// The domain every row that names one names, created by the cases that need it to exist.
const HELD: &str = "az-scope-domain";

/// This suite's OWN statement of the refusal text: the fixed sentence, with no variable
/// part. Restated here rather than imported from the service, so changing it in the
/// service fails this suite instead of being asserted against itself.
const EXPECTED_MESSAGE: &str =
    "Retrieve AZ flag need to be enabled to get Instance Type AZ information.";

/// The query members a read is probed beside, so the rule is not about one shape of
/// request: none, a page size, a paging token, and the flag sent explicitly false.
const BESIDE: [&str; 4] = ["", "maxResults=5", "nextToken=m5.large.search", "retrieveAZs=false"];

/// The ways a read can ask for instance-type Availability Zone information: the domain
/// whose Region's zones an entry would report, the instance type those zones would be
/// asked about, and both together.
///
/// The domain rows name one this account holds and one it does not, and the instance-type
/// rows a type the published catalog carries and a value of the member's own shape it
/// carries none for, so the refusal is neither the missing domain's nor the unpublished
/// type's.
fn asks_for_az_information() -> Vec<String> {
    vec![
        format!("domainName={HELD}"),
        "domainName=az-scope-absent".to_owned(),
        "instanceType=m5.large.search".to_owned(),
        "instanceType=not.a.real.search".to_owned(),
        format!("domainName={HELD}&instanceType=m5.large.search"),
    ]
}

/// `query`, with `beside` appended where it names a member.
fn beside(query: &str, beside: &str) -> String {
    if beside.is_empty() {
        query.to_owned()
    } else {
        format!("{query}&{beside}")
    }
}

/// Create [`HELD`], asserting it was accepted.
async fn given_domain(emulator: &Emulator) {
    let created = emulator.create(json!({ "DomainName": HELD })).await;
    assert_eq!(
        created.status, 200,
        "case create {HELD}: expected HTTP 200, got {} {:?}",
        created.status, created.body
    );
}

#[tokio::test]
async fn a_read_asking_for_az_information_without_the_flag_is_refused() {
    let emulator = Emulator::new();
    given_domain(&emulator).await;

    for asking in asks_for_az_information() {
        for extra in BESIDE {
            let query = beside(&asking, extra);
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
async fn a_read_asking_for_az_information_with_the_flag_is_answered_with_the_zones() {
    let emulator = Emulator::new();
    given_domain(&emulator).await;

    // The same reads, enabling the flag: each is answered, and every entry carries the
    // Availability Zone information the read asked for. The rows naming something the
    // service resolves nothing for are taken up below.
    for asking in [
        format!("domainName={HELD}"),
        "instanceType=m5.large.search".to_owned(),
        format!("domainName={HELD}&instanceType=m5.large.search"),
    ] {
        for extra in ["", "maxResults=5", "nextToken=m5.large.search"] {
            let query = beside(&format!("{asking}&retrieveAZs=true"), extra);
            let answer = list(&emulator, VERSION, &query).await;
            assert_eq!(
                answer.status, 200,
                "case {query}: expected the read to be answered, got {} {:?}",
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
    }

    // What the read names still has to resolve once the flag is enabled: a domain the
    // account holds none of is the modelled not-found refusal, and an instance type no
    // zone offers is refused as the zones answer it
    // ([`super::instance_type_az_resolution`] owns that sentence).
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

    let answer = list(
        &emulator,
        VERSION,
        "instanceType=not.a.real.search&retrieveAZs=true",
    )
    .await;
    assert_eq!(
        answer.status, 400,
        "case an unpublished type with the flag: expected HTTP 400, got {} {:?}",
        answer.status, answer.body
    );
    let actual = answer.body["message"].as_str().unwrap_or_default();
    assert_ne!(
        actual, EXPECTED_MESSAGE,
        "case an unpublished type with the flag: the flag was enabled, so the refusal is \
         the zones' and not the precondition's, got {actual:?}"
    );
}

#[tokio::test]
async fn a_read_asking_for_no_az_information_is_answered_whatever_the_flag_says() {
    let emulator = Emulator::new();

    // Naming neither the domain nor an instance type asks for no Availability Zone
    // information, so nothing is preconditioned: every other member, with the flag absent,
    // false and true.
    for extra in BESIDE {
        for flag in ["", "retrieveAZs=true"] {
            // One member is sent once: the row that sends the flag false is this row's
            // same read with the flag enabled rather than a second value for it.
            if extra.starts_with("retrieveAZs") && !flag.is_empty() {
                continue;
            }
            let query = [extra, flag]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<&str>>()
                .join("&");
            let answer = list(&emulator, VERSION, &query).await;
            assert_eq!(
                answer.status, 200,
                "case {query:?}: expected a read asking for no zone information to be \
                 answered, got {} {:?}",
                answer.status, answer.body
            );
        }
    }
}
