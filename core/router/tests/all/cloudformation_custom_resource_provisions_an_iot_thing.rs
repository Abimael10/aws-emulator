// SPDX-License-Identifier: CC0-1.0

//! A stack whose only work is done by a Lambda-backed custom resource that provisions a
//! resource in ANOTHER service of this emulator — the standard escape hatch for a resource type
//! CloudFormation does not model itself, and the shape every custom-resource provider framework
//! emits.
//!
//! RULE under test: **a stack operation acting on a custom resource invokes the function its
//! `ServiceToken` names with that operation's lifecycle request and holds the resource, and the
//! stack behind it, in `*_IN_PROGRESS` until the provider answers the request's `ResponseURL`;
//! the `Create` answer IS the resource — its `PhysicalResourceId` is what `Ref` resolves to and
//! its `Data` is what `Fn::GetAtt` resolves against, so the stack reaches `CREATE_COMPLETE` with
//! those values in its `Outputs` — whatever the provider's run created is readable from the
//! service that OWNS it, and a `DeleteStack` sends the same provider a `Delete` request carrying
//! that `PhysicalResourceId`, after which the stack is `DELETE_COMPLETE` and the object the
//! provider removed is gone from the owning service, which refuses a read of it.**
//!
//! Three crates have to agree for that to hold — CloudFormation sends the request and holds the
//! operation until it is answered, AWS Lambda runs the provider under the execution role IAM
//! holds for it, and AWS IoT decides and records the registry writes the provider makes — so
//! the suite lives here. The provider function and its role are deployed BEFORE the stack under
//! test and outside it, which is the shape a provider shared between stacks has; the stack under
//! test declares the custom resource and nothing else, so nothing but the provider's answer can
//! settle it.
//!
//! The rows span what the rule generalises over: how the resource's type is spelled (an author's
//! own `Custom::<Name>` and the generic `AWS::CloudFormation::CustomResource`), the form and
//! length of the name the provider registers (hyphenated, underscored with digits, and mixed
//! case — all three within `ThingName`'s documented `^[a-zA-Z0-9:_-]+$` and 1..=128, from
//! `smithy_query(query="operation", operation="CreateThing")`), and the COUNT of attribute
//! entries the declaration carries the provider (several, one, and none at all). Every row is
//! driven through both lifecycle events the deploy-then-teardown of a stack sends — the `Create`
//! and the `Delete` — and asserted where a client sees it: the status the stack settles at, the
//! outputs the provider's own answer resolves, and what AWS IoT's own registry reads report
//! before and after the teardown.

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
const DELETE_COMPLETE: &str = "DELETE_COMPLETE";

/// The `State` AWS Lambda serves invocations of a function in.
const FUNCTION_ACTIVE: &str = "Active";

/// The error shape AWS IoT's `DescribeThing` declares for a name its registry does not hold,
/// and the header a `restJson1` client resolves an error shape from.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// How long a condition the clock owes is waited for, and how often it is looked at. The wait is
/// on the CONDITION — nothing here sleeps a fixed span on a service's behalf — and the deadline
/// is far longer than the spans these transitions take, so a loaded host cannot decide the
/// outcome.
const SETTLE_DEADLINE: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(100);

/// The stack that deploys the provider, and the function and role it makes.
const PROVIDER_STACK: &str = "iot-thing-custom-resource-provider";
const FUNCTION: &str = "iot-thing-custom-resource-provider";

/// The logical id every row declares its custom resource under.
const CUSTOM: &str = "Registrar";

/// The timeout the provider function is created with. Nothing here tests a function's own
/// timeout, so it is given generous room: the run shares this process with every other test of
/// this target.
const FUNCTION_TIMEOUT: u64 = 120;

/// The provider's source, written the way AWS documents one: the lifecycle request decides what
/// it does, it does it with the AWS SDK its managed runtime supplies, and it PUTs the outcome to
/// the request's `ResponseURL` with the HTTP client of the standard library that runtime carries.
///
/// `Create` registers the thing the properties name and publishes what AWS IoT answered as the
/// resource's `Data`; `Delete` removes the thing the request's own `PhysicalResourceId` names,
/// which is what CloudFormation recorded from the `Create` answer.
const PROVIDER_SOURCE: &str = r#"
import json
import urllib.request

import boto3

IOT = boto3.client("iot")


def handler(event, context):
    properties = event["ResourceProperties"]
    request_type = event["RequestType"]
    attributes = properties.get("Attributes") or {}
    physical_id = event.get("PhysicalResourceId") or properties["ThingName"]
    status = "SUCCESS"
    reason = ""
    data = {}
    try:
        if request_type == "Delete":
            IOT.delete_thing(thingName=physical_id)
        else:
            registered = IOT.create_thing(
                thingName=properties["ThingName"],
                attributePayload={"attributes": attributes},
            )
            physical_id = registered["thingName"]
            data = {
                "ThingName": registered["thingName"],
                "ThingArn": registered["thingArn"],
                "AttributeCount": str(len(attributes)),
                "SawRequestType": request_type,
            }
    except Exception as error:
        status = "FAILED"
        reason = "Error: %s" % (error,)
    body = json.dumps(
        {
            "Status": status,
            "Reason": reason,
            "PhysicalResourceId": physical_id,
            "StackId": event["StackId"],
            "RequestId": event["RequestId"],
            "LogicalResourceId": event["LogicalResourceId"],
            "Data": data,
        }
    ).encode("utf-8")
    headers = {"content-type": "", "content-length": str(len(body))}
    answering = urllib.request.Request(
        url=event["ResponseURL"], headers=headers, data=body, method="PUT"
    )
    with urllib.request.urlopen(answering) as answered:
        answered.read()
    return {"answered": status}
"#;

/// What one call answered: its status, its headers and its body verbatim.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The error shape this answer names, as a `restJson1` client resolves it: the
    /// `x-amzn-errortype` header, without the namespace or URL a value there may be qualified
    /// with.
    fn error_code(&self) -> String {
        self.headers
            .get(ERROR_TYPE_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .split(['/', ':'])
                    .next()
                    .unwrap_or(value)
                    .rsplit('#')
                    .next()
                    .unwrap_or(value)
                    .to_string()
            })
            .unwrap_or_default()
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the provider CloudFormation invokes is the
/// function this suite's own stack created, the SDK call that provider makes is served by the
/// AWS IoT of this same router, and the answer it PUTs to the `ResponseURL` is served by this
/// router too.
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
    signing_name: &str,
    path: &str,
    content_type: &str,
    body: Bytes,
) -> Answered {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", content_type)
        .header("authorization", authorization(signing_name))
        .body(body)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts.headers,
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
        "cloudformation",
        "/",
        "application/x-www-form-urlencoded",
        Bytes::from(body),
    )
    .await
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(router: &MultiServiceRouter, method: &str, path: &str) -> Answered {
    dispatch(
        router,
        method,
        "lambda",
        path,
        "application/json",
        Bytes::new(),
    )
    .await
}

/// One AWS IoT call over its `restJson1` wire — the registry reads this suite checks the
/// provider's work with, made as the account's own, never through the provider.
async fn iot(router: &MultiServiceRouter, method: &str, path: &str) -> Answered {
    dispatch(router, method, "iot", path, "application/json", Bytes::new()).await
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
/// it reported with the answer that status was read from. A stack CloudFormation no longer holds
/// under that name has settled too, and reports no status at all.
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
/// when an operation did not reach the status it must.
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

/// The provider function and the role it runs as, deployed by a stack of their own — before the
/// stack that declares the custom resource, and outside it. The role grants exactly the two
/// registry writes the provider performs. Answers the function's ARN, once AWS Lambda reports
/// the function active.
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
                        "PolicyName": "register-and-remove-the-declared-thing",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["iot:CreateThing", "iot:DeleteThing"],
                                "Resource": "*",
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
    if settled != CREATE_COMPLETE {
        let failures = reported_failures(router, PROVIDER_STACK).await;
        panic!(
            "setup: the provider's stack must reach {CREATE_COMPLETE}, got {settled:?}; the \
             failures it reported: {failures:?}; last answer {described}"
        );
    }
    let service_token = outputs_of(&described)
        .get("ProviderArn")
        .cloned()
        .unwrap_or_default();
    assert!(
        service_token.contains(FUNCTION),
        "setup: the provider's stack must publish the function's ARN, got {service_token:?} with \
         {described}"
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
            "setup: Lambda must report {FUNCTION:?} {FUNCTION_ACTIVE}, last reported {state:?} \
             with {}",
            reported.text()
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One row: a stack whose only resource is a custom resource of this type, registering this
/// thing with these attributes through the provider.
struct Row {
    case: &'static str,
    stack: &'static str,
    resource_type: &'static str,
    /// The name the provider registers. Every row's name is within what `ThingName` documents:
    /// `^[a-zA-Z0-9:_-]+$`, 1..=128 characters.
    thing: &'static str,
    /// The attribute entries the declaration carries the provider — several, one, and none.
    attributes: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "an author's own custom resource type, registering a hyphenated name with several \
               attributes",
        stack: "iot-thing-custom-resource-authored",
        resource_type: "Custom::IotThing",
        thing: "custom-resource-thing-one",
        attributes: &[
            ("site", "plant-7"),
            ("model", "sensor-v2"),
            ("serial", "0001"),
        ],
    },
    Row {
        case: "the generic custom resource type, registering an underscored name with one \
               attribute",
        stack: "iot-thing-custom-resource-generic",
        resource_type: "AWS::CloudFormation::CustomResource",
        thing: "custom_resource_thing_2",
        attributes: &[("site", "plant-9")],
    },
    Row {
        case: "an author's own custom resource type, registering a mixed-case name with no \
               attributes",
        stack: "iot-thing-custom-resource-bare",
        resource_type: "Custom::ThingRegistrar",
        thing: "CustomResourceThing3",
        attributes: &[],
    },
];

impl Row {
    /// The attribute map as the template declares it, and as the registry must then report it.
    fn declared_attributes(&self) -> BTreeMap<String, String> {
        self.attributes
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    /// The template this row deploys: the custom resource and nothing else, backed by the
    /// provider deployed out of band, with the outputs that publish what its answer carried.
    fn template(&self, service_token: &str) -> String {
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {
                CUSTOM: {
                    "Type": self.resource_type,
                    "Properties": {
                        "ServiceToken": service_token,
                        "ThingName": self.thing,
                        "Attributes": self.declared_attributes(),
                    },
                },
            },
            "Outputs": {
                "ResourceRef": {"Value": {"Ref": CUSTOM}},
                "ThingName": {"Value": {"Fn::GetAtt": [CUSTOM, "ThingName"]}},
                "ThingArn": {"Value": {"Fn::GetAtt": [CUSTOM, "ThingArn"]}},
                "AttributeCount": {"Value": {"Fn::GetAtt": [CUSTOM, "AttributeCount"]}},
                "SawRequestType": {"Value": {"Fn::GetAtt": [CUSTOM, "SawRequestType"]}},
            },
        })
        .to_string()
    }
}

/// The account the ARN AWS IoT minted belongs to — the account a registry refusal names, read
/// off the resource's own ARN rather than assumed.
fn account_of(case: &str, arn: &str) -> String {
    arn.split(':')
        .nth(4)
        .filter(|account| !account.is_empty())
        .unwrap_or_else(|| panic!("case {case:?}: {arn:?} is not an ARN naming an account"))
        .to_string()
}

#[tokio::test]
async fn a_lambda_backed_custom_resource_registers_and_removes_its_iot_thing() {
    let router = emulator();
    let service_token = given_provider(&router).await;

    for row in ROWS {
        let case = row.case;

        // 1. The deployment is accepted and settles at the terminal status of the operation that
        // drove it: a client polling DescribeStacks is told the outcome rather than left in
        // CREATE_IN_PROGRESS.
        let body = row.template(&service_token);
        let created = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", row.stack), ("TemplateBody", &body)],
        )
        .await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: CreateStack must be accepted, got {} with {}",
            created.status,
            created.text()
        );
        let (settled, described) = settled_status(&router, row.stack).await;
        if settled != CREATE_COMPLETE {
            let failures = reported_failures(&router, row.stack).await;
            panic!(
                "case {case:?}: expected the stack to settle at {CREATE_COMPLETE}, got \
                 {settled:?}; the failures it reported: {failures:?}; last answer {described}"
            );
        }

        // 2. The provider's answer IS the resource: the id it minted is what `Ref` resolves to,
        // and the `Data` it published is what `Fn::GetAtt` resolves against.
        let outputs = outputs_of(&described);
        let expected_count = row.attributes.len().to_string();
        let seen: Vec<(&str, &str)> = vec![
            ("ResourceRef", row.thing),
            ("ThingName", row.thing),
            ("AttributeCount", expected_count.as_str()),
            ("SawRequestType", "Create"),
        ];
        for (output, expected) in seen {
            assert_eq!(
                outputs.get(output).map(String::as_str),
                Some(expected),
                "case {case:?}: expected the output {output} to be {expected:?}, got {outputs:?}"
            );
        }
        let published_arn = outputs
            .get("ThingArn")
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the stack published no ThingArn: {outputs:?}")
            });
        assert!(
            published_arn.ends_with(&format!(":thing/{}", row.thing)),
            "case {case:?}: expected the published ARN to name the registered thing, got \
             {published_arn:?}"
        );

        // 3. What the provider's run created is backed by the service that OWNS it: AWS IoT's
        // own read reports the thing, with the attributes the declaration carried and the ARN
        // the stack published.
        let read = iot(&router, "GET", &format!("/things/{}", row.thing)).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: DescribeThing({}) must be served after the stack completed, got {} \
             with {}",
            row.thing,
            read.status,
            read.text()
        );
        let reported = read.json();
        assert_eq!(
            reported["thingName"].as_str(),
            Some(row.thing),
            "case {case:?}: expected AWS IoT to report the registered name, got {reported}"
        );
        assert_eq!(
            reported["thingArn"].as_str(),
            Some(published_arn.as_str()),
            "case {case:?}: expected AWS IoT to report the ARN the stack published, got {reported}"
        );
        let held: BTreeMap<String, String> = reported["attributes"]
            .as_object()
            .map(|entries| {
                entries
                    .iter()
                    .map(|(key, value)| {
                        (key.clone(), value.as_str().unwrap_or_default().to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            held,
            row.declared_attributes(),
            "case {case:?}: expected the thing to hold the declared attributes, got {reported}"
        );

        // The stack's own record of the resource is the provider's answer too.
        let resources =
            cloudformation(&router, "DescribeStackResources", &[("StackName", row.stack)]).await;
        assert_eq!(
            resources.status,
            200,
            "case {case:?}: DescribeStackResources must be answered, got {} with {}",
            resources.status,
            resources.text()
        );
        let listed = resources.text();
        assert_eq!(
            element(&listed, "PhysicalResourceId"),
            row.thing,
            "case {case:?}: expected the stack to hold the physical id the provider answered \
             with, got {listed}"
        );

        // 4. The teardown sends the same provider the `Delete` of the resource it made, and the
        // stack reaches the terminal status of that operation.
        let deleted = cloudformation(&router, "DeleteStack", &[("StackName", row.stack)]).await;
        assert_eq!(
            deleted.status,
            200,
            "case {case:?}: DeleteStack must be accepted, got {} with {}",
            deleted.status,
            deleted.text()
        );
        let (torn_down, last) = settled_status(&router, row.stack).await;
        assert!(
            torn_down.is_empty() || torn_down == DELETE_COMPLETE,
            "case {case:?}: expected the teardown to converge to {DELETE_COMPLETE}, got \
             {torn_down:?} with {last}"
        );

        // 5. What the provider's `Delete` branch removed is gone from the service that owns it:
        // the read AWS IoT served above is now refused with the shape its model declares and the
        // sentence that refusal states, naming the account the ARN carried and the name asked
        // for.
        let account = account_of(case, &published_arn);
        let missing = iot(&router, "GET", &format!("/things/{}", row.thing)).await;
        assert_eq!(
            missing.status,
            404,
            "case {case:?}: DescribeThing({}) must be refused once the teardown ran, got {} with \
             {}",
            row.thing,
            missing.status,
            missing.text()
        );
        assert_eq!(
            missing.error_code(),
            RESOURCE_NOT_FOUND,
            "case {case:?}: expected the refusal to name {RESOURCE_NOT_FOUND}, got {} with {}",
            missing.error_code(),
            missing.text()
        );
        let expected_message = format!("Thing {account}:{} not found.", row.thing);
        assert_eq!(
            missing.json()["message"].as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the refusal to state {expected_message:?}, got {}",
            missing.text()
        );
    }
}
