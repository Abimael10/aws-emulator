// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that reports `CREATE_COMPLETE` has really created the
//! `AWS::CodeBuild::Fleet` resources its template declares — in CodeBuild, with the
//! configuration the declaration carries.
//!
//! The rule under test: **a stack reports a resource as created only where the service that
//! owns it created it, through that service's own public API and with every declared property
//! applied, so the handle the stack publishes addresses a resource that service really holds —
//! and deleting the stack releases it there.** For a compute fleet the handle is its ARN ("Ref
//! returns the fleet ARN"), a value carrying the `<name>:<id>` identifier only CodeBuild
//! mints, so right after the create waiter returns `ListFleets` reports that ARN and
//! `BatchGetFleets` resolves it to the fleet the template described — rather than CodeBuild
//! reporting the ARN of its own completed stack in `fleetsNotFound`. Two crates have to agree
//! for that to hold (CloudFormation makes the calls, CodeBuild serves them), so the suite
//! lives here: a crate-local test can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES the fleet
//! or leaves the name to CloudFormation, whether it is written in JSON or YAML, which
//! environment and compute type it declares, how much capacity it asks for, whether it carries
//! an overflow behaviour, a scaling configuration, a VPC configuration or a proxy
//! configuration, whether it declares tags, and the Region the stack is deployed in.
//!
//! Every wait here is a poll for the CONDITION — the status a stack or a fleet reaches on the
//! emulator's own clock — under a deadline generous enough for a loaded host; nothing asserts
//! how long a step took.
//!
//! The awsQuery form encoder and the XML element readers are the test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{form_encode, opt_element, outputs_of, physical_ids_of};

/// How long one poll waits before looking again, and how long the looking goes on for.
///
/// A stack's own status transitions and a fleet's move out of `CREATING` are owed on those
/// services' clocks, so every wait below looks for the CONDITION; the deadline is only here so
/// a premise that never holds fails rather than hangs, and it is many times the spans involved
/// so a loaded host cannot decide the outcome.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
/// See [`POLL_INTERVAL`].
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The bounds CodeBuild holds a compute fleet's name to (`FleetName`,
/// `@length(min: 2, max: 128)` with pattern `^[A-Za-z0-9][A-Za-z0-9\-_]{1,127}$` in that
/// service's own model), which a GENERATED name is held to as well.
const MIN_FLEET_NAME: usize = 2;
const MAX_FLEET_NAME: usize = 128;

/// The status a compute fleet whose capacity is provisioned reports, and the one it reports
/// while that is still in flight (`FleetStatus$statusCode`).
const FLEET_ACTIVE: &str = "ACTIVE";
const FLEET_CREATING: &str = "CREATING";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
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
        service_com_amazonaws_codebuild_codebuild_20161006::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which is
/// what routes the request and what each handler reads its Region from. The signature is never
/// checked: this router runs with request authentication off.
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

/// Call one CodeBuild operation over its awsJson1_1 wire, signed for `region`.
async fn codebuild(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("codebuild.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CodeBuild_20161006.{operation}"))
        .header("authorization", authorization("codebuild", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Look at `DescribeStacks` until the stack reports a TERMINAL status, and answer with that
/// answer. A status still in progress is looked at again until [`POLL_DEADLINE`] passes.
async fn settled_stack(router: &MultiServiceRouter, region: &str, stack: &str) -> String {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "expected the stack {stack:?} to settle into a terminal status, got {reported:?} \
             with answer {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The fleet CodeBuild holds for `handle`, or `None` where it reports the handle in
/// `fleetsNotFound` — the answer's own partition, read the way a client reads it.
async fn held_fleet(
    router: &MultiServiceRouter,
    region: &str,
    handle: &str,
) -> (Option<Value>, Vec<String>) {
    let (status, answer) =
        codebuild(router, region, "BatchGetFleets", &json!({ "names": [handle] })).await;
    assert_eq!(
        status, 200,
        "BatchGetFleets {handle:?}: expected HTTP 200, got {status} with body {answer}"
    );
    let held = answer
        .get("fleets")
        .and_then(Value::as_array)
        .and_then(|fleets| fleets.first())
        .cloned();
    let not_found = answer
        .get("fleetsNotFound")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    (held, not_found)
}

/// The ARNs `ListFleets` reports for the account in `region`.
async fn listed_fleets(router: &MultiServiceRouter, region: &str) -> Vec<String> {
    let (status, answer) = codebuild(router, region, "ListFleets", &json!({})).await;
    assert_eq!(
        status, 200,
        "ListFleets: expected HTTP 200, got {status} with body {answer}"
    );
    answer
        .get("fleets")
        .and_then(Value::as_array)
        .map(|arns| {
            arns.iter()
                .filter_map(|arn| arn.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Look at the fleet until it reports `FLEET_ACTIVE`, and answer with it: the move out of
/// `CREATING` falls due on CodeBuild's own clock, so this waits for the STATUS.
async fn active_fleet(router: &MultiServiceRouter, region: &str, arn: &str) -> Value {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (held, not_found) = held_fleet(router, region, arn).await;
        let held = held.unwrap_or_else(|| {
            panic!("expected BatchGetFleets to report the fleet for {arn}, got {not_found:?}")
        });
        let status = held
            .get("status")
            .and_then(|status| status.get("statusCode"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if status == FLEET_ACTIVE {
            return held;
        }
        assert_eq!(
            status, FLEET_CREATING,
            "expected the fleet {arn} to be provisioning or active, got {held}"
        );
        assert!(
            Instant::now() < deadline,
            "expected the fleet {arn} to reach {FLEET_ACTIVE}, got {held}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look at `ListFleets` until it no longer reports `arn`: the removal of a deleted fleet falls
/// due on CodeBuild's own clock, so this waits for the CONDITION.
async fn released_fleet(router: &MultiServiceRouter, region: &str, arn: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let listed = listed_fleets(router, region).await;
        if !listed.contains(&arn.to_string()) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected the fleet {arn} to be gone once the stack deleted it, got ListFleets \
             {listed:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the table: a template, and what the rule says the fleet it declares must then be
/// in CodeBuild.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the fleet is created in and the one
    /// its ARN carries.
    region: &'static str,
    /// The template body, in the form the row writes it (JSON or YAML).
    template: &'static str,
    /// The name the template declares, or `None` for a template that names nothing — whose
    /// fleet carries the name CloudFormation generated.
    declared_name: Option<&'static str>,
    /// The members `BatchGetFleets` must report for the fleet, built from what the row
    /// declares: the capacity, the types, the overflow behaviour and each nested
    /// configuration, under the member names CodeBuild's own model spells.
    expected_fleet: fn() -> Value,
    /// The tags the fleet's own declaration asks for.
    expected_tags: &'static [(&'static str, &'static str)],
}

/// The rows, built here because a `Value` is not a `const`.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a named LINUX_CONTAINER fleet in JSON, the smallest capacity, with tags",
            region: "us-west-2",
            template: r#"{
                "AWSTemplateFormatVersion": "2010-09-09",
                "Description": "a CodeBuild compute fleet",
                "Resources": {
                    "Builders": {
                        "Type": "AWS::CodeBuild::Fleet",
                        "Properties": {
                            "Name": "unit-test-builders",
                            "BaseCapacity": 1,
                            "EnvironmentType": "LINUX_CONTAINER",
                            "ComputeType": "BUILD_GENERAL1_SMALL",
                            "Tags": [
                                { "Key": "Owner", "Value": "orders" },
                                { "Key": "Tier", "Value": "prod" }
                            ]
                        }
                    }
                },
                "Outputs": {
                    "FleetRef": { "Value": { "Ref": "Builders" } }
                }
            }"#,
            declared_name: Some("unit-test-builders"),
            expected_fleet: || {
                json!({
                    "baseCapacity": 1,
                    "environmentType": "LINUX_CONTAINER",
                    "computeType": "BUILD_GENERAL1_SMALL",
                })
            },
            expected_tags: &[("Owner", "orders"), ("Tier", "prod")],
        },
        Row {
            case: "a named ARM_CONTAINER fleet in YAML queueing its overflow, scaling to a \
                   ceiling, in a VPC",
            region: "us-east-1",
            template: "Resources:\n  \
                       Builders:\n    \
                       Type: AWS::CodeBuild::Fleet\n    \
                       Properties:\n      \
                       Name: arm-builders\n      \
                       BaseCapacity: 2\n      \
                       EnvironmentType: ARM_CONTAINER\n      \
                       ComputeType: BUILD_GENERAL1_LARGE\n      \
                       OverflowBehavior: QUEUE\n      \
                       ScalingConfiguration:\n        \
                       ScalingType: TARGET_TRACKING_SCALING\n        \
                       MaxCapacity: 5\n        \
                       TargetTrackingScalingConfigs:\n          \
                       - MetricType: FLEET_UTILIZATION_RATE\n            \
                       TargetValue: 80\n      \
                       VpcConfig:\n        \
                       VpcId: vpc-0123456789abcdef0\n        \
                       Subnets:\n          \
                       - subnet-0123456789abcdef0\n        \
                       SecurityGroupIds:\n          \
                       - sg-0123456789abcdef0\n\
                       Outputs:\n  \
                       FleetRef:\n    \
                       Value: !Ref Builders\n",
            declared_name: Some("arm-builders"),
            expected_fleet: || {
                json!({
                    "baseCapacity": 2,
                    "environmentType": "ARM_CONTAINER",
                    "computeType": "BUILD_GENERAL1_LARGE",
                    "overflowBehavior": "QUEUE",
                    "scalingConfiguration": {
                        "scalingType": "TARGET_TRACKING_SCALING",
                        "maxCapacity": 5,
                        // The capacity the fleet WANTS, which `ScalingConfigurationOutput`
                        // adds to what the declaration stated: at creation it is the
                        // capacity the fleet was provisioned at.
                        "desiredCapacity": 2,
                        "targetTrackingScalingConfigs": [
                            {"metricType": "FLEET_UTILIZATION_RATE", "targetValue": 80.0},
                        ],
                    },
                    "vpcConfig": {
                        "vpcId": "vpc-0123456789abcdef0",
                        "subnets": ["subnet-0123456789abcdef0"],
                        "securityGroupIds": ["sg-0123456789abcdef0"],
                    },
                })
            },
            expected_tags: &[],
        },
        Row {
            case: "a fleet the template does not name, running on-demand overflow behind a \
                   proxy configuration",
            region: "eu-west-1",
            template: r#"{
                "Resources": {
                    "Builders": {
                        "Type": "AWS::CodeBuild::Fleet",
                        "Properties": {
                            "BaseCapacity": 3,
                            "EnvironmentType": "LINUX_CONTAINER",
                            "ComputeType": "BUILD_GENERAL1_MEDIUM",
                            "OverflowBehavior": "ON_DEMAND",
                            "ProxyConfiguration": {
                                "DefaultBehavior": "ALLOW_ALL",
                                "OrderedProxyRules": [
                                    {
                                        "Type": "DOMAIN",
                                        "Effect": "DENY",
                                        "Entities": ["example.com"]
                                    }
                                ]
                            },
                            "Tags": { "Owner": "platform" }
                        }
                    }
                },
                "Outputs": {
                    "FleetRef": { "Value": { "Ref": "Builders" } }
                }
            }"#,
            declared_name: None,
            expected_fleet: || {
                json!({
                    "baseCapacity": 3,
                    "environmentType": "LINUX_CONTAINER",
                    "computeType": "BUILD_GENERAL1_MEDIUM",
                    "overflowBehavior": "ON_DEMAND",
                    "proxyConfiguration": {
                        "defaultBehavior": "ALLOW_ALL",
                        "orderedProxyRules": [
                            {"type": "DOMAIN", "effect": "DENY", "entities": ["example.com"]},
                        ],
                    },
                })
            },
            expected_tags: &[("Owner", "platform")],
        },
        Row {
            case: "a fleet whose machine is described by compute ATTRIBUTES rather than named \
                   by the compute type",
            region: "ap-southeast-2",
            template: r#"{
                "Resources": {
                    "Builders": {
                        "Type": "AWS::CodeBuild::Fleet",
                        "Properties": {
                            "Name": "attribute-based-builders",
                            "BaseCapacity": 1,
                            "EnvironmentType": "LINUX_EC2",
                            "ComputeType": "ATTRIBUTE_BASED_COMPUTE",
                            "ComputeConfiguration": {
                                "vCpu": 4,
                                "memory": 8,
                                "disk": 50,
                                "machineType": "GENERAL"
                            },
                            "FleetServiceRole": "arn:aws:iam::000000000000:role/codebuild-fleet",
                            "ImageId": "ami-0123456789abcdef0"
                        }
                    }
                },
                "Outputs": {
                    "FleetRef": { "Value": { "Ref": "Builders" } }
                }
            }"#,
            declared_name: Some("attribute-based-builders"),
            expected_fleet: || {
                json!({
                    "baseCapacity": 1,
                    "environmentType": "LINUX_EC2",
                    "computeType": "ATTRIBUTE_BASED_COMPUTE",
                    "computeConfiguration": {
                        "vCpu": 4,
                        "memory": 8,
                        "disk": 50,
                        "machineType": "GENERAL",
                    },
                    "fleetServiceRole": "arn:aws:iam::000000000000:role/codebuild-fleet",
                    "imageId": "ami-0123456789abcdef0",
                })
            },
            expected_tags: &[],
        },
    ]
}

/// Every row: the identifier the stack reports for its `AWS::CodeBuild::Fleet` is a compute
/// fleet CodeBuild really holds under that ARN, configured as the template declared it and
/// reported by that service's own `ListFleets`; and deleting the stack releases it there.
#[tokio::test]
async fn a_stack_provisions_the_codebuild_fleet_its_template_declares() {
    let router = build_router();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("codebuild-fleet-stack-{index}");

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", row.template),
                ("Tags.member.1.Key", "Stack"),
                ("Tags.member.1.Value", "codebuild-fleet-suite"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let described = settled_stack(&router, region, &stack_name).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );

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
        let physical_ids = physical_ids_of(&resources);
        let arn = physical_ids
            .get("Builders")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no physical id for Builders in {resources}"));

        // "Ref returns the fleet ARN": the physical id is the ARN CodeBuild minted, which
        // carries the `fleet/<name>:<id>` path of that service's own ARN for a fleet.
        let prefix = format!("arn:aws:codebuild:{region}:{ACCOUNT}:fleet/");
        let path = arn.strip_prefix(&prefix).unwrap_or_else(|| {
            panic!("case {case:?}: expected a fleet ARN under {prefix:?}, got {arn:?}")
        });
        let (name, minted) = path.split_once(':').unwrap_or_else(|| {
            panic!("case {case:?}: expected a <name>:<id> fleet path, got {path:?}")
        });
        assert!(
            !minted.is_empty(),
            "case {case:?}: expected the ARN to carry the identifier CodeBuild minted, got \
             {arn:?}"
        );
        if let Some(declared) = row.declared_name {
            assert_eq!(
                name, declared,
                "case {case:?}: expected the fleet to be created under the declared name \
                 {declared:?}, got {name:?}"
            );
        }
        let name_length = name.chars().count();
        assert!(
            (MIN_FLEET_NAME..=MAX_FLEET_NAME).contains(&name_length),
            "case {case:?}: expected a name inside CodeBuild's own FleetName rule, got {name:?}"
        );

        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get("FleetRef"),
            Some(&arn),
            "case {case:?}: expected Ref to be the fleet ARN {arn:?}, got {:?}",
            outputs.get("FleetRef")
        );

        // CodeBuild itself reports the fleet the stack published, and reaches the status a
        // provisioned fleet rests in.
        let listed = listed_fleets(&router, region).await;
        assert!(
            listed.contains(&arn),
            "case {case:?}: expected ListFleets to report the stack's fleet {arn:?}, got \
             {listed:?}"
        );
        let held = active_fleet(&router, region, &arn).await;
        assert_eq!(
            held.get("arn").and_then(Value::as_str),
            Some(arn.as_str()),
            "case {case:?}: expected the fleet's ARN to be {arn:?}, got {held}"
        );
        assert_eq!(
            held.get("name").and_then(Value::as_str),
            Some(name),
            "case {case:?}: expected the fleet to carry the name it was created under, got \
             {held}"
        );

        // Every member the declaration stated reached CodeBuild under the name its own model
        // spells, and the fleet reports it back.
        let expected = (row.expected_fleet)();
        for (member, value) in expected.as_object().expect("an object of members") {
            assert_eq!(
                held.get(member),
                Some(value),
                "case {case:?}: expected the fleet's {member:?} to be {value}, got {held}"
            );
        }

        // The tags the declaration asked for are on the fleet, beside the stack's own and the
        // three CloudFormation records itself — CodeBuild's own `Tag` shape admits that key
        // namespace, so nothing is left off.
        let reported: BTreeMap<String, String> = held
            .get("tags")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: no 'tags' list in {held}"))
            .iter()
            .filter_map(|tag| {
                Some((
                    tag.get("key")?.as_str()?.to_string(),
                    tag.get("value")?.as_str()?.to_string(),
                ))
            })
            .collect();
        for (key, value) in row.expected_tags {
            assert_eq!(
                reported.get(*key).map(String::as_str),
                Some(*value),
                "case {case:?}: expected tag {key:?} to be {value:?}, got {reported:?}"
            );
        }
        assert_eq!(
            reported.get("Stack").map(String::as_str),
            Some("codebuild-fleet-suite"),
            "case {case:?}: expected the stack's own tag to be propagated, got {reported:?}"
        );
        assert_eq!(
            reported
                .get("aws:cloudformation:logical-id")
                .map(String::as_str),
            Some("Builders"),
            "case {case:?}: expected CloudFormation's own record of the logical id, got \
             {reported:?}"
        );

        // Deleting the stack releases the fleet CodeBuild holds for it.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        released_fleet(&router, region, &arn).await;
        let (held, not_found) = held_fleet(&router, region, &arn).await;
        assert!(
            held.is_none() && not_found == vec![arn.clone()],
            "case {case:?}: expected the fleet to be gone once the stack deleted it, got \
             {held:?} with fleetsNotFound {not_found:?}"
        );
    }
}

/// A declaration CodeBuild REFUSES does not report a resource nothing created: the stack fails
/// with that service's own refusal, and no fleet the declaration named is in CodeBuild
/// afterwards.
///
/// The rows differ along WHAT is refused: a capacity below the floor a fleet's machines are
/// counted against, a scaling ceiling under the capacity the fleet is provisioned at, a
/// compute type whose machine must be described by attributes with no attributes declared, a
/// name a client already holds, and a property this emulator maps onto no member of that
/// create.
#[tokio::test]
async fn a_fleet_the_owning_service_refuses_fails_the_stack() {
    let router = build_router();
    let region = "us-east-2";

    // The fleet a client already holds, which one row re-declares.
    let (status, created) = codebuild(
        &router,
        region,
        "CreateFleet",
        &json!({
            "name": "already-there",
            "baseCapacity": 1,
            "environmentType": "LINUX_CONTAINER",
            "computeType": "BUILD_GENERAL1_SMALL",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the client's own CreateFleet to succeed, got {status} with body {created}"
    );
    let client_arn = created
        .get("fleet")
        .and_then(|fleet| fleet.get("arn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected the client's own fleet to carry an ARN, got {created}"))
        .to_string();

    // (case, the resource's declared properties)
    let rows: Vec<(&str, Value)> = vec![
        (
            "a base capacity below the floor a fleet's machines are counted against",
            json!({
                "Name": "no-machines",
                "BaseCapacity": 0,
                "EnvironmentType": "LINUX_CONTAINER",
                "ComputeType": "BUILD_GENERAL1_SMALL",
            }),
        ),
        (
            "a scaling ceiling under the capacity the fleet is provisioned at",
            json!({
                "Name": "empty-range",
                "BaseCapacity": 4,
                "EnvironmentType": "LINUX_CONTAINER",
                "ComputeType": "BUILD_GENERAL1_SMALL",
                "ScalingConfiguration": {"ScalingType": "TARGET_TRACKING_SCALING", "MaxCapacity": 2},
            }),
        ),
        (
            "an attribute-based compute type with no compute configuration",
            json!({
                "Name": "no-attributes",
                "BaseCapacity": 1,
                "EnvironmentType": "LINUX_EC2",
                "ComputeType": "ATTRIBUTE_BASED_COMPUTE",
            }),
        ),
        (
            "a fleet name a client already holds",
            json!({
                "Name": "already-there",
                "BaseCapacity": 1,
                "EnvironmentType": "LINUX_CONTAINER",
                "ComputeType": "BUILD_GENERAL1_SMALL",
            }),
        ),
        (
            "a property no arm maps onto a member of that create",
            json!({
                "Name": "unmapped-property",
                "BaseCapacity": 1,
                "EnvironmentType": "LINUX_CONTAINER",
                "ComputeType": "BUILD_GENERAL1_SMALL",
                "NotAFleetProperty": "whatever",
            }),
        ),
    ];

    for (index, (case, properties)) in rows.into_iter().enumerate() {
        let stack_name = format!("codebuild-fleet-refusal-{index}");
        let declared_name = properties
            .get("Name")
            .and_then(Value::as_str)
            .expect("every row names its fleet")
            .to_string();
        let template = json!({
            "Resources": { "Builders": {
                "Type": "AWS::CodeBuild::Fleet",
                "Properties": properties,
            }}
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

        let described = settled_stack(&router, region, &stack_name).await;
        let stack_status = opt_element(&described, "StackStatus").unwrap_or_default();
        assert!(
            stack_status.contains("ROLLBACK") || stack_status.contains("FAILED"),
            "case {case:?}: expected the stack not to report success, got {stack_status:?} in \
             {described}"
        );

        // Nothing the refused declaration asked for is in CodeBuild under the name it stated —
        // bar the fleet a client created, which the stack never owned.
        let (held, not_found) = held_fleet(&router, region, &declared_name).await;
        let held_by_a_client = declared_name == "already-there";
        assert_eq!(
            held.is_some(),
            held_by_a_client,
            "case {case:?}: expected the fleet present={held_by_a_client} after the failed \
             stack, got {held:?} with fleetsNotFound {not_found:?}"
        );
    }

    // The client's own fleet is still exactly the one it created, under its own ARN.
    let (held, not_found) = held_fleet(&router, region, &client_arn).await;
    let held = held.unwrap_or_else(|| {
        panic!(
            "expected the client's own fleet to still be reported for {client_arn}, got \
             fleetsNotFound {not_found:?}"
        )
    });
    assert_eq!(
        held.get("name").and_then(Value::as_str),
        Some("already-there"),
        "expected the client's own fleet to keep its name, got {held}"
    );
    assert_eq!(
        held.get("baseCapacity").and_then(Value::as_i64),
        Some(1),
        "expected the client's own fleet to keep the capacity it was created with, got {held}"
    );
    assert!(
        listed_fleets(&router, region).await.contains(&client_arn),
        "expected ListFleets to still report the client's own fleet {client_arn}"
    );
}
