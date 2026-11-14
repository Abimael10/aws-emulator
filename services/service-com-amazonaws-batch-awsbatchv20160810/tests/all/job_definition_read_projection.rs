// SPDX-License-Identifier: CC0-1.0

//! How a REGISTERED job definition comes back on a read.
//!
//! Every case drives real HTTP through the router, so what is asserted is the wire
//! document a `batch` SDK client sees. The rules under test:
//!
//! * a described job definition MATERIALISES its collections: the definition's own maps
//!   (`parameters`, `tags`), every list member of each `ContainerProperties` object it
//!   carries, and the collections of the structures that object nests (a log
//!   configuration's `options` and `secretOptions`, Linux parameters' `devices` and
//!   `tmpfs`) come back present and EMPTY where the register supplied nothing for them,
//!   never as a missing member — so a client diffing a revision against desired state
//!   never sees a collection move between `[]` and absent, and one indexing a collection
//!   needs no null guard;
//! * a definition that runs on Fargate is stored carrying the platform version its
//!   containers will run on, and reports it: the one the request named, or the documented
//!   default when it named none. A definition that runs on Amazon EC2 reports none, since
//!   the member is one such a job "must not specify";
//! * a describe changes nothing a later describe sees.

use serde_json::{json, Value};

use crate::job_lifecycle::Batch;

/// Every list member `ContainerProperties` declares, and therefore every member a read of
/// a container job definition materialises. Source: the model's own shape
/// (`smithy_query(query="shape", shape="ContainerProperties")`) — the seven members whose
/// target is a list.
const CONTAINER_LIST_MEMBERS: &[&str] = &[
    "command",
    "volumes",
    "environment",
    "mountPoints",
    "ulimits",
    "resourceRequirements",
    "secrets",
];

/// The platform version AWS Batch applies to a Fargate container that names none. Source:
/// `FargatePlatformConfiguration$platformVersion`: "If one isn't specified, the `LATEST`
/// platform version is used by default".
const DEFAULT_PLATFORM_VERSION: &str = "LATEST";

/// An image every row below registers, so no row turns on which image it names.
const IMAGE: &str = "public.ecr.aws/amazonlinux/amazonlinux";

/// One row of the materialisation table: the container-properties members the register
/// supplies beside the image, and the `parameters` map it supplies, if any.
struct Row {
    case: &'static str,
    container: Value,
    parameters: Option<Value>,
}

/// The rows, differing along WHICH collections the register supplies (none, one, several,
/// all seven), along the SIZE of a supplied one (a supplied empty list, a one-entry list, a
/// two-entry list) and along whether a parameter map is supplied at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "nothing but the image",
            container: json!({}),
            parameters: None,
        },
        Row {
            case: "a command only",
            container: json!({"command": ["/bin/true"]}),
            parameters: None,
        },
        Row {
            case: "a command and parameters",
            container: json!({"command": ["echo", "Ref::greeting"]}),
            parameters: Some(json!({"greeting": "hello"})),
        },
        Row {
            case: "an environment and ulimits",
            container: json!({
                "environment": [{"name": "LANG", "value": "C"}],
                "ulimits": [{"name": "nofile", "hardLimit": 1024, "softLimit": 1024}],
            }),
            parameters: None,
        },
        Row {
            case: "a collection supplied already empty",
            container: json!({"environment": [], "command": []}),
            parameters: Some(json!({})),
        },
        Row {
            case: "every list member supplied",
            container: json!({
                "command": ["/bin/sh", "-c", "exit 0"],
                "volumes": [{"name": "scratch", "host": {"sourcePath": "/tmp"}}],
                "environment": [
                    {"name": "LANG", "value": "C"},
                    {"name": "TZ", "value": "UTC"},
                ],
                "mountPoints": [
                    {"containerPath": "/scratch", "sourceVolume": "scratch", "readOnly": true},
                ],
                "ulimits": [{"name": "nofile", "hardLimit": 2048, "softLimit": 1024}],
                "resourceRequirements": [
                    {"type": "VCPU", "value": "1"},
                    {"type": "MEMORY", "value": "512"},
                ],
                "secrets": [
                    {"name": "TOKEN", "valueFrom": "arn:aws:ssm:us-east-1:000000000000:parameter/token"},
                ],
            }),
            parameters: Some(json!({"greeting": "hello", "retries": "2"})),
        },
    ]
}

/// RULE: a read of a job definition reports its parameter and tag maps, and every list
/// member of each container-properties object it carries, as a present-and-empty map or
/// list where the register supplied nothing for them — and reports a supplied collection
/// exactly as it was supplied.
///
/// Each row is asserted in BOTH places a job definition carries container properties: the
/// top-level `containerProperties` and the `container` of a multi-node range, which is the
/// same shape and so owes the same document.
#[tokio::test]
async fn a_described_job_definition_materialises_its_collections() {
    let batch = Batch::new();
    for (index, row) in rows().iter().enumerate() {
        let mut container = row.container.clone();
        container["image"] = json!(IMAGE);

        // 1. The top-level containerProperties.
        let name = format!("jd-projection-container-{index}");
        let mut request = json!({
            "jobDefinitionName": &name,
            "type": "container",
            "containerProperties": container.clone(),
        });
        if let Some(parameters) = row.parameters.as_ref() {
            request["parameters"] = parameters.clone();
        }
        let described = register_and_describe(&batch, row.case, request, &name).await;
        let reported = described["containerProperties"].clone();
        assert_container_collections(row, "containerProperties", &reported);
        assert_definition_maps(row, "containerProperties", &described);

        // 2. The container of a multi-node range.
        let node_name = format!("jd-projection-node-{index}");
        let mut request = json!({
            "jobDefinitionName": &node_name,
            "type": "multinode",
            "nodeProperties": {
                "numNodes": 2,
                "mainNode": 0,
                "nodeRangeProperties": [{"targetNodes": "0:1", "container": container.clone()}],
            },
        });
        if let Some(parameters) = row.parameters.as_ref() {
            request["parameters"] = parameters.clone();
        }
        let described = register_and_describe(&batch, row.case, request, &node_name).await;
        let reported =
            described["nodeProperties"]["nodeRangeProperties"][0]["container"].clone();
        assert_container_collections(row, "a node range's container", &reported);
        assert_definition_maps(row, "a node range's container", &described);
    }
}

/// RULE: a job definition that runs on Fargate is stored carrying the platform version its
/// container will run on — the one the request named, or the documented default when it
/// named none — and a read reports it. One that runs on Amazon EC2 reports no platform
/// configuration at all.
#[tokio::test]
async fn a_fargate_container_reports_the_platform_version_it_will_run_on() {
    let batch = Batch::new();
    // (case, the members the register supplies beside the name and type, the
    // platformVersion the described container must report, or None for no configuration)
    let rows: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "an EC2 definition, by default",
            json!({"containerProperties": {"image": IMAGE}}),
            None,
        ),
        (
            "an EC2 definition, stated",
            json!({
                "platformCapabilities": ["EC2"],
                "containerProperties": {"image": IMAGE},
            }),
            None,
        ),
        (
            "a Fargate definition naming no platform version",
            json!({
                "platformCapabilities": ["FARGATE"],
                "containerProperties": fargate_container(json!({})),
            }),
            Some(DEFAULT_PLATFORM_VERSION),
        ),
        (
            "a Fargate definition whose platform configuration is empty",
            json!({
                "platformCapabilities": ["FARGATE"],
                "containerProperties": fargate_container(json!({
                    "fargatePlatformConfiguration": {},
                })),
            }),
            Some(DEFAULT_PLATFORM_VERSION),
        ),
        (
            "a Fargate definition naming a platform version",
            json!({
                "platformCapabilities": ["FARGATE"],
                "containerProperties": fargate_container(json!({
                    "fargatePlatformConfiguration": {"platformVersion": "1.4.0"},
                })),
            }),
            Some("1.4.0"),
        ),
        (
            "a Fargate definition naming an earlier platform version",
            json!({
                "platformCapabilities": ["FARGATE"],
                "containerProperties": fargate_container(json!({
                    "fargatePlatformConfiguration": {"platformVersion": "1.3.0"},
                })),
            }),
            Some("1.3.0"),
        ),
    ];

    for (index, (case, members, expected)) in rows.iter().enumerate() {
        let name = format!("jd-platform-{index}");
        let mut request = members.clone();
        request["jobDefinitionName"] = json!(&name);
        request["type"] = json!("container");
        let described = register_and_describe(&batch, case, request, &name).await;
        let configuration = &described["containerProperties"]["fargatePlatformConfiguration"];
        match expected {
            Some(version) => assert_eq!(
                configuration["platformVersion"].as_str(),
                Some(*version),
                "case {case:?}: expected platformVersion {version:?}, got {configuration}"
            ),
            None => assert!(
                configuration.is_null(),
                "case {case:?}: a job on Amazon EC2 must report no platform configuration, \
                 got {configuration}"
            ),
        }
    }
}

/// RULE: the same platform-version default binds to the other place a definition states
/// one — an `ecsProperties` task, whose `platformVersion` carries the same documented
/// default.
#[tokio::test]
async fn a_fargate_ecs_task_reports_the_platform_version_it_will_run_on() {
    let batch = Batch::new();
    // (case, the platform capabilities, the version the task names, the version reported)
    let rows: Vec<(&str, Value, Option<&str>, Option<&str>)> = vec![
        (
            "a Fargate task naming no platform version",
            json!(["FARGATE"]),
            None,
            Some(DEFAULT_PLATFORM_VERSION),
        ),
        (
            "a Fargate task naming a platform version",
            json!(["FARGATE"]),
            Some("1.4.0"),
            Some("1.4.0"),
        ),
        (
            "an EC2 task naming no platform version",
            json!(["EC2"]),
            None,
            None,
        ),
    ];

    for (index, (case, capabilities, named, expected)) in rows.iter().enumerate() {
        let name = format!("jd-ecs-platform-{index}");
        let mut task = json!({
            "executionRoleArn": "arn:aws:iam::000000000000:role/execution",
            "containers": [{
                "name": "main",
                "image": IMAGE,
                "resourceRequirements": [
                    {"type": "VCPU", "value": "0.25"},
                    {"type": "MEMORY", "value": "512"},
                ],
            }],
        });
        if let Some(version) = named {
            task["platformVersion"] = json!(version);
        }
        let request = json!({
            "jobDefinitionName": &name,
            "type": "container",
            "platformCapabilities": capabilities,
            "ecsProperties": {"taskProperties": [task]},
        });
        let described = register_and_describe(&batch, case, request, &name).await;
        let reported = &described["ecsProperties"]["taskProperties"][0]["platformVersion"];
        match expected {
            Some(version) => assert_eq!(
                reported.as_str(),
                Some(*version),
                "case {case:?}: expected platformVersion {version:?}, got {described}"
            ),
            None => assert!(
                reported.is_null(),
                "case {case:?}: a task on Amazon EC2 must report no platform version, \
                 got {reported}"
            ),
        }
    }
}

/// RULE: the collections of the structures a container-properties object NESTS are
/// materialised the same way — a log configuration reports its `options` map and its
/// `secretOptions` list — while a structure the register supplied nothing for stays absent,
/// because an absent structure is not an empty one.
#[tokio::test]
async fn a_described_container_materialises_its_log_configuration_collections() {
    let batch = Batch::new();
    let secret = json!([{
        "name": "TOKEN",
        "valueFrom": "arn:aws:ssm:us-east-1:000000000000:parameter/token",
    }]);
    // (case, the logConfiguration the register supplies, the options and secretOptions the
    // described container must report)
    let rows: Vec<(&str, Option<Value>, Value, Value)> = vec![
        (
            "no log configuration at all",
            None,
            Value::Null,
            Value::Null,
        ),
        (
            "a log driver only",
            Some(json!({"logDriver": "awslogs"})),
            json!({}),
            json!([]),
        ),
        (
            "a log driver and its options",
            Some(json!({
                "logDriver": "awslogs",
                "options": {"awslogs-group": "/aws/batch/job"},
            })),
            json!({"awslogs-group": "/aws/batch/job"}),
            json!([]),
        ),
        (
            "a log driver and a secret option",
            Some(json!({"logDriver": "splunk", "secretOptions": secret})),
            json!({}),
            secret.clone(),
        ),
        (
            "a log driver whose options are supplied already empty",
            Some(json!({"logDriver": "json-file", "options": {}, "secretOptions": []})),
            json!({}),
            json!([]),
        ),
    ];

    for (index, (case, logging, options, secret_options)) in rows.iter().enumerate() {
        let name = format!("jd-logging-{index}");
        let mut container = json!({"image": IMAGE});
        if let Some(logging) = logging.as_ref() {
            container["logConfiguration"] = logging.clone();
        }
        let request = json!({
            "jobDefinitionName": &name,
            "type": "container",
            "containerProperties": container,
        });
        let described = register_and_describe(&batch, case, request, &name).await;
        let reported = &described["containerProperties"]["logConfiguration"];
        if options.is_null() {
            assert!(
                reported.is_null(),
                "case {case:?}: a container with no log configuration must report none, \
                 got {reported}"
            );
            continue;
        }
        assert_eq!(
            &reported["options"], options,
            "case {case:?}: expected options {options}, got {reported}"
        );
        assert_eq!(
            &reported["secretOptions"], secret_options,
            "case {case:?}: expected secretOptions {secret_options}, got {reported}"
        );
    }
}

/// RULE: the same holds for the other structure a container nests collections in — its
/// Linux parameters report their `devices` and `tmpfs` lists, and each entry of those its
/// own list member.
#[tokio::test]
async fn a_described_container_materialises_its_linux_parameter_collections() {
    let batch = Batch::new();
    // (case, the linuxParameters the register supplies)
    let rows: Vec<(&str, Value)> = vec![
        ("nothing set", json!({})),
        ("one scalar set", json!({"sharedMemorySize": 64})),
        ("a device", json!({"devices": [{"hostPath": "/dev/null"}]})),
        (
            "a device naming its permissions",
            json!({"devices": [{"hostPath": "/dev/null", "permissions": ["READ"]}]}),
        ),
        (
            "a tmpfs",
            json!({"tmpfs": [{"containerPath": "/scratch", "size": 64}]}),
        ),
    ];

    for (index, (case, parameters)) in rows.iter().enumerate() {
        let name = format!("jd-linux-{index}");
        let request = json!({
            "jobDefinitionName": &name,
            "type": "container",
            "containerProperties": {"image": IMAGE, "linuxParameters": parameters},
        });
        let described = register_and_describe(&batch, case, request, &name).await;
        let reported = &described["containerProperties"]["linuxParameters"];
        for member in ["devices", "tmpfs"] {
            let expected = parameters.get(member).cloned().unwrap_or_else(|| json!([]));
            let actual = &reported[member];
            assert!(
                actual.is_array(),
                "case {case:?}: expected {member} to be reported as a list, got {reported}"
            );
            assert_eq!(
                actual.as_array().map(Vec::len),
                expected.as_array().map(Vec::len),
                "case {case:?}: expected {member} {expected}, got {actual}"
            );
        }
        for device in reported["devices"].as_array().cloned().unwrap_or_default() {
            assert!(
                device["permissions"].is_array(),
                "case {case:?}: every device reports its permissions list, got {device}"
            );
        }
        for mount in reported["tmpfs"].as_array().cloned().unwrap_or_default() {
            assert!(
                mount["mountOptions"].is_array(),
                "case {case:?}: every tmpfs reports its mountOptions list, got {mount}"
            );
        }
    }
}

/// A Fargate container: the image, the execution role Fargate requires and a supported
/// VCPU/MEMORY reservation, plus `members` merged over it.
fn fargate_container(members: Value) -> Value {
    let mut container = json!({
        "image": IMAGE,
        "executionRoleArn": "arn:aws:iam::000000000000:role/execution",
        "resourceRequirements": [
            {"type": "VCPU", "value": "0.25"},
            {"type": "MEMORY", "value": "512"},
        ],
    });
    for (member, value) in members.as_object().cloned().unwrap_or_default() {
        container[member] = value;
    }
    container
}

/// Register `request`, then read the revision back TWICE: the projection a read reports is
/// also one a later read reports unchanged, because a describe writes nothing.
async fn register_and_describe(batch: &Batch, case: &str, request: Value, name: &str) -> Value {
    let (status, body) = batch.call("/v1/registerjobdefinition", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the registration to be accepted, got {status} {body}"
    );
    let first = describe(batch, case, name).await;
    let again = describe(batch, case, name).await;
    assert_eq!(
        first, again,
        "case {case:?}: a describe must change nothing a later describe sees, got {first} \
         then {again}"
    );
    first
}

/// The one revision registered under `name`.
async fn describe(batch: &Batch, case: &str, name: &str) -> Value {
    let (status, body) = batch
        .call(
            "/v1/describejobdefinitions",
            json!({"jobDefinitionName": name}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the describe to answer 200, got {status} {body}"
    );
    let definitions = body["jobDefinitions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        definitions.len(),
        1,
        "case {case:?}: expected exactly one described revision, got {body}"
    );
    definitions[0].clone()
}

/// Every list member of `container` is present, and holds what the row supplied — or the
/// empty list where the row supplied nothing.
fn assert_container_collections(row: &Row, place: &str, container: &Value) {
    let case = row.case;
    for member in CONTAINER_LIST_MEMBERS {
        let expected = row
            .container
            .get(member)
            .cloned()
            .unwrap_or_else(|| json!([]));
        let actual = &container[*member];
        assert_eq!(
            actual, &expected,
            "case {case:?} in {place}: expected {member} {expected}, got {actual} \
             (the described container is {container})"
        );
    }
}

/// The definition's own maps are present: `parameters` as supplied or empty, and `tags`
/// empty for a revision registered with none.
fn assert_definition_maps(row: &Row, place: &str, definition: &Value) {
    let case = row.case;
    let expected = row.parameters.clone().unwrap_or_else(|| json!({}));
    let actual = &definition["parameters"];
    assert_eq!(
        actual, &expected,
        "case {case:?} in {place}: expected parameters {expected}, got {actual} \
         (the described definition is {definition})"
    );
    let tags = &definition["tags"];
    assert_eq!(
        tags,
        &json!({}),
        "case {case:?} in {place}: a revision registered with no tags reports the empty \
         map, got {tags}"
    );
}
