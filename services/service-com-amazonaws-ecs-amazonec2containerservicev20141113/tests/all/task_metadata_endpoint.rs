// SPDX-License-Identifier: CC0-1.0

//! The TASK METADATA ENDPOINT a container reads what it is at, driven end to end over HTTP.
//!
//! # The rule under test
//!
//! > A container of a task Amazon ECS launched carries the address of the task metadata
//! > endpoint in its environment, under `ECS_CONTAINER_METADATA_URI_V4`, and a program inside
//! > that container reads its OWN container's metadata at that address and its TASK's metadata
//! > — the task and every container in it — under `task` beneath it. Both documents are
//! > computed from the task's own records, so every value in them is one a client reads back
//! > through `DescribeTasks`; a path the endpoint holds no document for is answered `404`.
//!
//! # What the rows span
//!
//! WHICH container of a two-container task reads (each reads its own document and the one
//! task document), WHICH client the program asks with (`wget` writing the document to its own
//! output, and `curl`), WHICH document is asked for (the container's, the task's, and a path
//! under the endpoint it holds none of) and WHICH member of each document is asserted — every
//! one of them against what `DescribeTasks` reports for the same task.
//!
//! Nothing is asserted off the store: the containers' programs run inside the emulator, their
//! output is read back out of CloudWatch Logs' own store through that service's own
//! `GetLogEvents`, and every value compared against is read off Amazon ECS's own
//! `DescribeTasks`.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecs_wire::{cluster_with_capacity, list, logs, ok, poll_until, string, SIGNED_REGION};

/// The cluster the task of this suite runs in.
const CLUSTER: &str = "metadataendpoint";

/// The task-definition family it runs, with TWO containers so each one's own document is read
/// beside the one task document.
const FAMILY: &str = "metadataendpointfamily";

/// The two containers that family declares, each running the same program: what differs
/// between their readings is the container the program is inside.
const FIRST_CONTAINER: &str = "app";
const SECOND_CONTAINER: &str = "sidecar";

/// The image they start from: one this emulator holds no image of, so what the rows observe is
/// each container's own program.
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux:2023";

/// The log group the containers deliver their output to, and the stream prefix the `awslogs`
/// driver names their streams under.
const LOG_GROUP: &str = "/emulator/ecs/metadata";
const STREAM_PREFIX: &str = "meta";

/// What the task definition reserves: the task's own size, which the task document reports as
/// the vCPUs and mebibytes it comes to, and each container's own memory limit.
const TASK_CPU_UNITS: &str = "256";
const TASK_MEMORY_MIB: i64 = 512;
const CONTAINER_MEMORY_MIB: i64 = 128;

/// The cpu units ONE of the two containers reserves, so the rows span a container that
/// reserves some and one that reserves none — which carries no CPU limit at all.
const SECOND_CONTAINER_CPU_UNITS: i64 = 64;

/// The vCPUs [`TASK_CPU_UNITS`] stands for: "1024 CPU units is the equivalent of 1 vCPU"
/// (`TaskDefinition$cpu`).
const TASK_VCPUS: f64 = 0.25;

/// The address the endpoint is served at inside every task, as the four octets of the
/// link-local address the ECS developer guide's task metadata endpoint version 4 names, and the
/// version segment of the paths it answers. Written here rather than imported, so a wrong
/// address in the implementation cannot make this pass — and held as octets, the way this
/// emulator holds every link-local address it serves rather than dials.
const METADATA_HOST_OCTETS: [u8; 4] = [169, 254, 170, 2];
const METADATA_VERSION_SEGMENT: &str = "v4";

/// Those two as the start of every address this endpoint answers at.
fn metadata_uri_prefix() -> String {
    format!(
        "http://{}/{METADATA_VERSION_SEGMENT}/",
        std::net::Ipv4Addr::from(METADATA_HOST_OCTETS)
    )
}

/// The variable Amazon ECS injects that address into.
const METADATA_URI_VARIABLE: &str = "ECS_CONTAINER_METADATA_URI_V4";

/// The prefixes the containers' program writes each reading under.
const URI_LINE: &str = "uri=";
const SELF_DOCUMENT_LINE: &str = "self=";
const SELF_STATUS_LINE: &str = "self-status=";
const TASK_DOCUMENT_LINE: &str = "task=";
const TASK_STATUS_LINE: &str = "task-status=";
const MISSING_CURL_LINE: &str = "missing-curl=";
const MISSING_WGET_STATUS_LINE: &str = "missing-wget-status=";
const DONE_LINE: &str = "done";

/// The status both clients exit with for a transfer that was answered.
const ANSWERED: &str = "0";

/// The status `wget` exits with for a response it reports as an error: "8   Server issued an
/// error response" (the GNU Wget manual's exit-status list), which is what that client makes of
/// the `404` a path the endpoint holds no document for is answered with.
const WGET_SERVER_ERROR: &str = "8";

/// The status that same path is answered with, as the write-out format reports it.
const NOT_FOUND: &str = "404";

/// A router carrying Amazon ECS and CloudWatch Logs over ONE shared engine — the two services
/// a container's output travels through, mounted the way the production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ecs_amazonec2containerservicev20141113::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// The program every container of the task runs: it writes the address its environment carries,
/// then reads its own document and its task's document at that address — one with each client —
/// and then asks for a path under the endpoint that holds no document at all, with each client
/// in turn. Each reading is followed by the status the client exited with, so what a row
/// asserts is the client's own account of the request.
///
/// It ends by staying up, so the task is still RUNNING while the rows read the lines back —
/// what the documents report about a running container is what a running container reads.
fn reading_program() -> String {
    [
        format!("echo {URI_LINE}${METADATA_URI_VARIABLE}"),
        format!(
            "printf '{SELF_DOCUMENT_LINE}'; wget -qO- ${METADATA_URI_VARIABLE}; S=$?; echo; \
             echo {SELF_STATUS_LINE}$S"
        ),
        format!(
            "printf '{TASK_DOCUMENT_LINE}'; curl -s ${METADATA_URI_VARIABLE}/task; S=$?; echo; \
             echo {TASK_STATUS_LINE}$S"
        ),
        format!(
            "curl -s -o /dev/null -w '{MISSING_CURL_LINE}%{{http_code}}\\n' \
             ${METADATA_URI_VARIABLE}/stats"
        ),
        format!(
            "wget -qO- ${METADATA_URI_VARIABLE}/stats; echo {MISSING_WGET_STATUS_LINE}$?"
        ),
        format!("echo {DONE_LINE}"),
        "sleep infinity".to_string(),
    ]
    .join("; ")
}

/// One container definition of [`FAMILY`]: the program above, delivering its output to
/// [`LOG_GROUP`] through the `awslogs` driver, reserving `cpu_units` where it reserves any.
fn container(name: &str, cpu_units: Option<i64>) -> Value {
    let mut definition = json!({
        "name": name,
        "image": IMAGE,
        "memory": CONTAINER_MEMORY_MIB,
        "command": ["sh", "-c", reading_program()],
        "logConfiguration": {
            "logDriver": "awslogs",
            "options": {
                "awslogs-group": LOG_GROUP,
                "awslogs-region": SIGNED_REGION,
                "awslogs-stream-prefix": STREAM_PREFIX,
            },
        },
    });
    if let Some(units) = cpu_units {
        definition["cpu"] = json!(units);
    }
    definition
}

/// The `Limits` the document of the container `name` has to report: its memory limit, beside
/// the cpu units it reserves where it reserves any.
fn expected_container_limits(name: &str) -> Value {
    if name == SECOND_CONTAINER {
        json!({ "CPU": SECOND_CONTAINER_CPU_UNITS, "Memory": CONTAINER_MEMORY_MIB })
    } else {
        json!({ "Memory": CONTAINER_MEMORY_MIB })
    }
}

/// The stream the driver delivers one container of one task to:
/// `<prefix>/<container name>/<task id>`.
fn stream_of(container_name: &str, task_arn: &str) -> String {
    let task_id = task_arn.rsplit('/').next().unwrap_or_default();
    format!("{STREAM_PREFIX}/{container_name}/{task_id}")
}

/// The lines CloudWatch Logs holds in `stream`, oldest first. A stream nothing has been
/// delivered to yet reads as no lines at all.
async fn delivered(router: &MultiServiceRouter, stream: &str) -> Vec<String> {
    let (status, body) = logs(
        router,
        "GetLogEvents",
        json!({
            "logGroupName": LOG_GROUP,
            "logStreamName": stream,
            "startFromHead": true,
        }),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["message"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The one line of `lines` that starts with `prefix`, past the prefix — and a failure naming
/// what was delivered instead when no line does.
fn line_after(lines: &[String], prefix: &str, case: &str) -> String {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(prefix))
        .map(str::to_string)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a line starting {prefix:?}, got {lines:?}")
        })
}

/// The document a line carries, parsed — and a failure naming the text when it is not one.
fn document(text: &str, case: &str) -> Value {
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("case {case:?}: expected a JSON document, got {text:?} ({error})"))
}

/// What one container's program wrote, waited for until its last line is among the lines
/// delivered: the output of a line is delivered as the line runs, so a case waits for the
/// CONDITION rather than for a span.
async fn readings(router: &MultiServiceRouter, task_arn: &str, container_name: &str, case: &str) -> Vec<String> {
    let stream = stream_of(container_name, task_arn);
    let settled = poll_until(
        case,
        "the container's program to deliver every reading it took to its own log stream",
        || async {
            json!({ "lines": delivered(router, &stream).await })
        },
        |body| {
            body["lines"]
                .as_array()
                .is_some_and(|lines| lines.iter().any(|line| line == DONE_LINE))
        },
    )
    .await;
    list(&settled, "lines", case)
        .iter()
        .filter_map(|line| line.as_str().map(str::to_string))
        .collect()
}

/// The task as `DescribeTasks` reports it once it is RUNNING with a runtime id on each of its
/// containers — the id the agent gives a container it created, which the endpoint's address
/// names it by.
async fn running_task(router: &MultiServiceRouter, task_arn: &str, case: &str) -> Value {
    let settled = poll_until(
        case,
        "the task to report RUNNING with a runtime id on each of its containers",
        || {
            ok(
                router,
                "DescribeTasks",
                json!({ "cluster": CLUSTER, "tasks": [task_arn] }),
                case,
            )
        },
        |body| {
            let task = &body["tasks"][0];
            task["lastStatus"].as_str() == Some("RUNNING")
                && task["containers"].as_array().is_some_and(|containers| {
                    containers.len() == 2
                        && containers
                            .iter()
                            .all(|container| container["runtimeId"].is_string())
                })
        },
    )
    .await;
    settled["tasks"][0].clone()
}

/// The container `DescribeTasks` reports under `name`.
fn described_container(task: &Value, name: &str, case: &str) -> Value {
    list(task, "containers", case)
        .into_iter()
        .find(|container| container["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("case {case:?}: expected the container {name:?} in {task}"))
}

#[tokio::test]
async fn a_container_reads_its_own_and_its_tasks_metadata_at_the_endpoint() {
    let router = emulator();
    let case = "a task whose containers read the metadata endpoint";
    let (status, body) = logs(
        &router,
        "CreateLogGroup",
        json!({ "logGroupName": LOG_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateLogGroup to answer 200, got {status} with {body}"
    );
    let cluster_arn = cluster_with_capacity(&router, CLUSTER, case).await;
    let registered = ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": FAMILY,
            "cpu": TASK_CPU_UNITS,
            "memory": TASK_MEMORY_MIB.to_string(),
            "containerDefinitions": [
                container(FIRST_CONTAINER, None),
                container(SECOND_CONTAINER, Some(SECOND_CONTAINER_CPU_UNITS)),
            ],
        }),
        case,
    )
    .await;
    let definition_arn = string(&registered["taskDefinition"], "taskDefinitionArn", case);
    let revision = definition_arn
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .to_string();
    let launched = ok(
        &router,
        "RunTask",
        json!({
            "cluster": CLUSTER,
            "taskDefinition": FAMILY,
            "launchType": "EC2",
            "count": 1,
        }),
        case,
    )
    .await;
    let task_arn = string(&list(&launched, "tasks", case)[0], "taskArn", case);
    let described = running_task(&router, &task_arn, case).await;

    // What the one TASK document has to report, whichever container read it: every value is
    // read off `DescribeTasks` rather than composed here.
    let mut task_documents: Vec<Value> = Vec::new();
    for container_name in [FIRST_CONTAINER, SECOND_CONTAINER] {
        let case = format!("{case}: the reading of the container {container_name:?}");
        let case = case.as_str();
        let lines = readings(&router, &task_arn, container_name, case).await;
        let described_container = described_container(&described, container_name, case);
        let runtime_id = string(&described_container, "runtimeId", case);

        // The address the container's environment carries: the endpoint, named by the id the
        // agent gave this container.
        let uri = line_after(&lines, URI_LINE, case);
        let expected_uri = format!("{}{runtime_id}", metadata_uri_prefix());
        assert_eq!(
            uri, expected_uri,
            "case {case:?}: expected the endpoint of this container, got {uri:?}"
        );

        // Its OWN document, read with the client that writes what it fetched to its own
        // output, and the status that client exited with.
        assert_eq!(
            line_after(&lines, SELF_STATUS_LINE, case),
            ANSWERED,
            "case {case:?}: expected the client to report the transfer answered, got {lines:?}"
        );
        let own = document(&line_after(&lines, SELF_DOCUMENT_LINE, case), case);
        for (member, expected) in [
            ("DockerId", json!(runtime_id)),
            ("Name", json!(container_name)),
            ("Image", json!(IMAGE)),
            ("ContainerARN", described_container["containerArn"].clone()),
            ("DesiredStatus", json!("RUNNING")),
            ("KnownStatus", json!("RUNNING")),
            ("Type", json!("NORMAL")),
            ("Limits", expected_container_limits(container_name)),
            ("LogDriver", json!("awslogs")),
        ] {
            assert_eq!(
                own[member], expected,
                "case {case:?}: expected the container's {member} to be {expected}, got {own}"
            );
        }
        // The labels the agent applies to a container it created name the task, its family and
        // the cluster the task runs in.
        for (label, expected) in [
            ("com.amazonaws.ecs.cluster", json!(cluster_arn)),
            ("com.amazonaws.ecs.container-name", json!(container_name)),
            ("com.amazonaws.ecs.task-arn", json!(task_arn)),
            ("com.amazonaws.ecs.task-definition-family", json!(FAMILY)),
            ("com.amazonaws.ecs.task-definition-version", json!(revision)),
        ] {
            assert_eq!(
                own["Labels"][label], expected,
                "case {case:?}: expected the label {label:?} to be {expected}, got {own}"
            );
        }

        // Its TASK's document, read with the other client: the task, and every container in it.
        assert_eq!(
            line_after(&lines, TASK_STATUS_LINE, case),
            ANSWERED,
            "case {case:?}: expected the client to report the transfer answered, got {lines:?}"
        );
        let task = document(&line_after(&lines, TASK_DOCUMENT_LINE, case), case);
        for (member, expected) in [
            ("Cluster", json!(cluster_arn)),
            ("TaskARN", json!(task_arn)),
            ("Family", json!(FAMILY)),
            ("Revision", json!(revision)),
            ("DesiredStatus", json!("RUNNING")),
            ("KnownStatus", json!("RUNNING")),
            ("LaunchType", json!("EC2")),
            (
                "Limits",
                json!({ "CPU": TASK_VCPUS, "Memory": TASK_MEMORY_MIB }),
            ),
        ] {
            assert_eq!(
                task[member], expected,
                "case {case:?}: expected the task's {member} to be {expected}, got {task}"
            );
        }
        let names: Vec<Value> = list(&task, "Containers", case)
            .iter()
            .map(|container| container["Name"].clone())
            .collect();
        assert_eq!(
            names,
            vec![json!(FIRST_CONTAINER), json!(SECOND_CONTAINER)],
            "case {case:?}: expected the task document to carry every container of the task, \
             got {task}"
        );
        task_documents.push(task);

        // A path under the endpoint it holds no document for: answered as a document that is
        // not there, which each client reports in its own way.
        assert_eq!(
            line_after(&lines, MISSING_CURL_LINE, case),
            NOT_FOUND,
            "case {case:?}: expected the status of a path the endpoint holds none of, got \
             {lines:?}"
        );
        assert_eq!(
            line_after(&lines, MISSING_WGET_STATUS_LINE, case),
            WGET_SERVER_ERROR,
            "case {case:?}: expected the downloader to report the error response, got {lines:?}"
        );
    }

    // The two containers read ONE task document — the task is the same task from inside either
    // of them — while each read a container document of its own.
    assert_eq!(
        task_documents[0], task_documents[1],
        "expected both containers of the task to read the same task document"
    );
}

/// The cluster and family of the probe case below, kept apart from the one above so neither
/// case's task is placed beside the other's.
const PROBE_CLUSTER: &str = "metadataendpointprobes";
const PROBE_FAMILY: &str = "metadataendpointprobefamily";

/// The two containers it declares: one whose probe asks the endpoint for a document it serves,
/// and one whose probe asks for a path it holds none of.
const SERVED_PROBE_CONTAINER: &str = "serves";
const MISSING_PROBE_CONTAINER: &str = "missing";

/// The interval the probes run at: the smallest `HealthCheck$interval` the member documents
/// ("You may specify between 5 and 300 seconds").
const PROBE_INTERVAL_SECONDS: i64 = 5;

/// One container definition whose program stays up and whose health check runs `probe`.
fn probing_container(name: &str, probe: &str) -> Value {
    json!({
        "name": name,
        "image": IMAGE,
        "memory": CONTAINER_MEMORY_MIB,
        "command": ["sh", "-c", "sleep infinity"],
        "healthCheck": {
            "command": ["CMD-SHELL", probe],
            "interval": PROBE_INTERVAL_SECONDS,
            "retries": 1,
        },
    })
}

/// The `healthStatus` the container `name` of a described task reports.
fn health_status(task: &Value, name: &str, case: &str) -> Option<String> {
    described_container(task, name, case)["healthStatus"]
        .as_str()
        .map(str::to_string)
}

/// RULE, from the other side: the PROBE the agent runs inside a container is a session inside
/// that container too, so it reads the same endpoint — which is what a health check written as
/// a request to the task metadata endpoint turns on.
///
/// The rows span the probe's own outcome: one asking for a document the endpoint serves, which
/// its client exits zero on, and one asking for a path it holds none of, which the client
/// reports as an error response.
#[tokio::test]
async fn a_health_check_probe_reads_the_endpoint_from_inside_its_container() {
    let router = emulator();
    let case = "a task whose probes read the metadata endpoint";
    cluster_with_capacity(&router, PROBE_CLUSTER, case).await;
    ok(
        &router,
        "RegisterTaskDefinition",
        json!({
            "family": PROBE_FAMILY,
            "containerDefinitions": [
                probing_container(
                    SERVED_PROBE_CONTAINER,
                    &format!("wget -qO- ${METADATA_URI_VARIABLE}/task"),
                ),
                probing_container(
                    MISSING_PROBE_CONTAINER,
                    &format!("wget -qO- ${METADATA_URI_VARIABLE}/stats"),
                ),
            ],
        }),
        case,
    )
    .await;
    let launched = ok(
        &router,
        "RunTask",
        json!({
            "cluster": PROBE_CLUSTER,
            "taskDefinition": PROBE_FAMILY,
            "launchType": "EC2",
            "count": 1,
        }),
        case,
    )
    .await;
    let task_arn = string(&list(&launched, "tasks", case)[0], "taskArn", case);
    // (case, the container, the status its probe earns)
    let rows: &[(&str, &str, &str)] = &[
        (
            "a probe asking for a document the endpoint serves",
            SERVED_PROBE_CONTAINER,
            "HEALTHY",
        ),
        (
            "a probe asking for a path the endpoint holds none of",
            MISSING_PROBE_CONTAINER,
            "UNHEALTHY",
        ),
    ];
    let settled = poll_until(
        case,
        "every container of the task to report the verdict of its own probe",
        || {
            ok(
                &router,
                "DescribeTasks",
                json!({ "cluster": PROBE_CLUSTER, "tasks": [task_arn] }),
                case,
            )
        },
        |body| {
            let task = &body["tasks"][0];
            rows.iter().all(|(_, container, wanted)| {
                task["containers"].as_array().is_some_and(|containers| {
                    containers.iter().any(|held| {
                        held["name"].as_str() == Some(*container)
                            && held["healthStatus"].as_str() == Some(*wanted)
                    })
                })
            })
        },
    )
    .await;
    let task = settled["tasks"][0].clone();
    for (row, container, wanted) in rows {
        let reported = health_status(&task, container, row);
        assert_eq!(
            reported.as_deref(),
            Some(*wanted),
            "case {row:?}: expected the container {container:?} to report {wanted}, got {task}"
        );
    }
}
