// SPDX-License-Identifier: CC0-1.0

//! WHICH of a service's load-balancer faults Amazon ECS reports, when a request carries more
//! than one, and the sentence each of them carries — driven end to end over the shared wire
//! harness in [`crate::ecs_wire`] and the Elastic Load Balancing v2 harness in
//! [`crate::load_balancer_target_groups`].
//!
//! The rule under test: a `loadBalancers` entry is judged in two passes, and what the REQUEST
//! states is judged FIRST — before the stored target group it points at is read at all. The
//! first pass is about the entry's own members: it names ONE Elastic Load Balancing resource
//! (one of `targetGroupArn` / `loadBalancerName`, never both), it says which container of the
//! revision it attaches, and it says which port. The second pass is about the traffic the
//! attachment would carry: the target group has to exist, to carry the target type the
//! revision's network mode registers its tasks as, and the port has to be one that container
//! declares.
//!
//! So a request wrong in TWO ways is refused for the member it states wrongly, not for the
//! resource that member points at — which is what the recorded create of a service whose
//! binding named a container the revision does not declare, onto an incompatible target
//! group, answers.
//!
//! Each sentence is the whole message, and the rows below vary which member is at fault, how
//! it is at fault (blank, absent, both members filled), and what ELSE is wrong with the same
//! request — so the order is asserted rather than assumed.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{answered, ok, refusal, string};
use crate::load_balancer_target_groups::{
    create_target_group, create_unattached_target_group, emulator, register_definition,
};

/// The cluster every request below acts in.
const CLUSTER: &str = "lborder";

/// The `awsvpc` family every service below runs: its one container is `web`, publishing port
/// 80 ([`crate::load_balancer_target_groups::register_definition`]).
const FAMILY: &str = "lborderfamily";

/// The one container that family declares.
const CONTAINER: &str = "web";

/// The port it declares.
const PORT: i64 = 80;

/// The error this service models for a request whose parameters it will not serve.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The error this service models for a client-side fault that is not one parameter's value —
/// the class the member pair of an entry is refused under.
const CLIENT: &str = "ClientException";

/// A router with the ECS and Elastic Load Balancing v2 services over ONE engine, one cluster,
/// one `awsvpc` revision of [`FAMILY`], and two target groups: an `ip` group a load balancer
/// forwards to (the one an `awsvpc` service may bind) and an `instance` group that is
/// INCOMPATIBLE with that revision, so a row can be wrong in two ways at once.
async fn fixture() -> (MultiServiceRouter, String, String) {
    let router = emulator();
    ok(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        "fixture",
    )
    .await;
    register_definition(&router, FAMILY, "awsvpc", "fixture").await;
    let compatible = create_target_group(&router, "lborder-ip", "ip", "fixture").await;
    let incompatible = create_target_group(&router, "lborder-instance", "instance", "fixture").await;
    (router, compatible, incompatible)
}

/// The `CreateService` request for `name` carrying `entries` as its `loadBalancers`.
fn create(name: &str, entries: Value) -> Value {
    json!({
        "cluster": CLUSTER,
        "serviceName": name,
        "taskDefinition": FAMILY,
        "desiredCount": 0,
        "networkConfiguration": { "awsvpcConfiguration": { "subnets": ["subnet-0abc1234ecs0lb"] } },
        "loadBalancers": entries,
    })
}

#[tokio::test]
async fn a_binding_is_refused_for_the_member_it_states_before_the_group_it_points_at() {
    let (router, compatible, incompatible) = fixture().await;

    // Every row's entry points at the INCOMPATIBLE target group, so each one is wrong in two
    // ways: the member the row is about, and the group the entry points at. The sentence the
    // row expects is the member's, which is what proves the order.
    let rows: [(&str, &str, Value, &str, &str); 5] = [
        (
            "an entry filling both members of the pair",
            CLIENT,
            json!([{
                "targetGroupArn": incompatible.clone(),
                "loadBalancerName": "lborder-classic",
                "containerName": CONTAINER,
                "containerPort": PORT,
            }]),
            "loadBalancerName and targetGroupArn cannot both be specified. You must specify \
             either a loadBalancerName or a targetGroupArn in all loadBalancer entries",
            "both-members",
        ),
        (
            "an entry filling neither member of the pair",
            CLIENT,
            json!([{ "containerName": CONTAINER, "containerPort": PORT }]),
            "loadBalancerName and targetGroupArn cannot both be specified. You must specify \
             either a loadBalancerName or a targetGroupArn in all loadBalancer entries",
            "no-member",
        ),
        (
            "an entry that names no container at all",
            INVALID_PARAMETER,
            json!([{ "targetGroupArn": incompatible.clone(), "containerPort": PORT }]),
            "Container Name can not be blank.",
            "no-container",
        ),
        (
            "an entry whose container name is blank text",
            INVALID_PARAMETER,
            json!([{
                "targetGroupArn": incompatible.clone(),
                "containerName": "   ",
                "containerPort": PORT,
            }]),
            "Container Name can not be blank.",
            "blank-container",
        ),
        (
            "an entry that states no port",
            INVALID_PARAMETER,
            json!([{ "targetGroupArn": incompatible.clone(), "containerName": CONTAINER }]),
            "Container Port is missing",
            "no-port",
        ),
    ];

    for (case, expected_code, entries, expected, name) in rows {
        let (code, message) = refusal(&router, "CreateService", create(name, entries), case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (expected_code, expected),
            "case {case:?}: expected the member the entry states wrongly to be reported \
             before the group it points at, got {code:?} with {message:?}"
        );
    }

    // A container the revision does not declare is the member's fault too, and is reported
    // with the container the request named — ahead of the incompatible group, as above.
    let case = "an entry naming a container the revision does not declare";
    let (code, message) = refusal(
        &router,
        "CreateService",
        create(
            "unknown-container",
            json!([{
                "targetGroupArn": incompatible.clone(),
                "containerName": "sidecar",
                "containerPort": PORT,
            }]),
        ),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            INVALID_PARAMETER,
            "The container sidecar does not exist in the task definition."
        ),
        "case {case:?}: expected the container the entry named, got {code:?} with {message:?}"
    );

    // With every member stated, the GROUP is judged: its target type against the revision's
    // network mode, and only then the port the container declares.
    let case = "an entry whose members are all stated, onto the incompatible group";
    let (code, message) = refusal(
        &router,
        "CreateService",
        create(
            "incompatible-group",
            json!([{
                "targetGroupArn": incompatible.clone(),
                "containerName": CONTAINER,
                "containerPort": PORT,
            }]),
        ),
        case,
    )
    .await;
    assert_eq!(
        code, INVALID_PARAMETER,
        "case {case:?}: expected the pairing refusal, got {code:?} with {message:?}"
    );
    assert!(
        message.contains("has target type instance"),
        "case {case:?}: expected the group's own target type to be reported once every \
         member is stated, got {message:?}"
    );

    // And the port the container does not declare is reported once the group is one the
    // revision can use.
    let case = "an entry naming a port the container does not declare";
    let (code, message) = refusal(
        &router,
        "CreateService",
        create(
            "undeclared-port",
            json!([{
                "targetGroupArn": compatible.clone(),
                "containerName": CONTAINER,
                "containerPort": 8080,
            }]),
        ),
        case,
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (
            INVALID_PARAMETER,
            "The container web did not have a container port 8080 defined."
        ),
        "case {case:?}: expected the declared-port refusal, got {code:?} with {message:?}"
    );

    // The positive control: with every member stated and a group the revision can use, the
    // service is created and the binding reads back.
    let case = "the attachment that is wrong in no way";
    let created = ok(
        &router,
        "CreateService",
        create(
            "bound",
            json!([{
                "targetGroupArn": compatible.clone(),
                "containerName": CONTAINER,
                "containerPort": PORT,
            }]),
        ),
        case,
    )
    .await;
    assert_eq!(
        string(&created["service"], "serviceName", case),
        "bound",
        "case {case:?}: expected the service created, got {created}"
    );
    let described = ok(
        &router,
        "DescribeServices",
        json!({ "cluster": CLUSTER, "services": ["bound"] }),
        case,
    )
    .await;
    assert_eq!(
        described["services"][0]["loadBalancers"][0]["targetGroupArn"].as_str(),
        Some(compatible.as_str()),
        "case {case:?}: expected the stored binding back, got {described}"
    );
}

#[tokio::test]
async fn a_target_group_that_is_not_an_arn_is_reported_as_one_that_could_not_be_validated() {
    let (router, compatible, _) = fixture().await;

    // A target group the ELBv2 store does not hold, named by an ARN of the right shape, and
    // one named by values that are no ARN at all: the first names a group that does not
    // exist, the second names nothing Amazon ECS could read in the account.
    let absent = compatible.replace("lborder-ip", "lborder-gone");
    let orphan = create_unattached_target_group(&router, "lborder-orphan", "ip", "setup").await;

    let unvalidated = "Unable to assume role and validate the specified targetGroupArn. \
                       Please verify that the ECS service role being passed has the proper \
                       permissions.";
    let rows: [(&str, String, String); 4] = [
        (
            "a bare target-group name",
            "lborder-ip".to_string(),
            unvalidated.to_string(),
        ),
        (
            "the resource half of an ARN",
            "targetgroup/lborder-ip/0123456789abcdef".to_string(),
            unvalidated.to_string(),
        ),
        (
            "an ARN with a field missing",
            "arn:aws:elasticloadbalancing:us-west-2:000000000000".to_string(),
            unvalidated.to_string(),
        ),
        (
            "an ARN of the right shape naming no stored group",
            absent.clone(),
            format!("The target group {absent} does not exist."),
        ),
    ];

    for (index, (case, reference, expected)) in rows.iter().enumerate() {
        let (code, message) = refusal(
            &router,
            "CreateService",
            create(
                &format!("unvalidated{index}"),
                json!([{
                    "targetGroupArn": reference,
                    "containerName": CONTAINER,
                    "containerPort": PORT,
                }]),
            ),
            case,
        )
        .await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, expected.as_str()),
            "case {case:?}: unexpected refusal, got {code:?} with {message:?}"
        );
    }

    // A group that EXISTS but that no load balancer forwards to is neither of those: it is
    // the group's own association that is missing, which keeps its own sentence.
    let case = "an existing group no load balancer forwards to";
    let (status, body) = answered(
        &router,
        "CreateService",
        create(
            "orphaned",
            json!([{
                "targetGroupArn": orphan,
                "containerName": CONTAINER,
                "containerPort": PORT,
            }]),
        ),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the orphan refusal, got {status} with {body}"
    );
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("does not have an associated load balancer"),
        "case {case:?}: expected the association refusal to keep its own sentence, got {body}"
    );
}
