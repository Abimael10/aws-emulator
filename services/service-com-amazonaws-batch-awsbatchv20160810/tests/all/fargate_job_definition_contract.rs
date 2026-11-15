// SPDX-License-Identifier: CC0-1.0

//! What a job definition that runs on FARGATE may state, and what AWS Batch applies to it
//! when the request states nothing — both end to end over the wire.
//!
//! Two rules are asserted here, each over a TABLE of inputs rather than the one case that
//! found the defect:
//!
//! * **a container that runs on Fargate may state no member the model documents as
//!   unsupported there.** `ContainerProperties$vcpus` and `ContainerProperties$memory` are
//!   "deprecated, use `resourceRequirements`" and "not supported for jobs running on
//!   Fargate resources"; `ContainerProperties$privileged`, `ContainerProperties$ulimits`,
//!   `ContainerProperties$instanceType`, `EcsTaskProperties$ipcMode` and
//!   `TaskContainerProperties$privileged` say the same of themselves. A `FARGATE`
//!   definition stating one is refused with `ClientException` (HTTP 400) and nothing is
//!   stored; the same properties on an EC2 definition, where they ARE applicable, are
//!   stored and read back.
//! * **the Fargate platform version is APPLIED when the request names none.** "A platform
//!   version is specified only for jobs that are running on Fargate resources. If one isn't
//!   specified, the `LATEST` platform version is used by default"
//!   (`FargatePlatformConfiguration$platformVersion`, and `EcsTaskProperties$platformVersion`
//!   in the same words), so a read of the registered revision reports the applied default —
//!   while an EC2 definition, which "must not specify this parameter", carries none.
//!
//! Every member name and every quoted sentence above comes from this service's own model
//! (`smithy_query(query="shape", shape="ContainerProperties")` and
//! `smithy_query(query="shape", shape="EcsTaskProperties,TaskContainerProperties")`).

use serde_json::{json, Value};

use crate::job_lifecycle::{error_code, error_message, Batch};

/// The wire error code every client-side refusal of this service carries.
const CLIENT_EXCEPTION: &str = "ClientException";

/// The envelope AWS Batch's frontend wraps `RegisterJobDefinition`'s refusals in, up to the
/// detail and the request id it ends with (`state::client_exception_message`).
const ENVELOPE_PREFIX: &str = "Error executing request, Exception : ";

/// An execution-role ARN a definition that runs on Fargate names: "for jobs that run on
/// Fargate resources, you must provide an execution role" (`executionRoleArn`).
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/ecsTaskExecutionRole";

/// A reservation inside the catalogue `ResourceRequirement$value` publishes for Fargate, so
/// each row below is judged only on the member it is about.
fn reservation() -> Value {
    json!([
        {"type": "VCPU", "value": "1"},
        {"type": "MEMORY", "value": "2048"},
    ])
}

/// The container properties of a Fargate job that satisfies every OTHER rule, with `extra`
/// merged over it.
fn fargate_container(extra: &Value) -> Value {
    let mut properties = json!({
        "image": "busybox",
        "executionRoleArn": EXECUTION_ROLE,
        "resourceRequirements": reservation(),
    });
    merge(&mut properties, extra);
    properties
}

/// Merge every member of `extra` into `target`.
fn merge(target: &mut Value, extra: &Value) {
    let members = extra.as_object().expect("the patch is an object").clone();
    let target = target.as_object_mut().expect("the target is an object");
    for (member, value) in members {
        target.insert(member, value);
    }
}

/// Register `body`; answer the status and the decoded response.
async fn register(batch: &Batch, body: Value) -> (u16, Value) {
    batch.call("/v1/registerjobdefinition", body).await
}

/// Assert `body` is refused with `ClientException` (HTTP 400) carrying `detail`, and that
/// nothing was stored under `name`.
async fn refused(batch: &Batch, case: &str, name: &str, body: Value, detail: &str) {
    let (status, answered) = register(batch, body).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {answered}"
    );
    assert_eq!(
        error_code(&answered),
        CLIENT_EXCEPTION,
        "case {case:?}: expected the modelled {CLIENT_EXCEPTION}, got {answered}"
    );
    let message = error_message(&answered);
    let expected = format!("{ENVELOPE_PREFIX}{detail}, RequestId: ");
    assert!(
        message.starts_with(&expected),
        "case {case:?}: expected the message to start {expected:?}, got {message:?}"
    );
    let described = describe(batch, name).await;
    assert_eq!(
        described.as_array().map(Vec::len),
        Some(0),
        "case {case:?}: a refused registration stores nothing, got {described}"
    );
}

/// Assert `body` is accepted, and answer the revision it was stored as.
async fn accepted(batch: &Batch, case: &str, body: Value) -> i64 {
    let (status, answered) = register(batch, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {answered}"
    );
    answered["revision"].as_i64().unwrap_or_default()
}

/// The revisions `DescribeJobDefinitions` reports for `name`.
async fn describe(batch: &Batch, name: &str) -> Value {
    let (status, described) = batch
        .call(
            "/v1/describejobdefinitions",
            json!({"jobDefinitionName": name}),
        )
        .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from DescribeJobDefinitions for {name:?}, got {described}"
    );
    described["jobDefinitions"].clone()
}

/// The container members the model documents as unsupported on Fargate, one row per
/// member, with the detail the refusal states.
///
/// Source: each member's own documentation in `ContainerProperties`
/// (`smithy_query(query="shape", shape="ContainerProperties")`). The `privileged` row
/// carrying `false` and the empty `ulimits` row are the two cases that doc admits
/// ("shouldn't be provided, or specified as false"), so they are accepted.
///
/// (case, the members merged into the container properties, the refusal detail or `None`)
const CONTAINER_MEMBERS: &[(&str, &str, Option<&str>)] = &[
    (
        "the deprecated vcpus reservation",
        r#"{"vcpus": 1}"#,
        Some("vcpus is not applicable for Fargate. Use resource requirements instead."),
    ),
    (
        "the deprecated memory reservation",
        r#"{"memory": 2048}"#,
        Some("memory is not applicable for Fargate. Use resource requirements instead."),
    ),
    (
        "both deprecated reservations at once",
        r#"{"vcpus": 4, "memory": 8192}"#,
        Some("vcpus is not applicable for Fargate. Use resource requirements instead."),
    ),
    (
        "elevated privileges",
        r#"{"privileged": true}"#,
        Some("privileged is not applicable for Fargate."),
    ),
    (
        "a ulimit",
        r#"{"ulimits": [{"name": "nofile", "softLimit": 1024, "hardLimit": 65535}]}"#,
        Some("ulimits is not applicable for Fargate."),
    ),
    (
        "an EC2 instance type",
        r#"{"instanceType": "m5.large"}"#,
        Some("instanceType is not applicable for Fargate."),
    ),
    ("privileged stated as false", r#"{"privileged": false}"#, None),
    ("an empty ulimits list", r#"{"ulimits": []}"#, None),
    ("nothing but the supported members", r#"{}"#, None),
];

/// RULE: a job definition whose `platformCapabilities` include `FARGATE` may state no
/// container-properties member the model documents as unsupported on Fargate; one that does
/// is refused with `ClientException` (HTTP 400) and nothing is registered.
#[tokio::test]
async fn a_fargate_definition_refuses_the_container_members_fargate_does_not_support() {
    let batch = Batch::new();

    for (index, (case, patch, detail)) in CONTAINER_MEMBERS.iter().enumerate() {
        let extra: Value = serde_json::from_str(patch).expect("the patch is JSON");
        let name = format!("jd-fargate-member-{index}");
        let body = json!({
            "jobDefinitionName": &name,
            "type": "container",
            "platformCapabilities": ["FARGATE"],
            "containerProperties": fargate_container(&extra),
        });
        match detail {
            Some(detail) => refused(&batch, case, &name, body, detail).await,
            None => {
                let revision = accepted(&batch, case, body).await;
                assert_eq!(
                    revision, 1,
                    "case {case:?}: the first registration of a name is revision 1, got \
                     {revision}"
                );
            }
        }
    }
}

/// RULE: the same members are applicable to a job that runs on Amazon EC2 resources, so the
/// rule above is Fargate's alone: a multi-node EC2 definition — the kind `instanceType` and
/// the deprecated reservations document themselves for — is registered with them and reads
/// them back.
#[tokio::test]
async fn the_same_members_are_accepted_on_an_ec2_definition() {
    let batch = Batch::new();

    for (index, (case, patch, _)) in CONTAINER_MEMBERS.iter().enumerate() {
        let extra: Value = serde_json::from_str(patch).expect("the patch is JSON");
        let mut container = json!({"image": "busybox"});
        merge(&mut container, &extra);
        let name = format!("jd-ec2-member-{index}");
        accepted(
            &batch,
            case,
            json!({
                "jobDefinitionName": &name,
                "type": "multinode",
                "nodeProperties": {
                    "numNodes": 2,
                    "mainNode": 0,
                    "nodeRangeProperties": [
                        {"targetNodes": "0:1", "container": container.clone()}
                    ],
                },
            }),
        )
        .await;
        let described = describe(&batch, &name).await;
        let stored = &described[0]["nodeProperties"]["nodeRangeProperties"][0]["container"];
        for (member, value) in extra.as_object().expect("the patch is an object") {
            assert_eq!(
                &stored[member], value,
                "case {case:?}: the stored revision reports {member} as registered, got \
                 {described}"
            );
        }
    }
}

/// The `ecsProperties` members the model documents as unsupported on Fargate: the task's
/// `ipcMode` ("not supported for jobs that run on Fargate resources") and a task
/// container's `privileged` ("not supported for Windows containers or tasks run on
/// Fargate").
///
/// (case, the members merged into the task, the members merged into its container, the
/// refusal detail or `None`)
const ECS_MEMBERS: &[(&str, &str, &str, Option<&str>)] = &[
    (
        "a task IPC namespace",
        r#"{"ipcMode": "host"}"#,
        r#"{}"#,
        Some("ipcMode is not applicable for Fargate."),
    ),
    (
        "a privileged task container",
        r#"{}"#,
        r#"{"privileged": true}"#,
        Some("privileged is not applicable for Fargate."),
    ),
    (
        "a task container privileged as false",
        r#"{}"#,
        r#"{"privileged": false}"#,
        None,
    ),
    ("a task stating neither", r#"{}"#, r#"{}"#, None),
];

/// RULE: the same rule binds to the container-properties objects an `ecsProperties`
/// definition carries, not only to the top-level `containerProperties`.
#[tokio::test]
async fn a_fargate_definition_refuses_the_ecs_task_members_fargate_does_not_support() {
    let batch = Batch::new();

    for (index, (case, task_patch, container_patch, detail)) in ECS_MEMBERS.iter().enumerate() {
        let task_extra: Value = serde_json::from_str(task_patch).expect("the patch is JSON");
        let container_extra: Value =
            serde_json::from_str(container_patch).expect("the patch is JSON");
        let mut container = json!({
            "image": "busybox",
            "name": "main",
            "resourceRequirements": reservation(),
        });
        merge(&mut container, &container_extra);
        let mut task = json!({
            "executionRoleArn": EXECUTION_ROLE,
            "containers": [container],
        });
        merge(&mut task, &task_extra);
        let name = format!("jd-fargate-ecs-{index}");
        let body = json!({
            "jobDefinitionName": &name,
            "type": "container",
            "platformCapabilities": ["FARGATE"],
            "ecsProperties": {"taskProperties": [task]},
        });
        match detail {
            Some(detail) => refused(&batch, case, &name, body, detail).await,
            None => {
                accepted(&batch, case, body).await;
            }
        }
    }
}

/// RULE: a definition that runs on Fargate and names no platform version runs on the
/// `LATEST` one — "if one isn't specified, the `LATEST` platform version is used by
/// default" — and the registered revision carries that applied default, so a read reports
/// the version the job will run on. A version the request DOES name is kept as sent.
///
/// Rows differ along the form the request states: the member absent, the configuration
/// present but empty, and each of several explicit versions.
#[tokio::test]
async fn a_fargate_definition_carries_the_applied_platform_version() {
    let batch = Batch::new();
    // (case, the `fargatePlatformConfiguration` the request states, the version reported)
    let rows: &[(&str, Option<&str>, &str)] = &[
        ("no fargatePlatformConfiguration at all", None, "LATEST"),
        ("an empty configuration object", Some("{}"), "LATEST"),
        (
            "LATEST stated explicitly",
            Some(r#"{"platformVersion": "LATEST"}"#),
            "LATEST",
        ),
        (
            "a pinned platform version",
            Some(r#"{"platformVersion": "1.4.0"}"#),
            "1.4.0",
        ),
        (
            "an older pinned platform version",
            Some(r#"{"platformVersion": "1.3.0"}"#),
            "1.3.0",
        ),
    ];

    for (index, (case, configuration, expected)) in rows.iter().enumerate() {
        let mut extra = json!({});
        if let Some(configuration) = configuration {
            let value: Value = serde_json::from_str(configuration).expect("the patch is JSON");
            extra = json!({"fargatePlatformConfiguration": value});
        }
        let name = format!("jd-platform-version-{index}");
        accepted(
            &batch,
            case,
            json!({
                "jobDefinitionName": &name,
                "type": "container",
                "platformCapabilities": ["FARGATE"],
                "containerProperties": fargate_container(&extra),
            }),
        )
        .await;
        let described = describe(&batch, &name).await;
        let reported = &described[0]["containerProperties"]["fargatePlatformConfiguration"];
        assert_eq!(
            reported,
            &json!({"platformVersion": expected}),
            "case {case:?}: expected the platform version {expected:?} on the registered \
             revision, got {described}"
        );
    }
}

/// RULE: the same default is applied to every task of an `ecsProperties` definition that
/// runs on Fargate (`EcsTaskProperties$platformVersion` documents it in the same words),
/// and an EC2 definition — which "must not specify this parameter" — carries no platform
/// configuration at all.
#[tokio::test]
async fn the_platform_version_default_reaches_ecs_tasks_and_no_ec2_definition() {
    let batch = Batch::new();
    // (case, the `platformVersion` the task states, the version reported)
    let rows: &[(&str, Option<&str>, &str)] = &[
        ("a task naming no platform version", None, "LATEST"),
        ("a task pinning one", Some("1.4.0"), "1.4.0"),
    ];

    for (index, (case, stated, expected)) in rows.iter().enumerate() {
        let mut task = json!({
            "executionRoleArn": EXECUTION_ROLE,
            "containers": [{
                "image": "busybox",
                "name": "main",
                "resourceRequirements": reservation(),
            }],
        });
        if let Some(stated) = stated {
            merge(&mut task, &json!({"platformVersion": stated}));
        }
        let name = format!("jd-ecs-platform-version-{index}");
        accepted(
            &batch,
            case,
            json!({
                "jobDefinitionName": &name,
                "type": "container",
                "platformCapabilities": ["FARGATE"],
                "ecsProperties": {"taskProperties": [task]},
            }),
        )
        .await;
        let described = describe(&batch, &name).await;
        assert_eq!(
            described[0]["ecsProperties"]["taskProperties"][0]["platformVersion"].as_str(),
            Some(*expected),
            "case {case:?}: expected the task's platform version {expected:?}, got {described}"
        );
    }

    // An EC2 definition (the default when no `platformCapabilities` is stated) states no
    // platform version and gets none: the default is Fargate's.
    let name = "jd-ec2-platform-version";
    accepted(
        &batch,
        "an EC2 definition",
        json!({
            "jobDefinitionName": name,
            "type": "container",
            "containerProperties": {
                "image": "busybox",
                "resourceRequirements": reservation(),
            },
        }),
    )
    .await;
    let described = describe(&batch, name).await;
    assert_eq!(
        described[0]["containerProperties"]
            .get("fargatePlatformConfiguration")
            .unwrap_or(&Value::Null),
        &Value::Null,
        "an EC2 definition must not carry a Fargate platform configuration, got {described}"
    );
}
