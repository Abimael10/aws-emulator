// SPDX-License-Identifier: CC0-1.0

//! The identifier a CloudFormation stack PUBLISHES for an
//! `AWS::ApplicationInsights::Application` is the application ARN, never the generated
//! stack-derived name.
//!
//! The rule under test: **a resource type whose documented `Ref` is an ARN publishes that ARN as
//! its physical resource id, even where the ARN is a function of ANOTHER resource of the same
//! template — so every place a client reads the identifier from (`Ref`, the type's `Fn::GetAtt`
//! attribute, an `Fn::Sub` naming the resource, the physical id a resource listing reports, and
//! the resolved property of another resource that was fed from it) carries that one string, and
//! the `<stack>-<logical-id>-<suffix>` name CloudFormation generates for a resource whose
//! declaration names none is never published for it.** For this type the ARN is
//! `arn:<partition>:applicationinsights:<region>:<account>:application/resource-group/<group>`:
//! the fixed `application/resource-group/` segment with the partition, the Region and the account
//! of the deployment and the resource's RESOLVED `ResourceGroupName` — Application Insights keys
//! an application by the resource group it monitors and the type has no name property of its own,
//! which is why the identity is settled only once that group's own is known.
//!
//! The rows differ along what the rule generalises over: the Region deployed in, how the
//! declaration STATES the group (a `Ref` to a group declared beside it, a `Ref` to a group whose
//! own name comes from a stack parameter, an `Fn::Sub` over that `Ref`, and an `Fn::Join` that
//! composes the name of a group a client created beforehand), the form of the group's name
//! (plain, and one carrying the dots, underscores and digits the modelled `ResourceGroupName`
//! pattern admits), the resource's logical id, and which readers the template publishes the
//! identifier through.
//!
//! Three crates have to agree for the rule to hold — CloudFormation computes and publishes the
//! identifier, Resource Groups answers for the group it is built from, and Systems Manager holds
//! the parameter another resource of the stack was fed the identifier into — so the suite lives
//! here rather than in any one of them. Every expected value is computed here from what the row
//! declares, never read back out of the answer being checked.
//!
//! The awsQuery form encoder and the XML readers are the test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of, physical_ids_of};

/// How long a wait for a stack to reach a TERMINAL status may run before the suite calls it a
/// failure. What is waited for is the CONDITION — the stack out of its `_IN_PROGRESS` statuses —
/// and the deadline is only a safety net so a wait that will never be satisfied still ends. It is
/// far above every clock-owed span involved, so a loaded host cannot decide the outcome, and
/// nothing here is asserted about elapsed time.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(120);

/// How long to wait between two looks at a condition that is not satisfied yet.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// Application Insights' signing name, which is also its ARN namespace (that model's
/// `endpointPrefix` and `arnNamespace`, both `applicationinsights`).
const APPLICATION_INSIGHTS: &str = "applicationinsights";

/// Resource Groups' signing name, which is its ARN namespace too.
const RESOURCE_GROUPS: &str = "resource-groups";

/// Systems Manager's signing name and the `X-Amz-Target` prefix of its awsJson1_1 operations.
const SSM: &str = "ssm";
const SSM_TARGET_PREFIX: &str = "AmazonSSM";

/// The fixed path segment every application ARN carries between the account and the group: the
/// one part of the identifier that comes from neither the request nor the deployment.
const APPLICATION_PATH: &str = ":application/resource-group/";

/// The ARN Application Insights addresses the application over `group` in `region` by, which is
/// what this type's `Ref` returns: a total function of the partition, the namespace, the Region,
/// the account and that group.
fn application_arn(region: &str, group: &str) -> String {
    format!("arn:aws:{APPLICATION_INSIGHTS}:{region}:{ACCOUNT}{APPLICATION_PATH}{group}")
}

/// The emulator with the four services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other suite
/// here; the in-process callbacks CloudFormation makes sign their requests either way (the
/// credential scope is what routes them).
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
        service_com_amazonaws_applicationinsights_ec2windowsbarleyservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_resourcegroups_ardi::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads its Region from.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(String, String)],
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
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// [`cloudformation`] for a call whose only member is the stack it addresses.
async fn on_stack(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    stack_name: &str,
) -> (u16, String) {
    cloudformation(
        router,
        region,
        action,
        &[("StackName".to_string(), stack_name.to_string())],
    )
    .await
}

/// Call one Systems Manager operation over its awsJson1_1 wire, signed for `region`.
async fn ssm(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{SSM}.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{SSM_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(SSM, region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// Create one resource group through Resource Groups' own `POST /groups`, the way a client that
/// already has a group does.
async fn create_group(router: &MultiServiceRouter, region: &str, name: &str) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/groups")
        .header("host", format!("{RESOURCE_GROUPS}.{region}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization(RESOURCE_GROUPS, region))
        .body(Bytes::from(
            serde_json::to_vec(&json!({ "Name": name })).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: CreateGroup({name:?}) in {region} must answer 200, got {status} with body {body}"
    );
}

/// The stack's status once it is no longer `*_IN_PROGRESS`, with the `DescribeStacks` answer it
/// was read from: looked at again until it settles or [`TERMINAL_DEADLINE`] passes, so what the
/// suite waits for is the CONDITION rather than a span of the host's clock.
async fn settled_stack(
    router: &MultiServiceRouter,
    region: &str,
    stack_name: &str,
) -> (String, String) {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let (status, described) = on_stack(router, region, "DescribeStacks", stack_name).await;
        assert_eq!(
            status, 200,
            "DescribeStacks({stack_name}) must answer 200, got {status} with body {described}"
        );
        let reached = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") || Instant::now() >= deadline {
            return (reached, described);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `ResourceQuery` a resource group is declared with: the tag query form
/// (`TAG_FILTERS_1_0`), which is the one every row here uses — what the group SELECTS is beside
/// the rule under test, which is about the identifier built from its NAME.
fn tag_query(tag: &str) -> Value {
    json!({
        "Type": "TAG_FILTERS_1_0",
        "Query": {
            "ResourceTypeFilters": ["AWS::AllSupported"],
            "TagFilters": [{ "Key": "ref-is-its-arn", "Values": [tag] }],
        },
    })
}

/// One row: a template that declares an application, and what the row says the group it ends up
/// monitoring is.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the ARN carries.
    region: &'static str,
    /// The resource group the application monitors once every reference is resolved — what the
    /// expected ARN is built from.
    group: &'static str,
    /// Whether a CLIENT creates that group before the stack is deployed; a row that does not
    /// declares the group in the stack itself, so the application's identity is a function of a
    /// resource of the same template.
    group_created_first: bool,
    /// The values `CreateStack` passes for the template's own `Parameters`.
    stack_parameters: &'static [(&'static str, &'static str)],
    /// The template body.
    template: fn() -> String,
    /// The application's logical id in the template.
    logical_id: &'static str,
    /// The output keys whose value must be the application ARN: every output the template
    /// publishes over the resource, whichever intrinsic reads it.
    arn_outputs: &'static [&'static str],
    /// The Systems Manager parameter another resource of the same stack was fed the published
    /// identifier into, which that service must then hold the ARN under.
    parameter: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a group declared beside it and named through Ref",
            region: "us-west-2",
            group: "ai-ref-group",
            group_created_first: false,
            stack_parameters: &[],
            template: || {
                json!({
                    "AWSTemplateFormatVersion": "2010-09-09",
                    "Resources": {
                        "Group": {
                            "Type": "AWS::ResourceGroups::Group",
                            "Properties": {
                                "Name": "ai-ref-group",
                                "ResourceQuery": tag_query("ref"),
                            },
                        },
                        "App": {
                            "Type": "AWS::ApplicationInsights::Application",
                            "DependsOn": "Group",
                            "Properties": {
                                "ResourceGroupName": { "Ref": "Group" },
                                "AutoConfigurationEnabled": false,
                            },
                        },
                        "Published": {
                            "Type": "AWS::SSM::Parameter",
                            "Properties": {
                                "Name": "/ref-is-its-arn/ref",
                                "Type": "String",
                                "Value": { "Ref": "App" },
                            },
                        },
                    },
                    "Outputs": {
                        "AppArn": { "Value": { "Ref": "App" } },
                        "AppArnAttribute": {
                            "Value": { "Fn::GetAtt": ["App", "ApplicationARN"] }
                        },
                        "AppArnSubstituted": { "Value": { "Fn::Sub": "${App}" } },
                    },
                })
                .to_string()
            },
            logical_id: "App",
            arn_outputs: &["AppArn", "AppArnAttribute", "AppArnSubstituted"],
            parameter: "/ref-is-its-arn/ref",
        },
        Row {
            // The group's OWN name is only known once the stack's parameters are resolved, and
            // the application's identity only once the group has one — two levels, and the
            // published identifier still has to be the ARN. The name carries the dots,
            // underscores and digits the modelled `ResourceGroupName` pattern admits.
            case: "a group whose own name comes from a stack parameter",
            region: "eu-central-1",
            group: "ai.param_group-9",
            group_created_first: false,
            stack_parameters: &[("GroupName", "ai.param_group-9")],
            template: || {
                json!({
                    "Parameters": { "GroupName": { "Type": "String" } },
                    "Resources": {
                        "Group": {
                            "Type": "AWS::ResourceGroups::Group",
                            "Properties": {
                                "Name": { "Ref": "GroupName" },
                                "ResourceQuery": tag_query("parameter"),
                            },
                        },
                        "Monitored": {
                            "Type": "AWS::ApplicationInsights::Application",
                            "Properties": {
                                "ResourceGroupName": { "Ref": "Group" },
                                "CWEMonitorEnabled": true,
                            },
                        },
                        "Published": {
                            "Type": "AWS::SSM::Parameter",
                            "Properties": {
                                "Name": "/ref-is-its-arn/parameter",
                                "Type": "String",
                                "Value": { "Fn::GetAtt": ["Monitored", "ApplicationARN"] },
                            },
                        },
                    },
                    "Outputs": { "AppArn": { "Value": { "Ref": "Monitored" } } },
                })
                .to_string()
            },
            logical_id: "Monitored",
            arn_outputs: &["AppArn"],
            parameter: "/ref-is-its-arn/parameter",
        },
        Row {
            case: "a group named through Fn::Sub over its Ref",
            region: "ap-northeast-1",
            group: "ai-sub-group",
            group_created_first: false,
            stack_parameters: &[],
            template: || {
                json!({
                    "Resources": {
                        "Group": {
                            "Type": "AWS::ResourceGroups::Group",
                            "Properties": {
                                "Name": "ai-sub-group",
                                "ResourceQuery": tag_query("substituted"),
                            },
                        },
                        "App": {
                            "Type": "AWS::ApplicationInsights::Application",
                            "Properties": {
                                "ResourceGroupName": { "Fn::Sub": "${Group}" },
                                "OpsCenterEnabled": false,
                            },
                        },
                        "Published": {
                            "Type": "AWS::SSM::Parameter",
                            "Properties": {
                                "Name": "/ref-is-its-arn/substituted",
                                "Type": "String",
                                "Value": { "Ref": "App" },
                            },
                        },
                    },
                    "Outputs": {
                        "AppArnAttribute": {
                            "Value": { "Fn::GetAtt": ["App", "ApplicationARN"] }
                        },
                        "AppArn": { "Value": { "Ref": "App" } },
                    },
                })
                .to_string()
            },
            logical_id: "App",
            arn_outputs: &["AppArn", "AppArnAttribute"],
            parameter: "/ref-is-its-arn/substituted",
        },
        Row {
            // A group a CLIENT created beforehand, whose name the declaration composes with
            // `Fn::Join` rather than reading off another resource: the identifier is settled in
            // the first pass, and it is the same ARN.
            case: "a client's own group, named through Fn::Join",
            region: "us-east-1",
            group: "ai-joined-group",
            group_created_first: true,
            stack_parameters: &[],
            template: || {
                json!({
                    "Resources": {
                        "App": {
                            "Type": "AWS::ApplicationInsights::Application",
                            "Properties": {
                                "ResourceGroupName": {
                                    "Fn::Join": ["-", ["ai", "joined", "group"]]
                                },
                            },
                        },
                        "Published": {
                            "Type": "AWS::SSM::Parameter",
                            "Properties": {
                                "Name": "/ref-is-its-arn/joined",
                                "Type": "String",
                                "Value": { "Ref": "App" },
                            },
                        },
                    },
                    "Outputs": {
                        "AppArn": { "Value": { "Ref": "App" } },
                        "AppArnSubstituted": { "Value": { "Fn::Sub": "${App}" } },
                    },
                })
                .to_string()
            },
            logical_id: "App",
            arn_outputs: &["AppArn", "AppArnSubstituted"],
            parameter: "/ref-is-its-arn/joined",
        },
    ]
}

/// Every reader of the published identifier answers the application ARN — the physical id a
/// resource listing reports, each output the template publishes over the resource, and the
/// parameter another resource of the stack was fed it into — and none of them answers the
/// generated `<stack>-<logical-id>-<suffix>` name.
#[tokio::test]
async fn the_published_identifier_of_an_applicationinsights_application_is_its_arn() {
    let router = build_router();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("ai-ref-arn-stack-{index}");
        let expected_arn = application_arn(region, row.group);

        if row.group_created_first {
            create_group(&router, region, row.group).await;
        }

        let mut members = vec![
            ("StackName".to_string(), stack_name.clone()),
            ("TemplateBody".to_string(), (row.template)()),
        ];
        for (position, (key, value)) in row.stack_parameters.iter().enumerate() {
            let slot = position + 1;
            members.push((
                format!("Parameters.member.{slot}.ParameterKey"),
                (*key).to_string(),
            ));
            members.push((
                format!("Parameters.member.{slot}.ParameterValue"),
                (*value).to_string(),
            ));
        }
        let (status, body) = cloudformation(&router, region, "CreateStack", &members).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let (stack_status, described) = settled_stack(&router, region, &stack_name).await;
        assert_eq!(
            stack_status, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );

        // The physical resource id a client lists the stack's resources for is the ARN, which
        // carries the fixed `application/resource-group/` segment and the group the declaration
        // resolved to — not the generated name the stack would mint for a resource whose
        // identifier it had to choose.
        let (status, listed) = on_stack(&router, region, "ListStackResources", &stack_name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ListStackResources, got {status} with body \
             {listed}"
        );
        let physical_ids = physical_ids_of(&listed);
        let physical_id = physical_ids
            .get(row.logical_id)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: no physical id for {} in {listed}",
                    row.logical_id
                )
            });
        assert_eq!(
            physical_id, expected_arn,
            "case {case:?}: expected the physical id to be the application ARN {expected_arn}, \
             got {physical_id:?}"
        );
        assert!(
            physical_id.starts_with("arn:")
                && physical_id.contains(APPLICATION_PATH)
                && !physical_id.contains(&stack_name),
            "case {case:?}: expected an application ARN carrying {APPLICATION_PATH} and nothing \
             of the stack name {stack_name}, got {physical_id:?}"
        );

        // Every output the template publishes over the resource carries that same string,
        // whichever intrinsic reads it.
        let outputs = outputs_of(&described);
        for key in row.arn_outputs {
            assert_eq!(
                outputs.get(*key).map(String::as_str),
                Some(expected_arn.as_str()),
                "case {case:?}: expected output {key:?} to be {expected_arn}, got {outputs:?}"
            );
        }

        // And so does the property of ANOTHER resource of the stack that was fed the identifier:
        // what the deployment handed on is addressable, which is the point of publishing the ARN.
        let (status, read) = ssm(
            &router,
            region,
            "GetParameter",
            &json!({ "Name": row.parameter }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetParameter({}) to answer 200, got {status} with {read}",
            row.parameter
        );
        assert_eq!(
            read.get("Parameter")
                .and_then(|parameter| parameter.get("Value"))
                .and_then(Value::as_str),
            Some(expected_arn.as_str()),
            "case {case:?}: expected the parameter the stack wrote to hold {expected_arn}, got \
             {read}"
        );
    }
}
