// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusal the resource-policy READ answers once the resource a
//! grant was made on is DELETED.
//!
//! The rule under test: `GetPolicy` answers for the policy DOCUMENT the request addressed,
//! so when that document cannot be found — because the function, the version or the alias
//! the request names no longer exists — the refusal is `ResourceNotFoundException` at HTTP
//! 404 carrying the service's one fixed not-found sentence, which repeats nothing of the
//! request: not the function name, not the account id, not the Region, not the ARN the
//! reference resolves to. The resource-identifying refusal (`Function not found: <arn>`)
//! belongs to the read of the FUNCTION itself, which is what a teardown branching on the
//! refusal text tells apart from "this function holds no policy".
//!
//! The rows differ along what the rule generalises over: which resource was deleted (the
//! whole function, one published version of a live function, an alias of a live function),
//! the form the request names the function in (a bare name, the `function:` prefix form, a
//! partial ARN, a full ARN), and where the version or alias is spelled (in the
//! `FunctionName` or in the `Qualifier` query parameter). Every row drives real HTTP
//! requests through the router, sharing the harness in `function_lifecycle.rs`, and every
//! row's policy is first read back at HTTP 200 so no row is vacuously refused.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    create_function, function_arn, json_call, package_bytes, router_on, zip_file, Answer, ACCOUNT,
    REGION, ROLE,
};

/// Where the function operations and the policy sub-resource are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The one sentence a policy READ carries when the document it was asked for cannot be
/// found. Spelled out here rather than read from `state::RESOURCE_NOT_FOUND_MESSAGE`, so a
/// change to that constant fails this suite.
const RESOURCE_NOT_FOUND_MESSAGE: &str = "The resource you requested does not exist.";

/// The error code and status every refusal below carries
/// (`smithy_query(query="operation", operation="GetPolicy")`: `ResourceNotFoundException`
/// at HTTP 404).
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 404;

/// The fixed part of the RESOURCE-identifying not-found refusal — the one the read of the
/// function itself answers — which the policy read must never borrow.
const FUNCTION_NOT_FOUND_PREFIX: &str = "Function not found";

/// A grant every seeded resource carries, so each one holds a policy document before it is
/// deleted.
const STATEMENT_ID: &str = "object-store-invoke";
const GRANTED_PRINCIPAL: &str = "s3.amazonaws.com";
const GRANTED_ACTION: &str = "lambda:InvokeFunction";

/// The body of a dispatched answer, for a failure message.
fn body_text(answer: &Answer) -> String {
    String::from_utf8_lossy(&answer.body).to_string()
}

/// The `message` of a wire error, however the protocol spells the member.
fn message_of(answer: &Answer) -> String {
    answer
        .string("message")
        .or_else(|| answer.string("Message"))
        .unwrap_or_default()
}

/// The `?Qualifier=` suffix of a qualified call, and nothing for a function-level one.
fn qualifier_query(qualifier: Option<&str>) -> String {
    qualifier.map_or_else(String::new, |value| format!("?Qualifier={value}"))
}

/// `GetPolicy` on a function reference, optionally carrying a `Qualifier`.
async fn get_policy(
    router: &MultiServiceRouter,
    reference: &str,
    qualifier: Option<&str>,
) -> Answer {
    let uri = format!(
        "{FUNCTIONS_URI}/{reference}/policy{}",
        qualifier_query(qualifier)
    );
    json_call(router, "GET", &uri, &Value::Null).await
}

/// `GetFunction` on a function reference, which is the read whose refusal DOES identify the
/// resource.
async fn get_function(router: &MultiServiceRouter, reference: &str) -> Answer {
    let uri = format!("{FUNCTIONS_URI}/{reference}");
    json_call(router, "GET", &uri, &Value::Null).await
}

/// `AddPermission` on a function reference, optionally qualified.
async fn add_permission(router: &MultiServiceRouter, reference: &str, qualifier: Option<&str>) {
    let uri = format!(
        "{FUNCTIONS_URI}/{reference}/policy{}",
        qualifier_query(qualifier)
    );
    let answer = json_call(
        router,
        "POST",
        &uri,
        &json!({
            "StatementId": STATEMENT_ID,
            "Action": GRANTED_ACTION,
            "Principal": GRANTED_PRINCIPAL,
        }),
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "case {reference:?} qualifier {qualifier:?}: expected 201 from AddPermission, got {} {}",
        answer.status,
        body_text(&answer)
    );
}

/// `DeleteFunction`, on the whole function or on one qualifier of it.
async fn delete_function(router: &MultiServiceRouter, reference: &str, qualifier: Option<&str>) {
    let uri = format!("{FUNCTIONS_URI}/{reference}{}", qualifier_query(qualifier));
    let answer = json_call(router, "DELETE", &uri, &Value::Null).await;
    assert_eq!(
        answer.status,
        204,
        "case {reference:?} qualifier {qualifier:?}: expected 204 from DeleteFunction, got {} {}",
        answer.status,
        body_text(&answer)
    );
}

/// `DeleteAlias` on one alias of a function.
async fn delete_alias(router: &MultiServiceRouter, function: &str, alias: &str) {
    let uri = format!("{FUNCTIONS_URI}/{function}/aliases/{alias}");
    let answer = json_call(router, "DELETE", &uri, &Value::Null).await;
    assert_eq!(
        answer.status,
        204,
        "case {function:?} alias {alias:?}: expected 204 from DeleteAlias, got {} {}",
        answer.status,
        body_text(&answer)
    );
}

/// `PublishVersion`, which makes version 1 of a freshly created function.
async fn publish_version(router: &MultiServiceRouter, function: &str) {
    let uri = format!("{FUNCTIONS_URI}/{function}/versions");
    let answer = json_call(router, "POST", &uri, &json!({})).await;
    assert_eq!(
        answer.status,
        201,
        "case {function:?}: expected 201 from PublishVersion, got {} {}",
        answer.status,
        body_text(&answer)
    );
}

/// `CreateAlias` naming one published version.
async fn create_alias(router: &MultiServiceRouter, function: &str, alias: &str, version: &str) {
    let uri = format!("{FUNCTIONS_URI}/{function}/aliases");
    let answer = json_call(
        router,
        "POST",
        &uri,
        &json!({ "Name": alias, "FunctionVersion": version }),
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "case {function:?} alias {alias:?}: expected 201 from CreateAlias, got {} {}",
        answer.status,
        body_text(&answer)
    );
}

/// Create one function.
async fn create_plain_function(router: &MultiServiceRouter, name: &str) {
    let answer = create_function(
        router,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": zip_file(&package_bytes(64)) },
        }),
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "case {name:?}: expected 201 from CreateFunction, got {} {}",
        answer.status,
        body_text(&answer)
    );
}

/// Assert one read of a policy document answers at HTTP 200 and reports a document, so the
/// row that deletes the resource is refused for the deletion rather than for never having
/// held a policy.
async fn assert_policy_is_readable(
    router: &MultiServiceRouter,
    case: &str,
    reference: &str,
    qualifier: Option<&str>,
) {
    let answer = get_policy(router, reference, qualifier).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected 200 from GetPolicy before the deletion, got {} {}",
        answer.status,
        body_text(&answer)
    );
    let policy = answer
        .string("Policy")
        .unwrap_or_else(|| panic!("case {case:?}: no Policy member in {}", body_text(&answer)));
    assert!(
        policy.contains(STATEMENT_ID),
        "case {case:?}: expected the granted statement in the document, got {policy:?}"
    );
}

/// Assert one policy read is refused with the fixed sentence, naming nothing of the
/// request.
fn assert_fixed_refusal(case: &str, reference: &str, answer: &Answer) {
    let message = message_of(answer);
    assert_eq!(
        (answer.status, answer.error_code().contains(NOT_FOUND_CODE)),
        (NOT_FOUND_STATUS, true),
        "case {case:?}: expected HTTP {NOT_FOUND_STATUS} {NOT_FOUND_CODE}, got {} {}",
        answer.status,
        body_text(answer)
    );
    assert_eq!(
        message, RESOURCE_NOT_FOUND_MESSAGE,
        "case {case:?}: expected the fixed sentence, got {message:?}"
    );
    // Restated as its own check: what the rule forbids is a refusal that repeats the
    // request's own values back, or that borrows the resource-identifying wording of the
    // read of the function itself.
    for forbidden in [
        reference,
        REGION,
        ACCOUNT,
        FUNCTION_NOT_FOUND_PREFIX,
        &function_arn(reference),
    ] {
        assert!(
            !message.contains(forbidden),
            "case {case:?}: the sentence is fixed, so it may not carry {forbidden:?}, got \
             {message:?}"
        );
    }
}

/// One row: a policy read whose resource has been deleted.
struct Case {
    name: &'static str,
    reference: String,
    qualifier: Option<&'static str>,
}

/// The functions the rows address: one deleted outright, one that loses a published
/// version, one that loses an alias.
const DELETED_FUNCTION: &str = "granted-and-deleted";
const VERSION_DELETED: &str = "granted-version-deleted";
const ALIAS_DELETED: &str = "granted-alias-deleted";

/// The alias name the aliased rows use.
const ALIAS: &str = "live";

/// Create every resource the rows address, grant on it, read the grant back, and then
/// delete the resource the row is about.
async fn seed(router: &MultiServiceRouter) {
    // A function granted at three identities — the unqualified function, a published
    // version, and an alias of that version — and then deleted outright.
    create_plain_function(router, DELETED_FUNCTION).await;
    add_permission(router, DELETED_FUNCTION, None).await;
    publish_version(router, DELETED_FUNCTION).await;
    add_permission(router, DELETED_FUNCTION, Some("1")).await;
    create_alias(router, DELETED_FUNCTION, ALIAS, "1").await;
    add_permission(router, DELETED_FUNCTION, Some(ALIAS)).await;
    assert_policy_is_readable(router, "the function's own policy", DELETED_FUNCTION, None).await;
    assert_policy_is_readable(router, "version 1's policy", DELETED_FUNCTION, Some("1")).await;
    assert_policy_is_readable(router, "the alias's policy", DELETED_FUNCTION, Some(ALIAS)).await;
    delete_function(router, DELETED_FUNCTION, None).await;

    // A LIVE function one published version of which is deleted.
    create_plain_function(router, VERSION_DELETED).await;
    publish_version(router, VERSION_DELETED).await;
    add_permission(router, VERSION_DELETED, Some("1")).await;
    assert_policy_is_readable(
        router,
        "the deleted version's policy",
        VERSION_DELETED,
        Some("1"),
    )
    .await;
    delete_function(router, VERSION_DELETED, Some("1")).await;

    // A LIVE function one alias of which is deleted.
    create_plain_function(router, ALIAS_DELETED).await;
    publish_version(router, ALIAS_DELETED).await;
    create_alias(router, ALIAS_DELETED, ALIAS, "1").await;
    add_permission(router, ALIAS_DELETED, Some(ALIAS)).await;
    assert_policy_is_readable(
        router,
        "the deleted alias's policy",
        ALIAS_DELETED,
        Some(ALIAS),
    )
    .await;
    delete_alias(router, ALIAS_DELETED, ALIAS).await;
}

/// The rows: every documented form of the reference, and each kind of deleted resource.
/// The reference forms are the ones `FunctionName` documents — "Function name", "Function
/// ARN", "Partial ARN", each optionally carrying a version or alias
/// (`smithy_query(query="operation", operation="GetPolicy")`) — plus the `function:` prefix
/// form that pattern admits.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "the deleted function, by bare name",
            reference: DELETED_FUNCTION.to_string(),
            qualifier: None,
        },
        Case {
            name: "the deleted function, by the function: prefix form",
            reference: format!("function:{DELETED_FUNCTION}"),
            qualifier: None,
        },
        Case {
            name: "the deleted function, by partial ARN",
            reference: format!("{ACCOUNT}:function:{DELETED_FUNCTION}"),
            qualifier: None,
        },
        Case {
            name: "the deleted function, by full ARN",
            reference: function_arn(DELETED_FUNCTION),
            qualifier: None,
        },
        Case {
            name: "a version of the deleted function, qualified in the FunctionName",
            reference: format!("{DELETED_FUNCTION}:1"),
            qualifier: None,
        },
        Case {
            name: "a version of the deleted function, in the Qualifier",
            reference: DELETED_FUNCTION.to_string(),
            qualifier: Some("1"),
        },
        Case {
            name: "an alias of the deleted function, in the Qualifier",
            reference: DELETED_FUNCTION.to_string(),
            qualifier: Some(ALIAS),
        },
        Case {
            name: "an alias of the deleted function, by full ARN and Qualifier",
            reference: function_arn(DELETED_FUNCTION),
            qualifier: Some(ALIAS),
        },
        Case {
            name: "a deleted version of a live function",
            reference: VERSION_DELETED.to_string(),
            qualifier: Some("1"),
        },
        Case {
            name: "a deleted alias of a live function",
            reference: ALIAS_DELETED.to_string(),
            qualifier: Some(ALIAS),
        },
    ]
}

/// Every policy read whose resource was deleted is refused 404 `ResourceNotFoundException`
/// with the one fixed sentence, which repeats nothing of the request.
#[tokio::test]
async fn a_policy_read_of_a_deleted_resource_answers_the_fixed_not_found_sentence() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    seed(&router).await;

    for case in cases() {
        let answer = get_policy(&router, &case.reference, case.qualifier).await;
        assert_fixed_refusal(case.name, &case.reference, &answer);
    }
}

/// The refusal the policy read answers for a deleted function is NOT the one the read of
/// the function itself answers: the function read identifies the resource, the policy read
/// names nothing. A live sibling still reports its own document, and a function recreated
/// under the deleted one's name holds no policy, so the document went with the function
/// rather than outliving it.
#[tokio::test]
async fn a_deleted_functions_policy_refusal_differs_from_the_read_of_the_function_itself() {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    create_plain_function(&router, "torn-down").await;
    add_permission(&router, "torn-down", None).await;
    create_plain_function(&router, "kept").await;
    add_permission(&router, "kept", None).await;
    assert_policy_is_readable(&router, "the function to tear down", "torn-down", None).await;
    assert_policy_is_readable(&router, "the function to keep", "kept", None).await;
    delete_function(&router, "torn-down", None).await;

    // The read of the FUNCTION identifies the resource it could not find.
    let function_read = get_function(&router, "torn-down").await;
    let function_message = message_of(&function_read);
    assert!(
        function_read.status == NOT_FOUND_STATUS
            && function_message.contains(&function_arn("torn-down")),
        "case {:?}: expected HTTP {NOT_FOUND_STATUS} naming the function's ARN, got {} {}",
        "GetFunction on the deleted function",
        function_read.status,
        body_text(&function_read)
    );

    // The read of its POLICY names nothing, so the two refusals are distinguishable.
    let policy_read = get_policy(&router, "torn-down", None).await;
    assert_fixed_refusal(
        "GetPolicy on the deleted function",
        "torn-down",
        &policy_read,
    );
    assert_ne!(
        message_of(&policy_read),
        function_message,
        "case {:?}: the policy refusal must not reuse the function read's text, got {}",
        "GetPolicy on the deleted function",
        body_text(&policy_read)
    );

    // Deleting one function left the other's document alone.
    assert_policy_is_readable(&router, "the kept function after the deletion", "kept", None).await;

    // The document was deleted with the function: a new function of the same name holds
    // none, and that read is refused with the same fixed sentence.
    create_plain_function(&router, "torn-down").await;
    let recreated = get_policy(&router, "torn-down", None).await;
    assert_fixed_refusal(
        "GetPolicy on a function recreated under the deleted one's name",
        "torn-down",
        &recreated,
    );
}
