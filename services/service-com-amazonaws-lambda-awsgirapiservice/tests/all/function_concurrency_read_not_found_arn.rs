// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the RESOURCE `GetFunctionConcurrency` says it could not find.
//!
//! The rule under test: a read of a function's reserved concurrency addresses the FUNCTION
//! itself — the identifier it takes carries no qualifier at all — so a read that resolves to
//! no function is refused the modelled `ResourceNotFoundException` (HTTP 404) naming that
//! function's own ARN in the Region of the call, spelled with exactly the qualifier the
//! request named, which is none: the fixed text `Function not found: ` followed by the
//! unqualified ARN.
//!
//! The ARN asserted here is not written out as a literal: it is the `FunctionArn` the service
//! itself reported for that function while it existed, so the refusal is pinned to the
//! identity the service publishes for the resource rather than to a form this test chose. The
//! rows differ along what the rule generalises over: the shape of the function's own name
//! (letters, digits, hyphens, underscores, and the longest name the member admits), every
//! documented `FunctionName` form the refused read addresses it by, and whether the function
//! was DELETED or never created at all.
//!
//! The spelling the two WRITES on the same reservation name — the function at the version the
//! reserved capacity is held for — is pinned beside this one in
//! [`crate::reserved_concurrency_not_found_resource`].

use std::sync::Arc;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::function_concurrency::{
    call, create_function, get_concurrency, router_on, Answer, ACCOUNT, REGION,
};

/// The fixed text of the sentence, which is the whole message bar the resource it names.
const NOT_FOUND_PREFIX: &str = "Function not found: ";

/// The error code the model declares for a resource this read cannot find.
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// `GetFunction` (`GET /2015-03-31/functions/{FunctionName}`), whose `FunctionArn` is the
/// service's own spelling of the function's identity.
async fn get_function(router: &MultiServiceRouter, reference: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/2015-03-31/functions/{reference}"),
        &Value::Null,
    )
    .await
}

/// `DeleteFunction` on `name`, asserted to have removed it.
async fn delete_function(router: &MultiServiceRouter, name: &str) {
    let answer = call(
        router,
        "DELETE",
        &format!("/2015-03-31/functions/{name}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status, 204,
        "DeleteFunction {name:?} must succeed, got {} {}",
        answer.status,
        answer.text()
    );
}

/// The `FunctionArn` the service reports for the function `name`, which must exist.
async fn reported_arn(router: &MultiServiceRouter, name: &str) -> String {
    let answer = get_function(router, name).await;
    assert_eq!(
        answer.status, 200,
        "GetFunction {name:?} must succeed, got {} {}",
        answer.status,
        answer.text()
    );
    serde_json::from_slice::<Value>(&answer.body)
        .ok()
        .and_then(|body| {
            body.pointer("/Configuration/FunctionArn")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| panic!("GetFunction must report a FunctionArn, got {}", answer.text()))
}

/// The `Message` of a wire error, under either casing the JSON envelopes use.
fn message(answer: &Answer) -> String {
    serde_json::from_slice::<Value>(&answer.body)
        .ok()
        .and_then(|body| {
            body.get("Message")
                .or_else(|| body.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// Every documented `FunctionName` form that addresses the function `name` itself.
fn references(name: &str) -> Vec<(&'static str, String)> {
    vec![
        ("the bare name", name.to_string()),
        (
            "the full ARN",
            format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}"),
        ),
        ("the partial ARN", format!("{ACCOUNT}:function:{name}")),
        ("the function: prefix form", format!("function:{name}")),
    ]
}

/// Assert the read of `reference`'s reserved concurrency is refused for `expected_arn`.
async fn refused_for(
    router: &MultiServiceRouter,
    case: &str,
    reference: &str,
    expected_arn: &str,
) {
    let answer = get_concurrency(router, reference).await;
    assert_eq!(
        answer.status, 404,
        "case {case:?}: expected the modelled 404, got {} {}",
        answer.status,
        answer.text()
    );
    assert!(
        answer.error_code().contains(NOT_FOUND_CODE),
        "case {case:?}: expected {NOT_FOUND_CODE}, got {:?}",
        answer.error_code()
    );
    let expected = format!("{NOT_FOUND_PREFIX}{expected_arn}");
    let reported = message(&answer);
    assert_eq!(
        reported, expected,
        "case {case:?}: expected {expected:?}, got {reported:?}"
    );
}

/// RULE: the read's refusal names the function's own ARN — the very `FunctionArn` the service
/// reported for it — with no qualifier appended.
#[tokio::test]
async fn the_read_refusal_names_the_function_arn_the_service_reported() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    // (case, the function's own name) — `FunctionName`'s own shape (`@pattern` admits
    // letters, digits, hyphens and underscores) at the ends of the lengths one is seen in.
    let longest = "c".repeat(64);
    let names: [(&str, &str); 4] = [
        ("a plain name", "concurrency-read-plain"),
        ("digits and underscores", "concurrency_read_2_fn"),
        ("one character", "c"),
        ("the longest name the member admits", longest.as_str()),
    ];

    for (case, name) in names {
        create_function(&router, name).await;
        let arn = reported_arn(&router, name).await;
        // While it exists, the read is SERVED: the refusals below are about the function
        // being gone and not about the operation.
        let held = get_concurrency(&router, name).await;
        assert_eq!(
            held.status, 200,
            "case {case:?}: the read must be served while the function exists, got {} {}",
            held.status,
            held.text()
        );
        delete_function(&router, name).await;

        for (form, reference) in references(name) {
            let case = format!("{case}, deleted, addressed by {form}");
            refused_for(&router, &case, &reference, &arn).await;
        }
    }

    // A name no function was ever held under, whose ARN is derived from the one the service
    // reported above rather than written out here: the same Region, partition and account,
    // with this name.
    let witness = reported_arn_prefix(&router, "concurrency-read-witness").await;
    for (form, reference) in references("concurrency-read-absent") {
        let case = format!("a name no function was ever held under, addressed by {form}");
        refused_for(
            &router,
            &case,
            &reference,
            &format!("{witness}concurrency-read-absent"),
        )
        .await;
    }
}

/// Everything up to the function's own name in the ARN this service reports for a function:
/// read off a function it really holds, so no part of the ARN form is written out here.
async fn reported_arn_prefix(router: &MultiServiceRouter, name: &str) -> String {
    create_function(router, name).await;
    let arn = reported_arn(router, name).await;
    let prefix = arn
        .strip_suffix(name)
        .unwrap_or_else(|| panic!("the reported ARN {arn} must end in the function's name"));
    prefix.to_string()
}
