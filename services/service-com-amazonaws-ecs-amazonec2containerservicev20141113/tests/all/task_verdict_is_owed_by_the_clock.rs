// SPDX-License-Identifier: CC0-1.0

//! A task's verdict is owed by the CLOCK, not produced by the read: a scaled-up task reaches its
//! terminal state while NOTHING asks Amazon ECS about it.
//!
//! RULE: a task Amazon ECS accepts moves through the lifecycle on its own, so a task whose
//! placement cannot fetch the images its containers run reaches `Task$lastStatus` `STOPPED` with
//! the `TaskStopCode` member for a task whose containers were never started (`Task$stopCode`: "The
//! stop code indicating why a task was stopped. The stoppedReason might contain additional
//! details"), a `Task$stoppedReason` carrying the container runtime's `CannotPullContainerError`
//! for the reference it could not resolve, NO `Task$startedAt` ("the time when the task
//! transitioned from the `PENDING` state to the `RUNNING` state") and no `exitCode` on any
//! container — whether or not anybody polls Amazon ECS while it happens. An `awsvpc` task carries
//! exactly one `ElasticNetworkInterface` attachment whose `Attachment$details` name the subnet it
//! was placed in ("for elastic network interfaces, this includes the network interface ID, the MAC
//! address, the subnet ID, and the private IPv4 address"), and `Service$runningCount` ("the number
//! of tasks in the cluster that are in the `RUNNING` state") stays 0 for a service whose tasks
//! never ran.
//!
//! What makes the move the CLOCK's rather than the reading caller's is observable without asking
//! this service anything: the interface such a task is allocated is created in Amazon VPC when the
//! task is placed and torn down once that task has stopped, so Amazon EC2's own
//! `DescribeNetworkInterfaces` shows the whole lifecycle run its course. Each row below scales its
//! service up and then makes NO Amazon ECS reporting call at all until that Amazon EC2 read has
//! shown an interface created in its subnets and released again — and only then reads the verdict,
//! which is there to be read in full.
//!
//! The rows differ along what the rule generalises over: how many containers the revision declares
//! and which registry their references address (two containers, a non-essential FireLens log router
//! beside the essential service container, pulling from a repository of this account's own Amazon
//! ECR registry; one pulling from a public registry), and whether the launch asked for a public
//! address — which changes nothing for a subnet with no route off the VPC, since neither a public
//! registry nor the endpoints Amazon ECR publishes into a VPC that has none is reachable from one.
//! Each row is placed in subnets of its OWN, so the Amazon EC2 read sees the interfaces of its own
//! task and of no other. The dimension of WHICH Amazon ECS read a polling client waits on is tabled
//! beside the router, in `core/router/tests/all/ecs_scaled_up_task_verdict_is_owed_by_the_clock.rs`.
//!
//! Every fact the rule is read over is created through the OWNING service's own API — the VPC, the
//! subnets, their route tables and the security group through Amazon EC2's, the repository through
//! Amazon ECR's — over the shared harness of this binary ([`crate::ecs_wire::emulator_with_ec2`]).

use std::cell::RefCell;
use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    ec2_element, ec2_form, ecr, emulator_with_ec2, ok, poll_until, string, ACCOUNT, SIGNED_REGION,
};
// The ONE repeated-element reader of this binary (`all/load_balancer_target_groups.rs`: the suites
// of this binary share ONE reader instead of each keeping its own).
use crate::load_balancer_target_groups::all_texts;

/// The cluster every service below runs in.
const CLUSTER: &str = "clockverdict";

/// The two containers of the two-container revision: a FireLens log router, which is NOT
/// essential, and the service container, which is.
const LOG_ROUTER_CONTAINER: &str = "log-router";
const SERVICE_CONTAINER: &str = "service";

/// The log router kind the FireLens container declares
/// (`smithy_query(query="shape",shape="FirelensConfigurationType")` -> fluentbit, fluentd).
const FIRELENS_TYPE: &str = "fluentbit";

/// The network the rows run in: one VPC and two PAIRS of subnets sharing one route table that
/// carries no route off the VPC at all — a pair per row, so each row's interfaces are its own.
const VPC_CIDR: &str = "10.98.0.0/16";
const ISOLATED_SUBNET_PAIRS: [[(&str, &str); 2]; 2] = [
    [("10.98.16.0/20", "a"), ("10.98.32.0/20", "b")],
    [("10.98.48.0/20", "a"), ("10.98.64.0/20", "b")],
];

/// The destination of the route that carries traffic OFF a VPC: "a route table contains a set of
/// rules, called routes" and the one for everything outside the VPC is the default route (Amazon
/// VPC User Guide, "Route tables"). No table below carries one.
const DEFAULT_ROUTE: &str = "0.0.0.0/0";

/// The repository of this account's own Amazon ECR registry the two-container revision's references
/// address, and the tags they name. NOTHING is pushed into it: what these rows turn on is whether
/// the placement can REACH the registry, which is decided before any manifest is asked for.
const REPOSITORY: &str = "clockverdict/app";
const LOG_ROUTER_TAG: &str = "log-router";
const SERVICE_TAG: &str = "service";

/// An image in a PUBLIC container registry, whose registry is no AWS service and so is reachable
/// only over the internet.
const PUBLIC_IMAGE: &str = "public.ecr.aws/docker/library/busybox:latest";

/// The Fargate task sizes the revisions declare (`TaskDefinition$cpu`: "2048 (2 vCPU) - Available
/// memory values: 4096 (4 GB) and 16384 (16 GB) in 1024 (1 GB) increments"), the larger for the
/// two-container revision and the smallest for the single-container one.
const PAIR_CPU: &str = "2048";
const PAIR_MEMORY: &str = "4096";
const SMALL_CPU: &str = "256";
const SMALL_MEMORY: &str = "512";

/// `AwsVpcConfiguration$assignPublicIp` as each row names it: "whether the task's elastic network
/// interface receives a public IP address".
const ASSIGN_PUBLIC_IP_ENABLED: &str = "ENABLED";
const ASSIGN_PUBLIC_IP_DISABLED: &str = "DISABLED";

/// `Task$lastStatus` of a task that has stopped.
const STOPPED: &str = "STOPPED";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of at all
/// (`smithy_query(query="shape",shape="TaskStopCode")` -> EssentialContainerExited,
/// InfrastructureHealth, ServiceSchedulerInitiated, SpotInterruption, TaskFailedToStart,
/// TerminationNotice, UserInitiated).
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// `Attachment$type` of the elastic network interface an `awsvpc` task is allocated, and the
/// `Attachment$details` entry naming where it was created.
const ELASTIC_NETWORK_INTERFACE: &str = "ElasticNetworkInterface";
const SUBNET_ID_DETAIL: &str = "subnetId";

/// What one row's revision declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Revision {
    /// A non-essential FireLens log router beside the essential service container, both pulling
    /// from this account's own Amazon ECR registry.
    FirelensPair,
    /// One essential container pulling from a public registry.
    SingleService,
}

/// One row: which pair of isolated subnets it is placed in, what it runs, and whether its launch
/// asked for a public address.
struct Row {
    case: &'static str,
    /// The name the row's family and service are built from.
    name: &'static str,
    /// Which of [`ISOLATED_SUBNET_PAIRS`] this row's service is placed in.
    pair: usize,
    assign_public_ip: &'static str,
    revision: Revision,
}

const ROWS: [Row; 2] = [
    Row {
        case: "a FireLens pair out of this account's registry, no public address",
        name: "pair",
        pair: 0,
        assign_public_ip: ASSIGN_PUBLIC_IP_DISABLED,
        revision: Revision::FirelensPair,
    },
    Row {
        case: "one container out of a public registry, with a public address asked for anyway",
        name: "single",
        pair: 1,
        assign_public_ip: ASSIGN_PUBLIC_IP_ENABLED,
        revision: Revision::SingleService,
    },
];

impl Row {
    /// The task-definition family the row's revision is registered under.
    fn family(&self) -> String {
        format!("clockverdict-{}", self.name)
    }

    /// The service the row scales up.
    fn service_name(&self) -> String {
        format!("clockverdict-{}", self.name)
    }

    /// The container names the row's revision declares, in the order it declares them.
    fn containers(&self) -> Vec<&'static str> {
        match self.revision {
            Revision::FirelensPair => vec![LOG_ROUTER_CONTAINER, SERVICE_CONTAINER],
            Revision::SingleService => vec![SERVICE_CONTAINER],
        }
    }
}

/// The references one revision's containers run, in the order the revision declares them.
fn images_of(revision: Revision, repository_uri: &str) -> Vec<String> {
    match revision {
        Revision::FirelensPair => vec![
            format!("{repository_uri}:{LOG_ROUTER_TAG}"),
            format!("{repository_uri}:{SERVICE_TAG}"),
        ],
        Revision::SingleService => vec![PUBLIC_IMAGE.to_string()],
    }
}

/// The REGISTRY an image reference is fetched from: the part before the first `/`, which every
/// reference below names explicitly. Panics on a reference of any other shape, so a row added later
/// cannot quietly compare against the wrong host.
fn registry_host(image: &str) -> &str {
    match image.split_once('/') {
        Some((host, _)) if host.contains('.') || host.contains(':') => host,
        _ => panic!("every reference in this suite names its registry, got {image:?}"),
    }
}

/// The WHOLE `stoppedReason` a task whose placement cannot reach the registry of `image` must
/// carry: the container runtime's error class, the reference the revision named, and the registry
/// that reference resolves to in the condition no path to it produces. Spelled out here rather than
/// imported, so a wrong constant in the implementation cannot make this pass.
fn expected_stopped_reason(image: &str) -> String {
    let host = registry_host(image);
    format!(
        "CannotPullContainerError: failed to resolve ref {image}: failed to do request: \
         Get \"https://{host}/v2/\": dial tcp {host}:443: i/o timeout"
    )
}

/// One subnet of `vpc`, created through Amazon EC2's own API in a zone of the signed Region.
async fn subnet(
    router: &MultiServiceRouter,
    vpc: &str,
    (cidr, zone_suffix): (&str, &str),
    case: &str,
) -> String {
    let zone = format!("{SIGNED_REGION}{zone_suffix}");
    let body = ec2_form(
        router,
        "CreateSubnet",
        &[
            ("VpcId", vpc),
            ("CidrBlock", cidr),
            ("AvailabilityZone", &zone),
        ],
        case,
    )
    .await;
    ec2_element(&body, "subnetId", case)
}

/// Create the network the rows run in — the VPC, the two pairs of subnets, the one route table they
/// share and the security group every placement names — and assert the precondition every row rests
/// on: the table in force on those subnets carries NO default route, which is what makes them
/// isolated.
///
/// Answers the pairs of subnet ids and the security group, every id minted by Amazon EC2.
async fn seed_network(router: &MultiServiceRouter) -> (Vec<Vec<String>>, String) {
    let case = "network";
    let vpc = ec2_element(
        &ec2_form(router, "CreateVpc", &[("CidrBlock", VPC_CIDR)], case).await,
        "vpcId",
        case,
    );
    let mut pairs: Vec<Vec<String>> = Vec::new();
    for pair in ISOLATED_SUBNET_PAIRS {
        let mut created = Vec::new();
        for cidr_and_zone in pair {
            created.push(subnet(router, &vpc, cidr_and_zone, case).await);
        }
        pairs.push(created);
    }
    // One route table of their own, with no route added to it: every route table comes with the
    // local route of its VPC alone, so a table nothing was added to reaches nothing outside it.
    let table = ec2_element(
        &ec2_form(router, "CreateRouteTable", &[("VpcId", &vpc)], case).await,
        "routeTableId",
        case,
    );
    for subnet_id in pairs.iter().flatten() {
        ec2_form(
            router,
            "AssociateRouteTable",
            &[("RouteTableId", &table), ("SubnetId", subnet_id)],
            case,
        )
        .await;
    }
    let described = ec2_form(
        router,
        "DescribeRouteTables",
        &[("RouteTableId.1", &table)],
        case,
    )
    .await;
    assert!(
        !all_texts(&described, "destinationCidrBlock").contains(&DEFAULT_ROUTE.to_string()),
        "case {case:?}: expected the table {table} to carry no default route, got {described}"
    );
    let security_group = ec2_element(
        &ec2_form(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", "clockverdict-group"),
                ("GroupDescription", "the group every placement below names"),
                ("VpcId", &vpc),
            ],
            case,
        )
        .await,
        "groupId",
        case,
    );
    (pairs, security_group)
}

/// Create the repository the two-container revision's references address, through Amazon ECR's own
/// API, and answer the URI it published for it. Nothing is pushed into it.
async fn seed_repository(router: &MultiServiceRouter) -> String {
    let created = ecr(
        router,
        "CreateRepository",
        json!({ "repositoryName": REPOSITORY }),
        "registry",
    )
    .await;
    string(&created["repository"], "repositoryUri", "registry")
}

/// The container definitions one row's revision declares.
fn container_definitions(row: &Row, repository_uri: &str) -> Value {
    let images = images_of(row.revision, repository_uri);
    match row.revision {
        // The log router is NOT essential — "if the `essential` parameter of a container is marked
        // as `false`, its failure doesn't affect the rest of the containers in a task" — and
        // carries the `firelensConfiguration` that makes it the task's log router.
        Revision::FirelensPair => json!([
            {
                "name": LOG_ROUTER_CONTAINER,
                "image": images[0],
                "essential": false,
                "firelensConfiguration": { "type": FIRELENS_TYPE },
            },
            {
                "name": SERVICE_CONTAINER,
                "image": images[1],
                "essential": true,
            },
        ]),
        Revision::SingleService => json!([
            {
                "name": SERVICE_CONTAINER,
                "image": images[0],
                "essential": true,
            },
        ]),
    }
}

/// Register the revision one row's service runs — an `awsvpc` Fargate revision of the row's
/// containers — and answer the ARN Amazon ECS minted for it.
///
/// Every revision names an EXECUTION role, the principal Amazon ECS fetches an image as: a Fargate
/// revision whose image is held in Amazon ECR and that names none could be pulled by nobody, which
/// `RegisterTaskDefinition` refuses.
async fn register_revision(router: &MultiServiceRouter, row: &Row, repository_uri: &str) -> String {
    let (cpu, memory) = match row.revision {
        Revision::FirelensPair => (PAIR_CPU, PAIR_MEMORY),
        Revision::SingleService => (SMALL_CPU, SMALL_MEMORY),
    };
    let registered = ok(
        router,
        "RegisterTaskDefinition",
        json!({
            "family": row.family(),
            "networkMode": "awsvpc",
            "requiresCompatibilities": ["FARGATE"],
            "cpu": cpu,
            "memory": memory,
            "executionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/ecs-execution"),
            "containerDefinitions": container_definitions(row, repository_uri),
        }),
        row.case,
    )
    .await;
    string(&registered["taskDefinition"], "taskDefinitionArn", row.case)
}

/// Every interface Amazon EC2 holds in `subnets` — the read a client has into the interfaces Amazon
/// ECS created for the tasks it placed there, which names no Amazon ECS operation at all.
async fn interfaces_in(router: &MultiServiceRouter, subnets: &[String], case: &str) -> Value {
    let names: Vec<String> = (1..=subnets.len())
        .map(|index| format!("Filter.1.Value.{index}"))
        .collect();
    let mut params: Vec<(&str, &str)> = vec![("Filter.1.Name", "subnet-id")];
    for (name, subnet_id) in names.iter().zip(subnets) {
        params.push((name.as_str(), subnet_id.as_str()));
    }
    let body = ec2_form(router, "DescribeNetworkInterfaces", &params, case).await;
    json!({ "held": all_texts(&body, "networkInterfaceId") })
}

/// Wait — reading ONLY Amazon EC2 — until an interface this row's subnets held is no longer held,
/// which is a task of this row placed and then stopped while nothing asked Amazon ECS anything.
///
/// The observation cannot be missed for want of looking often enough, and cannot be used up: an
/// interface is created when the task is placed and released only after that task has stopped,
/// which is several clock spans later, and the scheduler goes on replacing a task it could not
/// start — so interfaces are created, held and released over and over in these subnets while this
/// loop watches. The condition is "an id this loop SAW is no longer held", not "the subnets are
/// empty", so a replacement's interface arriving before the stopped task's is torn down still
/// satisfies it.
async fn wait_for_a_released_interface(
    router: &MultiServiceRouter,
    placed: &[String],
    case: &str,
) {
    let seen: RefCell<BTreeSet<String>> = RefCell::new(BTreeSet::new());
    poll_until(
        case,
        "an interface Amazon EC2 held in the row's subnets and then released",
        || async { interfaces_in(router, placed, case).await },
        |answered| {
            let held: BTreeSet<String> = answered["held"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect();
            let released = seen.borrow().iter().any(|id| !held.contains(id));
            seen.borrow_mut().extend(held);
            released
        },
    )
    .await;
}

/// The subnets the one `ElasticNetworkInterface` attachment of `task` names, asserting an `awsvpc`
/// task carries exactly one such attachment.
fn attachment_subnets(task: &Value, case: &str) -> Vec<String> {
    let interfaces: Vec<&Value> = task["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|attachment| attachment["type"].as_str() == Some(ELASTIC_NETWORK_INTERFACE))
        .collect();
    assert_eq!(
        interfaces.len(),
        1,
        "case {case:?}: an awsvpc task is allocated exactly one elastic network interface, got \
         {task}"
    );
    interfaces
        .iter()
        .flat_map(|attachment| attachment["details"].as_array().into_iter().flatten())
        .filter(|detail| detail["name"].as_str() == Some(SUBNET_ID_DETAIL))
        .filter_map(|detail| detail["value"].as_str().map(str::to_string))
        .collect()
}

/// Assert that `document` carries NO value for `member` — absent, or present as `null`.
///
/// Read as "absence", not as "no value of the type I expected": a member present with a value of
/// another JSON type would satisfy a typed read of `None`, so a wrongly typed `stopCode` or
/// `startedAt` would slip through such a check.
fn assert_absent(document: &Value, member: &str, case: &str) {
    assert!(
        document.get(member).is_none_or(Value::is_null),
        "case {case:?}: expected no {member} at all, got {document}"
    );
}

#[tokio::test]
async fn a_scaled_up_task_reaches_its_verdict_with_no_ecs_read_to_drive_it() {
    let router = emulator_with_ec2();
    let (pairs, security_group) = seed_network(&router).await;
    let repository_uri = seed_repository(&router).await;
    ok(
        &router,
        "CreateCluster",
        json!({ "clusterName": CLUSTER }),
        "cluster",
    )
    .await;

    for row in &ROWS {
        let case = row.case;
        let placed = pairs[row.pair].clone();
        let revision_arn = register_revision(&router, row, &repository_uri).await;
        // The service is created wanting NO tasks and then asked for one, which is the sequence a
        // client scaling a settled service up makes.
        let created = ok(
            &router,
            "CreateService",
            json!({
                "cluster": CLUSTER,
                "serviceName": row.service_name(),
                "taskDefinition": row.family(),
                "desiredCount": 0,
                "launchType": "FARGATE",
                "networkConfiguration": {
                    "awsvpcConfiguration": {
                        "subnets": placed,
                        "securityGroups": [security_group],
                        "assignPublicIp": row.assign_public_ip,
                    }
                },
            }),
            case,
        )
        .await;
        assert_eq!(
            created["service"]["desiredCount"].as_i64(),
            Some(0),
            "case {case:?}: the created service must want no tasks yet, got {created}"
        );
        let updated = ok(
            &router,
            "UpdateService",
            json!({
                "cluster": CLUSTER,
                "service": row.service_name(),
                "desiredCount": 1,
            }),
            case,
        )
        .await;
        assert_eq!(
            updated["service"]["desiredCount"].as_i64(),
            Some(1),
            "case {case:?}: the scale-up must be accepted, got {updated}"
        );

        // From here to the reads below, NOTHING asks Amazon ECS anything: the lifecycle is watched
        // in Amazon EC2's own inventory of the interfaces Amazon ECS created for it.
        wait_for_a_released_interface(&router, &placed, case).await;

        // The verdict is there to be read, in full, at the first read made of it.
        let listed = ok(
            &router,
            "ListTasks",
            json!({
                "cluster": CLUSTER,
                "serviceName": row.service_name(),
                "desiredStatus": STOPPED,
            }),
            case,
        )
        .await;
        let arns: Vec<String> = listed["taskArns"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|arn| arn.as_str().map(str::to_string))
            .collect();
        assert!(
            !arns.is_empty(),
            "case {case:?}: expected ListTasks to name at least one task wanted {STOPPED}, got \
             {listed}"
        );
        let described = ok(
            &router,
            "DescribeTasks",
            json!({ "cluster": CLUSTER, "tasks": arns }),
            case,
        )
        .await;
        let task = described["tasks"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|task| task["lastStatus"].as_str() == Some(STOPPED))
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a task that has arrived at {STOPPED}, got {described}")
            });
        assert_eq!(
            task["taskDefinitionArn"].as_str(),
            Some(revision_arn.as_str()),
            "case {case:?}: the task runs the revision the service deploys, got {task}"
        );
        assert_eq!(
            task["stopCode"].as_str(),
            Some(TASK_FAILED_TO_START),
            "case {case:?}: a task whose image could not be fetched never started, got {task}"
        );
        let first_image = images_of(row.revision, &repository_uri)
            .first()
            .cloned()
            .expect("every revision here declares a container");
        let expected = expected_stopped_reason(&first_image);
        assert_eq!(
            task["stoppedReason"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the stopped reason {expected:?}, got {task}"
        );
        assert_absent(&task, "startedAt", case);
        let subnets = attachment_subnets(&task, case);
        assert!(
            subnets.len() == 1 && placed.contains(&subnets[0]),
            "case {case:?}: expected the interface in one of {placed:?}, got {subnets:?} from \
             {task}"
        );
        let containers = task["containers"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            containers
                .iter()
                .filter_map(|container| container["name"].as_str())
                .collect::<Vec<&str>>(),
            row.containers(),
            "case {case:?}: expected the containers the revision declares, got {task}"
        );
        for container in &containers {
            assert_eq!(
                container["lastStatus"].as_str(),
                Some(STOPPED),
                "case {case:?}: every container of a stopped task is stopped, got {container}"
            );
            assert_absent(container, "exitCode", case);
        }
        let service = ok(
            &router,
            "DescribeServices",
            json!({ "cluster": CLUSTER, "services": [row.service_name()] }),
            case,
        )
        .await;
        assert_eq!(
            service["services"][0]["runningCount"].as_i64(),
            Some(0),
            "case {case:?}: a service whose tasks never ran runs none, got {service}"
        );
    }
}
