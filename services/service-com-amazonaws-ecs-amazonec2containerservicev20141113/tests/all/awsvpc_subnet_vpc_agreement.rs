// SPDX-License-Identifier: CC0-1.0

//! The ONE network the subnets of an `awsvpcConfiguration` have to share, driven end to end
//! over the shared wire harness in [`crate::ecs_wire`].
//!
//! The rule under test: an `awsvpc` placement configures ONE elastic network interface,
//! created in the subnets the request names, so a `subnets` list whose entries resolve to
//! more than one VPC describes an interface that could not be created — the placement is
//! refused `InvalidParameterException` (HTTP 400) stating what that member requires, and
//! nothing of it is stored.
//!
//! Rule source: `smithy_query(query="doc", shape="AwsVpcConfiguration", member="subnets")` —
//! "All specified subnets must be from the same VPC" — and the same query for
//! `securityGroups` — "All specified security groups must be from the same VPC", which is the
//! OTHER half of the one-network requirement and the pairing Amazon EC2 itself rejects, so
//! the two faults carry different sentences.
//!
//! The rule belongs to the CONFIGURATION rather than to one operation, so it is asserted on a
//! table that spans the inputs it generalises over — how many subnets the list names, how
//! many networks they come from, the POSITION of the subnet of another network, and whether
//! any security group is named at all — and then on every operation that accepts such a
//! configuration: `RunTask`, `StartTask`, `CreateService`, `UpdateService` and
//! `CreateTaskSet`. Every refusal also has to leave the store as it was, which each case
//! reads back through the operation that reports it.
//!
//! Every VPC, subnet and security group below is minted by AMAZON EC2's own API over the
//! shared engine, because which VPC a subnet belongs to is that service's record.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    answered, create_cluster, ec2_element, ec2_form, emulator_with_ec2, list, ok, refusal,
    register_instance, string, ACCOUNT, SIGNED_REGION,
};
use crate::task_lifecycle::container;
use crate::task_sets::{create_external_service, describe_task_sets};

/// The cluster every placement below is made in.
const CLUSTER: &str = "subnetvpcscope";

/// The `awsvpc` family every placement below runs, registered for both launch types so one
/// revision serves the Fargate placements and the one directed at a container instance.
const FAMILY: &str = "subnetvpcfamily";

/// The one error code every refusal below carries.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The sentence a placement whose `subnets` span two VPCs earns: Amazon ECS resolves that
/// list itself, so it states the rule the member carries.
const SUBNETS_OF_ONE_VPC: &str = "All input subnets must be from the same VPC.";

/// The sentence a placement that pairs a security group with subnets of another network
/// earns: the pairing is Amazon EC2's to reject, so Amazon ECS reports that service's answer.
const DIFFERENT_NETWORKS: &str = "You have specified two resources that belong to different \
                                  networks.";

/// The image the revision's container starts from; irrelevant to the rule, so one value.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// One network of the account as Amazon EC2 holds it: a VPC, `subnets` subnets inside it and
/// one security group of it. `block` is the second octet of the `/16`, so two calls of this
/// build two networks that share no address.
async fn network(
    router: &MultiServiceRouter,
    label: &str,
    block: u8,
    subnets: usize,
    case: &str,
) -> (Vec<String>, String) {
    let vpc = ec2_element(
        &ec2_form(
            router,
            "CreateVpc",
            &[("CidrBlock", &format!("10.{block}.0.0/16"))],
            case,
        )
        .await,
        "vpcId",
        case,
    );
    let mut held = Vec::new();
    for index in 0..subnets {
        held.push(ec2_element(
            &ec2_form(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", &vpc),
                    ("CidrBlock", &format!("10.{block}.{index}.0/24")),
                    ("AvailabilityZone", &format!("{SIGNED_REGION}a")),
                ],
                case,
            )
            .await,
            "subnetId",
            case,
        ));
    }
    let group = ec2_element(
        &ec2_form(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", &format!("{label}-group")),
                ("GroupDescription", "a group of this network"),
                ("VpcId", &vpc),
            ],
            case,
        )
        .await,
        "groupId",
        case,
    );
    (held, group)
}

/// The three networks every case below names resources out of: the first holds two subnets,
/// so a row can name two subnets of ONE VPC.
struct Networks {
    /// Two subnets of the first VPC.
    here: Vec<String>,
    /// A security group of the first VPC.
    here_group: String,
    /// One subnet of the second VPC.
    there: String,
    /// A security group of the second VPC.
    there_group: String,
    /// One subnet of a third VPC, so a row can span more than two networks.
    elsewhere: String,
}

/// A router carrying Amazon ECS beside Amazon EC2 on one engine, a cluster with one
/// registered container instance, an `awsvpc` revision to place, and three networks.
async fn fixture() -> (MultiServiceRouter, Networks, String) {
    let case = "fixture";
    let router = emulator_with_ec2();
    create_cluster(&router, CLUSTER, case).await;
    let host = register_instance(&router, CLUSTER, "i-0a0a0a0a0a0a0a0a0", case).await;
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "requiresCompatibilities": ["EC2", "FARGATE"],
            "containerDefinitions": [container("web", IMAGE)],
        }),
        case,
    )
    .await;
    let (here, here_group) = network(&router, "here", 80, 2, case).await;
    let (there, there_group) = network(&router, "there", 81, 1, case).await;
    let (elsewhere, _) = network(&router, "elsewhere", 82, 1, case).await;
    let networks = Networks {
        here,
        here_group,
        there: there.into_iter().next().expect("the second VPC has a subnet"),
        there_group,
        elsewhere: elsewhere
            .into_iter()
            .next()
            .expect("the third VPC has a subnet"),
    };
    (router, networks, host)
}

/// One row: the subnets and groups a placement names, and the sentence it earns — `None`
/// when one VPC holds all of them, so the placement is made.
struct Row {
    case: &'static str,
    subnets: Vec<String>,
    groups: Vec<String>,
    refusal: Option<&'static str>,
}

/// The rows differ along how many subnets the list names, how many VPCs they come from, the
/// POSITION of the subnet of another network, and whether a security group is named at all —
/// with the one-network rows as the controls that show the refusal is the second network
/// rather than the shape of the request.
fn rows(networks: &Networks) -> Vec<Row> {
    let here = networks.here[0].clone();
    let sibling = networks.here[1].clone();
    vec![
        Row {
            case: "one subnet beside a group of its own VPC",
            subnets: vec![here.clone()],
            groups: vec![networks.here_group.clone()],
            refusal: None,
        },
        Row {
            case: "two subnets of one VPC",
            subnets: vec![here.clone(), sibling.clone()],
            groups: vec![networks.here_group.clone()],
            refusal: None,
        },
        Row {
            case: "a subnet of another VPC after one of this VPC",
            subnets: vec![here.clone(), networks.there.clone()],
            groups: vec![networks.here_group.clone()],
            refusal: Some(SUBNETS_OF_ONE_VPC),
        },
        Row {
            case: "a subnet of another VPC before one of this VPC",
            subnets: vec![networks.there.clone(), here.clone()],
            groups: vec![networks.there_group.clone()],
            refusal: Some(SUBNETS_OF_ONE_VPC),
        },
        Row {
            case: "two subnets of one VPC and one of another",
            subnets: vec![here.clone(), sibling.clone(), networks.elsewhere.clone()],
            groups: vec![networks.here_group.clone()],
            refusal: Some(SUBNETS_OF_ONE_VPC),
        },
        Row {
            case: "subnets of three VPCs",
            subnets: vec![
                here.clone(),
                networks.there.clone(),
                networks.elsewhere.clone(),
            ],
            groups: vec![networks.here_group.clone()],
            refusal: Some(SUBNETS_OF_ONE_VPC),
        },
        Row {
            case: "subnets of two VPCs and no group at all",
            subnets: vec![here.clone(), networks.there.clone()],
            groups: Vec::new(),
            refusal: Some(SUBNETS_OF_ONE_VPC),
        },
        Row {
            case: "subnets of one VPC beside a group of another",
            subnets: vec![here, sibling],
            groups: vec![networks.there_group.clone()],
            refusal: Some(DIFFERENT_NETWORKS),
        },
    ]
}

/// The `networkConfiguration` a row sends.
fn configuration(subnets: &[String], groups: &[String]) -> Value {
    json!({
        "awsvpcConfiguration": { "subnets": subnets, "securityGroups": groups },
    })
}

/// The `awsvpcConfiguration` one service reports, read back through `DescribeServices`, and
/// `None` for a name the cluster holds no service under — which the same call reports as a
/// failure with the reason it carries for a described resource it could not find.
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
    match list(&body, "services", case).first().cloned() {
        Some(stored) => Some(stored["networkConfiguration"]["awsvpcConfiguration"].clone()),
        None => {
            assert_eq!(
                list(&body, "failures", case),
                vec![json!({
                    "arn": format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:service/{CLUSTER}/{name}"),
                    "reason": "MISSING",
                })],
                "case {case:?}: expected the unheld name to be reported MISSING, got {body}"
            );
            None
        }
    }
}

/// Assert that a response is the refusal `expected` under the one error code these refusals
/// carry.
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
async fn a_placement_is_refused_unless_every_subnet_it_names_is_of_one_vpc() {
    let (router, networks, _host) = fixture().await;

    for (index, row) in rows(&networks).iter().enumerate() {
        let case = row.case;
        let name = format!("subnetvpcsvc{index}");
        let network_configuration = configuration(&row.subnets, &row.groups);

        // The same configuration goes to a RUN and to a CREATE: both place a task of the
        // revision, so both resolve the networks the configuration names.
        let (run_status, run_body) = answered(
            &router,
            "RunTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": FAMILY,
                "launchType": "FARGATE",
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
                "launchType": "FARGATE",
                "networkConfiguration": network_configuration.clone(),
            }),
        )
        .await;

        match row.refusal {
            Some(expected) => {
                assert_refused("RunTask", case, run_status, &run_body, expected);
                assert_refused("CreateService", case, create_status, &create_body, expected);
                // Nothing of a refused placement is stored: no task was placed, and the name
                // the create asked for holds no service.
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
                // A configuration of ONE network is stored, so the create reads back through
                // `DescribeServices` carrying the lists it was given.
                let read = stored_configuration(&router, &name, case)
                    .await
                    .unwrap_or_else(|| panic!("case {case:?}: expected the service stored"));
                assert_eq!(
                    read["subnets"],
                    json!(row.subnets),
                    "case {case:?}: expected the subnets to round-trip, got {read}"
                );
                assert_eq!(
                    read["securityGroups"],
                    json!(row.groups),
                    "case {case:?}: expected the security groups to round-trip, got {read}"
                );
            }
        }
    }

    // The create of a name a refused attempt asked for is served once its subnets are of one
    // network: the refusal stored nothing.
    let case = "the corrected create of a refused name";
    let corrected = ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": "subnetvpcsvc2",
            "taskDefinition": FAMILY,
            "desiredCount": 0,
            "launchType": "FARGATE",
            "networkConfiguration": configuration(
                &networks.here[..1],
                std::slice::from_ref(&networks.here_group),
            ),
        }),
        case,
    )
    .await;
    assert_eq!(
        string(&corrected["service"], "serviceName", case),
        "subnetvpcsvc2",
        "case {case:?}: expected the corrected create to store the service, got {corrected}"
    );
}

#[tokio::test]
async fn every_operation_that_takes_a_network_configuration_is_held_to_the_one_network_rule() {
    let (router, networks, host) = fixture().await;
    let case = "subnets of two VPCs";

    // The configuration every operation below is refused for, and the one-network
    // configuration a stored service already carries, which a refused update may not change.
    let spanning = configuration(
        &[networks.here[0].clone(), networks.there.clone()],
        std::slice::from_ref(&networks.here_group),
    );
    let one_network = configuration(
        &networks.here[..1],
        std::slice::from_ref(&networks.here_group),
    );

    // The service an update is refused on, and the EXTERNAL service a task set is created in
    // ("This is used when a service uses the EXTERNAL deployment controller type").
    ok(
        &router,
        "CreateService",
        json!({
            "cluster": CLUSTER,
            "serviceName": "subnetvpcupdate",
            "taskDefinition": FAMILY,
            "desiredCount": 0,
            "launchType": "FARGATE",
            "networkConfiguration": one_network.clone(),
        }),
        "fixture",
    )
    .await;
    create_external_service(&router, CLUSTER, "subnetvpcexternal", 0, "fixture").await;

    let requests: [(&str, Value); 5] = [
        (
            "RunTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": FAMILY,
                "launchType": "FARGATE",
                "networkConfiguration": spanning.clone(),
            }),
        ),
        (
            "StartTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": FAMILY,
                "containerInstances": [host],
                "networkConfiguration": spanning.clone(),
            }),
        ),
        (
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": "subnetvpcrefused",
                "taskDefinition": FAMILY,
                "desiredCount": 0,
                "launchType": "FARGATE",
                "networkConfiguration": spanning.clone(),
            }),
        ),
        (
            "UpdateService",
            json!({
                "cluster": CLUSTER,
                "service": "subnetvpcupdate",
                "networkConfiguration": spanning.clone(),
            }),
        ),
        (
            "CreateTaskSet",
            json!({
                "cluster": CLUSTER,
                "service": "subnetvpcexternal",
                "taskDefinition": FAMILY,
                "launchType": "FARGATE",
                "networkConfiguration": spanning.clone(),
            }),
        ),
    ];

    for (operation, request) in requests {
        let (code, message) = refusal(&router, operation, request, case).await;
        assert_eq!(
            (code.as_str(), message.as_str()),
            (INVALID_PARAMETER, SUBNETS_OF_ONE_VPC),
            "case {case:?}: expected {operation} to answer {SUBNETS_OF_ONE_VPC:?}, got {code:?} \
             with {message:?}"
        );
    }

    // Each refusal left the store as it was: no service under the refused name, the updated
    // service still carrying the configuration it was created with, no task set in the
    // EXTERNAL service and no task in the cluster.
    assert!(
        stored_configuration(&router, "subnetvpcrefused", case)
            .await
            .is_none(),
        "case {case:?}: a refused create stores no service"
    );
    let unchanged = stored_configuration(&router, "subnetvpcupdate", case)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected the updated service to be stored"));
    // The two lists the rule is about, as the service still reports them; the configuration
    // also carries the modelled default of `assignPublicIp`, which no row here varies.
    assert_eq!(
        (&unchanged["subnets"], &unchanged["securityGroups"]),
        (
            &one_network["awsvpcConfiguration"]["subnets"],
            &one_network["awsvpcConfiguration"]["securityGroups"],
        ),
        "case {case:?}: a refused update leaves the configuration as it was, got {unchanged}"
    );
    let described = describe_task_sets(
        &router,
        CLUSTER,
        "subnetvpcexternal",
        None,
        false,
        "the task sets the service holds",
    )
    .await;
    assert!(
        list(&described, "taskSets", case).is_empty(),
        "case {case:?}: a refused create stores no task set, got {described}"
    );
    let tasks = ok(&router, "ListTasks", json!({ "cluster": CLUSTER }), case).await;
    assert!(
        list(&tasks, "taskArns", case).is_empty(),
        "case {case:?}: a refused placement runs no task, got {tasks}"
    );
}
