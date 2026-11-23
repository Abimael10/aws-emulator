// SPDX-License-Identifier: CC0-1.0

//! `AWS::IoT::Thing` end to end: the thing a stack declares is a record of the AWS IoT
//! registry, under the physical id the stack publishes.
//!
//! RULE: a resource type CloudFormation deploys is created in the service that OWNS it,
//! through that service's own public API, under the physical id the stack publishes — so the
//! name a client reads out of a stack's outputs is one that service answers for, every later
//! control-plane call about the resource resolves it, the attributes the stack publishes are
//! the ones that service minted, and every precondition that service enforces — on the create
//! and on the DELETE — is the stack's answer too.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES the thing
//! or leaves CloudFormation to name it, the characters the declared name uses, whether the
//! declaration carries attributes (and whether their values are written as text or as a
//! number), the template spelling (JSON or YAML), and whether another resource the thing
//! depends on is deployed beside it.
//!
//! Every case drives the real request path for both services through one router — the
//! CloudFormation calls over the awsQuery wire (the harness `stack_lifecycle.rs` owns) and the
//! AWS IoT calls over its REST wire — so what is asserted is what a client that deploys the
//! template and then uses the device observes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{
    call, create_stack, ec2, member, output_value, router, settled_stack, xml_text, ACCOUNT_ID,
    REGION,
};

/// Call one AWS IoT operation through the same router, on the method and URI that service's
/// model binds it to (`restJson1`), with the headers the operation binds its members to: the
/// status and the parsed answer document.
async fn iot(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://iot.{REGION}.amazonaws.com{uri}"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        );
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let http_request = request
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed AWS IoT request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let answer = response.into_body().to_vec();
    (status, serde_json::from_slice(&answer).unwrap_or(Value::Null))
}

/// `DescribeThing`: the thing AWS IoT holds under `name`, or `None` where it holds none.
async fn described_thing(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    let (status, document) = iot(router, "GET", &thing_uri(name), None, &[]).await;
    (status == 200).then_some(document)
}

/// The URI the per-thing operations bind (`/things/{thingName}`), with the name as one path
/// segment — every name a case uses is inside the pattern that model states for it, whose
/// characters are all legal in a path segment.
fn thing_uri(name: &str) -> String {
    format!("/things/{name}")
}

/// `CreateKeysAndCertificate`: the ARN of one active X.509 certificate, the principal a device
/// is reached through.
async fn certificate_arn(router: &MultiServiceRouter, case: &str) -> String {
    let (status, document) = iot(
        router,
        "POST",
        "/keys-and-certificate?setAsActive=true",
        Some(&json!({})),
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected a certificate from CreateKeysAndCertificate, got {status} \
         with {document}"
    );
    document
        .get("certificateArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no certificateArn in {document}"))
        .to_string()
}

/// `AttachThingPrincipal`: the principal travels in the header the model binds it to.
async fn attach_principal(
    router: &MultiServiceRouter,
    thing: &str,
    principal: &str,
) -> (u16, Value) {
    iot(
        router,
        "PUT",
        &format!("{}/principals", thing_uri(thing)),
        None,
        &[("x-amzn-principal", principal)],
    )
    .await
}

/// `DetachThingPrincipal`, the call AWS's teardown order clears a device attachment with.
async fn detach_principal(
    router: &MultiServiceRouter,
    thing: &str,
    principal: &str,
) -> (u16, Value) {
    iot(
        router,
        "DELETE",
        &format!("{}/principals", thing_uri(thing)),
        None,
        &[("x-amzn-principal", principal)],
    )
    .await
}

/// `ListThingPrincipals`: the principals that service reports attached to `thing`.
async fn listed_principals(router: &MultiServiceRouter, thing: &str) -> Vec<String> {
    let (status, document) = iot(
        router,
        "GET",
        &format!("{}/principals", thing_uri(thing)),
        None,
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected ListThingPrincipals to answer 200 for {thing}, got {status} with {document}"
    );
    document
        .get("principals")
        .and_then(Value::as_array)
        .map(|principals| {
            principals
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The attribute map a read of the thing reports, which is empty where it reports none.
fn reported_attributes(described: &Value) -> Value {
    described
        .get("attributes")
        .cloned()
        .unwrap_or_else(|| json!({}))
}

/// One deployment: the template, the name the thing must end up under where the declaration
/// states one, and the attributes AWS IoT must hold for it.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: String,
    /// The thing name the declaration states, or `None` where CloudFormation names it.
    thing_name: Option<&'static str>,
    /// The attribute map AWS IoT must report, as a map of text.
    attributes: Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a named thing with attributes, in JSON, behind a dependency it waits for",
            stack: "iot-thing-named",
            template: json!({
                "Resources": {
                    "Vpc": {
                        "Type": "AWS::EC2::VPC",
                        "Properties": {"CidrBlock": "10.90.0.0/16"}
                    },
                    "Thing": {
                        "Type": "AWS::IoT::Thing",
                        "DependsOn": "Vpc",
                        "Properties": {
                            "ThingName": "iot-stack-sensor",
                            // A value a template wrote as a number reaches the registry as the
                            // text it spells, because the attribute map is a map of strings.
                            "AttributePayload": {
                                "Attributes": {"Model": "a-1", "Revision": 2}
                            }
                        }
                    }
                },
                "Outputs": {
                    "ThingName": {"Value": {"Ref": "Thing"}},
                    "ThingArn": {"Value": {"Fn::GetAtt": ["Thing", "Arn"]}},
                    "ThingId": {"Value": {"Fn::GetAtt": ["Thing", "Id"]}}
                }
            })
            .to_string(),
            thing_name: Some("iot-stack-sensor"),
            attributes: json!({"Model": "a-1", "Revision": "2"}),
        },
        Row {
            case: "nothing named by the template, in YAML, with no attributes at all",
            stack: "iotthinggen",
            template: [
                "Resources:",
                "  Thing:",
                "    Type: AWS::IoT::Thing",
                "Outputs:",
                "  ThingName:",
                "    Value: !Ref Thing",
                "  ThingArn:",
                "    Value: !GetAtt Thing.Arn",
                "  ThingId:",
                "    Value: !GetAtt Thing.Id",
            ]
            .join("\n"),
            thing_name: None,
            attributes: json!({}),
        },
        Row {
            case: "a declared name carrying a colon and an underscore, in JSON",
            stack: "iot-thing-punctuated",
            template: json!({
                "Resources": {
                    "Thing": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": {
                            "ThingName": "fleet:eu_1",
                            "AttributePayload": {"Attributes": {"Site": "EU"}}
                        }
                    }
                },
                "Outputs": {
                    "ThingName": {"Value": {"Ref": "Thing"}},
                    "ThingArn": {"Value": {"Fn::GetAtt": ["Thing", "Arn"]}},
                    "ThingId": {"Value": {"Fn::GetAtt": ["Thing", "Id"]}}
                }
            })
            .to_string(),
            thing_name: Some("fleet:eu_1"),
            attributes: json!({"Site": "EU"}),
        },
    ]
}

/// RULE: the thing a stack declares is registered in AWS IoT itself, the stack publishes the
/// name it was registered under and the ARN and id that service minted, and the device can
/// then be used through that service — a certificate attaches to it and the attachment is
/// listed back.
#[tokio::test]
async fn the_declared_thing_is_registered_in_the_owning_service() {
    for row in rows() {
        let case = row.case;
        let router = router();
        let (status, body) = create_stack(&router, row.stack, &row.template, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
        );
        let body = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to complete, got {body}"
        );

        // The name the stack PUBLISHES is the one the thing answers to.
        let thing_name = output_value(&body, "ThingName")
            .unwrap_or_else(|| panic!("case {case:?}: no ThingName output in {body}"));
        if let Some(declared) = row.thing_name {
            assert_eq!(
                thing_name, declared,
                "case {case:?}: expected the declared thing name, got {thing_name:?}"
            );
        }

        // AWS IoT's OWN read answers for it, with what the declaration asked for.
        let described = described_thing(&router, &thing_name)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: AWS IoT holds no thing {thing_name:?}"));
        assert_eq!(
            described.get("thingName").and_then(Value::as_str),
            Some(thing_name.as_str()),
            "case {case:?}: expected the thing to answer under the published name, got \
             {described}"
        );
        let attributes = reported_attributes(&described);
        assert_eq!(
            attributes, row.attributes,
            "case {case:?}: expected the declared attributes {} in the registry, got \
             {attributes}",
            row.attributes
        );

        // The attributes the stack publishes are the ones that service minted — read back off
        // its own record rather than compared against a literal.
        assert_eq!(
            output_value(&body, "ThingArn"),
            described
                .get("thingArn")
                .and_then(Value::as_str)
                .map(str::to_string),
            "case {case:?}: expected the published Arn to be the one AWS IoT minted, got {body}"
        );
        assert_eq!(
            output_value(&body, "ThingId"),
            described
                .get("thingId")
                .and_then(Value::as_str)
                .map(str::to_string),
            "case {case:?}: expected the published Id to be the one AWS IoT minted, got {body}"
        );
        assert!(
            output_value(&body, "ThingArn").is_some_and(|arn| arn
                == format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}")),
            "case {case:?}: expected an AWS IoT thing ARN of this account, got {body}"
        );

        // The deployed thing is a registry resource like any other: a certificate attaches to
        // it, and the attachment is listed back.
        let certificate = certificate_arn(&router, case).await;
        let (attached, answer) = attach_principal(&router, &thing_name, &certificate).await;
        assert_eq!(
            attached, 200,
            "case {case:?}: expected AttachThingPrincipal to answer 200 for the deployed \
             thing, got {attached} with {answer}"
        );
        let listed = listed_principals(&router, &thing_name).await;
        assert_eq!(
            listed,
            vec![certificate.clone()],
            "case {case:?}: expected the attached certificate to be listed, got {listed:?}"
        );
    }
}

/// RULE, at the DELETE end: the precondition AWS IoT enforces on `DeleteThing` — a thing a
/// principal is still attached to is not torn out from under the credential that reaches it —
/// is the stack's answer too, so the teardown ends `DELETE_FAILED` with the thing and the
/// resource it depended on both intact; once the principal is detached the same delete
/// succeeds and the thing is gone.
#[tokio::test]
async fn the_stack_teardown_reports_the_refusal_of_a_thing_that_still_has_a_principal() {
    let case = "a thing whose certificate is still attached";
    let router = router();
    let stack = "iot-thing-attached";
    let template = json!({
        "Resources": {
            "Vpc": {"Type": "AWS::EC2::VPC", "Properties": {"CidrBlock": "10.91.0.0/16"}},
            "Thing": {
                "Type": "AWS::IoT::Thing",
                "DependsOn": "Vpc",
                "Properties": {"ThingName": "attached-sensor"}
            }
        },
        "Outputs": {
            "ThingName": {"Value": {"Ref": "Thing"}},
            "VpcId": {"Value": {"Ref": "Vpc"}}
        }
    })
    .to_string();

    let (status, body) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
    );
    let body = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete, got {body}"
    );
    let thing_name = output_value(&body, "ThingName")
        .unwrap_or_else(|| panic!("case {case:?}: no ThingName output in {body}"));
    let vpc_id = output_value(&body, "VpcId")
        .unwrap_or_else(|| panic!("case {case:?}: no VpcId output in {body}"));

    let certificate = certificate_arn(&router, case).await;
    let (attached, answer) = attach_principal(&router, &thing_name, &certificate).await;
    assert_eq!(
        attached, 200,
        "case {case:?}: expected AttachThingPrincipal to answer 200, got {attached} with \
         {answer}"
    );

    let (status, deleted) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {deleted}"
    );
    let body = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&body, "StackStatus").as_deref(),
        Some("DELETE_FAILED"),
        "case {case:?}: expected the refused delete to leave the stack DELETE_FAILED, got \
         {body}"
    );
    assert!(
        described_thing(&router, &thing_name).await.is_some(),
        "case {case:?}: the thing the delete could not remove must still be registered"
    );
    assert_eq!(
        listed_principals(&router, &thing_name).await,
        vec![certificate.clone()],
        "case {case:?}: the attachment the refusal is about must still stand"
    );
    // The resource the thing depended on is untouched by the refused teardown: Amazon EC2
    // still holds the VPC, read back through its own wire AFTER the failure.
    let (status, described_vpcs) = ec2(&router, "DescribeVpcs", &[("VpcId.1", &vpc_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeVpcs to answer 200 after the refused teardown, got \
         {status} with {described_vpcs}"
    );
    assert!(
        described_vpcs.contains(&vpc_id),
        "case {case:?}: expected Amazon EC2 to still hold {vpc_id}, got {described_vpcs}"
    );

    // Detaching the principal clears the precondition, so the same teardown now succeeds and
    // the thing is gone from the registry.
    let (detached, answer) = detach_principal(&router, &thing_name, &certificate).await;
    assert_eq!(
        detached, 200,
        "case {case:?}: expected DetachThingPrincipal to answer 200, got {detached} with \
         {answer}"
    );
    let (status, deleted) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the retried DeleteStack, got {status} with \
         {deleted}"
    );
    let body = settled_stack(&router, case, stack).await;
    assert!(
        body.is_empty() || xml_text(&body, "StackStatus").as_deref() == Some("DELETE_COMPLETE"),
        "case {case:?}: expected the retried delete to complete, got {body}"
    );
    assert!(
        described_thing(&router, &thing_name).await.is_none(),
        "case {case:?}: the completed delete must have released the thing"
    );
}

/// A changed declaration is applied through the owning service's own update: the thing keeps
/// the identity AWS IoT minted and holds exactly the attributes the new revision states —
/// including none, where the revision drops the payload.
#[tokio::test]
async fn a_changed_declaration_is_applied_through_the_owning_service() {
    let router = router();
    let stack = "iot-thing-updated";
    let template = |payload: Value| {
        let mut properties = json!({"ThingName": "updated-sensor"});
        if let Value::Object(declared) = &payload {
            if !declared.is_empty() {
                properties["AttributePayload"] = payload.clone();
            }
        }
        json!({
            "Resources": {"Thing": {"Type": "AWS::IoT::Thing", "Properties": properties}},
            "Outputs": {"ThingId": {"Value": {"Fn::GetAtt": ["Thing", "Id"]}}}
        })
        .to_string()
    };

    let case = "the first revision";
    let (status, body) = create_stack(
        &router,
        stack,
        &template(json!({"Attributes": {"Model": "a-1", "Site": "EU"}})),
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
    );
    let body = settled_stack(&router, case, stack).await;
    assert_eq!(
        xml_text(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the stack to complete, got {body}"
    );
    let minted_id = output_value(&body, "ThingId")
        .unwrap_or_else(|| panic!("case {case:?}: no ThingId output in {body}"));

    // The rows are the revisions: one that changes an attribute and drops another, and one
    // that drops the payload altogether.
    let revisions: Vec<(&str, Value, Value)> = vec![
        (
            "a revision that changes one attribute and drops another",
            json!({"Attributes": {"Model": "a-2"}}),
            json!({"Model": "a-2"}),
        ),
        (
            "a revision that drops the attributes altogether",
            json!({}),
            json!({}),
        ),
    ];
    for (case, payload, expected) in revisions {
        let (status, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", stack),
                member("TemplateBody", &template(payload)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {body}"
        );
        let body = settled_stack(&router, case, stack).await;
        assert_eq!(
            xml_text(&body, "StackStatus").as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: expected the update to complete, got {body}"
        );
        let described = described_thing(&router, "updated-sensor")
            .await
            .unwrap_or_else(|| panic!("case {case:?}: AWS IoT holds no updated thing"));
        let attributes = reported_attributes(&described);
        assert_eq!(
            attributes, expected,
            "case {case:?}: expected the registry to hold {expected}, got {attributes}"
        );
        assert_eq!(
            output_value(&body, "ThingId"),
            Some(minted_id.clone()),
            "case {case:?}: the in-place update must keep the identity AWS IoT minted, got \
             {body}"
        );
    }
}

/// A declaration the OWNING SERVICE refuses is the stack's answer: a thing name outside the
/// pattern AWS IoT models for it is refused there, so the stack does not report a thing it
/// never registered and nothing is left under the name.
#[tokio::test]
async fn a_name_the_owning_service_refuses_leaves_no_thing_behind() {
    let case = "a thing name carrying a character the owning service's pattern excludes";
    let router = router();
    let stack = "iot-thing-refused";
    // `CreateThing.thingName` is `^[a-zA-Z0-9:_-]+$`
    // (`smithy_query(query="operation",operation="CreateThing")`), which admits no space.
    let refused = "bad sensor name";
    let template = json!({
        "Resources": {
            "Thing": {
                "Type": "AWS::IoT::Thing",
                "Properties": {"ThingName": refused}
            }
        }
    })
    .to_string();

    let (status, body) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
    );
    let body = settled_stack(&router, case, stack).await;
    assert_ne!(
        xml_text(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: a name the owning service refuses must not complete the stack, got \
         {body}"
    );
    assert!(
        // The name carries a space, so it is written into the read's URI percent-encoded —
        // the operation's label decodes it back to the name the template declared.
        described_thing(&router, &refused.replace(' ', "%20"))
            .await
            .is_none(),
        "case {case:?}: a refused create must leave no thing behind"
    );
}
