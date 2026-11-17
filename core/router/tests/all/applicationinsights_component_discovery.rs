// SPDX-License-Identifier: CC0-1.0

//! Application Insights onboards the resources of the resource group an
//! application is created over as that application's components.
//!
//! The rule under test: creating an application over a resource group makes the
//! service onboard every resource that group resolves to as a DEFAULT
//! (standalone) `ApplicationComponent` whose `ComponentName` is the resource's
//! own ARN, unmonitored until a configuration turns monitoring on; a resource a
//! custom component groups stops being a standalone component; and the
//! application's `LifeCycle` follows that monitoring — `NOT_CONFIGURED` while no
//! component is monitored, `ACTIVE` once any one is. `ListComponents` "Lists the
//! auto-grouped, standalone, and custom components of the application"
//! (smithy_query: operation ListComponents), and which resources a group holds is
//! Resource Groups' to answer, so the membership comes from there.
//!
//! This suite needs TWO services on one engine — Application Insights reaches
//! Resource Groups through the in-process callback, so a single-service router has
//! nobody to ask and the group a row names has to be creatable — which is why it
//! lives here rather than in either service's own crate, exactly as
//! `applicationinsights_resource_group_admission.rs` does for the group gate.
//!
//! The rows span what the rule generalises over: how many resources the group
//! holds (none, one, several), the resource types and ARN forms it holds (a
//! regional resource with a path, a global resource with neither region nor
//! account, a bare-id resource, and one whose type Resource Groups names none of),
//! and which component a configuration is applied to (a discovered one, a custom
//! one).
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built —
//! and the requests driven — on a sized thread. The wire itself is
//! `crate::applicationinsights_wire`, shared with the other suites of this service
//! pair.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::applicationinsights_wire::{
    create_group as create_group_with, resource_groups, Answer, BOOT_STACK_SIZE,
};

/// The region every request below is signed for, and the region every resource a
/// row names lives in.
const REGION: &str = "eu-west-2";

/// How long a poll keeps looking for the state a call owes. Generous: it bounds
/// nothing the rows assert, it only stops a broken emulator from hanging.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// One Application Insights operation of [`REGION`], over its awsJson1_1 wire.
fn application_insights(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: Value,
) -> Answer {
    crate::applicationinsights_wire::application_insights(router, runtime, REGION, operation, body)
}

/// `POST /groups` — Resource Groups' own `CreateGroup`, for a group whose service
/// configuration lets resources be grouped into it by hand (`GroupResources`
/// serves a group declaring one of the supported configuration types).
fn create_group(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime, name: &str) {
    create_group_with(
        router,
        runtime,
        REGION,
        name,
        Some(json!([{ "Type": "AWS::EC2::HostManagement" }])),
    );
}

/// `POST /group-resources` — Resource Groups' own `GroupResources`, which is what
/// makes the group hold `resource_arns`.
fn group_resources(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
    resource_arns: &[String],
) {
    if resource_arns.is_empty() {
        return;
    }
    let answer = resource_groups(
        router,
        runtime,
        REGION,
        "/group-resources",
        json!({ "Group": name, "ResourceArns": resource_arns }),
    );
    assert_eq!(
        answer.status, 200,
        "setup: GroupResources({name:?}) must succeed, got {} / {:?}",
        answer.status, answer.body
    );
    let succeeded: BTreeSet<String> = answer.body["Succeeded"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .filter_map(|arn| arn.as_str().map(str::to_string))
        .collect();
    assert_eq!(
        succeeded,
        resource_arns.iter().cloned().collect::<BTreeSet<String>>(),
        "setup: GroupResources({name:?}) must record every resource, got {:?}",
        answer.body
    );
}

/// Create the application over `name` and report what the create answered.
fn create_application(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
) -> Answer {
    let created = application_insights(
        router,
        runtime,
        "CreateApplication",
        json!({ "ResourceGroupName": name, "AutoConfigEnabled": false }),
    );
    assert_eq!(
        created.status, 200,
        "setup: CreateApplication({name:?}) must succeed, got {} / {:?}",
        created.status, created.body
    );
    created
}

/// The components `ListComponents` reports for `name`, keyed by `ComponentName`.
fn components(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
) -> Vec<Value> {
    let listed = application_insights(
        router,
        runtime,
        "ListComponents",
        json!({ "ResourceGroupName": name }),
    );
    assert_eq!(
        listed.status, 200,
        "ListComponents({name:?}) must be served, got {} / {:?}",
        listed.status, listed.body
    );
    listed.body["ApplicationComponentList"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The `ComponentName`s `ListComponents` reports for `name`.
fn component_names(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
) -> BTreeSet<String> {
    components(router, runtime, name)
        .iter()
        .filter_map(|component| component["ComponentName"].as_str().map(str::to_string))
        .collect()
}

/// The `LifeCycle` `DescribeApplication` reports for `name`.
fn life_cycle(router: &MultiServiceRouter, runtime: &tokio::runtime::Runtime, name: &str) -> String {
    let described = application_insights(
        router,
        runtime,
        "DescribeApplication",
        json!({ "ResourceGroupName": name }),
    );
    assert_eq!(
        described.status, 200,
        "DescribeApplication({name:?}) must be served, got {} / {:?}",
        described.status, described.body
    );
    described.body["ApplicationInfo"]["LifeCycle"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Look again until `ready` holds, or until [`POLL_DEADLINE`] passes — the
/// onboarding a create starts is owed by the clock, so a reader waits for the
/// CONDITION rather than for a duration.
fn poll_until(case: &str, what: &str, mut ready: impl FnMut() -> bool) {
    let since = Instant::now();
    loop {
        if ready() {
            return;
        }
        assert!(
            since.elapsed() < POLL_DEADLINE,
            "case {case:?}: {what} did not hold while polling"
        );
        std::thread::sleep(LOOK_AGAIN);
    }
}

/// An EC2 instance ARN of [`REGION`], the one resource a custom component may
/// group.
fn instance_arn(id: &str) -> String {
    format!(
        "arn:aws:ec2:{REGION}:{}:instance/{id}",
        engine::emulator_account_id()
    )
}

/// An SQS queue ARN: a resource whose ARN names its id with no type segment.
fn queue_arn(name: &str) -> String {
    format!(
        "arn:aws:sqs:{REGION}:{}:{name}",
        engine::emulator_account_id()
    )
}

/// An S3 bucket ARN: a GLOBAL resource, whose ARN carries neither a region nor
/// an account.
fn bucket_arn(name: &str) -> String {
    format!("arn:aws:s3:::{name}")
}

/// An Application Load Balancer ARN: a resource type Resource Groups names none
/// of, so the type a component reports for it is the one Application Insights
/// derives from the ARN itself.
fn load_balancer_arn(name: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{}:loadbalancer/app/{name}/0123456789abcdef",
        engine::emulator_account_id()
    )
}

/// One row: the group to create, the resources to put in it, and the
/// `ResourceType` each of those resources must be reported under.
struct Row {
    case: &'static str,
    group: &'static str,
    members: Vec<(String, Option<&'static str>)>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a group holding no resource",
            group: "ai-discovery-empty",
            members: Vec::new(),
        },
        Row {
            case: "a group holding one EC2 instance",
            group: "ai-discovery-one",
            members: vec![(
                instance_arn("i-0123456789abcdef0"),
                Some("AWS::EC2::Instance"),
            )],
        },
        Row {
            case: "a group holding resources of four different types",
            group: "ai-discovery-many",
            members: vec![
                (
                    instance_arn("i-00000000000000001"),
                    Some("AWS::EC2::Instance"),
                ),
                (bucket_arn("ai-discovery-bucket"), Some("AWS::S3::Bucket")),
                (queue_arn("ai-discovery-queue"), Some("AWS::SQS::Queue")),
                (
                    load_balancer_arn("ai-discovery-alb"),
                    Some("AWS::ElasticLoadBalancingV2::LoadBalancer"),
                ),
            ],
        },
    ]
}

#[test]
fn an_application_onboards_the_resources_of_its_resource_group() {
    std::thread::Builder::new()
        .name("applicationinsights-component-discovery".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            for row in rows() {
                let arns: Vec<String> = row
                    .members
                    .iter()
                    .map(|(arn, _)| arn.clone())
                    .collect();
                create_group(&router, &runtime, row.group);
                group_resources(&router, &runtime, row.group, &arns);
                create_application(&router, &runtime, row.group);

                // Every resource of the group is a component, named by its own
                // ARN, and nothing else is.
                let expected: BTreeSet<String> = arns.iter().cloned().collect();
                poll_until(row.case, "every resource of the group became a component", || {
                    component_names(&router, &runtime, row.group) == expected
                });
                let listed = components(&router, &runtime, row.group);
                assert_eq!(
                    listed.len(),
                    row.members.len(),
                    "case {:?}: expected {} components, got {listed:?}",
                    row.case,
                    row.members.len()
                );

                for (arn, resource_type) in &row.members {
                    let component = listed
                        .iter()
                        .find(|component| component["ComponentName"] == json!(arn))
                        .unwrap_or_else(|| {
                            panic!("case {:?}: {arn} is not a component: {listed:?}", row.case)
                        });
                    assert_eq!(
                        component["Monitor"],
                        json!(false),
                        "case {:?}: a discovered component is unmonitored until a \
                         configuration says otherwise, got {component:?}",
                        row.case
                    );
                    assert_eq!(
                        component["Tier"],
                        json!("DEFAULT"),
                        "case {:?}: a discovered component sits in the DEFAULT tier, got \
                         {component:?}",
                        row.case
                    );
                    assert_eq!(
                        component.get("ResourceType").and_then(Value::as_str),
                        *resource_type,
                        "case {:?}: expected ResourceType {resource_type:?} for {arn}, got \
                         {component:?}",
                        row.case
                    );

                    // Round trip: the discovered component is readable back one
                    // at a time, and reports the single resource it stands for.
                    let described = application_insights(
                        &router,
                        &runtime,
                        "DescribeComponent",
                        json!({ "ResourceGroupName": row.group, "ComponentName": arn }),
                    );
                    assert_eq!(
                        described.status, 200,
                        "case {:?}: DescribeComponent({arn}) must be served, got {} / {:?}",
                        row.case, described.status, described.body
                    );
                    assert_eq!(
                        described.body["ResourceList"],
                        json!([arn]),
                        "case {:?}: a discovered component groups its own resource, got {:?}",
                        row.case,
                        described.body
                    );
                    // And so is its configuration: unmonitored, DEFAULT tier,
                    // with no configuration document stored for it.
                    let configuration = application_insights(
                        &router,
                        &runtime,
                        "DescribeComponentConfiguration",
                        json!({ "ResourceGroupName": row.group, "ComponentName": arn }),
                    );
                    assert_eq!(
                        configuration.status, 200,
                        "case {:?}: DescribeComponentConfiguration({arn}) must be served, got \
                         {} / {:?}",
                        row.case, configuration.status, configuration.body
                    );
                    assert_eq!(
                        configuration.body["Monitor"],
                        json!(false),
                        "case {:?}: expected an unmonitored configuration for {arn}, got {:?}",
                        row.case,
                        configuration.body
                    );
                }

                // Nothing is monitored yet, so the onboarded application settles
                // on NOT_CONFIGURED and stays there.
                poll_until(row.case, "the application settled off CREATING", || {
                    life_cycle(&router, &runtime, row.group) != "CREATING"
                });
                let settled = life_cycle(&router, &runtime, row.group);
                assert_eq!(
                    settled, "NOT_CONFIGURED",
                    "case {:?}: an onboarded application with nothing monitored reports \
                     NOT_CONFIGURED, got {settled:?}",
                    row.case
                );
            }
        })
        .expect("spawn the component-discovery thread")
        .join()
        .expect("an application must onboard the resources of its resource group");
}

#[test]
fn a_custom_component_takes_over_the_resources_it_groups() {
    std::thread::Builder::new()
        .name("applicationinsights-custom-component".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            let group = "ai-custom-component";
            let first = instance_arn("i-0aaaaaaaaaaaaaaa1");
            let second = instance_arn("i-0aaaaaaaaaaaaaaa2");
            let bucket = bucket_arn("ai-custom-bucket");
            let queue = queue_arn("ai-custom-queue");
            let arns = vec![
                first.clone(),
                second.clone(),
                bucket.clone(),
                queue.clone(),
            ];

            create_group(&router, &runtime, group);
            group_resources(&router, &runtime, group, &arns);
            create_application(&router, &runtime, group);
            let discovered: BTreeSet<String> = arns.iter().cloned().collect();
            poll_until("setup", "every resource of the group became a component", || {
                component_names(&router, &runtime, group) == discovered
            });

            // A custom component may group only EC2 instances, so grouping the
            // bucket is refused — and a refused create changes nothing: the
            // bucket is still a standalone component.
            let refused = application_insights(
                &router,
                &runtime,
                "CreateComponent",
                json!({
                    "ResourceGroupName": group,
                    "ComponentName": "storage-tier",
                    "ResourceList": [bucket.clone()],
                }),
            );
            assert_eq!(
                (refused.status, refused.error_code().as_str()),
                (400, "ValidationException"),
                "grouping a bucket must be refused, got {} / {:?}",
                refused.status,
                refused.body
            );
            assert_eq!(
                refused.error_message(),
                "Only EC2 Instance can be grouped.",
                "expected the only-instances refusal, got {:?}",
                refused.body
            );
            assert_eq!(
                component_names(&router, &runtime, group),
                discovered,
                "a refused create must leave every discovered component where it was"
            );

            // Grouping the two instances takes them out of the standalone
            // components: each resource belongs to ONE component.
            let created = application_insights(
                &router,
                &runtime,
                "CreateComponent",
                json!({
                    "ResourceGroupName": group,
                    "ComponentName": "web-tier",
                    "ResourceList": [first.clone(), second.clone()],
                }),
            );
            assert_eq!(
                created.status, 200,
                "grouping two instances must succeed, got {} / {:?}",
                created.status, created.body
            );

            let expected: BTreeSet<String> = [
                "web-tier".to_string(),
                bucket.clone(),
                queue.clone(),
            ]
            .into_iter()
            .collect();
            assert_eq!(
                component_names(&router, &runtime, group),
                expected,
                "the grouped instances must no longer be standalone components"
            );

            // The instances are reachable only through the custom component now.
            for arn in [&first, &second] {
                let gone = application_insights(
                    &router,
                    &runtime,
                    "DescribeComponent",
                    json!({ "ResourceGroupName": group, "ComponentName": arn }),
                );
                assert_eq!(
                    (gone.status, gone.error_code().as_str()),
                    (400, "ResourceNotFoundException"),
                    "case {arn}: a grouped resource is no component of its own, got {} / {:?}",
                    gone.status,
                    gone.body
                );
                assert_eq!(
                    gone.error_message(),
                    format!("Custom component {arn} does not exist for {group}"),
                    "case {arn}: expected the missing-component refusal, got {:?}",
                    gone.body
                );
            }
            let custom = application_insights(
                &router,
                &runtime,
                "DescribeComponent",
                json!({ "ResourceGroupName": group, "ComponentName": "web-tier" }),
            );
            assert_eq!(
                custom.status, 200,
                "the custom component must be served, got {} / {:?}",
                custom.status, custom.body
            );
            assert_eq!(
                custom.body["ResourceList"],
                json!([first, second]),
                "the custom component must report the resources it grouped, got {:?}",
                custom.body
            );
            assert_eq!(
                custom.body["ApplicationComponent"]["ResourceType"],
                json!("AWS::EC2::Instance"),
                "a component of instances reports their type, got {:?}",
                custom.body
            );
        })
        .expect("spawn the custom-component thread")
        .join()
        .expect("a custom component must take over the resources it groups");
}

#[test]
fn monitoring_one_component_activates_the_application() {
    std::thread::Builder::new()
        .name("applicationinsights-component-monitoring".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a current-thread runtime");

            let instance = instance_arn("i-0bbbbbbbbbbbbbbb1");
            let bucket = bucket_arn("ai-monitoring-bucket");

            // Two rows: the configured component is one onboarding discovered,
            // and one a `CreateComponent` call grouped.
            let cases: [(&str, &str, Option<&str>); 2] = [
                ("a discovered component", "ai-monitoring-discovered", None),
                (
                    "a custom component",
                    "ai-monitoring-custom",
                    Some("web-tier"),
                ),
            ];

            for (case, group, custom) in cases {
                create_group(&router, &runtime, group);
                group_resources(
                    &router,
                    &runtime,
                    group,
                    &[instance.clone(), bucket.clone()],
                );
                create_application(&router, &runtime, group);
                poll_until(case, "the application settled off CREATING", || {
                    life_cycle(&router, &runtime, group) != "CREATING"
                });
                let settled = life_cycle(&router, &runtime, group);
                assert_eq!(
                    settled, "NOT_CONFIGURED",
                    "case {case:?}: an onboarded application with nothing monitored reports \
                     NOT_CONFIGURED, got {settled:?}"
                );

                // The component this row configures: the discovered one, or a
                // custom one grouping the same instance.
                let component = match custom {
                    None => instance.clone(),
                    Some(name) => {
                        let created = application_insights(
                            &router,
                            &runtime,
                            "CreateComponent",
                            json!({
                                "ResourceGroupName": group,
                                "ComponentName": name,
                                "ResourceList": [instance.clone()],
                            }),
                        );
                        assert_eq!(
                            created.status, 200,
                            "case {case:?}: CreateComponent must succeed, got {} / {:?}",
                            created.status, created.body
                        );
                        name.to_string()
                    }
                };
                // Creating a component configures nothing, so the application is
                // still unconfigured.
                let before = life_cycle(&router, &runtime, group);
                assert_eq!(
                    before, "NOT_CONFIGURED",
                    "case {case:?}: an unmonitored component leaves the application \
                     NOT_CONFIGURED, got {before:?}"
                );

                let configured = application_insights(
                    &router,
                    &runtime,
                    "UpdateComponentConfiguration",
                    json!({
                        "ResourceGroupName": group,
                        "ComponentName": component,
                        "Monitor": true,
                        "Tier": "DEFAULT",
                        "ComponentConfiguration": "{\"subComponents\":[]}",
                    }),
                );
                assert_eq!(
                    configured.status, 200,
                    "case {case:?}: UpdateComponentConfiguration must succeed, got {} / {:?}",
                    configured.status, configured.body
                );

                // ACTIVE with no further call: the configuration is what moves
                // the lifecycle, not elapsed time.
                let after = life_cycle(&router, &runtime, group);
                assert_eq!(
                    after, "ACTIVE",
                    "case {case:?}: a monitored component makes the application ACTIVE, got \
                     {after:?}"
                );

                // The configuration is readable back, and the other component is
                // untouched.
                let read = application_insights(
                    &router,
                    &runtime,
                    "DescribeComponentConfiguration",
                    json!({ "ResourceGroupName": group, "ComponentName": component }),
                );
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: DescribeComponentConfiguration must be served, got {} / {:?}",
                    read.status, read.body
                );
                assert_eq!(
                    read.body["Monitor"],
                    json!(true),
                    "case {case:?}: the stored configuration must report the monitoring, got {:?}",
                    read.body
                );
                assert_eq!(
                    read.body["ComponentConfiguration"],
                    json!("{\"subComponents\":[]}"),
                    "case {case:?}: the stored configuration must round-trip, got {:?}",
                    read.body
                );
                let other = application_insights(
                    &router,
                    &runtime,
                    "DescribeComponentConfiguration",
                    json!({ "ResourceGroupName": group, "ComponentName": bucket }),
                );
                assert_eq!(
                    other.body["Monitor"],
                    json!(false),
                    "case {case:?}: the other component must stay unmonitored, got {:?}",
                    other.body
                );

                // Turning the only monitored component off again returns the
                // application to NOT_CONFIGURED.
                let stopped = application_insights(
                    &router,
                    &runtime,
                    "UpdateComponentConfiguration",
                    json!({
                        "ResourceGroupName": group,
                        "ComponentName": component,
                        "Monitor": false,
                    }),
                );
                assert_eq!(
                    stopped.status, 200,
                    "case {case:?}: turning monitoring off must succeed, got {} / {:?}",
                    stopped.status, stopped.body
                );
                let unconfigured = life_cycle(&router, &runtime, group);
                assert_eq!(
                    unconfigured, "NOT_CONFIGURED",
                    "case {case:?}: with nothing monitored the application reports \
                     NOT_CONFIGURED again, got {unconfigured:?}"
                );
            }
        })
        .expect("spawn the component-monitoring thread")
        .join()
        .expect("monitoring a component must activate its application");
}
