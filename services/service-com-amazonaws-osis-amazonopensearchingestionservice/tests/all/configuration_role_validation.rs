// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the IAM roles a submitted pipeline configuration names, with IAM
//! mounted beside this service on one engine — the shape `core/router/src/lib.rs` mounts
//! them in.
//!
//! The RULE asserted below: a Data Prepper configuration reaches other Amazon Web Services
//! resources under the role its `sts_role_arn` options name, so an operation that submits a
//! `PipelineConfigurationBody` completes only where this service can take those roles on —
//! the account's IAM holds a role under that name and the trust policy it carries admits the
//! ingestion service principal. Where it does not, the call is refused 400
//! `ValidationException`, nothing is persisted, and the message states the member that did
//! not validate and one `ValidationMessage` per role, naming the role the request submitted.
//! A value naming no role of THIS account establishes nothing about the role, so it is not
//! refused.
//!
//! The rows differ along what the rule generalises over: whether the account holds the role,
//! what its trust document says, which account the value names, where in the document the
//! option sits, how many roles one document names, and whether it names one at all.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A Region that is NOT the emulator default, so nothing here passes because it was pinned
/// to the default one.
const REGION: &str = "eu-west-2";

/// The canonical emulator account id, which is the account IAM answers for.
const ACCOUNT: &str = "000000000000";

/// An account that is NOT this one: a role of another account is not this emulator's IAM to
/// resolve.
const OTHER_ACCOUNT: &str = "123456789012";

/// The service principal a role a pipeline names has to trust, as the refusal states it.
const INGESTION_PRINCIPAL: &str = "osis-pipelines.amazonaws.com";

/// What one call answered: the status, the modelled error code (empty on success) and the
/// parsed body.
#[derive(Debug)]
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

impl Answer {
    /// The `message` member of a refusal.
    fn message(&self) -> String {
        self.body["message"].as_str().unwrap_or_default().to_string()
    }
}

/// This service and IAM on ONE engine: the roles a configuration names are resolved against
/// the records IAM holds, so the test creates them through IAM's own wire.
fn new_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's own concern and is exercised elsewhere;
    // these cases are about this service's behaviour.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_osis_amazonopensearchingestionservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Answer {
    let payload = match body {
        Some(value) => bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize body")),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://osis.{REGION}.amazonaws.com{path}"))
        // The credential scope is how a handler learns the Region its ARNs name.
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/osis/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("build request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(['/', ':'])
                .next()
                .unwrap_or(value)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer {
        status,
        error_type,
        body,
    }
}

/// Percent-encode one form value the way an AWS SDK encodes an `awsQuery` body.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// One IAM call: IAM speaks `awsQuery`, so its parameters travel as a form body and its
/// answer comes back as XML.
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
        .header("host", format!("iam.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/iam/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("build request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    (status, body)
}

/// A trust document with one statement, as a client sends one.
fn trust(effect: &str, principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// Create a role called `name` carrying `document`, and answer the ARN IAM persisted.
async fn given_role(router: &MultiServiceRouter, case: &str, name: &str, document: &Value) -> String {
    let document = document.to_string();
    let (status, created) = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &document)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: setup CreateRole answered {status}: {created}"
    );
    let open = "<Arn>";
    let start = created
        .find(open)
        .unwrap_or_else(|| panic!("case {case:?}: no Arn in {created}"))
        + open.len();
    let end = created[start..]
        .find("</Arn>")
        .unwrap_or_else(|| panic!("case {case:?}: no Arn in {created}"))
        + start;
    created[start..end].to_string()
}

/// The ARN of a role of `account` called `name`, whether or not IAM holds it.
fn role_arn(account: &str, name: &str) -> String {
    format!("arn:aws:iam::{account}:role/{name}")
}

/// A configuration body whose SINK assumes `role_arn`.
fn sink_role_body(role_arn: &str) -> String {
    format!(
        "version: \"2\"\nlog-pipeline:\n  source:\n    http:\n      path: \"/ingest\"\n  \
         sink:\n    - opensearch:\n        hosts:\n          - \
         \"https://search-domain-endpoint.{REGION}.es.amazonaws.com\"\n        aws:\n          \
         sts_role_arn: \"{role_arn}\"\n          region: \"{REGION}\"\n        index: \"logs\"\n"
    )
}

/// A configuration body whose SOURCE assumes `role_arn`.
fn source_role_body(role_arn: &str) -> String {
    format!(
        "version: \"2\"\nlog-pipeline:\n  source:\n    s3:\n      codec:\n        newline: {{}}\n      \
         aws:\n        sts_role_arn: \"{role_arn}\"\n        region: \"{REGION}\"\n  \
         sink:\n    - s3:\n        bucket: \"records\"\n"
    )
}

/// A configuration body whose source and sink assume two DIFFERENT roles.
fn two_role_body(source_role_arn: &str, sink_role_arn: &str) -> String {
    format!(
        "version: \"2\"\nlog-pipeline:\n  source:\n    s3:\n      aws:\n        sts_role_arn: \
         \"{source_role_arn}\"\n  sink:\n    - s3:\n        bucket: \"records\"\n        aws:\n          \
         sts_role_arn: \"{sink_role_arn}\"\n"
    )
}

/// The refusal a configuration naming the unassumable roles `role_arns` is answered with.
///
/// Spelled out here rather than taken from the crate, so the wording itself is under test:
/// the member that did not validate, then one `ValidationMessage` per role with the role ARN
/// copied from the request as its only variable part.
fn expected_message(role_arns: &[&str]) -> String {
    let listed = role_arns
        .iter()
        .map(|role_arn| {
            format!(
                "ValidationMessage(message=Unable to assume the STS role {role_arn}. This role \
                 must contain a trust relationship with '{INGESTION_PRINCIPAL}'.)"
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("pipelineConfigurationBody is invalid with the following errors: [{listed}]")
}

async fn create_pipeline(router: &MultiServiceRouter, name: &str, body: &str) -> Answer {
    call(
        router,
        "POST",
        "/2022-01-01/osis/createPipeline",
        Some(json!({
            "PipelineName": name,
            "MinUnits": 1,
            "MaxUnits": 2,
            "PipelineConfigurationBody": body,
        })),
    )
    .await
}

async fn get_pipeline(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/2022-01-01/osis/getPipeline/{name}"),
        None,
    )
    .await
}

async fn list_pipeline_names(router: &MultiServiceRouter) -> Vec<String> {
    let listed = call(router, "GET", "/2022-01-01/osis/listPipelines", None).await;
    assert_eq!(
        listed.status, 200,
        "ListPipelines answered {} / {:?}",
        listed.status, listed.body
    );
    listed.body["Pipelines"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|summary| summary["PipelineName"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// What the account's IAM holds for the role a row's configuration names.
enum Held {
    /// IAM holds no role under that name.
    Nothing,
    /// IAM holds the role, carrying this trust document.
    Trusting(&'static str, &'static str),
}

/// One row of the create table.
struct Row {
    case: &'static str,
    /// The pipeline the row creates.
    pipeline: &'static str,
    /// The role name the configuration body names.
    role: &'static str,
    /// The account the configuration's role ARN names.
    account: &'static str,
    /// What IAM holds for it.
    held: Held,
    /// Which part of the document names the role.
    body: fn(&str) -> String,
    /// Whether the create is expected to be refused.
    refused: bool,
}

/// A create is refused exactly where the roles its configuration names are ones this service
/// cannot take on, and a refused create persists nothing.
#[tokio::test]
async fn a_create_is_refused_where_its_configuration_names_a_role_this_service_cannot_assume() {
    let rows = [
        Row {
            case: "a role this account does not hold",
            pipeline: "absent-role-pipeline",
            role: "osis-absent-role",
            account: ACCOUNT,
            held: Held::Nothing,
            body: sink_role_body,
            refused: true,
        },
        Row {
            case: "a role whose trust policy admits the ingestion service principal",
            pipeline: "trusted-role-pipeline",
            role: "osis-trusted-role",
            account: ACCOUNT,
            held: Held::Trusting("Allow", INGESTION_PRINCIPAL),
            body: sink_role_body,
            refused: false,
        },
        Row {
            case: "the same trust, with the role named by the SOURCE's aws container",
            pipeline: "trusted-source-pipeline",
            role: "osis-trusted-source-role",
            account: ACCOUNT,
            held: Held::Trusting("Allow", INGESTION_PRINCIPAL),
            body: source_role_body,
            refused: false,
        },
        Row {
            case: "a role whose trust policy admits another service",
            pipeline: "other-principal-pipeline",
            role: "osis-other-principal-role",
            account: ACCOUNT,
            held: Held::Trusting("Allow", "lambda.amazonaws.com"),
            body: sink_role_body,
            refused: true,
        },
        Row {
            case: "a role whose trust policy denies the ingestion service principal",
            pipeline: "denied-role-pipeline",
            role: "osis-denied-role",
            account: ACCOUNT,
            held: Held::Trusting("Deny", INGESTION_PRINCIPAL),
            body: sink_role_body,
            refused: true,
        },
        Row {
            case: "a role of another account, which this emulator's IAM does not answer for",
            pipeline: "other-account-pipeline",
            role: "osis-other-account-role",
            account: OTHER_ACCOUNT,
            held: Held::Nothing,
            body: sink_role_body,
            refused: false,
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
        let body = (row.body)(&arn);
        let created = create_pipeline(&router, row.pipeline, &body).await;

        if row.refused {
            assert_eq!(
                created.status, 400,
                "case {:?}: expected 400, got {} / {:?}",
                row.case, created.status, created.body
            );
            assert_eq!(
                created.error_type, "ValidationException",
                "case {:?}: expected ValidationException, got {:?}",
                row.case, created.error_type
            );
            assert_eq!(
                created.message(),
                expected_message(&[&arn]),
                "case {:?}: expected the configuration-body refusal, got {:?}",
                row.case,
                created.message()
            );
            // Nothing is persisted: neither a read of the name nor the account listing
            // reports a pipeline the create was refused for.
            let read = get_pipeline(&router, row.pipeline).await;
            assert_eq!(
                read.status, 404,
                "case {:?}: a refused create persists nothing, GetPipeline answered {} / {:?}",
                row.case, read.status, read.body
            );
            let listed = list_pipeline_names(&router).await;
            assert!(
                !listed.iter().any(|name| name == row.pipeline),
                "case {:?}: a refused create persists nothing, ListPipelines reported {listed:?}",
                row.case
            );
        } else {
            assert_eq!(
                created.status, 200,
                "case {:?}: expected 200, got {} / {:?}",
                row.case, created.status, created.body
            );
            // Read it back, so the accepted body is the one the record holds.
            let read = get_pipeline(&router, row.pipeline).await;
            assert_eq!(
                read.status, 200,
                "case {:?}: GetPipeline answered {} / {:?}",
                row.case, read.status, read.body
            );
            assert_eq!(
                read.body["Pipeline"]["PipelineConfigurationBody"].as_str(),
                Some(body.as_str()),
                "case {:?}: the stored body must be the submitted one, got {:?}",
                row.case,
                read.body["Pipeline"]["PipelineConfigurationBody"]
            );
        }
    }
}

/// A document naming SEVERAL roles is refused for the ones this service cannot assume, and
/// the refusal lists exactly those.
#[tokio::test]
async fn a_configuration_naming_several_roles_is_refused_for_the_ones_it_cannot_assume() {
    let case = "a source role IAM holds and a sink role it does not";
    let router = new_router();
    let source_arn = given_role(
        &router,
        case,
        "osis-source-role",
        &trust("Allow", INGESTION_PRINCIPAL),
    )
    .await;
    let sink_arn = role_arn(ACCOUNT, "osis-missing-sink-role");

    let refused = create_pipeline(
        &router,
        "two-role-pipeline",
        &two_role_body(&source_arn, &sink_arn),
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected 400, got {} / {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.message(),
        expected_message(&[&sink_arn]),
        "case {case:?}: only the role it cannot assume is listed, got {:?}",
        refused.message()
    );

    // Both roles unassumable: both are listed, in the order the document names them.
    let case = "two roles the account holds nothing under";
    let first = role_arn(ACCOUNT, "osis-missing-first-role");
    let second = role_arn(ACCOUNT, "osis-missing-second-role");
    let refused =
        create_pipeline(&router, "both-missing-pipeline", &two_role_body(&first, &second)).await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected 400, got {} / {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.message(),
        expected_message(&[&first, &second]),
        "case {case:?}: both roles are listed in document order, got {:?}",
        refused.message()
    );

    // A configuration naming no role at all submits nothing to resolve, so it is accepted.
    let case = "a configuration naming no role";
    let body = "version: \"2\"\nlog-pipeline:\n  source:\n    http:\n      path: \"/ingest\"\n  \
                sink:\n    - s3:\n        bucket: \"records\"\n";
    let created = create_pipeline(&router, "no-role-pipeline", body).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected 200, got {} / {:?}",
        created.status, created.body
    );
}

/// The same rule holds for an UPDATE, which submits the same member: a body naming a role
/// this service cannot assume is refused and leaves the pipeline's stored configuration
/// alone.
#[tokio::test]
async fn an_update_is_refused_where_its_configuration_names_a_role_it_cannot_assume() {
    let case = "an update to a body naming an absent role";
    let router = new_router();
    let good_arn = given_role(
        &router,
        case,
        "osis-update-role",
        &trust("Allow", INGESTION_PRINCIPAL),
    )
    .await;
    let stored_body = sink_role_body(&good_arn);
    let created = create_pipeline(&router, "update-role-pipeline", &stored_body).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: setup CreatePipeline answered {} / {:?}",
        created.status, created.body
    );

    let absent_arn = role_arn(ACCOUNT, "osis-absent-update-role");
    let refused = call(
        &router,
        "PUT",
        "/2022-01-01/osis/updatePipeline/update-role-pipeline",
        Some(json!({ "PipelineConfigurationBody": sink_role_body(&absent_arn) })),
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected 400, got {} / {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_type, "ValidationException",
        "case {case:?}: expected ValidationException, got {:?}",
        refused.error_type
    );
    assert_eq!(
        refused.message(),
        expected_message(&[&absent_arn]),
        "case {case:?}: expected the configuration-body refusal, got {:?}",
        refused.message()
    );
    let read = get_pipeline(&router, "update-role-pipeline").await;
    assert_eq!(
        read.body["Pipeline"]["PipelineConfigurationBody"].as_str(),
        Some(stored_body.as_str()),
        "case {case:?}: a refused update changes nothing, got {:?}",
        read.body["Pipeline"]["PipelineConfigurationBody"]
    );

    // A pipeline that does not exist is answered 404 whatever its body names, so the
    // not-found contract is not reworded by this rule.
    let case = "an update of a pipeline nothing was created under";
    let missing = call(
        &router,
        "PUT",
        "/2022-01-01/osis/updatePipeline/no-such-pipeline",
        Some(json!({ "PipelineConfigurationBody": sink_role_body(&absent_arn) })),
    )
    .await;
    assert_eq!(
        missing.status, 404,
        "case {case:?}: expected 404, got {} / {:?}",
        missing.status, missing.body
    );

    // And an update to a body whose roles ARE assumable is applied.
    let case = "an update to a body naming a role the account holds";
    let other_arn = given_role(
        &router,
        case,
        "osis-second-update-role",
        &trust("Allow", INGESTION_PRINCIPAL),
    )
    .await;
    let updated_body = source_role_body(&other_arn);
    let updated = call(
        &router,
        "PUT",
        "/2022-01-01/osis/updatePipeline/update-role-pipeline",
        Some(json!({ "PipelineConfigurationBody": updated_body.clone() })),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "case {case:?}: expected 200, got {} / {:?}",
        updated.status, updated.body
    );
    let read = get_pipeline(&router, "update-role-pipeline").await;
    assert_eq!(
        read.body["Pipeline"]["PipelineConfigurationBody"].as_str(),
        Some(updated_body.as_str()),
        "case {case:?}: the stored body must be the submitted one, got {:?}",
        read.body["Pipeline"]["PipelineConfigurationBody"]
    );
}
