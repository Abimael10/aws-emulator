// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::MediaConvert::CertificateAssociation`
//! deploys: the trust is really made in AWS Elemental MediaConvert's own control plane, the
//! stack publishes the certificate ARN its `Ref` returns, Certificate Manager reports the
//! certificate as in use by that service, and deleting the stack takes the trust away — while
//! a certificate Certificate Manager never issued fails the RESOURCE with that service's own
//! refusal and rolls the stack back.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare, and a
//! declaration of it is deployed by calling the service that OWNS the type — so what the stack
//! publishes is what that service answered, and what that service refuses is a FAILED RESOURCE
//! rather than a template-format error; a name that reference does not publish is still
//! refused `Template format error: Unrecognized resource types: [<types>]`.** Three crates
//! have to agree for that to hold (CloudFormation submits the template and makes the call,
//! MediaConvert serves it, Certificate Manager holds the certificate and reports who uses it),
//! so the suite lives here: a crate-local test can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the resource NAME inside one published namespace — every type the reference publishes
//!   under `AWS::MediaConvert::` is a row of the admission table, so the reported one is a row
//!   among its siblings rather than a special case;
//! * the OPERATION that submits the body — `CreateStack` and `CreateChangeSet` are both held
//!   to the catalogue — and the FORM it is written in, JSON and YAML;
//! * for the deploy table: how many associations one stack declares, and which published value
//!   the output reads (`Ref`, and the stack's own record of the physical id);
//! * for the refusal table: the CLASS of refusal MediaConvert answers — an ARN of the right
//!   form naming a certificate nobody issued (`NotFoundException`, 404), and a value that is no
//!   ACM certificate ARN at all (`BadRequestException`, 400) — each reported on the resource
//!   that failed;
//! * for the refusing half of admission: the near misses of the same namespace — a plural, a
//!   wrong case and an invented resource — so admitting the published set is not admitting
//!   `AWS::MediaConvert::<anything>`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_recognized, AWS_RESOURCE_TYPES,
};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{elements, form_encode, opt_element, outputs_of, physical_ids_of};

/// The Region every request here is signed for, and so the Region the stacks belong to and
/// the one the certificates are issued in.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// How long the suite goes on polling for an outcome the clock already owes, and how often: a
/// loaded machine can be slower than the emulator's own spans without the rule being any
/// different, so the deadline is generous and the wait is for the CONDITION.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLLS: usize = 400;

/// The `StackStatus` values the rule is stated in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// The namespace AWS Elemental MediaConvert's resource types are published under, which is the
/// `cloudFormationName` of its own service model.
const MEDIACONVERT_NAMESPACE: &str = "AWS::MediaConvert::";

/// The resource type this suite deploys.
const CERTIFICATE_ASSOCIATION: &str = "AWS::MediaConvert::CertificateAssociation";

/// Every resource type the "AWS resource and property types reference" publishes under
/// `AWS::MediaConvert::`.
///
/// Source: that reference's AWS Elemental MediaConvert section. This is the expectation side of
/// the emulator's catalogue: the first test holds the catalogue's own `AWS::MediaConvert::`
/// group to exactly this set, so a missing entry (the reported defect, which refused every
/// template declaring the certificate association) and a stray one both fail.
const PUBLISHED: &[&str] = &[
    // AssociateCertificate / DisassociateCertificate
    CERTIFICATE_ASSOCIATION,
    // CreateJobTemplate
    "AWS::MediaConvert::JobTemplate",
    // CreatePreset
    "AWS::MediaConvert::Preset",
    // CreateQueue
    "AWS::MediaConvert::Queue",
];

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for a type AWS publishes.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// The ARN that names the consumer an ACM report lists for a certificate the account's
/// MediaConvert service trusts: that service in the Region the association was made for, with
/// no resource name after it.
///
/// The partition, Region and account come from the request and the caller — never from a
/// recorded run — and `mediaconvert` is that model's own `awsService.arnNamespace`
/// (`smithy_query(query="service_info")` of the `mediaconvert` model).
fn mediaconvert_service_arn() -> String {
    format!("arn:aws:mediaconvert:{REGION}:{ACCOUNT}:service")
}

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
///
/// Request authentication is off, as in every other suite here; the in-process callback
/// CloudFormation makes signs its request either way (the credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_mediaconvert_mediaconvert::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_acm_certificatemanager::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation action over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Certificate Manager operation the way an SDK drives it: `POST /` with
/// `X-Amz-Target: CertificateManager.<Op>` over its awsJson1_1 wire.
async fn acm(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("acm.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CertificateManager.{operation}"))
        .header("authorization", authorization("acm"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Ask Certificate Manager for a certificate covering `domain_name`, and answer the ARN it
/// stored it under — a certificate that service really holds, which is what makes the
/// association's happy path the happy path.
async fn requested_certificate(router: &MultiServiceRouter, domain_name: &str) -> String {
    let (status, answered) = acm(
        router,
        "RequestCertificate",
        &json!({"DomainName": domain_name, "ValidationMethod": "DNS"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case RequestCertificate({domain_name:?}): expected HTTP 200, got {status} with \
         {answered}"
    );
    answered
        .get("CertificateArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case RequestCertificate({domain_name:?}): expected a CertificateArn in \
                    {answered}")
        })
        .to_string()
}

/// The resources Certificate Manager reports as USING `certificate_arn`
/// (`CertificateDetail$InUseBy`), in the order it reports them.
async fn in_use_by(router: &MultiServiceRouter, case: &str, certificate_arn: &str) -> Vec<String> {
    let (status, answered) = acm(
        router,
        "DescribeCertificate",
        &json!({"CertificateArn": certificate_arn}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeCertificate, got {status} with {answered}"
    );
    answered["Certificate"]["InUseBy"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected an InUseBy list, got {answered}"))
        .iter()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect()
}

/// Poll the stack until it reports `expected`, answering the status it reported and the
/// `DescribeStacks` body that reported it.
///
/// The wait is for the CONDITION — a status the clock already owes — rather than for a fixed
/// span, so the suite is as fast as the emulator and still correct on a loaded machine.
async fn wait_for_status(
    router: &MultiServiceRouter,
    stack: &str,
    expected: &str,
) -> (String, String) {
    let mut last = (String::new(), String::new());
    for _ in 0..POLLS {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        // A stack whose deletion completed is no longer resolvable by name, which is the
        // `DELETE_COMPLETE` a client reads as the delete having finished.
        if expected == DELETE_COMPLETE && status == 400 {
            return (DELETE_COMPLETE.to_string(), body);
        }
        if reported == expected {
            return (reported, body);
        }
        last = (reported, body);
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    last
}

/// Submit a template and answer the `DescribeStacks` body of the stack once it reports
/// `expected`.
async fn create_and_settle(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
    expected: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — {CERTIFICATE_ASSOCIATION} is a \
         type a template may declare — got {status} with body {body}"
    );
    assert!(
        !body.contains(UNRECOGNIZED_PREFIX),
        "case {case:?}: expected the type to be recognised, got {body}"
    );
    let (reached, described) = wait_for_status(router, stack_name, expected).await;
    assert_eq!(
        reached, expected,
        "case {case:?}: expected the stack to settle at {expected}, got {reached:?} with body \
         {described}"
    );
    described
}

/// The catalogue carries exactly the published AWS Elemental MediaConvert namespace — every
/// type the reference publishes under it, and nothing else — and each of them is recognised.
#[test]
fn the_catalogue_carries_the_published_mediaconvert_namespace() {
    let mut expected: Vec<&str> = PUBLISHED.to_vec();
    expected.sort_unstable();
    let mut actual: Vec<&str> = AWS_RESOURCE_TYPES
        .iter()
        .copied()
        .filter(|entry| entry.starts_with(MEDIACONVERT_NAMESPACE))
        .collect();
    actual.sort_unstable();
    assert_eq!(
        actual, expected,
        "case {MEDIACONVERT_NAMESPACE:?}: expected the catalogue to carry exactly the published \
         types {expected:?}, got {actual:?}"
    );
    for resource_type in PUBLISHED {
        assert!(
            is_recognized(resource_type),
            "case {resource_type:?}: expected a published type to be recognized, it is not"
        );
    }
}

/// Every published MediaConvert type may be declared by each operation that submits a template
/// for deployment, in either body form: `CreateStack` answers with the stack it created and the
/// stack is recorded, `CreateChangeSet` answers with the change set it created.
#[tokio::test]
async fn every_published_mediaconvert_type_may_be_declared() {
    let router = build_router();

    for resource_type in PUBLISHED {
        for (form, body) in [
            (
                "JSON",
                json!({ "Resources": { "Subject": { "Type": resource_type } } }).to_string(),
            ),
            (
                "YAML",
                ["Resources:", "  Subject:", &format!("    Type: {resource_type}"), ""].join("\n"),
            ),
        ] {
            let case = format!("{resource_type} declared in {form}");
            let resource = resource_type
                .rsplit("::")
                .next()
                .unwrap_or(resource_type)
                .to_ascii_lowercase();
            let stack_name = format!("mediaconvert-{}-{resource}", form.to_ascii_lowercase());

            let (status, answer) = cloudformation(
                &router,
                "CreateStack",
                &[("StackName", &stack_name), ("TemplateBody", &body)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from CreateStack for a published resource \
                 type, got {status} with body {answer}"
            );
            let stack_id = opt_element(&answer, "StackId").unwrap_or_default();
            assert!(
                stack_id.contains(&format!(":stack/{stack_name}/")),
                "case {case:?}: expected CreateStack to answer with the stack's id, got \
                 {stack_id:?} in {answer}"
            );

            // The stack really is there: the reported defect created none at all.
            let (status, described) =
                cloudformation(&router, "DescribeStacks", &[("StackName", &stack_name)]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected DescribeStacks to report the created stack, got \
                 {status} with body {described}"
            );

            let change_set_stack = format!("{stack_name}-change-set");
            let (status, answer) = cloudformation(
                &router,
                "CreateChangeSet",
                &[
                    ("StackName", &change_set_stack),
                    ("ChangeSetName", "a-change-set"),
                    ("ChangeSetType", "CREATE"),
                    ("TemplateBody", &body),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from CreateChangeSet for a published resource \
                 type, got {status} with body {answer}"
            );
            assert!(
                opt_element(&answer, "Id").is_some_and(|id| id.contains("changeSet/")),
                "case {case:?}: expected CreateChangeSet to answer with the change set's id, got \
                 body {answer}"
            );
        }
    }
}

/// A name the reference does NOT publish under this namespace is still refused, and the
/// refusal names exactly the types the body declared.
#[tokio::test]
async fn a_type_the_namespace_does_not_publish_is_still_refused() {
    let router = build_router();
    // The near misses of the same namespace: a plural, a wrong case, an invented resource, and
    // the service's signing name as the namespace.
    let unpublished = [
        "AWS::MediaConvert::CertificateAssociations",
        "AWS::MediaConvert::Certificateassociation",
        "AWS::MediaConvert::Certificate",
        "AWS::mediaconvert::CertificateAssociation",
    ];
    for (index, resource_type) in unpublished.into_iter().enumerate() {
        let case = format!("{resource_type} declared");
        assert!(
            !is_recognized(resource_type),
            "case {case:?}: expected an unpublished type to be unrecognized, it is not"
        );
        let body = json!({ "Resources": { "Subject": { "Type": resource_type } } }).to_string();
        let (status, answer) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &format!("mediaconvert-unpublished-{index}")),
                ("TemplateBody", &body),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for a type the reference does not publish, got \
             {status} with body {answer}"
        );
        assert!(
            answer.contains("ValidationError")
                && answer.contains(&format!("{UNRECOGNIZED_PREFIX}{resource_type}]")),
            "case {case:?}: expected the unrecognized-type refusal naming the declared type, \
             got {answer}"
        );
    }
}

/// One deploy row: a stack declaring one or more associations, and what it must publish.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// The domain each declared association's certificate covers, one per association, which
    /// is also the order the logical ids are generated in.
    domains: &'static [&'static str],
    /// Whether the body is written in YAML rather than JSON.
    yaml: bool,
}

const ROWS: &[Row] = &[
    // The issue's own stack: one association, with the `Ref` read back as an output, in JSON.
    Row {
        case: "one association, Ref read back, JSON spelling",
        stack_name: "mediaconvert-cert-assoc-round-trip",
        domains: &["player.example-widgets.test"],
        yaml: false,
    },
    // The same rule over a body in the other form, and over a stack declaring TWO associations:
    // the account trusts a SET of certificates, so each one has to be reported in use.
    Row {
        case: "two associations in one stack, YAML spelling",
        stack_name: "mediaconvert-cert-assoc-pair",
        domains: &["live.example-widgets.test", "vod.example-widgets.test"],
        yaml: true,
    },
];

/// The logical id of the `index`th association a row declares.
fn logical_id(index: usize) -> String {
    format!("CertAssoc{index}")
}

/// The template a row submits: one association per certificate ARN, each publishing its `Ref`
/// as an output of the same name.
fn template_of(row: &Row, certificate_arns: &[String]) -> String {
    if row.yaml {
        let mut lines = vec!["AWSTemplateFormatVersion: '2010-09-09'".to_string(), "Resources:".to_string()];
        for (index, arn) in certificate_arns.iter().enumerate() {
            lines.push(format!("  {}:", logical_id(index)));
            lines.push(format!("    Type: {CERTIFICATE_ASSOCIATION}"));
            lines.push("    Properties:".to_string());
            lines.push(format!("      Arn: {arn}"));
        }
        lines.push("Outputs:".to_string());
        for index in 0..certificate_arns.len() {
            lines.push(format!("  {}:", logical_id(index)));
            lines.push(format!("    Value: !Ref {}", logical_id(index)));
        }
        lines.push(String::new());
        return lines.join("\n");
    }
    let mut resources = serde_json::Map::new();
    let mut outputs = serde_json::Map::new();
    for (index, arn) in certificate_arns.iter().enumerate() {
        resources.insert(
            logical_id(index),
            json!({"Type": CERTIFICATE_ASSOCIATION, "Properties": {"Arn": arn}}),
        );
        outputs.insert(
            logical_id(index),
            json!({"Value": {"Ref": logical_id(index)}}),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// A stack declaring certificate associations deploys: each one is really made in MediaConvert
/// — which is what puts the account's MediaConvert service in Certificate Manager's own report
/// of who uses the certificate — the stack publishes the certificate ARN as the resource's
/// identity, and deleting the stack takes the trust away again.
#[tokio::test]
async fn a_stack_deploys_mediaconvert_certificate_associations() {
    for row in ROWS {
        let router = build_router();
        let case = row.case;
        let certificate_arns: Vec<String> = {
            let mut arns = Vec::new();
            for domain in row.domains {
                arns.push(requested_certificate(&router, domain).await);
            }
            arns
        };
        let template = template_of(row, &certificate_arns);

        // Before the stack, nothing uses the certificates.
        for arn in &certificate_arns {
            let using = in_use_by(&router, case, arn).await;
            assert!(
                using.is_empty(),
                "case {case:?}: expected {arn} to be used by nothing before the stack, got \
                 {using:?}"
            );
        }

        let described =
            create_and_settle(&router, case, row.stack_name, &template, CREATE_COMPLETE).await;

        // What the stack publishes is the certificate ARN: this type's `Ref` IS that ARN, and
        // it is the resource's physical id — the one handle either MediaConvert call takes.
        let outputs = outputs_of(&described);
        let expected_outputs: BTreeMap<String, String> = certificate_arns
            .iter()
            .enumerate()
            .map(|(index, arn)| (logical_id(index), arn.clone()))
            .collect();
        assert_eq!(
            outputs, expected_outputs,
            "case {case:?}: expected each Ref to be the declared certificate ARN, got \
             {outputs:?}"
        );
        let (status, resources) = cloudformation(
            &router,
            "DescribeStackResources",
            &[("StackName", row.stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             {resources}"
        );
        assert_eq!(
            physical_ids_of(&resources),
            expected_outputs,
            "case {case:?}: expected each resource's physical id to be its certificate ARN, got \
             {resources}"
        );

        // The trust really reached MediaConvert: Certificate Manager reports the account's
        // MediaConvert service as using each certificate, which only that service's own record
        // of the association can put there.
        for arn in &certificate_arns {
            let using = in_use_by(&router, case, arn).await;
            assert_eq!(
                using,
                vec![mediaconvert_service_arn()],
                "case {case:?}: expected {arn} to be reported in use by the account's \
                 MediaConvert service, got {using:?}"
            );
        }

        // Deleting the stack takes the association away, so the certificate is free again.
        let (status, deleted) =
            cloudformation(&router, "DeleteStack", &[("StackName", row.stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
        );
        let (reached, body) = wait_for_status(&router, row.stack_name, DELETE_COMPLETE).await;
        assert_eq!(
            reached, DELETE_COMPLETE,
            "case {case:?}: expected the stack to be deleted, got {reached:?} with body {body}"
        );
        for arn in &certificate_arns {
            let using = in_use_by(&router, case, arn).await;
            assert!(
                using.is_empty(),
                "case {case:?}: expected {arn} to be used by nothing once the stack is gone, \
                 got {using:?}"
            );
        }
    }
}

/// One refusal row: a declared `Arn` MediaConvert refuses, and what the failed resource must
/// report.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    /// The `Arn` the declaration states. A `{}` placeholder is replaced by the Region, so a row
    /// states the ARN form rather than a recorded value.
    declared_arn: &'static str,
    /// The FIXED text of the sentence MediaConvert words that class of refusal with, which the
    /// resource's failure reason has to carry.
    sentence: &'static str,
    /// The error code that service answers, which the SDK diagnostic in the reason names.
    code: &'static str,
    /// The HTTP status that error carries, from the model's own `@httpError`.
    status: u16,
    /// The `HandlerErrorCode` class the provider puts that refusal in.
    handler_error_code: &'static str,
}

const REFUSED: &[Refused] = &[
    // The issue's own refusal: an ARN of exactly the right form whose certificate Certificate
    // Manager never issued. The certificate is that service's resource, so MediaConvert
    // resolves it there and refuses the association for a reference that resolves to nothing.
    Refused {
        case: "a well-formed certificate ARN nobody issued",
        stack_name: "mediaconvert-cert-assoc-unknown",
        declared_arn: "arn:aws:acm:{}:000000000000:certificate/\
                       00000000-0000-0000-0000-000000000000",
        sentence: "The resource you specified in arn was not found.",
        code: "NotFoundException",
        status: 404,
        handler_error_code: "NotFound",
    },
    // Another id of the same form: the rule is about the certificate not being there, not about
    // one recorded id.
    Refused {
        case: "another certificate id of the same form",
        stack_name: "mediaconvert-cert-assoc-unknown-second",
        declared_arn: "arn:aws:acm:{}:000000000000:certificate/\
                       7d9f2c41-5b6e-4a18-9c03-2fe8b1a47d55",
        sentence: "The resource you specified in arn was not found.",
        code: "NotFoundException",
        status: 404,
        handler_error_code: "NotFound",
    },
    // A different CLASS of refusal: a value that is no ACM certificate ARN at all is a problem
    // in the request, which that service answers with its 400 rather than its 404.
    Refused {
        case: "an Arn that is not an ACM certificate ARN",
        stack_name: "mediaconvert-cert-assoc-not-a-certificate",
        declared_arn: "arn:aws:acm:{}:000000000000:key/7d9f2c41-5b6e-4a18-9c03-2fe8b1a47d55",
        sentence: "is not the ARN of an ACM certificate",
        code: "BadRequestException",
        status: 400,
        handler_error_code: "InvalidRequest",
    },
];

/// A declared certificate MediaConvert refuses fails the RESOURCE with that service's own
/// refusal, quoted in the handler-error reason its provider reports, and rolls the stack back —
/// never the template-format error a type CloudFormation does not know is refused with.
#[tokio::test]
async fn a_certificate_mediaconvert_refuses_rolls_the_stack_back() {
    for row in REFUSED {
        let router = build_router();
        let case = row.case;
        let declared_arn = row.declared_arn.replace("{}", REGION);
        let template = json!({
            "Resources": {
                "CertAssoc": {
                    "Type": CERTIFICATE_ASSOCIATION,
                    "Properties": {"Arn": declared_arn},
                }
            },
            "Outputs": {"CertArn": {"Value": {"Ref": "CertAssoc"}}}
        })
        .to_string();

        let described =
            create_and_settle(&router, case, row.stack_name, &template, ROLLBACK_COMPLETE).await;
        assert!(
            outputs_of(&described).is_empty(),
            "case {case:?}: expected no output for a resource nothing created, got {described}"
        );

        let (_, events) =
            cloudformation(&router, "DescribeStackEvents", &[("StackName", row.stack_name)]).await;
        assert!(
            !events.contains(UNRECOGNIZED_PREFIX),
            "case {case:?}: expected no template-format error for a type AWS publishes, got \
             {events}"
        );
        // The reason is the provider's report of the owning service's own refusal: that
        // service's sentence, the SDK's diagnostic of the call it made, and the
        // `HandlerErrorCode` class the refusal falls in.
        let reasons = elements(&events, "ResourceStatusReason");
        let expected_envelope = format!(
            "(Service: MediaConvert; Operation: AssociateCertificate; Status Code: {}; Error \
             Code: {};",
            row.status, row.code
        );
        let expected_suffix = format!("HandlerErrorCode: {}", row.handler_error_code);
        assert!(
            reasons.iter().any(|reason| reason.contains(row.sentence)
                && reason.contains("Resource handler returned message:")
                && reason.contains(&expected_envelope)
                && reason.contains(&expected_suffix)),
            "case {case:?}: expected a failure reason carrying {:?}, {expected_envelope:?} and \
             {expected_suffix:?}, got {reasons:?}",
            row.sentence
        );

        // And nothing was trusted: the rolled-back stack left no association behind, so no
        // resource of MediaConvert is reported as using the certificate the declaration named.
        let (status, answered) = acm(
            &router,
            "DescribeCertificate",
            &json!({"CertificateArn": declared_arn}),
        )
        .await;
        assert_ne!(
            status, 200,
            "case {case:?}: expected Certificate Manager to hold no certificate under the \
             declared ARN, got {answered}"
        );
    }
}
