// SPDX-License-Identifier: CC0-1.0

//! WHICH invocations one statement of a function's resource-based policy admits — the
//! invocation-TYPE complement of `crate::lambda_invoke_grant_lifetime`, which owns which
//! DOCUMENT carries the grant and which kind of principal calls. Both read one rule over the
//! same three crates (IAM holds the user and the key each request presents, the token service
//! resolves the principal, Lambda holds the policy and decides the invocation), so this suite
//! drives the harness that one publishes rather than carrying a second copy of it: its router,
//! its Lambda call, its principal, its deploy-and-wait, its refusal sentence and its refusal
//! assertion.
//!
//! RULE: a statement of the function's own resource-based policy admits an invocation of that
//! function BY ITSELF — the caller needs no identity-based grant — for every invocation type
//! the operation admits, and the decision is taken on the documents as they stand at the
//! instant of each call: every call made while no statement names the caller is refused HTTP
//! 403 `AccessDeniedException` carrying the sentence AWS words an authorization failure with,
//! both before `AddPermission` writes the statement and after `RemovePermission` takes it out.
//!
//! The rows span what this reading adds: the invocation TYPE the call asks for, every value the
//! model's `InvocationType` enum admits (the default `RequestResponse`, `Event`, `DryRun` —
//! `smithy_query(query="operation", operation="Invoke")` shows `StatusCode` is the response
//! code, so an admitted call of each answers 200 carrying what the handler returned, 202
//! carrying no payload, and 204 carrying none). Each type is driven through the statement's
//! whole life — refused before it is written, admitted while it stands, refused again once it is
//! withdrawn — against its own caller and its own statement, so the policy a row is decided on
//! is the one that row wrote.
//!
//! Every wait is for a CONDITION with a generous deadline (the deploy's, inherited from the
//! suite beside it, and Lambda having room to decide an invocation at all), so nothing asserted
//! here is decided by how fast or how loaded this host is.
//!
//! Why it is a cross-service suite rather than a row of the Lambda crate's own
//! `tests/all/invoke_resource_policy_grant_forms.rs` (which owns the FORM the statement names its
//! principal in, the action it names and the qualifier it is attached to, and whose denial half is
//! `tests/all/invoke_resource_policy_denial.rs`): the caller each row invokes as is an IAM user
//! whose key IAM minted and whom the token service resolves the request to, and the function is
//! declared by a CloudFormation stack, which is what archives inline handler source into a
//! deployment package the run can read. Four crates therefore have to agree before any invocation
//! type can be decided at all, and only the assembled router mounts them on one engine.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::lambda_invoke_grant_lifetime::{
    assert_refused, build_router, given_execution_role, given_function, given_user, handler_answer,
    lambda_as, refusal_message, Answer, ACCOUNT_KEY, FUNCTIONS_URI, INVOKE_ACTION,
};

/// The function every row invokes, the stack that declares it and the role it runs as — names of
/// this suite's own, so it deploys beside the suite it shares its harness with rather than over
/// it.
const FUNCTION: &str = "xsvc-invoke-type-grant";
const STACK: &str = "xsvc-invoke-type-grant-stack";
const EXECUTION_ROLE: &str = "xsvc-invoke-type-grant-exec";

/// The statuses Lambda answers an invocation it has not DECIDED with, each of which says "not
/// now" and carries no authorization verdict: no room in the account's concurrent executions
/// (`TooManyRequestsException`, HTTP 429), or the function not being `Active` yet
/// (`ResourceConflictException`, HTTP 409). Whether one arrives is decided by what the rest of
/// this process is running at that instant and by nothing a row varies, so a call LOOKS AGAIN on
/// one rather than reading a verdict out of an invocation that was never decided. Nothing is
/// tolerated by looking again: one of these still standing at the deadline fails the row.
const UNSERVED: &[u16] = &[409, 429];

/// How long a call keeps looking for a decision, and how long between two looks.
const DECISION_DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The header a caller asks for an invocation type with (`Invoke`'s `InvocationType` is bound to
/// `X-Amz-Invocation-Type`).
const INVOCATION_TYPE_HEADER: &str = "x-amz-invocation-type";

/// One row: the invocation type asked for, and what an ADMITTED call of that type answers.
struct Row {
    case: &'static str,
    /// The `X-Amz-Invocation-Type` the call carries, or none for the default the operation
    /// documents ("By default, Lambda invokes your function synchronously").
    invocation_type: Option<&'static str>,
    /// The status an admitted call of this type answers with.
    admitted_status: u16,
    /// What an admitted call's body must carry: the handler's own answer for the synchronous
    /// type, and no payload at all for the two whose answer reports no run.
    admitted_payload: Option<Value>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the default, synchronous invocation",
            invocation_type: None,
            admitted_status: 200,
            admitted_payload: Some(handler_answer()),
        },
        Row {
            case: "an asynchronous invocation, whose answer reports the queueing",
            invocation_type: Some("Event"),
            admitted_status: 202,
            admitted_payload: None,
        },
        Row {
            case: "a dry run, which verifies the permission and runs nothing",
            invocation_type: Some("DryRun"),
            admitted_status: 204,
            admitted_payload: None,
        },
    ]
}

/// Invoke the function as the credential `access_key_id` names, asking for the type `headers`
/// states, and answer the DECISION: looks again, every [`LOOK`] until [`DECISION_DEADLINE`]
/// passes, for as long as Lambda answers one of the statuses that carry no verdict
/// ([`UNSERVED`]).
async fn invoke_as(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    headers: &[(&str, &str)],
) -> Answer {
    let deadline = Instant::now() + DECISION_DEADLINE;
    loop {
        let answer = lambda_as(
            router,
            access_key_id,
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
            Some(&json!({"probe": true})),
            headers,
        )
        .await;
        if !UNSERVED.contains(&answer.status) {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never had room to decide the invocation, last answered {} \
             with {}",
            answer.status,
            answer.text()
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Write one statement into the function's resource-based policy through `AddPermission`, the
/// call a client grants a principal the invocation with.
async fn add_permission(
    router: &MultiServiceRouter,
    case: &str,
    statement_id: &str,
    principal: &str,
) {
    let answer = lambda_as(
        router,
        ACCOUNT_KEY,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/policy"),
        Some(&json!({
            "StatementId": statement_id,
            "Action": INVOKE_ACTION,
            "Principal": principal,
        })),
        &[],
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "case {case:?}: AddPermission({statement_id}) must answer 201, got {} with {}",
        answer.status,
        answer.text()
    );
}

/// Take that statement back out through `RemovePermission`.
async fn remove_permission(router: &MultiServiceRouter, case: &str, statement_id: &str) {
    let answer = lambda_as(
        router,
        ACCOUNT_KEY,
        "DELETE",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/policy/{statement_id}"),
        None,
        &[],
    )
    .await;
    assert_eq!(
        answer.status,
        204,
        "case {case:?}: RemovePermission({statement_id}) must answer 204, got {} with {}",
        answer.status,
        answer.text()
    );
}

/// Assert that `answer` is the ADMITTED answer `row` states for its invocation type.
fn assert_admitted(case: &str, row: &Row, answer: &Answer) {
    assert_eq!(
        answer.status,
        row.admitted_status,
        "case {case:?}: expected HTTP {} once the statement admits the caller, got {} with {}",
        row.admitted_status,
        answer.status,
        answer.text()
    );
    match &row.admitted_payload {
        Some(expected) => assert_eq!(
            &answer.json(),
            expected,
            "case {case:?}: expected the handler's own answer {expected}, got {}",
            answer.text()
        ),
        None => assert_eq!(
            answer.json(),
            Value::Null,
            "case {case:?}: expected no payload from an answer that reports no run, got {}",
            answer.text()
        ),
    }
}

/// Every row: the statement admits that invocation type on its own while it stands, and the same
/// call is refused 403 `AccessDeniedException` before it was written and after it was withdrawn.
#[tokio::test]
async fn a_resource_policy_statement_admits_each_invocation_type_while_it_stands() {
    let router = build_router();
    let setup = "the shared function";
    let role = given_execution_role(&router, setup, EXECUTION_ROLE).await;
    let function_arn = given_function(&router, setup, &role, FUNCTION, STACK).await;

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let headers: Vec<(&str, &str)> = match row.invocation_type {
            None => Vec::new(),
            Some(kind) => vec![(INVOCATION_TYPE_HEADER, kind)],
        };
        // Each row calls as a principal of its own, holding no identity-based document at all,
        // and writes a statement of its own: the only thing that can admit its invocation is
        // the statement this row wrote.
        let caller = format!("xsvc-invoke-type-caller-{index}");
        let principal = given_user(&router, case, &caller).await;
        let expected = refusal_message(&principal.arn, &function_arn);
        let statement_id = format!("granted-type-{index}");

        let before = invoke_as(&router, case, &principal.access_key_id, &headers).await;
        assert_refused(case, "before any statement was written", &before, &expected);

        add_permission(
            &router,
            case,
            &statement_id,
            &principal.statement_principal,
        )
        .await;
        let admitted = invoke_as(&router, case, &principal.access_key_id, &headers).await;
        assert_admitted(case, row, &admitted);

        remove_permission(&router, case, &statement_id).await;
        let after = invoke_as(&router, case, &principal.access_key_id, &headers).await;
        assert_refused(case, "after the statement was removed", &after, &expected);
    }
}
