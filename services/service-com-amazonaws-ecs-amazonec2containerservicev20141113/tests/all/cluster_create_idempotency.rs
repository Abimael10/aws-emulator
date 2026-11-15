// SPDX-License-Identifier: CC0-1.0

//! Repeating a `CreateCluster` for a name the account already holds, driven end to end
//! over the shared wire harness in [`crate::ecs_wire`].
//!
//! The rule under test: a cluster is identified by its name, so a create for a name that
//! is taken is the same create being made again — it is answered with the stored cluster
//! when it asks for the cluster that is stored, and refused with
//! `InvalidParameterException` when any argument it supplies differs, with the stored
//! cluster left exactly as it stands. The comparison is over the arguments the repeat
//! CARRIES (`CreateCluster`'s input members beside `clusterName`:
//! `smithy_query(query="operation", operation="CreateCluster")` reports `tags`,
//! `settings`, `configuration`, `capacityProviders`, `defaultCapacityProviderStrategy`
//! and `serviceConnectDefaults`), and the keyed collections — `tags` by key, `settings`
//! by name — are compared by their keys rather than by position.
//!
//! The rows below span that member set, the kind of difference (a value changed, a member
//! the first create never carried, an entry added, an entry dropped) and the ORDER the
//! same entries are sent in; the name alone is the repeat that asserts nothing.
//!
//! A DELETED cluster is only a tombstone, so its name is free again: the last test shows
//! a create over it taking whatever arguments it likes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{answered, count, list, ok, service, string};

/// The fixed sentence a repeat that disagrees with the create before it carries
/// (`state`'s `CLUSTER_ARGUMENTS_INCONSISTENT_MESSAGE`), as the wire corpus records it.
const ARGUMENTS_INCONSISTENT_MESSAGE: &str =
    "Arguments on this idempotent request are inconsistent with arguments used in \
     previous request(s).";

/// The modelled client error such a repeat is refused under
/// (`smithy_query(query="operation", operation="CreateCluster")` lists
/// `InvalidParameterException`, httpError 400).
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// Every `DescribeClusters$include` field the model defines
/// (`smithy_query(query="shape", shape="ClusterField")` -> `ATTACHMENTS`,
/// `CONFIGURATIONS`, `SETTINGS`, `STATISTICS`, `TAGS`), so the read a repeat is weighed
/// against materialises everything a cluster can report.
const EVERY_CLUSTER_FIELD: [&str; 5] = [
    "ATTACHMENTS",
    "CONFIGURATIONS",
    "SETTINGS",
    "STATISTICS",
    "TAGS",
];

/// Two capacity providers Amazon ECS manages for every account, so a cluster can be
/// created over them without one being created first.
const MANAGED_PROVIDERS: [&str; 2] = ["FARGATE", "FARGATE_SPOT"];

/// One row: the arguments the FIRST create carries beside `clusterName`, the arguments
/// the REPEAT carries beside it, and whether the repeat is the same create.
struct RepeatCase {
    name: &'static str,
    create: fn() -> Value,
    repeat: fn() -> Value,
    same_create: bool,
}

/// The tag set used by the rows about `tags`.
fn two_tags() -> Value {
    json!([
        { "key": "env", "value": "test" },
        { "key": "owner", "value": "ecs" },
    ])
}

/// Merge the row's arguments onto the cluster name, so each row spells only what it is
/// about.
fn request(name: &str, arguments: Value) -> Value {
    let mut body = json!({ "clusterName": name });
    if let Some(members) = arguments.as_object() {
        for (member, value) in members {
            body[member] = value.clone();
        }
    }
    body
}

/// Read one cluster back with every `include` field, so what a repeat did or did not
/// write is visible.
async fn describe(router: &MultiServiceRouter, cluster: &str, case: &str) -> Value {
    let body = ok(
        router,
        "DescribeClusters",
        json!({ "clusters": [cluster], "include": EVERY_CLUSTER_FIELD }),
        case,
    )
    .await;
    assert!(
        list(&body, "failures", case).is_empty(),
        "case {case:?}: expected no failures describing {cluster:?}, got {body}"
    );
    let clusters = list(&body, "clusters", case);
    assert_eq!(
        clusters.len(),
        1,
        "case {case:?}: expected exactly one cluster back, got {body}"
    );
    clusters[0].clone()
}

/// The configured state of a cluster — the members `CreateCluster` sets — reduced to a
/// comparable form, with the keyed and set-valued collections ordered so the comparison
/// does not depend on the order anything was written in.
fn configured_state(cluster: &Value, case: &str) -> Value {
    let mut tags: Vec<(String, String)> = list(cluster, "tags", case)
        .iter()
        .map(|tag| (text(tag, "key"), text(tag, "value")))
        .collect();
    tags.sort();
    let mut settings: Vec<(String, String)> = list(cluster, "settings", case)
        .iter()
        .map(|setting| (text(setting, "name"), text(setting, "value")))
        .collect();
    settings.sort();
    let mut providers: Vec<String> = list(cluster, "capacityProviders", case)
        .iter()
        .map(|provider| provider.as_str().unwrap_or_default().to_string())
        .collect();
    providers.sort();
    let mut strategy: Vec<(String, i64, i64)> = list(cluster, "defaultCapacityProviderStrategy", case)
        .iter()
        .map(|item| {
            (
                text(item, "capacityProvider"),
                count(item, "weight", case),
                count(item, "base", case),
            )
        })
        .collect();
    strategy.sort();
    json!({
        "tags": tags,
        "settings": settings,
        "capacityProviders": providers,
        "defaultCapacityProviderStrategy": strategy,
        "configuration": cluster["configuration"].clone(),
        "serviceConnectDefaults": cluster["serviceConnectDefaults"].clone(),
    })
}

/// A string member of a reported object, absent reading as empty.
fn text(object: &Value, member: &str) -> String {
    object[member].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn a_repeated_create_cluster_is_answered_only_when_it_asks_for_the_stored_cluster() {
    let router = service();

    let cases = [
        RepeatCase {
            name: "the same arguments, member for member",
            create: || json!({ "tags": two_tags(), "settings": [{ "name": "containerInsights", "value": "enabled" }] }),
            repeat: || json!({ "tags": two_tags(), "settings": [{ "name": "containerInsights", "value": "enabled" }] }),
            same_create: true,
        },
        RepeatCase {
            name: "the name alone, asserting nothing about the cluster",
            create: || json!({ "tags": two_tags(), "settings": [{ "name": "containerInsights", "value": "enhanced" }] }),
            repeat: || json!({}),
            same_create: true,
        },
        RepeatCase {
            name: "the same tags and settings in another order",
            create: || json!({
                "tags": two_tags(),
                "settings": [
                    { "name": "containerInsights", "value": "disabled" },
                ],
            }),
            repeat: || json!({
                "tags": [
                    { "key": "owner", "value": "ecs" },
                    { "key": "env", "value": "test" },
                ],
                "settings": [
                    { "name": "containerInsights", "value": "disabled" },
                ],
            }),
            same_create: true,
        },
        RepeatCase {
            name: "a setting value flipped",
            create: || json!({ "tags": two_tags(), "settings": [{ "name": "containerInsights", "value": "enabled" }] }),
            repeat: || json!({ "tags": two_tags(), "settings": [{ "name": "containerInsights", "value": "disabled" }] }),
            same_create: false,
        },
        RepeatCase {
            name: "a setting the first create never named",
            create: || json!({}),
            repeat: || json!({ "settings": [{ "name": "containerInsights", "value": "enhanced" }] }),
            same_create: false,
        },
        RepeatCase {
            name: "a tag value changed",
            create: || json!({ "tags": two_tags() }),
            repeat: || json!({ "tags": [
                { "key": "env", "value": "prod" },
                { "key": "owner", "value": "ecs" },
            ] }),
            same_create: false,
        },
        RepeatCase {
            name: "one tag more than the create carried",
            create: || json!({ "tags": two_tags() }),
            repeat: || json!({ "tags": [
                { "key": "env", "value": "test" },
                { "key": "owner", "value": "ecs" },
                { "key": "team", "value": "platform" },
            ] }),
            same_create: false,
        },
        RepeatCase {
            name: "one tag fewer than the create carried",
            create: || json!({ "tags": two_tags() }),
            repeat: || json!({ "tags": [{ "key": "env", "value": "test" }] }),
            same_create: false,
        },
        RepeatCase {
            name: "the same capacity providers in another order",
            create: || json!({ "capacityProviders": MANAGED_PROVIDERS }),
            repeat: || json!({ "capacityProviders": ["FARGATE_SPOT", "FARGATE"] }),
            same_create: true,
        },
        RepeatCase {
            name: "one capacity provider more than the create associated",
            create: || json!({ "capacityProviders": ["FARGATE"] }),
            repeat: || json!({ "capacityProviders": MANAGED_PROVIDERS }),
            same_create: false,
        },
        RepeatCase {
            name: "the same default strategy with another weight",
            create: || json!({
                "capacityProviders": ["FARGATE"],
                "defaultCapacityProviderStrategy": [
                    { "capacityProvider": "FARGATE", "weight": 1, "base": 0 },
                ],
            }),
            repeat: || json!({
                "capacityProviders": ["FARGATE"],
                "defaultCapacityProviderStrategy": [
                    { "capacityProvider": "FARGATE", "weight": 2, "base": 0 },
                ],
            }),
            same_create: false,
        },
        RepeatCase {
            name: "the same configuration repeated",
            create: || json!({
                "configuration": { "executeCommandConfiguration": { "logging": "DEFAULT" } },
            }),
            repeat: || json!({
                "configuration": { "executeCommandConfiguration": { "logging": "DEFAULT" } },
            }),
            same_create: true,
        },
        RepeatCase {
            name: "another execute-command logging option",
            create: || json!({
                "configuration": { "executeCommandConfiguration": { "logging": "DEFAULT" } },
            }),
            repeat: || json!({
                "configuration": { "executeCommandConfiguration": { "logging": "NONE" } },
            }),
            same_create: false,
        },
        RepeatCase {
            name: "the same Service Connect namespace repeated",
            create: || json!({ "serviceConnectDefaults": { "namespace": "recreate-alpha" } }),
            repeat: || json!({ "serviceConnectDefaults": { "namespace": "recreate-alpha" } }),
            same_create: true,
        },
        RepeatCase {
            name: "another Service Connect namespace",
            create: || json!({ "serviceConnectDefaults": { "namespace": "recreate-alpha" } }),
            repeat: || json!({ "serviceConnectDefaults": { "namespace": "recreate-beta" } }),
            same_create: false,
        },
    ];

    for (index, test) in cases.iter().enumerate() {
        let case = test.name;
        let cluster = format!("recreate-{index}");
        let created = ok(
            &router,
            "CreateCluster",
            request(&cluster, (test.create)()),
            case,
        )
        .await;
        let arn = string(&created["cluster"], "clusterArn", case);
        let before = configured_state(&describe(&router, &cluster, case).await, case);

        let (status, body) =
            answered(&router, "CreateCluster", request(&cluster, (test.repeat)())).await;
        if test.same_create {
            assert_eq!(
                status, 200,
                "case {case:?}: a repeat that asks for the stored cluster is the same \
                 create, so it must be answered, got {status} with {body}"
            );
            assert_eq!(
                string(&body["cluster"], "clusterArn", case),
                arn,
                "case {case:?}: the repeat must report the cluster already stored, got \
                 {body}"
            );
            assert_eq!(
                string(&body["cluster"], "status", case),
                "ACTIVE",
                "case {case:?}: the stored cluster is still ACTIVE, got {body}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: a repeat that supplies a different argument must be \
                 refused, got {status} with {body}"
            );
            assert!(
                body["__type"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(INVALID_PARAMETER),
                "case {case:?}: expected a {INVALID_PARAMETER}, got {body}"
            );
            let message = body["message"].as_str().unwrap_or_default();
            assert_eq!(
                message, ARGUMENTS_INCONSISTENT_MESSAGE,
                "case {case:?}: expected the fixed sentence \
                 {ARGUMENTS_INCONSISTENT_MESSAGE:?}, got {message:?}"
            );
            // The sentence is fixed text: nothing the request carried reaches it.
            for value in [cluster.as_str(), "containerInsights", "FARGATE", "env"] {
                assert!(
                    !message.contains(value),
                    "case {case:?}: the sentence must not carry {value:?}, got \
                     {message:?}"
                );
            }
        }

        // Answered or refused, the repeat wrote nothing: the cluster is configured as the
        // FIRST create left it.
        let after = configured_state(&describe(&router, &cluster, case).await, case);
        assert_eq!(
            after, before,
            "case {case:?}: the repeat must leave the stored cluster untouched"
        );
    }
}

#[tokio::test]
async fn a_create_over_a_deleted_cluster_takes_its_own_arguments() {
    let router = service();
    let case = "a create over a tombstone";
    let cluster = "recreate-tombstone";

    ok(
        &router,
        "CreateCluster",
        json!({
            "clusterName": cluster,
            "settings": [{ "name": "containerInsights", "value": "enabled" }],
            "tags": two_tags(),
        }),
        case,
    )
    .await;
    let deleted = ok(&router, "DeleteCluster", json!({ "cluster": cluster }), case).await;
    assert_eq!(
        string(&deleted["cluster"], "status", case),
        "INACTIVE",
        "case {case:?}: expected the delete to tombstone the cluster, got {deleted}"
    );

    // The name is free again, so the arguments of the FIRST create constrain nothing.
    let recreated = ok(
        &router,
        "CreateCluster",
        json!({
            "clusterName": cluster,
            "settings": [{ "name": "containerInsights", "value": "disabled" }],
        }),
        case,
    )
    .await;
    assert_eq!(
        string(&recreated["cluster"], "status", case),
        "ACTIVE",
        "case {case:?}: expected a new ACTIVE cluster over the tombstone, got {recreated}"
    );
    let stored = describe(&router, cluster, case).await;
    assert_eq!(
        configured_state(&stored, case)["settings"],
        json!([["containerInsights", "disabled"]]),
        "case {case:?}: expected the new cluster's own settings, got {stored}"
    );
    assert_eq!(
        list(&stored, "tags", case),
        Vec::<Value>::new(),
        "case {case:?}: the new cluster carries only the tags its own create named, got \
         {stored}"
    );
}
