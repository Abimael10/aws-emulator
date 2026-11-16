// SPDX-License-Identifier: CC0-1.0

//! The BOUNDS the two lists of an `awsvpcConfiguration` carry, driven end to end over the
//! shared wire harness in [`crate::ecs_wire`].
//!
//! The rule under test: each list of the network configuration a placement supplies is
//! held to the limit its own member documents — a placement naming more entries than that
//! configures an elastic network interface that could not be created — so the launch is
//! refused `InvalidParameterException` and nothing of it is stored.
//!
//! Limit source: `smithy_query(query="doc", shape="AwsVpcConfiguration", member="subnets")`
//! — "There's a limit of 16 subnets that can be specified" — and the same query for
//! `securityGroups` — "There's a limit of 5 security groups that can be specified."
//!
//! The rule belongs to the CONFIGURATION, not to one operation, so it is asserted on a
//! table that spans both lists (a size inside the bound, the published bound itself, and a
//! size past it) and then on every operation that accepts such a configuration: `RunTask`,
//! `StartTask`, `CreateService`, `UpdateService` and `CreateTaskSet`. Every refusal also has
//! to leave the store as it was, which each case reads back through the operation that
//! reports it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    answered, cluster_with_capacity, list, ok, refusal, register_instance, service, string,
};
use crate::task_lifecycle::container;

/// The cluster every placement below is made in.
const CLUSTER: &str = "awsvpcbounds";

/// The `awsvpc` family every placement below runs.
const FAMILY: &str = "awsvpcboundsfamily";

/// The one error code every refusal below carries.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The image the revision's container starts from; irrelevant to the rule, so one value.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// `AwsVpcConfiguration$subnets`: "There's a limit of 16 subnets that can be specified."
const MAX_SUBNETS: usize = 16;

/// `AwsVpcConfiguration$securityGroups`: "There's a limit of 5 security groups that can be
/// specified."
const MAX_SECURITY_GROUPS: usize = 5;

/// The sentence a subnet list past its bound earns.
const SUBNETS_REFUSAL: &str = "subnets can have at most 16 items.";

/// The sentence a security-group list past its bound earns.
const SECURITY_GROUPS_REFUSAL: &str = "security groups can have at most 5 items.";

/// Subnet ids of the account, as many as the case needs: the harness holds this family of
/// ids in the Amazon VPC records, so a placement naming them resolves every one and can
/// only be refused for the LENGTH of the list.
fn subnet_ids(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("subnet-{index:017x}")).collect()
}

/// EC2-shaped security-group ids, as many as the case needs.
fn security_group_ids(count: usize) -> Vec<String> {
    (0..count).map(|index| format!("sg-{index:017x}")).collect()
}

/// The `networkConfiguration` a row sends: `subnets` always (the member is required) and
/// `securityGroups` only when the row names the member at all.
fn configuration(subnets: usize, security_groups: Option<usize>) -> Value {
    let mut awsvpc = json!({ "subnets": subnet_ids(subnets) });
    if let Some(groups) = security_groups {
        awsvpc["securityGroups"] = json!(security_group_ids(groups));
    }
    json!({ "awsvpcConfiguration": awsvpc })
}

/// One row of the table: how long each list of the configuration is, and the sentence the
/// placement earns — `None` for a configuration inside both bounds.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    subnets: usize,
    security_groups: Option<usize>,
    refusal: Option<&'static str>,
}

/// The rows differ along WHICH list is over its bound, how far over it is, whether the
/// optional member is named at all, and the size inside the bound — including each list's
/// published maximum and one entry past it.
const ROWS: [Row; 9] = [
    Row {
        case: "one subnet and no security-group member",
        subnets: 1,
        security_groups: None,
        refusal: None,
    },
    Row {
        case: "one subnet and an explicitly empty security-group list",
        subnets: 1,
        security_groups: Some(0),
        refusal: None,
    },
    Row {
        case: "one security group",
        subnets: 1,
        security_groups: Some(1),
        refusal: None,
    },
    Row {
        case: "the documented maximum of five security groups",
        subnets: 1,
        security_groups: Some(MAX_SECURITY_GROUPS),
        refusal: None,
    },
    Row {
        case: "one security group past the documented maximum",
        subnets: 1,
        security_groups: Some(MAX_SECURITY_GROUPS + 1),
        refusal: Some(SECURITY_GROUPS_REFUSAL),
    },
    Row {
        case: "twice the documented maximum of security groups",
        subnets: 1,
        security_groups: Some(MAX_SECURITY_GROUPS * 2),
        refusal: Some(SECURITY_GROUPS_REFUSAL),
    },
    Row {
        case: "the documented maximum of sixteen subnets",
        subnets: MAX_SUBNETS,
        security_groups: Some(1),
        refusal: None,
    },
    Row {
        case: "one subnet past the documented maximum",
        subnets: MAX_SUBNETS + 1,
        security_groups: Some(1),
        refusal: Some(SUBNETS_REFUSAL),
    },
    Row {
        case: "both lists past their documented maximum",
        subnets: MAX_SUBNETS + 1,
        security_groups: Some(MAX_SECURITY_GROUPS + 1),
        refusal: Some(SUBNETS_REFUSAL),
    },
];

/// A cluster with registered capacity and one `awsvpc` revision to place.
async fn fixture() -> MultiServiceRouter {
    let router = service();
    cluster_with_capacity(&router, CLUSTER, "fixture").await;
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [container("web", IMAGE)],
        }),
        "fixture",
    )
    .await;
    router
}

/// The `awsvpcConfiguration` one service reports, read back through `DescribeServices`.
async fn stored_configuration(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
) -> Option<Value> {
    let body = ok(
        router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": [name] }),
        case,
    )
    .await;
    let stored = list(&body, "services", case).first().cloned()?;
    Some(stored["networkConfiguration"]["awsvpcConfiguration"].clone())
}

/// Assert that a response is the refusal `expected` under the one error code these
/// refusals carry.
fn assert_refused(operation: &str, case: &str, status: u16, body: &Value, expected: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected {operation} to be refused, got {status} with {body}"
    );
    assert_eq!(
        (
            body["__type"]
                .as_str()
                .unwrap_or_default()
                .rsplit('#')
                .next()
                .unwrap_or_default(),
            body["message"].as_str().unwrap_or_default()
        ),
        (INVALID_PARAMETER, expected),
        "case {case:?}: expected {operation} to answer {expected:?}, got {body}"
    );
}

#[tokio::test]
async fn a_placement_is_refused_a_configuration_list_longer_than_its_member_allows() {
    let router = fixture().await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let name = format!("boundsvc{index}");
        let network_configuration = configuration(row.subnets, row.security_groups);

        // The same configuration goes to a RUN and to a CREATE: both place a task of the
        // revision, so both are held to the lists the configuration carries.
        let (run_status, run_body) = answered(
            &router,
            "RunTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": FAMILY,
                "launchType": "EC2",
                "networkConfiguration": network_configuration.clone(),
            }),
        )
        .await;
        let (create_status, create_body) = answered(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": name.clone(),
                "taskDefinition": FAMILY,
                "desiredCount": 0,
                "launchType": "EC2",
                "networkConfiguration": network_configuration.clone(),
            }),
        )
        .await;

        match row.refusal {
            Some(expected) => {
                assert_refused("RunTask", case, run_status, &run_body, expected);
                assert_refused("CreateService", case, create_status, &create_body, expected);
                // Nothing of a refused placement is stored: no task was placed, and the
                // name the create asked for holds no service.
                assert!(
                    list(&run_body, "tasks", case).is_empty(),
                    "case {case:?}: a refused run places no task, got {run_body}"
                );
                assert!(
                    stored_configuration(&router, &name, case).await.is_none(),
                    "case {case:?}: a refused create stores no service"
                );
            }
            None => {
                assert_eq!(
                    run_status, 200,
                    "case {case:?}: expected the run to be answered, got {run_status} with \
                     {run_body}"
                );
                assert_eq!(
                    create_status, 200,
                    "case {case:?}: expected the create to be answered, got {create_status} \
                     with {create_body}"
                );
                // A configuration inside both bounds is STORED, so the create reads back
                // through `DescribeServices` carrying the lists it was given.
                assert_eq!(
                    string(&create_body["service"], "serviceName", case),
                    name,
                    "case {case:?}: expected the create to store the service, got \
                     {create_body}"
                );
                let read = stored_configuration(&router, &name, case)
                    .await
                    .unwrap_or_else(|| panic!("case {case:?}: expected the service stored"));
                assert_eq!(
                    read["subnets"],
                    json!(subnet_ids(row.subnets)),
                    "case {case:?}: expected the subnets to round-trip, got {read}"
                );
                assert_eq!(
                    read["securityGroups"],
                    json!(security_group_ids(row.security_groups.unwrap_or(0))),
                    "case {case:?}: expected the security groups to round-trip, got {read}"
                );
            }
        }
    }
}

#[tokio::test]
async fn every_operation_that_takes_a_network_configuration_is_held_to_the_same_bound() {
    let router = fixture().await;
    // A second container instance, so the placement a client DIRECTS at an instance can
    // name one.
    let host = register_instance(&router, CLUSTER, "i-0b0b0b0b0b0b0b0b0", "fixture").await;

    // The configuration every operation below is refused for: one list past the limit its
    // own member documents.
    let case = "a security-group list one entry past the documented maximum";
    let over_bound = configuration(1, Some(MAX_SECURITY_GROUPS + 1));
    // The configuration a stored service already carries, which a refused update may not
    // change.
    let inside_bound = configuration(1, Some(MAX_SECURITY_GROUPS));

    // The service an update is refused on, and the EXTERNAL service a task set is created
    // in ("This is used when a service uses the EXTERNAL deployment controller type").
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": "boundsupdate",
            "taskDefinition": FAMILY,
            "desiredCount": 0,
            "launchType": "EC2",
            "networkConfiguration": inside_bound.clone(),
        }),
        "fixture",
    )
    .await;
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": "boundsexternal",
            "desiredCount": 0,
            "deploymentController": { "type": "EXTERNAL" },
        }),
        "fixture",
    )
    .await;

    let requests: [(&str, Value); 5] = [
        (
            "RunTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": FAMILY,
                "launchType": "EC2",
                "networkConfiguration": over_bound.clone(),
            }),
        ),
        (
            "StartTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": FAMILY,
                "containerInstances": [host],
                "networkConfiguration": over_bound.clone(),
            }),
        ),
        (
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": "boundsrefused",
                "taskDefinition": FAMILY,
                "desiredCount": 0,
                "launchType": "EC2",
                "networkConfiguration": over_bound.clone(),
            }),
        ),
        (
            "UpdateService",
            json!({
                "cluster": CLUSTER,
                "service": "boundsupdate",
                "networkConfiguration": over_bound.clone(),
            }),
        ),
        (
            "CreateTaskSet",
            json!({
                "cluster": CLUSTER,
                "service": "boundsexternal",
                "taskDefinition": FAMILY,
                "launchType": "EC2",
                "networkConfiguration": over_bound.clone(),
            }),
        ),
    ];

    for (operation, request) in requests {
        let (code, message) = refusal(&router, operation, request, case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, SECURITY_GROUPS_REFUSAL),
            "case {case:?}: expected {operation} to answer {SECURITY_GROUPS_REFUSAL:?}, got \
             {code:?} with {message:?}"
        );
    }

    // Each refusal left the store as it was: no service under the refused name, the
    // updated service still carrying the configuration it was created with, and no task
    // set in the EXTERNAL service.
    assert!(
        stored_configuration(&router, "boundsrefused", case)
            .await
            .is_none(),
        "case {case:?}: a refused create stores no service"
    );
    let unchanged = stored_configuration(&router, "boundsupdate", case)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected the updated service to be stored"));
    assert_eq!(
        unchanged["securityGroups"],
        json!(security_group_ids(MAX_SECURITY_GROUPS)),
        "case {case:?}: a refused update changes no stored configuration, got {unchanged}"
    );
    let sets = ok(
        &router,
        "DescribeTaskSets",
        json!({ "cluster": CLUSTER, "service": "boundsexternal" }),
        case,
    )
    .await;
    assert!(
        list(&sets, "taskSets", case).is_empty(),
        "case {case:?}: a refused create stores no task set, got {sets}"
    );
    let listed = ok(&router, "ListTasks", json!({ "cluster": CLUSTER }), case).await;
    assert!(
        list(&listed, "taskArns", case).is_empty(),
        "case {case:?}: no refused placement left a task behind, got {listed}"
    );
}
