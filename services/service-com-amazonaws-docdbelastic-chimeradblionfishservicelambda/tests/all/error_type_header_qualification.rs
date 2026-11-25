// SPDX-License-Identifier: CC0-1.0

//! The two response layers of this service cannot disagree about one call's
//! `x-amzn-errortype`.
//!
//! `tests/all/error_type_header_form.rs` asserts the FORM a failure names its error shape in,
//! over the operations, the error shapes and the layers; this suite asserts the one property
//! that table cannot state, because it is a comparison between layers rather than a value:
//! **a failure an operation answered and a failure the framework answered for the same
//! service name their shape with the SAME single locator**, so a client classifying either
//! reads one rule. That is the invariant the service's composition of the two layers exists
//! for, and the one a second application of the completion would break.
//!
//! The request plumbing, the independent locator oracle and the form assertion are the
//! sibling suite's, imported rather than copied, so there is one harness for the header.

use crate::error_type_header_form::{
    answer, emulator, label, locator, qualified_shape_named, ACCOUNT, REGION,
};
use serde_json::json;

#[tokio::test]
async fn both_response_layers_name_the_shape_with_one_locator() {
    let router = emulator();
    let unheld = format!(
        "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster/00000000-0000-0000-0000-000000000000"
    );

    // Answered by an operation: the modelled refusal a tag write makes for an ARN it holds
    // nothing under.
    let (status, answered_by_operation, body) = answer(
        &router,
        "POST",
        &format!("/tags/{}", label(&unheld)),
        Some(json!({"tags": {"Project": "lionfish"}})),
    )
    .await;
    assert_eq!(
        status, 404,
        "case \"an operation's refusal\": expected HTTP 404, got {status} with {body}"
    );
    let by_operation = qualified_shape_named("an operation's refusal", &answered_by_operation);

    // Answered by NO operation: a request the routing table cannot name at all, rendered by
    // the framework and completed at the definition layer instead.
    let (status, answered_by_framework, body) =
        answer(&router, "GET", "/not-an-operation", None).await;
    assert!(
        status >= 400,
        "case \"a routing failure\": a request naming no operation is a failure, got {status} \
         with {body}"
    );
    let by_framework = qualified_shape_named("a routing failure", &answered_by_framework);

    // Each layer named its own shape — they are different failures — but both qualified it
    // with the one locator this service derives from its own identity, exactly once.
    let expected_operation = format!("{by_operation}{}", locator());
    let expected_framework = format!("{by_framework}{}", locator());
    assert_eq!(
        answered_by_operation.as_deref(),
        Some(expected_operation.as_str()),
        "case \"an operation's refusal\": expected {expected_operation:?}, got \
         {answered_by_operation:?}"
    );
    assert_eq!(
        answered_by_framework.as_deref(),
        Some(expected_framework.as_str()),
        "case \"a routing failure\": expected {expected_framework:?}, got \
         {answered_by_framework:?}"
    );
}
