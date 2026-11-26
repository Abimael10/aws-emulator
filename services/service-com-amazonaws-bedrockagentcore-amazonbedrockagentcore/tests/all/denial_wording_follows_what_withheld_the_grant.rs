// SPDX-License-Identifier: CC0-1.0

//! WHICH sentence a refused memory-record call carries, driven end to end over the wire with
//! the services that hold the calling identity (STS) and the policies in force on it (IAM).
//!
//! The rule under test, one level above the recorded case (a principal whose only grant for
//! `ListMemoryRecords` is conditioned on one namespace calling for a sibling namespace):
//!
//! > A refusal is worded by WHAT withheld the grant. Where a document in force on the caller
//! > grants the action on the resource the request addressed and the only thing that refused
//! > the call is that grant's `Condition` over a key the request published
//! > (`bedrock-agentcore:namespace`, the member the model documents as the one "used for IAM
//! > condition key authorization"), the refusal names the PRINCIPAL and nothing further:
//! > `User: <principal> is not authorized to perform this operation`. Where no grant of the
//! > account's covers the call at all — no statement about the action and the resource, or an
//! > explicit `Deny` over them — the refusal is IAM's own sentence, naming the principal, the
//! > action, the resource and which of IAM's rules withheld the grant. Both are
//! > `AccessDeniedException` at HTTP 403, and nothing of the resource is reported either way.
//!
//! Why the two must be told apart: the sentence is what a client acts on. The long one says
//! the principal holds no grant for the action, so the remedy is to grant it; the short one
//! says it holds one and may not address this key's value under it, so the remedy is the
//! clause. A gate that answers the long sentence for a condition failure sends every
//! namespace-scoped caller to widen the wrong thing.
//!
//! The RESOURCE is part of the first half: the grant has to cover the container the request
//! addressed as well as the action, because a grant of the same action over ANOTHER container
//! withheld nothing on a condition — it does not cover the call at all — so such a refusal
//! keeps the long sentence however its clause reads, which the contrast rows drive both ways.
//!
//! The rows span what the rule generalises over: the OPERATION asked (every memory-record
//! operation that carries the `namespace` member — the listing, the search, the read and the
//! delete), the CLAUSE that withheld the grant (an equality on one namespace, an equality
//! listing several, a pattern on a prefix, a negated equality, and a request that carries no
//! value for the key at all), and — the contrast — the grants that withhold the call for no
//! condition at all (one scoped to another container, a conditioned one scoped to another
//! container asked both inside and outside its clause, no policy at all, and an explicit deny
//! beside a wildcard allow).
//!
//! Sources: `smithy_query(query="operation",operation="ListMemoryRecords,RetrieveMemoryRecords,
//! GetMemoryRecord,DeleteMemoryRecord")` for the HTTP bindings, the `namespace` member and
//! `AccessDeniedException` (httpError 403);
//! `smithy_query(query="doc",shape="GetMemoryRecordInput",member="namespace")` for what the
//! member is used for. The harness, the account credential and the two sentences' readings
//! live in [`super::workload_access_token_authorization`], which drives the same gate from
//! the decision side.

use serde_json::{json, Value};
use service_com_amazonaws_bedrockagentcore_amazonbedrockagentcore as agentcore;

use super::workload_access_token_authorization::{
    action, condition_refusal, error_code, message_of, refusal_with_reason, Harness, ACCOUNT_KEY,
    ACTION_PREFIX, DENIED_CODE, DENIED_STATUS, REGION,
};

/// The container every row addresses, and a second one the only unconditioned grant below is
/// scoped to. Both are inside `MemoryId` (len 12.., pattern
/// `^(arn:…:memory/)?[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$`).
const MEMORY: &str = "wording-memory-0123456789";
const OTHER_MEMORY: &str = "wording-other-9876543210";

/// Two sibling namespaces of that container and the parent hierarchy both sit under, inside
/// `Namespace` (len 1..1024).
const ALPHA: &str = "/t/tenant-9/actor/alpha/facts/";
const BETA: &str = "/t/tenant-9/actor/beta/facts/";
const ACTOR_PREFIX: &str = "/t/tenant-9/actor/";

/// The condition key this plane publishes the addressed namespace under.
const NAMESPACE_KEY: &str = "bedrock-agentcore:namespace";

/// A `memoryRecordId` no container holds: the gate decides before the service resolves what
/// the request named, so the refusal is owed whether or not the record exists. Inside
/// `MemoryRecordId` (len 40..50, pattern `^mem-[a-zA-Z0-9-_]*$`).
const UNHELD_RECORD: &str = "mem-0123456789abcdef0123456789abcdef01234567";

/// An identity-based policy with ONE Allow of `actions` on `resource`, conditioned on
/// `clauses` when the row carries any.
fn policy(actions: &[String], resource: &str, clauses: Option<Value>) -> String {
    let mut statement = json!({
        "Effect": "Allow",
        "Action": actions,
        "Resource": resource,
    });
    if let Some(clauses) = clauses {
        statement["Condition"] = clauses;
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string()
}

/// Every memory-record operation that carries the `namespace` member, as its own name and
/// the HTTP binding and payload it takes for `namespace` on [`MEMORY`].
///
/// The bindings are the model's: a listing is a `POST` of `/memories/{memoryId}/memoryRecords`,
/// a search a `POST` of `/memories/{memoryId}/retrieve`, a read a `GET` of
/// `/memories/{memoryId}/memoryRecord/{memoryRecordId}` and a delete a `DELETE` of
/// `/memories/{memoryId}/memoryRecords/{memoryRecordId}`, the last two carrying the member as
/// the query parameter the model binds it to.
fn calls(namespace: Option<&str>) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    let container = super::harness::label(MEMORY);
    let record = super::harness::label(UNHELD_RECORD);
    let query = namespace.map_or_else(String::new, |namespace| {
        format!("?namespace={}", super::harness::label(namespace))
    });
    let mut listing = json!({});
    let mut search = json!({ "searchCriteria": { "searchQuery": "anything" } });
    if let Some(namespace) = namespace {
        listing["namespace"] = json!(namespace);
        search["namespace"] = json!(namespace);
    }
    vec![
        (
            "ListMemoryRecords",
            "POST",
            format!("/memories/{container}/memoryRecords"),
            Some(listing),
        ),
        (
            "RetrieveMemoryRecords",
            "POST",
            format!("/memories/{container}/retrieve"),
            Some(search),
        ),
        (
            "GetMemoryRecord",
            "GET",
            format!("/memories/{container}/memoryRecord/{record}{query}"),
            None,
        ),
        (
            "DeleteMemoryRecord",
            "DELETE",
            format!("/memories/{container}/memoryRecords/{record}{query}"),
            None,
        ),
    ]
}

/// The actions every policy below grants: one per operation the rows drive, so the grant
/// always covers the action the call asks about and the clause is the only thing that can
/// withhold it.
fn granted_actions() -> Vec<String> {
    calls(Some(ALPHA))
        .into_iter()
        .map(|(operation, ..)| action(operation))
        .collect()
}

/// RULE: a call whose caller holds a grant for the action on the container it addressed, and
/// which only that grant's `Condition` over the published namespace key refused, is refused
/// with the sentence naming the principal alone — whichever operation asked, whichever clause
/// shape withheld it, and whether the request named a sibling namespace or none at all.
#[tokio::test]
async fn a_denial_a_condition_took_names_the_principal_alone() {
    let harness = Harness::new();
    harness.create_memory(MEMORY);
    let memory_arn = agentcore::state::memory_arn(REGION, MEMORY);

    // (case, role, the clause of the role's only Allow, the namespace the refused request
    // addresses)
    let rows: Vec<(&str, &str, Value, Option<&str>)> = vec![
        (
            "an equality on one namespace, asked for its sibling",
            "wording-equals",
            json!({ "StringEquals": { NAMESPACE_KEY: ALPHA } }),
            Some(BETA),
        ),
        (
            "an equality listing both siblings, asked for a third namespace",
            "wording-either",
            json!({ "StringEquals": { NAMESPACE_KEY: [ALPHA, BETA] } }),
            Some("/t/tenant-9/actor/gamma/facts/"),
        ),
        (
            "a pattern on one actor's prefix, asked outside it",
            "wording-like",
            json!({ "StringLike": { NAMESPACE_KEY: format!("{ALPHA}*") } }),
            Some(BETA),
        ),
        (
            "a negated equality, asked for the namespace it excludes",
            "wording-not-equals",
            json!({ "StringNotEquals": { NAMESPACE_KEY: BETA } }),
            Some(BETA),
        ),
        (
            // The member is absent, so the call carries no value for the key and an equality
            // clause does not hold — the grant still covers the action and the container.
            "an equality on one namespace, asked with no namespace at all",
            "wording-absent",
            json!({ "StringEquals": { NAMESPACE_KEY: ALPHA } }),
            None,
        ),
    ];

    for (row, role, clauses, addressed) in rows {
        let document = policy(&granted_actions(), &memory_arn, Some(clauses));
        let (key, principal) = harness.assumed_role(role, Some(&document)).await;
        for (operation, method, path, body) in calls(addressed) {
            let case = format!("{row:?}/{operation}");
            let (status, answered) = harness.call(&key, method, &path, body).await;
            assert_eq!(
                status, DENIED_STATUS,
                "case {case}: expected HTTP {DENIED_STATUS}, got {status} with {answered}"
            );
            assert_eq!(
                error_code(&answered),
                DENIED_CODE,
                "case {case}: expected {DENIED_CODE}, got {answered}"
            );
            let expected = condition_refusal(&principal);
            let message = message_of(&answered);
            assert_eq!(
                message, expected,
                "case {case}: expected {expected:?}, got {message:?}"
            );
            // The sentence closes where it closes: a condition failure reports neither the
            // action it asked for nor the container it addressed.
            for withheld in [action(operation), memory_arn.clone()] {
                assert!(
                    !message.contains(&withheld),
                    "case {case}: the condition-failure sentence must withhold {withheld:?}, \
                     got {message:?}"
                );
            }
            for member in ["memoryRecord", "memoryRecordId", "memoryRecordSummaries"] {
                assert!(
                    answered.get(member).is_none(),
                    "case {case}: a refused call must report no {member}, got {answered}"
                );
            }
        }
    }
}

/// The CONTRAST, and the bound of the rule: a call no grant of the account's covers keeps
/// IAM's own sentence — the principal, the action, the container the request addressed and
/// the rule that withheld the grant — so the two denials stay tellable apart.
#[tokio::test]
async fn a_denial_no_grant_covered_keeps_iams_own_sentence() {
    let harness = Harness::new();
    harness.create_memory(MEMORY);
    harness.create_memory(OTHER_MEMORY);
    let memory_arn = agentcore::state::memory_arn(REGION, MEMORY);
    let other_arn = agentcore::state::memory_arn(REGION, OTHER_MEMORY);

    // (case, role, its only policy, whether a readable document EXPLICITLY denies the call)
    let rows: Vec<(&str, &str, Option<String>, bool)> = vec![
        (
            "a grant scoped to another container, which states nothing about this one",
            "wording-other-container",
            Some(policy(&granted_actions(), &other_arn, None)),
            false,
        ),
        (
            // The clause the request satisfies, over a container the grant does not name:
            // the condition held, so what withheld the grant is the resource element.
            "a conditioned grant on another container, asked for the namespace it allows",
            "wording-other-conditioned",
            Some(policy(
                &granted_actions(),
                &other_arn,
                Some(json!({ "StringEquals": { NAMESPACE_KEY: ALPHA } })),
            )),
            false,
        ),
        (
            // The same grant asked for a namespace the clause does NOT cover: both the
            // resource element and the clause would refuse, and the resource is what the
            // refusal is about — the caller holds no grant over this container at all.
            "a conditioned grant on another container, asked outside its namespace too",
            "wording-other-conditioned-outside",
            Some(policy(
                &granted_actions(),
                &other_arn,
                Some(json!({ "StringEquals": { NAMESPACE_KEY: BETA } })),
            )),
            false,
        ),
        (
            "no policy at all, which is IAM's default deny",
            "wording-no-policy",
            None,
            false,
        ),
        (
            "an explicit deny of this container beside a wildcard allow",
            "wording-explicit-deny",
            Some(
                json!({
                    "Version": "2012-10-17",
                    "Statement": [
                        { "Effect": "Allow", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
                        {
                            "Effect": "Deny",
                            "Action": format!("{ACTION_PREFIX}:*"),
                            "Resource": memory_arn.clone(),
                        },
                    ],
                })
                .to_string(),
            ),
            true,
        ),
    ];

    for (row, role, document, explicitly_denied) in rows {
        let (key, principal) = harness.assumed_role(role, document.as_deref()).await;
        for (operation, method, path, body) in calls(Some(ALPHA)) {
            let case = format!("{row:?}/{operation}");
            let (status, answered) = harness.call(&key, method, &path, body).await;
            assert_eq!(
                status, DENIED_STATUS,
                "case {case}: expected HTTP {DENIED_STATUS}, got {status} with {answered}"
            );
            let reason = if explicitly_denied {
                "with an explicit deny in an identity-based policy".to_string()
            } else {
                format!(
                    "because no identity-based policy allows the {} action",
                    action(operation)
                )
            };
            let expected = refusal_with_reason(&principal, operation, &memory_arn, &reason);
            let message = message_of(&answered);
            assert_eq!(
                message, expected,
                "case {case}: expected {expected:?}, got {message:?}"
            );
        }
    }
}

/// The grant the rows above are refused under SERVES the namespace its clause names, for
/// every operation that publishes the key: the refusals are one clause being enforced rather
/// than the plane being closed, and the sentence is only reached by a call the clause refused.
#[tokio::test]
async fn the_conditioned_grant_serves_the_namespace_its_clause_names() {
    let harness = Harness::new();
    harness.create_memory(MEMORY);
    let memory_arn = agentcore::state::memory_arn(REGION, MEMORY);
    // One record under each sibling namespace, written as the account's own credential, which
    // holds no IAM policies — so the served calls below have something to report.
    let (status, created) = harness
        .call(
            ACCOUNT_KEY,
            "POST",
            &format!(
                "/memories/{}/memoryRecords/batchCreate",
                super::harness::label(MEMORY)
            ),
            Some(json!({"records": [
                {
                    "requestIdentifier": "alpha-1",
                    "namespaces": [ALPHA],
                    "content": { "text": "alpha-fact" },
                    "timestamp": 1_700_000_000,
                },
                {
                    "requestIdentifier": "beta-1",
                    "namespaces": [BETA],
                    "content": { "text": "beta-fact" },
                    "timestamp": 1_700_000_001,
                },
            ]})),
        )
        .await;
    assert_eq!(
        status, 201,
        "setup: seeding the two sibling records must succeed, got {status} with {created}"
    );
    let alpha_record = created["successfulRecords"]
        .as_array()
        .and_then(|records| {
            records
                .iter()
                .find(|record| record["requestIdentifier"].as_str() == Some("alpha-1"))
        })
        .and_then(|record| record["memoryRecordId"].as_str())
        .unwrap_or_else(|| panic!("setup: expected the seeded alpha record, got {created}"))
        .to_string();

    // A clause over the parent hierarchy, so both the listing of one namespace under it and
    // the read of a record in it are served.
    let document = policy(
        &granted_actions(),
        &memory_arn,
        Some(json!({ "StringLike": { NAMESPACE_KEY: format!("{ACTOR_PREFIX}*") } })),
    );
    let (key, _) = harness.assumed_role("wording-served", Some(&document)).await;

    for (case, method, path, body) in [
        (
            "the listing of a namespace the clause covers",
            "POST",
            format!(
                "/memories/{}/memoryRecords",
                super::harness::label(MEMORY)
            ),
            Some(json!({ "namespace": ALPHA })),
        ),
        (
            "the search of a namespace the clause covers",
            "POST",
            format!("/memories/{}/retrieve", super::harness::label(MEMORY)),
            Some(json!({
                "namespace": ALPHA,
                "searchCriteria": { "searchQuery": "alpha-fact" },
            })),
        ),
        (
            "the read of a record in a namespace the clause covers",
            "GET",
            format!(
                "/memories/{}/memoryRecord/{}?namespace={}",
                super::harness::label(MEMORY),
                super::harness::label(&alpha_record),
                super::harness::label(ALPHA)
            ),
            None,
        ),
    ] {
        let (status, answered) = harness.call(&key, method, &path, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {answered}"
        );
    }
}
