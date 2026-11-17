// SPDX-License-Identifier: CC0-1.0

//! What the CAPACITY-PROVIDER surface reports about a provider and about an identifier it
//! cannot resolve, driven end to end over the shared wire harness in [`crate::ecs_wire`].
//!
//! The rules under test:
//!
//! * `CapacityProvider$type` names the class of capacity the provider binds onto, as a
//!   value of `CapacityProviderType` (`smithy_query(query="shape",
//!   shape="com.amazonaws.ecs#CapacityProviderType")` -> `["EC2_AUTOSCALING",
//!   "MANAGED_INSTANCES", "FARGATE", "FARGATE_SPOT"]`): the caller's own Auto Scaling
//!   group, the instances Amazon ECS manages for a cluster, or — for each provider Amazon
//!   ECS holds for every account — the Fargate class that provider is;
//! * `CapacityProvider$tags` is reported whatever the request asked for: the provider's own
//!   tags when `include` names `TAGS`, and the empty list otherwise, so a client reading the
//!   member never gets an absent value;
//! * an identifier a `DescribeCapacityProviders` cannot resolve fails BESIDE the results —
//!   200 with one `failures` entry naming the identifier as the request spelled it and the
//!   one sentence that states no provider answers to it — rather than failing the call;
//! * a page size above the page this read serves is served that page, and only a page size
//!   below one — which asks for no page at all — is refused; and
//! * `autoScalingGroupProvider` names a group of the CALLER's, so a create whose reference
//!   resolves to no Auto Scaling group of the account in the Region the request was signed
//!   for is refused with `ClientException` and stores nothing; and
//! * a provider over an Auto Scaling group of the caller's is a resource of the account from
//!   the moment its create is answered, so EVERY lookup that names one resolves it against
//!   those stored records — `DescribeCapacityProviders` by either of the two forms its own
//!   member documents, and the members documented as taking the provider's SHORT NAME (a
//!   cluster create's `capacityProviders` and `defaultCapacityProviderStrategy`, a
//!   placement's and a service update's `capacityProviderStrategy`) — so each of those calls
//!   is SERVED, and each operation's modelled refusals stay reserved for what they state: a
//!   provider the account holds none of, a cluster it holds none of, and a provider whose
//!   capacity holds no container instance.
//!
//! Every case goes through the same front door a client uses — including the Auto Scaling
//! groups, which are created through that service's own API — so nothing reaches into a
//! state struct and a response that is not computed from persisted state cannot pass.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::capacity_providers::{group_arn, managed_instances};
use crate::ecs_wire::{
    auto_scaling_group_arn, caller_principal, count, create_auto_scaling_group,
    create_auto_scaling_group_in, create_cluster, list, ok, refusal, register_capacity,
    register_definition, service, string, ACCOUNT, SIGNED_REGION,
};

/// The capacity providers Amazon ECS holds for every account, in every Region
/// (`CapacityProviderStrategyItem`: "the only valid capacity provider values are FARGATE or
/// FARGATE_SPOT"). Nothing creates them and nothing can take their names.
const BUILT_IN_PROVIDERS: [&str; 2] = ["FARGATE", "FARGATE_SPOT"];

/// The whole sentence a `failures` entry carries for a capacity-provider identifier that
/// names no provider of the account.
const DOES_NOT_EXIST: &str = "The specified capacity provider does not exist. Specify a \
                              valid name or ARN and try again.";

/// The whole sentence a create whose `autoScalingGroupProvider` names no Auto Scaling group
/// of the account is refused with.
const NO_SUCH_GROUP: &str = "The specified Auto Scaling group does not exist. Specify a \
                             valid Auto Scaling group and try again.";

/// The ARN of the capacity provider named `name` in the signed Region.
fn provider_arn(name: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:capacity-provider/{name}")
}

/// Describe the providers `references` names, with or without their tags, and return the
/// whole answer.
async fn describe(
    router: &MultiServiceRouter,
    references: &[String],
    include_tags: bool,
    case: &str,
) -> Value {
    let mut request = json!({ "capacityProviders": references });
    if include_tags {
        request["include"] = json!(["TAGS"]);
    }
    ok(router, "DescribeCapacityProviders", request, case).await
}

/// One row of the `type`/`tags` table: a provider of one capacity class, how it gets into
/// the account, and what the surface must then report about it.
struct ClassCase {
    case: &'static str,
    /// The provider's name, which for a built-in one is not created at all.
    provider_name: &'static str,
    /// The Auto Scaling group the provider binds onto, if any.
    group: Option<&'static str>,
    /// The members `CreateCapacityProvider` carries beside `name`, or `None` for a provider
    /// nothing creates.
    request: Option<fn() -> Value>,
    /// The `CapacityProviderType` value the provider must report.
    expect_type: &'static str,
    /// The tags the provider must report when the request asks for them.
    expect_tags: fn() -> Value,
}

#[tokio::test]
async fn every_capacity_provider_reports_the_class_it_binds_onto_and_a_tag_list() {
    let router = service();
    create_cluster(&router, "classes", "setup").await;

    // The rows span every class of capacity a provider binds onto — the two Fargate
    // providers Amazon ECS holds for the account, one over a caller's Auto Scaling group,
    // and one over the instances Amazon ECS manages for a cluster — and, across them, a
    // provider created with tags and one created with none.
    let rows: [ClassCase; 5] = [
        ClassCase {
            case: "the Fargate provider Amazon ECS holds for the account",
            provider_name: "FARGATE",
            group: None,
            request: None,
            expect_type: "FARGATE",
            expect_tags: || json!([]),
        },
        ClassCase {
            case: "the Fargate Spot provider Amazon ECS holds for the account",
            provider_name: "FARGATE_SPOT",
            group: None,
            request: None,
            expect_type: "FARGATE_SPOT",
            expect_tags: || json!([]),
        },
        ClassCase {
            case: "a provider over a caller's Auto Scaling group, with tags",
            provider_name: "overgroup",
            group: Some("asg-overgroup"),
            request: Some(|| {
                json!({
                    "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-overgroup" },
                    "tags": [{ "key": "team", "value": "platform" }],
                })
            }),
            expect_type: "EC2_AUTOSCALING",
            expect_tags: || json!([{ "key": "team", "value": "platform" }]),
        },
        ClassCase {
            case: "a provider over a caller's Auto Scaling group named by its ARN, untagged",
            provider_name: "byarnclass",
            group: Some("asg-byarnclass"),
            request: Some(|| {
                json!({
                    "autoScalingGroupProvider": {
                        "autoScalingGroupArn": group_arn("asg-byarnclass"),
                    },
                })
            }),
            expect_type: "EC2_AUTOSCALING",
            expect_tags: || json!([]),
        },
        ClassCase {
            case: "a provider over the instances Amazon ECS manages for a cluster",
            provider_name: "overmanaged",
            group: None,
            request: Some(|| {
                json!({
                    "cluster": "classes",
                    "managedInstancesProvider": managed_instances(),
                    "tags": [{ "key": "env", "value": "test" }],
                })
            }),
            expect_type: "MANAGED_INSTANCES",
            expect_tags: || json!([{ "key": "env", "value": "test" }]),
        },
    ];

    for row in &rows {
        let case = row.case;
        if let Some(group) = row.group {
            create_auto_scaling_group(&router, group, case).await;
        }
        if let Some(request) = row.request {
            let mut request = request();
            request["name"] = json!(row.provider_name);
            let created = ok(&router, "CreateCapacityProvider", request, case).await;
            let created = &created["capacityProvider"];
            // The create has no `include` member to narrow its answer with, so it reports
            // the class it stored and the tags it was given.
            assert_eq!(
                created["type"].as_str(),
                Some(row.expect_type),
                "case {case:?}: expected the create to report type {:?}, got {created}",
                row.expect_type
            );
            assert_eq!(
                created["tags"],
                (row.expect_tags)(),
                "case {case:?}: unexpected tags from the create, got {created}"
            );
        }

        // The same pair reads back, so the answer is the stored record rather than one the
        // create made up.
        let references = vec![row.provider_name.to_string()];
        for (include_tags, expected_tags) in
            [(true, (row.expect_tags)()), (false, json!([]))]
        {
            let body = describe(&router, &references, include_tags, case).await;
            let described = list(&body, "capacityProviders", case);
            assert_eq!(
                described.len(),
                1,
                "case {case:?}: expected exactly the named provider, got {body}"
            );
            let provider = &described[0];
            assert_eq!(
                provider["type"].as_str(),
                Some(row.expect_type),
                "case {case:?}: expected type {:?} read back, got {provider}",
                row.expect_type
            );
            assert_eq!(
                provider["tags"], expected_tags,
                "case {case:?}: expected tags {expected_tags} with include_tags \
                 {include_tags}, got {provider}"
            );
        }
    }

    // Every class a provider of this account reports is a value of the modelled set, so a
    // client switching on it never sees one outside it.
    let case = "every provider of the account";
    let body = ok(&router, "DescribeCapacityProviders", json!({}), case).await;
    for provider in list(&body, "capacityProviders", case) {
        let reported = string(&provider, "type", case);
        assert!(
            ["EC2_AUTOSCALING", "MANAGED_INSTANCES", "FARGATE", "FARGATE_SPOT"]
                .contains(&reported.as_str()),
            "case {case:?}: {reported:?} is not a CapacityProviderType, got {provider}"
        );
    }
}

#[tokio::test]
async fn an_unfiltered_describe_lists_the_account_s_providers_and_fails_nothing() {
    let router = service();

    // A call that names no provider and no cluster describes every provider the account
    // holds, which in a fresh account is exactly the two Amazon ECS holds for it. The rows
    // differ along the page size the request asks for: none at all, the smallest, the
    // largest page this read serves, and asks ABOVE that page — which are served the page
    // this read serves rather than refused.
    let rows: [(&str, Value); 5] = [
        ("no page size at all", json!({})),
        ("the smallest page", json!({ "maxResults": 1 })),
        ("the largest page this read serves", json!({ "maxResults": 10 })),
        ("one page above it", json!({ "maxResults": 11 })),
        ("far above it", json!({ "maxResults": 1_000 })),
    ];

    for (case, request) in &rows {
        let smallest = request["maxResults"].as_i64() == Some(1);
        let body = ok(&router, "DescribeCapacityProviders", request.clone(), case).await;
        let mut names: Vec<String> = list(&body, "capacityProviders", case)
            .iter()
            .map(|provider| string(provider, "name", case))
            .collect();
        if smallest {
            // One name per page, walked through to the end by the token.
            let mut token = body["nextToken"].as_str().map(str::to_string);
            while let Some(next) = token {
                let body = ok(
                    &router,
                    "DescribeCapacityProviders",
                    json!({ "maxResults": 1, "nextToken": next }),
                    case,
                )
                .await;
                names.extend(
                    list(&body, "capacityProviders", case)
                        .iter()
                        .map(|provider| string(provider, "name", case)),
                );
                token = body["nextToken"].as_str().map(str::to_string);
            }
        }
        names.sort();
        assert_eq!(
            names,
            BUILT_IN_PROVIDERS.map(str::to_string).to_vec(),
            "case {case:?}: expected the providers Amazon ECS holds for the account, got \
             {body}"
        );
        assert_eq!(
            body["failures"],
            json!([]),
            "case {case:?}: a call that names no identifier fails none, got {body}"
        );
        for provider in list(&body, "capacityProviders", case) {
            let name = string(&provider, "name", case);
            assert_eq!(
                (
                    provider["capacityProviderArn"].as_str(),
                    provider["status"].as_str(),
                    provider["type"].as_str(),
                    provider["tags"].clone(),
                ),
                (
                    Some(provider_arn(&name).as_str()),
                    Some("ACTIVE"),
                    Some(name.as_str()),
                    json!([]),
                ),
                "case {case:?}: unexpected projection of {name}, got {provider}"
            );
        }
    }

    // A page size below the smallest page asks for no page at all, which is the one value
    // this member cannot be given.
    let case = "no page at all";
    let (code, message) = refusal(
        &router,
        "DescribeCapacityProviders",
        json!({ "maxResults": 0 }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("InvalidParameterException", "Max results can not be lesser than 1"),
        "case {case:?}: expected the smallest page named, got {code:?} with {message:?}"
    );
}

/// One row of the unresolved-identifier table: the identifiers one call names, the
/// providers it must describe, and the identifiers it must report as naming none.
type FailureCase = (&'static str, Vec<String>, Vec<&'static str>, Vec<String>);

#[tokio::test]
async fn an_identifier_that_names_no_provider_is_reported_beside_the_results() {
    let router = service();
    create_auto_scaling_group(&router, "asg-known", "setup").await;
    ok(
        &router,
        "CreateCapacityProvider",
        json!({
            "name": "known",
            "autoScalingGroupProvider": { "autoScalingGroupArn": "asg-known" },
        }),
        "setup",
    )
    .await;

    // The rows differ along how the request spells the identifier that resolves to nothing
    // (a short name, a full ARN, a name that merely resembles a stored one), how many of
    // them one call carries, and whether a resolvable identifier is asked for beside them.
    // Every row's expectation is the SAME rule: one entry per unresolved identifier, naming
    // it as the request spelled it.
    let rows: Vec<FailureCase> = vec![
        (
            "one short name nothing holds",
            vec!["ghost".to_string()],
            vec![],
            vec!["ghost".to_string()],
        ),
        (
            "one full ARN nothing holds",
            vec![provider_arn("ghost")],
            vec![],
            vec![provider_arn("ghost")],
        ),
        (
            "a name that only resembles a stored one",
            vec!["known2".to_string()],
            vec![],
            vec!["known2".to_string()],
        ),
        (
            "a stored provider beside a name nothing holds",
            vec!["known".to_string(), "ghost".to_string()],
            vec!["known"],
            vec!["ghost".to_string()],
        ),
        (
            "a stored provider named by ARN beside two that name nothing",
            vec![
                provider_arn("known"),
                "ghost".to_string(),
                provider_arn("phantom"),
            ],
            vec!["known"],
            vec!["ghost".to_string(), provider_arn("phantom")],
        ),
    ];

    for (case, references, expected_names, expected_failures) in &rows {
        let body = describe(&router, references, true, case).await;
        let names: Vec<String> = list(&body, "capacityProviders", case)
            .iter()
            .map(|provider| string(provider, "name", case))
            .collect();
        assert_eq!(
            names, *expected_names,
            "case {case:?}: unexpected providers described, got {body}"
        );
        let failures: Vec<(String, String)> = list(&body, "failures", case)
            .iter()
            .map(|failure| {
                (
                    string(failure, "arn", case),
                    string(failure, "reason", case),
                )
            })
            .collect();
        let expected: Vec<(String, String)> = expected_failures
            .iter()
            .map(|reference| (reference.clone(), DOES_NOT_EXIST.to_string()))
            .collect();
        assert_eq!(
            failures, expected,
            "case {case:?}: expected one failure per unresolved identifier, got {body}"
        );
    }
}

/// One row of the Auto Scaling group table: how the create names the group, whether the
/// account holds it, and so whether the provider is created at all.
struct GroupCase {
    case: &'static str,
    provider_name: &'static str,
    /// The group to create first, and the Region to create it in.
    existing_group: Option<(&'static str, &'static str)>,
    /// The `autoScalingGroupArn` the create carries.
    reference: fn() -> String,
    accepted: bool,
}

#[tokio::test]
async fn a_provider_over_an_auto_scaling_group_the_account_does_not_hold_is_refused() {
    let router = service();

    // The rows differ along how the create spells the group (a bare name, a full Auto
    // Scaling group ARN), whether the account holds a group of that name, which Region holds
    // it, and whether the spelling of the create matches the spelling of the group's own
    // create — naming one group by ARN and by name names the same group.
    let rows: [GroupCase; 6] = [
        GroupCase {
            case: "a bare name the account holds",
            provider_name: "heldbyname",
            existing_group: Some(("asg-held", SIGNED_REGION)),
            reference: || "asg-held".to_string(),
            accepted: true,
        },
        GroupCase {
            case: "the ARN of a group the account created by name",
            provider_name: "heldbyarn",
            existing_group: Some(("asg-heldarn", SIGNED_REGION)),
            reference: || group_arn("asg-heldarn"),
            accepted: true,
        },
        GroupCase {
            case: "a bare name nothing holds",
            provider_name: "absentbyname",
            existing_group: None,
            reference: || "asg-absent".to_string(),
            accepted: false,
        },
        GroupCase {
            case: "the ARN of a group nothing holds",
            provider_name: "absentbyarn",
            existing_group: None,
            reference: || group_arn("asg-absentarn"),
            accepted: false,
        },
        GroupCase {
            case: "a group of another Region",
            provider_name: "elsewhere",
            existing_group: Some(("asg-elsewhere", "us-east-1")),
            reference: || "asg-elsewhere".to_string(),
            accepted: false,
        },
        GroupCase {
            case: "a name that only resembles a group the account holds",
            provider_name: "nearmiss",
            existing_group: Some(("asg-near", SIGNED_REGION)),
            reference: || "asg-near2".to_string(),
            accepted: false,
        },
    ];

    for row in &rows {
        let case = row.case;
        if let Some((group, region)) = row.existing_group {
            create_auto_scaling_group_in(&router, region, group, case).await;
        }
        let request = json!({
            "name": row.provider_name,
            "autoScalingGroupProvider": { "autoScalingGroupArn": (row.reference)() },
        });
        if row.accepted {
            let created = ok(&router, "CreateCapacityProvider", request, case).await;
            assert_eq!(
                created["capacityProvider"]["capacityProviderArn"].as_str(),
                Some(provider_arn(row.provider_name).as_str()),
                "case {case:?}: expected the provider created, got {created}"
            );
        } else {
            let (code, message) = refusal(&router, "CreateCapacityProvider", request, case).await;
            assert_eq!(
                (code.as_str(), message.as_str()),
                ("ClientException", NO_SUCH_GROUP),
                "case {case:?}: expected the group refusal, got {code:?} with {message:?}"
            );
        }

        // The refused create stored nothing, and the accepted one stored exactly the
        // provider it answered with: the read decides, not the create's answer.
        let body = describe(
            &router,
            &[row.provider_name.to_string()],
            false,
            case,
        )
        .await;
        let described: Vec<String> = list(&body, "capacityProviders", case)
            .iter()
            .map(|provider| string(provider, "name", case))
            .collect();
        let expected: Vec<String> = if row.accepted {
            vec![row.provider_name.to_string()]
        } else {
            vec![]
        };
        assert_eq!(
            described, expected,
            "case {case:?}: a provider exists exactly when its create was accepted, got \
             {body}"
        );
    }
}

/// `InvalidParameterException$message` for an association that names a capacity provider
/// this account holds none of: one fixed sentence that names no provider.
const PROVIDER_NOT_FOUND: &str = "The specified capacity provider was not found. Specify a \
                                  valid capacity provider and try again.";

/// `ClusterNotFoundException$message`, the whole of what that error states ("The specified
/// cluster wasn't found"), which is the refusal reserved for a cluster the account does not
/// hold.
const CLUSTER_NOT_FOUND: &str = "Cluster not found.";

/// `Failure$reason` for an identifier that names nothing, from the API failure reasons:
/// "MISSING - the described resource could not be found".
const MISSING: &str = "MISSING";

/// `Deployment$rolloutState`'s whole value set (`smithy_query(query="shape",
/// shape="com.amazonaws.ecs#DeploymentRolloutState")` -> COMPLETED, FAILED, IN_PROGRESS).
const ROLLOUT_STATES: [&str; 3] = ["COMPLETED", "FAILED", "IN_PROGRESS"];

/// The whole sentence a placement through a capacity provider whose capacity holds no
/// container instance is refused with. Built from the request: the provider the strategy
/// named is the one whose Auto Scaling group has to grow, and a cluster can carry several.
fn no_container_instances(provider: &str) -> String {
    format!("The specified capacity provider '{provider}' contains no container instances.")
}

/// The ARN of the cluster `name` in the signed Region.
fn cluster_arn(name: &str) -> String {
    format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:cluster/{name}")
}

/// A `capacityProviderStrategy` (or a cluster's `defaultCapacityProviderStrategy`) as the
/// three values each entry carries, so a strategy is compared whether or not the answer
/// spells out the `base` the request left at its modelled default.
fn strategy_entries(body: &Value, member: &str, case: &str) -> Vec<(String, i64, i64)> {
    list(body, member, case)
        .iter()
        .map(|item| {
            (
                string(item, "capacityProvider", case),
                count(item, "weight", case),
                count(item, "base", case),
            )
        })
        .collect()
}

/// One row of the EC2-capacity bootstrap table: a capacity provider over an Auto Scaling
/// group of the caller's, plus the names the row's own cluster and service take.
struct BootstrapRow {
    case: &'static str,
    provider: &'static str,
    group: &'static str,
    cluster: &'static str,
    /// The `managedScaling` block the create carries, or `None` for a create that leaves the
    /// whole block to Amazon ECS.
    managed_scaling: Option<Value>,
    /// The `managedScaling` block the stored provider must then report.
    expect_scaling: Value,
}

/// The `scalingApproach` a managed-scaling block is reported with: Amazon ECS "manages a
/// target tracking scaling policy" for the group (`ManagedScaling`'s own doc), which is the
/// value the recorded answers of the real service carry.
const SCALING_POLICY: &str = "SCALING_POLICY";

/// The rows every bootstrap test below walks.
///
/// They differ along how much of the Auto Scaling group binding the create spells out —
/// nothing but the group, the whole block at the value each member's own doc publishes as its
/// default, and a block that sizes its scaling steps and target capacity itself — because the
/// provider has to be resolvable whatever its binding says. Managed scaling is OFF in every
/// row (absent is the documented default), so the only capacity behind each provider is what
/// its group registers into the cluster.
fn bootstrap_rows() -> Vec<BootstrapRow> {
    // The members Amazon ECS fills in for a create that names none, each from its own
    // member doc: "If this parameter is omitted, the default value of 1 is used"
    // (minimumScalingStepSize), "of 10000" (maximumScalingStepSize), "of 300 seconds"
    // (instanceWarmupPeriod), and "The default value of 100 percent" (targetCapacity).
    let filled_in = json!({
        "scalingApproach": SCALING_POLICY,
        "targetCapacity": 100,
        "minimumScalingStepSize": 1,
        "maximumScalingStepSize": 10_000,
        "instanceWarmupPeriod": 300,
    });
    let turned_off = json!({
        "scalingApproach": SCALING_POLICY,
        "status": "DISABLED",
        "targetCapacity": 100,
        "minimumScalingStepSize": 1,
        "maximumScalingStepSize": 10_000,
        "instanceWarmupPeriod": 300,
    });
    // A block whose steps and target capacity are the caller's own, inside the ranges the
    // members declare (`targetCapacity` 1..100, either step size 1..10000, the warmup
    // period 0..10000).
    let sized = json!({
        "scalingApproach": SCALING_POLICY,
        "status": "DISABLED",
        "targetCapacity": 75,
        "minimumScalingStepSize": 2,
        "maximumScalingStepSize": 20,
        "instanceWarmupPeriod": 120,
    });
    vec![
        BootstrapRow {
            case: "a provider whose create names nothing but its group",
            provider: "bootdefault",
            group: "asg-bootdefault",
            cluster: "bootdefault",
            managed_scaling: None,
            expect_scaling: filled_in,
        },
        BootstrapRow {
            case: "a provider whose create turns managed scaling off",
            provider: "bootoff",
            group: "asg-bootoff",
            cluster: "bootoff",
            managed_scaling: Some(turned_off.clone()),
            expect_scaling: turned_off,
        },
        BootstrapRow {
            case: "a provider whose create sizes its own scaling steps",
            provider: "bootsized",
            group: "asg-bootsized",
            cluster: "bootsized",
            managed_scaling: Some(sized.clone()),
            expect_scaling: sized,
        },
    ]
}

/// Create the row's Auto Scaling group — through that service's OWN API — and the capacity
/// provider bound onto it, and answer the provider the create reported.
async fn create_group_provider(router: &MultiServiceRouter, row: &BootstrapRow) -> Value {
    create_auto_scaling_group(router, row.group, row.case).await;
    let mut binding = json!({ "autoScalingGroupArn": row.group });
    if let Some(scaling) = row.managed_scaling.clone() {
        binding["managedScaling"] = scaling;
    }
    let created = ok(
        router,
        "CreateCapacityProvider",
        json!({ "name": row.provider, "autoScalingGroupProvider": binding }),
        row.case,
    )
    .await;
    created["capacityProvider"].clone()
}

/// Create the row's cluster, associating the row's provider with it and defaulting over it,
/// and answer the cluster the create reported.
async fn create_associated_cluster(router: &MultiServiceRouter, row: &BootstrapRow) -> Value {
    let created = ok(
        router,
        "CreateCluster",
        json!({
            "clusterName": row.cluster,
            "capacityProviders": [row.provider],
            "defaultCapacityProviderStrategy": [
                { "capacityProvider": row.provider, "weight": 1 },
            ],
        }),
        row.case,
    )
    .await;
    created["cluster"].clone()
}

/// The `capacityProviderStrategy` the row's placements and service updates are made with:
/// one entry over the row's own provider, named by the SHORT NAME
/// `CapacityProviderStrategyItem$capacityProvider` documents.
fn row_strategy(row: &BootstrapRow) -> Value {
    json!([{ "capacityProvider": row.provider, "weight": 1 }])
}

#[tokio::test]
async fn a_group_provider_is_described_and_associable_from_its_create_on() {
    let router = service();

    for row in bootstrap_rows() {
        let case = row.case;
        let created = create_group_provider(&router, &row).await;
        // The group's identity comes from the service that OWNS the group, so the binding is
        // compared against what Amazon EC2 Auto Scaling minted rather than an ARN composed
        // here.
        let group = auto_scaling_group_arn(&router, row.group, case).await;

        // "Only capacity providers in an ACTIVE state can be used in a cluster": a provider
        // over a group the caller ALREADY has is usable from this answer on.
        assert_eq!(
            (
                created["status"].as_str(),
                created["type"].as_str(),
                created["capacityProviderArn"].as_str(),
            ),
            (
                Some("ACTIVE"),
                Some("EC2_AUTOSCALING"),
                Some(provider_arn(row.provider).as_str()),
            ),
            "case {case:?}: unexpected provider from the create, got {created}"
        );

        // The provider resolves under BOTH forms its own member documents ("The short name
        // or full Amazon Resource Name (ARN) of one or more capacity providers"), and each
        // answer is the stored record rather than one the create made up.
        for (spelling, reference) in [
            ("its short name", row.provider.to_string()),
            ("its full ARN", provider_arn(row.provider)),
        ] {
            let case = &format!("{case}, described by {spelling}");
            let body = describe(&router, &[reference], true, case).await;
            assert_eq!(
                body["failures"],
                json!([]),
                "case {case:?}: a provider the account holds is no failure, got {body}"
            );
            let described = list(&body, "capacityProviders", case);
            assert_eq!(
                described.len(),
                1,
                "case {case:?}: expected exactly the provider named, got {body}"
            );
            let provider = &described[0];
            assert_eq!(
                (
                    provider["name"].as_str(),
                    provider["capacityProviderArn"].as_str(),
                    provider["status"].as_str(),
                    provider["type"].as_str(),
                    provider["tags"].clone(),
                ),
                (
                    Some(row.provider),
                    Some(provider_arn(row.provider).as_str()),
                    Some("ACTIVE"),
                    Some("EC2_AUTOSCALING"),
                    json!([]),
                ),
                "case {case:?}: unexpected projection of the provider, got {provider}"
            );
            // "The Auto Scaling group settings for the capacity provider": the group it was
            // bound onto, the managed-scaling block it was created with, and the draining
            // option Amazon ECS applies when the create names none.
            let binding = &provider["autoScalingGroupProvider"];
            assert_eq!(
                (
                    binding["autoScalingGroupArn"].as_str(),
                    binding["managedDraining"].as_str(),
                ),
                (Some(group.as_str()), Some("ENABLED")),
                "case {case:?}: unexpected binding read back, got {provider}"
            );
            assert_eq!(
                binding["managedScaling"], row.expect_scaling,
                "case {case:?}: expected the managed scaling the provider was created \
                 with, got {provider}"
            );
            assert_eq!(
                *binding, created["autoScalingGroupProvider"],
                "case {case:?}: the read reports the binding the create stored, got \
                 {provider}"
            );
        }

        // "A capacity provider must be associated with a cluster before it can be included
        // as part of the default capacity provider strategy of the cluster": the create that
        // associates it is ANSWERED, with the cluster carrying both lists.
        let cluster = create_associated_cluster(&router, &row).await;
        assert_eq!(
            (
                cluster["status"].as_str(),
                cluster["clusterArn"].as_str(),
                list(&cluster, "capacityProviders", case),
            ),
            (
                Some("ACTIVE"),
                Some(cluster_arn(row.cluster).as_str()),
                vec![json!(row.provider)],
            ),
            "case {case:?}: unexpected cluster from the create, got {cluster}"
        );
        assert_eq!(
            strategy_entries(&cluster, "defaultCapacityProviderStrategy", case),
            vec![(row.provider.to_string(), 1, 0)],
            "case {case:?}: expected the default strategy the create named, got {cluster}"
        );

        // And the association READS back, so it was persisted rather than echoed.
        let body = ok(
            &router,
            "DescribeClusters",
            json!({ "clusters": [row.cluster] }),
            case,
        )
        .await;
        assert_eq!(
            body["failures"],
            json!([]),
            "case {case:?}: a cluster this account holds is no failure, got {body}"
        );
        let described = list(&body, "clusters", case);
        assert_eq!(
            described.len(),
            1,
            "case {case:?}: expected the cluster back, got {body}"
        );
        assert_eq!(
            (
                list(&described[0], "capacityProviders", case),
                strategy_entries(&described[0], "defaultCapacityProviderStrategy", case),
            ),
            (
                vec![json!(row.provider)],
                vec![(row.provider.to_string(), 1, 0)],
            ),
            "case {case:?}: expected the stored association read back, got {body}"
        );
    }

    // The control: an association naming a provider the account holds NONE of is the one
    // thing this refusal is for, and it creates no cluster.
    let case = "a cluster create naming a provider nothing holds";
    let (code, message) = refusal(
        &router,
        "CreateCluster",
        json!({
            "clusterName": "bootghost",
            "capacityProviders": ["ghostprovider"],
        }),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("InvalidParameterException", PROVIDER_NOT_FOUND),
        "case {case:?}: expected the unknown-provider refusal, got {code:?} with {message:?}"
    );
    let body = ok(
        &router,
        "DescribeClusters",
        json!({ "clusters": ["bootghost"] }),
        case,
    )
    .await;
    assert_eq!(
        (
            list(&body, "clusters", case),
            list(&body, "failures", case),
        ),
        (
            Vec::new(),
            vec![json!({ "arn": cluster_arn("bootghost"), "reason": MISSING })],
        ),
        "case {case:?}: a refused create leaves no cluster behind, got {body}"
    );
}

#[tokio::test]
async fn a_placement_on_the_provider_is_refused_for_its_capacity_and_served_once_it_has_some() {
    let router = service();
    let family = "bootapp";
    register_definition(&router, family, "setup").await;

    for row in bootstrap_rows() {
        let case = row.case;
        create_group_provider(&router, &row).await;
        create_associated_cluster(&router, &row).await;

        let request = json!({
            "cluster": row.cluster,
            "taskDefinition": family,
            "capacityProviderStrategy": row_strategy(&row),
        });

        // The cluster EXISTS and the strategy resolves, so the one thing missing is the
        // capacity behind the provider — and that is what the refusal states, naming the
        // provider the placement went through.
        let (code, message) = refusal(&router, "RunTask", request.clone(), case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (
                "InvalidParameterException",
                no_container_instances(row.provider).as_str(),
            ),
            "case {case:?}: expected the empty-provider refusal, got {code:?} with {message:?}"
        );
        // A refused run places nothing.
        let body = ok(
            &router,
            "ListTasks",
            json!({ "cluster": row.cluster }),
            case,
        )
        .await;
        assert_eq!(
            list(&body, "taskArns", case),
            Vec::<Value>::new(),
            "case {case:?}: a refused RunTask runs no task, got {body}"
        );

        // `ClusterNotFoundException` stays reserved for a cluster the account does not
        // hold, which is a refusal about another resource entirely.
        let absent = "nosuchbootcluster";
        let (code, message) = refusal(
            &router,
            "RunTask",
            json!({
                "cluster": absent,
                "taskDefinition": family,
                "capacityProviderStrategy": row_strategy(&row),
            }),
            case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("ClusterNotFoundException", CLUSTER_NOT_FOUND),
            "case {case:?}: expected the absent-cluster refusal, got {code:?} with {message:?}"
        );

        // Register capacity into the cluster and the SAME request is served: the refusal
        // above was about the capacity, not about an identifier the emulator could not
        // resolve.
        register_capacity(&router, row.cluster, case).await;
        let body = ok(&router, "RunTask", request, case).await;
        assert_eq!(
            body["failures"],
            json!([]),
            "case {case:?}: a served run fails nothing, got {body}"
        );
        let tasks = list(&body, "tasks", case);
        assert_eq!(
            tasks.len(),
            1,
            "case {case:?}: expected the task to be placed, got {body}"
        );
        assert_eq!(
            string(&tasks[0], "capacityProviderName", case),
            row.provider,
            "case {case:?}: the task names the provider that placed it, got {body}"
        );
    }
}

#[tokio::test]
async fn an_update_onto_the_providers_strategy_answers_the_updated_service() {
    let router = service();
    let family = "bootsvcapp";
    register_definition(&router, family, "setup").await;

    for row in bootstrap_rows() {
        let case = row.case;
        create_group_provider(&router, &row).await;
        create_associated_cluster(&router, &row).await;
        let service_name = format!("{}svc", row.provider);

        // The configuration the repair takes the service OFF: a launch type, which is the
        // other placement `CreateService` offers.
        ok(
            &router,
            "CreateService",
            json!({
                "cluster": row.cluster,
                "serviceName": service_name,
                "taskDefinition": family,
                "desiredCount": 0,
                "launchType": "EC2",
            }),
            case,
        )
        .await;

        let strategy = row_strategy(&row);
        let expected = vec![(row.provider.to_string(), 1, 0)];
        // The repair, and the identical re-issue of it: the tasks a service already runs
        // were placed under the configuration it is being taken off, so the switch carries
        // the new deployment that replaces them.
        let request = json!({
            "cluster": row.cluster,
            "service": service_name,
            "capacityProviderStrategy": strategy,
            "forceNewDeployment": true,
        });
        for attempt in ["the repair", "the identical re-issue"] {
            let case = &format!("{case}, {attempt}");
            let body = ok(&router, "UpdateService", request.clone(), case).await;
            let updated = &body["service"];
            assert_eq!(
                strategy_entries(updated, "capacityProviderStrategy", case),
                expected,
                "case {case:?}: expected the strategy now in force, got {updated}"
            );
            assert_eq!(
                (
                    updated["serviceName"].as_str(),
                    updated["clusterArn"].as_str(),
                    updated["createdBy"].as_str(),
                    updated["deploymentController"]["type"].as_str(),
                ),
                (
                    Some(service_name.as_str()),
                    Some(cluster_arn(row.cluster).as_str()),
                    Some(caller_principal().as_str()),
                    Some("ECS"),
                ),
                "case {case:?}: unexpected service element, got {updated}"
            );
            assert!(
                !updated["createdAt"].is_null(),
                "case {case:?}: expected the service's creation instant, got {updated}"
            );
            let configuration = &updated["deploymentConfiguration"];
            assert_eq!(
                (
                    configuration["strategy"].as_str(),
                    count(configuration, "maximumPercent", case),
                    count(configuration, "minimumHealthyPercent", case),
                ),
                (Some("ROLLING"), 200, 100),
                "case {case:?}: expected the rolling update the service runs under, got \
                 {updated}"
            );
            // The rollout the repair started: the current deployment, placing its tasks
            // under the strategy now in force.
            let deployments = list(updated, "deployments", case);
            let deployment = deployments
                .first()
                .unwrap_or_else(|| panic!("case {case:?}: expected a deployment, got {updated}"));
            assert_eq!(
                (
                    deployment["status"].as_str(),
                    deployment["taskDefinition"].as_str(),
                ),
                (Some("PRIMARY"), updated["taskDefinition"].as_str()),
                "case {case:?}: expected the current deployment of the service's revision, \
                 got {deployment}"
            );
            assert_eq!(
                strategy_entries(deployment, "capacityProviderStrategy", case),
                expected,
                "case {case:?}: expected the rollout's own strategy, got {deployment}"
            );
            let rollout = string(deployment, "rolloutState", case);
            assert!(
                ROLLOUT_STATES.contains(&rollout.as_str()),
                "case {case:?}: {rollout:?} is no DeploymentRolloutState, got {deployment}"
            );
            assert!(
                !deployment["stabilityStatus"].is_null(),
                "case {case:?}: expected the rollout's stability status, got {deployment}"
            );
            // "The list of the service revisions": the one this rollout is on, and the one
            // each deployment it is still replacing rolled out — every entry named by its
            // own ARN.
            let revisions = list(updated, "currentServiceRevisions", case);
            assert!(
                !revisions.is_empty(),
                "case {case:?}: expected the revision this rollout is on, got {updated}"
            );
            for revision in &revisions {
                assert!(
                    !revision["arn"].is_null(),
                    "case {case:?}: expected the revision's ARN, got {revision}"
                );
            }
        }

        // The strategy is in force on the stored service, not only in the update's answer.
        let body = ok(
            &router,
            "DescribeServices",
            json!({ "cluster": row.cluster, "services": [service_name] }),
            case,
        )
        .await;
        let described = list(&body, "services", case);
        assert_eq!(
            described.len(),
            1,
            "case {case:?}: expected the service back, got {body}"
        );
        assert_eq!(
            strategy_entries(&described[0], "capacityProviderStrategy", case),
            expected,
            "case {case:?}: expected the repaired strategy read back, got {body}"
        );
    }
}
