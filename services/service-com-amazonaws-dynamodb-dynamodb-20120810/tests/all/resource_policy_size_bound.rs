// SPDX-License-Identifier: CC0-1.0

//! The documented size limit on a resource-based policy document, and how crossing it is
//! worded.
//!
//! THE RULE: the policy document a resource-policy write carries is bounded in SIZE, and
//! a document past that bound breaks a bound on the value of a member — so it is refused
//! as the parameter-value violation it is, in AWS's wording for that class, naming the
//! limit and nothing of the document or of the resource it was offered for. The bound
//! itself is inclusive: a document OF the limit is attached, one byte past it is not.
//!
//! The rows span the bound rather than one recorded document: the published limit exactly,
//! one byte past it, and a document many times over it — each refusal stating the same
//! sentence, because the limit is a property of the member and not of the document that
//! broke it.

use std::time::{Duration, Instant};

use serde_json::json;

use service_com_amazonaws_dynamodb_dynamodb_20120810::state::MAX_RESOURCE_POLICY_BYTES;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok, REGION};

/// The table the policies below are attached to.
const TABLE: &str = "PolicySizeBound";

/// A policy document whose JSON text is exactly `bytes` long, padded in the `Sid` of its
/// one statement — every padding character adds exactly one byte to the text, so the
/// length a row asks for is the length the request carries.
fn document_of(arn: &str, bytes: usize) -> String {
    let empty = padded(arn, 0);
    let padding = bytes.saturating_sub(empty.len());
    let document = padded(arn, padding);
    assert_eq!(
        document.len(),
        bytes.max(empty.len()),
        "case a document of {bytes} bytes: the padding must land the text on that length"
    );
    document
}

/// A valid policy document with `padding` characters of `Sid`.
fn padded(arn: &str, padding: usize) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "s".repeat(padding),
            "Effect": "Deny",
            "Principal": {"AWS": "*"},
            "Action": "dynamodb:GetItem",
            "Resource": arn,
        }],
    })
    .to_string()
}

/// Poll `GetResourcePolicy` until the resource reports `expected` — the span the store
/// takes to apply a policy write is its own, so the wait is for the CONDITION rather than
/// for a time. Bounded: a document that never becomes readable fails instead of hanging,
/// naming the lengths rather than the documents, which are kilobytes long.
async fn applied(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    arn: &str,
    expected: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = String::new();
    while Instant::now() < deadline {
        let (status, body) = call(router, "GetResourcePolicy", json!({"ResourceArn": arn})).await;
        last = if status == 200 {
            body["Policy"].as_str().unwrap_or_default().to_string()
        } else {
            format!("<{}: {}>", status, error_code(&body))
        };
        if last == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "case {case}: expected the resource to report the {} byte document it accepted, \
         got {} byte(s): {}",
        expected.len(),
        last.len(),
        &last[..last.len().min(120)]
    )
}

#[tokio::test]
async fn a_policy_document_is_attached_up_to_the_documented_size_and_refused_past_it() {
    let router = emulator();
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    let described = describe_when_active(&router, "the policy table", TABLE).await;
    let arn = described["TableArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the description carries the table's ARN, got {described}"))
        .to_string();

    // The sentence AWS states for a document past the limit: fixed text naming the
    // limit, with nothing of the document or the resource in it.
    let refusal = format!(
        "One or more parameter values were invalid: Maximum policy size of \
         {MAX_RESOURCE_POLICY_BYTES} bytes exceeded"
    );
    // (case, the document's own byte length, whether it is attached)
    let rows: [(&str, usize, bool); 4] = [
        ("well inside the limit", MAX_RESOURCE_POLICY_BYTES / 2, true),
        ("exactly the limit", MAX_RESOURCE_POLICY_BYTES, true),
        ("one byte past the limit", MAX_RESOURCE_POLICY_BYTES + 1, false),
        (
            "many times the limit",
            MAX_RESOURCE_POLICY_BYTES * 20,
            false,
        ),
    ];
    for (case, bytes, attached) in rows {
        let document = document_of(&arn, bytes);
        let (status, body) = call(
            &router,
            "PutResourcePolicy",
            json!({"ResourceArn": arn, "Policy": document}),
        )
        .await;
        if attached {
            assert_eq!(
                status, 200,
                "case {case}: a document of {bytes} bytes is attached, got {status} with {body}"
            );
            // The round trip: the document the write accepted is the document a read of
            // the resource reports, once the write has been applied.
            applied(&router, case, &arn, &document).await;
        } else {
            assert_eq!(
                (status, error_code(&body).as_str(), message(&body).as_str()),
                (400, "ValidationException", refusal.as_str()),
                "case {case}: expected 400 ValidationException {refusal:?}, got {status} \
                 with {body}"
            );
        }
    }

    // A refused document attached nothing: the policy the resource carries is still the
    // last one that was accepted, which is the one OF the limit.
    applied(
        &router,
        "after the refusals",
        &arn,
        &document_of(&arn, MAX_RESOURCE_POLICY_BYTES),
    )
    .await;
    // The region the ARN above names is the one these calls were signed for, which is
    // what makes the resource addressable at all.
    assert!(
        arn.contains(REGION),
        "the table's ARN names the Region the requests are signed for, got {arn}"
    );
}
