// SPDX-License-Identifier: CC0-1.0

//! The components an Application Insights application owes the EC2 instances of
//! its resource group, and what grouping SEVERAL of them into one custom
//! component does to that set.
//!
//! The RULE pinned here: creating an application over a resource group onboards
//! every resource that group resolves to as one unmonitored standalone
//! `ApplicationComponent` named by that resource's own ARN and covering that
//! resource plus the storage it runs on, and a custom component groups EC2
//! instances ONLY and EXCLUSIVELY — every instance one `CreateComponent` call
//! names leaves the standalone set and is covered by the custom component, a call
//! naming any resource of another type is refused, a call naming any instance
//! another custom component already holds is refused and creates nothing, and
//! `DeleteComponent` gives every instance it held back as its own standalone
//! component, free to be grouped again.
//!
//! Three crates have to agree for this to hold — Amazon EC2, which owns the
//! instances and the volumes attached to them, Resource Groups, which resolves the
//! group's query, and Application Insights, which onboards what the group resolves
//! to — so the suite lives here rather than in any one of them. Nothing below
//! reaches into a store: every resource is created through its own service's wire,
//! and every assertion reads what another service reports.
//!
//! The sibling `applicationinsights_tag_filtered_group_onboarding.rs` pins the
//! rule for ONE grouped instance beside a bucket; the rows below differ along what
//! the rule generalises over and that suite does not reach: how many instances the
//! group holds (one, two, three), how many of them a single custom component
//! groups (one, two, all of them), whether any instance is left ungrouped, whether
//! the group also holds a resource of another type, and whether the refused
//! `ResourceList` mixes types or mixes a free instance with a grouped one.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built —
//! and the requests driven — on a sized thread. Every wire this suite drives — the
//! Application Insights operations, Resource Groups, and the Amazon EC2 and Amazon
//! S3 calls that build the estate — comes from `crate::applicationinsights_wire`,
//! which holds that plumbing once for every suite of these services; only the rows
//! and what they assert live here.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The wires this suite drives and the estate it onboards an application from are
// written once in `applicationinsights_wire`, which every suite of this service
// pair shares.
use crate::applicationinsights_wire as wire;
use crate::applicationinsights_wire::{Answer, BOOT_STACK_SIZE};

/// The Region every request below is signed for, and so the Region the instances,
/// the bucket, the group and the application live in.
const REGION: &str = "us-east-1";

/// The tag key every row's resources carry; each row's VALUE is what selects that
/// row's resources and nothing else.
const CASE_KEY: &str = "GroupingCase";

/// The `ResourceType` an EC2 instance's component reports, as the resource-type
/// vocabulary of `ApplicationComponent$ResourceType` names it
/// (`smithy_query(query="shape",shape="ApplicationComponent")`).
const EC2_INSTANCE: &str = "AWS::EC2::Instance";

/// What Application Insights answers a request that would group anything other
/// than EC2 instances.
const ONLY_INSTANCES: &str = "Only EC2 Instance can be grouped.";

/// The HTTP status this service answers a not-found refusal with: Application
/// Insights refuses on what the caller asked for with 400 throughout, the
/// not-found refusal included, which is what its own crate's suites pin and what
/// the error channel of this service answers whatever `@httpError` the shape
/// declares. What a row below reads off such an answer is the error CODE; the
/// status is asserted so a row cannot pass on a refusal of another class.
const NOT_FOUND_STATUS: u16 = 400;

/// The tag one row's resources carry: the case key, and the value that selects
/// that row's resources and nothing else.
fn case_tag(case_value: &str) -> [(&str, &str); 1] {
    [(CASE_KEY, case_value)]
}

/// One Application Insights operation of [`REGION`], over its awsJson1_1 wire.
fn application_insights(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: Value,
) -> Answer {
    wire::application_insights(router, runtime, REGION, operation, body)
}

/// What `DescribeComponent` answers for one component of the application over
/// `group`.
fn describe_component(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    group: &str,
    component_name: &str,
) -> Answer {
    application_insights(
        router,
        runtime,
        "DescribeComponent",
        json!({ "ResourceGroupName": group, "ComponentName": component_name }),
    )
}

/// `CreateComponent` over `resources`, as it answered.
fn create_component(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    group: &str,
    component_name: &str,
    resources: &[String],
) -> Answer {
    application_insights(
        router,
        runtime,
        "CreateComponent",
        json!({
            "ResourceGroupName": group,
            "ComponentName": component_name,
            "ResourceList": resources,
        }),
    )
}

/// One row: the group and the case tag that selects its resources, how many
/// instances it holds, which of them one custom component groups, and the bucket
/// it also holds (none for a group of instances alone).
struct Row {
    case: &'static str,
    group: &'static str,
    case_value: &'static str,
    instances: usize,
    grouped: &'static [usize],
    bucket: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one instance, grouped alone beside a bucket",
            group: "ai-grouping-single",
            case_value: "single",
            instances: 1,
            grouped: &[0],
            bucket: Some("ai-grouping-single-bucket"),
        },
        Row {
            case: "three instances, two of them grouped together",
            group: "ai-grouping-partial",
            case_value: "partial",
            instances: 3,
            grouped: &[0, 2],
            bucket: Some("ai-grouping-partial-bucket"),
        },
        Row {
            case: "two instances and nothing else, both grouped at once",
            group: "ai-grouping-whole",
            case_value: "whole",
            instances: 2,
            grouped: &[0, 1],
            bucket: None,
        },
    ]
}

#[test]
fn a_custom_component_groups_the_instances_of_its_applications_group() {
    std::thread::Builder::new()
        .name("applicationinsights-grouped-instances".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            for row in rows() {
                let case = row.case;

                // The estate of this row, each resource created through the
                // service that owns it.
                let mut instance_arns: Vec<String> = Vec::new();
                let mut storage: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
                for _ in 0..row.instances {
                    let (instance_id, instance_arn) = wire::launch_instance(
                        &router,
                        &runtime,
                        REGION,
                        &case_tag(row.case_value),
                    );
                    let volumes = wire::attached_volumes(&router, &runtime, REGION, &instance_id);
                    assert!(
                        !volumes.is_empty(),
                        "case {case:?}: a launched instance must hold EBS storage, got {volumes:?}"
                    );
                    storage.insert(instance_arn.clone(), volumes);
                    instance_arns.push(instance_arn);
                }
                let bucket_arn = row.bucket.map(|name| {
                    wire::tagged_bucket(&router, &runtime, REGION, name, &case_tag(row.case_value))
                });

                wire::create_tag_filtered_group(
                    &router,
                    &runtime,
                    REGION,
                    row.group,
                    &[wire::ALL_SUPPORTED],
                    json!([{ "Key": CASE_KEY, "Values": [row.case_value] }]),
                );
                let onboarded: BTreeSet<String> = instance_arns
                    .iter()
                    .cloned()
                    .chain(bucket_arn.clone())
                    .collect();
                let resolved = wire::group_members(&router, &runtime, REGION, row.group);
                assert_eq!(
                    resolved, onboarded,
                    "case {case:?}: expected the group to resolve to {onboarded:?}, got \
                     {resolved:?}"
                );

                let created = application_insights(
                    &router,
                    &runtime,
                    "CreateApplication",
                    json!({ "ResourceGroupName": row.group, "AutoConfigEnabled": false }),
                );
                assert_eq!(
                    created.status, 200,
                    "case {case:?}: CreateApplication({:?}) must succeed, got {} / {:?}",
                    row.group, created.status, created.body
                );

                // Every resource the group resolved to is a standalone component
                // named by its own ARN, unmonitored, and an instance's component
                // covers the instance and the storage it runs on.
                wire::poll_until(case, "every resolved resource became a component", || {
                    wire::component_names(&router, &runtime, REGION, row.group) == onboarded
                });
                for component in wire::components(&router, &runtime, REGION, row.group) {
                    assert_eq!(
                        component["Monitor"],
                        json!(false),
                        "case {case:?}: a discovered component is unmonitored until a \
                         configuration says so, got {component:?}"
                    );
                }
                for instance_arn in &instance_arns {
                    let described =
                        describe_component(&router, &runtime, row.group, instance_arn);
                    assert_eq!(
                        described.body["ApplicationComponent"]["ResourceType"],
                        json!(EC2_INSTANCE),
                        "case {case:?}: an instance's component reports its type, got {:?}",
                        described.body
                    );
                    let covered =
                        wire::component_resources(&router, &runtime, REGION, row.group, instance_arn);
                    let expected = cover(instance_arn, &storage);
                    assert_eq!(
                        covered, expected,
                        "case {case:?}: the standalone component of {instance_arn} must cover \
                         {expected:?}, got {covered:?}"
                    );
                }

                let grouped: Vec<String> = row
                    .grouped
                    .iter()
                    .map(|at| instance_arns[*at].clone())
                    .collect();
                let free: Vec<String> = instance_arns
                    .iter()
                    .enumerate()
                    .filter(|(at, _)| !row.grouped.contains(at))
                    .map(|(_, arn)| arn.clone())
                    .collect();

                // A custom component groups EC2 instances only, so a list that
                // mixes one with a resource of another type is refused whole and
                // makes nothing.
                if let Some(bucket_arn) = &bucket_arn {
                    let mut mixed = grouped.clone();
                    mixed.push(bucket_arn.clone());
                    let refused =
                        create_component(&router, &runtime, row.group, "mixed_tier", &mixed);
                    assert_eq!(
                        (refused.status, refused.error_code().as_str()),
                        (400, "ValidationException"),
                        "case {case:?}: grouping {mixed:?} must be refused 400 \
                         ValidationException, got {} / {:?}",
                        refused.status,
                        refused.body
                    );
                    assert_eq!(
                        refused.error_message(),
                        ONLY_INSTANCES,
                        "case {case:?}: expected the only-instances refusal, got {:?}",
                        refused.body
                    );
                    let after_refusal =
                        wire::component_names(&router, &runtime, REGION, row.group);
                    assert_eq!(
                        after_refusal, onboarded,
                        "case {case:?}: the refused create must make nothing, got \
                         {after_refusal:?}"
                    );
                }

                // Grouping the row's instances takes each one's standalone
                // component away, and the custom component covers every grouped
                // instance together with its storage.
                let group_name = "grouped_tier";
                let answer =
                    create_component(&router, &runtime, row.group, group_name, &grouped);
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: grouping {grouped:?} must succeed, got {} / {:?}",
                    answer.status, answer.body
                );
                let after_grouping: BTreeSet<String> = free
                    .iter()
                    .cloned()
                    .chain(bucket_arn.clone())
                    .chain([group_name.to_string()])
                    .collect();
                let listed = wire::component_names(&router, &runtime, REGION, row.group);
                assert_eq!(
                    listed, after_grouping,
                    "case {case:?}: expected the components {after_grouping:?}, got {listed:?}"
                );
                let custom_cover =
                    wire::component_resources(&router, &runtime, REGION, row.group, group_name);
                let expected_cover: BTreeSet<String> = grouped
                    .iter()
                    .flat_map(|instance_arn| cover(instance_arn, &storage))
                    .collect();
                assert_eq!(
                    custom_cover, expected_cover,
                    "case {case:?}: the custom component must cover {expected_cover:?}, got \
                     {custom_cover:?}"
                );
                let described =
                    describe_component(&router, &runtime, row.group, group_name);
                assert_eq!(
                    described.body["ApplicationComponent"]["ResourceType"],
                    json!(EC2_INSTANCE),
                    "case {case:?}: a component of instances reports their type, got {:?}",
                    described.body
                );

                // An instance a custom component holds is grouped into no second
                // one: the refusal names the grouped instance even when the
                // request asks for free instances beside it, and the free ones
                // stay standalone because nothing was created.
                let mut again = free.clone();
                again.push(grouped[0].clone());
                let refused =
                    create_component(&router, &runtime, row.group, "second_tier", &again);
                assert_eq!(
                    (refused.status, refused.error_code().as_str()),
                    (400, "ValidationException"),
                    "case {case:?}: grouping {again:?} again must be refused 400 \
                     ValidationException, got {} / {:?}",
                    refused.status,
                    refused.body
                );
                assert_eq!(
                    refused.error_message(),
                    format!("Component {} is grouped in another component.", grouped[0]),
                    "case {case:?}: expected the already-grouped refusal naming {}, got {:?}",
                    grouped[0],
                    refused.body
                );
                let missing =
                    describe_component(&router, &runtime, row.group, "second_tier");
                assert_eq!(
                    (missing.status, missing.error_code().as_str()),
                    (NOT_FOUND_STATUS, "ResourceNotFoundException"),
                    "case {case:?}: the refused component must not exist, got {} / {:?}",
                    missing.status,
                    missing.body
                );
                let unchanged = wire::component_names(&router, &runtime, REGION, row.group);
                assert_eq!(
                    unchanged, after_grouping,
                    "case {case:?}: the refused create must leave the component set alone, got \
                     {unchanged:?}"
                );

                // Ungrouping gives every instance the component held back as its
                // own standalone component, covering its storage again, and each
                // is groupable once more.
                let ungrouped = application_insights(
                    &router,
                    &runtime,
                    "DeleteComponent",
                    json!({ "ResourceGroupName": row.group, "ComponentName": group_name }),
                );
                assert_eq!(
                    ungrouped.status, 200,
                    "case {case:?}: DeleteComponent({group_name:?}) must succeed, got {} / {:?}",
                    ungrouped.status, ungrouped.body
                );
                let restored = wire::component_names(&router, &runtime, REGION, row.group);
                assert_eq!(
                    restored, onboarded,
                    "case {case:?}: every ungrouped instance must be standalone again, got \
                     {restored:?}"
                );
                for instance_arn in &grouped {
                    let covered =
                        wire::component_resources(&router, &runtime, REGION, row.group, instance_arn);
                    let expected = cover(instance_arn, &storage);
                    assert_eq!(
                        covered, expected,
                        "case {case:?}: the restored component of {instance_arn} must cover \
                         {expected:?}, got {covered:?}"
                    );
                }
                let regrouped =
                    create_component(&router, &runtime, row.group, "regrouped_tier", &grouped);
                assert_eq!(
                    regrouped.status, 200,
                    "case {case:?}: grouping {grouped:?} again once free must succeed, got {} / \
                     {:?}",
                    regrouped.status,
                    regrouped.body
                );
            }
        })
        .expect("spawn the instance-grouping thread")
        .join()
        .expect("a custom component must group the instances of its application's group");
}

/// What one instance's component covers: the instance, and the storage Amazon EC2
/// reports attached to it.
fn cover(
    instance_arn: &str,
    storage: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeSet<String> {
    let mut covered: BTreeSet<String> = storage.get(instance_arn).cloned().unwrap_or_default();
    covered.insert(instance_arn.to_string());
    covered
}
