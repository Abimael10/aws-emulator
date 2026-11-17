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

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::lambda_invoke_grant_lifetime::{
    add_permission, assert_refused, build_router, given_execution_role, given_function, given_user,
    handler_answer, invoke_decided, refusal_message, remove_permission, Answer,
};

/// The function every row invokes, the stack that declares it and the role it runs as — names of
/// this suite's own, so it deploys beside the suite it shares its harness with rather than over
/// it.
const FUNCTION: &str = "xsvc-invoke-type-grant";
const STACK: &str = "xsvc-invoke-type-grant-stack";
const EXECUTION_ROLE: &str = "xsvc-invoke-type-grant-exec";

// The statuses an invocation that was never DECIDED answers with, the deadline a call keeps
// looking for a decision over, and the look-again span all live in the harness this suite drives
// (`crate::lambda_invoke_grant_lifetime::invoke_decided`), so every suite over these routes reads
// a verdict the one way.

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
/// states, and answer the DECISION — read through the harness's one reader of an invocation's
/// verdict.
async fn invoke_as(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    headers: &[(&str, &str)],
) -> Answer {
    invoke_decided(router, case, FUNCTION, access_key_id, headers).await
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
            FUNCTION,
            &statement_id,
            &principal.statement_principal,
        )
        .await;
        let admitted = invoke_as(&router, case, &principal.access_key_id, &headers).await;
        assert_admitted(case, row, &admitted);

        remove_permission(&router, case, FUNCTION, &statement_id).await;
        let after = invoke_as(&router, case, &principal.access_key_id, &headers).await;
        assert_refused(case, "after the statement was removed", &after, &expected);
    }
}
