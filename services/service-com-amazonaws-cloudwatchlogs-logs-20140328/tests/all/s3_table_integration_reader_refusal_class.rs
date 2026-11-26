// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CLASS and the SENTENCE
//! `ListSourcesForS3TableIntegration` refuses an `integrationArn` with.
//!
//! The rule under test: this reader resolves ONE family — the S3 Table Integrations of the
//! service that owns them, which is what its `integrationArn` names — so an ARN carrying any
//! other resource is told the resource form an integration's ARN carries, whatever else that
//! ARN names and whatever the account holds under it, and every refusal of an
//! `integrationArn` it cannot resolve travels under the ONE class this operation records for
//! them, the modelled `ValidationException` (HTTP 400): the rung decides the sentence, never
//! the class.
//!
//! The table spans what the rule generalises over — each field an integration's ARN can be
//! wrong in (the partition, the service, the Region, the account, the resource type and the
//! integration id), an ARN of this service's OWN `integration:` resource, and a handle of the
//! right form the account holds nothing under — and the controls on either side keep it
//! honest: an integration the account DOES hold is served 200, and the sibling
//! `AssociateSourceToS3TableIntegration` is driven with the same ARNs in
//! [`crate::s3_table_source_validation`], which pins the classes IT records.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.ListSourcesForS3TableIntegration` through the router that
//! mounts it — so a response that is not computed by the service cannot pass.

use serde_json::{json, Value};

use crate::harness::{seed_s3_table_integration, Logs, REGION};

/// The operation under test.
const LIST: &str = "ListSourcesForS3TableIntegration";

/// The one class this reader answers an `integrationArn` it cannot resolve under.
const VALIDATION: &str = "ValidationException";

/// The account every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The sentence for an ARN whose RESOURCE is not an S3 Table Integration's: the form one
/// carries, then the ARN the request gave.
fn arn_form_refusal(arn: &str) -> String {
    format!(
        "Invalid S3 table integration ARN format. Expected: s3tableintegration/\
         {{integration-id}}: {arn}"
    )
}

/// The sentence for an ARN that could name no integration this caller holds at all.
fn unreachable_arn_refusal(arn: &str) -> String {
    format!("Invalid integration ARN: {arn}")
}

/// The sentence for an ARN naming another ACCOUNT's integration.
fn cross_account_refusal(arn: &str) -> String {
    format!("Cross account access not supported for S3 Table Integrations: {arn}")
}

/// The sentence for a handle of the right form the account holds no integration under.
fn not_found_refusal(arn: &str) -> String {
    format!("Integration not found: {arn}")
}

/// RULE: every rung of this reader's ladder is answered HTTP 400 `ValidationException`, with
/// the sentence that rung words.
#[tokio::test]
async fn every_unresolvable_integration_arn_is_refused_one_class() {
    let logs = Logs::new();

    // The control: one integration the account DOES hold, which the same reader serves.
    let held = seed_s3_table_integration(&logs, REGION, "reader-class-held");
    let (status, page) = logs.call(LIST, &json!({"integrationArn": held})).await;
    assert_eq!(
        status, 200,
        "case \"an integration the account holds\": expected HTTP 200, got {status} {page}"
    );
    assert!(
        page.get("sources").and_then(Value::as_array).is_some(),
        "case \"an integration the account holds\": expected a sources list, got {page}"
    );

    let another_account =
        format!("arn:aws:observabilityadmin:{REGION}:123456789012:s3tableintegration/x");
    let unheld = crate::harness::s3_table_integration_arn(REGION, "reader-class-unheld");
    let logs_integration = format!("arn:aws:logs:{REGION}:{ACCOUNT}:integration:opensearch-probe");
    // (case, the ARN, the sentence that rung words)
    let rows: [(&str, String, String); 8] = [
        (
            "another partition",
            format!("arn:aws-cn:observabilityadmin:{REGION}:{ACCOUNT}:s3tableintegration/x"),
            unreachable_arn_refusal(&format!(
                "arn:aws-cn:observabilityadmin:{REGION}:{ACCOUNT}:s3tableintegration/x"
            )),
        ),
        (
            "another service",
            format!("arn:aws:logs:{REGION}:{ACCOUNT}:s3tableintegration/x"),
            unreachable_arn_refusal(&format!(
                "arn:aws:logs:{REGION}:{ACCOUNT}:s3tableintegration/x"
            )),
        ),
        (
            "another Region",
            format!("arn:aws:observabilityadmin:ap-south-1:{ACCOUNT}:s3tableintegration/x"),
            unreachable_arn_refusal(&format!(
                "arn:aws:observabilityadmin:ap-south-1:{ACCOUNT}:s3tableintegration/x"
            )),
        ),
        (
            "another account",
            another_account.clone(),
            cross_account_refusal(&another_account),
        ),
        (
            "an integration of this service's own family",
            logs_integration.clone(),
            arn_form_refusal(&logs_integration),
        ),
        (
            "another resource type of the owning service",
            format!("arn:aws:observabilityadmin:{REGION}:{ACCOUNT}:telemetry-rule/probe"),
            arn_form_refusal(&format!(
                "arn:aws:observabilityadmin:{REGION}:{ACCOUNT}:telemetry-rule/probe"
            )),
        ),
        (
            "the resource prefix with no integration id",
            format!("arn:aws:observabilityadmin:{REGION}:{ACCOUNT}:s3tableintegration/"),
            arn_form_refusal(&format!(
                "arn:aws:observabilityadmin:{REGION}:{ACCOUNT}:s3tableintegration/"
            )),
        ),
        (
            "a handle the account holds no integration under",
            unheld.clone(),
            not_found_refusal(&unheld),
        ),
    ];

    for (case, arn, expected) in &rows {
        let (status, answered) = logs.call(LIST, &json!({"integrationArn": arn})).await;
        assert_eq!(
            status, 400,
            "case {case:?} ({arn}): expected HTTP 400, got {status} {answered}"
        );
        let code = crate::harness::error_code(&answered);
        assert_eq!(
            code, VALIDATION,
            "case {case:?} ({arn}): expected {VALIDATION}, got {code} / {answered}"
        );
        assert_eq!(
            answered.get("message").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?} ({arn}): expected {expected:?}, got {answered}"
        );
    }

    // The control, after every refusal: the integration the account holds is still served, so
    // the refusals above are about the ARN each one carried.
    let (status, page) = logs.call(LIST, &json!({"integrationArn": held})).await;
    assert_eq!(
        status, 200,
        "case \"the held integration after every refusal\": expected HTTP 200, got {status} \
         {page}"
    );
}

/// RULE (the same rung, read twice): an ARN of this service's OWN `integration:` resource is
/// told the resource form an S3 Table Integration's ARN carries whether or not the account
/// holds an integration under that name — this reader resolves no integration of that family,
/// so what the account holds there cannot change its answer.
#[tokio::test]
async fn an_integration_arn_of_this_service_is_told_the_expected_resource_form() {
    let logs = Logs::new();
    // The ids differ in form (a word, a hyphenated name, a hexadecimal token), which is
    // everything the resource segment of such an ARN varies in.
    for name in ["opensearch", "logs-to-opensearch-2", "9f0a1b2c3d4e5f60"] {
        let arn = format!("arn:aws:logs:{REGION}:{ACCOUNT}:integration:{name}");
        let (status, answered) = logs.call(LIST, &json!({"integrationArn": arn})).await;
        let expected = arn_form_refusal(&arn);
        let code = crate::harness::error_code(&answered);
        assert_eq!(
            (
                status,
                code.as_str(),
                answered.get("message").and_then(Value::as_str)
            ),
            (400, VALIDATION, Some(expected.as_str())),
            "case {name:?}: expected 400 {VALIDATION} {expected:?}, got {status} {answered}"
        );
    }
}
