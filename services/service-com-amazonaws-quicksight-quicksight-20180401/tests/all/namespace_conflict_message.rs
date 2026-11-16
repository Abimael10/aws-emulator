// SPDX-License-Identifier: CC0-1.0

//! The message `CreateNamespace` refuses an already-held name with, end to end over
//! HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` with a signed
//! restJson1 request — so what is asserted is what a QuickSight SDK client would see:
//! the status, the error code, and the message text the handler rendered from state.
//!
//! The rule under test: a create addressing a namespace name the account already holds
//! is refused with `ConflictException`, whose message states the `NamespaceStatus` that
//! namespace reports as of the call and then names the requested namespace and the
//! request's account. So the rows below vary what the rule generalises over — the status
//! the already-held namespace is in (every status a namespace of this emulator reaches),
//! whether it was minted by a create or is the one every account has, the name's form and
//! length, and the account — rather than re-running one recorded call. The status word is
//! never a constant: each row also reads the namespace back through `DescribeNamespace`
//! and requires the message to carry exactly the status that read reports.

use serde_json::{json, Value};

use super::data_source_data_set_lifecycle::Harness;
use super::delete_namespace::delete_namespace;
use super::namespace_lifecycle::{create_namespace_in, describe_namespace_in, settle_creation};

/// The account most rows use.
const ACCOUNT: &str = "000000000000";
/// A second, unrelated 12-digit account id, for the rows about account keying.
const OTHER_ACCOUNT: &str = "210987654321";
/// The region the test client signs for.
const REGION: &str = "us-west-2";

/// The namespace every account already has, which no create minted.
const DEFAULT_NAMESPACE: &str = "default";

/// How the namespace holding the name got into the status the refused create reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    /// A create put it there and the transition the clock owes is not due yet.
    StillCreating,
    /// A create put it there and the transition the clock owes has fallen due.
    Created,
    /// A delete asked for its removal, which the clock has not made due yet.
    BeingDeleted,
    /// Nothing created it: it is the namespace every account has.
    AlwaysThere,
}

/// One row of the conflict-message table.
struct Row {
    /// The case label every assertion names.
    case: &'static str,
    account: &'static str,
    /// The `Namespace` both creates name; the model admits len 0..64 over the pattern
    /// `^[a-zA-Z0-9._-]*$`.
    /// Source: smithy_query(query="operation", operation="CreateNamespace").
    namespace: String,
    held: Held,
    /// The `NamespaceStatus` the held namespace reports when the second create runs
    /// (model enum `com.amazonaws.quicksight#NamespaceStatus`: CREATED | CREATING |
    /// DELETING | NON_RETRYABLE_FAILURE | RETRYABLE_FAILURE — this emulator fails no
    /// namespace, so the first three are the ones a namespace here reaches).
    /// Source: smithy_query(query="shape", shape="NamespaceStatus").
    expected_status: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "created-plain-name",
            account: ACCOUNT,
            namespace: "sales".to_string(),
            held: Held::Created,
            expected_status: "CREATED",
        },
        Row {
            case: "still-creating-punctuated-name",
            account: ACCOUNT,
            namespace: "finance.eu-west_2".to_string(),
            held: Held::StillCreating,
            expected_status: "CREATING",
        },
        Row {
            case: "being-deleted-single-character-name",
            account: ACCOUNT,
            namespace: "n".to_string(),
            held: Held::BeingDeleted,
            expected_status: "DELETING",
        },
        Row {
            // The longest name the model admits: `Namespace` is len 0..64.
            case: "created-longest-name",
            account: ACCOUNT,
            namespace: "n".repeat(64),
            held: Held::Created,
            expected_status: "CREATED",
        },
        Row {
            case: "created-other-account",
            account: OTHER_ACCOUNT,
            namespace: "sales".to_string(),
            held: Held::Created,
            expected_status: "CREATED",
        },
        Row {
            case: "default-namespace",
            account: ACCOUNT,
            namespace: DEFAULT_NAMESPACE.to_string(),
            held: Held::AlwaysThere,
            expected_status: "CREATED",
        },
        Row {
            case: "default-namespace-other-account",
            account: OTHER_ACCOUNT,
            namespace: DEFAULT_NAMESPACE.to_string(),
            held: Held::AlwaysThere,
            expected_status: "CREATED",
        },
    ]
}

/// The body a create of `namespace` submits.
fn create_body(namespace: &str) -> Value {
    json!({ "Namespace": namespace, "IdentityStore": "QUICKSIGHT" })
}

/// `DeleteNamespace` is driven through the sibling delete suite's helper, so this suite
/// shapes no second copy of that request.
async fn hold_the_name(harness: &Harness, row: &Row) {
    let case = row.case;
    let namespace = row.namespace.as_str();
    if row.held == Held::AlwaysThere {
        // Nothing to arrange: the account has this namespace without any create.
        return;
    }

    let (status, body) =
        create_namespace_in(harness, REGION, row.account, create_body(namespace)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the first create expected 200, got {status} with {body}"
    );

    match row.held {
        // A create reports the non-terminal status, so the name is already held while
        // the transition is still owed.
        Held::StillCreating => {}
        Held::Created => settle_creation(harness, row.account, namespace),
        Held::BeingDeleted => {
            settle_creation(harness, row.account, namespace);
            let (status, body) = delete_namespace(harness, row.account, namespace).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the delete expected 200, got {status} with {body}"
            );
        }
        Held::AlwaysThere => unreachable!("handled above"),
    }
}

#[tokio::test]
async fn create_namespace_conflict_names_the_status_the_namespace_and_the_account() {
    let harness = Harness::new();

    for row in rows() {
        let case = row.case;
        let namespace = row.namespace.as_str();
        let account = row.account;
        hold_the_name(&harness, &row).await;

        // The status the refusal must report is the one the read of that namespace
        // reports: the message is rendered from state, not from a fixed word.
        let (status, body) = describe_namespace_in(&harness, REGION, account, namespace).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the held namespace must be describable, got {status} with {body}"
        );
        assert_eq!(
            body["Namespace"]["CreationStatus"],
            json!(row.expected_status),
            "case {case:?}: expected the held namespace in status {:?}, got {body}",
            row.expected_status
        );

        let (status, body) =
            create_namespace_in(&harness, REGION, account, create_body(namespace)).await;
        assert_eq!(
            status, 409,
            "case {case:?}: re-creating a held namespace expected 409, got {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ConflictException"),
            "case {case:?}: a name already in use is a conflict, got {body}"
        );
        let expected = format!(
            "Cannot create a namespace when status is {}. Failed to create namespace \
             {namespace} for account {account}. ",
            row.expected_status
        );
        assert_eq!(
            body["Message"],
            json!(expected),
            "case {case:?}: expected message {expected:?}, got {:?}",
            body["Message"]
        );
    }
}

#[tokio::test]
async fn the_conflict_message_varies_with_the_namespace_status() {
    let harness = Harness::new();

    // One name per status, so the status word in the message cannot be a constant: the
    // three statuses a namespace of this emulator reaches each produce their own
    // sentence for the same operation.
    let mut seen: Vec<&'static str> = Vec::new();
    for row in rows() {
        let case = row.case;
        let namespace = row.namespace.as_str();
        hold_the_name(&harness, &row).await;
        let (status, body) =
            create_namespace_in(&harness, REGION, row.account, create_body(namespace)).await;
        assert_eq!(
            status, 409,
            "case {case:?}: re-creating a held namespace expected 409, got {status} with {body}"
        );
        assert_eq!(
            body["__type"],
            json!("ConflictException"),
            "case {case:?}: a name already in use is a conflict, got {body}"
        );
        let message = body["Message"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the conflict carried no Message: {body}"))
            .to_string();
        assert!(
            message.starts_with(&format!(
                "Cannot create a namespace when status is {}. ",
                row.expected_status
            )),
            "case {case:?}: the message must open with the held namespace's status, got \
             {message:?}"
        );
        assert!(
            message.ends_with(". "),
            "case {case:?}: the message ends in a period and a space, got {message:?}"
        );
        assert!(
            message.contains(&format!(" for account {}. ", row.account)),
            "case {case:?}: the message must name the request's account, got {message:?}"
        );
        if !seen.contains(&row.expected_status) {
            seen.push(row.expected_status);
        }
    }

    for status in ["CREATING", "CREATED", "DELETING"] {
        assert!(
            seen.contains(&status),
            "the table must cover status {status:?}, it covered {seen:?}"
        );
    }
}
