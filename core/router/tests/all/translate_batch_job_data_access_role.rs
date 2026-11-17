// SPDX-License-Identifier: CC0-1.0

//! Amazon Translate's data-access-role check on a batch translation job, across
//! the two services it spans.
//!
//! RULE: a batch translation job RUNS AS the role the submission hands the
//! service (`StartTextTranslationJob$DataAccessRoleArn` is "the Amazon Resource
//! Name (ARN) of an AWS Identity Access and Management (IAM) role that grants
//! Amazon Translate read access to your input data"), so the role is resolved
//! against this account's IAM records BEFORE the job is recorded: a role those
//! records HOLD whose stored trust policy does not let
//! `translate.amazonaws.com` assume it is refused `InvalidRequestException`
//! (HTTP 400) stating which role was refused and what to change about it — and
//! no job comes into existence, so the read that follows reports the job absent.
//! A role whose trust policy does admit this service is submitted as before,
//! and so is one those records say nothing about: the refusal is grounded in
//! what IAM answered, and a data-access role is routinely provisioned outside
//! this emulator.
//!
//! Only a cross-service suite can pin this: the role and its trust policy are
//! IAM's and the job is Amazon Translate's, so the check is observable only
//! where both are mounted on one engine and the role is created through the real
//! `CreateRole` — what the check reads is then the document IAM persisted rather
//! than a copy.
//!
//! The rows differ along what the rule generalises over: the form of the trust
//! policy (which service principal it names, whether it names several, how the
//! principal and the action are written, which action is named, which kind of
//! principal is named, and whether the grant is an `Allow` with or without an
//! explicit `Deny` beside it), and along whether this account's IAM holds the
//! role the submission names in the first place.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder that builds the form body of an awsQuery call and the
// reader that pulls one element's text out of the XML answer are the test
// crate's own, written once in `all/query_wire.rs`; the two readers of an
// awsJson error envelope are the ones the sibling role-trust suites already
// expose, so this suite adds no second copy of either.
use crate::query_wire::{element, form_encode};
use crate::sagemaker_create_admission::{error_code, error_message};

/// The Region every call below is signed for, and the one the submission's job
/// id is derived in.
const REGION: &str = "us-east-1";

/// The account every emulated resource belongs to.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The `X-Amz-Target` prefix this service's operations are named under.
const TARGET_PREFIX: &str = "AWSShineFrontendService_20170701";

/// The service principal Amazon Translate assumes a job's data-access role as,
/// as the crate states it.
const SERVICE_PRINCIPAL: &str =
    service_com_amazonaws_translate_awsshinefrontendservice_20170701::state::batch_translation::JOB_SERVICE_PRINCIPAL;

/// One emulator carrying both services over ONE shared engine, exactly as the
/// production router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_translate_awsshinefrontendservice_20170701::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// One awsQuery call to IAM, answered with the HTTP status and the XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    (status, body)
}

/// One awsJson1_1 call to Amazon Translate, answered with the HTTP status and
/// the JSON body.
async fn translate(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("translate.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/translate/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=abc"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the test body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The code a SERVED call carries, which is none: the two readers above report
/// an envelope that names no error as the empty string.
const NO_ERROR: &str = "";

/// A trust policy naming `principals` in one `Allow` statement that grants the
/// assume.
fn trust_allowing(principals: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principals },
            "Action": "sts:AssumeRole"
        }]
    })
    .to_string()
}

/// Create a role in IAM carrying `document` and hand back its ARN.
async fn role_with_trust(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", document)],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateRole {name:?}: expected the role to be persisted, got {status}: {created}"
    );
    element(&created, "Arn")
}

/// A submission under `token` that runs as `role_arn`, with everything else a
/// well-formed batch job needs.
fn start_body(token: &str, role_arn: &str) -> Value {
    json!({
        "ClientToken": token,
        "DataAccessRoleArn": role_arn,
        "SourceLanguageCode": "en",
        "TargetLanguageCodes": ["fr"],
        "InputDataConfig": {
            "S3Uri": "s3://translate-batch-documents/input/",
            "ContentType": "text/plain",
        },
        "OutputDataConfig": {"S3Uri": "s3://translate-batch-documents/output/"},
    })
}

/// AWS's sentence for a role this service cannot assume: the two fixed
/// sentences, pinned here as literals, with the role ARN the REQUEST named
/// interpolated between them — the one variable part of the wording.
fn unassumable_sentence(role_arn: &str) -> String {
    format!(
        "Translate is not authorized to assume role: {role_arn}. \
         Please update the role's trust policy."
    )
}

/// The sentence a batch-job read states for a job this account holds none of,
/// pinned here as the literal it is: it has no variable part at all.
fn job_not_found_sentence() -> String {
    "Request failed due to job not found.".to_string()
}

/// Assert that the submission under `token` recorded NO job: the id that
/// submission's token mints is one the read reports absent.
///
/// The id is derived from the token and the Region
/// (`state::batch_translation::job_id`), so a refused submission is observable
/// as the absence of exactly that job rather than as the absence of some id the
/// test made up.
async fn assert_no_job(router: &MultiServiceRouter, case: &str, token: &str) {
    let job_id =
        service_com_amazonaws_translate_awsshinefrontendservice_20170701::state::batch_translation::job_id(
            REGION, token,
        );
    let (status, payload) = translate(
        router,
        "DescribeTextTranslationJob",
        json!({ "JobId": job_id }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: a refused submission must leave no job to describe, got {status} {payload}"
    );
    assert_eq!(
        error_code(&payload),
        "ResourceNotFoundException",
        "case {case:?}: expected the read to report the job missing, got {payload}"
    );
    assert_eq!(
        error_message(&payload),
        job_not_found_sentence(),
        "case {case:?}: expected the job-not-found sentence, got {payload}"
    );
}

/// One row: a trust policy, and whether a job that runs as a role carrying it
/// is submitted.
struct Row {
    case: &'static str,
    document: String,
    accepted: bool,
}

#[tokio::test]
async fn start_text_translation_job_refuses_a_role_whose_trust_policy_withholds_translate() {
    let router = emulator();

    let rows = [
        Row {
            case: "translate alone",
            document: trust_allowing(&[SERVICE_PRINCIPAL]),
            accepted: true,
        },
        Row {
            case: "translate among several services",
            document: trust_allowing(&["ec2.amazonaws.com", SERVICE_PRINCIPAL]),
            accepted: true,
        },
        Row {
            case: "translate named as a single string rather than a list",
            document: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": SERVICE_PRINCIPAL },
                    "Action": "sts:AssumeRole"
                }]
            })
            .to_string(),
            accepted: true,
        },
        Row {
            case: "translate allowed through an sts wildcard",
            document: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": SERVICE_PRINCIPAL },
                    "Action": "sts:*"
                }]
            })
            .to_string(),
            accepted: true,
        },
        Row {
            case: "another service only",
            document: trust_allowing(&["lambda.amazonaws.com"]),
            accepted: false,
        },
        Row {
            case: "translate denied",
            document: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "Service": SERVICE_PRINCIPAL },
                    "Action": "sts:AssumeRole"
                }]
            })
            .to_string(),
            accepted: false,
        },
        Row {
            // IAM's deny precedence: an explicit `Deny` wins over any `Allow`.
            case: "translate both allowed and denied",
            document: json!({
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Principal": { "Service": SERVICE_PRINCIPAL },
                        "Action": "sts:AssumeRole"
                    },
                    {
                        "Effect": "Deny",
                        "Principal": { "Service": SERVICE_PRINCIPAL },
                        "Action": "sts:AssumeRole"
                    }
                ]
            })
            .to_string(),
            accepted: false,
        },
        Row {
            // The statement names translate, but for an action that is not
            // assuming the role, so it grants nothing: a trust policy governs
            // `sts:AssumeRole`.
            case: "translate allowed an action that is not assuming the role",
            document: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": SERVICE_PRINCIPAL },
                    "Action": "s3:GetObject"
                }]
            })
            .to_string(),
            accepted: false,
        },
        Row {
            case: "an account principal rather than a service one",
            document: json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
                    "Action": "sts:AssumeRole"
                }]
            })
            .to_string(),
            accepted: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let role_arn =
            role_with_trust(&router, &format!("translate-trust-{index}"), &row.document).await;
        let token = format!("trust-job-{index}");

        let (status, payload) = translate(
            &router,
            "StartTextTranslationJob",
            start_body(&token, &role_arn),
        )
        .await;
        if row.accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: a role this service may assume must be accepted, got {status} \
                 {payload}"
            );
            assert!(
                payload
                    .get("JobId")
                    .and_then(Value::as_str)
                    .is_some_and(|job_id| !job_id.is_empty()),
                "case {case:?}: an accepted submission mints a job id, got {payload}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: a role this service cannot assume must be refused, got {status} \
             {payload}"
        );
        assert_eq!(
            error_code(&payload),
            "InvalidRequestException",
            "case {case:?}: expected the modelled client-request refusal, got {payload}"
        );
        assert_eq!(
            error_message(&payload),
            unassumable_sentence(&role_arn),
            "case {case:?}: expected the trust-policy sentence naming {role_arn}, got {payload}"
        );
        assert_no_job(&router, case, &token).await;
    }
}

#[tokio::test]
async fn start_text_translation_job_resolves_the_role_against_this_accounts_records() {
    let router = emulator();

    // The rows are role ARNs that satisfy `DataAccessRoleArn`'s modelled pattern
    // and name no role this account's IAM holds — a name nothing was created
    // under, one under a path, and one that is a prefix of a role that does
    // exist. The refusal has to be GROUNDED in what those records say, and they
    // say nothing about such a role: a data-access role is routinely
    // provisioned outside this emulator, so its absence here is evidence about
    // this store rather than about the role, and the submission stands.
    role_with_trust(&router, "held-role", &trust_allowing(&[SERVICE_PRINCIPAL])).await;
    for (index, (case, role_arn)) in [
        (
            "a role nothing was created under",
            format!("arn:aws:iam::{ACCOUNT}:role/does-not-exist"),
        ),
        (
            "a role under a path",
            format!("arn:aws:iam::{ACCOUNT}:role/service-role/does-not-exist"),
        ),
        (
            "a role name that is a prefix of one that exists",
            format!("arn:aws:iam::{ACCOUNT}:role/held-rol"),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let token = format!("absent-role-job-{index}");
        let (status, payload) = translate(
            &router,
            "StartTextTranslationJob",
            start_body(&token, &role_arn),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a role these records say nothing about is no refusal, got {status} \
             {payload}"
        );
        assert_eq!(
            error_code(&payload),
            NO_ERROR,
            "case {case:?}: the submission names no error, got {payload}"
        );
    }

    // And the role this account DOES hold whose trust policy withholds this
    // service: the one case those records settle, refused with the sentence
    // naming it.
    let case = "a held role whose trust policy withholds this service";
    let withheld = role_with_trust(
        &router,
        "withheld-role",
        &trust_allowing(&["lambda.amazonaws.com"]),
    )
    .await;
    let (status, payload) = translate(
        &router,
        "StartTextTranslationJob",
        start_body("withheld-role-job", &withheld),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the refusal, got {status} {payload}"
    );
    assert_eq!(
        error_message(&payload),
        unassumable_sentence(&withheld),
        "case {case:?}: expected the trust-policy sentence naming {withheld}, got {payload}"
    );
    assert_no_job(&router, case, "withheld-role-job").await;

    // The positive control: the same submission naming the role this account
    // holds and this service may assume is accepted.
    let held = format!("arn:aws:iam::{ACCOUNT}:role/held-role");
    let (status, payload) = translate(
        &router,
        "StartTextTranslationJob",
        start_body("held-role-job", &held),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the role this account holds\": the submission must be accepted, got {status} \
         {payload}"
    );
    assert_eq!(
        error_code(&payload),
        NO_ERROR,
        "case \"the role this account holds\": an accepted submission names no error, got \
         {payload}"
    );
}
