// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::ApplicationInsights::Application` really
//! creates the application in Amazon CloudWatch Application Insights, and the handle the stack
//! hands back is the one that service answers to.
//!
//! The rule under test: **a stack reports a resource as created only where the service that OWNS
//! it created it, through that service's own public API, under the configuration the template
//! declared — so `Ref` and `Fn::GetAtt` resolve to the identifier the type's published return
//! values define, that service's own reads resolve the resource, a declaration it REFUSES fails
//! the resource and rolls the stack back carrying that service's own refusal, and deleting the
//! stack removes the resource there.** For this type the one published return value is the
//! `ApplicationARN` (`Ref` returns it, and it is the type's only `Fn::GetAtt` attribute), which
//! Application Insights builds from its own `arnNamespace`, the Region, the account and the
//! RESOURCE GROUP the application monitors — the group that service resolves in Resource Groups
//! before it holds anything. Three crates have to agree for that to hold (CloudFormation makes
//! the calls, Application Insights serves them and Resource Groups answers for the group), so
//! the suite lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: the Region deployed in, whether the
//! group is DECLARED IN THE SAME STACK (so the application's `ResourceGroupName` is a `Ref` that
//! resolves to a group the deployment itself creates) or was created by a client beforehand and
//! named as text, the form of the group's name (plain, and one carrying the dots, underscores
//! and digits the modelled `ResourceGroupName` pattern admits), which optional properties the
//! declaration carries (the two monitoring switches, auto configuration, tags, custom
//! components, or none of them), how many outputs it publishes over the resource and with which
//! intrinsic, and both template spellings (JSON and YAML).
//!
//! Every expected value is computed here from what the row declares, never read back out of the
//! answer being checked.
//!
//! The awsQuery form encoder and the XML readers are the test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, form_encode, members_of, opt_element, outputs_of, physical_ids_of};

/// How long a wait for a stack to reach a TERMINAL status may run before the suite calls it a
/// failure. What is waited for is the CONDITION — the stack out of its `_IN_PROGRESS` statuses,
/// which for this type includes the onboarding Application Insights owes the application — and
/// the deadline is only a safety net so a wait that will never be satisfied still ends. It is
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

/// The `X-Amz-Target` prefix this service's awsJson1_1 operations are named by: its service
/// shape's own name.
const TARGET_PREFIX: &str = "EC2WindowsBarleyService";

/// Resource Groups' signing name, which is its ARN namespace too.
const RESOURCE_GROUPS: &str = "resource-groups";

/// The `LifeCycle` Application Insights reports an application it is still onboarding in, which
/// a stack reported `CREATE_COMPLETE` must have waited out.
const LIFE_CYCLE_CREATING: &str = "CREATING";

/// The resource status a refused create is recorded under.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The refusal this service answers a lookup that resolved nothing with: the modelled
/// `ResourceNotFoundException`, under the status Application Insights answers every refusal
/// about what the caller asked for with (its own `scaffolded::NOT_FOUND_STATUS`).
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 400;

/// The diagnostic this service's own SDK appends to the message of the exception it raises for
/// a refused call, which is part of the message a failed resource's reason relays: the
/// comma-separated envelope naming the service and the status, the call's own id, and the
/// attempt count — one, for a refusal of the REQUEST, which no SDK retries.
fn sdk_diagnostic_prefix() -> String {
    format!("(Service: ApplicationInsights, Status Code: {NOT_FOUND_STATUS}, Request ID: ")
}
const SDK_ATTEMPT_COUNT: &str = "(SDK Attempt Count: 1)";

/// The refusal Application Insights answers a create over a group the account does not hold:
/// that service's own sentence, built from the group the request named and the caller's account.
fn group_missing_message(name: &str) -> String {
    format!("Resource Group with name {name} does not exist in {ACCOUNT}")
}

/// The ARN Application Insights addresses the application over `group` in `region` by — the one
/// total function of the partition, the namespace, the Region, the account and that group, which
/// is what this type's `Ref` returns.
fn application_arn(region: &str, group: &str) -> String {
    format!("arn:aws:{APPLICATION_INSIGHTS}:{region}:{ACCOUNT}:application/resource-group/{group}")
}

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other suite
/// here; the in-process callbacks CloudFormation and Application Insights make sign their
/// requests either way (the credential scope is what routes them).
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

/// What one JSON call answered: its HTTP status, the parsed body, and the error code the
/// envelope named.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The error code of a refusal, without the namespace an awsJson envelope qualifies it with.
    fn code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit(['#', ':'])
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The message of a refusal, whichever casing the envelope spells it with.
    fn message(&self) -> String {
        self.body
            .get("Message")
            .or_else(|| self.body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// Call one Application Insights operation over its awsJson1_1 wire, signed for `region`.
async fn application_insights(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> Answer {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header(
            "host",
            format!("{APPLICATION_INSIGHTS}.{region}.amazonaws.com"),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(APPLICATION_INSIGHTS, region))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    Answer { status, body }
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
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
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

/// Application Insights' own read of the application over `group`.
async fn describe_application(router: &MultiServiceRouter, region: &str, group: &str) -> Answer {
    application_insights(
        router,
        region,
        "DescribeApplication",
        json!({ "ResourceGroupName": group }),
    )
    .await
}

/// That same read, once Application Insights no longer holds an application over `group`:
/// looked at again every [`POLL_INTERVAL`] until the read is refused or [`TERMINAL_DEADLINE`]
/// passes, and answered either way — so a teardown the stack has not finished yet is waited for
/// as a CONDITION rather than as a span of the host's clock, and one that never finishes fails
/// on the refusal the case asserts.
async fn removed_application(router: &MultiServiceRouter, region: &str, group: &str) -> Answer {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let read = describe_application(router, region, group).await;
        if read.status != 200 || Instant::now() >= deadline {
            return read;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every resource group Application Insights reports an application for in `region`, as a
/// client walking `ListApplications` reads them.
async fn listed_applications(router: &MultiServiceRouter, region: &str) -> Vec<String> {
    let answer = application_insights(router, region, "ListApplications", json!({})).await;
    assert_eq!(
        answer.status, 200,
        "ListApplications in {region} must answer 200, got {} / {:?}",
        answer.status, answer.body
    );
    answer
        .body
        .get("ApplicationInfoList")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|info| info.get("ResourceGroupName").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect()
}

/// The names of the components Application Insights reports for the application over `group`.
async fn listed_components(router: &MultiServiceRouter, region: &str, group: &str) -> Vec<String> {
    let answer = application_insights(
        router,
        region,
        "ListComponents",
        json!({ "ResourceGroupName": group }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "ListComponents({group}) must answer 200, got {} / {:?}",
        answer.status, answer.body
    );
    answer
        .body
        .get("ApplicationComponentList")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|component| component.get("ComponentName").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect()
}

/// One row: a template, and what the rule says the application it declares must then be.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the application is created in and the
    /// one its ARN carries.
    region: &'static str,
    /// The resource group the application monitors.
    group: &'static str,
    /// Whether the group is created by a CLIENT before the stack is deployed; a row that does
    /// not declares the group in the stack itself.
    group_created_first: bool,
    /// The template body, in the form the row writes it (JSON or YAML).
    template: fn() -> String,
    /// The application's logical id in the template.
    logical_id: &'static str,
    /// The members `DescribeApplication` must report for the application, each the value the
    /// declaration states for the property of the same name.
    reported: fn() -> Value,
    /// The custom components the declaration asks for, each of which that service must report.
    custom_components: &'static [&'static str],
    /// The output keys whose value must be the application's ARN — every output the template
    /// publishes over the resource, since `Ref` and `Fn::GetAtt ApplicationARN` both resolve to
    /// it.
    arn_outputs: &'static [&'static str],
}

/// An EC2 instance ARN, the one resource type a custom component may group ("Only EC2 Instance
/// can be grouped.").
fn instance_arn(region: &str, instance: &str) -> String {
    format!("arn:aws:ec2:{region}:{ACCOUNT}:instance/{instance}")
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a group declared in the same stack, monitored with both switches on, tagged",
            region: "us-west-2",
            group: "ai-stack-group",
            group_created_first: false,
            template: || {
                json!({
                    "AWSTemplateFormatVersion": "2010-09-09",
                    "Description": "an Application Insights application over its own group",
                    "Resources": {
                        "Group": {
                            "Type": "AWS::ResourceGroups::Group",
                            "Properties": {
                                "Name": "ai-stack-group",
                                "ResourceQuery": {
                                    "Type": "TAG_FILTERS_1_0",
                                    "Query": {
                                        "ResourceTypeFilters": ["AWS::AllSupported"],
                                        "TagFilters": [
                                            { "Key": "sim2real", "Values": ["probe"] }
                                        ],
                                    },
                                },
                            },
                        },
                        "App": {
                            "Type": "AWS::ApplicationInsights::Application",
                            "Properties": {
                                "ResourceGroupName": { "Ref": "Group" },
                                "CWEMonitorEnabled": true,
                                "OpsCenterEnabled": true,
                                "Tags": [{ "Key": "Owner", "Value": "observability" }],
                            },
                        },
                    },
                    "Outputs": {
                        "AppArn": { "Value": { "Ref": "App" } },
                        "AppArnAttribute": {
                            "Value": { "Fn::GetAtt": ["App", "ApplicationARN"] }
                        },
                    }
                })
                .to_string()
            },
            logical_id: "App",
            reported: || json!({ "CWEMonitorEnabled": true, "OpsCenterEnabled": true }),
            custom_components: &[],
            arn_outputs: &["AppArn", "AppArnAttribute"],
        },
        Row {
            // A group a client already had, named as TEXT rather than through a `Ref`, with a
            // name carrying the dots, underscores and digits the modelled `ResourceGroupName`
            // pattern admits — and written in YAML.
            case: "a client's own group, named as text in YAML, with no optional property",
            region: "eu-west-1",
            group: "ai.client_group-7",
            group_created_first: true,
            template: || {
                [
                    "Resources:",
                    "  Monitored:",
                    "    Type: AWS::ApplicationInsights::Application",
                    "    Properties:",
                    "      ResourceGroupName: ai.client_group-7",
                    "      CWEMonitorEnabled: true",
                    "Outputs:",
                    "  AppArn:",
                    "    Value: !Ref Monitored",
                    "",
                ]
                .join("\n")
            },
            logical_id: "Monitored",
            reported: || json!({ "CWEMonitorEnabled": true }),
            custom_components: &[],
            arn_outputs: &["AppArn"],
        },
        Row {
            case: "an application with a custom component and auto configuration on",
            region: "ap-south-1",
            group: "ai-components-group",
            group_created_first: true,
            template: || {
                json!({
                    "Resources": {
                        "App": {
                            "Type": "AWS::ApplicationInsights::Application",
                            "Properties": {
                                "ResourceGroupName": "ai-components-group",
                                "AutoConfigurationEnabled": true,
                                "OpsCenterEnabled": false,
                                "CustomComponents": [{
                                    "ComponentName": "web-tier",
                                    "ResourceList": [
                                        instance_arn("ap-south-1", "i-0123456789abcdef0"),
                                        instance_arn("ap-south-1", "i-0fedcba987654321f"),
                                    ],
                                }],
                            },
                        }
                    },
                    "Outputs": {
                        "AppArnAttribute": {
                            "Value": { "Fn::GetAtt": ["App", "ApplicationARN"] }
                        }
                    }
                })
                .to_string()
            },
            logical_id: "App",
            reported: || json!({ "AutoConfigEnabled": true, "OpsCenterEnabled": false }),
            custom_components: &["web-tier"],
            arn_outputs: &["AppArnAttribute"],
        },
    ]
}

#[tokio::test]
async fn a_stack_provisions_the_applicationinsights_application_its_template_declares() {
    let router = build_router();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let group = row.group;
        let stack_name = format!("ai-application-stack-{index}");
        let template = (row.template)();

        if row.group_created_first {
            create_group(&router, region, group).await;
        }

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let (stack_status, described) = settled_stack(&router, region, &stack_name).await;
        assert_eq!(
            stack_status, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );

        // The resource's physical id is the ARN the type's `Ref` returns, which is a function of
        // the Region, the account and the group the application monitors.
        let expected_arn = application_arn(region, group);
        let (status, resources) = cloudformation(
            &router,
            region,
            "DescribeStackResources",
            &[("StackName", &stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             body {resources}"
        );
        let physical_id = physical_ids_of(&resources)
            .get(row.logical_id)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: no physical id for {} in {resources}",
                    row.logical_id
                )
            });
        assert_eq!(
            physical_id, expected_arn,
            "case {case:?}: expected the physical id to be the application ARN {expected_arn}, \
             got {physical_id:?}"
        );

        // Every output the template publishes over the resource carries that same ARN, whichever
        // intrinsic reads it.
        let outputs = outputs_of(&described);
        for key in row.arn_outputs {
            assert_eq!(
                outputs.get(*key).map(String::as_str),
                Some(expected_arn.as_str()),
                "case {case:?}: expected output {key:?} to be {expected_arn}, got {outputs:?}"
            );
        }
        assert_eq!(
            outputs.len(),
            row.arn_outputs.len(),
            "case {case:?}: expected exactly the {} output(s) the template declares, got \
             {outputs:?}",
            row.arn_outputs.len()
        );

        // Application Insights itself reports the application, over the group the declaration
        // named and carrying every member it declared.
        let read = describe_application(&router, region, group).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected DescribeApplication to resolve the application over \
             {group}, got {} / {:?}",
            read.status, read.body
        );
        let info = read
            .body
            .get("ApplicationInfo")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no ApplicationInfo in {:?}", read.body));
        assert_eq!(
            info.get("ResourceGroupName").and_then(Value::as_str),
            Some(group),
            "case {case:?}: expected the application to monitor {group}, got {info}"
        );
        for (member, declared) in (row.reported)()
            .as_object()
            .expect("the reported members are an object")
        {
            assert_eq!(
                info.get(member),
                Some(declared),
                "case {case:?}: expected {member} to be the declared {declared}, got {info}"
            );
        }
        // A stack reported `CREATE_COMPLETE` waited out the onboarding that service owes the
        // application, so the application a client then reads is past `CREATING`.
        assert_ne!(
            info.get("LifeCycle").and_then(Value::as_str),
            Some(LIFE_CYCLE_CREATING),
            "case {case:?}: expected the onboarding to be finished once the stack completed, \
             got {info}"
        );

        // The account's own listing holds it, so a client walking `ListApplications` finds the
        // application the stack created.
        let listed = listed_applications(&router, region).await;
        assert!(
            listed.iter().any(|reported| reported == group),
            "case {case:?}: expected ListApplications to report the application over {group}, \
             got {listed:?}"
        );

        // A custom component the declaration asked for is a component that service reports: the
        // second call the create makes really reached it.
        let components = listed_components(&router, region, group).await;
        for component in row.custom_components {
            assert!(
                components.iter().any(|reported| reported == component),
                "case {case:?}: expected ListComponents to report {component:?}, got \
                 {components:?}"
            );
        }

        // Deleting the stack deletes the application Application Insights holds for it. The
        // teardown is the stack's own work, so what is waited for is the CONDITION — that
        // service no longer resolving the application — looked at again under the same
        // deadline the create was settled under.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        let gone = removed_application(&router, region, group).await;
        assert_eq!(
            (gone.status, gone.code().as_str()),
            (NOT_FOUND_STATUS, NOT_FOUND_CODE),
            "case {case:?}: expected DescribeApplication to resolve nothing over {group} once \
             the stack was deleted, got {} / {:?}",
            gone.status,
            gone.body
        );
    }
}

/// The group names a rejected row names: every one of them a group no account here holds,
/// spanning how such a name is spelled — plain, carrying the punctuation the modelled
/// `ResourceGroupName` pattern admits, the shortest the modelled 1..256 length admits, and the
/// longest.
fn absent_groups() -> Vec<(&'static str, String)> {
    vec![
        ("a plain name nothing created", "ai-ghost-group".to_string()),
        (
            "a dotted, underscored, digit-bearing name",
            "ai.ghost_group-9".to_string(),
        ),
        ("the shortest name the member admits", "g".to_string()),
        (
            "the longest name the member admits",
            format!("ai-ghost-{}", "a".repeat(247)),
        ),
    ]
}

/// A stack whose application names a resource group that does not exist does not report a
/// resource nothing created: Application Insights refuses the create, the application is
/// recorded `CREATE_FAILED` carrying that service's own not-found sentence, the stack settles
/// `ROLLBACK_COMPLETE`, and nothing is left behind for the group it named.
#[tokio::test]
async fn an_application_over_a_missing_resource_group_rolls_the_stack_back() {
    let router = build_router();
    let region = "us-east-2";

    for (index, (case, absent)) in absent_groups().into_iter().enumerate() {
        let stack_name = format!("ai-missing-group-stack-{index}");
        let template = json!({
            "Resources": {
                "App": {
                    "Type": "AWS::ApplicationInsights::Application",
                    "Properties": { "ResourceGroupName": absent },
                }
            }
        })
        .to_string();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", &stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let (stack_status, described) = settled_stack(&router, region, &stack_name).await;
        assert_eq!(
            stack_status, "ROLLBACK_COMPLETE",
            "case {case:?}: expected an application over a group that does not exist to roll the \
             stack back, got {described}"
        );

        // The failure is recorded against the APPLICATION, with Application Insights' own
        // refusal as its reason.
        let (status, events) = cloudformation(
            &router,
            region,
            "DescribeStackEvents",
            &[("StackName", &stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body \
             {events}"
        );
        let failed: Vec<(String, String)> = members_of(&events)
            .into_iter()
            .filter(|event| block_of(event, "ResourceStatus") == Some(CREATE_FAILED))
            .map(|event| {
                (
                    block_of(event, "LogicalResourceId")
                        .unwrap_or_default()
                        .to_string(),
                    block_of(event, "ResourceStatusReason")
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .filter(|(logical_id, _)| logical_id != &stack_name)
            .collect();
        assert_eq!(
            failed.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["App"],
            "case {case:?}: expected the application to be the one resource recorded \
             {CREATE_FAILED}, got {failed:?}"
        );
        let expected = group_missing_message(&absent);
        let reason = &failed[0].1;
        assert!(
            reason.contains(&expected),
            "case {case:?}: expected the failure reason to relay {expected:?}, got {reason:?}"
        );
        // And to relay it as that service's own SDK raises it: its sentence, then the
        // diagnostic of the refused call.
        let diagnostic = format!("{expected} {}", sdk_diagnostic_prefix());
        assert!(
            reason.contains(&diagnostic),
            "case {case:?}: expected the reason to carry the SDK diagnostic {diagnostic:?}, got \
             {reason:?}"
        );
        assert!(
            reason.contains(SDK_ATTEMPT_COUNT),
            "case {case:?}: expected the reason to close the diagnostic with \
             {SDK_ATTEMPT_COUNT:?}, got {reason:?}"
        );

        // And nothing was left behind for the group the template named.
        let read = describe_application(&router, region, &absent).await;
        assert_eq!(
            (read.status, read.code().as_str()),
            (NOT_FOUND_STATUS, NOT_FOUND_CODE),
            "case {case:?}: expected no application over {absent:?}, got {} / {:?}",
            read.status,
            read.body
        );
    }

    // The control, over the same Region and the same template shape: once the group exists, the
    // identical declaration deploys and the application is really there — so what decides is
    // the group's existence and nothing about the request itself.
    let group = "ai-control-group";
    create_group(&router, region, group).await;
    let template = json!({
        "Resources": {
            "App": {
                "Type": "AWS::ApplicationInsights::Application",
                "Properties": { "ResourceGroupName": group },
            }
        }
    })
    .to_string();
    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[
            ("StackName", "ai-control-stack"),
            ("TemplateBody", &template),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the control stack's CreateStack must answer 200, got {status} with body {body}"
    );
    let (stack_status, described) = settled_stack(&router, region, "ai-control-stack").await;
    assert_eq!(
        stack_status, "CREATE_COMPLETE",
        "the control stack must deploy green, got {described}"
    );
    let read = describe_application(&router, region, group).await;
    assert_eq!(
        read.status, 200,
        "the control stack's application must resolve, got {} / {:?}",
        read.status, read.body
    );
    assert_eq!(
        read.body
            .get("ApplicationInfo")
            .and_then(|info| info.get("ResourceGroupName"))
            .and_then(Value::as_str),
        Some(group),
        "the control application must monitor {group}, got {:?}",
        read.body
    );
    // The refusal a missing group earns is Application Insights' own, so a client's direct call
    // over one of those names answers exactly the same sentence the stack's reason relayed.
    let refused = application_insights(
        &router,
        region,
        "CreateApplication",
        json!({ "ResourceGroupName": "ai-ghost-group" }),
    )
    .await;
    assert_eq!(
        refused.message(),
        group_missing_message("ai-ghost-group"),
        "a client's own create over a missing group must answer that service's sentence, got \
         {:?}",
        refused.body
    );
}
