// SPDX-License-Identifier: CC0-1.0

//! What a memory-plane refusal of this service SAYS, driven end to end over the wire with
//! the services that hold the calling identity (STS) and the policies in force on it (IAM).
//!
//! The rule these cases assert, one level above the recorded case (a principal granted the
//! read of ONE memory container calling for a second container):
//!
//! > A memory-record call IAM does not permit is refused `AccessDeniedException` (HTTP 403)
//! > with IAM's own authorization sentence, whose variable parts come from the REQUEST and
//! > the caller's credential: the principal ARN the credential speaks for, the action
//! > `bedrock-agentcore:<OperationName>` of the operation being called, the memory-container
//! > ARN the request's own `memoryId` addresses — whichever spelling `MemoryId` admits it
//! > used, and whether or not the account holds such a container — and the clause naming
//! > which of IAM's rules withheld the grant. The container named is the one the REQUEST
//! > targeted, never the one the caller's grant happens to name.
//!
//! The rows span what the rule generalises over: every memory-plane operation that takes a
//! `memoryId` (`GetMemoryRecord`, `DeleteMemoryRecord`, `ListMemoryRecords`,
//! `RetrieveMemoryRecords`, `BatchCreateMemoryRecords`), the container the request targets
//! (a second container the account holds, and one it does not hold at all), the two
//! spellings `MemoryId` admits for it (the bare id and the full memory ARN), and which rule
//! withheld the grant (IAM's default deny, and an explicit `Deny` beside a wildcard allow).
//!
//! The harness, the account credential and the refusal's reading live in
//! [`super::workload_access_token_authorization`], which drives the same gate from the
//! namespace-condition side.

use serde_json::{json, Value};
use service_com_amazonaws_bedrockagentcore_amazonbedrockagentcore as agentcore;

use super::workload_access_token_authorization::{
    action, error_code, message_of, refusal_with_reason, Harness, ACCOUNT_KEY, ACTION_PREFIX,
    DENIED_CODE, DENIED_STATUS, REGION,
};

/// The container the only grant in these cases is scoped to, a second container the account
/// also holds, and a third the account holds no container for at all. Every one is inside
/// `MemoryId` (len 12.., pattern
/// `^(arn:(aws|aws-cn|aws-us-gov):bedrock-agentcore:[a-z0-9-]+:[0-9]{12}:memory/)?[a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$`).
const GRANTED_MEMORY: &str = "tenant-a-memory-0123456789";
const OTHER_MEMORY: &str = "tenant-b-memory-9876543210";
const UNHELD_MEMORY: &str = "tenant-c-memory-abcdef0123";

/// The namespace the records of these cases sit under, inside `Namespace` (len 1..1024,
/// pattern `^[a-zA-Z0-9/*][a-zA-Z0-9-_/*]*(?::[a-zA-Z0-9-_/*]+)*[a-zA-Z0-9-_/*]*$`).
const NAMESPACE: &str = "/t/tenant/actor/probe/facts/";

/// A `memoryRecordId` no container of these cases holds: the gate decides ahead of the
/// service resolving anything the request named, so a refusal is owed whether or not the
/// record exists. Inside `MemoryRecordId` (len 40..50, pattern `^mem-[a-zA-Z0-9-_]*$`).
const UNHELD_RECORD: &str = "mem-0123456789abcdef0123456789abcdef01234567";

/// The memory-container ARN a `memoryId` addresses — the resource an IAM statement scoped to
/// one container names, and the one a refusal of a call about it names.
fn memory_arn(memory_id: &str) -> String {
    agentcore::state::memory_arn(REGION, memory_id)
}

/// An identity-based policy carrying `statements`.
fn document(statements: Value) -> String {
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// One memory-plane call: the operation's own name, and the HTTP binding and payload it
/// takes for the container `memory_id` names.
///
/// The five bindings are the model's (`smithy_query query="operation"`): a read is a `GET` of
/// `/memories/{memoryId}/memoryRecord/{memoryRecordId}`, a delete a `DELETE` of
/// `/memories/{memoryId}/memoryRecords/{memoryRecordId}`, a listing a `POST` of
/// `/memories/{memoryId}/memoryRecords`, a search a `POST` of `/memories/{memoryId}/retrieve`
/// and a write a `POST` of `/memories/{memoryId}/memoryRecords/batchCreate`.
fn calls_of(memory_id: &str) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    let container = super::harness::label(memory_id);
    vec![
        (
            "GetMemoryRecord",
            "GET",
            format!(
                "/memories/{container}/memoryRecord/{}?namespace={}",
                super::harness::label(UNHELD_RECORD),
                super::harness::label(NAMESPACE)
            ),
            None,
        ),
        (
            "DeleteMemoryRecord",
            "DELETE",
            format!(
                "/memories/{container}/memoryRecords/{}?namespace={}",
                super::harness::label(UNHELD_RECORD),
                super::harness::label(NAMESPACE)
            ),
            None,
        ),
        (
            "ListMemoryRecords",
            "POST",
            format!("/memories/{container}/memoryRecords"),
            Some(json!({ "namespace": NAMESPACE })),
        ),
        (
            "RetrieveMemoryRecords",
            "POST",
            format!("/memories/{container}/retrieve"),
            Some(json!({
                "namespace": NAMESPACE,
                "searchCriteria": { "searchQuery": "anything" },
            })),
        ),
        (
            "BatchCreateMemoryRecords",
            "POST",
            format!("/memories/{container}/memoryRecords/batchCreate"),
            Some(json!({ "records": [{
                "requestIdentifier": "probe-1",
                "namespaces": [NAMESPACE],
                "content": { "text": "a record a refused write must not store" },
                "timestamp": 1_700_000_000,
            }]})),
        ),
    ]
}

/// One record of the container the grant names, written as the account's own credential
/// (which holds no IAM policies), answering its `memoryRecordId`.
async fn seed_record(harness: &Harness, memory_id: &str) -> String {
    let (status, created) = harness
        .call(
            ACCOUNT_KEY,
            "POST",
            &format!(
                "/memories/{}/memoryRecords/batchCreate",
                super::harness::label(memory_id)
            ),
            Some(json!({"records": [{
                "requestIdentifier": "seed-1",
                "namespaces": [NAMESPACE],
                "content": { "text": "a fact of the granted container" },
                "timestamp": 1_700_000_000,
            }]})),
        )
        .await;
    assert_eq!(
        status, 201,
        "setup: seeding a record of {memory_id:?} must succeed, got {status} with {created}"
    );
    created["successfulRecords"][0]["memoryRecordId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: the seeded record must carry an id, got {created}"))
        .to_string()
}

/// How many records of `memory_id` the account's own credential can see.
async fn record_count(harness: &Harness, memory_id: &str) -> usize {
    let (status, listed) = harness
        .call(
            ACCOUNT_KEY,
            "POST",
            &format!(
                "/memories/{}/memoryRecords",
                super::harness::label(memory_id)
            ),
            Some(json!({ "namespace": NAMESPACE })),
        )
        .await;
    assert_eq!(
        status, 200,
        "setup: listing the records of {memory_id:?} must succeed, got {status} with {listed}"
    );
    listed["memoryRecordSummaries"]
        .as_array()
        .map_or(0, Vec::len)
}

/// Every refusal of this plane names the action the call asked for and the container the
/// REQUEST addressed, whichever spelling it used and whether or not the account holds one.
#[tokio::test]
async fn a_memory_plane_denial_names_the_container_the_request_targeted() {
    let harness = Harness::new();
    harness.create_memory(GRANTED_MEMORY);
    harness.create_memory(OTHER_MEMORY);
    // One record of the GRANTED container, so the closing count tells an untouched container
    // apart from one a refused write reached.
    seed_record(&harness, GRANTED_MEMORY).await;

    // (case, the `memoryId` the request carries, the ARN it addresses)
    let targets: Vec<(&str, String, String)> = vec![
        (
            "a second container the account holds, by its bare id",
            OTHER_MEMORY.to_string(),
            memory_arn(OTHER_MEMORY),
        ),
        (
            "that same container, by the memory ARN the shape also admits",
            memory_arn(OTHER_MEMORY),
            memory_arn(OTHER_MEMORY),
        ),
        (
            "a container the account does not hold at all",
            UNHELD_MEMORY.to_string(),
            memory_arn(UNHELD_MEMORY),
        ),
        (
            "that unheld container, by the memory ARN the shape also admits",
            memory_arn(UNHELD_MEMORY),
            memory_arn(UNHELD_MEMORY),
        ),
    ];

    // (case, role, its only policy, whether a document of its own EXPLICITLY denies the
    // call — the rule the refusal then names, as against IAM's default deny)
    let rows: Vec<(&str, &str, String, bool)> = vec![
        (
            "a grant scoped to one other container, which states nothing about these",
            "scoped-to-one-memory",
            document(json!([{
                "Effect": "Allow",
                "Action": format!("{ACTION_PREFIX}:*"),
                "Resource": memory_arn(GRANTED_MEMORY),
            }])),
            false,
        ),
        (
            "no policy at all, which is IAM's default deny",
            "no-policy-at-all",
            String::new(),
            false,
        ),
        (
            "a wildcard allow with an explicit deny of these containers beside it",
            "explicit-deny",
            document(json!([
                {
                    "Effect": "Allow",
                    "Action": format!("{ACTION_PREFIX}:*"),
                    "Resource": "*",
                },
                {
                    "Effect": "Deny",
                    "Action": format!("{ACTION_PREFIX}:*"),
                    "Resource": [memory_arn(OTHER_MEMORY), memory_arn(UNHELD_MEMORY)],
                },
            ])),
            true,
        ),
    ];

    for (row, role, policy, explicitly_denied) in rows {
        let policy = (!policy.is_empty()).then_some(policy);
        let (key, principal) = harness.assumed_role(role, policy.as_deref()).await;
        for (target, memory_id, resource) in &targets {
            for (operation, method, path, body) in calls_of(memory_id) {
                let case = format!("{row:?}/{target:?}/{operation}");
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
                let reason = if explicitly_denied {
                    "with an explicit deny in an identity-based policy".to_string()
                } else {
                    format!(
                        "because no identity-based policy allows the {} action",
                        action(operation)
                    )
                };
                let expected = refusal_with_reason(&principal, operation, resource, &reason);
                let message = message_of(&answered);
                assert_eq!(
                    message, expected,
                    "case {case}: expected {expected:?}, got {message:?}"
                );
                for member in [
                    "memoryRecord",
                    "memoryRecordId",
                    "memoryRecordSummaries",
                    "successfulRecords",
                ] {
                    assert!(
                        answered.get(member).is_none(),
                        "case {case}: a refused call must report no {member}, got {answered}"
                    );
                }
            }
        }
    }

    // A refused write stored nothing: the container the refused calls addressed holds no
    // record, and the one the grant names still holds exactly the record seeded into it.
    for (memory_id, expected) in [(OTHER_MEMORY, 0usize), (GRANTED_MEMORY, 1)] {
        let held = record_count(&harness, memory_id).await;
        assert_eq!(
            held, expected,
            "case {memory_id:?}: expected {expected} record(s) after the refused writes, got \
             {held}"
        );
    }
}

/// The same grant SERVES the container it names, addressed by either spelling: the decision
/// is per container, so the refusals above are the grant being enforced rather than the
/// whole plane being closed.
#[tokio::test]
async fn the_grant_serves_the_container_it_names_under_either_spelling() {
    let harness = Harness::new();
    harness.create_memory(GRANTED_MEMORY);
    let record = seed_record(&harness, GRANTED_MEMORY).await;
    let policy = document(json!([{
        "Effect": "Allow",
        "Action": format!("{ACTION_PREFIX}:*"),
        "Resource": memory_arn(GRANTED_MEMORY),
    }]));
    let (key, _) = harness
        .assumed_role("scoped-grant-served", Some(&policy))
        .await;

    for (case, memory_id) in [
        ("the bare id", GRANTED_MEMORY.to_string()),
        ("the memory ARN", memory_arn(GRANTED_MEMORY)),
    ] {
        let (status, answered) = harness
            .call(
                &key,
                "GET",
                &format!(
                    "/memories/{}/memoryRecord/{}?namespace={}",
                    super::harness::label(&memory_id),
                    super::harness::label(&record),
                    super::harness::label(NAMESPACE)
                ),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {answered}"
        );
        let read = answered["memoryRecord"]["memoryRecordId"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(
            read, record,
            "case {case:?}: expected the seeded record {record:?}, got {read:?}"
        );
    }
}
