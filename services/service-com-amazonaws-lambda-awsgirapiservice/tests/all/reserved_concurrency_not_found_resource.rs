// SPDX-License-Identifier: CC0-1.0

//! What an operation on a function's RESERVED CONCURRENCY says it could not FIND:
//! `PutFunctionConcurrency` (`PUT /2017-10-31/functions/{FunctionName}/concurrency`),
//! `GetFunctionConcurrency` (`GET /2019-09-30/functions/{FunctionName}/concurrency`) and
//! `DeleteFunctionConcurrency` (`DELETE /2017-10-31/functions/{FunctionName}/concurrency`),
//! each of which models `ResourceNotFoundException` at HTTP 404
//! (`smithy_query(query="operation",
//! operation="PutFunctionConcurrency,GetFunctionConcurrency,DeleteFunctionConcurrency")`).
//!
//! The rule under test: **the capacity a reservation sets aside is the capacity an INVOCATION
//! of the function holds, so the resource one of these operations addresses is the function AT
//! the version such an invocation runs — the identifier they take carries no qualifier of its
//! own, so it resolves to the unpublished version — and a request that resolves to no function
//! is refused with the fixed text `Function not found: ` followed by that fully qualified ARN,
//! `:$LATEST` included.** It is the same resource a synchronous `Invoke` of the same name says
//! it could not find ([`crate::invoke_addressed_function`] pins that one), and the refusal
//! changes nothing: a function that DOES hold a reservation still holds exactly it afterwards.
//!
//! The rows differ along what the rule generalises over: WHICH operation asked (the write, the
//! read, the removal), the documented `FunctionName` form the request addresses the function by
//! (the bare name, the full ARN, the partial ARN, the `function:` prefix form), and WHY nothing
//! resolves (a name no function of this account was ever held under, and one whose function was
//! created and then DELETED). Two controls keep the table honest: the function this suite does
//! hold answers all three operations, and the reservation it holds is unchanged after every
//! refusal.

use std::sync::Arc;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::function_concurrency::{
    call, create_function, get_concurrency, put_concurrency, reported, router_on, Answer, ACCOUNT,
    REGION,
};

/// Where the function collection is bound, which is where a function is deleted.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The fixed text of the sentence, which is the whole message bar the resource it names.
const NOT_FOUND_PREFIX: &str = "Function not found: ";

/// The error code the model declares for a resource one of these operations cannot find.
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// How this service spells the unpublished version, which is what an identifier carrying no
/// qualifier resolves to (`Invoke`'s `ExecutedVersion`: "$LATEST").
const LATEST: &str = "$LATEST";

/// The function this suite HOLDS, and the reservation it holds — the control that no refusal
/// disturbs.
const HELD: &str = "reserved-not-found-held";
const HELD_RESERVATION: i64 = 37;

/// A name no function of this account is ever held under.
const ABSENT: &str = "reserved-not-found-absent";

/// A function this suite creates and then DELETES, so nothing resolves for a name that once
/// did.
const DELETED: &str = "reserved-not-found-deleted";

/// The resource the rule says a refusal names: the function's ARN in this endpoint's Region,
/// qualified by the version an invocation of the identifier would run.
fn resolved_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}:{LATEST}")
}

/// `DeleteFunctionConcurrency` on `reference`.
async fn delete_concurrency(router: &MultiServiceRouter, reference: &str) -> Answer {
    call(
        router,
        "DELETE",
        &format!("/2017-10-31/functions/{reference}/concurrency"),
        &Value::Null,
    )
    .await
}

/// `DeleteFunction` on `name`, asserted to have removed it.
async fn delete_function(router: &MultiServiceRouter, name: &str) {
    let answer = call(
        router,
        "DELETE",
        &format!("{FUNCTIONS_URI}/{name}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status,
        204,
        "DeleteFunction {name:?} must succeed, got {} {}",
        answer.status,
        answer.text()
    );
}

/// The `Message` of a wire error, under either casing the JSON envelopes use.
fn message(answer: &Answer) -> String {
    serde_json::from_str::<Value>(&answer.text())
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

/// Drive all three operations on `reference` and answer what each one said, labelled.
async fn every_operation(router: &MultiServiceRouter, reference: &str) -> Vec<(&'static str, Answer)> {
    vec![
        (
            "PutFunctionConcurrency",
            put_concurrency(router, reference, 1).await,
        ),
        (
            "GetFunctionConcurrency",
            get_concurrency(router, reference).await,
        ),
        (
            "DeleteFunctionConcurrency",
            delete_concurrency(router, reference).await,
        ),
    ]
}

#[tokio::test]
async fn a_reservation_that_resolves_to_nothing_names_the_version_qualified_resource() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);

    // The control function, holding a reservation nothing below may disturb, and the function
    // that is created and then deleted.
    create_function(&router, HELD).await;
    let reserved = put_concurrency(&router, HELD, HELD_RESERVATION).await;
    assert_eq!(
        reserved.status,
        200,
        "the control reservation must be made, got {} {}",
        reserved.status,
        reserved.text()
    );
    create_function(&router, DELETED).await;
    delete_function(&router, DELETED).await;

    // (what is missing, the function the refusal must name)
    for (missing, name) in [
        ("a name no function was ever held under", ABSENT),
        ("a function that was created and then deleted", DELETED),
    ] {
        let expected = format!("{NOT_FOUND_PREFIX}{}", resolved_arn(name));
        for (form, reference) in references(name) {
            for (operation, answer) in every_operation(&router, &reference).await {
                let case = format!("{missing} addressed by {form} ({operation})");
                assert_eq!(
                    answer.status,
                    404,
                    "case {case:?}: expected the modelled 404, got {} {}",
                    answer.status,
                    answer.text()
                );
                assert!(
                    answer.error_code().contains(NOT_FOUND_CODE),
                    "case {case:?}: expected {NOT_FOUND_CODE}, got {:?}",
                    answer.error_code()
                );
                let reported = message(&answer);
                assert_eq!(
                    reported, expected,
                    "case {case:?}: expected {expected:?}, got {reported:?}"
                );
            }
        }
    }

    // The controls, after every refusal: the function this suite holds still answers all three
    // operations, and the reservation it holds is exactly the one it was given.
    let held = reported("the function this suite holds", &router, HELD).await;
    assert_eq!(
        held,
        Some(HELD_RESERVATION),
        "a refused call must leave the live function's reservation alone, got {held:?}"
    );
    let removed = delete_concurrency(&router, HELD).await;
    assert_eq!(
        removed.status,
        204,
        "the held function must answer DeleteFunctionConcurrency, got {} {}",
        removed.status,
        removed.text()
    );
    let cleared = reported("the function this suite holds, after its removal", &router, HELD).await;
    assert_eq!(
        cleared, None,
        "the removal on the held function must clear exactly its own reservation, got \
         {cleared:?}"
    );
}
