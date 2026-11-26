// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ValidatePipeline`, with IAM mounted beside this service on one
//! engine — the shape `core/router/src/lib.rs` mounts them in.
//!
//! The RULE asserted below: `ValidatePipeline` "checks whether an OpenSearch Ingestion
//! pipeline configuration is valid prior to creation" (the operation's own documentation), and
//! it applies the SAME judgement to a submitted `PipelineConfigurationBody` that a create
//! applies — a configuration reaches other Amazon Web Services resources under the roles its
//! `sts_role_arn` options name, so a body naming a role this service cannot take on (the
//! account's IAM holds no role under that name, or the trust policy it carries does not admit
//! the ingestion service principal) is reported invalid, with one `ValidationMessage` per such
//! role naming the role the request submitted, while a body whose roles it can take on is
//! reported valid and carries no error list. A value naming no role of THIS account
//! establishes nothing about the role, so it is not reported.
//!
//! The verdict is a REPORT: it creates nothing, and it agrees with what a create of the same
//! body answers — the last test asserts exactly that, which is the reason the two operations
//! share one judgement rather than carrying one each.
//!
//! The rows differ along what the rule generalises over: whether the account holds the role,
//! what its trust document says, which account the value names, where in the document the
//! option sits, how many roles one document names, and whether it names one at all.
//!
//! The harness — the router with IAM beside this service, the role setup over IAM's own wire,
//! the configuration bodies and the wording under test — is the suite's shared one, in
//! [`crate::configuration_role_validation`], so this suite and the create's cannot drift.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::configuration_role_validation::{
    call, create_pipeline, expected_message, given_role, list_pipeline_names, new_router,
    role_arn, sink_role_body, source_role_body, trust, two_role_body, unable_to_assume, Answer,
    ACCOUNT, INGESTION_PRINCIPAL, OTHER_ACCOUNT,
};

async fn validate_pipeline(router: &MultiServiceRouter, configuration_body: &str) -> Answer {
    call(
        router,
        "POST",
        "/2022-01-01/osis/validatePipeline",
        Some(json!({ "PipelineConfigurationBody": configuration_body })),
    )
    .await
}

/// The messages a validation answered, in the order it reported them.
fn reported_messages(case: &str, answered: &Answer) -> Vec<String> {
    answered.body["Errors"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|reported| {
            reported["Message"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no Message in {reported:?}"))
                .to_string()
        })
        .collect()
}

/// What the account's IAM holds for the role a row's configuration names.
enum Held {
    /// IAM holds no role under that name.
    Nothing,
    /// IAM holds the role, trusting this principal under this effect.
    Trusting(&'static str, &'static str),
}

/// One row of the validation table.
struct Row {
    case: &'static str,
    /// The role name the configuration body names.
    role: &'static str,
    /// The account the configuration's role ARN names.
    account: &'static str,
    /// What IAM holds for it.
    held: Held,
    /// Which part of the document names the role.
    body: fn(&str) -> String,
    /// Whether the configuration is expected to be reported invalid.
    invalid: bool,
}

/// A configuration is valid exactly where the roles it names are ones this service can take
/// on, and an invalid one is reported with one message per role it cannot.
#[tokio::test]
async fn a_configuration_is_valid_where_the_roles_it_names_can_be_assumed() {
    let rows = [
        Row {
            case: "a role this account does not hold",
            role: "osis-validate-absent-role",
            account: ACCOUNT,
            held: Held::Nothing,
            body: sink_role_body,
            invalid: true,
        },
        Row {
            case: "a role whose trust policy admits the ingestion service principal",
            role: "osis-validate-trusted-role",
            account: ACCOUNT,
            held: Held::Trusting("Allow", INGESTION_PRINCIPAL),
            body: sink_role_body,
            invalid: false,
        },
        Row {
            case: "the same trust, with the role named by the SOURCE's aws container",
            role: "osis-validate-trusted-source-role",
            account: ACCOUNT,
            held: Held::Trusting("Allow", INGESTION_PRINCIPAL),
            body: source_role_body,
            invalid: false,
        },
        Row {
            case: "a role whose trust policy admits another service",
            role: "osis-validate-other-principal-role",
            account: ACCOUNT,
            held: Held::Trusting("Allow", "lambda.amazonaws.com"),
            body: sink_role_body,
            invalid: true,
        },
        Row {
            case: "a role whose trust policy denies the ingestion service principal",
            role: "osis-validate-denied-role",
            account: ACCOUNT,
            held: Held::Trusting("Deny", INGESTION_PRINCIPAL),
            body: sink_role_body,
            invalid: true,
        },
        Row {
            case: "a role of another account, which this emulator's IAM does not answer for",
            role: "osis-validate-other-account-role",
            account: OTHER_ACCOUNT,
            held: Held::Nothing,
            body: sink_role_body,
            invalid: false,
        },
    ];

    for row in rows {
        let router = new_router();
        let arn = match row.held {
            Held::Nothing => role_arn(row.account, row.role),
            Held::Trusting(effect, principal) => {
                given_role(&router, row.case, row.role, &trust(effect, principal)).await
            }
        };
        let reported = validate_pipeline(&router, &(row.body)(&arn)).await;
        assert_eq!(
            reported.status, 200,
            "case {:?}: a validation is answered, got {} / {:?}",
            row.case, reported.status, reported.body
        );
        assert_eq!(
            reported.body["isValid"].as_bool(),
            Some(!row.invalid),
            "case {:?}: expected isValid {}, got {:?}",
            row.case,
            !row.invalid,
            reported.body
        );
        let messages = reported_messages(row.case, &reported);
        let expected: Vec<String> = if row.invalid {
            vec![unable_to_assume(&arn)]
        } else {
            Vec::new()
        };
        assert_eq!(
            messages, expected,
            "case {:?}: expected {expected:?}, got {messages:?}",
            row.case
        );
        if !row.invalid {
            assert!(
                reported.body.get("Errors").is_none(),
                "case {:?}: a valid configuration carries no error list, got {:?}",
                row.case,
                reported.body
            );
        }
    }
}

/// A document naming SEVERAL roles is reported for the ones this service cannot assume, in
/// the order the document names them, and exactly those.
#[tokio::test]
async fn a_configuration_naming_several_roles_reports_the_ones_it_cannot_assume() {
    let case = "a source role IAM holds and a sink role it does not";
    let router = new_router();
    let source_arn = given_role(
        &router,
        case,
        "osis-validate-source-role",
        &trust("Allow", INGESTION_PRINCIPAL),
    )
    .await;
    let sink_arn = role_arn(ACCOUNT, "osis-validate-missing-sink-role");

    let reported = validate_pipeline(&router, &two_role_body(&source_arn, &sink_arn)).await;
    assert_eq!(
        reported.body["isValid"].as_bool(),
        Some(false),
        "case {case:?}: expected isValid false, got {:?}",
        reported.body
    );
    let messages = reported_messages(case, &reported);
    assert_eq!(
        messages,
        vec![unable_to_assume(&sink_arn)],
        "case {case:?}: only the role it cannot assume is reported, got {messages:?}"
    );

    let case = "two roles the account holds nothing under";
    let first = role_arn(ACCOUNT, "osis-validate-missing-first-role");
    let second = role_arn(ACCOUNT, "osis-validate-missing-second-role");
    let reported = validate_pipeline(&router, &two_role_body(&first, &second)).await;
    let messages = reported_messages(case, &reported);
    assert_eq!(
        messages,
        vec![unable_to_assume(&first), unable_to_assume(&second)],
        "case {case:?}: both roles are reported in document order, got {messages:?}"
    );

    let case = "a configuration naming no role";
    let reported = validate_pipeline(
        &router,
        "version: \"2\"\nlog-pipeline:\n  source:\n    http:\n      path: \"/ingest\"\n  \
         sink:\n    - s3:\n        bucket: \"records\"\n",
    )
    .await;
    assert_eq!(
        reported.body["isValid"].as_bool(),
        Some(true),
        "case {case:?}: a document submitting no role to resolve is valid, got {:?}",
        reported.body
    );

    // A body that is no configuration document at all submits no role to resolve either, so
    // it is reported the same way. This is the emulator's reading, not AWS's: the service's
    // own wording for a configuration whose SYNTAX does not parse is in neither the model nor
    // its documentation, so no refusal is invented for it here, and the module header of
    // `src/ops/validate_pipeline.rs` records that the judgement is the role resolution.
    let case = "a body that is no configuration document";
    let reported = validate_pipeline(&router, "not: [a, pipeline\n").await;
    assert_eq!(
        reported.body["isValid"].as_bool(),
        Some(true),
        "case {case:?}: expected the role judgement to report nothing, got {:?}",
        reported.body
    );
}

/// The verdict a validation reports and the answer a create of the same body gives are one
/// judgement: a body reported invalid is refused by the create, naming the same roles, and a
/// body reported valid creates a pipeline. Validating itself creates nothing.
#[tokio::test]
async fn the_validation_verdict_and_the_create_agree_on_the_same_body() {
    let router = new_router();
    let trusted = given_role(
        &router,
        "the create of a validated body",
        "osis-validate-agreeing-role",
        &trust("Allow", INGESTION_PRINCIPAL),
    )
    .await;
    let absent = role_arn(ACCOUNT, "osis-validate-agreeing-absent-role");

    let rows: [(&str, String, &str, bool); 2] = [
        (
            "a body whose role IAM holds, trusting the service",
            trusted,
            "agree-valid-pipeline",
            true,
        ),
        (
            "a body naming a role the account does not hold",
            absent,
            "agree-invalid-pipeline",
            false,
        ),
    ];
    for (case, arn, pipeline, expected_valid) in rows {
        let body = sink_role_body(&arn);
        let reported = validate_pipeline(&router, &body).await;
        assert_eq!(
            reported.body["isValid"].as_bool(),
            Some(expected_valid),
            "case {case:?}: expected isValid {expected_valid}, got {:?}",
            reported.body
        );

        // Nothing was created by validating: the name is free for the create below, and the
        // account listing reports no pipeline under it yet.
        let names = list_pipeline_names(&router).await;
        assert!(
            !names.iter().any(|name| name == pipeline),
            "case {case:?}: validating creates nothing, ListPipelines reported {names:?}"
        );

        let created = create_pipeline(&router, pipeline, &body).await;
        if expected_valid {
            assert_eq!(
                created.status, 200,
                "case {case:?}: a body reported valid is created, got {} / {:?}",
                created.status, created.body
            );
        } else {
            assert_eq!(
                created.status, 400,
                "case {case:?}: a body reported invalid is refused, got {} / {:?}",
                created.status, created.body
            );
            // The create's refusal carries the validation messages the report listed, in the
            // envelope naming the member that did not validate.
            assert_eq!(
                created.message(),
                expected_message(&[&arn]),
                "case {case:?}: the create must refuse with the reported messages, got {:?}",
                created.message()
            );
        }
    }
}
