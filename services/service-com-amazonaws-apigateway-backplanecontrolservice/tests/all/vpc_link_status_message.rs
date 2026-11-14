// SPDX-License-Identifier: CC0-1.0

//! What a VPC link reports as its `statusMessage`, asserted on the wire.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the
//! `CreateVpcLink`/`GetVpcLink` helpers are this crate's existing harness, reused from
//! `rest_api_lifecycle`, `vpc_links` and `patch_updates`.
//!
//! The RULE under test:
//!
//! * **a VPC link's `statusMessage` is the service's FIXED sentence for the `status` the
//!   link reports at the same instant** — the same text for every link in that status and
//!   a text of its own per status, interpolating nothing from the request or from the
//!   link's record — and every projection of the link (`CreateVpcLink`, `GetVpcLink`,
//!   `GetVpcLinks`, `UpdateVpcLink`) reports that one pair, so a readiness waiter matching
//!   on the message terminates whichever reader it polls. The exception is the status a
//!   link that could NOT be provisioned settles in: "a description about the VPC link
//!   status" (`VpcLink$statusMessage`) is then the cause of THAT link's failure, the one
//!   value its caller has to act on, so it names the target that could not be
//!   provisioned.
//!
//! For the provisioned status the sentence itself is pinned, to the one the service is
//! known to serve; the status a link is still being provisioned in is held to the
//! invariants above instead, because no sentence of the service's has been observed for it
//! to pin.
//!
//! The rows vary what the rule generalises over: how many targets a link names and where
//! the load balancers they name live, which optional members the request carries, how many
//! tags it sets, the length of its name (one at the bound the service accepts), and
//! whether the link can be provisioned at all.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    MAX_STRING_CHARS, VPC_LINK_AVAILABLE, VPC_LINK_FAILED, VPC_LINK_PENDING,
};

use crate::patch_updates::{patch, replace};
use crate::rest_api_lifecycle::{page_items, string_member, Gateway};
use crate::vpc_links::{create_vpc_link, settled_link, target_arn};

/// The sentence a provisioned VPC link reports.
///
/// Spelled out here rather than taken from the crate's own constant: a test that asserts a
/// handler's constant back at it cannot catch the wording drifting away from the service's.
/// It is the one sentence the service is known to serve, so the provisioned status is
/// pinned to it; the other statuses are held to the rule's invariants instead (see
/// [`assert_pair`]), because this suite has no wording of the service's to pin them to.
const AVAILABLE_SENTENCE: &str = "VPC link is ready to route traffic";

/// The sentence each `status` was seen reporting: the rule says there is exactly one per
/// status, so the first reading of a status is what every later reading of it — in any row,
/// from any reader — is held to.
type SentencePerStatus = BTreeMap<String, String>;

/// One VPC link the rule is asserted over.
struct Row {
    case: &'static str,
    /// The `CreateVpcLink` request body.
    request: Value,
    /// The status this link settles in once its provisioning is due.
    settles: &'static str,
    /// Every request value the fixed sentences must be free of — the rule is that they
    /// interpolate nothing from the request.
    carries_nothing_of: Vec<String>,
}

/// A network-load-balancer ARN that names no load balancer, so the link it targets cannot
/// be provisioned: a VPC endpoint service is a resource `targetArns` does not take.
const UNUSABLE_TARGET: &str = "arn:aws:ec2:us-east-1:000000000000:vpc-endpoint-service/vpce-svc-9";

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one target and nothing else",
            request: json!({ "name": "status-minimal", "targetArns": [target_arn(0)] }),
            settles: VPC_LINK_AVAILABLE,
            carries_nothing_of: vec!["status-minimal".to_string(), target_arn(0)],
        },
        Row {
            case: "three targets, described and tagged",
            request: json!({
                "name": "status-three-targets",
                "description": "the private link",
                "targetArns": [target_arn(1), target_arn(2), target_arn(3)],
                "tags": { "team": "platform", "env": "test" },
            }),
            settles: VPC_LINK_AVAILABLE,
            carries_nothing_of: vec![
                "status-three-targets".to_string(),
                "the private link".to_string(),
                target_arn(2),
                "platform".to_string(),
            ],
        },
        Row {
            case: "a target in another Region and account",
            request: json!({
                "name": "status-cross-account",
                "targetArns": [
                    "arn:aws:elasticloadbalancing:eu-west-1:111122223333:loadbalancer/net/nlb/abc1",
                ],
            }),
            settles: VPC_LINK_AVAILABLE,
            carries_nothing_of: vec![
                "status-cross-account".to_string(),
                "eu-west-1".to_string(),
                "111122223333".to_string(),
            ],
        },
        Row {
            // A name at the longest the service stores: the size of a link's own members
            // changes nothing about the sentence for its status.
            case: "a name at the bound the service accepts",
            request: json!({
                "name": "n".repeat(MAX_STRING_CHARS),
                "targetArns": [target_arn(4)],
            }),
            settles: VPC_LINK_AVAILABLE,
            carries_nothing_of: vec!["n".repeat(MAX_STRING_CHARS)],
        },
        Row {
            // The exception to the rule: this link cannot be provisioned, so the
            // description it settles with is the cause of its OWN failure.
            case: "a target that names no network load balancer",
            request: json!({
                "name": "status-unusable-target",
                "targetArns": [UNUSABLE_TARGET],
            }),
            settles: VPC_LINK_FAILED,
            carries_nothing_of: vec!["status-unusable-target".to_string()],
        },
    ]
}

/// Assert what the rule says of the `status`/`statusMessage` pair `projection` carries,
/// record the sentence a fixed-text status reports in `seen`, and answer the status it
/// reported.
///
/// Every status is held to carrying a description at all. A status whose description is
/// FIXED text is held to being the same sentence every other reading of that status
/// carried and to interpolating none of `carries_nothing_of`, and the provisioned one to
/// [`AVAILABLE_SENTENCE`] itself; the status of a link that could not be provisioned is
/// held to naming that link's own unusable target instead.
fn assert_pair(
    case: &str,
    reader: &str,
    projection: &Value,
    carries_nothing_of: &[String],
    seen: &mut SentencePerStatus,
) -> String {
    let status = string_member(projection, "status", case);
    let message = projection["statusMessage"].as_str().unwrap_or_default();
    assert!(
        !message.is_empty(),
        "case {case:?} ({reader}): expected a statusMessage describing status {status:?}, \
         got {projection}"
    );

    if status == VPC_LINK_FAILED {
        assert!(
            message.contains(UNUSABLE_TARGET),
            "case {case:?} ({reader}): expected the description of {VPC_LINK_FAILED} to name \
             the target that could not be provisioned ({UNUSABLE_TARGET}), got {message:?}"
        );
        return status;
    }

    if status == VPC_LINK_AVAILABLE {
        assert_eq!(
            message, AVAILABLE_SENTENCE,
            "case {case:?} ({reader}): expected the provisioned link to report \
             {AVAILABLE_SENTENCE:?}, got {message:?}"
        );
    }
    for value in carries_nothing_of {
        assert!(
            !message.contains(value.as_str()),
            "case {case:?} ({reader}): expected the sentence for {status:?} to carry nothing \
             of the request or the record, but {message:?} carries {value:?}"
        );
    }
    match seen.get(&status) {
        Some(first) => assert_eq!(
            message, first.as_str(),
            "case {case:?} ({reader}): expected every link in {status:?} to report the one \
             sentence {first:?}, got {message:?}"
        ),
        None => {
            seen.insert(status.clone(), message.to_string());
        }
    }
    status
}

/// Assert the sentences observed tell the statuses APART, so a client reading the
/// description can tell a provisioned link from one still being provisioned.
fn assert_one_distinct_sentence_per_status(seen: &SentencePerStatus) {
    for (status, message) in seen {
        for (other, other_message) in seen {
            if other == status {
                continue;
            }
            assert_ne!(
                message, other_message,
                "expected {status:?} and {other:?} to be described by sentences of their own, \
                 got {message:?} for both"
            );
        }
    }
}

/// The item `GetVpcLinks` reports for `vpc_link_id`.
async fn listed_item(gateway: &Gateway, case: &str, vpc_link_id: &str) -> Value {
    let answer = gateway.call("GET", "/vpclinks", None).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from GetVpcLinks, got {} with {:?}",
        answer.status, answer.body
    );
    page_items(&answer.body, case)
        .into_iter()
        .find(|item| item["id"].as_str() == Some(vpc_link_id))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected GetVpcLinks to list {vpc_link_id}, got {:?}",
                answer.body
            )
        })
}

/// RULE: every projection of a VPC link describes the status it reports with the service's
/// sentence for that status — except the status of a link that could not be provisioned,
/// which describes that link's own failure.
#[tokio::test]
async fn every_reader_describes_the_status_it_reports_with_the_sentence_for_that_status() {
    let gateway = Gateway::new();

    // Every reading of every row goes in here, so the sentences are held to being one per
    // status ACROSS the rows and the readers, not only within a row.
    let mut seen = SentencePerStatus::new();
    for row in rows() {
        let case = row.case;
        let created = create_vpc_link(&gateway, row.request.clone()).await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: expected 202 from CreateVpcLink, got {} with {:?}",
            created.status, created.body
        );
        let id = string_member(&created.body, "id", case);
        // Nothing of the request, and nothing of the record, may be interpolated into a
        // fixed sentence — the minted id included.
        let mut forbidden = row.carries_nothing_of.clone();
        forbidden.push(id.clone());

        // A create answers while the link is still being provisioned, so the create's own
        // projection describes THAT status.
        let created_status = assert_pair(case, "CreateVpcLink", &created.body, &forbidden, &mut seen);
        assert_eq!(
            created_status, VPC_LINK_PENDING,
            "case {case:?}: expected CreateVpcLink to report status {VPC_LINK_PENDING}, got {:?}",
            created.body
        );

        // The provisioning is a transition the clock owes: the record is read again until
        // it reports the status it settled in, never waited out.
        let read = settled_link(&gateway, &id, case).await;
        let settled_status = assert_pair(case, "GetVpcLink", &read.body, &forbidden, &mut seen);
        assert_eq!(
            settled_status, row.settles,
            "case {case:?}: expected the link to settle {}, got {:?}",
            row.settles, read.body
        );

        // The collection reader and the updater project the settled link the same way.
        let item = listed_item(&gateway, case, &id).await;
        assert_pair(case, "GetVpcLinks", &item, &forbidden, &mut seen);
        assert_eq!(
            item["statusMessage"], read.body["statusMessage"],
            "case {case:?}: the listed link must describe its status as its own read does \
             ({:?}), got {:?}",
            read.body["statusMessage"], item["statusMessage"]
        );

        let patched = patch(
            &gateway,
            &format!("/vpclinks/{id}"),
            vec![replace("/description", "described by the patch")],
        )
        .await;
        assert_eq!(
            patched.status, 200,
            "case {case:?}: expected 200 from UpdateVpcLink, got {} with {:?}",
            patched.status, patched.body
        );
        assert_pair(case, "UpdateVpcLink", &patched.body, &forbidden, &mut seen);
    }

    // Both statuses whose description is fixed text were observed, and they are described
    // differently, so the description MOVED with the status rather than standing still.
    for status in [VPC_LINK_PENDING, VPC_LINK_AVAILABLE] {
        assert!(
            seen.contains_key(status),
            "expected the rows to observe {status:?}, got {seen:?}"
        );
    }
    assert_one_distinct_sentence_per_status(&seen);
}

/// RULE: the description is derived from the status on every read, so two links in one
/// status are described identically however they differ, and re-reading one link neither
/// changes nor re-words it.
#[tokio::test]
async fn two_links_in_one_status_are_described_identically_and_a_read_changes_nothing() {
    let gateway = Gateway::new();

    let mut seen = SentencePerStatus::new();
    let mut created_links: Vec<(&str, String)> = Vec::new();
    for row in rows() {
        let created = create_vpc_link(&gateway, row.request.clone()).await;
        assert_eq!(
            created.status, 202,
            "case {:?}: expected 202 from CreateVpcLink, got {} with {:?}",
            row.case, created.status, created.body
        );
        created_links.push((row.case, string_member(&created.body, "id", row.case)));
    }

    // Read every link to its settled status, then read them all AGAIN: a description the
    // reader derives rather than consumes is the same the second time round.
    for round in 0..2 {
        for (case, id) in &created_links {
            let read = settled_link(&gateway, id, case).await;
            let reader = format!("GetVpcLink(round {round})");
            assert_pair(case, &reader, &read.body, &[], &mut seen);
        }
    }

    assert!(
        seen.contains_key(VPC_LINK_AVAILABLE),
        "expected the rows to observe {VPC_LINK_AVAILABLE:?}, got {seen:?}"
    );
    assert_one_distinct_sentence_per_status(&seen);
}
