// SPDX-License-Identifier: CC0-1.0

//! The ECS CAPACITY PROVIDER control plane, driven end to end: `CreateCapacityProvider`
//! and `DescribeCapacityProviders`.
//!
//! The rules under test:
//!
//! * a capacity provider is a PERSISTED, account-scoped record — `CreateCapacityProvider`
//!   mints its ARN and stores the binding it was given (an Auto Scaling group of the
//!   caller's, whose own ARN that service minted is what the binding reports, or capacity
//!   Amazon ECS manages), and `DescribeCapacityProviders` reports that stored record back:
//!   its name, status, ARN, binding, type and tags;
//! * a provider over an Auto Scaling group the caller ALREADY has is created `ACTIVE`,
//!   while one whose capacity Amazon ECS has to stand up is created `PROVISIONING` with
//!   its create in flight (what it settles into is
//!   [`crate::cluster_scoped_capacity_providers`]'s subject);
//! * the NAME identifies the provider and an Auto Scaling group backs exactly one, so a
//!   create that reuses either is refused and stores nothing;
//! * the create is `@idempotent`, so REPEATING it for the provider that is already stored
//!   — the same name over the same binding — reports that provider unchanged and writes
//!   nothing, while a repeat that would have to change the stored binding is refused;
//! * `UpdateCapacityProvider` modifies the parameters of the provider it names: each member
//!   of the configuration the request carries REPLACES the stored one and each member it
//!   leaves out keeps it, the capacity the provider is bound onto never changes (the Auto
//!   Scaling group is no member of the update), the configuration a client reads back is the
//!   one the update stored, and applying it to that capacity is the update the clock
//!   completes — while an update that names no live provider of the account, a cluster the
//!   account does not hold, the cluster-scoped provider of another cluster, or the
//!   configuration of the other class of capacity is refused and writes nothing;
//! * a name outside the documented character set is refused as a malformed parameter, and
//!   one carrying a prefix AWS reserves as an identifier that is not the caller's; and so
//!   is a request that names neither binding, or both.
//!
//! Every case goes through the shared wire harness in [`crate::ecs_wire`], the same front
//! door a client uses, so nothing reaches into the state struct and a response that is not
//! computed from persisted state cannot pass.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    auto_scaling_group_arn, create_auto_scaling_group, create_cluster, list, ok, refusal, refused,
    service, string, ACCOUNT, SIGNED_REGION,
};

/// One row of the create table: how the provider is created, and what the stored record
/// must then report.
struct CreateCase {
    name: &'static str,
    provider_name: &'static str,
    /// The members the request carries beside `name`.
    request: fn() -> Value,
    /// The `cluster` the stored provider must report, if any.
    expect_cluster: Option<&'static str>,
    /// The `type` the stored provider must report, if any.
    expect_type: Option<&'static str>,
    /// The NAME of the Auto Scaling group the stored provider must report its binding onto,
    /// which it reports as the ARN Amazon EC2 Auto Scaling minted for that group — whichever
    /// of the two forms the request named it by. `None` for a provider over capacity Amazon
    /// ECS manages, which binds onto no group of the caller's.
    expect_group: Option<&'static str>,
    /// The tags the stored provider must report.
    expect_tags: Value,
    /// The `status` the create answers under: a provider over a group the caller already
    /// has is usable at once, while one whose capacity Amazon ECS has to stand up is
    /// still `PROVISIONING`.
    expect_status: &'static str,
    /// The `updateStatus` reported beside it — the create that is still running, and
    /// nothing at all when there was none to run.
    expect_update_status: Option<&'static str>,
}

/// An Auto Scaling group ARN of the account, as `autoScalingGroupProvider` takes it.
/// Shared with every suite that needs a provider over a group the caller already has.
pub(crate) fn group_arn(name: &str) -> String {
    format!(
        "arn:aws:autoscaling:{SIGNED_REGION}:{ACCOUNT}:autoScalingGroup:\
         11111111-2222-3333-4444-555555555555:autoScalingGroupName/{name}"
    )
}

/// A managed-instances provider configuration, with the members the model requires
/// (`infrastructureRoleArn`, `instanceLaunchTemplate.ec2InstanceProfileArn` and that
/// template's `networkConfiguration`). Shared with the daemon suite, which needs a
/// MANAGED_INSTANCES provider to create a daemon over.
pub(crate) fn managed_instances() -> Value {
    managed_instances_monitored(None)
}

/// The same configuration with the launch template's optional `monitoring` option set to
/// `monitoring` when a suite names one — the ONE place this payload is spelled, so every
/// suite creates the same provider.
pub(crate) fn managed_instances_monitored(monitoring: Option<&str>) -> Value {
    let mut configuration = json!({
        "infrastructureRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/ecsInfrastructureRole"),
        "instanceLaunchTemplate": {
            "ec2InstanceProfileArn": format!("arn:aws:iam::{ACCOUNT}:instance-profile/ecsInstanceRole"),
            "networkConfiguration": {
                "subnets": ["subnet-0123456789abcdef0"],
                "securityGroups": ["sg-0123456789abcdef0"],
            },
        },
    });
    if let Some(monitoring) = monitoring {
        configuration["instanceLaunchTemplate"]["monitoring"] = json!(monitoring);
    }
    configuration
}

/// Describe one capacity provider by name and return it, with or without its tags.
async fn describe(
    router: &MultiServiceRouter,
    provider_name: &str,
    include_tags: bool,
    case: &str,
) -> Value {
    let mut request = json!({ "capacityProviders": [provider_name] });
    if include_tags {
        request["include"] = json!(["TAGS"]);
    }
    let body = ok(router, "DescribeCapacityProviders", request, case).await;
    assert!(
        list(&body, "failures", case).is_empty(),
        "case {case:?}: a stored capacity provider is described, not failed, got {body}"
    );
    let providers = list(&body, "capacityProviders", case);
    assert_eq!(
        providers.len(),
        1,
        "case {case:?}: expected exactly one described provider, got {body}"
    );
    providers[0].clone()
}

#[tokio::test]
async fn a_created_capacity_provider_reads_back_through_describe() {
    let router = service();
    create_cluster(&router, "capacity", "setup").await;
    // Every Auto Scaling group a row below binds a provider onto, created through Auto
    // Scaling's own API first: a provider over a group the account does not hold is
    // refused.
    for group in ["asg-byarn", "asg-byname"] {
        create_auto_scaling_group(&router, group, "setup").await;
    }

    // The rows differ along what backs the provider (an Auto Scaling group named by ARN,
    // the same named by its bare name, or capacity Amazon ECS manages), whether managed
    // scaling is configured, whether the provider is scoped to a cluster, and whether it
    // carries tags.
    let cases = [
        CreateCase {
            name: "an Auto Scaling group named by ARN, with managed scaling",
            provider_name: "byarn",
            request: || {
                json!({
                    "autoScalingGroupProvider": {
                        "autoScalingGroupArn": group_arn("asg-byarn"),
                        "managedScaling": {
                            "status": "ENABLED",
                            "targetCapacity": 90,
                            "minimumScalingStepSize": 2,
                            "maximumScalingStepSize": 20,
                            "instanceWarmupPeriod": 120,
                        },
                        "managedTerminationProtection": "ENABLED",
                        "managedDraining": "ENABLED",
                    },
                    "tags": [{ "key": "team", "value": "platform" }],
                })
            },
            expect_cluster: None,
            expect_type: Some("EC2_AUTOSCALING"),
            expect_group: Some("asg-byarn"),
            expect_tags: json!([{ "key": "team", "value": "platform" }]),
            expect_status: "ACTIVE",
            expect_update_status: None,
        },
        CreateCase {
            name: "an Auto Scaling group named by its bare name, no managed scaling",
            provider_name: "byname",
            request: || {
                json!({
                    "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-byname" },
                })
            },
            expect_cluster: None,
            expect_type: Some("EC2_AUTOSCALING"),
            expect_group: Some("asg-byname"),
            expect_tags: json!([]),
            expect_status: "ACTIVE",
            expect_update_status: None,
        },
        CreateCase {
            name: "capacity Amazon ECS manages, scoped to a cluster",
            provider_name: "managed",
            request: || {
                json!({
                    "cluster": "capacity",
                    "managedInstancesProvider": managed_instances(),
                    "tags": [
                        { "key": "env", "value": "test" },
                        { "key": "owner", "value": "ecs" },
                    ],
                })
            },
            expect_cluster: Some("capacity"),
            expect_type: Some("MANAGED_INSTANCES"),
            expect_group: None,
            expect_tags: json!([
                { "key": "env", "value": "test" },
                { "key": "owner", "value": "ecs" },
            ]),
            expect_status: "PROVISIONING",
            expect_update_status: Some("CREATE_IN_PROGRESS"),
        },
        CreateCase {
            name: "a cluster named by its ARN scopes the provider to that cluster",
            provider_name: "byclusterarn",
            request: || {
                json!({
                    "cluster": format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:cluster/capacity"),
                    "managedInstancesProvider": managed_instances(),
                })
            },
            expect_cluster: Some("capacity"),
            expect_type: Some("MANAGED_INSTANCES"),
            expect_group: None,
            expect_tags: json!([]),
            expect_status: "PROVISIONING",
            expect_update_status: Some("CREATE_IN_PROGRESS"),
        },
    ];

    for test in &cases {
        let case = test.name;
        let mut request = (test.request)();
        request["name"] = json!(test.provider_name);
        let created = ok(&router, "CreateCapacityProvider", request, case).await;
        let created = created["capacityProvider"].clone();
        let expect_group = match test.expect_group {
            Some(group) => Some(auto_scaling_group_arn(&router, group, case).await),
            None => None,
        };

        // Create and read must agree member for member: the answer is the stored record,
        // not the request.
        let read = describe(&router, test.provider_name, true, case).await;
        for reported in [&created, &read] {
            assert_eq!(
                string(reported, "capacityProviderArn", case),
                format!(
                    "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:capacity-provider/{}",
                    test.provider_name
                ),
                "case {case:?}: unexpected capacityProviderArn in {reported}"
            );
            assert_eq!(
                string(reported, "name", case),
                test.provider_name,
                "case {case:?}: unexpected name in {reported}"
            );
            assert_eq!(
                string(reported, "status", case),
                test.expect_status,
                "case {case:?}: unexpected status in {reported}"
            );
            assert_eq!(
                reported["cluster"].as_str(),
                test.expect_cluster,
                "case {case:?}: unexpected cluster in {reported}"
            );
            assert_eq!(
                reported["type"].as_str(),
                test.expect_type,
                "case {case:?}: unexpected type in {reported}"
            );
            assert_eq!(
                reported["autoScalingGroupProvider"]["autoScalingGroupArn"].as_str(),
                expect_group.as_deref(),
                "case {case:?}: unexpected autoScalingGroupProvider in {reported}"
            );
            assert_eq!(
                list(reported, "tags", case),
                test.expect_tags
                    .as_array()
                    .expect("the expected tags are a list")
                    .clone(),
                "case {case:?}: unexpected tags in {reported}"
            );
            // The create itself is the only update in flight, and it did what it was
            // asked, so no reason is stated for it.
            assert_eq!(
                reported["updateStatus"].as_str(),
                test.expect_update_status,
                "case {case:?}: unexpected updateStatus in {reported}"
            );
            assert!(
                reported["updateStatusReason"].is_null(),
                "case {case:?}: a create that did what it was asked states no reason, got \
                 {reported}"
            );
        }

        // "If this field is omitted, tags aren't included in the response": the member is
        // still reported, carrying no tag.
        let without_tags = describe(&router, test.provider_name, false, case).await;
        assert_eq!(
            without_tags["tags"],
            json!([]),
            "case {case:?}: a provider read without include reports an empty tag list, got \
             {without_tags}"
        );
    }

    // The managed-scaling row's configuration is stored whole.
    let case = "managed scaling reads back";
    let scaling = describe(&router, "byarn", false, case).await;
    let scaling = &scaling["autoScalingGroupProvider"]["managedScaling"];
    assert_eq!(
        (
            scaling["status"].as_str(),
            scaling["targetCapacity"].as_i64(),
            scaling["minimumScalingStepSize"].as_i64(),
            scaling["maximumScalingStepSize"].as_i64(),
            scaling["instanceWarmupPeriod"].as_i64(),
        ),
        (Some("ENABLED"), Some(90), Some(2), Some(20), Some(120)),
        "case {case:?}: expected the stored managed scaling back, got {scaling}"
    );

    // Every provider the account holds is described when the request names none —
    // including the two Amazon ECS manages for every account in every Region (`FARGATE`
    // and `FARGATE_SPOT`), which nothing has to create.
    let case = "every provider";
    let body = ok(&router, "DescribeCapacityProviders", json!({}), case).await;
    let mut names: Vec<String> = list(&body, "capacityProviders", case)
        .iter()
        .map(|provider| string(provider, "name", case))
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "FARGATE",
            "FARGATE_SPOT",
            "byarn",
            "byclusterarn",
            "byname",
            "managed"
        ],
        "case {case:?}: expected every stored provider, got {body}"
    );
}

#[tokio::test]
async fn a_capacity_provider_request_that_cannot_be_satisfied_is_refused() {
    let router = service();
    create_cluster(&router, "capacity", "setup").await;
    // The groups the rows below bind onto, so each row is refused for the rule it is about
    // rather than for a group the account does not hold.
    for group in ["asg-taken", "asg-other"] {
        create_auto_scaling_group(&router, group, "setup").await;
    }
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "taken",
            "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-taken") },
        }),
        "setup",
    )
    .await;

    // One row per rule the handler enforces, each with the modelled error it answers.
    // The name rows span the reserved prefixes (`CreateCapacityProvider$name`: "The name
    // can't be prefixed with \"aws\", \"ecs\", or \"fargate\"") in both spellings, the
    // character set and the documented length. The name is the provider's IDENTIFIER, so
    // every value outside that one rule earns the `ClientException` modelled for an
    // identifier that isn't valid; the rows about other members earn the
    // malformed-parameter refusal.
    let cases: [(&str, Value, &str); 12] = [
        (
            "a name prefixed with aws",
            json!({
                "name": "aws-provider",
                "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-1" },
            }),
            "ClientException",
        ),
        (
            "a name prefixed with ecs",
            json!({
                "name": "ecsprovider",
                "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-1" },
            }),
            "ClientException",
        ),
        (
            "a name prefixed with fargate",
            json!({
                "name": "fargate_spot_clone",
                "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-1" },
            }),
            "ClientException",
        ),
        (
            "a reserved prefix in upper case",
            json!({
                "name": "AWSProvider",
                "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-1" },
            }),
            "ClientException",
        ),
        (
            "a name outside the documented character set",
            json!({
                "name": "not a provider name",
                "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-1" },
            }),
            "ClientException",
        ),
        (
            "a name longer than 255 characters",
            json!({
                "name": "p".repeat(256),
                "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-1" },
            }),
            "ClientException",
        ),
        (
            "neither an Auto Scaling group nor managed instances",
            json!({ "name": "nothing" }),
            "InvalidParameterException",
        ),
        (
            "both an Auto Scaling group and managed instances",
            json!({
                "name": "both",
                "cluster": "capacity",
                "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-both" },
                "managedInstancesProvider": managed_instances(),
            }),
            "InvalidParameterException",
        ),
        (
            "managed instances with no cluster to scope them to",
            json!({
                "name": "unscoped",
                "managedInstancesProvider": managed_instances(),
            }),
            "InvalidParameterException",
        ),
        (
            "a cluster that does not exist",
            json!({
                "name": "ghostcluster",
                "cluster": "nosuchcluster",
                "managedInstancesProvider": managed_instances(),
            }),
            "ClusterNotFoundException",
        ),
        (
            "a name the account already holds",
            json!({
                "name": "taken",
                "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-other") },
            }),
            "ClientException",
        ),
        (
            "an Auto Scaling group another provider already backs, named by its bare name",
            json!({
                "name": "second",
                "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-taken" },
            }),
            "ClientException",
        ),
    ];

    for (case, request, expected) in cases {
        refused(&router, "CreateCapacityProvider", request, expected, case).await;
    }

    // Nothing above was stored: a refused create creates nothing. The account's two
    // ECS-managed providers are there whether or not anybody created anything.
    let case = "only the provider that was accepted exists";
    let body = ok(&router, "DescribeCapacityProviders", json!({}), case).await;
    let names: Vec<String> = list(&body, "capacityProviders", case)
        .iter()
        .map(|provider| string(provider, "name", case))
        .collect();
    assert_eq!(
        names,
        vec![
            "FARGATE".to_string(),
            "FARGATE_SPOT".to_string(),
            "taken".to_string()
        ],
        "case {case:?}: a refused create must store nothing, got {body}"
    );

    // Every refusal of the NAME carries the one sentence that states the whole name rule,
    // whichever half of it the value broke: the rows differ along which half that is.
    let name_rule = "The specified capacity provider name is invalid. Up to 255 characters \
                     are allowed, including letters (upper and lowercase), numbers, \
                     underscores, and hyphens. The name cannot be prefixed with \"aws\", \
                     \"ecs\", or \"fargate\". Specify a valid name and try again.";
    let name_rows: [(&str, String); 3] = [
        ("a reserved prefix", "ecs-mine".to_string()),
        ("a character the member does not allow", "not a name".to_string()),
        ("a name past the documented length", "p".repeat(256)),
    ];
    for (case, provider_name) in name_rows {
        let (code, message) = refusal(
            &router,
            "CreateCapacityProvider",
            json!({
                "name": provider_name,
                "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-unused" },
            }),
            case,
        )
        .await;
        assert_eq!(
            code, "ClientException",
            "case {case:?}: the name is the provider's identifier, got {code:?} with \
             {message:?}"
        );
        assert_eq!(
            message, name_rule,
            "case {case:?}: unexpected refusal message"
        );
    }

    // The group-reuse refusal names the MEMBER at fault and what a retry has to carry:
    // the Auto Scaling group ARN has to be one no other provider is using.
    let case = "the reuse refusal names the member at fault";
    let (code, message) = refusal(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "third",
            "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-taken") },
        }),
        case,
    )
    .await;
    assert_eq!(
        code, "ClientException",
        "case {case:?}: expected a ClientException, got {code:?} with {message:?}"
    );
    assert_eq!(
        message,
        "The specified Auto Scaling group ARN is already being used by another capacity \
         provider. Specify a unique Auto Scaling group ARN and try again.",
        "case {case:?}: unexpected refusal message"
    );
}

/// One row of the repeat table: how the provider is first created, how the create is
/// REPEATED, and whether that repeat is the same provider (an idempotent no-op) or a
/// request that conflicts with what is stored.
struct RepeatCase {
    name: &'static str,
    /// The members the first create carries beside `name`.
    create: fn() -> Value,
    /// The members the repeat carries beside `name`.
    repeat: fn() -> Value,
    /// Whether the repeat asks for the provider that is already stored.
    idempotent: bool,
}

#[tokio::test]
async fn repeating_a_capacity_provider_create_reports_the_stored_provider() {
    let router = service();
    create_cluster(&router, "capacity", "setup").await;
    create_cluster(&router, "elsewhere", "setup").await;
    // Every group a row below binds onto, including the one a repeat tries to move a
    // provider to: a group the account does not hold is refused before the repeat rule is
    // ever reached.
    for group in [
        "asg-r0", "asg-r1", "asg-r2", "asg-r4", "asg-r5", "asg-other5", "asg-r6", "asg-r7",
    ] {
        create_auto_scaling_group(&router, group, "setup").await;
    }

    // `CreateCapacityProvider` carries the `@idempotent` trait
    // (`smithy_query(query="operation",operation="CreateCapacityProvider")` reports
    // `idempotent: true`), so the rows below span the repeats a caller makes: the same
    // binding spelled the same way and spelled the other way, each kind of binding, a
    // repeat differing only in metadata, and the repeats that would have to CHANGE the
    // stored binding.
    let cases = [
        RepeatCase {
            name: "the same Auto Scaling group, named the same way",
            create: || json!({ "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-r0") } }),
            repeat: || json!({ "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-r0") } }),
            idempotent: true,
        },
        RepeatCase {
            name: "the same Auto Scaling group, named by its bare name on the repeat",
            create: || json!({ "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-r1") } }),
            repeat: || json!({ "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-r1" } }),
            idempotent: true,
        },
        RepeatCase {
            name: "the same group with the same managed scaling configuration",
            create: || {
                json!({
                    "autoScalingGroupProvider": {
                        "autoScalingGroupArn": group_arn("asg-r2"),
                        "managedScaling": {
                            "status": "ENABLED",
                            "targetCapacity": 75,
                            "minimumScalingStepSize": 1,
                            "maximumScalingStepSize": 10,
                            "instanceWarmupPeriod": 60,
                        },
                        "managedTerminationProtection": "DISABLED",
                        "managedDraining": "ENABLED",
                    },
                })
            },
            repeat: || {
                json!({
                    "autoScalingGroupProvider": {
                        "autoScalingGroupArn": group_arn("asg-r2"),
                        "managedScaling": {
                            "status": "ENABLED",
                            "targetCapacity": 75,
                            "minimumScalingStepSize": 1,
                            "maximumScalingStepSize": 10,
                            "instanceWarmupPeriod": 60,
                        },
                        "managedTerminationProtection": "DISABLED",
                        "managedDraining": "ENABLED",
                    },
                })
            },
            idempotent: true,
        },
        RepeatCase {
            name: "the same managed-instances configuration in the same cluster",
            create: || json!({ "cluster": "capacity", "managedInstancesProvider": managed_instances() }),
            repeat: || json!({ "cluster": "capacity", "managedInstancesProvider": managed_instances() }),
            idempotent: true,
        },
        RepeatCase {
            name: "the same binding with other tags, which are not what it binds onto",
            create: || {
                json!({
                    "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-r4") },
                    "tags": [{ "key": "team", "value": "platform" }],
                })
            },
            repeat: || {
                json!({
                    "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-r4") },
                    "tags": [{ "key": "team", "value": "other" }],
                })
            },
            idempotent: true,
        },
        RepeatCase {
            name: "a different Auto Scaling group under the name already taken",
            create: || json!({ "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-r5") } }),
            repeat: || json!({ "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-other5") } }),
            idempotent: false,
        },
        RepeatCase {
            name: "the same group with managed scaling turned on by the repeat",
            create: || json!({ "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-r6") } }),
            repeat: || {
                json!({
                    "autoScalingGroupProvider": {
                        "autoScalingGroupArn": group_arn("asg-r6"),
                        "managedScaling": { "status": "ENABLED", "targetCapacity": 100 },
                    },
                })
            },
            idempotent: false,
        },
        RepeatCase {
            name: "managed instances under a name an Auto Scaling group holds",
            create: || json!({ "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-r7") } }),
            repeat: || json!({ "cluster": "capacity", "managedInstancesProvider": managed_instances() }),
            idempotent: false,
        },
        RepeatCase {
            name: "the same managed-instances configuration scoped to another cluster",
            create: || json!({ "cluster": "capacity", "managedInstancesProvider": managed_instances() }),
            repeat: || json!({ "cluster": "elsewhere", "managedInstancesProvider": managed_instances() }),
            idempotent: false,
        },
    ];

    for (index, test) in cases.iter().enumerate() {
        let case = test.name;
        let provider_name = format!("repeat{index}");

        let mut create = (test.create)();
        create["name"] = json!(provider_name);
        let created = ok(&router, "CreateCapacityProvider", create, case).await;
        let created = created["capacityProvider"].clone();
        let stored = describe(&router, &provider_name, true, case).await;

        let mut repeat = (test.repeat)();
        repeat["name"] = json!(provider_name);
        if test.idempotent {
            let again = ok(&router, "CreateCapacityProvider", repeat, case).await;
            assert_eq!(
                again["capacityProvider"], created,
                "case {case:?}: a repeat of the same create reports the stored provider \
                 unchanged, got {again}"
            );
        } else {
            let (code, message) = refusal(&router, "CreateCapacityProvider", repeat, case).await;
            assert_eq!(
                code, "ClientException",
                "case {case:?}: expected a ClientException, got {code:?} with {message:?}"
            );
            assert_eq!(
                message,
                "The specified capacity provider already exists. To change the \
                 configuration of an existing capacity provider, update the capacity \
                 provider.",
                "case {case:?}: unexpected refusal message"
            );
        }

        // Either way the stored record is the one the FIRST create wrote: an idempotent
        // repeat writes nothing, and a refused one changes nothing.
        assert_eq!(
            describe(&router, &provider_name, true, case).await,
            stored,
            "case {case:?}: the stored provider must be untouched by the repeat"
        );
    }

    // The name is what is repeated: the same binding under a DIFFERENT name is a second
    // provider on one Auto Scaling group, which is still refused.
    let case = "the same group under another name";
    let (code, message) = refusal(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "renamed",
            "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-r0") },
        }),
        case,
    )
    .await;
    assert_eq!(
        code, "ClientException",
        "case {case:?}: expected a ClientException, got {code:?} with {message:?}"
    );
    assert_eq!(
        message,
        "The specified Auto Scaling group ARN is already being used by another capacity \
         provider. Specify a unique Auto Scaling group ARN and try again.",
        "case {case:?}: a repeat under another name is a second provider on one group"
    );
}

#[tokio::test]
async fn describe_capacity_providers_filters_pages_and_reports_failures() {
    let router = service();
    create_cluster(&router, "scoped", "setup").await;
    create_cluster(&router, "other", "setup").await;

    // Twelve providers here plus the cluster-scoped one below, so the documented page
    // size of 10 is crossed.
    let names: Vec<String> = (0..12).map(|index| format!("provider{index:02}")).collect();
    for name in &names {
        let group = format!("asg-{name}");
        create_auto_scaling_group(&router, &group, "setup").await;
        ok(
            &router,
            "CreateCapacityProvider",
            json!({
                "name": name,
                "autoScalingGroupProvider": { "autoScalingGroupArn": group },
            }),
            "setup",
        )
        .await;
    }
    // One provider scoped to `scoped` by construction, and one merely ASSOCIATED with it.
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "scopedmanaged",
            "cluster": "scoped",
            "managedInstancesProvider": managed_instances(),
        }),
        "setup",
    )
    .await;
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": "scoped",
            "capacityProviders": ["provider00"],
            "defaultCapacityProviderStrategy": [
                { "capacityProvider": "provider00", "weight": 1 },
            ],
        }),
        "setup",
    )
    .await;

    // Paging walks every provider exactly once, in name order, whatever page size the
    // request asks for: the rows differ along that size, and the unpaged default.
    let page_sizes: [Option<i64>; 4] = [None, Some(1), Some(5), Some(10)];
    for size in page_sizes {
        let case = &format!("paging {size:?} at a time");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut request = json!({});
            if let Some(size) = size {
                request["maxResults"] = json!(size);
            }
            if let Some(token) = token.as_deref() {
                request["nextToken"] = json!(token);
            }
            let body = ok(&router, "DescribeCapacityProviders", request, case).await;
            let page = list(&body, "capacityProviders", case);
            let expected_page = size.unwrap_or(10);
            assert!(
                page.len() as i64 <= expected_page,
                "case {case:?}: a page carries at most {expected_page} providers, got {body}"
            );
            seen.extend(page.iter().map(|provider| string(provider, "name", case)));
            match body["nextToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        let mut expected: Vec<String> = names.clone();
        expected.push("scopedmanaged".to_string());
        // The providers Amazon ECS manages for the account are walked with the rest.
        expected.push("FARGATE".to_string());
        expected.push("FARGATE_SPOT".to_string());
        expected.sort();
        assert_eq!(
            seen, expected,
            "case {case:?}: expected every provider exactly once in name order"
        );
    }

    // "This value can be between 1 and 10. If this parameter is not used, then
    // DescribeCapacityProviders returns up to 10 results": a page size below the range asks
    // for no page at all and is refused, while one above it is served the page this read
    // serves.
    refused(
        &router,
        "DescribeCapacityProviders",
        json!({ "maxResults": 0 }),
        "InvalidParameterException",
        "zero",
    )
    .await;
    for (case, max_results) in [("eleven", 11), ("a hundred", 100)] {
        let body = ok(
            &router,
            "DescribeCapacityProviders",
            json!({ "maxResults": max_results }),
            case,
        )
        .await;
        assert_eq!(
            list(&body, "capacityProviders", case).len(),
            10,
            "case {case:?}: a page above the range is served the page this read serves, got \
             {body}"
        );
    }

    // "only capacity providers associated with this cluster are returned, including
    // Amazon ECS Managed Instances capacity providers".
    let case = "the providers of one cluster";
    let body = ok(
        &router,
        "DescribeCapacityProviders",
        json!({ "cluster": "scoped" }),
        case,
    )
    .await;
    let mut scoped: Vec<String> = list(&body, "capacityProviders", case)
        .iter()
        .map(|provider| string(provider, "name", case))
        .collect();
    scoped.sort();
    assert_eq!(
        scoped,
        vec!["provider00".to_string(), "scopedmanaged".to_string()],
        "case {case:?}: expected the associated and the cluster-scoped provider, got {body}"
    );
    let case = "a cluster with no providers";
    let body = ok(
        &router,
        "DescribeCapacityProviders",
        json!({ "cluster": "other" }),
        case,
    )
    .await;
    assert!(
        list(&body, "capacityProviders", case).is_empty(),
        "case {case:?}: expected no provider for a cluster holding none, got {body}"
    );

    // A name that matches nothing is a FAILURE entry beside the results, not a refusal.
    let case = "a mix of known and unknown names";
    let body = ok(
        &router,
        "DescribeCapacityProviders",
        json!({
            "capacityProviders": [
                "provider00",
                format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:capacity-provider/provider01"),
                "ghost",
            ],
        }),
        case,
    )
    .await;
    let found: Vec<String> = list(&body, "capacityProviders", case)
        .iter()
        .map(|provider| string(provider, "name", case))
        .collect();
    assert_eq!(
        found,
        vec!["provider00".to_string(), "provider01".to_string()],
        "case {case:?}: a provider named by ARN is the same provider, got {body}"
    );
    assert_eq!(
        list(&body, "failures", case),
        vec![json!({
            "arn": "ghost",
            "reason": "The specified capacity provider does not exist. Specify a valid name \
                       or ARN and try again.",
        })],
        "case {case:?}: unexpected failures in {body}"
    );

    // "Up to 100 capacity providers can be described in an action."
    let overlong: Vec<String> = (0..101).map(|index| format!("name{index}")).collect();
    refused(
        &router,
        "DescribeCapacityProviders",
        json!({ "capacityProviders": overlong }),
        "InvalidParameterException",
        "more than a hundred providers in one describe",
    )
    .await;
}

/// How often the update polls below look again.
const UPDATE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long they keep looking before giving up — generous, since each stops the instant the
/// condition it waits on holds.
const UPDATE_POLL_BUDGET: Duration = Duration::from_secs(60);

/// The whole sentence an update whose `name` resolves to no live capacity provider of the
/// account is refused with, in the class Amazon ECS answers an identifier it cannot RESOLVE
/// in: `ClientException`.
const UPDATE_UNRESOLVED_PROVIDER: &str = "The specified capacity provider does not exist. \
                                          Specify a valid name or ARN and try again.";

/// The whole sentence an update of a CLUSTER-SCOPED provider that does not name its own
/// cluster earns, which is the whole of the `cluster` member's own rule.
const CLUSTER_SCOPED_UPDATE_REFUSAL: &str = "Managed instances capacity providers are \
                                             cluster-scoped and can only be updated within \
                                             their associated cluster.";

/// The whole sentence an update whose configuration is not the one the provider it names is
/// built on earns.
const CONFIGURATION_CLASS_REFUSAL: &str =
    "The specified configuration doesn't match the type of the specified capacity provider. \
     Specify autoScalingGroupProvider for an Auto Scaling group capacity provider or \
     managedInstancesProvider for a managed instances capacity provider and try again.";

/// The whole sentence a `managedScaling` block whose minimum scaling step size is above its
/// maximum earns, from whichever operation carries the block.
const STEP_SIZE_REFUSAL: &str =
    "The minimum or maximum scaling step size value is invalid. Specify a minimum scaling \
     step size value that is less than or equal to the maximum scaling step size value and \
     try again.";

/// The ARN form of a capacity provider of this account, which a reference to one accepts
/// beside the short name.
fn capacity_provider_arn(name: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:capacity-provider/{name}")
}

/// The `(status, updateStatus)` pair a reported provider carries — the pair every outcome of
/// an update is read off.
fn lifecycle(provider: &Value, case: &str) -> (String, Option<String>) {
    (
        string(provider, "status", case),
        provider["updateStatus"].as_str().map(str::to_string),
    )
}

/// Wait until `name` reports the `(status, updateStatus)` pair `wanted`, and answer the
/// provider as it then stands. Both the end of a create Amazon ECS stands capacity up for
/// and the end of an update of that capacity are owed by the CLOCK, so the wait is on the
/// condition and nothing sleeps in the emulator.
async fn wait_for_lifecycle(
    router: &MultiServiceRouter,
    name: &str,
    wanted: (&str, Option<&str>),
    case: &str,
) -> Value {
    let deadline = Instant::now() + UPDATE_POLL_BUDGET;
    loop {
        let read = describe(router, name, false, case).await;
        let (status, update_status) = lifecycle(&read, case);
        if (status.as_str(), update_status.as_deref()) == wanted {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the provider to report {wanted:?}, got {read}"
        );
        tokio::time::sleep(UPDATE_POLL_INTERVAL).await;
    }
}

/// One row of the update table: the provider to update, how the request names it, the
/// `autoScalingGroupProvider` the update carries, and the whole configuration the stored
/// provider must then report.
struct UpdateCase {
    case: &'static str,
    provider: &'static str,
    /// The Auto Scaling group the provider is bound onto, whose own ARN — read off the
    /// service that minted it — the stored binding must still report after the update.
    group: &'static str,
    /// What the request's `name` carries — a short name or a full ARN.
    named: fn(&'static str) -> String,
    /// The update's own `autoScalingGroupProvider`.
    update: fn() -> Value,
    /// The `autoScalingGroupProvider` the provider must report after it, beside that group.
    expect: fn() -> Value,
}

#[tokio::test]
async fn an_update_replaces_the_members_it_carries_and_keeps_the_rest() {
    let router = service();
    for group in ["asg-scaling", "asg-options", "asg-arn"] {
        create_auto_scaling_group(&router, group, "setup").await;
    }
    // A provider whose create named managed scaling and turned termination protection on,
    // so an update that names neither of those options is read against values a request put
    // there rather than against defaults.
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "scaling",
            "autoScalingGroupProvider": {
                "autoScalingGroupArn": group_arn("asg-scaling"),
                "managedScaling": {
                    "status": "ENABLED",
                    "targetCapacity": 90,
                    "minimumScalingStepSize": 2,
                    "maximumScalingStepSize": 20,
                    "instanceWarmupPeriod": 120,
                },
                "managedTerminationProtection": "ENABLED",
            },
        }),
        "setup",
    )
    .await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "options",
            "autoScalingGroupProvider": {
                "autoScalingGroupArn": group_arn("asg-options"),
                "managedScaling": { "status": "ENABLED", "targetCapacity": 75 },
            },
        }),
        "setup",
    )
    .await;
    // And one created with nothing but its group, so every value its configuration reports
    // is one Amazon ECS applied.
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "arnnamed",
            "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-arn") },
        }),
        "setup",
    )
    .await;

    // The rows differ along WHICH members the update carries (the scaling block alone, the
    // two options alone, a whole block of explicit numbers), how the request NAMES the
    // provider (short name or full ARN), and the FORM of the values (defaulted, explicit,
    // and the published floor of the warm-up period).
    let rows = [
        UpdateCase {
            case: "the scaling block alone, leaving the options as they stand",
            provider: "scaling",
            group: "asg-scaling",
            named: |name| name.to_string(),
            update: || json!({ "managedScaling": { "status": "ENABLED", "targetCapacity": 60 } }),
            expect: || {
                json!({
                    "managedScaling": {
                        "scalingApproach": "SCALING_POLICY",
                        "status": "ENABLED",
                        "targetCapacity": 60,
                        // The numbers the update's block left out stand at the default each
                        // of their own members documents, as a create's block does.
                        "minimumScalingStepSize": 1,
                        "maximumScalingStepSize": 10000,
                        "instanceWarmupPeriod": 300,
                    },
                    // Not named by the update, so still what the create turned on.
                    "managedTerminationProtection": "ENABLED",
                    "managedDraining": "ENABLED",
                })
            },
        },
        UpdateCase {
            case: "the two options alone, leaving the scaling block as it stands",
            provider: "options",
            group: "asg-options",
            named: |name| name.to_string(),
            update: || {
                json!({
                    "managedTerminationProtection": "ENABLED",
                    "managedDraining": "DISABLED",
                })
            },
            expect: || {
                json!({
                    "managedScaling": {
                        "scalingApproach": "SCALING_POLICY",
                        "status": "ENABLED",
                        "targetCapacity": 75,
                        "minimumScalingStepSize": 1,
                        "maximumScalingStepSize": 10000,
                        "instanceWarmupPeriod": 300,
                    },
                    "managedTerminationProtection": "ENABLED",
                    "managedDraining": "DISABLED",
                })
            },
        },
        UpdateCase {
            case: "every scaling number, on a provider named by its full ARN",
            provider: "arnnamed",
            group: "asg-arn",
            named: capacity_provider_arn,
            update: || {
                json!({
                    "managedScaling": {
                        "status": "DISABLED",
                        "targetCapacity": 50,
                        "minimumScalingStepSize": 5,
                        "maximumScalingStepSize": 5,
                        // `instanceWarmupPeriod`'s published floor (`@range` 0..10000).
                        "instanceWarmupPeriod": 0,
                    },
                })
            },
            expect: || {
                json!({
                    "managedScaling": {
                        "scalingApproach": "SCALING_POLICY",
                        "status": "DISABLED",
                        "targetCapacity": 50,
                        "minimumScalingStepSize": 5,
                        "maximumScalingStepSize": 5,
                        "instanceWarmupPeriod": 0,
                    },
                    // Nothing has ever named either option, so each stands at the value its
                    // own member documents.
                    "managedTerminationProtection": "DISABLED",
                    "managedDraining": "ENABLED",
                })
            },
        },
    ];

    for row in &rows {
        let (case, provider) = (row.case, row.provider);
        let before = describe(&router, provider, false, case).await;
        // The binding is reported as the ARN Amazon EC2 Auto Scaling minted for the group,
        // read off the service that owns the group rather than composed here.
        let mut expected = (row.expect)();
        expected["autoScalingGroupArn"] =
            json!(auto_scaling_group_arn(&router, row.group, case).await);

        let answer = ok(
            &router,
            "UpdateCapacityProvider",
            json!({
                "name": (row.named)(provider),
                "autoScalingGroupProvider": (row.update)(),
            }),
            case,
        )
        .await;
        let updated = answer["capacityProvider"].clone();
        assert_eq!(
            updated["autoScalingGroupProvider"], expected,
            "case {case:?}: unexpected configuration after the update, got {updated}"
        );
        // An update changes the provider's parameters, never which provider it is or which
        // capacity it is bound onto.
        for member in ["capacityProviderArn", "name", "type", "cluster", "status"] {
            assert_eq!(
                updated[member], before[member],
                "case {case:?}: an update must not change {member}, got {updated}"
            );
        }
        assert_eq!(
            lifecycle(&updated, case).1.as_deref(),
            Some("UPDATE_IN_PROGRESS"),
            "case {case:?}: an accepted update leaves itself in flight, got {updated}"
        );

        // The describe answers what the update stored, so the update reported state it wrote
        // rather than a configuration of its own.
        let read = describe(&router, provider, false, case).await;
        assert_eq!(
            read["autoScalingGroupProvider"], updated["autoScalingGroupProvider"],
            "case {case:?}: the describe must report what the update stored, got {read}"
        );

        // The end of the update is owed by the clock, and the provider stays usable across
        // it with the configuration the update stored.
        let settled =
            wait_for_lifecycle(&router, provider, ("ACTIVE", Some("UPDATE_COMPLETE")), case).await;
        assert_eq!(
            settled["autoScalingGroupProvider"], expected,
            "case {case:?}: the settled provider reports the stored configuration, got \
             {settled}"
        );
    }
}

#[tokio::test]
async fn an_update_of_a_managed_instances_provider_changes_what_it_names() {
    let router = service();
    create_cluster(&router, "scoped", "setup").await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "scopedmanaged",
            "cluster": "scoped",
            "managedInstancesProvider": managed_instances(),
        }),
        "setup",
    )
    .await;
    // The create of a provider whose capacity Amazon ECS stands up is itself an update in
    // flight, so the suite waits for it to land before asking for another.
    let created = wait_for_lifecycle(
        &router,
        "scopedmanaged",
        ("ACTIVE", Some("CREATE_COMPLETE")),
        "setup",
    )
    .await;
    let launch_template = created["managedInstancesProvider"]["instanceLaunchTemplate"].clone();

    // A member of the launch template the update names is replaced and every member it
    // leaves out is kept — and `monitoring` is accepted in the lower-case spelling its own
    // documentation publishes, stored in the spelling the modelled value set lists
    // (`smithy_query(query="shape", shape="ManagedInstancesMonitoringOptions")` ->
    // `["BASIC", "DETAILED"]`).
    let case = "the role, the monitoring option and the tag propagation";
    let role = format!("arn:aws:iam::{ACCOUNT}:role/ecsInfrastructureRoleUpdated");
    let answer = ok(
        &router,
        "UpdateCapacityProvider",
        json!({
            "name": "scopedmanaged",
            "cluster": "scoped",
            "managedInstancesProvider": {
                "infrastructureRoleArn": role,
                "instanceLaunchTemplate": { "monitoring": "detailed" },
                "propagateTags": "CAPACITY_PROVIDER",
            },
        }),
        case,
    )
    .await;
    let updated = answer["capacityProvider"].clone();
    let mut expected_template = launch_template.clone();
    expected_template["monitoring"] = json!("DETAILED");
    assert_eq!(
        updated["managedInstancesProvider"],
        json!({
            "infrastructureRoleArn": role,
            "instanceLaunchTemplate": expected_template,
            "propagateTags": "CAPACITY_PROVIDER",
        }),
        "case {case:?}: unexpected configuration after the update, got {updated}"
    );
    assert_eq!(
        lifecycle(&updated, case),
        (
            "ACTIVE".to_string(),
            Some("UPDATE_IN_PROGRESS".to_string())
        ),
        "case {case:?}: an accepted update leaves itself in flight on a usable provider, got \
         {updated}"
    );
    let settled = wait_for_lifecycle(
        &router,
        "scopedmanaged",
        ("ACTIVE", Some("UPDATE_COMPLETE")),
        case,
    )
    .await;
    assert_eq!(
        settled["managedInstancesProvider"], updated["managedInstancesProvider"],
        "case {case:?}: the describe must report what the update stored, got {settled}"
    );

    // A second update that carries nothing but the two members the model requires keeps
    // every member it does not name, including the ones the first update put there.
    let case = "an update that names no optional member keeps them all";
    let answer = ok(
        &router,
        "UpdateCapacityProvider",
        json!({
            "name": "scopedmanaged",
            "cluster": "scoped",
            "managedInstancesProvider": {
                "infrastructureRoleArn": role,
                "instanceLaunchTemplate": {},
            },
        }),
        case,
    )
    .await;
    assert_eq!(
        answer["capacityProvider"]["managedInstancesProvider"],
        settled["managedInstancesProvider"],
        "case {case:?}: an update that names no optional member changes none, got {answer}"
    );
}

/// One row of the refusal table: the request, the error it earns, and the provider whose
/// stored record must be untouched by it.
struct UpdateRefusalCase {
    case: &'static str,
    request: fn() -> Value,
    code: &'static str,
    message: &'static str,
    /// The provider to read back afterwards, if the row names one that exists.
    untouched: Option<&'static str>,
}

#[tokio::test]
async fn an_update_that_cannot_be_satisfied_is_refused_and_writes_nothing() {
    let router = service();
    create_cluster(&router, "scoped", "setup").await;
    create_cluster(&router, "other", "setup").await;
    for group in ["asg-live", "asg-retired"] {
        create_auto_scaling_group(&router, group, "setup").await;
    }
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "live",
            "autoScalingGroupProvider": {
                "autoScalingGroupArn": group_arn("asg-live"),
                "managedScaling": { "status": "ENABLED", "targetCapacity": 80 },
            },
        }),
        "setup",
    )
    .await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "retired",
            "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-retired") },
        }),
        "setup",
    )
    .await;
    ok(
        &router,
        "DeleteCapacityProvider",
        json!({ "capacityProvider": "retired" }),
        "setup",
    )
    .await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "scopedmanaged",
            "cluster": "scoped",
            "managedInstancesProvider": managed_instances(),
        }),
        "setup",
    )
    .await;
    // Both of those providers have a transition the clock owes them — the release the
    // delete left in flight, and the capacity the create of a managed-instances provider
    // has Amazon ECS stand up. Each row below reads its provider before and after the
    // refusal, so the suite waits for the clock to settle them first and the comparison is
    // about what the refused call wrote.
    wait_for_lifecycle(
        &router,
        "retired",
        ("INACTIVE", Some("DELETE_COMPLETE")),
        "setup",
    )
    .await;
    wait_for_lifecycle(
        &router,
        "scopedmanaged",
        ("ACTIVE", Some("CREATE_COMPLETE")),
        "setup",
    )
    .await;

    // The rows differ along WHAT the request cannot be satisfied for: the provider it names,
    // the cluster it names, the cluster it does not name, the class of configuration it
    // carries, how many configurations it carries, and the values inside one.
    let rows = [
        UpdateRefusalCase {
            case: "a name no provider of the account answers to",
            request: || {
                json!({
                    "name": "ghost",
                    "autoScalingGroupProvider": { "managedDraining": "DISABLED" },
                })
            },
            code: "ClientException",
            message: UPDATE_UNRESOLVED_PROVIDER,
            untouched: None,
        },
        UpdateRefusalCase {
            case: "a provider a delete has retired",
            request: || {
                json!({
                    "name": "retired",
                    "autoScalingGroupProvider": { "managedDraining": "DISABLED" },
                })
            },
            code: "ClientException",
            message: UPDATE_UNRESOLVED_PROVIDER,
            untouched: Some("retired"),
        },
        UpdateRefusalCase {
            case: "a cluster the account holds none of",
            request: || {
                json!({
                    "name": "live",
                    "cluster": "ghostcluster",
                    "autoScalingGroupProvider": { "managedDraining": "DISABLED" },
                })
            },
            code: "ClusterNotFoundException",
            message: "Cluster not found.",
            untouched: Some("live"),
        },
        UpdateRefusalCase {
            case: "a cluster-scoped provider updated outside its cluster",
            request: || {
                json!({
                    "name": "scopedmanaged",
                    "cluster": "other",
                    "managedInstancesProvider": managed_instances(),
                })
            },
            code: "InvalidParameterException",
            message: CLUSTER_SCOPED_UPDATE_REFUSAL,
            untouched: Some("scopedmanaged"),
        },
        UpdateRefusalCase {
            case: "a cluster-scoped provider updated with no cluster at all",
            request: || {
                json!({
                    "name": "scopedmanaged",
                    "managedInstancesProvider": managed_instances(),
                })
            },
            code: "InvalidParameterException",
            message: CLUSTER_SCOPED_UPDATE_REFUSAL,
            untouched: Some("scopedmanaged"),
        },
        UpdateRefusalCase {
            case: "the managed-instances configuration on an Auto Scaling group provider",
            request: || {
                json!({ "name": "live", "managedInstancesProvider": managed_instances() })
            },
            code: "InvalidParameterException",
            message: CONFIGURATION_CLASS_REFUSAL,
            untouched: Some("live"),
        },
        UpdateRefusalCase {
            case: "the Auto Scaling group configuration on a managed-instances provider",
            request: || {
                json!({
                    "name": "scopedmanaged",
                    "cluster": "scoped",
                    "autoScalingGroupProvider": { "managedDraining": "DISABLED" },
                })
            },
            code: "InvalidParameterException",
            message: CONFIGURATION_CLASS_REFUSAL,
            untouched: Some("scopedmanaged"),
        },
        UpdateRefusalCase {
            case: "no configuration at all",
            request: || json!({ "name": "live" }),
            code: "InvalidParameterException",
            message: "either autoScalingGroupProvider or managedInstancesProvider must be \
                      specified",
            untouched: Some("live"),
        },
        UpdateRefusalCase {
            case: "both configurations at once",
            request: || {
                json!({
                    "name": "live",
                    "autoScalingGroupProvider": { "managedDraining": "DISABLED" },
                    "managedInstancesProvider": managed_instances(),
                })
            },
            code: "InvalidParameterException",
            message: "autoScalingGroupProvider and managedInstancesProvider must not both be \
                      specified",
            untouched: Some("live"),
        },
        UpdateRefusalCase {
            case: "a scaling step size range that bounds nothing",
            request: || {
                json!({
                    "name": "live",
                    "autoScalingGroupProvider": {
                        "managedScaling": {
                            "minimumScalingStepSize": 7,
                            "maximumScalingStepSize": 6,
                        },
                    },
                })
            },
            code: "ClientException",
            message: STEP_SIZE_REFUSAL,
            untouched: Some("live"),
        },
    ];

    for row in &rows {
        let case = row.case;
        let before = match row.untouched {
            Some(provider) => Some(describe(&router, provider, true, case).await),
            None => None,
        };

        assert_eq!(
            refusal(&router, "UpdateCapacityProvider", (row.request)(), case).await,
            (row.code.to_string(), row.message.to_string()),
            "case {case:?}: unexpected refusal"
        );

        if let (Some(provider), Some(before)) = (row.untouched, before) {
            let after = describe(&router, provider, true, case).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused update writes nothing, got {after}"
            );
        }
    }
}

/// The whole sentence an operation asked to MODIFY a capacity provider Amazon ECS reserves
/// for every account is refused with: fixed text for the class, naming neither the provider
/// the request carried nor the operation that was asked.
const RESERVED_PROVIDER_REFUSAL: &str =
    "The specified capacity provider is reserved and cannot be modified by this operation. \
     Specify a valid name or ARN and try again.";

/// The class that refusal is answered in: a client action Amazon ECS will not perform, as
/// against the `InvalidParameterException` a parameter value it cannot read earns.
const RESERVED_PROVIDER_CODE: &str = "ClientException";

/// The capacity providers Amazon ECS reserves for every account — the whole catalogue, as
/// `UpdateService$capacityProviderStrategy` publishes it ("When you use Fargate, the
/// capacity providers are FARGATE or FARGATE_SPOT"; `smithy_query` query="doc"
/// shape="UpdateServiceRequest" member="capacityProviderStrategy").
const RESERVED_PROVIDERS: [&str; 2] = ["FARGATE", "FARGATE_SPOT"];

/// One row of the reserved-provider table: which operation is asked, and the request it is
/// asked with.
struct ReservedCase {
    case: &'static str,
    operation: &'static str,
    request: fn() -> Value,
}

#[tokio::test]
async fn a_reserved_capacity_provider_cannot_be_modified() {
    // The rule: a capacity provider Amazon ECS reserves for every account is published by
    // the service rather than held by the caller, so an operation asked to MODIFY one — to
    // retire it or to change its configuration — is refused with HTTP 400 and
    // `ClientException` carrying the one fixed reserved-provider sentence, whatever else the
    // request names, and nothing about the provider changes.
    //
    // The rows differ along what the rule generalises over: WHICH reserved provider is named
    // (both of them, which is the whole catalogue, [`RESERVED_PROVIDERS`]), HOW it is named
    // (the short name and the full ARN form a reference to a provider accepts), WHICH
    // operation asks (the delete and the update), whether the request names a live `cluster`
    // as well, and which class of configuration an update carries.
    let router = service();
    create_cluster(&router, "live", "setup").await;
    create_auto_scaling_group(&router, "asg-unreserved", "setup").await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "unreserved",
            "autoScalingGroupProvider": { "autoScalingGroupArn": group_arn("asg-unreserved") },
        }),
        "setup",
    )
    .await;

    let rows = [
        ReservedCase {
            case: "a delete naming FARGATE by its short name",
            operation: "DeleteCapacityProvider",
            request: || json!({ "capacityProvider": "FARGATE" }),
        },
        ReservedCase {
            case: "a delete naming FARGATE_SPOT by its short name",
            operation: "DeleteCapacityProvider",
            request: || json!({ "capacityProvider": "FARGATE_SPOT" }),
        },
        ReservedCase {
            case: "a delete naming FARGATE by its ARN",
            operation: "DeleteCapacityProvider",
            request: || json!({ "capacityProvider": capacity_provider_arn("FARGATE") }),
        },
        ReservedCase {
            case: "a delete naming FARGATE_SPOT by its ARN, through a live cluster",
            operation: "DeleteCapacityProvider",
            request: || {
                json!({
                    "capacityProvider": capacity_provider_arn("FARGATE_SPOT"),
                    "cluster": "live",
                })
            },
        },
        ReservedCase {
            case: "a delete naming FARGATE through a live cluster",
            operation: "DeleteCapacityProvider",
            request: || json!({ "capacityProvider": "FARGATE", "cluster": "live" }),
        },
        ReservedCase {
            case: "an update of FARGATE carrying an Auto Scaling group configuration",
            operation: "UpdateCapacityProvider",
            request: || {
                json!({
                    "name": "FARGATE",
                    "autoScalingGroupProvider": { "managedDraining": "DISABLED" },
                })
            },
        },
        ReservedCase {
            case: "an update of FARGATE_SPOT carrying a managed-scaling block",
            operation: "UpdateCapacityProvider",
            request: || {
                json!({
                    "name": "FARGATE_SPOT",
                    "autoScalingGroupProvider": {
                        "managedScaling": { "status": "ENABLED", "targetCapacity": 50 },
                    },
                })
            },
        },
        ReservedCase {
            case: "an update of FARGATE carrying a managed-instances configuration",
            operation: "UpdateCapacityProvider",
            request: || {
                json!({ "name": "FARGATE", "managedInstancesProvider": managed_instances() })
            },
        },
        ReservedCase {
            case: "an update naming FARGATE_SPOT by its ARN, through a live cluster",
            operation: "UpdateCapacityProvider",
            request: || {
                json!({
                    "name": capacity_provider_arn("FARGATE_SPOT"),
                    "cluster": "live",
                    "autoScalingGroupProvider": { "managedDraining": "ENABLED" },
                })
            },
        },
    ];

    for row in &rows {
        let case = row.case;
        let (code, message) = refusal(&router, row.operation, (row.request)(), case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (RESERVED_PROVIDER_CODE, RESERVED_PROVIDER_REFUSAL),
            "case {case:?}: expected the reserved-provider refusal, got {code:?} / {message:?}"
        );
    }

    // Every refused call left the reserved providers exactly as the account holds them: each
    // is still reported, `ACTIVE`, with no update of any kind against it — so the refusal is
    // the request being turned away and not a provider being retired.
    for name in RESERVED_PROVIDERS {
        let case = "a reserved provider the refusals did not touch";
        let provider = describe(&router, name, false, case).await;
        assert_eq!(
            (string(&provider, "name", case), lifecycle(&provider, case)),
            (name.to_string(), ("ACTIVE".to_string(), None)),
            "case {case:?}: expected {name} ACTIVE with no update, got {provider}"
        );
    }

    // The same two operations serve a provider the ACCOUNT holds, so the rows above are the
    // reserved names being refused and not these operations refusing every request: the
    // caller's own provider takes the update and is then retired.
    let case = "the account's own provider is modified by the same operations";
    let updated = ok(
        &router,
        "UpdateCapacityProvider",
        json!({
            "name": "unreserved",
            "autoScalingGroupProvider": { "managedDraining": "DISABLED" },
        }),
        case,
    )
    .await;
    assert_eq!(
        string(&updated["capacityProvider"], "name", case),
        "unreserved",
        "case {case:?}: expected the update to report the provider, got {updated}"
    );
    let deleted = ok(
        &router,
        "DeleteCapacityProvider",
        json!({ "capacityProvider": "unreserved" }),
        case,
    )
    .await;
    assert_eq!(
        string(&deleted["capacityProvider"], "status", case),
        "INACTIVE",
        "case {case:?}: expected the delete to retire the provider, got {deleted}"
    );
}
