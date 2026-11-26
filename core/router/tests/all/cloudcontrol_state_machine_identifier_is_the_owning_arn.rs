// SPDX-License-Identifier: CC0-1.0

//! The handle Cloud Control API hands back for a state machine it created, and what the
//! service that owns it then answers to.
//!
//! RULE: `ProgressEvent$Identifier` is "The primary identifier for the resource" — the
//! property the resource type's own registry schema points `primaryIdentifier` at — so for a
//! type whose schema points it at an ARN the OWNING service assigns, every answer reports
//! that ARN rather than any property the desired state carried: the fixed text of the ARN
//! with the partition and the account this emulator presents, the Region the call was signed
//! for, and the resource parts the desired state named. The resource model carries it under
//! that property, and it is the handle the owning service's own API resolves the resource by
//! — for `AWS::StepFunctions::StateMachine` the `stateMachineArn` of
//! `states:DescribeStateMachine`.
//!
//! The second rule, which the same flow goes on to exercise: **a property the type's schema
//! declares write-only may only be `add`ed — any other patch operation naming one is refused
//! for naming it, and a refused patch changes nothing the owning service holds** — so a
//! `replace` of the write-only `/Definition` is refused while the workflow and the revision
//! Step Functions holds stay exactly as they were, and the same change sent as an `add`
//! lands and is what that service then runs.
//!
//! Three crates have to agree for either rule to hold — IAM holds the execution role, Cloud
//! Control API carries out the resource operation, Step Functions holds the state machine and
//! answers for it — so the suite lives here rather than in any of them. Every assertion about
//! the resource is made against the OWNING service's own read, so nothing is confirmed from
//! the answer being checked.
//!
//! The rows differ along what the first rule generalises over: the REGION the create is
//! signed for (which is the Region the ARN carries), the form of the name the ARN is built
//! from, which of the two forms the schema carries the definition in the create states, and
//! which workflow type it names.
//!
//! Overlap, stated rather than hidden: the cloudcontrol crate's own `arn_assigned_identifiers`
//! suite holds the first rule for this type in ONE Region with a fabricated role ARN, and its
//! `patch_path_refusals` suite holds the second over a wider table of patch documents. What is
//! new here is that the three services are assembled as the router assembles them — the role
//! is one IAM really holds, the Region is the call's own rather than a single fixed one, and
//! the identifier is carried from the create straight into the owning service's API and back
//! into Cloud Control API, which is the flow a generic client performs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element reader this crate writes ONCE
// (`tests/all/query_wire.rs`), which every suite driving an awsQuery service reads through.
use super::query_wire::{element, form_encode};

/// The registry type under test.
const STATE_MACHINE: &str = "AWS::StepFunctions::StateMachine";

/// The property that type's registry schema points `primaryIdentifier` at
/// (`/properties/Arn`), which that schema also declares read-only — so no desired state
/// carries it and the owning service is what assigns it.
const IDENTIFIER_PROPERTY: &str = "Arn";

/// The property of that type a create SETS the workflow through, which the type's schema
/// declares write-only.
const DEFINITION: &str = "Definition";

/// The pointer a patch document names that property by.
const DEFINITION_POINTER: &str = "/Definition";

/// The property a READ reports the workflow under, which is the form the stored model carries.
const DEFINITION_STRING: &str = "DefinitionString";

/// The Region an IAM call is signed for: that service is global, so its ARNs carry none.
const IAM_REGION: &str = "us-east-1";

/// The API version an awsQuery call to IAM carries, as its model declares it.
const IAM_API_VERSION: &str = "2010-05-08";

/// The signing name Cloud Control API's model gives it, which is what routes a call to it.
const CLOUD_CONTROL_SIGNING_NAME: &str = "cloudcontrolapi";

/// The `X-Amz-Target` prefix a client writes for it: the local name of its Smithy service
/// shape.
const CLOUD_CONTROL_TARGET_PREFIX: &str = "CloudApiService";

/// How long a wait for a resource operation request to reach a terminal status runs before the
/// case fails.
///
/// An accepted request is answered in a non-terminal status and the clock owes it the rest of
/// the progression, so this is far above anything that flow takes — including on a host running
/// the whole workspace's tests at once. No case waits for a span: each re-reads until the
/// status it is after holds or this deadline passes.
const SETTLED_BUDGET: Duration = Duration::from_secs(120);

/// How often that wait re-reads.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The terminal `OperationStatus` values a resource operation request reports.
const STATUS_SUCCESS: &str = "SUCCESS";
const STATUS_FAILED: &str = "FAILED";
const STATUS_CANCEL_COMPLETE: &str = "CANCEL_COMPLETE";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other suite
/// here; the credential scope of each call is still what routes it and what each handler reads
/// its Region from.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudcontrol_cloudapiservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads the caller's Region from. The signature
/// is never checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one operation of an awsJson1_0 service over its own wire, signed for `region`.
async fn json_rpc(
    router: &MultiServiceRouter,
    region: &str,
    signing_name: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", target)
        .header("authorization", authorization(signing_name, region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, answered)
}

/// Call one Cloud Control API operation, signed for `region`.
async fn cloud_control(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    json_rpc(
        router,
        region,
        CLOUD_CONTROL_SIGNING_NAME,
        &format!("{CLOUD_CONTROL_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await
}

/// [`cloud_control`] where the call must be served.
async fn cloud_control_ok(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answered) = cloud_control(router, region, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with body {answered}"
    );
    answered
}

/// Call one Step Functions operation (`sigv4Name: states`, service shape `AWSStepFunctions`),
/// signed for `region`.
async fn sfn(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    json_rpc(
        router,
        region,
        "states",
        &format!("AWSStepFunctions.{operation}"),
        body,
    )
    .await
}

/// [`sfn`] where the call must be served.
async fn sfn_ok(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answered) = sfn(router, region, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with body {answered}"
    );
    answered
}

/// One awsQuery call to IAM, which must be served, answered with the XML body.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={IAM_API_VERSION}");
    for (name, value) in members {
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
        .header("authorization", authorization("iam", IAM_REGION))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from IAM {action}, got {status} with body {answered}"
    );
    answered
}

/// A string member of an answered document, or `""` when absent — so a missing member fails an
/// assertion rather than panicking the suite.
fn text(value: &Value, member: &str) -> String {
    value
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `ProgressEvent` a mutating Cloud Control operation's answer carries.
fn progress(answered: &Value) -> Value {
    answered["ProgressEvent"].clone()
}

/// The error SHAPE an awsJson error body names: the `__type` with the namespace that may
/// precede it and the URI that may follow it taken off, so a differently namespaced or
/// suffixed code is told from the modelled shape rather than matched by it.
fn shape_of(body: &Value) -> String {
    let rendered = text(body, "__type");
    let after_namespace = rendered
        .rsplit_once('#')
        .map_or(rendered.as_str(), |(_, shape)| shape);
    after_namespace
        .split_once(':')
        .map_or(after_namespace, |(shape, _)| shape)
        .to_string()
}

/// The `message` an error body carries, under either casing the envelope may use.
fn message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// Read the request `token` names until it reports a TERMINAL status, answering the
/// `ProgressEvent` it reports then — whichever status that is, so a failure is asserted on
/// rather than waited out.
async fn terminal(router: &MultiServiceRouter, region: &str, case: &str, token: &str) -> Value {
    let started = Instant::now();
    loop {
        let answered = cloud_control_ok(
            router,
            region,
            case,
            "GetResourceRequestStatus",
            &json!({ "RequestToken": token }),
        )
        .await;
        let event = progress(&answered);
        let reached = text(&event, "OperationStatus");
        if [STATUS_SUCCESS, STATUS_FAILED, STATUS_CANCEL_COMPLETE].contains(&reached.as_str()) {
            return event;
        }
        assert!(
            started.elapsed() < SETTLED_BUDGET,
            "case {case:?}: expected request {token:?} to reach a terminal status within \
             {SETTLED_BUDGET:?}, it is still {reached:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// [`terminal`], required to have SUCCEEDED.
async fn settled(router: &MultiServiceRouter, region: &str, case: &str, token: &str) -> Value {
    let event = terminal(router, region, case, token).await;
    assert_eq!(
        text(&event, "OperationStatus"),
        STATUS_SUCCESS,
        "case {case:?}: expected the resource operation to succeed, got {event}"
    );
    event
}

/// The resource model `GetResource` reports for one resource, parsed back from the JSON
/// document its `Properties` member carries.
async fn model(router: &MultiServiceRouter, region: &str, case: &str, identifier: &str) -> Value {
    let answered = cloud_control_ok(
        router,
        region,
        case,
        "GetResource",
        &json!({ "TypeName": STATE_MACHINE, "Identifier": identifier }),
    )
    .await;
    let rendered = text(&answered["ResourceDescription"], "Properties");
    serde_json::from_str(&rendered).unwrap_or_else(|_| {
        panic!("case {case:?}: expected a resource model document, got {answered}")
    })
}

/// What the OWNING service holds for the state machine `identifier` names: the workflow it runs
/// and the revision it is at, which is where a refused patch has to leave them.
async fn described(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    identifier: &str,
) -> (String, String) {
    let read = sfn_ok(
        router,
        region,
        case,
        "DescribeStateMachine",
        &json!({ "stateMachineArn": identifier }),
    )
    .await;
    (text(&read, "definition"), text(&read, "revisionId"))
}

/// The ARN Step Functions assigns the state machine `name` in `region`: the fixed text `arn:`,
/// `:states:` and `:stateMachine:` with the partition and the account this emulator presents,
/// the Region the call was signed for, and the name the desired state carried.
fn state_machine_arn(region: &str, name: &str) -> String {
    engine::emulator_arn("states", region, &format!("stateMachine:{name}"))
}

/// A trust policy naming the Step Functions service principal, which is what an execution role
/// of a state machine is trusted by.
fn states_trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "states.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// One execution role, written through IAM's OWN `CreateRole`, answered by the ARN that service
/// minted for it — so the role every create below names is one IAM really holds.
async fn execution_role(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = iam(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &states_trust_policy().to_string()),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// One Amazon States Language definition: a single `Pass` state that ends immediately, stamped
/// with `version` so two definitions of one row are told apart.
fn definition(version: u64) -> Value {
    json!({
        "StartAt": "P",
        "States": { "P": { "Type": "Pass", "Result": { "version": version }, "End": true } },
    })
}

/// One row of the table: the Region the create is signed for, the name it chooses, and the form
/// it states the workflow and the workflow type in.
struct Created {
    case: &'static str,
    /// The Region every call about this row is signed for, which is the Region its state
    /// machine lives in and the one its ARN carries.
    region: &'static str,
    /// The name the create chooses, which the ARN is built from.
    name: String,
    /// Whether the definition travels as the schema's write-only `Definition` object rather
    /// than as the `DefinitionString` a read reports.
    definition_as_object: bool,
    /// The workflow type the create names, or `None` where it names none and the owning
    /// service's own default applies.
    workflow_type: Option<&'static str>,
}

impl Created {
    /// The workflow type the state machine ends up with: the one the create names, or
    /// `STANDARD` — "the default is `STANDARD`" (`CreateStateMachineInput$type`) — for a create
    /// naming none.
    fn workflow_type_in_force(&self) -> &str {
        self.workflow_type.unwrap_or("STANDARD")
    }

    /// The desired state this row creates the state machine with. It carries NO `Arn`: the
    /// type's schema declares the primary identifier read-only, so the only resource part the
    /// caller names is the name.
    fn desired(&self, role_arn: &str) -> Value {
        let mut desired = json!({ "StateMachineName": self.name, "RoleArn": role_arn });
        if self.definition_as_object {
            desired[DEFINITION] = definition(1);
        } else {
            desired[DEFINITION_STRING] = json!(definition(1).to_string());
        }
        if let Some(workflow_type) = self.workflow_type {
            desired["StateMachineType"] = json!(workflow_type);
        }
        desired
    }
}

/// The rows of the create table: see this suite's header for what they differ along.
///
/// Catalog source for the names: `smithy_query(query="shape",shape="Name")` of the Step
/// Functions model — 1 to 80 characters — and that operation's own documentation of what a name
/// must NOT contain (white space, brackets, wildcards, the listed special characters, control
/// characters and surrogates), so `-`, `_`, `.` and `+` are admitted. The rows span a plain
/// name, one carrying that admitted punctuation, the published maximum length and the shortest
/// name there is.
fn created_rows() -> Vec<Created> {
    vec![
        Created {
            case: "a plain name, the definition as the write-only object, no workflow type",
            region: "us-east-1",
            name: "cc-sfn-orders".to_string(),
            definition_as_object: true,
            workflow_type: None,
        },
        Created {
            case: "a name carrying admitted punctuation, in ANOTHER Region, an express workflow",
            region: "eu-west-2",
            name: "cc-sfn-2026_eu.west+1".to_string(),
            definition_as_object: false,
            workflow_type: Some("EXPRESS"),
        },
        Created {
            case: "the published maximum name length, in a third Region",
            region: "us-west-2",
            name: "s".repeat(80),
            definition_as_object: true,
            workflow_type: Some("STANDARD"),
        },
        Created {
            case: "the shortest name there is, the definition as DefinitionString",
            region: "ap-southeast-1",
            name: "x".to_string(),
            definition_as_object: false,
            workflow_type: None,
        },
    ]
}

/// RULE: the identifier a `CreateResource` of this type reports is the type's primary
/// identifier — the ARN the owning service assigned, built from the Region the call was signed
/// for and the name the desired state named — so it is the handle
/// `states:DescribeStateMachine` resolves the state machine by and the value the resource model
/// carries under `Arn`. The name the desired state carried is not that handle: the owning
/// service refuses it as no ARN at all.
#[tokio::test]
async fn the_identifier_a_create_reports_is_the_arn_step_functions_answers_to() {
    let router = build_router();
    let role_arn = execution_role(&router, "the execution role", "cc-sfn-states-execution").await;
    let rows = created_rows();

    // Started together, then waited for: the creates are independent, so the span the clock
    // owes each of them is covered by one wait rather than four.
    let mut tokens = Vec::new();
    for row in &rows {
        let accepted = cloud_control_ok(
            &router,
            row.region,
            row.case,
            "CreateResource",
            &json!({
                "TypeName": STATE_MACHINE,
                "DesiredState": row.desired(&role_arn).to_string(),
            }),
        )
        .await;
        tokens.push(text(&progress(&accepted), "RequestToken"));
    }

    for (row, token) in rows.iter().zip(&tokens) {
        let case = row.case;
        let event = settled(&router, row.region, case, token).await;
        let identifier = text(&event, "Identifier");
        let expected = state_machine_arn(row.region, &row.name);
        assert_eq!(
            identifier, expected,
            "case {case:?}: expected the state machine ARN {expected:?} as the identifier, got \
             {identifier:?}"
        );

        // It is the handle the OWNING service resolves the state machine by — the one
        // `states:DescribeStateMachine` and `states:StartExecution` take — and what it resolves
        // to is the state machine the desired state described.
        let wanted = definition(1).to_string();
        let read = sfn_ok(
            &router,
            row.region,
            case,
            "DescribeStateMachine",
            &json!({ "stateMachineArn": identifier }),
        )
        .await;
        for (member, expected) in [
            ("stateMachineArn", identifier.as_str()),
            ("name", row.name.as_str()),
            ("definition", wanted.as_str()),
            ("roleArn", role_arn.as_str()),
            ("type", row.workflow_type_in_force()),
        ] {
            assert_eq!(
                text(&read, member),
                expected,
                "case {case:?}: expected the owning service to report {member} {expected:?}, got \
                 {read}"
            );
        }

        // ...while the NAME the desired state carried is no handle at all: the owning service
        // refuses it as a malformed ARN, which is why the identifier has to be the ARN.
        let (status, refused) = sfn(
            &router,
            row.region,
            "DescribeStateMachine",
            &json!({ "stateMachineArn": row.name }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the bare name to be refused with HTTP 400, got {status} with \
             body {refused}"
        );
        assert_eq!(
            shape_of(&refused),
            "InvalidArn",
            "case {case:?}: expected the bare name to be refused as no ARN, got {refused}"
        );

        // ...and the identifier is a reference THIS service resolves too, reporting it under the
        // property the type's schema points `primaryIdentifier` at.
        let reported = model(&router, row.region, case, &identifier).await;
        for (property, expected) in [
            (IDENTIFIER_PROPERTY, identifier.as_str()),
            ("Name", row.name.as_str()),
            (DEFINITION_STRING, wanted.as_str()),
            ("RoleArn", role_arn.as_str()),
            ("StateMachineType", row.workflow_type_in_force()),
        ] {
            assert_eq!(
                reported.get(property).and_then(Value::as_str),
                Some(expected),
                "case {case:?}: expected the model's {property} to be {expected:?}, got {reported}"
            );
        }
        assert_eq!(
            reported.get(DEFINITION),
            None,
            "case {case:?}: the write-only property is part of no model a read reports, got \
             {reported}"
        );
    }
}

/// One row of the refusal table: the patch document, and the pointers the refusal must name.
struct Refused {
    case: &'static str,
    /// The patch document the request carries, built where the row is read so a document
    /// carrying a definition can be written out.
    patch: fn() -> Value,
    /// The pointers the refusal lists, in the order the type's schema declares the properties.
    pointers: &'static str,
}

const REFUSED: &[Refused] = &[
    Refused {
        case: "replacing the write-only definition",
        patch: || json!([{ "op": "replace", "path": "/Definition", "value": definition(2) }]),
        pointers: "/Definition",
    },
    Refused {
        case: "replacing a member INSIDE it, which is the same property",
        patch: || json!([{ "op": "replace", "path": "/Definition/StartAt", "value": "Q" }]),
        pointers: "/Definition",
    },
    Refused {
        case: "a document naming two write-only properties at once",
        patch: || {
            json!([
                { "op": "remove", "path": "/DefinitionS3Location" },
                { "op": "replace", "path": "/Definition", "value": definition(3) },
            ])
        },
        pointers: "/Definition, /DefinitionS3Location",
    },
];

/// RULE: a property the type's schema declares write-only may only be `add`ed — every other
/// patch operation naming one is refused for naming it, and the refusal changes nothing the
/// owning service holds — while the same change sent as an `add` lands and is what that service
/// then runs.
#[tokio::test]
async fn a_write_only_definition_is_refused_a_replace_and_takes_an_add() {
    let router = build_router();
    let role_arn = execution_role(&router, "the execution role", "cc-sfn-patched-execution").await;
    // Two Regions and both forms the schema carries the definition in, so neither rule below is
    // read off one Region or one spelling of the create.
    let rows = vec![
        Created {
            case: "a standard workflow created with the write-only object",
            region: "us-east-1",
            name: "cc-sfn-patched".to_string(),
            definition_as_object: true,
            workflow_type: Some("STANDARD"),
        },
        Created {
            case: "an express workflow created with DefinitionString, in another Region",
            region: "eu-west-2",
            name: "cc-sfn-patched-2".to_string(),
            definition_as_object: false,
            workflow_type: Some("EXPRESS"),
        },
    ];

    let mut tokens = Vec::new();
    for row in &rows {
        let accepted = cloud_control_ok(
            &router,
            row.region,
            row.case,
            "CreateResource",
            &json!({
                "TypeName": STATE_MACHINE,
                "DesiredState": row.desired(&role_arn).to_string(),
            }),
        )
        .await;
        tokens.push(text(&progress(&accepted), "RequestToken"));
    }
    let mut identifiers = Vec::new();
    for (row, token) in rows.iter().zip(&tokens) {
        let event = settled(&router, row.region, row.case, token).await;
        identifiers.push(text(&event, "Identifier"));
    }

    for (row, identifier) in rows.iter().zip(&identifiers) {
        // What the owning service holds before any patch is sent: the workflow the create
        // carried and the revision it is at, neither of which a refused patch may move.
        let before_native = described(&router, row.region, row.case, identifier).await;
        let before_model = model(&router, row.region, row.case, identifier).await;
        assert_eq!(
            before_native.0,
            definition(1).to_string(),
            "case {:?}: expected the created workflow to be the one the owning service holds, \
             got {before_native:?}",
            row.case
        );

        for refused in REFUSED {
            let case = format!("{} — {}", row.case, refused.case);
            let (status, body) = cloud_control(
                &router,
                row.region,
                "UpdateResource",
                &json!({
                    "TypeName": STATE_MACHINE,
                    "Identifier": identifier,
                    "PatchDocument": (refused.patch)().to_string(),
                }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                shape_of(&body),
                "ValidationException",
                "case {case:?}: expected a ValidationException, got {body}"
            );
            assert_eq!(
                message(&body),
                format!(
                    "Invalid patch update: writeOnlyProperties [{}] can only be updated using \
                     'add' operation",
                    refused.pointers
                ),
                "case {case:?}: unexpected wording, got {:?}",
                message(&body)
            );
            // The refusal left the resource exactly as it was, on both sides: the workflow and
            // the revision the owning service holds, and the model this service reports.
            let after_native = described(&router, row.region, &case, identifier).await;
            assert_eq!(
                after_native, before_native,
                "case {case:?}: expected the owning service to still hold the workflow and the \
                 revision it had, {before_native:?}, got {after_native:?}"
            );
            let after_model = model(&router, row.region, &case, identifier).await;
            assert_eq!(
                after_model, before_model,
                "case {case:?}: expected the refused patch to change nothing in the model, got \
                 {after_model}"
            );
        }
    }

    // `add` is the one operation that SETS a write-only property, and what it sets is what the
    // owning service then runs. Started together for the reason the creates were.
    let wanted = definition(4);
    let mut tokens = Vec::new();
    for (row, identifier) in rows.iter().zip(&identifiers) {
        let accepted = cloud_control_ok(
            &router,
            row.region,
            row.case,
            "UpdateResource",
            &json!({
                "TypeName": STATE_MACHINE,
                "Identifier": identifier,
                "PatchDocument": json!([
                    { "op": "add", "path": DEFINITION_POINTER, "value": wanted },
                ])
                .to_string(),
            }),
        )
        .await;
        tokens.push(text(&progress(&accepted), "RequestToken"));
    }

    for ((row, identifier), token) in rows.iter().zip(&identifiers).zip(&tokens) {
        let case = row.case;
        let event = settled(&router, row.region, case, token).await;
        assert_eq!(
            text(&event, "Identifier"),
            *identifier,
            "case {case:?}: expected the patch to keep the resource's own identity, got {event}"
        );
        let (definition_held, revision) = described(&router, row.region, case, identifier).await;
        assert_eq!(
            definition_held,
            wanted.to_string(),
            "case {case:?}: expected the owning service to run the added workflow, got \
             {definition_held:?}"
        );
        // A change the owning service applied is a change it reports a revision for, so the
        // added workflow is at a revision the created one was not at.
        assert!(
            !revision.is_empty(),
            "case {case:?}: expected the owning service to report the revision the change left \
             it at, got {revision:?}"
        );
        let reported = model(&router, row.region, case, identifier).await;
        assert_eq!(
            reported.get(DEFINITION_STRING).and_then(Value::as_str),
            Some(wanted.to_string().as_str()),
            "case {case:?}: expected the model to report the added workflow as \
             {DEFINITION_STRING}, got {reported}"
        );
    }
}
