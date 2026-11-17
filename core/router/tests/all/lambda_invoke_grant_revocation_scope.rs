// SPDX-License-Identifier: CC0-1.0

//! What a REVOCATION of one resource-policy statement does to the next invocation, over the
//! three crates that hold the decision: IAM holds the callers, the token service resolves which
//! principal a presented access key id speaks for, and Lambda holds the function's own
//! resource-based policy and decides the invocation before the deployment package is reached. It
//! drives the harness [`crate::lambda_invoke_grant_lifetime`] publishes — which owns the whole
//! LIFE of a single grant — so both suites deploy their function, make their principals, write
//! and withdraw their statements and read an invocation's verdict the one way.
//!
//! RULE: `lambda:RemovePermission` takes out of the addressed resource-based policy exactly the
//! statement its `StatementId` names, so from the next call on the principal that statement
//! named holds no grant and, carrying no identity-based document either, falls to IAM's default
//! deny and is refused HTTP 403 `AccessDeniedException` with the sentence AWS words an
//! authorization failure with — while every statement still in the document goes on authorizing
//! its own principal, and a statement written again under the same id authorizes again.
//!
//! Only a cross-service suite can pin it: the principals and their (absent) identity documents
//! are IAM's records, the ARN a refusal blames is what the token service reports for the key the
//! call presented, and both the policy and the refusal are Lambda's.
//!
//! The rows span what the rule generalises over: WHICH statement of a multi-statement document
//! the removal names (the first written, a middle one, the last), and therefore whose grant is
//! withdrawn while the others stand — a removal that emptied the document, or that left the
//! wrong grant in force, fails here where a single-statement document could not tell the
//! difference. Each row is read three ways over: the withdrawn principal's own invocation, a
//! standing principal's (a different one each row, so each of them is read as standing too), and
//! `GetPolicy`, which must report exactly the statements that still stand.
//!
//! Every invocation here asks for the DRY RUN, the invocation type whose whole documented
//! purpose is this decision — "Validate parameter values and verify that the user or role has
//! permission to invoke the function" — which answers 204 for a caller a document in force
//! admits and runs nothing for anybody. That is what this suite is about, and it keeps the suite
//! from standing up a second in-process language runtime beside the suites that assert a RUN:
//! that an admitted invocation really runs the deployment package is
//! [`crate::lambda_invoke_grant_lifetime`]'s and
//! [`crate::lambda_resource_policy_admits_each_invocation_type`]'s reading, the latter over this
//! very invocation type beside the other two.
//!
//! Every wait is for a CONDITION with a generous deadline — the deploy's and the one the
//! harness's reader of a verdict holds — so nothing asserted here is decided by how fast or how
//! loaded this host is.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::lambda_invoke_grant_lifetime::{
    add_permission, assert_refused, build_router, given_execution_role, given_function, given_user,
    invoke_decided, lambda_as, refusal_message, remove_permission, Answer, ACCOUNT_KEY,
    FUNCTIONS_URI,
};

/// The function every invocation below addresses, the stack that deploys it, and the execution
/// role it runs as — named for this suite, so it shares nothing with the suites beside it.
const FUNCTION: &str = "xsvc-grant-revocation-scope";
const STACK: &str = "xsvc-grant-revocation-scope-stack";
const EXECUTION_ROLE: &str = "xsvc-grant-revocation-scope-exec";

/// The invocation type every call below asks for, and the status an ADMITTED call of it answers.
/// Source: `Invoke`'s `InvocationType` member, whose `DryRun` value "validate[s] parameter values
/// and verif[ies] that the user or role has permission to invoke the function", and the
/// `@httpResponseCode` the operation answers it with.
const DRY_RUN_HEADER: (&str, &str) = ("x-amz-invocation-type", "DryRun");
const DRY_RUN_STATUS: u16 = 204;

/// How many principals the function's policy grants at once, and so how many statements it
/// holds while every grant stands.
const GRANTED: usize = 3;

/// The statement id the grant of the `index`-th principal is written under.
fn statement_id(index: usize) -> String {
    format!("grant-{index}")
}

/// The statement ids `GetPolicy` reports the function's policy as holding, sorted, read through
/// the API a client reads a policy with ("You can get the ID of the statement from the output of
/// GetPolicy").
async fn held_statement_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let read = lambda_as(
        router,
        ACCOUNT_KEY,
        "GET",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/policy"),
        None,
        &[],
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetPolicy must answer 200, got {} with {}",
        read.status,
        read.text()
    );
    let reported = read.json();
    let policy = reported
        .get("Policy")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: GetPolicy must report a Policy, got {reported}"))
        .to_string();
    let document: Value = serde_json::from_str(&policy).unwrap_or_else(|_| {
        panic!("case {case:?}: GetPolicy must report a policy document, got {policy}")
    });
    let statements = document
        .get("Statement")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a policy document carries statements, got {document}")
        });
    let mut held: Vec<String> = statements
        .iter()
        .filter_map(|statement| statement.get("Sid").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    held.sort();
    held
}

/// Ask for the dry run of the function as `access_key_id`, read through the harness's one reader
/// of an invocation's verdict.
async fn verify_permission(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
) -> Answer {
    invoke_decided(router, case, FUNCTION, access_key_id, &[DRY_RUN_HEADER]).await
}

/// Assert that `answer` is the ADMITTED answer: the documented 204, carrying no payload, which is
/// what Lambda answers a caller whose permission to invoke the function it verified.
fn assert_admitted(case: &str, phase: &str, answer: &Answer) {
    assert_eq!(
        answer.status,
        DRY_RUN_STATUS,
        "case {case:?} {phase}: expected HTTP {DRY_RUN_STATUS}, got {} with {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.json(),
        Value::Null,
        "case {case:?} {phase}: expected no payload from an answer that reports no run, got {}",
        answer.text()
    );
}

/// Every row: the withdrawal of one statement refuses exactly the principal that statement
/// named, leaves every other grant of the same document in force, is reported by `GetPolicy` as
/// that one statement gone, and is undone by writing the statement again.
#[tokio::test]
async fn a_revocation_withdraws_only_the_statement_it_names() {
    let router = build_router();
    let setup = "the shared function";
    let role = given_execution_role(&router, setup, EXECUTION_ROLE).await;
    let function_arn = given_function(&router, setup, &role, FUNCTION, STACK).await;

    // Each principal holds NO identity-based document at all, so the only thing that can admit
    // an invocation of theirs is the statement written for it here.
    let mut callers = Vec::new();
    for index in 0..GRANTED {
        let case = "the granted principals";
        let caller = given_user(&router, case, &format!("xsvc-revocation-caller-{index}")).await;
        add_permission(
            &router,
            case,
            FUNCTION,
            &statement_id(index),
            &caller.statement_principal,
        )
        .await;
        assert_admitted(
            case,
            "once its own statement is written",
            &verify_permission(&router, case, &caller.access_key_id).await,
        );
        callers.push(caller);
    }
    // Compared against what `GetPolicy` reports, which this suite reads back SORTED, so the
    // comparison is between two sorted lists whatever order the ids were written in and however
    // many of them there are.
    let all_granted: Vec<String> = {
        let mut ids: Vec<String> = (0..GRANTED).map(statement_id).collect();
        ids.sort();
        ids
    };

    for withdrawn in 0..GRANTED {
        let case = format!("statement {withdrawn} of {GRANTED} withdrawn");
        let revoked = statement_id(withdrawn);
        remove_permission(&router, &case, FUNCTION, &revoked).await;

        // What the document holds now: every statement but the one just named.
        let expected: Vec<String> = all_granted
            .iter()
            .filter(|held| **held != revoked)
            .cloned()
            .collect();
        let held = held_statement_ids(&router, &case).await;
        assert_eq!(
            held, expected,
            "case {case:?}: expected the policy to hold {expected:?}, got {held:?}"
        );

        // The principal whose statement is gone holds no grant anywhere.
        let refused = verify_permission(&router, &case, &callers[withdrawn].access_key_id).await;
        assert_refused(
            &case,
            "the principal whose statement was withdrawn",
            &refused,
            &refusal_message(&callers[withdrawn].arn, &function_arn),
        );

        // And one whose statement still stands is admitted as before — a different one each row,
        // so no principal is only ever read as the withdrawn one.
        let standing = (withdrawn + 1) % GRANTED;
        let admitted = verify_permission(&router, &case, &callers[standing].access_key_id).await;
        assert_admitted(&case, "a principal whose statement still stands", &admitted);

        // The same statement written again restores the document for the next row.
        add_permission(
            &router,
            &case,
            FUNCTION,
            &revoked,
            &callers[withdrawn].statement_principal,
        )
        .await;
        // And it authorizes that principal again straight away, so the refusal above was the
        // document's state and not a caller the decision had given up on.
        let regranted = verify_permission(&router, &case, &callers[withdrawn].access_key_id).await;
        assert_admitted(&case, "the statement written again", &regranted);
    }

    let case = "every statement written again";
    let held = held_statement_ids(&router, case).await;
    assert_eq!(
        held, all_granted,
        "case {case:?}: expected the policy to hold {all_granted:?}, got {held:?}"
    );
}
