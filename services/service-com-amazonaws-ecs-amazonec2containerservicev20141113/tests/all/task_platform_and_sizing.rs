// SPDX-License-Identifier: CC0-1.0

//! The PLATFORM and the SIZING a placed task reports, and the record it keeps once it has
//! stopped — driven end to end over the shared wire harness in [`crate::ecs_wire`], with
//! Amazon EC2 (which holds the subnets a placement names) and Amazon ECR (which serves the
//! image pull) mounted beside this service on one engine, so every subnet and every image
//! is created through the OWNING service's own API.
//!
//! # The rule under test
//!
//! > A `Task` is a record of where Amazon ECS placed a workload, so every task-returning
//! > operation reports what the PLACEMENT resolved, from the instant the task exists: the
//! > processor architecture its containers run on (`Task$attributes`, under
//! > `ecs.cpu-architecture`), the Availability Zone of the capacity it was placed on
//! > (`Task$availabilityZone`), the operating-system family and the RESOLVED platform
//! > version of a Fargate placement (`Task$platformFamily`/`$platformVersion`, "specified
//! > only for tasks that use the Fargate launch type", "If you didn't specify one, the
//! > `LATEST` platform version is used"), the ephemeral storage it holds
//! > (`Task$ephemeralStorage`/`$fargateEphemeralStorage`), and the overrides it runs under
//! > (`Task$overrides`), which name every container of its revision. The containers it
//! > reports carry an id Amazon ECS spells as a UUID, a `lastStatus` from the CONTAINER
//! > lifecycle (`PROVISIONING` is a task-level status), and the runtime id Amazon ECS gave
//! > each container when it created it.
//!
//! # What the rows span
//!
//! Whatever the rule generalises over: the launch selection (a launch type, a capacity
//! provider strategy, Fargate against the capacity registered to the cluster), the
//! operating-system family and the CPU architecture the revision declares, the platform
//! version the request names against the `LATEST` alias it may leave out, where the
//! ephemeral storage size comes from (nothing, the registration, the placement's
//! overrides), the subnet the placement names — and so the zone it is in — and how many
//! containers the revision declares against how many of them the request overrides.
//!
//! The second case carries the rule into the task's TERMINAL record: a task whose image the
//! registry will not serve stops without ever running, and the stopped record still reports
//! every member above, with the interface Amazon ECS created for it torn down.
//!
//! # Two further rules of the same record
//!
//! > The size of the capacity Amazon ECS PROVISIONED for a task is reported beside the size
//! > the task itself states, for a placement onto capacity Amazon ECS provisions: a Fargate
//! > task runs on infrastructure sized for that task ("This field is required for Fargate",
//! > `Task$cpu`), so what was provisioned for it is its own resolved `cpu`/`memory`, while a
//! > task placed on capacity registered to the cluster runs on an instance the account
//! > provisioned for itself and reports no provisioned size at all. The rows span both ways
//! > a placement selects Fargate (a launch type, a capacity provider strategy), two task
//! > sizes, and a placement onto the cluster's own registered capacity.
//!
//! > Every instant this service reports is an epoch-seconds timestamp carrying at most
//! > MILLISECOND resolution, whatever resolution the host clock this emulator reads ticks
//! > at. The rows span the records that carry one — a task, a task-definition revision and a
//! > service with its deployment — read through the call that wrote it and through the read
//! > that answers it afterwards.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{
    assert_millisecond_resolution, create_cluster, ec2, ec2_element, ecr, emulator_with_ec2,
    emulator_with_ecr_and_iam, iam, identity_document, identity_document_signature, list, ok,
    register_capacity, register_definition, service, string, ACCOUNT, AWSVPC_SUBNET,
    SIGNED_REGION,
};

/// The cluster every placement below is made in.
const CLUSTER: &str = "taskplatform";

/// A SECOND cluster, whose one container instance reports an ARM64 processor: the
/// architecture a task's containers run on is the HOST's, so the row that places onto it
/// needs capacity of its own.
const ARM_CLUSTER: &str = "taskplatformarm";

/// The image the containers run. Which image it is does not decide any member under test in
/// the first case; the second case replaces it with a reference the registry answers for.
const IMAGE: &str = "public.ecr.aws/nginx/nginx:latest";

/// The attribute name Amazon ECS records a task's processor architecture under.
const CPU_ARCHITECTURE_ATTRIBUTE: &str = "ecs.cpu-architecture";

/// The attribute a container instance's agent reports its Availability Zone under.
const AVAILABILITY_ZONE_ATTRIBUTE: &str = "ecs.availability-zone";

/// `Task$stopCode` of a task Amazon ECS could not launch the containers of
/// (`smithy_query(query="shape",shape="TaskStopCode")`).
const TASK_FAILED_TO_START: &str = "TaskFailedToStart";

/// The error class a container runtime reports a failed image fetch under.
const CANNOT_PULL: &str = "CannotPullContainerError";

/// How often a poll re-reads the task it waits on.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long a poll keeps trying: Amazon ECS creates and attaches the interface an `awsvpc`
/// task is allocated (one `state::INTERFACE_STEP_SPAN` per step of
/// `state::INTERFACE_ATTACH_LIFECYCLE`), then the task's own start falls due, and the stop a
/// failed launch earns takes one more span — with generous margin for a loaded test machine.
/// Built by `Duration` arithmetic, so no figure here is a cast.
const POLL_BUDGET: Duration =
    service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::INTERFACE_STEP_SPAN
        .saturating_mul(2)
        .saturating_add(
            service_com_amazonaws_ecs_amazonec2containerservicev20141113::state::TRANSITION_SPAN
                .saturating_mul(4),
        )
        .saturating_add(Duration::from_secs(30));

/// One container definition.
fn container(name: &str, image: &str) -> Value {
    json!({ "name": name, "image": image, "memory": 128, "cpu": 0 })
}

/// The `networkConfiguration` a launch of an `awsvpc` revision carries, into `subnet`.
fn network_configuration(subnet: &str) -> Value {
    json!({
        "awsvpcConfiguration": {
            "subnets": [subnet],
            "securityGroups": ["sg-0123456789abcdef0"],
        }
    })
}

/// Create one subnet of `vpc_id` in the zone `zone`, through Amazon EC2's own API, and
/// answer the id it minted.
async fn subnet(
    router: &MultiServiceRouter,
    vpc_id: &str,
    cidr_block: &str,
    zone: &str,
    case: &str,
) -> String {
    let body = ec2(
        router,
        "CreateSubnet",
        &[
            ("VpcId", vpc_id),
            ("CidrBlock", cidr_block),
            ("AvailabilityZone", zone),
        ],
        case,
    )
    .await;
    ec2_element(&body, "subnetId", case)
}

/// The value of the `Attribute` named `name` on `task`, or `None` when the task reports no
/// such attribute.
fn attribute(task: &Value, name: &str, case: &str) -> Option<String> {
    list(task, "attributes", case)
        .into_iter()
        .find(|attribute| attribute["name"].as_str() == Some(name))
        .and_then(|attribute| attribute["value"].as_str().map(ToString::to_string))
}

/// Whether `id` is spelled as a UUID: five hyphen-separated groups of 8, 4, 4, 4 and 12
/// hexadecimal digits.
fn is_uuid(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .map(|group| group.len())
            .eq([8usize, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.chars().all(|digit| digit.is_ascii_hexdigit()))
}

/// The one container of a task, or a panic naming the task that holds another number of
/// them.
fn only_container(task: &Value, case: &str) -> Value {
    let containers = list(task, "containers", case);
    assert_eq!(
        containers.len(),
        1,
        "case {case:?}: expected one container, got {task}"
    );
    containers[0].clone()
}

/// Create the task execution role a Fargate task pulling an Amazon ECR image needs,
/// through IAM's OWN API: trusted by Amazon ECS and granted the two calls a pull makes, so
/// the only thing that can refuse the fetch below is the reference itself.
async fn execution_role(router: &MultiServiceRouter, name: &str, case: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "ecs-tasks.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }]
    })
    .to_string();
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
        case,
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name.to_string()),
            ("PolicyName", "pull".to_string()),
            (
                "PolicyDocument",
                json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": ["ecr:GetAuthorizationToken", "ecr:BatchGetImage"],
                        "Resource": "*",
                    }],
                })
                .to_string(),
            ),
        ],
        case,
    )
    .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Read one task back through `DescribeTasks` — the stored record, not the answer of the
/// call that placed it.
async fn describe(
    router: &MultiServiceRouter,
    cluster: &str,
    task_arn: &str,
    case: &str,
) -> Value {
    let body = ok(
        router,
        "DescribeTasks",
        json!({ "cluster": cluster, "tasks": [task_arn] }),
        case,
    )
    .await;
    list(&body, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected one task back, got {body}"))
}

/// What one row of the table asks for, and what the task it places must report.
struct Row {
    case: &'static str,
    /// The cluster the row places into: the capacity a row is about is the capacity its
    /// cluster holds.
    cluster: &'static str,
    /// The family the row registers, so each row reads its own revision back.
    family: &'static str,
    /// The containers the revision declares.
    containers: Vec<Value>,
    /// `ephemeralStorage` the REGISTRATION carries, if any.
    registered_storage: Option<i32>,
    /// `runtimePlatform` the registration carries, if any.
    runtime_platform: Option<Value>,
    /// The task size the registration carries: Fargate requires one.
    cpu: &'static str,
    memory: &'static str,
    /// Whether the revision is registered for Fargate, and so what the placement may name.
    fargate: bool,
    /// What the run names for its placement: a `launchType`, else a strategy.
    launch_type: Option<&'static str>,
    strategy: Option<&'static str>,
    /// The `platformVersion` the run names, if any.
    platform_version: Option<&'static str>,
    /// The `overrides` the run carries, if any.
    overrides: Option<Value>,
    /// The subnet the run places into, and the zone that subnet is in.
    subnet: String,
    expect_zone: String,
    /// The members the task must report: `None` for a member a task on this capacity
    /// reports none of.
    expect_platform_family: Option<&'static str>,
    expect_platform_version: Option<&'static str>,
    expect_storage_gib: Option<i64>,
    expect_architecture: &'static str,
    /// The `containerOverrides` the task must report, in order.
    expect_container_overrides: Value,
}

#[tokio::test]
async fn a_placed_task_reports_the_platform_and_sizing_its_placement_resolved() {
    let router = emulator_with_ec2();
    create_cluster(&router, CLUSTER, "setup").await;
    // A task placed with the `EC2` launch type runs on the capacity registered to its
    // cluster, so the cluster holds one container instance.
    register_capacity(&router, CLUSTER, "setup").await;
    // The predefined Fargate providers place tasks in this cluster only once they are
    // associated with it.
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": CLUSTER,
            "capacityProviders": ["FARGATE", "FARGATE_SPOT"],
            "defaultCapacityProviderStrategy": [],
        }),
        "setup",
    )
    .await;

    // Two subnets in two different Availability Zones, created through Amazon EC2's own
    // API: a task's zone is the zone of the subnet its interface was created in, so the
    // rows that differ only in the subnet they name must differ in the zone they report.
    let vpc = ec2_element(
        &ec2(&router, "CreateVpc", &[("CidrBlock", "10.80.0.0/16")], "setup").await,
        "vpcId",
        "setup",
    );
    let first_zone = format!("{SIGNED_REGION}a");
    let second_zone = format!("{SIGNED_REGION}b");
    let first_subnet = subnet(&router, &vpc, "10.80.1.0/24", &first_zone, "setup").await;
    let second_subnet = subnet(&router, &vpc, "10.80.2.0/24", &second_zone, "setup").await;

    // A cluster whose one container instance's agent reports an ARM64 processor and the
    // zone it is in, which is what the agent reports as the instance's built-in attributes.
    create_cluster(&router, ARM_CLUSTER, "setup").await;
    ok(
        &router,
        "RegisterContainerInstance",
        json!({
            "cluster": ARM_CLUSTER,
            "instanceIdentityDocument": identity_document("i-0a11b22c33d44e55f"),
            "instanceIdentityDocumentSignature": identity_document_signature(),
            "attributes": [
                { "name": CPU_ARCHITECTURE_ATTRIBUTE, "value": "arm64" },
                { "name": AVAILABILITY_ZONE_ATTRIBUTE, "value": second_zone.clone() },
            ],
        }),
        "setup",
    )
    .await;

    let rows = vec![
        Row {
            case: "a Fargate launch type that names no platform version",
            family: "platformdefault",
            cluster: CLUSTER,
            containers: vec![container("app", IMAGE)],
            registered_storage: None,
            runtime_platform: None,
            cpu: "256",
            memory: "512",
            fargate: true,
            launch_type: Some("FARGATE"),
            strategy: None,
            platform_version: None,
            overrides: None,
            subnet: first_subnet.clone(),
            expect_zone: first_zone.clone(),
            expect_platform_family: Some("Linux"),
            // The `LATEST` alias resolves to the newest Linux platform version.
            expect_platform_version: Some("1.4.0"),
            // The amount a Fargate task holds without asking for more.
            expect_storage_gib: Some(20),
            expect_architecture: "x86_64",
            expect_container_overrides: json!([{ "name": "app" }]),
        },
        Row {
            case: "a Fargate launch type naming the platform version it wants, on ARM64",
            family: "platformnamed",
            cluster: CLUSTER,
            containers: vec![container("app", IMAGE)],
            registered_storage: None,
            runtime_platform: Some(json!({
                "cpuArchitecture": "ARM64",
                "operatingSystemFamily": "LINUX",
            })),
            cpu: "256",
            memory: "512",
            fargate: true,
            launch_type: Some("FARGATE"),
            strategy: None,
            platform_version: Some("1.4.0"),
            overrides: None,
            subnet: second_subnet.clone(),
            expect_zone: second_zone.clone(),
            expect_platform_family: Some("Linux"),
            expect_platform_version: Some("1.4.0"),
            expect_storage_gib: Some(20),
            expect_architecture: "arm64",
            expect_container_overrides: json!([{ "name": "app" }]),
        },
        Row {
            case: "a Fargate launch type on a Windows operating-system family",
            family: "platformwindows",
            cluster: CLUSTER,
            containers: vec![container("app", IMAGE)],
            registered_storage: None,
            runtime_platform: Some(json!({
                "operatingSystemFamily": "WINDOWS_SERVER_2019_CORE",
            })),
            // The smallest Windows task size Fargate publishes.
            cpu: "1024",
            memory: "2048",
            fargate: true,
            launch_type: Some("FARGATE"),
            strategy: None,
            platform_version: None,
            overrides: None,
            subnet: first_subnet.clone(),
            expect_zone: first_zone.clone(),
            expect_platform_family: Some("Windows-Server-2019-Core"),
            // The one Windows platform version the Fargate platform-versions page
            // publishes, which is what `LATEST` resolves to for a Windows task.
            expect_platform_version: Some("1.0.0"),
            expect_storage_gib: Some(20),
            expect_architecture: "x86_64",
            expect_container_overrides: json!([{ "name": "app" }]),
        },
        Row {
            case: "a capacity provider strategy naming a Fargate provider",
            family: "platformspot",
            cluster: CLUSTER,
            containers: vec![container("app", IMAGE)],
            registered_storage: None,
            runtime_platform: None,
            cpu: "512",
            memory: "1024",
            fargate: true,
            launch_type: None,
            strategy: Some("FARGATE_SPOT"),
            platform_version: None,
            overrides: None,
            subnet: second_subnet.clone(),
            expect_zone: second_zone.clone(),
            expect_platform_family: Some("Linux"),
            expect_platform_version: Some("1.4.0"),
            expect_storage_gib: Some(20),
            expect_architecture: "x86_64",
            expect_container_overrides: json!([{ "name": "app" }]),
        },
        Row {
            case: "a registration that expanded its ephemeral storage",
            family: "platformstorage",
            cluster: CLUSTER,
            containers: vec![container("app", IMAGE)],
            registered_storage: Some(50),
            runtime_platform: None,
            cpu: "256",
            memory: "512",
            fargate: true,
            launch_type: Some("FARGATE"),
            strategy: None,
            platform_version: None,
            overrides: None,
            subnet: first_subnet.clone(),
            expect_zone: first_zone.clone(),
            expect_platform_family: Some("Linux"),
            expect_platform_version: Some("1.4.0"),
            expect_storage_gib: Some(50),
            expect_architecture: "x86_64",
            expect_container_overrides: json!([{ "name": "app" }]),
        },
        Row {
            case: "a placement whose overrides expand the ephemeral storage further",
            family: "platformoverridden",
            cluster: CLUSTER,
            containers: vec![container("app", IMAGE)],
            registered_storage: Some(50),
            runtime_platform: None,
            cpu: "256",
            memory: "512",
            fargate: true,
            launch_type: Some("FARGATE"),
            strategy: None,
            platform_version: None,
            overrides: Some(json!({ "ephemeralStorage": { "sizeInGiB": 100 } })),
            subnet: second_subnet.clone(),
            expect_zone: second_zone.clone(),
            expect_platform_family: Some("Linux"),
            expect_platform_version: Some("1.4.0"),
            expect_storage_gib: Some(100),
            expect_architecture: "x86_64",
            expect_container_overrides: json!([{ "name": "app" }]),
        },
        Row {
            case: "an EC2 launch type with two containers, one of them overridden",
            family: "platformec2",
            cluster: CLUSTER,
            containers: vec![container("app", IMAGE), container("sidecar", IMAGE)],
            registered_storage: None,
            runtime_platform: None,
            cpu: "256",
            memory: "512",
            fargate: false,
            launch_type: Some("EC2"),
            strategy: None,
            platform_version: None,
            overrides: Some(json!({
                "containerOverrides": [{ "name": "sidecar", "command": ["sleep", "1"] }],
            })),
            subnet: first_subnet.clone(),
            expect_zone: first_zone.clone(),
            // "A platform family is specified only for tasks that use the Fargate launch
            // type", and the same holds for the platform version and the Fargate ephemeral
            // storage.
            expect_platform_family: None,
            expect_platform_version: None,
            expect_storage_gib: None,
            expect_architecture: "x86_64",
            // Every container of the revision is named, in the revision's own order, and
            // the one the request overrode carries what it overrode.
            expect_container_overrides: json!([
                { "name": "app" },
                { "name": "sidecar", "command": ["sleep", "1"] },
            ]),
        },
        Row {
            // The architecture a task's containers run on is the architecture of the
            // CAPACITY it was placed on, so a task on an instance whose agent reports
            // ARM64 reports `arm64` whatever its revision declares — this one declares
            // nothing, which would otherwise default to x86_64.
            case: "an EC2 launch type on capacity whose agent reports an ARM64 processor",
            family: "platformarm",
            cluster: ARM_CLUSTER,
            containers: vec![container("app", IMAGE)],
            registered_storage: None,
            runtime_platform: None,
            cpu: "256",
            memory: "512",
            fargate: false,
            launch_type: Some("EC2"),
            strategy: None,
            platform_version: None,
            overrides: None,
            subnet: second_subnet.clone(),
            expect_zone: second_zone.clone(),
            expect_platform_family: None,
            expect_platform_version: None,
            expect_storage_gib: None,
            expect_architecture: "arm64",
            expect_container_overrides: json!([{ "name": "app" }]),
        },
    ];

    for row in &rows {
        let case = row.case;
        let mut definition = json!({
            "family": row.family,
            "networkMode": "awsvpc",
            "cpu": row.cpu,
            "memory": row.memory,
            "containerDefinitions": row.containers.clone(),
        });
        if row.fargate {
            definition["requiresCompatibilities"] = json!(["FARGATE"]);
        }
        if let Some(size) = row.registered_storage {
            definition["ephemeralStorage"] = json!({ "sizeInGiB": size });
        }
        if let Some(platform) = row.runtime_platform.clone() {
            definition["runtimePlatform"] = platform;
        }
        ok(&router, "RegisterTaskDefinition", definition, case).await;

        let mut request = json!({
            "cluster": row.cluster,
            "taskDefinition": row.family,
            "networkConfiguration": network_configuration(&row.subnet),
        });
        if let Some(launch_type) = row.launch_type {
            request["launchType"] = json!(launch_type);
        }
        if let Some(provider) = row.strategy {
            request["capacityProviderStrategy"] =
                json!([{ "capacityProvider": provider, "weight": 1 }]);
        }
        if let Some(version) = row.platform_version {
            request["platformVersion"] = json!(version);
        }
        if let Some(overrides) = row.overrides.clone() {
            request["overrides"] = overrides;
        }
        let placed = ok(&router, "RunTask", request, case).await;
        let tasks = list(&placed, "tasks", case);
        assert_eq!(
            tasks.len(),
            1,
            "case {case:?}: expected RunTask to place one task, got {placed}"
        );
        let task = tasks[0].clone();
        let task_arn = string(&task, "taskArn", case);

        // The placement answers with the record it just wrote, and the record reads back
        // the same through `DescribeTasks`: both are asserted, so a member computed for one
        // answer and not stored cannot pass.
        let read = describe(&router, row.cluster, &task_arn, case).await;
        for (surface, task) in [("RunTask", &task), ("DescribeTasks", &read)] {
            assert_eq!(
                attribute(task, CPU_ARCHITECTURE_ATTRIBUTE, case).as_deref(),
                Some(row.expect_architecture),
                "case {case:?} ({surface}): expected the {CPU_ARCHITECTURE_ATTRIBUTE} \
                 attribute {:?}, got {task}",
                row.expect_architecture
            );
            assert_eq!(
                task["availabilityZone"].as_str(),
                Some(row.expect_zone.as_str()),
                "case {case:?} ({surface}): expected availabilityZone {:?}, got {task}",
                row.expect_zone
            );
            assert_eq!(
                task["platformFamily"].as_str(),
                row.expect_platform_family,
                "case {case:?} ({surface}): expected platformFamily {:?}, got {task}",
                row.expect_platform_family
            );
            assert_eq!(
                task["platformVersion"].as_str(),
                row.expect_platform_version,
                "case {case:?} ({surface}): expected platformVersion {:?}, got {task}",
                row.expect_platform_version
            );
            for member in ["ephemeralStorage", "fargateEphemeralStorage"] {
                assert_eq!(
                    task[member]["sizeInGiB"].as_i64(),
                    row.expect_storage_gib,
                    "case {case:?} ({surface}): expected {member} of {:?} GiB, got {task}",
                    row.expect_storage_gib
                );
            }
            assert_eq!(
                task["overrides"]["containerOverrides"],
                row.expect_container_overrides,
                "case {case:?} ({surface}): expected the overrides to name every container \
                 of the revision, got {task}"
            );
            assert_eq!(
                task["overrides"]["inferenceAcceleratorOverrides"],
                json!([]),
                "case {case:?} ({surface}): expected an empty \
                 inferenceAcceleratorOverrides list, got {task}"
            );
            for container in list(task, "containers", case) {
                let container_arn = string(&container, "containerArn", case);
                let id = container_arn
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    is_uuid(&id),
                    "case {case:?} ({surface}): expected the container id in \
                     {container_arn:?} to be spelled as a UUID, got {id:?}"
                );
            }
        }

        // `PROVISIONING` is a stage of the TASK's lifecycle — Amazon ECS has to create and
        // attach the interface an `awsvpc` task is allocated before it can launch it — and
        // not one of a container's, which starts at the first stage of its own lifecycle.
        assert_eq!(
            task["lastStatus"].as_str(),
            Some("PROVISIONING"),
            "case {case:?}: expected the placed task in PROVISIONING, got {task}"
        );
        // And nothing of that interface is published before it exists: the attachment
        // carries the subnet the launch supplied and nothing else, and no container reports
        // an interface it cannot be reached on yet.
        let attachments = list(&task, "attachments", case);
        assert_eq!(
            attachments.len(),
            1,
            "case {case:?}: expected the one interface an awsvpc task is allocated, got \
             {task}"
        );
        let details: Vec<String> = list(&attachments[0], "details", case)
            .iter()
            .map(|detail| string(detail, "name", case))
            .collect();
        assert_eq!(
            details,
            vec!["subnetId".to_string()],
            "case {case:?}: expected only the subnet the launch supplied on a task whose \
             interface is not in place, got {:?}",
            attachments[0]
        );
        for container in list(&task, "containers", case) {
            assert_eq!(
                container["lastStatus"].as_str(),
                Some("PENDING"),
                "case {case:?}: expected the container at PENDING while its task \
                 provisions, got {container}"
            );
            assert_eq!(
                list(&container, "networkInterfaces", case),
                Vec::<Value>::new(),
                "case {case:?}: expected no network interface before the task's own is in \
                 place, got {container}"
            );
            assert!(
                container["runtimeId"].is_null(),
                "case {case:?}: expected no runtimeId before Amazon ECS has created the \
                 container, got {container}"
            );
        }
        // And once it HAS created them — the step the image fetch is made in — every
        // container reports the runtime id it was created with.
        for container in list(&read, "containers", case) {
            let runtime_id = string(&container, "runtimeId", case);
            assert!(
                !runtime_id.is_empty(),
                "case {case:?}: expected the runtime id of the created container, got \
                 {container}"
            );
        }
    }
}

#[tokio::test]
async fn an_allocated_interface_takes_an_address_of_the_subnet_it_was_created_in() {
    // The rows differ along the subnet the placement names — and so along the CIDR block
    // the interface created in it has to take its address out of.
    let router = emulator_with_ec2();
    let case = "the interface's own address";
    create_cluster(&router, CLUSTER, case).await;
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": CLUSTER,
            "capacityProviders": ["FARGATE"],
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;
    let vpc = ec2_element(
        &ec2(&router, "CreateVpc", &[("CidrBlock", "10.81.0.0/16")], case).await,
        "vpcId",
        case,
    );
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": "platformaddress",
            "requiresCompatibilities": ["FARGATE"],
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [container("app", IMAGE)],
        }),
        case,
    )
    .await;

    // Two subnets of the same VPC, in two different blocks: an interface is reachable at an
    // address OF the subnet it sits in, so the address each task's interface reports has to
    // be inside the block of the subnet that task was placed into.
    let zone = format!("{SIGNED_REGION}a");
    let mut placed = Vec::new();
    for (cidr, prefix) in [("10.81.7.0/24", "10.81.7."), ("10.81.9.0/24", "10.81.9.")] {
        let created = subnet(&router, &vpc, cidr, &zone, case).await;
        let body = ok(
            &router,
            "RunTask",
            json!({
                "cluster": CLUSTER,
                "taskDefinition": "platformaddress",
                "launchType": "FARGATE",
                "networkConfiguration": network_configuration(&created),
            }),
            case,
        )
        .await;
        placed.push((
            cidr,
            prefix,
            string(&list(&body, "tasks", case)[0], "taskArn", case),
        ));
    }

    for (cidr, prefix, task_arn) in placed {
        // Wait for the CONDITION: the interface being in place is what publishes its own
        // address, so the read is repeated until it does or a generous deadline passes.
        let deadline = Instant::now() + POLL_BUDGET;
        let address = loop {
            let task = describe(&router, CLUSTER, &task_arn, case).await;
            let attachment = list(&task, "attachments", case)
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("case {case:?}: expected the task's interface, got {task}"));
            let reported = list(&attachment, "details", case)
                .into_iter()
                .find(|detail| detail["name"].as_str() == Some("privateIPv4Address"))
                .and_then(|detail| detail["value"].as_str().map(ToString::to_string));
            if let Some(address) = reported {
                break address;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the interface in {cidr} to report its address, got \
                 {task}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert!(
            address.starts_with(prefix),
            "case {case:?}: expected an address of the subnet {cidr} the interface was \
             created in, got {address:?}"
        );
    }
}

#[tokio::test]
async fn a_task_that_never_launched_keeps_its_platform_record_and_loses_its_interface() {
    let router = emulator_with_ecr_and_iam();
    let case = "a tag the repository does not hold";
    create_cluster(&router, CLUSTER, case).await;
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": CLUSTER,
            "capacityProviders": ["FARGATE"],
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;

    // The registry's records: one repository holding one tagged image, through Amazon ECR's
    // own API.
    ecr(
        &router,
        "CreateRepository",
        json!({ "repositoryName": "platform" }),
        case,
    )
    .await;
    ecr(
        &router,
        "PutImage",
        json!({
            "repositoryName": "platform",
            "imageManifest": concat!(
                r#"{"schemaVersion":2,"#,
                r#""mediaType":"application/vnd.docker.distribution.manifest.v2+json"}"#
            ),
            "imageTag": "1",
        }),
        case,
    )
    .await;

    // A revision running a tag that repository does not hold: the registry will not serve
    // the pull, so the containers never start.
    let image = format!("{ACCOUNT}.dkr.ecr.{SIGNED_REGION}.amazonaws.com/platform:2");
    let role = execution_role(&router, "platformpull", case).await;
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": "platformunpullable",
            "requiresCompatibilities": ["FARGATE"],
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "executionRoleArn": role,
            "ephemeralStorage": { "sizeInGiB": 30 },
            "containerDefinitions": [container("app", &image)],
        }),
        case,
    )
    .await;
    let placed = ok(
        &router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": "platformunpullable",
            "launchType": "FARGATE",
            "networkConfiguration": network_configuration(AWSVPC_SUBNET),
        }),
        case,
    )
    .await;
    let task_arn = string(&list(&placed, "tasks", case)[0], "taskArn", case);

    // Wait for the CONDITION the rule is about — the task reaching its terminal status —
    // re-reading it until it holds or a generous deadline passes.
    let deadline = Instant::now() + POLL_BUDGET;
    let stopped = loop {
        let task = describe(&router, CLUSTER, &task_arn, case).await;
        if task["lastStatus"].as_str() == Some("STOPPED") {
            break task;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the task to stop, it was still {:?}: {task}",
            task["lastStatus"].as_str()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    // The stop a task that never ran earns: no `startedAt`, the stop code for a task that
    // failed before its containers started, and the registry's own account of the fetch.
    assert_eq!(
        (
            stopped["desiredStatus"].as_str(),
            stopped["stopCode"].as_str(),
            stopped["startedAt"].is_null(),
        ),
        (Some("STOPPED"), Some(TASK_FAILED_TO_START), true),
        "case {case:?}: expected the terminal record of a task that never ran, got {stopped}"
    );
    let reason = string(&stopped, "stoppedReason", case);
    assert!(
        reason.contains(CANNOT_PULL) && reason.contains(&image),
        "case {case:?}: expected the stoppedReason to report {CANNOT_PULL} for {image}, got \
         {reason:?}"
    );
    for member in [
        "pullStartedAt",
        "pullStoppedAt",
        "stoppingAt",
        "stoppedAt",
        "executionStoppedAt",
    ] {
        assert!(
            !stopped[member].is_null(),
            "case {case:?}: expected {member} on the stopped task, got {stopped}"
        );
    }

    // The platform and sizing the placement resolved are still reported: the record is
    // where the task ran, which stopping does not forget.
    assert_eq!(
        (
            attribute(&stopped, CPU_ARCHITECTURE_ATTRIBUTE, case).as_deref(),
            stopped["availabilityZone"].as_str(),
            stopped["platformFamily"].as_str(),
            stopped["platformVersion"].as_str(),
            stopped["ephemeralStorage"]["sizeInGiB"].as_i64(),
            stopped["fargateEphemeralStorage"]["sizeInGiB"].as_i64(),
        ),
        (
            Some("x86_64"),
            Some(format!("{SIGNED_REGION}a").as_str()),
            Some("Linux"),
            Some("1.4.0"),
            Some(30),
            Some(30),
        ),
        "case {case:?}: expected the stopped task to keep its platform record, got {stopped}"
    );
    assert_eq!(
        stopped["overrides"]["containerOverrides"],
        json!([{ "name": "app" }]),
        "case {case:?}: expected the overrides the task ran under, got {stopped}"
    );

    // The container stopped with the task, and still reports the runtime id Amazon ECS gave
    // it when it created it — the step the fetch that failed was made in.
    let container = only_container(&stopped, case);
    assert_eq!(
        container["lastStatus"].as_str(),
        Some("STOPPED"),
        "case {case:?}: expected the container to stop with its task, got {container}"
    );
    assert!(
        !string(&container, "runtimeId", case).is_empty(),
        "case {case:?}: expected the created container's runtime id, got {container}"
    );

    // The interface Amazon ECS created for the task is torn down with the task, so the
    // attachment that stood for it reports the resource as gone — while keeping the details
    // a client resolves the interface it HELD by.
    let attachments = list(&stopped, "attachments", case);
    assert_eq!(
        attachments.len(),
        1,
        "case {case:?}: expected the one interface an awsvpc task is allocated, got \
         {stopped}"
    );
    assert_eq!(
        attachments[0]["status"].as_str(),
        Some("DELETED"),
        "case {case:?}: expected the stopped task's interface to be gone, got {:?}",
        attachments[0]
    );
    for name in [
        "subnetId",
        "networkInterfaceId",
        "macAddress",
        "privateIPv4Address",
    ] {
        assert!(
            list(&attachments[0], "details", case)
                .iter()
                .any(|detail| detail["name"].as_str() == Some(name)),
            "case {case:?}: expected the interface's {name} on the attachment, got {:?}",
            attachments[0]
        );
    }

    // And a `StopTask` on a task that has ALREADY stopped reports that terminal record as
    // it stands: the reason it stopped is the diagnostic a client reads, so nothing
    // overwrites it with an operator-initiated stop.
    let answered = ok(
        &router,
        "StopTask",
        json!({ "cluster": CLUSTER, "task": task_arn, "reason": "asked to stop" }),
        case,
    )
    .await;
    let task = answered["task"].clone();
    assert_eq!(
        (
            task["lastStatus"].as_str(),
            task["stopCode"].as_str(),
            task["stoppedReason"].as_str(),
            task["platformVersion"].as_str(),
        ),
        (
            Some("STOPPED"),
            Some(TASK_FAILED_TO_START),
            Some(reason.as_str()),
            Some("1.4.0"),
        ),
        "case {case:?}: expected StopTask to answer the terminal record it found, got {task}"
    );
}

/// The cluster the provisioned-size rows are placed in.
const PROVISIONED_CLUSTER: &str = "taskprovisioned";

/// One row of the provisioned-size table: the task size a revision registers, how the
/// placement selects its capacity, and what the task must then report as provisioned.
struct SizeRow {
    case: &'static str,
    /// The family the row registers, so each row reads its own revision back.
    family: &'static str,
    /// The task size the revision carries, which is the size the task states.
    cpu: &'static str,
    memory: &'static str,
    /// What the run names for its placement: a `launchType`, else a capacity provider
    /// strategy over the provider named here.
    launch_type: Option<&'static str>,
    strategy: Option<&'static str>,
    /// Whether the capacity this placement runs on is capacity Amazon ECS provisions, which
    /// is what decides whether a provisioned size is reported at all.
    expect_provisioned: bool,
}

#[tokio::test]
async fn a_task_reports_the_size_of_the_capacity_amazon_ecs_provisioned_for_it() {
    let router = service();
    let case = "setup";
    create_cluster(&router, PROVISIONED_CLUSTER, case).await;
    // A task placed with the `EC2` launch type runs on the capacity registered to its
    // cluster, so the cluster holds one container instance; the predefined Fargate
    // providers place tasks in it only once they are associated with it.
    register_capacity(&router, PROVISIONED_CLUSTER, case).await;
    ok(
        &router,
        "PutClusterCapacityProviders",
        json!({
            "cluster": PROVISIONED_CLUSTER,
            "capacityProviders": ["FARGATE", "FARGATE_SPOT"],
            "defaultCapacityProviderStrategy": [],
        }),
        case,
    )
    .await;

    let rows = [
        SizeRow {
            case: "a Fargate launch type",
            family: "provisionedfargate",
            cpu: "256",
            memory: "512",
            launch_type: Some("FARGATE"),
            strategy: None,
            expect_provisioned: true,
        },
        SizeRow {
            case: "a larger Fargate task size",
            family: "provisionedlarge",
            cpu: "1024",
            memory: "2048",
            launch_type: Some("FARGATE"),
            strategy: None,
            expect_provisioned: true,
        },
        SizeRow {
            case: "a capacity provider strategy naming a Fargate provider",
            family: "provisionedspot",
            cpu: "512",
            memory: "1024",
            launch_type: None,
            strategy: Some("FARGATE_SPOT"),
            expect_provisioned: true,
        },
        SizeRow {
            case: "an EC2 launch type on the capacity registered to the cluster",
            family: "provisionedec2",
            cpu: "256",
            memory: "512",
            launch_type: Some("EC2"),
            strategy: None,
            expect_provisioned: false,
        },
    ];

    for row in &rows {
        let case = row.case;
        let mut definition = json!({
            "family": row.family,
            "networkMode": "awsvpc",
            "cpu": row.cpu,
            "memory": row.memory,
            "containerDefinitions": [container("app", IMAGE)],
        });
        if row.expect_provisioned {
            definition["requiresCompatibilities"] = json!(["FARGATE"]);
        }
        ok(&router, "RegisterTaskDefinition", definition, case).await;

        let mut request = json!({
            "cluster": PROVISIONED_CLUSTER,
            "taskDefinition": row.family,
            "networkConfiguration": network_configuration(AWSVPC_SUBNET),
        });
        if let Some(launch_type) = row.launch_type {
            request["launchType"] = json!(launch_type);
        }
        if let Some(provider) = row.strategy {
            request["capacityProviderStrategy"] =
                json!([{ "capacityProvider": provider, "weight": 1 }]);
        }
        let placed = ok(&router, "RunTask", request, case).await;
        let tasks = list(&placed, "tasks", case);
        assert_eq!(
            tasks.len(),
            1,
            "case {case:?}: expected RunTask to place one task, got {placed}"
        );
        let task = tasks[0].clone();
        let task_arn = string(&task, "taskArn", case);
        // Both surfaces are asserted, so a member computed for the placement's own answer
        // and not stored cannot pass.
        let read = describe(&router, PROVISIONED_CLUSTER, &task_arn, case).await;
        for (surface, task) in [("RunTask", &task), ("DescribeTasks", &read)] {
            let (expect_cpu, expect_memory) = if row.expect_provisioned {
                (Some(row.cpu), Some(row.memory))
            } else {
                (None, None)
            };
            assert_eq!(
                task["provisionedCpu"].as_str(),
                expect_cpu,
                "case {case:?} ({surface}): expected provisionedCpu {expect_cpu:?}, got {task}"
            );
            assert_eq!(
                task["provisionedMemory"].as_str(),
                expect_memory,
                "case {case:?} ({surface}): expected provisionedMemory {expect_memory:?}, \
                 got {task}"
            );
            // Where a size IS reported it is the task's own: the capacity was provisioned
            // for this task, so the two can never disagree.
            if row.expect_provisioned {
                assert_eq!(
                    (task["provisionedCpu"].as_str(), task["provisionedMemory"].as_str()),
                    (task["cpu"].as_str(), task["memory"].as_str()),
                    "case {case:?} ({surface}): expected the provisioned size to be the \
                     task's own, got {task}"
                );
            }
        }
    }
}

/// The cluster the records read for their reported instants are created in.
const INSTANT_CLUSTER: &str = "taskinstants";

#[tokio::test]
async fn every_instant_a_record_reports_carries_at_most_milliseconds() {
    let router = service();
    let case = "setup";
    create_cluster(&router, INSTANT_CLUSTER, case).await;
    register_capacity(&router, INSTANT_CLUSTER, case).await;

    // A task-definition revision, a task of it, and a service over a revision of its own:
    // three records that each carry an instant, each read through the call that wrote it
    // and through the read that answers it afterwards.
    let registered = ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": "instantfamily",
            "requiresCompatibilities": ["FARGATE"],
            "networkMode": "awsvpc",
            "cpu": "256",
            "memory": "512",
            "containerDefinitions": [container("app", IMAGE)],
        }),
        case,
    )
    .await;
    let placed = ok(
        &router,
        "RunTask",
        json!({
            "cluster": INSTANT_CLUSTER,
            "taskDefinition": "instantfamily",
            "launchType": "FARGATE",
            "networkConfiguration": network_configuration(AWSVPC_SUBNET),
        }),
        case,
    )
    .await;
    let task = list(&placed, "tasks", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected one placed task, got {placed}"));
    let task_arn = string(&task, "taskArn", case);
    let read = describe(&router, INSTANT_CLUSTER, &task_arn, case).await;
    register_definition(&router, "instantservicefamily", case).await;
    let created = ok(
        &router,
        "CreateService",
        json!({
            "cluster": INSTANT_CLUSTER,
            "serviceName": "instantservice",
            "taskDefinition": "instantservicefamily",
            "desiredCount": 1,
            "launchType": "EC2",
        }),
        case,
    )
    .await;
    let described = ok(
        &router,
        "DescribeServices",
        json!({ "cluster": INSTANT_CLUSTER, "services": ["instantservice"] }),
        case,
    )
    .await;
    let described_service = list(&described, "services", case)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: expected the service back, got {described}"));
    let stopped = ok(
        &router,
        "StopTask",
        json!({
            "cluster": INSTANT_CLUSTER,
            "task": task_arn,
            "reason": "the record this row reads",
        }),
        case,
    )
    .await;

    // The rows span the operations and the members that carry an instant.
    let rows = [
        (
            "RegisterTaskDefinition",
            "$.taskDefinition.registeredAt",
            registered["taskDefinition"]["registeredAt"].clone(),
        ),
        ("RunTask", "$.tasks[0].createdAt", task["createdAt"].clone()),
        (
            "DescribeTasks",
            "$.tasks[0].createdAt",
            read["createdAt"].clone(),
        ),
        (
            "StopTask",
            "$.task.stoppingAt",
            stopped["task"]["stoppingAt"].clone(),
        ),
        (
            "CreateService",
            "$.service.createdAt",
            created["service"]["createdAt"].clone(),
        ),
        (
            "CreateService",
            "$.service.deployments[0].createdAt",
            list(&created["service"], "deployments", case)[0]["createdAt"].clone(),
        ),
        (
            "CreateService",
            "$.service.deployments[0].updatedAt",
            list(&created["service"], "deployments", case)[0]["updatedAt"].clone(),
        ),
        (
            "DescribeServices",
            "$.services[0].createdAt",
            described_service["createdAt"].clone(),
        ),
    ];

    for (operation, member, reported) in rows {
        assert!(
            !reported.is_null(),
            "case {operation:?}: expected {member} on the record, got {reported}"
        );
        assert_millisecond_resolution(operation, member, &reported);
    }
}
