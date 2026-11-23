// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the per-item `ErrorCode` a BATCH READ answers an unresolvable id
//! under, driven over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`)
//! through the same router the emulator boots.
//!
//! THE RULE UNDER TEST: the per-item `ErrorCode` of a partial-success batch response is the
//! code AWS answers ON THE WIRE for that resource kind's not-found class, and it is the only
//! machine-readable discriminator such an entry carries — so each code is asserted here as
//! its literal text rather than through this service's own constants, which is what a client
//! matching AWS's code sees:
//!
//! * `BatchGetNamedQuery` → `NAMED_QUERY_NOT_FOUND`;
//! * `BatchGetQueryExecution` → `QUERY_EXECUTION_NOT_FOUND`;
//! * `BatchGetPreparedStatement` → `STATEMENTNAME_NOT_FOUND`, which is not the
//!   `STATEMENT_NOT_FOUND` spelling the member's prose lists among the MESSAGES it may
//!   carry.
//!
//! The rows differ along the form and the size of the id that resolves to nothing, inside the
//! constraints the model declares on it, and every row asks for a HELD id beside the miss, so
//! the answer is a partition rather than one class for everything.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, create_work_group, router, start};

/// Where the query this suite starts writes its results: what matters is that the query HAS a
/// location, so the start is accepted and really mints an id.
const OUTPUT_LOCATION: &str = "s3://athena-results/batch-read-wire-codes/";

/// The workgroup the prepared statement of this suite belongs to: a prepared statement is
/// addressed within one workgroup.
const WORK_GROUP: &str = "batch-read-wire-codes";

/// One batch read of this suite: the operation, its members, the literal code an unresolvable
/// id must come back under, the id this account holds, and the ids that resolve to nothing.
struct BatchRead {
    kind: &'static str,
    operation: &'static str,
    ids_member: &'static str,
    unprocessed_member: &'static str,
    id_member: &'static str,
    /// Any other required input member (the workgroup a prepared statement is addressed in).
    extra: Value,
    /// The literal `ErrorCode` AWS answers for a miss of this kind.
    wire_code: &'static str,
    held: String,
    misses: Vec<(&'static str, String)>,
}

impl BatchRead {
    /// Ask for `ids` and hand back `(status, the unprocessed entries)`.
    async fn read(&self, router: &MultiServiceRouter, ids: &[String]) -> (u16, Vec<Value>) {
        let mut request = Map::new();
        request.insert(self.ids_member.to_string(), json!(ids));
        for (member, value) in self.extra.as_object().into_iter().flatten() {
            request.insert(member.clone(), value.clone());
        }
        let (status, body) = call(router, self.operation, &Value::Object(request)).await;
        let unprocessed = body[self.unprocessed_member]
            .as_array()
            .cloned()
            .unwrap_or_default();
        (status, unprocessed)
    }
}

/// `held` with its last character replaced: an id of exactly the form this service holds,
/// which nothing answers to. Built FROM state rather than written down.
fn flip_last(held: &str) -> String {
    let mut near_miss = held.to_string();
    let last = near_miss.pop().expect("a held id is not empty");
    near_miss.push(if last == 'z' { 'y' } else { 'z' });
    near_miss
}

/// The ids that resolve to nothing for a `NamedQueryId`/`QueryExecutionId` (1 to 128
/// characters matching `^\S+$`, read with
/// `smithy_query(query="shape",shape="NamedQueryId,QueryExecutionId")`).
fn id_misses(held: &str) -> Vec<(&'static str, String)> {
    vec![
        ("the shortest id the model admits", "x".to_string()),
        ("the longest id the model admits", "x".repeat(128)),
        (
            "a hyphenated UUID nothing was minted under",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
        ),
        ("a held id with its last character changed", flip_last(held)),
    ]
}

/// The names that resolve to nothing for a prepared statement (1 to 256 characters matching
/// `^[a-zA-Z_][a-zA-Z0-9_@:]{1,256}$`, read with
/// `smithy_query(query="shape",shape="StatementName")`).
fn name_misses(held: &str) -> Vec<(&'static str, String)> {
    vec![
        ("the shortest name the pattern admits", "ab".to_string()),
        (
            "the longest name the model admits",
            format!("a{}", "b".repeat(255)),
        ),
        (
            "a name carrying the punctuation the pattern admits",
            "unheld_statement@v1:2".to_string(),
        ),
        ("a held name with its last character changed", flip_last(held)),
    ]
}

/// Every batch read this service serves, each beside one resource this account really holds.
async fn batch_reads(router: &MultiServiceRouter) -> Vec<BatchRead> {
    create_work_group(router, WORK_GROUP).await;

    let (status, saved) = call(
        router,
        "CreateNamedQuery",
        &json!({
            "Name": "held_by_the_wire_code_suite",
            "Database": "logs",
            "QueryString": "SELECT count(*) FROM requests",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the named query must be created, got {status} with {saved:?}"
    );
    let named_query_id = saved["NamedQueryId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateNamedQuery must mint an id, got {saved:?}"))
        .to_string();

    let execution_id = start(
        router,
        &json!({
            "QueryString": "SELECT 1 AS n",
            "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
        }),
    )
    .await;

    let statement_name = "held_statement".to_string();
    let (status, created) = call(
        router,
        "CreatePreparedStatement",
        &json!({
            "StatementName": statement_name,
            "WorkGroup": WORK_GROUP,
            "QueryStatement": "SELECT * FROM requests WHERE status = ?",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the prepared statement must be created, got {status} with {created:?}"
    );

    vec![
        BatchRead {
            kind: "a saved query",
            operation: "BatchGetNamedQuery",
            ids_member: "NamedQueryIds",
            unprocessed_member: "UnprocessedNamedQueryIds",
            id_member: "NamedQueryId",
            extra: json!({}),
            wire_code: "NAMED_QUERY_NOT_FOUND",
            misses: id_misses(&named_query_id),
            held: named_query_id,
        },
        BatchRead {
            kind: "a query execution",
            operation: "BatchGetQueryExecution",
            ids_member: "QueryExecutionIds",
            unprocessed_member: "UnprocessedQueryExecutionIds",
            id_member: "QueryExecutionId",
            extra: json!({}),
            wire_code: "QUERY_EXECUTION_NOT_FOUND",
            misses: id_misses(&execution_id),
            held: execution_id,
        },
        BatchRead {
            kind: "a prepared statement",
            operation: "BatchGetPreparedStatement",
            ids_member: "PreparedStatementNames",
            unprocessed_member: "UnprocessedPreparedStatementNames",
            id_member: "StatementName",
            extra: json!({ "WorkGroup": WORK_GROUP }),
            wire_code: "STATEMENTNAME_NOT_FOUND",
            misses: name_misses(&statement_name),
            held: statement_name,
        },
    ]
}

/// An id a batch read resolves nothing under comes back under the literal `ErrorCode` AWS
/// answers for that resource kind, carrying a reason and the id asked for, while the resource
/// asked for beside it is still answered.
#[tokio::test]
async fn an_unresolvable_id_carries_the_wire_code_of_its_resource_kind() {
    let router = router();
    let reads = batch_reads(&router).await;

    for read in &reads {
        for (case, id) in &read.misses {
            let case = format!("{} / {case}", read.kind);
            let (status, unprocessed) =
                read.read(&router, &[read.held.clone(), id.clone()]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: an unresolvable id is reported, not refused, got {status}"
            );
            assert_eq!(
                unprocessed.len(),
                1,
                "case {case:?}: expected the one unresolvable id to be reported, got \
                 {unprocessed:?}"
            );
            let entry = &unprocessed[0];
            assert_eq!(
                entry[read.id_member].as_str(),
                Some(id.as_str()),
                "case {case:?}: the entry must name the id asked for, got {entry:?}"
            );
            assert_eq!(
                entry["ErrorCode"].as_str(),
                Some(read.wire_code),
                "case {case:?}: expected ErrorCode {:?}, got {entry:?}",
                read.wire_code
            );
            let reason = entry["ErrorMessage"].as_str().unwrap_or_default();
            assert!(
                !reason.is_empty(),
                "case {case:?}: the entry must state why the id could not be resolved, got \
                 {entry:?}"
            );
        }
    }

    // The three codes are three classes: a client branching on them tells one missing
    // resource kind from another, and none of them is the class of a bad request.
    let codes: Vec<&str> = reads.iter().map(|read| read.wire_code).collect();
    for code in &codes {
        assert_ne!(
            *code, "INVALID_INPUT",
            "a resource that is simply absent must not be reported as a bad request"
        );
    }
    assert_eq!(
        codes.len(),
        codes
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        "each batch read must report its own class, got {codes:?}"
    );
}
