// SPDX-License-Identifier: CC0-1.0

//! A stack whose ONLY resource is a Lambda-backed custom resource: what a client polling
//! `DescribeStacks` is told, and what the provider's run leaves behind in the service it wrote
//! to.
//!
//! RULE under test: **a stack operation acting on a custom resource invokes the function its
//! `ServiceToken` names with the lifecycle event — the `RequestType` of that operation, the
//! `StackId` of the stack the resource belongs to, its `LogicalResourceId`, its `ResourceType`
//! and its `ResourceProperties`, every property value as TEXT — and holds the operation only
//! until the provider answers the request's `ResponseURL`, after which `DescribeStacks` reports
//! the terminal status of that operation (`CREATE_COMPLETE` for the create that first declares
//! the resource, `UPDATE_COMPLETE` for a later deployment that changes its declaration) and
//! everything the provider's run did in another service is readable there, including what an
//! earlier event of the same resource had already written.**
//!
//! Three crates have to agree for that to hold — CloudFormation sends the request and holds the
//! resource until it is answered, AWS Lambda runs the provider under the execution role IAM
//! holds for it, and Amazon S3 decides the one write that role allows — so the suite lives
//! here. The provider function and its role are deployed BEFORE the stack under test and
//! outside it, which is the shape a provider shared between stacks has: the stack under test
//! declares the custom resource and nothing else, so nothing but the provider's answer can
//! settle it.
//!
//! The rows span what the rule generalises over: how the resource's type is spelled (an
//! author's own `Custom::<Name>` and the generic `AWS::CloudFormation::CustomResource`), and
//! the shape, type and count of the properties the request carries (a nested object, a list of
//! none, one and several members, and a revision marker the template spells once as a JSON
//! number and once as text). Each row drives the two request types a deployment sends — the
//! `Create` of its first deployment and the `Update` of its second — and is asserted where a
//! client sees it: the status the stack settles at, the outputs the provider's own answer
//! resolves, and the objects Amazon S3 holds.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, members_of, opt_element, outputs_of};

/// The Region every call below is made in.
const REGION: &str = "us-east-1";

/// The statuses this suite waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The `State` AWS Lambda serves invocations of a function in.
const FUNCTION_ACTIVE: &str = "Active";

/// How long a condition the clock owes is waited for, and how often it is looked at. The wait
/// is on the CONDITION — nothing here sleeps a fixed span on a service's behalf — and the
/// deadline is far longer than the spans these transitions take, so a loaded host cannot decide
/// the outcome.
const SETTLE_DEADLINE: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(100);

/// The bucket the provider writes into, the stack that deploys the provider, and the function
/// and role that stack makes.
const BUCKET: &str = "sole-custom-resource-bucket";
const PROVIDER_STACK: &str = "sole-custom-resource-provider";
const FUNCTION: &str = "sole-custom-resource-provider";

/// The logical id every row declares its custom resource under.
const CUSTOM: &str = "Writer";

/// The prefix of the physical id the provider mints for the resource it is responsible for.
const PHYSICAL_PREFIX: &str = "written-object-";

/// The timeout the provider function is created with. Nothing here tests a function's own
/// timeout, so it is given generous room: the run shares this process with every other test of
/// this target.
const FUNCTION_TIMEOUT: u64 = 120;

/// The provider's source: it writes the object the request's properties describe with the AWS
/// SDK its runtime supplies — the one call its execution role allows — reports back what the
/// request told it, and PUTs the outcome to the request's `ResponseURL` with the HTTP client
/// that SDK is built on, which is how AWS's own documented provider code answers.
const PROVIDER_SOURCE: &str = r#"
import json

import boto3
import urllib3

S3 = boto3.client("s3")
HTTP = urllib3.PoolManager()


def handler(event, context):
    request_type = event["RequestType"]
    properties = event["ResourceProperties"]
    status = "SUCCESS"
    reason = ""
    data = {
        "SawRequestType": request_type,
        "SawStackId": event["StackId"],
        "SawLogicalResourceId": event["LogicalResourceId"],
        "SawResourceType": event["ResourceType"],
        "SawRevision": properties["Revision"],
        "SawRevisionType": type(properties["Revision"]).__name__,
        "SawKey": "",
    }
    if request_type != "Delete":
        target = properties["Target"]
        written = target["Body"] + "".join(properties["Suffixes"])
        try:
            S3.put_object(
                Bucket=target["Bucket"],
                Key=target["Key"],
                Body=written.encode("utf-8"),
            )
            data["SawKey"] = target["Key"]
        except Exception as error:
            status = "FAILED"
            reason = "Error: %s" % (error,)
    body = json.dumps(
        {
            "Status": status,
            "Reason": reason,
            "PhysicalResourceId": "written-object-" + event["LogicalResourceId"],
            "StackId": event["StackId"],
            "RequestId": event["RequestId"],
            "LogicalResourceId": event["LogicalResourceId"],
            "Data": data,
        }
    ).encode("utf-8")
    answered = HTTP.request(
        "PUT",
        event["ResponseURL"],
        headers={"content-type": ""},
        body=body,
    )
    return {"answered": answered.status}
"#;

/// What one call answered.
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
/// function this suite's own stack created, its SDK call is served by the Amazon S3 of this same
/// router, and the answer it PUTs to the `ResponseURL` is served by this router too.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: every call carries a credential scope so the
    // router can name the target service, and the signature itself is not under test.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request. The key id is one no identity store holds, so a call this suite makes on its own
/// behalf is the account's own.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
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
        "POST",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(router: &MultiServiceRouter, method: &str, path: &str) -> Answered {
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::new(),
    )
    .await
}

/// One request against Amazon S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answered {
    dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/octet-stream"),
            ("authorization", &authorization("s3")),
        ],
        Bytes::from(body.as_bytes().to_vec()),
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

/// Poll `stack` until it reports a status that is not `*_IN_PROGRESS`, and answer the last status
/// it reported with the answer that status was read from.
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

/// The bucket the provider writes into.
async fn given_bucket(router: &MultiServiceRouter) {
    let answered = s3(router, "PUT", &format!("/{BUCKET}"), "").await;
    assert_eq!(
        answered.status,
        200,
        "setup: creating the bucket {BUCKET:?} must answer 200, got {} with {}",
        answered.status,
        answered.text()
    );
}

/// The provider function and the role it runs as, deployed by a stack of their own — before the
/// stack that declares the custom resource, and outside it. The role's only grant is the one
/// object write the provider performs, on the one bucket it writes to. Answers the function's
/// ARN, once AWS Lambda reports the function active.
async fn given_provider(router: &MultiServiceRouter) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "write-the-declared-object",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": "s3:PutObject",
                                "Resource": format!("arn:aws:s3:::{BUCKET}/*"),
                            }],
                        },
                    }],
                },
            },
            "Provider": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Code": {"ZipFile": PROVIDER_SOURCE},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": FUNCTION_TIMEOUT,
                },
            },
        },
        "Outputs": {
            "ProviderArn": {"Value": {"Fn::GetAtt": ["Provider", "Arn"]}},
        },
    })
    .to_string();
    let created = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", PROVIDER_STACK),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup: CreateStack({PROVIDER_STACK:?}) must be accepted, got {} with {}",
        created.status,
        created.text()
    );
    let (settled, described) = settled_status(router, PROVIDER_STACK).await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "setup: the provider's stack must reach {CREATE_COMPLETE}, got {settled:?} with \
         {described}"
    );
    let service_token = outputs_of(&described)
        .get("ProviderArn")
        .cloned()
        .unwrap_or_default();
    assert!(
        service_token.contains(FUNCTION),
        "setup: the provider's stack must publish the function's ARN, got \
         {service_token:?} with {described}"
    );

    // The function's own provisioning window is owed by the clock: this looks until Lambda
    // reports it in the state it serves invocations in.
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
        )
        .await;
        let state = reported.json()["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state == FUNCTION_ACTIVE {
            return service_token;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: Lambda must report {FUNCTION:?} {FUNCTION_ACTIVE}, last reported \
             {state:?} with {}",
            reported.text()
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One deployment of a row's stack: what the custom resource declares, and the status the
/// operation that deploys it must reach.
struct Leg {
    case: &'static str,
    /// The key the provider is told to write.
    key: &'static str,
    /// The body it is told to write there, before the suffixes are joined onto it.
    body: &'static str,
    /// The list property the provider joins onto that body — none, one or several members.
    suffixes: &'static [&'static str],
    /// The revision marker the template declares, spelled as a JSON number when true and as
    /// text when false: a property value reaches a provider as TEXT however the template
    /// spelled it.
    revision: &'static str,
    revision_is_a_number: bool,
    /// The `RequestType` this deployment sends, and the status the stack must settle at.
    request_type: &'static str,
    status: &'static str,
}

impl Leg {
    /// The revision marker as the template spells it.
    fn declared_revision(&self) -> Value {
        if self.revision_is_a_number {
            Value::Number(
                self.revision
                    .parse::<u64>()
                    .expect("a row's numeric revision is a whole number")
                    .into(),
            )
        } else {
            Value::String(self.revision.to_string())
        }
    }

    /// What the object the provider writes must hold.
    fn written(&self) -> String {
        format!("{}{}", self.body, self.suffixes.concat())
    }
}

/// One row: a stack whose only resource is a custom resource of this type, deployed once per
/// leg.
struct Row {
    case: &'static str,
    stack: &'static str,
    resource_type: &'static str,
    legs: &'static [Leg],
}

const ROWS: &[Row] = &[
    Row {
        case: "an author's own custom resource type",
        stack: "sole-custom-resource-authored",
        resource_type: "Custom::ObjectWriter",
        legs: &[
            Leg {
                case: "the create that first declares the resource",
                key: "least-privilege/v1",
                body: "least-privilege-write-v1",
                suffixes: &[],
                revision: "1",
                revision_is_a_number: true,
                request_type: "Create",
                status: CREATE_COMPLETE,
            },
            Leg {
                case: "an update changing the key and the body the provider writes",
                key: "least-privilege/v2",
                body: "least-privilege-write-v2",
                suffixes: &["+appended"],
                revision: "2",
                revision_is_a_number: false,
                request_type: "Update",
                status: UPDATE_COMPLETE,
            },
        ],
    },
    Row {
        case: "the generic custom resource type",
        stack: "sole-custom-resource-generic",
        resource_type: "AWS::CloudFormation::CustomResource",
        legs: &[
            Leg {
                case: "the create that first declares the resource",
                key: "generic/first",
                body: "generic-write",
                suffixes: &["-one", "-two", "-three"],
                revision: "14400",
                revision_is_a_number: false,
                request_type: "Create",
                status: CREATE_COMPLETE,
            },
            Leg {
                case: "an update changing only the list property the provider joins",
                key: "generic/second",
                body: "generic-write",
                suffixes: &["-only"],
                revision: "7",
                revision_is_a_number: true,
                request_type: "Update",
                status: UPDATE_COMPLETE,
            },
        ],
    },
];

/// The template one leg deploys: the custom resource and nothing else, backed by the provider
/// deployed out of band, with the outputs that publish what its answer carried.
fn template(service_token: &str, resource_type: &str, leg: &Leg) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            CUSTOM: {
                "Type": resource_type,
                "Properties": {
                    "ServiceToken": service_token,
                    "Target": {"Bucket": BUCKET, "Key": leg.key, "Body": leg.body},
                    "Suffixes": leg.suffixes,
                    "Revision": leg.declared_revision(),
                },
            },
        },
        "Outputs": {
            "ResourceRef": {"Value": {"Ref": CUSTOM}},
            "SawRequestType": {"Value": {"Fn::GetAtt": [CUSTOM, "SawRequestType"]}},
            "SawStackId": {"Value": {"Fn::GetAtt": [CUSTOM, "SawStackId"]}},
            "SawLogicalResourceId": {"Value": {"Fn::GetAtt": [CUSTOM, "SawLogicalResourceId"]}},
            "SawResourceType": {"Value": {"Fn::GetAtt": [CUSTOM, "SawResourceType"]}},
            "SawKey": {"Value": {"Fn::GetAtt": [CUSTOM, "SawKey"]}},
            "SawRevision": {"Value": {"Fn::GetAtt": [CUSTOM, "SawRevision"]}},
            "SawRevisionType": {"Value": {"Fn::GetAtt": [CUSTOM, "SawRevisionType"]}},
        },
    })
    .to_string()
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

/// The body Amazon S3 holds at `key`, or `None` when it holds no object there.
async fn object_body(router: &MultiServiceRouter, key: &str) -> Option<String> {
    let answered = s3(router, "GET", &format!("/{BUCKET}/{key}"), "").await;
    (answered.status == 200).then(|| answered.text())
}

#[tokio::test]
async fn a_stack_whose_only_resource_is_a_custom_resource_settles_on_its_providers_answer() {
    let router = emulator();
    given_bucket(&router).await;
    let service_token = given_provider(&router).await;

    for row in ROWS {
        let row_case = row.case;
        // Every object an earlier leg of this row wrote, with what that leg wrote there.
        let mut written: BTreeMap<&str, String> = BTreeMap::new();
        for leg in row.legs {
            let case = format!("{row_case}, {}", leg.case);
            let body = template(&service_token, row.resource_type, leg);
            let action = if leg.request_type == "Create" {
                "CreateStack"
            } else {
                "UpdateStack"
            };
            let answered = cloudformation(
                &router,
                action,
                &[("StackName", row.stack), ("TemplateBody", &body)],
            )
            .await;
            assert_eq!(
                answered.status,
                200,
                "case {case:?}: {action} must be accepted, got {} with {}",
                answered.status,
                answered.text()
            );

            // 1. The operation settles, and settles at its own terminal status: a client
            // polling DescribeStacks is told the outcome rather than left in *_IN_PROGRESS.
            let (settled, described) = settled_status(&router, row.stack).await;
            if settled != leg.status {
                let failures = reported_failures(&router, row.stack).await;
                panic!(
                    "case {case:?}: expected the stack to settle at {}, got {settled:?}; the \
                     failures it reported: {failures:?}; last answer {described}",
                    leg.status
                );
            }

            // 2. The provider's answer IS the resource: the id it minted is what `Ref`
            // resolves to, and the `Data` it published is what `Fn::GetAtt` resolves against.
            let outputs = outputs_of(&described);
            let expected_ref = format!("{PHYSICAL_PREFIX}{CUSTOM}");
            assert_eq!(
                outputs.get("ResourceRef").map(String::as_str),
                Some(expected_ref.as_str()),
                "case {case:?}: expected Ref to resolve to the id the provider answered with, \
                 got {outputs:?}"
            );

            // 3. The request the provider was sent was this operation's, about this resource of
            // this stack, carrying this leg's own declared properties as text.
            let stack_id = element(&described, "StackId");
            let seen: Vec<(&str, &str)> = vec![
                ("SawRequestType", leg.request_type),
                ("SawStackId", stack_id.as_str()),
                ("SawLogicalResourceId", CUSTOM),
                ("SawResourceType", row.resource_type),
                ("SawKey", leg.key),
                ("SawRevision", leg.revision),
                // A property value reaches the provider as text however the template spelled
                // it, so a revision declared as a JSON number is read as one too.
                ("SawRevisionType", "str"),
            ];
            for (output, expected) in seen {
                assert_eq!(
                    outputs.get(output).map(String::as_str),
                    Some(expected),
                    "case {case:?}: expected the provider's {output} to be {expected:?}, got \
                     {outputs:?}"
                );
            }

            // 4. What the provider's run did in the service that owns it is readable there —
            // this leg's own write, and every write an earlier leg of this row made.
            written.insert(leg.key, leg.written());
            for (key, expected) in &written {
                let held = object_body(&router, key).await;
                assert_eq!(
                    held.as_deref(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the object {key:?} the provider wrote to hold \
                     {expected:?}, got {held:?}"
                );
            }
        }

        // The stack's own record of the resource is the provider's answer too.
        let resources =
            cloudformation(&router, "DescribeStackResources", &[("StackName", row.stack)]).await;
        assert_eq!(
            resources.status,
            200,
            "case {row_case:?}: DescribeStackResources must be answered, got {} with {}",
            resources.status,
            resources.text()
        );
        let described = resources.text();
        assert_eq!(
            element(&described, "PhysicalResourceId"),
            format!("{PHYSICAL_PREFIX}{CUSTOM}"),
            "case {row_case:?}: expected the stack to hold the physical id the provider \
             answered with, got {described}"
        );
    }
}
