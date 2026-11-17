// SPDX-License-Identifier: CC0-1.0

//! What a custom resource's provider PUBLISHES, read by another resource of the same stack.
//!
//! RULE under test: **the `Data` a custom resource's provider answers a request with is that
//! resource's attributes, so an `Fn::GetAtt` naming one of them — in the properties of a
//! resource that is CREATED by the same deployment, directly or through an intrinsic that
//! composes it — resolves to the value the provider published, and that resource is created
//! with it rather than before it, so the service that owns it holds the provider's value.**
//!
//! Three crates have to agree for that to hold — CloudFormation sends the provider its
//! request, withholds the dependent resource until the answer has landed and creates it with
//! the value the answer carried, AWS Lambda runs the provider, and AWS Systems Manager holds
//! the value the dependent resource was created with — so the suite lives here: the only place
//! the published value is observable is the service that owns the resource that consumed it.
//!
//! The rows differ along what the rule generalises over: how many fields the provider
//! publishes and under which names, the shape of the value (a plain word, a value carrying the
//! punctuation of a path, and one carrying a whole URL), how the dependent declaration reads it
//! (`Fn::GetAtt` alone, inside an `Fn::Join` and inside an `Fn::Sub`), and how the custom
//! resource's type is spelled.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, members_of, opt_element, outputs_of};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The status a deployment of each row has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The status a create another resource's failure rolls back settles on.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// How long a condition the clock owes is waited for, and how often it is looked at. The wait
/// is on the CONDITION: nothing here sleeps a fixed span on a service's behalf, and the
/// deadline is far longer than the spans this emulator gives these transitions, so a loaded
/// host cannot decide the outcome.
const SETTLE_DEADLINE: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(100);

/// The logical ids every row's template declares: the provider's execution role, the provider
/// function, the custom resource and the resource that reads what the provider published.
const ROLE: &str = "Role";
const PROVIDER: &str = "Provider";
const PROBE: &str = "Probe";
const MIRROR: &str = "Mirror";

/// The provider's source: it publishes, as its response `Data`, exactly the fields its own
/// properties name, and answers at the request's `ResponseURL` with the HTTP client its runtime
/// supplies.
const PROVIDER_SOURCE: &str = r#"
import json
import urllib.request


def handler(event, context):
    properties = event["ResourceProperties"]
    data = {}
    if event["RequestType"] != "Delete":
        data = dict(properties.get("Fields") or {})
    answer = {
        "Status": "SUCCESS",
        "PhysicalResourceId": event.get("PhysicalResourceId") or "probe-physical-id",
        "StackId": event["StackId"],
        "RequestId": event["RequestId"],
        "LogicalResourceId": event["LogicalResourceId"],
        "Data": data,
    }
    body = json.dumps(answer).encode("utf-8")
    request = urllib.request.Request(
        event["ResponseURL"],
        data=body,
        headers={"content-type": ""},
        method="PUT",
    )
    with urllib.request.urlopen(request) as answered:
        answered.read()
    return {}
"#;

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the provider CloudFormation invokes is the
/// function this suite's own stack created, and the parameter the dependent resource becomes is
/// read back out of the service that owns it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: every call carries a credential scope so
    // the router can name the target service, and the signature itself is not under test.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    host: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        body,
    }
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        &format!("cloudformation.{REGION}.amazonaws.com"),
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await
}

/// Call one AWS Systems Manager operation over its awsJson1_1 wire.
async fn ssm(router: &MultiServiceRouter, operation: &str, request: &Value) -> Answered {
    dispatch(
        router,
        &format!("ssm.{REGION}.amazonaws.com"),
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("AmazonSSM.{operation}")),
            ("authorization", &authorization("ssm")),
        ],
        Bytes::from(request.to_string()),
    )
    .await
}

/// What the stack reports now: its `StackStatus`, the answer it was read from, and whether
/// CloudFormation still holds a stack under that name at all.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> (String, String, bool) {
    let answered = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    let body = answered.text();
    (
        opt_element(&body, "StackStatus").unwrap_or_default(),
        body,
        answered.status == 200,
    )
}

/// Poll `stack` until it reports a status that is not `*_IN_PROGRESS`, and answer the last
/// status it reported with the answer that status was read from.
async fn settled_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (reported, body, held) = stack_status(router, stack).await;
        let settled = !held || (!reported.is_empty() && !reported.ends_with("_IN_PROGRESS"));
        if settled || Instant::now() >= giving_up_at {
            return (reported, body);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Every resource failure the stack reported, with the reason it gave — what this suite quotes
/// when a deployment did not reach the status it must.
async fn reported_failures(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let answered = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    let body = answered.text();
    members_of(&body)
        .into_iter()
        .filter_map(|member| {
            let status = elements(member, "ResourceStatus").into_iter().next()?;
            if !status.contains("FAILED") {
                return None;
            }
            let logical = elements(member, "LogicalResourceId")
                .into_iter()
                .next()
                .unwrap_or_default();
            let reason = elements(member, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default();
            Some(format!("{logical} {status}: {reason}"))
        })
        .collect()
}

/// One row: what the provider publishes, which field the dependent resource reads and how it
/// reads it, and what the parameter it becomes must then hold.
struct Row {
    case: &'static str,
    stack: &'static str,
    function: &'static str,
    parameter: &'static str,
    resource_type: &'static str,
    /// The fields the provider publishes, as the custom resource's properties state them.
    fields: &'static [(&'static str, &'static str)],
    /// The field the dependent declaration reads.
    read: &'static str,
    /// How that declaration reads it.
    form: Form,
    /// The value the parameter must hold once the stack is created.
    expected: &'static str,
}

/// How a dependent declaration reads the attribute: on its own, or composed with other text by
/// an intrinsic that resolves one.
#[derive(Clone, Copy)]
enum Form {
    /// `{ "Fn::GetAtt": [<logical id>, <field>] }`.
    Attribute,
    /// `{ "Fn::Join": ["", ["https://", { "Fn::GetAtt": [...] }]] }`.
    Joined,
    /// `{ "Fn::Sub": ["prefix/${Probe.Field}", {}] }` — the substitution form that reads an
    /// attribute.
    Substituted,
}

const ROWS: &[Row] = &[
    Row {
        case: "one published field, read on its own, under the generic custom resource type",
        stack: "custom-resource-data-plain",
        function: "custom-resource-data-plain-provider",
        parameter: "/custom-resource-data/plain",
        resource_type: "AWS::CloudFormation::CustomResource",
        fields: &[("Value", "the-value-the-provider-computed")],
        read: "Value",
        form: Form::Attribute,
        expected: "the-value-the-provider-computed",
    },
    Row {
        case: "several published fields, one of them read and composed by Fn::Join",
        stack: "custom-resource-data-joined",
        function: "custom-resource-data-joined-provider",
        parameter: "/custom-resource-data/joined",
        resource_type: "Custom::Publisher",
        fields: &[
            ("Host", "host.example.invalid"),
            ("Path", "/a/path/with/segments"),
            ("Port", "8443"),
        ],
        read: "Host",
        form: Form::Joined,
        expected: "https://host.example.invalid",
    },
    Row {
        case: "a published field carrying a whole URL, read through Fn::Sub",
        stack: "custom-resource-data-substituted",
        function: "custom-resource-data-substituted-provider",
        parameter: "/custom-resource-data/substituted",
        resource_type: "Custom::Publisher",
        fields: &[
            ("Endpoint", "https://endpoint.example.invalid:9443/v1?probe=1"),
            ("Value", "not the field this row reads"),
        ],
        read: "Endpoint",
        form: Form::Substituted,
        expected: "endpoint=https://endpoint.example.invalid:9443/v1?probe=1",
    },
];

/// How one row's dependent declaration reads the attribute it consumes.
fn read_attribute(row: &Row) -> Value {
    match row.form {
        Form::Attribute => json!({ "Fn::GetAtt": [PROBE, row.read] }),
        Form::Joined => json!({ "Fn::Join": ["", ["https://", { "Fn::GetAtt": [PROBE, row.read] }]] }),
        Form::Substituted => json!({ "Fn::Sub": format!("endpoint=${{{PROBE}.{}}}", row.read) }),
    }
}

/// The template one row deploys: the provider's execution role, the provider function, the
/// custom resource it backs, and the parameter declared with what that resource publishes.
fn template(row: &Row) -> String {
    let fields: serde_json::Map<String, Value> = row
        .fields
        .iter()
        .map(|(name, value)| ((*name).to_string(), json!(value)))
        .collect();
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    },
                },
            },
            PROVIDER: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": row.function,
                    "Code": {"ZipFile": PROVIDER_SOURCE},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": 60,
                },
            },
            PROBE: {
                "Type": row.resource_type,
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": [PROVIDER, "Arn"]},
                    "Fields": Value::Object(fields),
                },
            },
            MIRROR: {
                "Type": "AWS::SSM::Parameter",
                "Properties": {
                    "Name": row.parameter,
                    "Type": "String",
                    "Value": read_attribute(row),
                },
            },
        },
        "Outputs": {
            "MirrorName": {"Value": {"Ref": MIRROR}},
            "Published": {"Value": read_attribute(row)},
        },
    })
    .to_string()
}

#[tokio::test]
async fn a_providers_published_data_is_what_a_dependent_resource_is_created_with() {
    for row in ROWS {
        let case = row.case;
        let router = emulator();
        let created = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &template(row)),
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
            created.status,
            created.text()
        );

        // 1. The deployment completes: the resource that reads the provider's value was
        // created with a value, not ahead of the answer that carries it.
        let (settled, described) = settled_status(&router, row.stack).await;
        if settled != CREATE_COMPLETE {
            let failures = reported_failures(&router, row.stack).await;
            panic!(
                "case {case:?}: expected the stack to settle at {CREATE_COMPLETE}, got \
                 {settled:?}; the failures it reported: {failures:?}; last answer {described}"
            );
        }

        // 2. The stack resolves the attribute to what the provider published.
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get("Published").map(String::as_str),
            Some(row.expected),
            "case {case:?}: expected Fn::GetAtt of the published field {:?} to resolve to \
             {:?}, got {outputs:?}",
            row.read,
            row.expected
        );

        // 3. ...and the service that OWNS the dependent resource holds that same value, which
        // is the only evidence the resource was created with it.
        let read = ssm(&router, "GetParameter", &json!({ "Name": row.parameter })).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected HTTP 200 from GetParameter for {:?}, got {} with {:?}",
            row.parameter,
            read.status,
            read.text()
        );
        let held = read.json()["Parameter"]["Value"].as_str().map(str::to_string);
        assert_eq!(
            held.as_deref(),
            Some(row.expected),
            "case {case:?}: expected the parameter {:?} to hold the value the provider \
             published, got {held:?} with {:?}",
            row.parameter,
            read.text()
        );
    }
}

/// The stack, the function and the parameter the lifecycle below drives.
const LIFECYCLE_STACK: &str = "custom-resource-data-lifecycle";
const LIFECYCLE_FUNCTION: &str = "custom-resource-data-lifecycle-provider";
const LIFECYCLE_PARAMETER: &str = "/custom-resource-data/lifecycle";

/// The statuses the lifecycle waits for.
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// The reason the provider refuses the request it is told to refuse.
const REFUSED_REASON: &str = "the provider refused: the value it was asked for conflicts";

/// A provider that publishes the fields its properties name, names the physical resource its
/// properties ask for, and REFUSES the request outright when they say so — the shape a
/// provider that decides its own resource's identity and validates what it is asked for has.
///
/// A `Delete` about a physical resource it never minted is refused as well: a provider acts on
/// the resource the request names, so one naming anything else is a request it cannot carry
/// out. That is what makes every removal below evidence that CloudFormation sent the
/// provider's OWN id.
const REFUSING_PROVIDER_SOURCE: &str = r#"
import json
import urllib.request


def handler(event, context):
    properties = event["ResourceProperties"]
    status = "SUCCESS"
    reason = ""
    data = {}
    minted = properties.get("Id") or "lifecycle-physical-id"
    named = event.get("PhysicalResourceId")
    if event["RequestType"] == "Delete":
        if named != minted:
            status = "FAILED"
            reason = "the provider minted %s, not %s" % (minted, named)
    elif properties.get("Refuse") == "true":
        status = "FAILED"
        reason = properties["Reason"]
    else:
        data = dict(properties.get("Fields") or {})
    answer = {
        "Status": status,
        "Reason": reason,
        "PhysicalResourceId": minted,
        "StackId": event["StackId"],
        "RequestId": event["RequestId"],
        "LogicalResourceId": event["LogicalResourceId"],
        "Data": data,
    }
    body = json.dumps(answer).encode("utf-8")
    request = urllib.request.Request(
        event["ResponseURL"],
        data=body,
        headers={"content-type": ""},
        method="PUT",
    )
    with urllib.request.urlopen(request) as answered:
        answered.read()
    return {}
"#;

/// The template one leg of the lifecycle deploys: the provider, the custom resource declared
/// with the field it is to publish (and whether it is to refuse the request), and the
/// parameter declared with what that resource publishes.
fn lifecycle_template(value: &str, refuse: bool) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    },
                },
            },
            PROVIDER: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": LIFECYCLE_FUNCTION,
                    "Code": {"ZipFile": REFUSING_PROVIDER_SOURCE},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": 60,
                },
            },
            PROBE: {
                "Type": "AWS::CloudFormation::CustomResource",
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": [PROVIDER, "Arn"]},
                    "Fields": {"Value": value},
                    "Refuse": match refuse {
                        true => "true",
                        false => "false",
                    },
                    "Reason": REFUSED_REASON,
                },
            },
            MIRROR: {
                "Type": "AWS::SSM::Parameter",
                "Properties": {
                    "Name": LIFECYCLE_PARAMETER,
                    "Type": "String",
                    "Value": {"Fn::GetAtt": [PROBE, "Value"]},
                },
            },
        },
    })
    .to_string()
}

/// The value the parameter the custom resource feeds holds now, or `None` where Systems
/// Manager holds no parameter under that name.
async fn mirrored_value(router: &MultiServiceRouter) -> Option<String> {
    let read = ssm(router, "GetParameter", &json!({ "Name": LIFECYCLE_PARAMETER })).await;
    (read.status == 200)
        .then(|| read.json()["Parameter"]["Value"].as_str().map(str::to_string))
        .flatten()
}

/// RULE (the lifecycle half): every request of a custom resource's lifecycle is answered by
/// its provider and the stack settles on that answer — an `Update` the provider REFUSES rolls
/// the operation back to the resource the previous answer published, an `Update` it accepts
/// republishes the dependent resource's value, and the stack's deletion converges — so a
/// deployment that reads a provider's value can be driven again and again rather than leaving
/// the stack stuck.
#[tokio::test]
async fn each_lifecycle_event_republishes_what_the_dependent_resource_holds() {
    let router = emulator();
    let case = "the create that first declares the resource";
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", LIFECYCLE_STACK),
            ("TemplateBody", &lifecycle_template("the first value", false)),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (settled, described) = settled_status(&router, LIFECYCLE_STACK).await;
    if settled != CREATE_COMPLETE {
        let failures = reported_failures(&router, LIFECYCLE_STACK).await;
        panic!(
            "case {case:?}: expected {CREATE_COMPLETE}, got {settled:?}; the failures it \
             reported: {failures:?}; last answer {described}"
        );
    }
    let held = mirrored_value(&router).await;
    assert_eq!(
        held.as_deref(),
        Some("the first value"),
        "case {case:?}: expected the parameter to hold what the provider published, got \
         {held:?}"
    );

    // An Update the provider REFUSES: the operation fails with the provider's own reason and
    // rolls back, leaving the resource the earlier answer published — not a stack stuck
    // part-way through its own rollback.
    let case = "an update the provider refuses";
    let refused = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", LIFECYCLE_STACK),
            ("TemplateBody", &lifecycle_template("the refused value", true)),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        refused.status,
        200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {} with {:?}",
        refused.status,
        refused.text()
    );
    let (settled, described) = settled_status(&router, LIFECYCLE_STACK).await;
    if settled != UPDATE_ROLLBACK_COMPLETE {
        let failures = reported_failures(&router, LIFECYCLE_STACK).await;
        panic!(
            "case {case:?}: expected {UPDATE_ROLLBACK_COMPLETE}, got {settled:?}; the failures \
             it reported: {failures:?}; last answer {described}"
        );
    }
    let failures = reported_failures(&router, LIFECYCLE_STACK).await;
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains(REFUSED_REASON)),
        "case {case:?}: expected the refusal to carry the provider's own reason \
         {REFUSED_REASON:?}, got {failures:?}"
    );

    // ...and an Update the provider ACCEPTS republishes the value the dependent resource
    // holds, which is the same wiring the create exercised, driven again.
    let case = "an update the provider accepts";
    let accepted = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", LIFECYCLE_STACK),
            ("TemplateBody", &lifecycle_template("the second value", false)),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        accepted.status,
        200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {} with {:?}",
        accepted.status,
        accepted.text()
    );
    let (settled, described) = settled_status(&router, LIFECYCLE_STACK).await;
    if settled != UPDATE_COMPLETE {
        let failures = reported_failures(&router, LIFECYCLE_STACK).await;
        panic!(
            "case {case:?}: expected {UPDATE_COMPLETE}, got {settled:?}; the failures it \
             reported: {failures:?}; last answer {described}"
        );
    }
    let held = mirrored_value(&router).await;
    assert_eq!(
        held.as_deref(),
        Some("the second value"),
        "case {case:?}: expected the parameter to hold what the second answer published, got \
         {held:?}"
    );

    // Last: the stack's own deletion converges, and takes the parameter it made with it.
    let case = "the deletion of the stack";
    let deleted = cloudformation(
        &router,
        "DeleteStack",
        &[("StackName", LIFECYCLE_STACK)],
    )
    .await;
    assert_eq!(
        deleted.status,
        200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {} with {:?}",
        deleted.status,
        deleted.text()
    );
    let (settled, described) = settled_status(&router, LIFECYCLE_STACK).await;
    assert!(
        settled == DELETE_COMPLETE || settled.is_empty(),
        "case {case:?}: expected the deletion to converge to {DELETE_COMPLETE}, got \
         {settled:?} with {described}"
    );
    let held = mirrored_value(&router).await;
    assert_eq!(
        held, None,
        "case {case:?}: expected the parameter the stack made to be gone, got {held:?}"
    );
}

/// The stack and the function of the rollback case below.
const ROLLBACK_STACK: &str = "custom-resource-data-rollback";
const ROLLBACK_FUNCTION: &str = "custom-resource-data-rollback-provider";

/// The logical id of the resource whose create the owning service refuses.
const BREAKER: &str = "Breaker";

/// RULE (the rollback half): a resource of a stack whose create ANOTHER resource's failure
/// rolls back is removed again in the component that owns it — for a custom resource, by the
/// `Delete` its provider is sent and answers — so the stack settles on `ROLLBACK_COMPLETE`
/// rather than stopping part-way through its own rollback.
#[tokio::test]
async fn a_rollback_removes_the_custom_resource_the_create_had_already_made() {
    let case = "a sibling resource the owning service refuses, declared behind the custom \
                resource";
    let router = emulator();
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    },
                },
            },
            PROVIDER: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": ROLLBACK_FUNCTION,
                    "Code": {"ZipFile": REFUSING_PROVIDER_SOURCE},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": 60,
                },
            },
            PROBE: {
                "Type": "AWS::CloudFormation::CustomResource",
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": [PROVIDER, "Arn"]},
                    "Fields": {"Value": "the value the provider published"},
                    "Refuse": "false",
                    "Reason": REFUSED_REASON,
                },
            },
            // A managed policy whose statement states no `Action` is one IAM refuses, so this
            // resource's create fails AFTER the custom resource's own answer has landed.
            BREAKER: {
                "Type": "AWS::IAM::ManagedPolicy",
                "DependsOn": PROBE,
                "Properties": {
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{"Effect": "Allow", "Resource": "*"}],
                    },
                },
            },
        },
    })
    .to_string();
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", ROLLBACK_STACK),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (settled, described) = settled_status(&router, ROLLBACK_STACK).await;
    let failures = reported_failures(&router, ROLLBACK_STACK).await;
    assert_eq!(
        settled, ROLLBACK_COMPLETE,
        "case {case:?}: expected the rollback to converge to {ROLLBACK_COMPLETE}, got \
         {settled:?}; the failures it reported: {failures:?}; last answer {described}"
    );
}

/// The stack, the function and the parameter of the replacement case below.
const REPLACED_STACK: &str = "custom-resource-data-replaced";
const REPLACED_FUNCTION: &str = "custom-resource-data-replaced-provider";
const REPLACED_PARAMETER: &str = "/custom-resource-data/replaced";

/// The template one leg of the replacement case deploys: the custom resource whose provider
/// names the physical resource the declaration asks for, and the parameter declared with what
/// that resource publishes.
fn replacement_template(id: &str, value: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    },
                },
            },
            PROVIDER: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": REPLACED_FUNCTION,
                    "Code": {"ZipFile": REFUSING_PROVIDER_SOURCE},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": 60,
                },
            },
            PROBE: {
                "Type": "AWS::CloudFormation::CustomResource",
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": [PROVIDER, "Arn"]},
                    "Id": id,
                    "Fields": {"Value": value},
                    "Refuse": "false",
                },
            },
            MIRROR: {
                "Type": "AWS::SSM::Parameter",
                "Properties": {
                    "Name": REPLACED_PARAMETER,
                    "Type": "String",
                    "Value": {"Fn::GetAtt": [PROBE, "Value"]},
                },
            },
        },
    })
    .to_string()
}

/// RULE (the replacement half): an `Update` a provider answers with a DIFFERENT
/// `PhysicalResourceId` has REPLACED the resource — the new physical resource is the one the
/// stack keeps, the old one is cleaned up by a further `Delete` the provider is sent and
/// answers, and the resource that reads the custom resource's published value is republished
/// from the answer that replaced it — so the update completes rather than leaving the stack on
/// a cleanup nothing concluded.
#[tokio::test]
async fn a_replacement_the_provider_answers_completes_and_republishes_what_it_feeds() {
    let case = "an update the provider answers with a different physical resource";
    let router = emulator();
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", REPLACED_STACK),
            (
                "TemplateBody",
                &replacement_template("probe-first", "the first value"),
            ),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (settled, described) = settled_status(&router, REPLACED_STACK).await;
    if settled != CREATE_COMPLETE {
        let failures = reported_failures(&router, REPLACED_STACK).await;
        panic!(
            "case {case:?}: expected {CREATE_COMPLETE}, got {settled:?}; the failures it \
             reported: {failures:?}; last answer {described}"
        );
    }

    let updated = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", REPLACED_STACK),
            (
                "TemplateBody",
                &replacement_template("probe-second", "the second value"),
            ),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {} with {:?}",
        updated.status,
        updated.text()
    );
    let (settled, described) = settled_status(&router, REPLACED_STACK).await;
    if settled != UPDATE_COMPLETE {
        let failures = reported_failures(&router, REPLACED_STACK).await;
        panic!(
            "case {case:?}: expected {UPDATE_COMPLETE}, got {settled:?}; the failures it \
             reported: {failures:?}; last answer {described}"
        );
    }
    // The resource the stack keeps is the one the answer named...
    let resources = cloudformation(
        &router,
        "DescribeStackResources",
        &[("StackName", REPLACED_STACK), ("LogicalResourceId", PROBE)],
    )
    .await;
    let described_resources = resources.text();
    assert!(
        described_resources.contains("probe-second"),
        "case {case:?}: expected the stack to hold the physical resource the answer named, got \
         {described_resources}"
    );
    // ...and the resource the stack keeps is the one the answer named.
    let read = ssm(&router, "GetParameter", &json!({ "Name": REPLACED_PARAMETER })).await;
    let held = read.json()["Parameter"]["Value"].as_str().map(str::to_string);
    assert_eq!(
        held.as_deref(),
        Some("the second value"),
        "case {case:?}: expected the parameter to hold what the replacing answer published, got \
         {held:?} with {:?}",
        read.text()
    );
}

/// The stack and the function of the refused-replacement case below.
const REFUSED_STACK: &str = "custom-resource-data-refused";
const REFUSED_FUNCTION: &str = "custom-resource-data-refused-provider";
const REFUSED_PARAMETER: &str = "/custom-resource-data/refused";

/// The template one leg of the refused-replacement case deploys.
fn refused_template(id: &str, value: &str, refuse: bool) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    },
                },
            },
            PROVIDER: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": REFUSED_FUNCTION,
                    "Code": {"ZipFile": REFUSING_PROVIDER_SOURCE},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": 60,
                },
            },
            PROBE: {
                "Type": "AWS::CloudFormation::CustomResource",
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": [PROVIDER, "Arn"]},
                    "Id": id,
                    "Fields": {"Value": value},
                    "Refuse": match refuse {
                        true => "true",
                        false => "false",
                    },
                    "Reason": REFUSED_REASON,
                },
            },
            MIRROR: {
                "Type": "AWS::SSM::Parameter",
                "Properties": {
                    "Name": REFUSED_PARAMETER,
                    "Type": "String",
                    "Value": {"Fn::GetAtt": [PROBE, "Value"]},
                },
            },
        },
    })
    .to_string()
}

/// RULE (the refused-update half): an `Update` the provider REFUSES changes nothing — the
/// resource the previous answer published is the one the stack keeps, and the rollback
/// restores it rather than removing a resource the refused answer named — so the stack
/// settles on `UPDATE_ROLLBACK_COMPLETE` and the next deployment can be made.
#[tokio::test]
async fn an_update_the_provider_refuses_keeps_the_resource_the_last_answer_published() {
    let case = "an update the provider refuses while asking for another physical resource";
    let router = emulator();
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", REFUSED_STACK),
            (
                "TemplateBody",
                &refused_template("refused-first", "the first value", false),
            ),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (settled, described) = settled_status(&router, REFUSED_STACK).await;
    if settled != CREATE_COMPLETE {
        let failures = reported_failures(&router, REFUSED_STACK).await;
        panic!(
            "case {case:?}: expected {CREATE_COMPLETE}, got {settled:?}; the failures it \
             reported: {failures:?}; last answer {described}"
        );
    }

    let refused = cloudformation(
        &router,
        "UpdateStack",
        &[
            ("StackName", REFUSED_STACK),
            (
                "TemplateBody",
                &refused_template("refused-second", "the second value", true),
            ),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        refused.status,
        200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {} with {:?}",
        refused.status,
        refused.text()
    );
    let (settled, described) = settled_status(&router, REFUSED_STACK).await;
    if settled != UPDATE_ROLLBACK_COMPLETE {
        let failures = reported_failures(&router, REFUSED_STACK).await;
        panic!(
            "case {case:?}: expected {UPDATE_ROLLBACK_COMPLETE}, got {settled:?}; the failures \
             it reported: {failures:?}; last answer {described}"
        );
    }
    let resources = cloudformation(
        &router,
        "DescribeStackResources",
        &[("StackName", REFUSED_STACK), ("LogicalResourceId", PROBE)],
    )
    .await;
    let described_resources = resources.text();
    assert!(
        described_resources.contains("refused-first"),
        "case {case:?}: expected the stack to keep the physical resource the last ACCEPTED \
         answer published, got {described_resources}"
    );
    let read = ssm(&router, "GetParameter", &json!({ "Name": REFUSED_PARAMETER })).await;
    let held = read.json()["Parameter"]["Value"].as_str().map(str::to_string);
    assert_eq!(
        held.as_deref(),
        Some("the first value"),
        "case {case:?}: expected the parameter to keep what that answer published, got {held:?} \
         with {:?}",
        read.text()
    );
}
