// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the DMS migration-task slice this crate implements:
//! `CreateReplicationInstance`, `CreateEndpoint`, `CreateReplicationTask`,
//! `DescribeReplicationInstances`, `DescribeEndpoints`, `DescribeReplicationTasks`,
//! `StartReplicationTask`, `DeleteReplicationTask` and `DeleteEndpoint`.
//!
//! Every case drives the real wire path — `POST /` with
//! `X-Amz-Target: AmazonDMSv20160101.<Operation>` through the multi-service router — so
//! what is asserted is what a client sees, body and HTTP status included.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_databasemigrationservice_amazondmsv20160101::state::{
    ENDPOINT_ACTIVE, ENDPOINT_DELETING, INSTANCE_AVAILABLE, INSTANCE_DELETING, LOG_COMPONENTS,
    LOG_SEVERITY_DEFAULT, MAX_RECORDS_MAX, MAX_RECORDS_MIN, RESOURCE_IDENTIFIER_MAX_LEN,
    TRANSITION_DELAY,
};
use service_com_amazonaws_databasemigrationservice_amazondmsv20160101::EmulatedService;
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

/// The region every case signs for.
pub(crate) const REGION: &str = "us-east-1";

/// The `X-Amz-Target` prefix of this service, which is its Smithy service shape name.
const TARGET_PREFIX: &str = "AmazonDMSv20160101";

/// The account segment every ARN this emulator mints carries.
pub(crate) const ACCOUNT: &str = "000000000000";

/// A router carrying this service — and, through [`Emulator::with_network`], the owners of
/// the resources it resolves — over one shared engine.
///
/// Shared with the other suites in this target, so every case drives the same wire
/// path.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// This service mounted beside the OWNERS of the resources it resolves — Amazon EC2,
    /// which holds the account's subnets and the Region's Availability Zones, and IAM,
    /// which holds the roles an endpoint asks DMS to assume — on ONE shared engine, plus
    /// an SDK client for each over the router's own in-process callback: the very seam
    /// this service's own resolution reaches them through.
    ///
    /// Nothing in a suite using this reaches into either peer's store: a subnet exists
    /// only because EC2 answered for it, and a role only because IAM did.
    pub(crate) fn with_network() -> (Self, ::aws_sdk_ec2::Client, ::aws_sdk_iam::Client) {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        let callback = EmbeddedEmulator::from_router(router.clone());
        let ec2 = ::aws_sdk_ec2::Client::from_conf(
            ::aws_sdk_ec2::Config::builder()
                .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
                .region(::aws_sdk_ec2::config::Region::new(REGION.to_string()))
                .endpoint_url(format!("https://ec2.{REGION}.amazonaws.com"))
                .credentials_provider(callback.aws_credentials_provider())
                .http_client(callback.http_client())
                .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
                .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
                .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
                .stalled_stream_protection(
                    ::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled(),
                )
                .build(),
        );
        let iam = ::aws_sdk_iam::Client::from_conf(
            ::aws_sdk_iam::Config::builder()
                .behavior_version(::aws_sdk_iam::config::BehaviorVersion::latest())
                .region(::aws_sdk_iam::config::Region::new(REGION.to_string()))
                .endpoint_url(format!("https://iam.{REGION}.amazonaws.com"))
                .credentials_provider(callback.aws_credentials_provider())
                .http_client(callback.http_client())
                .retry_config(::aws_sdk_iam::config::retry::RetryConfig::disabled())
                .timeout_config(::aws_sdk_iam::config::timeout::TimeoutConfig::disabled())
                .identity_cache(::aws_sdk_iam::config::IdentityCache::no_cache())
                .stalled_stream_protection(
                    ::aws_sdk_iam::config::StalledStreamProtectionConfig::disabled(),
                )
                .build(),
        );
        (Self { router, engine }, ec2, iam)
    }

    /// The engine the service is mounted on, so a suite can seed a record for a
    /// resource whose own create operation is still a stub.
    pub(crate) fn engine(&self) -> &engine::InMemoryEngine {
        &self.engine
    }

    /// Call `operation` with `body`, signed for [`REGION`] so the service resolves the
    /// request's region the way it does in production (the SigV4 credential scope).
    pub(crate) async fn call(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/dms/\
                     aws4_request, SignedHeaders=host;x-amz-date, Signature=deadbeef"
                ),
            )
            .body(Bytes::from(body.to_string()))
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// Call `operation`, asserting a 200 and returning the response body.
    pub(crate) async fn ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, response) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {operation} to answer 200, got {status} {response}"
        );
        response
    }

    /// A replication instance that has finished provisioning, and its ARN.
    pub(crate) async fn available_instance(&self, identifier: &str) -> String {
        let created = self
            .ok(
                identifier,
                "CreateReplicationInstance",
                &json!({
                    "ReplicationInstanceIdentifier": identifier,
                    "ReplicationInstanceClass": "dms.t3.medium",
                }),
            )
            .await;
        wait_for_transition();
        text(&created, "/ReplicationInstance/ReplicationInstanceArn")
    }

    /// An endpoint of `endpoint_type`, and its ARN.
    pub(crate) async fn endpoint(
        &self,
        identifier: &str,
        endpoint_type: &str,
        engine_name: &str,
    ) -> String {
        let created = self
            .ok(
                identifier,
                "CreateEndpoint",
                &json!({
                    "EndpointIdentifier": identifier,
                    "EndpointType": endpoint_type,
                    "EngineName": engine_name,
                }),
            )
            .await;
        text(&created, "/Endpoint/EndpointArn")
    }
}

/// The `__type` an awsJson1_1 error body carries.
pub(crate) fn error_code(response: &Value) -> String {
    response
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .to_string()
}

/// The string at `pointer` in `value`, or a marker naming its absence.
pub(crate) fn text(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or("<missing>")
        .to_string()
}

/// Every string at `member` across the list at `pointer`.
fn column(value: &Value, pointer: &str, member: &str) -> Vec<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|records| {
            records
                .iter()
                .map(|record| {
                    record
                        .get(member)
                        .and_then(Value::as_str)
                        .unwrap_or("<missing>")
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Wait past a clock-owed transition's due instant.
fn wait_for_transition() {
    std::thread::sleep(TRANSITION_DELAY + Duration::from_millis(400));
}

/// A table-mappings document, which DMS takes as an opaque string.
fn table_mappings() -> String {
    json!({
        "rules": [{
            "rule-type": "selection",
            "rule-id": "1",
            "rule-name": "1",
            "object-locator": { "schema-name": "sales", "table-name": "%" },
            "rule-action": "include",
        }],
    })
    .to_string()
}

#[tokio::test]
async fn migration_task_slice_is_read_back_from_state() {
    let emulator = Emulator::new();

    // ---- CreateReplicationInstance -------------------------------------------------
    let created_instance = emulator
        .ok(
            "create instance",
            "CreateReplicationInstance",
            &json!({
                "ReplicationInstanceIdentifier": "My-Rep-Instance",
                "ReplicationInstanceClass": "dms.t3.medium",
                "AllocatedStorage": 100,
                "EngineVersion": "3.5.3",
                "MultiAZ": false,
                "PubliclyAccessible": false,
                "AvailabilityZone": "us-east-1d",
                "Tags": [{ "Key": "team", "Value": "migrations" }],
            }),
        )
        .await;
    let instance_arn = text(&created_instance, "/ReplicationInstance/ReplicationInstanceArn");
    let instance_prefix = format!("arn:aws:dms:{REGION}:{ACCOUNT}:rep:");
    assert!(
        instance_arn.starts_with(&instance_prefix),
        "case \"create instance\": expected an ARN under {instance_prefix}, got \
         {instance_arn:?}"
    );
    assert_eq!(
        text(
            &created_instance,
            "/ReplicationInstance/ReplicationInstanceIdentifier"
        ),
        "my-rep-instance",
        "case \"create instance\": expected the identifier stored lowercased, got \
         {created_instance}"
    );
    assert_eq!(
        text(
            &created_instance,
            "/ReplicationInstance/ReplicationInstanceStatus"
        ),
        "creating",
        "case \"create instance\": expected a freshly created instance to be creating, \
         got {created_instance}"
    );
    assert_eq!(
        created_instance
            .pointer("/ReplicationInstance/AllocatedStorage")
            .and_then(Value::as_i64),
        Some(100),
        "case \"create instance\": expected the requested storage, got {created_instance}"
    );
    assert!(
        created_instance
            .pointer("/ReplicationInstance/InstanceCreateTime")
            .is_some(),
        "case \"create instance\": expected an InstanceCreateTime, got {created_instance}"
    );

    // The describe answers from the same record the create wrote.
    let described = emulator
        .ok(
            "describe instance",
            "DescribeReplicationInstances",
            &json!({
                "Filters": [{
                    "Name": "replication-instance-id",
                    "Values": ["my-rep-instance"],
                }],
            }),
        )
        .await;
    assert_eq!(
        column(
            &described,
            "/ReplicationInstances",
            "ReplicationInstanceArn"
        ),
        vec![instance_arn.clone()],
        "case \"describe instance\": expected exactly the created instance, got {described}"
    );
    assert_eq!(
        text(&described, "/ReplicationInstances/0/EngineVersion"),
        "3.5.3",
        "case \"describe instance\": expected the requested engine version, got {described}"
    );
    assert_eq!(
        text(&described, "/ReplicationInstances/0/AvailabilityZone"),
        "us-east-1d",
        "case \"describe instance\": expected the requested AZ, got {described}"
    );

    // `creating` is not terminal: the clock owes `available`.
    wait_for_transition();
    let settled = emulator
        .ok(
            "instance settles",
            "DescribeReplicationInstances",
            &json!({}),
        )
        .await;
    assert_eq!(
        column(&settled, "/ReplicationInstances", "ReplicationInstanceStatus"),
        vec!["available".to_string()],
        "case \"instance settles\": expected the instance to reach available, got {settled}"
    );

    // ---- CreateEndpoint ------------------------------------------------------------
    let created_source = emulator
        .ok(
            "create source endpoint",
            "CreateEndpoint",
            &json!({
                "EndpointIdentifier": "src-ep",
                "EndpointType": "source",
                "EngineName": "mysql",
                "ServerName": "db1.example.internal",
                "Port": 3306,
                "DatabaseName": "sales",
                "Username": "migrator",
                "Password": "never-echoed",
            }),
        )
        .await;
    let source_arn = text(&created_source, "/Endpoint/EndpointArn");
    let endpoint_prefix = format!("arn:aws:dms:{REGION}:{ACCOUNT}:endpoint:");
    assert!(
        source_arn.starts_with(&endpoint_prefix),
        "case \"create source endpoint\": expected an ARN under {endpoint_prefix}, got \
         {source_arn:?}"
    );
    assert_eq!(
        text(&created_source, "/Endpoint/Status"),
        "active",
        "case \"create source endpoint\": expected an endpoint to be created active, got \
         {created_source}"
    );
    assert_eq!(
        text(&created_source, "/Endpoint/SslMode"),
        "none",
        "case \"create source endpoint\": expected the documented default SSL mode, got \
         {created_source}"
    );
    assert!(
        !created_source.to_string().contains("never-echoed"),
        "case \"create source endpoint\": the password must never be echoed, got \
         {created_source}"
    );

    let created_target = emulator
        .ok(
            "create target endpoint",
            "CreateEndpoint",
            &json!({
                "EndpointIdentifier": "tgt-ep",
                "EndpointType": "target",
                "EngineName": "postgres",
                "ServerName": "warehouse.example.internal",
                "Port": 5432,
                "SslMode": "require",
            }),
        )
        .await;
    let target_arn = text(&created_target, "/Endpoint/EndpointArn");
    assert_eq!(
        text(&created_target, "/Endpoint/SslMode"),
        "require",
        "case \"create target endpoint\": expected the requested SSL mode, got \
         {created_target}"
    );
    assert_ne!(
        source_arn, target_arn,
        "case \"create target endpoint\": two endpoints must not share an ARN"
    );

    let described_sources = emulator
        .ok(
            "describe source endpoints",
            "DescribeEndpoints",
            &json!({ "Filters": [{ "Name": "endpoint-type", "Values": ["source"] }] }),
        )
        .await;
    assert_eq!(
        column(&described_sources, "/Endpoints", "EndpointArn"),
        vec![source_arn.clone()],
        "case \"describe source endpoints\": expected only the source endpoint, got \
         {described_sources}"
    );
    assert_eq!(
        described_sources
            .pointer("/Endpoints/0/Port")
            .and_then(Value::as_i64),
        Some(3306),
        "case \"describe source endpoints\": expected the stored port, got \
         {described_sources}"
    );

    // ---- CreateReplicationTask -----------------------------------------------------
    let settings = json!({ "TargetMetadata": { "SupportLobs": true } }).to_string();
    let created_task = emulator
        .ok(
            "create task",
            "CreateReplicationTask",
            &json!({
                "ReplicationTaskIdentifier": "Nightly-Load",
                "SourceEndpointArn": source_arn,
                "TargetEndpointArn": target_arn,
                "ReplicationInstanceArn": instance_arn,
                "MigrationType": "full-load-and-cdc",
                "TableMappings": table_mappings(),
                "ReplicationTaskSettings": settings,
            }),
        )
        .await;
    let task_arn = text(&created_task, "/ReplicationTask/ReplicationTaskArn");
    let task_prefix = format!("arn:aws:dms:{REGION}:{ACCOUNT}:task:");
    assert!(
        task_arn.starts_with(&task_prefix),
        "case \"create task\": expected an ARN under {task_prefix}, got {task_arn:?}"
    );
    assert_eq!(
        text(&created_task, "/ReplicationTask/Status"),
        "creating",
        "case \"create task\": expected a freshly created task to be creating, got \
         {created_task}"
    );
    assert!(
        created_task
            .pointer("/ReplicationTask/ReplicationTaskCreationDate")
            .is_some(),
        "case \"create task\": expected a ReplicationTaskCreationDate, got {created_task}"
    );

    // A task that is still being created cannot be started.
    let (status, response) = emulator
        .call(
            "StartReplicationTask",
            &json!({
                "ReplicationTaskArn": task_arn,
                "StartReplicationTaskType": "start-replication",
            }),
        )
        .await;
    assert_eq!(
        (status, error_code(&response).contains("InvalidResourceStateFault")),
        (400, true),
        "case \"start a creating task\": expected 400 InvalidResourceStateFault, got \
         {status} {response}"
    );

    wait_for_transition();
    let described_tasks = emulator
        .ok("describe tasks", "DescribeReplicationTasks", &json!({}))
        .await;
    assert_eq!(
        column(&described_tasks, "/ReplicationTasks", "ReplicationTaskArn"),
        vec![task_arn.clone()],
        "case \"describe tasks\": expected exactly the created task, got {described_tasks}"
    );
    for (member, expected) in [
        ("ReplicationTaskIdentifier", "Nightly-Load".to_string()),
        ("SourceEndpointArn", source_arn.clone()),
        ("TargetEndpointArn", target_arn.clone()),
        ("ReplicationInstanceArn", instance_arn.clone()),
        ("MigrationType", "full-load-and-cdc".to_string()),
        ("TableMappings", table_mappings()),
        ("Status", "ready".to_string()),
    ] {
        assert_eq!(
            text(&described_tasks, &format!("/ReplicationTasks/0/{member}")),
            expected,
            "case \"describe tasks\": expected {member} to come back from state, got \
             {described_tasks}"
        );
    }

    // The settings the task runs with are the caller's resolved against the service
    // defaults: the member it named is its own, and the defaults it never mentioned are
    // there to read. The whole rule is `task_settings_are_resolved_against_the_defaults`.
    let reported_settings = settings_document(&described_tasks, "/ReplicationTasks/0");
    assert_eq!(
        (
            reported_settings.pointer("/TargetMetadata/SupportLobs").cloned(),
            reported_settings.pointer("/TargetMetadata/LobMaxSize").cloned(),
        ),
        (Some(Value::from(true)), Some(Value::from(32))),
        "case \"describe tasks\": expected the caller's setting over the defaults, got \
         {reported_settings}"
    );

    // `WithoutSettings` drops the settings and nothing else.
    let without_settings = emulator
        .ok(
            "describe tasks without settings",
            "DescribeReplicationTasks",
            &json!({ "WithoutSettings": true }),
        )
        .await;
    assert!(
        without_settings
            .pointer("/ReplicationTasks/0/ReplicationTaskSettings")
            .is_none(),
        "case \"describe tasks without settings\": expected no ReplicationTaskSettings, \
         got {without_settings}"
    );
    assert_eq!(
        text(&without_settings, "/ReplicationTasks/0/TableMappings"),
        table_mappings(),
        "case \"describe tasks without settings\": expected the table mappings to stay, \
         got {without_settings}"
    );

    // ---- StartReplicationTask ------------------------------------------------------
    let started = emulator
        .ok(
            "start task",
            "StartReplicationTask",
            &json!({
                "ReplicationTaskArn": task_arn,
                "StartReplicationTaskType": "start-replication",
            }),
        )
        .await;
    assert_eq!(
        text(&started, "/ReplicationTask/Status"),
        "starting",
        "case \"start task\": expected the task to move to starting, got {started}"
    );
    assert!(
        started
            .pointer("/ReplicationTask/ReplicationTaskStartDate")
            .is_some(),
        "case \"start task\": expected a ReplicationTaskStartDate, got {started}"
    );

    let after_start = emulator
        .ok(
            "describe after start",
            "DescribeReplicationTasks",
            &json!({ "Filters": [{ "Name": "replication-task-arn", "Values": [task_arn] }] }),
        )
        .await;
    assert_eq!(
        column(&after_start, "/ReplicationTasks", "Status"),
        vec!["starting".to_string()],
        "case \"describe after start\": expected the persisted status to be starting, got \
         {after_start}"
    );

    wait_for_transition();
    let running = emulator
        .ok("task settles", "DescribeReplicationTasks", &json!({}))
        .await;
    assert_eq!(
        column(&running, "/ReplicationTasks", "Status"),
        vec!["running".to_string()],
        "case \"task settles\": expected the task to reach running, got {running}"
    );

    // A running task cannot be deleted: it has to be stopped first.
    let (status, response) = emulator
        .call(
            "DeleteReplicationTask",
            &json!({ "ReplicationTaskArn": task_arn }),
        )
        .await;
    assert_eq!(
        (status, error_code(&response).contains("InvalidResourceStateFault")),
        (400, true),
        "case \"delete a running task\": expected 400 InvalidResourceStateFault, got \
         {status} {response}"
    );

    // ---- DeleteReplicationTask -----------------------------------------------------
    let second = emulator
        .ok(
            "create second task",
            "CreateReplicationTask",
            &json!({
                "ReplicationTaskIdentifier": "backfill-load",
                "SourceEndpointArn": source_arn,
                "TargetEndpointArn": target_arn,
                "ReplicationInstanceArn": instance_arn,
                "MigrationType": "full-load",
                "TableMappings": table_mappings(),
            }),
        )
        .await;
    let second_arn = text(&second, "/ReplicationTask/ReplicationTaskArn");

    let deleted = emulator
        .ok(
            "delete second task",
            "DeleteReplicationTask",
            &json!({ "ReplicationTaskArn": second_arn }),
        )
        .await;
    assert_eq!(
        (
            text(&deleted, "/ReplicationTask/ReplicationTaskArn"),
            text(&deleted, "/ReplicationTask/Status")
        ),
        (second_arn.clone(), "deleting".to_string()),
        "case \"delete second task\": expected the deleted task to come back deleting, got \
         {deleted}"
    );

    wait_for_transition();
    let remaining = emulator
        .ok("describe after delete", "DescribeReplicationTasks", &json!({}))
        .await;
    assert_eq!(
        column(&remaining, "/ReplicationTasks", "ReplicationTaskArn"),
        vec![task_arn.clone()],
        "case \"describe after delete\": expected only the running task to remain, got \
         {remaining}"
    );

    // The identifier is free again, and deleting the same ARN twice finds nothing.
    let (status, response) = emulator
        .call(
            "DeleteReplicationTask",
            &json!({ "ReplicationTaskArn": second_arn }),
        )
        .await;
    assert_eq!(
        (status, error_code(&response).contains("ResourceNotFoundFault")),
        (400, true),
        "case \"delete twice\": expected 400 ResourceNotFoundFault, got {status} {response}"
    );
}

#[tokio::test]
async fn an_arn_that_names_no_task_is_refused_as_not_found() {
    let emulator = Emulator::new();
    let source_arn = emulator.endpoint("src-ep", "source", "mysql").await;

    // Every ARN here is one the store does not hold: a well-formed task ARN nobody
    // created, the same resource id in another region, an ARN of another resource type,
    // and a string that is not an ARN at all.
    let cases = [
        (
            "well-formed but unknown",
            format!("arn:aws:dms:{REGION}:{ACCOUNT}:task:ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
        ),
        (
            "another region",
            format!("arn:aws:dms:eu-west-1:{ACCOUNT}:task:ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
        ),
        ("an endpoint ARN", source_arn),
        ("not an ARN", "nightly-load".to_string()),
    ];

    for (case, arn) in cases {
        for (operation, body) in [
            (
                "StartReplicationTask",
                json!({
                    "ReplicationTaskArn": arn,
                    "StartReplicationTaskType": "start-replication",
                }),
            ),
            (
                "DeleteReplicationTask",
                json!({ "ReplicationTaskArn": arn }),
            ),
        ] {
            let (status, response) = emulator.call(operation, &body).await;
            assert_eq!(
                (status, error_code(&response).contains("ResourceNotFoundFault")),
                (400, true),
                "case {case:?} on {operation}: expected 400 ResourceNotFoundFault, got \
                 {status} {response}"
            );
        }
    }
}

#[tokio::test]
async fn a_task_can_only_be_created_against_resources_that_exist() {
    let emulator = Emulator::new();
    let instance_arn = emulator.available_instance("task-deps").await;
    let source_arn = emulator.endpoint("src-ep", "source", "mysql").await;
    let target_arn = emulator.endpoint("tgt-ep", "target", "postgres").await;
    let unknown = format!("arn:aws:dms:{REGION}:{ACCOUNT}:endpoint:NOSUCHENDPOINT0000000000AA");
    let unknown_instance = format!("arn:aws:dms:{REGION}:{ACCOUNT}:rep:NOSUCHINSTANCE0000000000AA");

    // Each row breaks exactly one of the three ARN resolutions.
    let cases = [
        (
            "unknown source endpoint",
            unknown.clone(),
            target_arn.clone(),
            instance_arn.clone(),
            "ResourceNotFoundFault",
        ),
        (
            "unknown target endpoint",
            source_arn.clone(),
            unknown.clone(),
            instance_arn.clone(),
            "ResourceNotFoundFault",
        ),
        (
            "unknown replication instance",
            source_arn.clone(),
            target_arn.clone(),
            unknown_instance,
            "ResourceNotFoundFault",
        ),
        (
            "source and target swapped",
            target_arn.clone(),
            source_arn.clone(),
            instance_arn.clone(),
            "ValidationException",
        ),
    ];

    for (case, source, target, instance, expected) in cases {
        let (status, response) = emulator
            .call(
                "CreateReplicationTask",
                &json!({
                    "ReplicationTaskIdentifier": "nightly-load",
                    "SourceEndpointArn": source,
                    "TargetEndpointArn": target,
                    "ReplicationInstanceArn": instance,
                    "MigrationType": "full-load",
                    "TableMappings": table_mappings(),
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&response).contains(expected)),
            (400, true),
            "case {case:?}: expected 400 {expected}, got {status} {response}"
        );
    }

    // A task cannot be created on an instance that is still provisioning.
    let creating = emulator
        .ok(
            "instance still creating",
            "CreateReplicationInstance",
            &json!({
                "ReplicationInstanceIdentifier": "still-creating",
                "ReplicationInstanceClass": "dms.t3.medium",
            }),
        )
        .await;
    let creating_arn = text(&creating, "/ReplicationInstance/ReplicationInstanceArn");
    let (status, response) = emulator
        .call(
            "CreateReplicationTask",
            &json!({
                "ReplicationTaskIdentifier": "nightly-load",
                "SourceEndpointArn": source_arn,
                "TargetEndpointArn": target_arn,
                "ReplicationInstanceArn": creating_arn,
                "MigrationType": "full-load",
                "TableMappings": table_mappings(),
            }),
        )
        .await;
    assert_eq!(
        (status, error_code(&response).contains("InvalidResourceStateFault")),
        (400, true),
        "case \"instance still creating\": expected 400 InvalidResourceStateFault, got \
         {status} {response}"
    );
}

#[tokio::test]
async fn a_duplicate_identifier_is_refused_on_every_create() {
    let emulator = Emulator::new();
    let instance_arn = emulator.available_instance("rep-dup").await;
    let source_arn = emulator.endpoint("src-dup", "source", "mysql").await;
    let target_arn = emulator.endpoint("tgt-dup", "target", "postgres").await;
    emulator
        .ok(
            "first task",
            "CreateReplicationTask",
            &json!({
                "ReplicationTaskIdentifier": "task-dup",
                "SourceEndpointArn": source_arn,
                "TargetEndpointArn": target_arn,
                "ReplicationInstanceArn": instance_arn,
                "MigrationType": "cdc",
                "TableMappings": table_mappings(),
            }),
        )
        .await;

    // One row per resource kind this slice creates; the second create of the same
    // identifier is refused, and the first resource keeps its ARN.
    let cases = [
        (
            "replication instance",
            "CreateReplicationInstance",
            json!({
                "ReplicationInstanceIdentifier": "REP-DUP",
                "ReplicationInstanceClass": "dms.t3.large",
            }),
        ),
        (
            "endpoint",
            "CreateEndpoint",
            json!({
                "EndpointIdentifier": "src-dup",
                "EndpointType": "source",
                "EngineName": "oracle",
            }),
        ),
        (
            "replication task",
            "CreateReplicationTask",
            json!({
                "ReplicationTaskIdentifier": "task-dup",
                "SourceEndpointArn": source_arn,
                "TargetEndpointArn": target_arn,
                "ReplicationInstanceArn": instance_arn,
                "MigrationType": "cdc",
                "TableMappings": table_mappings(),
            }),
        ),
    ];

    for (case, operation, body) in cases {
        let (status, response) = emulator.call(operation, &body).await;
        assert_eq!(
            (
                status,
                error_code(&response).contains("ResourceAlreadyExistsFault")
            ),
            (400, true),
            "case {case:?}: expected 400 ResourceAlreadyExistsFault, got {status} {response}"
        );
    }

    // Nothing was added by the refused creates.
    for (pointer, operation) in [
        ("/ReplicationInstances", "DescribeReplicationInstances"),
        ("/Endpoints", "DescribeEndpoints"),
        ("/ReplicationTasks", "DescribeReplicationTasks"),
    ] {
        let described = emulator.ok("after duplicates", operation, &json!({})).await;
        let count = described
            .pointer(pointer)
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let expected = usize::from(operation == "DescribeEndpoints") + 1;
        assert_eq!(
            count, expected,
            "case \"after duplicates\": expected {operation} to report {expected} \
             resources, got {described}"
        );
    }
}

/// One identifier, and whether the documented identifier rule accepts it.
struct IdentifierCase {
    name: &'static str,
    identifier: String,
    valid: bool,
    /// Whether the row tests a length bound, which only the replication-instance and
    /// replication-task identifiers publish.
    length_bound: bool,
}

#[tokio::test]
async fn an_identifier_must_satisfy_the_documented_character_rule() {
    // The rule the `ReplicationInstanceIdentifier`, `EndpointIdentifier` and
    // `ReplicationTaskIdentifier` docs publish: ASCII letters, digits and hyphens only,
    // a letter first, no trailing hyphen and no two consecutive hyphens.
    let cases = vec![
        IdentifierCase {
            name: "lowercase word",
            identifier: "myrepinstance".to_string(),
            valid: true,
            length_bound: false,
        },
        IdentifierCase {
            name: "hyphenated",
            identifier: "sales-to-warehouse".to_string(),
            valid: true,
            length_bound: false,
        },
        IdentifierCase {
            name: "digits inside",
            identifier: "db2zos-7".to_string(),
            valid: true,
            length_bound: false,
        },
        IdentifierCase {
            name: "mixed case",
            identifier: "NightlyLoad".to_string(),
            valid: true,
            length_bound: false,
        },
        IdentifierCase {
            name: "single letter",
            identifier: "a".to_string(),
            valid: true,
            length_bound: false,
        },
        IdentifierCase {
            name: "empty",
            identifier: String::new(),
            valid: false,
            length_bound: false,
        },
        IdentifierCase {
            name: "leading digit",
            identifier: "9lives".to_string(),
            valid: false,
            length_bound: false,
        },
        IdentifierCase {
            name: "leading hyphen",
            identifier: "-lead".to_string(),
            valid: false,
            length_bound: false,
        },
        IdentifierCase {
            name: "trailing hyphen",
            identifier: "trail-".to_string(),
            valid: false,
            length_bound: false,
        },
        IdentifierCase {
            name: "two consecutive hyphens",
            identifier: "two--hyphens".to_string(),
            valid: false,
            length_bound: false,
        },
        IdentifierCase {
            name: "underscore",
            identifier: "has_underscore".to_string(),
            valid: false,
            length_bound: false,
        },
        IdentifierCase {
            name: "dot",
            identifier: "has.dot".to_string(),
            valid: false,
            length_bound: false,
        },
        IdentifierCase {
            name: "non-ascii letter",
            identifier: "caf\u{e9}".to_string(),
            valid: false,
            length_bound: false,
        },
        // The published bound, and one character past it: "Must contain 1-63
        // alphanumeric characters or hyphens" for a replication instance.
        IdentifierCase {
            name: "63 characters",
            identifier: format!("a{}", "b".repeat(62)),
            valid: true,
            length_bound: true,
        },
        IdentifierCase {
            name: "64 characters",
            identifier: format!("a{}", "b".repeat(63)),
            valid: false,
            length_bound: true,
        },
    ];

    for case in &cases {
        let emulator = Emulator::new();
        let (status, response) = emulator
            .call(
                "CreateReplicationInstance",
                &json!({
                    "ReplicationInstanceIdentifier": case.identifier,
                    "ReplicationInstanceClass": "dms.t3.medium",
                }),
            )
            .await;
        if case.valid {
            assert_eq!(
                status, 200,
                "case {:?}: expected CreateReplicationInstance to accept {:?}, got {status} \
                 {response}",
                case.name, case.identifier
            );
            assert_eq!(
                text(&response, "/ReplicationInstance/ReplicationInstanceIdentifier"),
                case.identifier.to_lowercase(),
                "case {:?}: expected the identifier stored lowercased, got {response}",
                case.name
            );
        } else {
            assert_eq!(
                (status, error_code(&response).contains("ValidationException")),
                (400, true),
                "case {:?}: expected CreateReplicationInstance to refuse {:?} with 400 \
                 ValidationException, got {status} {response}",
                case.name,
                case.identifier
            );
        }

        // The same rule governs an endpoint identifier, whose doc publishes no length
        // bound, so the length rows are skipped there.
        if case.length_bound {
            continue;
        }
        let (status, response) = emulator
            .call(
                "CreateEndpoint",
                &json!({
                    "EndpointIdentifier": case.identifier,
                    "EndpointType": "source",
                    "EngineName": "mysql",
                }),
            )
            .await;
        let expected_status = if case.valid { 200 } else { 400 };
        assert_eq!(
            status, expected_status,
            "case {:?}: expected CreateEndpoint to answer {expected_status} for {:?}, got \
             {status} {response}",
            case.name, case.identifier
        );
    }
}

#[tokio::test]
async fn a_resource_identifier_names_the_last_arn_field() {
    let emulator = Emulator::new();

    // "The value for this parameter can have up to 31 characters" — the published
    // bound, and one character past it.
    let at_bound = format!("Example-App{}", "A".repeat(RESOURCE_IDENTIFIER_MAX_LEN - 11));
    let past_bound = format!("{at_bound}A");
    assert_eq!(
        at_bound.chars().count(),
        RESOURCE_IDENTIFIER_MAX_LEN,
        "the at-bound case must be exactly {RESOURCE_IDENTIFIER_MAX_LEN} characters"
    );

    let created = emulator
        .ok(
            "resource identifier at the bound",
            "CreateReplicationInstance",
            &json!({
                "ReplicationInstanceIdentifier": "named-suffix",
                "ReplicationInstanceClass": "dms.t3.medium",
                "ResourceIdentifier": at_bound,
            }),
        )
        .await;
    assert_eq!(
        text(&created, "/ReplicationInstance/ReplicationInstanceArn"),
        format!("arn:aws:dms:{REGION}:{ACCOUNT}:rep:{at_bound}"),
        "case \"resource identifier at the bound\": expected the ARN to end in the \
         requested resource identifier, got {created}"
    );

    for (case, operation, body) in [
        (
            "one character past the bound",
            "CreateReplicationInstance",
            json!({
                "ReplicationInstanceIdentifier": "too-long-suffix",
                "ReplicationInstanceClass": "dms.t3.medium",
                "ResourceIdentifier": past_bound,
            }),
        ),
        (
            "trailing hyphen",
            "CreateEndpoint",
            json!({
                "EndpointIdentifier": "bad-suffix-ep",
                "EndpointType": "source",
                "EngineName": "mysql",
                "ResourceIdentifier": "Example-App-",
            }),
        ),
    ] {
        let (status, response) = emulator.call(operation, &body).await;
        assert_eq!(
            (status, error_code(&response).contains("ValidationException")),
            (400, true),
            "case {case:?}: expected 400 ValidationException, got {status} {response}"
        );
    }
}

#[tokio::test]
async fn max_records_is_bounded_and_a_marker_pages_the_scan() {
    let emulator = Emulator::new();

    // The range every paginated DMS describe publishes for `MaxRecords`: "Default: 100"
    // and "Constraints: Minimum 20, maximum 100". Each published bound is tested, and
    // one value past each.
    let bounds = [
        ("no MaxRecords", None, 200),
        ("one below the minimum", Some(MAX_RECORDS_MIN - 1), 400),
        ("the published minimum", Some(MAX_RECORDS_MIN), 200),
        ("the published maximum", Some(MAX_RECORDS_MAX), 200),
        ("one above the maximum", Some(MAX_RECORDS_MAX + 1), 400),
        ("zero", Some(0), 400),
        ("negative", Some(-1), 400),
    ];
    for (case, max_records, expected) in bounds {
        for operation in [
            "DescribeReplicationInstances",
            "DescribeEndpoints",
            "DescribeReplicationTasks",
        ] {
            let body = match max_records {
                Some(max_records) => json!({ "MaxRecords": max_records }),
                None => json!({}),
            };
            let (status, response) = emulator.call(operation, &body).await;
            assert_eq!(
                status, expected,
                "case {case:?} on {operation}: expected {expected}, got {status} {response}"
            );
            if expected == 400 {
                assert!(
                    error_code(&response).contains("ValidationException"),
                    "case {case:?} on {operation}: expected a ValidationException, got \
                     {response}"
                );
            }
        }
    }

    // One more endpoint than a minimum-sized page holds, so the scan has to resume.
    let page = usize::try_from(MAX_RECORDS_MIN).expect("a positive page size");
    let total = page + 5;
    let mut created = Vec::new();
    for index in 0..total {
        created.push(
            emulator
                .endpoint(&format!("page-ep-{index}"), "source", "mysql")
                .await,
        );
    }

    let first = emulator
        .ok(
            "first page",
            "DescribeEndpoints",
            &json!({ "MaxRecords": MAX_RECORDS_MIN }),
        )
        .await;
    let first_arns = column(&first, "/Endpoints", "EndpointArn");
    assert_eq!(
        first_arns.len(),
        page,
        "case \"first page\": expected {page} endpoints, got {first}"
    );
    let marker = first
        .get("Marker")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        !marker.is_empty(),
        "case \"first page\": expected a Marker when records remain, got {first}"
    );
    assert!(
        !marker.contains(':'),
        "case \"first page\": expected an opaque Marker, got {marker:?}"
    );

    let second = emulator
        .ok(
            "second page",
            "DescribeEndpoints",
            &json!({ "MaxRecords": MAX_RECORDS_MIN, "Marker": marker }),
        )
        .await;
    let second_arns = column(&second, "/Endpoints", "EndpointArn");
    assert_eq!(
        second_arns.len(),
        total - page,
        "case \"second page\": expected the remaining {} endpoints, got {second}",
        total - page
    );
    assert!(
        second.get("Marker").is_none(),
        "case \"second page\": expected no Marker on the final page, got {second}"
    );

    let paged: Vec<String> = first_arns.into_iter().chain(second_arns).collect();
    assert_eq!(
        paged, created,
        "case \"paged scan\": expected the pages to cover every endpoint exactly once, \
         in creation order, got {paged:?}"
    );

    // A single page the size of the whole scan carries no marker.
    let whole = emulator
        .ok(
            "one page for the whole scan",
            "DescribeEndpoints",
            &json!({ "MaxRecords": MAX_RECORDS_MAX }),
        )
        .await;
    assert_eq!(
        column(&whole, "/Endpoints", "EndpointArn").len(),
        total,
        "case \"one page for the whole scan\": expected every endpoint, got {whole}"
    );
    assert!(
        whole.get("Marker").is_none(),
        "case \"one page for the whole scan\": expected no Marker, got {whole}"
    );

    // A marker this emulator did not mint is refused rather than silently ignored.
    for (case, marker) in [
        ("not hex", "page-ep-0"),
        ("hex that names nothing", "6e6f742d616e2d61726e"),
    ] {
        let (status, response) = emulator
            .call(
                "DescribeEndpoints",
                &json!({ "MaxRecords": MAX_RECORDS_MIN, "Marker": marker }),
            )
            .await;
        assert_eq!(
            (status, error_code(&response).contains("ValidationException")),
            (400, true),
            "case {case:?}: expected 400 ValidationException, got {status} {response}"
        );
    }
}

#[tokio::test]
async fn every_documented_filter_narrows_the_describe() {
    let emulator = Emulator::new();
    let instance_arn = emulator.available_instance("filter-rep").await;
    let other_instance_arn = emulator.available_instance("other-rep").await;
    let source_arn = emulator.endpoint("filter-src", "source", "mysql").await;
    let target_arn = emulator.endpoint("filter-tgt", "target", "postgres").await;
    let task = emulator
        .ok(
            "filter task",
            "CreateReplicationTask",
            &json!({
                "ReplicationTaskIdentifier": "filter-task",
                "SourceEndpointArn": source_arn,
                "TargetEndpointArn": target_arn,
                "ReplicationInstanceArn": instance_arn,
                "MigrationType": "cdc",
                "TableMappings": table_mappings(),
            }),
        )
        .await;
    let task_arn = text(&task, "/ReplicationTask/ReplicationTaskArn");

    // Every filter name the three `Filters` docs publish, each with a value that
    // matches and one that does not.
    let cases: Vec<(&str, &str, &str, String, Vec<String>)> = vec![
        (
            "DescribeReplicationInstances",
            "/ReplicationInstances",
            "replication-instance-arn",
            instance_arn.clone(),
            vec![instance_arn.clone()],
        ),
        (
            "DescribeReplicationInstances",
            "/ReplicationInstances",
            "replication-instance-id",
            "other-rep".to_string(),
            vec![other_instance_arn.clone()],
        ),
        (
            "DescribeReplicationInstances",
            "/ReplicationInstances",
            "replication-instance-class",
            "dms.t3.medium".to_string(),
            vec![instance_arn.clone(), other_instance_arn.clone()],
        ),
        (
            "DescribeReplicationInstances",
            "/ReplicationInstances",
            "replication-instance-class",
            "dms.c4.xlarge".to_string(),
            Vec::new(),
        ),
        (
            "DescribeReplicationInstances",
            "/ReplicationInstances",
            "engine-version",
            "0.0.0".to_string(),
            Vec::new(),
        ),
        (
            "DescribeEndpoints",
            "/Endpoints",
            "endpoint-arn",
            target_arn.clone(),
            vec![target_arn.clone()],
        ),
        (
            "DescribeEndpoints",
            "/Endpoints",
            "endpoint-type",
            "target".to_string(),
            vec![target_arn.clone()],
        ),
        (
            "DescribeEndpoints",
            "/Endpoints",
            "endpoint-id",
            "filter-src".to_string(),
            vec![source_arn.clone()],
        ),
        (
            "DescribeEndpoints",
            "/Endpoints",
            "engine-name",
            "mysql".to_string(),
            vec![source_arn.clone()],
        ),
        (
            "DescribeEndpoints",
            "/Endpoints",
            "engine-name",
            "sqlserver".to_string(),
            Vec::new(),
        ),
        (
            "DescribeReplicationTasks",
            "/ReplicationTasks",
            "replication-task-arn",
            task_arn.clone(),
            vec![task_arn.clone()],
        ),
        (
            "DescribeReplicationTasks",
            "/ReplicationTasks",
            "replication-task-id",
            "filter-task".to_string(),
            vec![task_arn.clone()],
        ),
        (
            "DescribeReplicationTasks",
            "/ReplicationTasks",
            "migration-type",
            "cdc".to_string(),
            vec![task_arn.clone()],
        ),
        (
            "DescribeReplicationTasks",
            "/ReplicationTasks",
            "migration-type",
            "full-load".to_string(),
            Vec::new(),
        ),
        (
            "DescribeReplicationTasks",
            "/ReplicationTasks",
            "endpoint-arn",
            target_arn.clone(),
            vec![task_arn.clone()],
        ),
        (
            "DescribeReplicationTasks",
            "/ReplicationTasks",
            "replication-instance-arn",
            other_instance_arn.clone(),
            Vec::new(),
        ),
    ];

    for (operation, pointer, name, value, expected) in cases {
        let member = match operation {
            "DescribeReplicationInstances" => "ReplicationInstanceArn",
            "DescribeEndpoints" => "EndpointArn",
            _ => "ReplicationTaskArn",
        };
        // DMS raises its one modelled error rather than answering an empty list when
        // the filters narrow a describe to nothing. The two describes that do so each
        // have their own fixed message.
        let no_match_message = match operation {
            "DescribeReplicationInstances" => Some("No instances found matching provided filters"),
            "DescribeEndpoints" => Some("No Endpoints found matching provided filters"),
            "DescribeReplicationTasks" => {
                Some("No ReplicationTasks found matching provided filters")
            }
            _ => None,
        };
        if expected.is_empty() {
            if let Some(message) = no_match_message {
                let (status, response) = emulator
                    .call(
                        operation,
                        &json!({ "Filters": [{ "Name": name, "Values": [value] }] }),
                    )
                    .await;
                assert_eq!(
                    (
                        status,
                        error_code(&response),
                        text(&response, "/message").as_str()
                    ),
                    (400, "ResourceNotFoundFault".to_string(), message),
                    "case {name:?}={value:?} on {operation}: expected 400 ResourceNotFoundFault \
                     {message:?}, got {status} {response}"
                );
                continue;
            }
        }
        let described = emulator
            .ok(
                &format!("{name}={value}"),
                operation,
                &json!({ "Filters": [{ "Name": name, "Values": [value] }] }),
            )
            .await;
        assert_eq!(
            column(&described, pointer, member),
            expected,
            "case {name:?}={value:?} on {operation}: expected {expected:?}, got {described}"
        );
    }

    // A filter name no doc publishes is refused, rather than ignored — which would
    // answer with resources the caller asked to exclude. DMS refuses it as a parameter
    // value it does not accept, naming the filter name the request carried.
    for operation in [
        "DescribeReplicationInstances",
        "DescribeEndpoints",
        "DescribeReplicationTasks",
    ] {
        let (status, response) = emulator
            .call(
                operation,
                &json!({ "Filters": [{ "Name": "endpoint-colour", "Values": ["blue"] }] }),
            )
            .await;
        assert_eq!(
            (status, error_code(&response), text(&response, "/message")),
            (
                400,
                "InvalidParameterValueException".to_string(),
                "Unrecognized filter name: endpoint-colour".to_string()
            ),
            "case \"undocumented filter\" on {operation}: expected 400 \
             InvalidParameterValueException naming the filter name, got {status} {response}"
        );
    }
}

// ---- DeleteEndpoint, and what a deleted endpoint leaves behind ----------------------
//
// RULE: a delete takes the endpoint it names out of the account — the endpoint moves to
// the transient `deleting` status whose removal the clock owes, and once that removal has
// been applied the account no longer holds it: a describe narrowed to the endpoint raises
// the one error `DescribeEndpoints` models, a second delete is refused as an ARN that
// names nothing, and the `EndpointIdentifier` it carried is free again, so a later create
// may use that identifier and mints a fresh `EndpointArn` for the endpoint it creates. An
// endpoint a replication task still names is not deleted at all: "All tasks associated
// with the endpoint must be deleted before you can delete the endpoint".
//
// The rows span what the rule generalises over: the endpoint type, the engine, the
// connection parameter the two endpoints of a case differ in and the members a response
// reports it through, the port, and whether DMS minted the ARN's resource id or the caller
// supplied it.

/// How long one poll of a clock-owed transition waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a poll loop waits for a clock-owed transition before giving up. Far past
/// every span this emulator gives one, so a loaded machine does not fail a case the clock
/// would have settled.
const POLL_BUDGET: Duration = Duration::from_secs(60);

/// How far out a held transition is pushed, so a case about the transient state a delete
/// creates reads that state instead of racing the clock that ends it.
const HELD_FOR: Duration = Duration::from_secs(3600);

/// The members a create carries to distinguish the two endpoints of one delete case: the
/// value goes into the connection parameter its engine reports it through.
type Members = fn(&str) -> Value;

/// The server the endpoint database resides on (`CreateEndpoint$ServerName`), which every
/// relational engine's settings structure reports a copy of.
fn server_name(value: &str) -> Value {
    json!({ "ServerName": value })
}

/// The bucket an Amazon S3 endpoint reads or writes (`S3Settings$BucketName`): an S3
/// endpoint names no server, so its settings structure is where its case's two endpoints
/// differ.
fn s3_bucket(value: &str) -> Value {
    json!({ "S3Settings": { "BucketName": value } })
}

/// One delete-and-re-create case.
struct DeleteCase {
    /// What the case is, named in every assertion of it.
    case: &'static str,
    /// `source` or `target`, as `ReplicationEndpointTypeValue` models them.
    endpoint_type: &'static str,
    /// The engine the endpoint runs, from the catalog `CreateEndpoint$EngineName`
    /// publishes.
    engine_name: &'static str,
    /// The members carrying the value the case's two endpoints differ in.
    members: Members,
    /// Where a described endpoint must report that value.
    reported_at: &'static [&'static str],
    /// The value the endpoint the case deletes carries, and the port it runs on.
    first: (&'static str, i64),
    /// The value the endpoint re-created under the released identifier carries, and its
    /// port.
    second: (&'static str, i64),
    /// The `ResourceIdentifier` both creates supply, where the case supplies one: with one
    /// the ARN's last field is the caller's, without one DMS mints it.
    resource_identifier: Option<&'static str>,
}

/// The delete cases, spanning the engines, both endpoint types, the connection parameter
/// each engine reports and both ways the ARN's resource id is arrived at.
const DELETE_CASES: &[DeleteCase] = &[
    DeleteCase {
        case: "a target postgres endpoint re-pointed at a second server",
        endpoint_type: "target",
        engine_name: "postgres",
        members: server_name,
        reported_at: &["/ServerName", "/PostgreSQLSettings/ServerName"],
        first: ("pg1.example.invalid", 5432),
        second: ("pg2.example.invalid", 5433),
        resource_identifier: None,
    },
    DeleteCase {
        case: "a source mysql endpoint",
        endpoint_type: "source",
        engine_name: "mysql",
        members: server_name,
        reported_at: &["/ServerName", "/MySQLSettings/ServerName"],
        first: ("my1.example.invalid", 3306),
        second: ("my2.example.invalid", 3307),
        resource_identifier: None,
    },
    DeleteCase {
        case: "a source oracle endpoint whose resource id the caller supplied",
        endpoint_type: "source",
        engine_name: "oracle",
        members: server_name,
        reported_at: &["/ServerName", "/OracleSettings/ServerName"],
        first: ("ora1.example.invalid", 1521),
        second: ("ora2.example.invalid", 1522),
        resource_identifier: Some("Example-App-ARN1"),
    },
    DeleteCase {
        case: "a target sqlserver endpoint",
        endpoint_type: "target",
        engine_name: "sqlserver",
        members: server_name,
        reported_at: &["/ServerName", "/MicrosoftSQLServerSettings/ServerName"],
        first: ("mssql1.example.invalid", 1433),
        second: ("mssql2.example.invalid", 1434),
        resource_identifier: None,
    },
    DeleteCase {
        case: "a target s3 endpoint, which names a bucket rather than a server",
        endpoint_type: "target",
        engine_name: "s3",
        members: s3_bucket,
        reported_at: &["/S3Settings/BucketName"],
        first: ("dms-first-bucket", 443),
        second: ("dms-second-bucket", 443),
        resource_identifier: None,
    },
];

/// A `Filters` request narrowing a describe to one endpoint identifier.
fn by_endpoint_id(identifier: &str) -> Value {
    json!({ "Filters": [{ "Name": "endpoint-id", "Values": [identifier] }] })
}

/// A `Filters` request narrowing a describe to one endpoint ARN.
fn by_endpoint_arn(arn: &str) -> Value {
    json!({ "Filters": [{ "Name": "endpoint-arn", "Values": [arn] }] })
}

/// The create body for one delete case: the shared connection parameters, the case's
/// engine and distinguishing value, and the `ResourceIdentifier` it supplies, if any.
fn delete_case_body(case: &DeleteCase, identifier: &str, value: &str, port: i64) -> Value {
    let mut body = json!({
        "EndpointIdentifier": identifier,
        "EndpointType": case.endpoint_type,
        "EngineName": case.engine_name,
        "Port": port,
        "DatabaseName": "inventory",
        "Username": "migrator",
    });
    let members = (case.members)(value);
    for (name, member) in members.as_object().expect("the members are an object") {
        body[name.as_str()] = member.clone();
    }
    if let Some(resource_identifier) = case.resource_identifier {
        body["ResourceIdentifier"] = json!(resource_identifier);
    }
    body
}

/// The one endpoint a describe narrowed by `request` reports, failing the case when it
/// reports anything else.
async fn one_endpoint(emulator: &Emulator, case: &str, request: &Value) -> Value {
    let answered = emulator.ok(case, "DescribeEndpoints", request).await;
    let endpoints = answered
        .pointer("/Endpoints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        endpoints.len(),
        1,
        "case {case:?}: expected the describe to report exactly one endpoint, got {answered}"
    );
    endpoints[0].clone()
}

/// Poll `DescribeEndpoints` narrowed by `request` until it is refused because nothing
/// matches, and report the refusal body. A describe still reporting an endpoint past
/// [`POLL_BUDGET`] fails the case.
async fn poll_until_no_endpoint_matches(
    emulator: &Emulator,
    case: &str,
    request: &Value,
) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let (status, response) = emulator.call("DescribeEndpoints", request).await;
        if status != 200 {
            return response;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the deleted endpoint never left the account; the describe still \
             answered {status} with {response}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `DescribeReplicationInstances` until the instance `arn` names has finished
/// provisioning, which is what a replication task needs of the instance that carries it.
async fn poll_until_instance_available(emulator: &Emulator, case: &str, arn: &str) {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let described = emulator
            .ok(
                case,
                "DescribeReplicationInstances",
                &json!({
                    "Filters": [{ "Name": "replication-instance-arn", "Values": [arn] }],
                }),
            )
            .await;
        let status = text(
            &described,
            "/ReplicationInstances/0/ReplicationInstanceStatus",
        );
        if status == INSTANCE_AVAILABLE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the replication instance never became available, last {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `DescribeReplicationTasks` until the task `arn` names has left the account, and
/// report the refusal that says so.
///
/// A describe narrowed to a task the account no longer holds matches nothing, which this
/// operation answers with the one error it models rather than an empty list.
async fn poll_until_task_is_gone(emulator: &Emulator, case: &str, arn: &str) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let (status, described) = emulator
            .call(
                "DescribeReplicationTasks",
                &json!({
                    "Filters": [{ "Name": "replication-task-arn", "Values": [arn] }],
                }),
            )
            .await;
        if status == 400 {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the deleted replication task never left the account, {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Push every removal the store still owes an endpoint an hour out, so a case about the
/// `deleting` state a delete creates reads that state.
///
/// This writes only the due instant the emulator itself stores; the removal is still
/// applied by the store's own settling on the next request, exactly as in production.
fn hold_endpoint_removals(engine: &engine::InMemoryEngine) {
    let store = engine.state::<
        service_com_amazonaws_databasemigrationservice_amazondmsv20160101::ComAmazonawsDatabasemigrationserviceAmazondmsv20160101State,
    >();
    let mut store = store.lock();
    let held = SystemTime::now() + HELD_FOR;
    for endpoint in store.endpoints.values_mut() {
        if let Some(pending) = endpoint.pending.as_mut() {
            pending.due = held;
        }
    }
}

/// A delete takes the endpoint it names out of the account and frees its identifier, so
/// the same identifier creates a NEW endpoint, with its own ARN and its own connection
/// parameters, while every other endpoint the account holds stays where it was.
#[tokio::test]
async fn a_deleted_endpoint_leaves_the_account_and_releases_its_identifier() {
    let emulator = Emulator::new();

    // An endpoint no case deletes: a delete removes the endpoint it names, not the store.
    let bystander_arn = emulator.endpoint("kept-endpoint", "source", "postgres").await;

    for (index, case_row) in DELETE_CASES.iter().enumerate() {
        let case = case_row.case;
        let identifier = format!("reused-identifier-{index}");

        // ---- the endpoint the case deletes -------------------------------------------
        let created = emulator
            .ok(
                case,
                "CreateEndpoint",
                &delete_case_body(case_row, &identifier, case_row.first.0, case_row.first.1),
            )
            .await;
        let first_arn = text(&created, "/Endpoint/EndpointArn");
        let described = one_endpoint(&emulator, case, &by_endpoint_id(&identifier)).await;
        for pointer in case_row.reported_at {
            assert_eq!(
                text(&described, pointer),
                case_row.first.0,
                "case {case:?}: expected the created endpoint to report {:?} at {pointer}, \
                 got {described}",
                case_row.first.0
            );
        }

        // ---- the delete ----------------------------------------------------------------
        let deleted = emulator
            .ok(case, "DeleteEndpoint", &json!({ "EndpointArn": first_arn }))
            .await;
        assert_eq!(
            (
                text(&deleted, "/Endpoint/EndpointArn"),
                text(&deleted, "/Endpoint/EndpointIdentifier"),
                text(&deleted, "/Endpoint/Status"),
            ),
            (
                first_arn.clone(),
                identifier.clone(),
                ENDPOINT_DELETING.to_string(),
            ),
            "case {case:?}: expected the delete to answer with the endpoint it deleted, on \
             its way out, got {deleted}"
        );

        // ---- what the account holds afterwards -----------------------------------------
        let refused =
            poll_until_no_endpoint_matches(&emulator, case, &by_endpoint_id(&identifier)).await;
        assert_eq!(
            (error_code(&refused), text(&refused, "/message")),
            (
                "ResourceNotFoundFault".to_string(),
                "No Endpoints found matching provided filters".to_string(),
            ),
            "case {case:?}: expected the describe of the deleted endpoint to be refused as a \
             resource DMS cannot find, got {refused}"
        );
        let (arn_status, by_arn) = emulator
            .call("DescribeEndpoints", &by_endpoint_arn(&first_arn))
            .await;
        assert_eq!(
            (arn_status, error_code(&by_arn)),
            (400, "ResourceNotFoundFault".to_string()),
            "case {case:?}: expected a describe narrowed to the deleted ARN to be refused, \
             got {arn_status} {by_arn}"
        );
        let (again_status, again) = emulator
            .call("DeleteEndpoint", &json!({ "EndpointArn": first_arn }))
            .await;
        assert_eq!(
            (
                again_status,
                error_code(&again),
                text(&again, "/message")
            ),
            (
                400,
                "ResourceNotFoundFault".to_string(),
                format!("Endpoint for {first_arn} not found."),
            ),
            "case {case:?}: expected a second delete of the gone endpoint to be refused as an \
             ARN that names nothing, got {again_status} {again}"
        );

        // ---- the released identifier ----------------------------------------------------
        let recreated = emulator
            .ok(
                case,
                "CreateEndpoint",
                &delete_case_body(case_row, &identifier, case_row.second.0, case_row.second.1),
            )
            .await;
        let second_arn = text(&recreated, "/Endpoint/EndpointArn");
        // An ARN whose resource id DMS minted is a fresh one; one whose resource id the
        // caller supplied is the same string again, because the caller named it.
        if case_row.resource_identifier.is_none() {
            assert_ne!(
                second_arn, first_arn,
                "case {case:?}: expected the re-created endpoint to carry a freshly minted \
                 ARN, got the deleted endpoint's"
            );
        } else {
            assert_eq!(
                second_arn, first_arn,
                "case {case:?}: expected the ARN to carry the ResourceIdentifier the request \
                 supplied"
            );
        }

        let described = one_endpoint(&emulator, case, &by_endpoint_id(&identifier)).await;
        assert_eq!(
            (
                text(&described, "/EndpointArn"),
                described.pointer("/Port").and_then(Value::as_i64),
                text(&described, "/Status"),
            ),
            (
                second_arn.clone(),
                Some(case_row.second.1),
                ENDPOINT_ACTIVE.to_string(),
            ),
            "case {case:?}: expected the describe to report the endpoint that now carries the \
             identifier, got {described}"
        );
        for pointer in case_row.reported_at {
            assert_eq!(
                text(&described, pointer),
                case_row.second.0,
                "case {case:?}: expected the re-created endpoint to report {:?} at {pointer}, \
                 got {described}",
                case_row.second.0
            );
        }
    }

    // The endpoint no case deleted is still held, beside exactly one endpoint per case.
    let held = emulator
        .ok("the bystander endpoint", "DescribeEndpoints", &json!({}))
        .await;
    let arns = column(&held, "/Endpoints", "EndpointArn");
    assert!(
        arns.contains(&bystander_arn),
        "case \"the bystander endpoint\": expected the endpoint no case deleted to still be \
         held, got {arns:?}"
    );
    assert_eq!(
        arns.len(),
        DELETE_CASES.len() + 1,
        "case \"the bystander endpoint\": expected the bystander and one re-created endpoint \
         per case, got {arns:?}"
    );
}

/// The span between the delete and the removal it owes is observable: the endpoint reads
/// `deleting`, a second delete is refused in that status, and the identifier is still
/// taken — the delete is not a change the caller cannot see.
#[tokio::test]
async fn a_deleting_endpoint_is_still_held_until_its_removal_falls_due() {
    let case = "an endpoint inside its deleting span";
    let emulator = Emulator::new();
    let created = emulator
        .ok(
            case,
            "CreateEndpoint",
            &json!({
                "EndpointIdentifier": "leaving-endpoint",
                "EndpointType": "target",
                "EngineName": "postgres",
                "ServerName": "leaving.example.invalid",
                "Port": 5432,
            }),
        )
        .await;
    let arn = text(&created, "/Endpoint/EndpointArn");

    emulator
        .ok(case, "DeleteEndpoint", &json!({ "EndpointArn": arn }))
        .await;
    // Hold the removal the store owes, so the case reads the state the delete created
    // rather than whichever side of the span the host happened to land on.
    hold_endpoint_removals(emulator.engine());

    let described = one_endpoint(&emulator, case, &by_endpoint_arn(&arn)).await;
    assert_eq!(
        text(&described, "/Status"),
        ENDPOINT_DELETING,
        "case {case:?}: expected the endpoint to read as leaving, got {described}"
    );

    let (status, refused) = emulator
        .call("DeleteEndpoint", &json!({ "EndpointArn": arn }))
        .await;
    let message = text(&refused, "/message");
    assert_eq!(
        (
            status,
            error_code(&refused).contains("InvalidResourceStateFault"),
            message.contains(&arn) && message.contains(ENDPOINT_DELETING),
        ),
        (400, true, true),
        "case {case:?}: expected a delete of an endpoint already leaving to be refused in the \
         state it is in, naming the endpoint and that status, got {status} {refused}"
    );

    let (taken_status, taken) = emulator
        .call(
            "CreateEndpoint",
            &json!({
                "EndpointIdentifier": "leaving-endpoint",
                "EndpointType": "target",
                "EngineName": "postgres",
                "ServerName": "second.example.invalid",
                "Port": 5432,
            }),
        )
        .await;
    assert_eq!(
        (
            taken_status,
            error_code(&taken).contains("ResourceAlreadyExistsFault")
        ),
        (400, true),
        "case {case:?}: expected the identifier of an endpoint that has not left yet to still \
         be in use, got {taken_status} {taken}"
    );
}

/// "All tasks associated with the endpoint must be deleted before you can delete the
/// endpoint": an endpoint either end of a replication task stays, at both ends, and goes
/// once the task has left the account.
#[tokio::test]
async fn an_endpoint_a_replication_task_names_is_not_deleted_until_the_task_is_gone() {
    let case = "an endpoint a replication task names";
    let emulator = Emulator::new();

    let instance = emulator
        .ok(
            case,
            "CreateReplicationInstance",
            &json!({
                "ReplicationInstanceIdentifier": "task-carrier",
                "ReplicationInstanceClass": "dms.t3.medium",
            }),
        )
        .await;
    let instance_arn = text(&instance, "/ReplicationInstance/ReplicationInstanceArn");
    poll_until_instance_available(&emulator, case, &instance_arn).await;

    let source_arn = emulator.endpoint("task-source", "source", "postgres").await;
    let target_arn = emulator.endpoint("task-target", "target", "postgres").await;
    let task = emulator
        .ok(
            case,
            "CreateReplicationTask",
            &json!({
                "ReplicationTaskIdentifier": "endpoint-holder",
                "SourceEndpointArn": source_arn,
                "TargetEndpointArn": target_arn,
                "ReplicationInstanceArn": instance_arn,
                "MigrationType": "full-load",
                "TableMappings": table_mappings(),
            }),
        )
        .await;
    let task_arn = text(&task, "/ReplicationTask/ReplicationTaskArn");

    // BOTH ends of the task are refused, which is what makes this a rule about the
    // association rather than about one member of the task.
    for (end, arn) in [("source", &source_arn), ("target", &target_arn)] {
        let (status, refused) = emulator
            .call("DeleteEndpoint", &json!({ "EndpointArn": arn }))
            .await;
        let message = text(&refused, "/message");
        assert_eq!(
            (
                status,
                error_code(&refused).contains("InvalidResourceStateFault"),
                message.contains(arn.as_str()) && message.contains(&task_arn),
            ),
            (400, true, true),
            "case {case:?}: expected the {end} endpoint of a task to be refused, naming the \
             endpoint and the task that holds it, got {status} {refused}"
        );
        let still_there = one_endpoint(&emulator, case, &by_endpoint_arn(arn)).await;
        assert_eq!(
            text(&still_there, "/Status"),
            ENDPOINT_ACTIVE,
            "case {case:?}: expected the refused delete to leave the {end} endpoint as it was, \
             got {still_there}"
        );
    }

    // Once the task has left the account, the endpoints it named can go.
    emulator
        .ok(
            case,
            "DeleteReplicationTask",
            &json!({ "ReplicationTaskArn": task_arn }),
        )
        .await;
    poll_until_task_is_gone(&emulator, case, &task_arn).await;

    for (end, arn) in [("source", &source_arn), ("target", &target_arn)] {
        let deleted = emulator
            .ok(case, "DeleteEndpoint", &json!({ "EndpointArn": arn }))
            .await;
        assert_eq!(
            text(&deleted, "/Endpoint/EndpointArn"),
            *arn.as_str(),
            "case {case:?}: expected the {end} endpoint of the deleted task to be deletable, \
             got {deleted}"
        );
        let refused =
            poll_until_no_endpoint_matches(&emulator, case, &by_endpoint_arn(arn)).await;
        assert_eq!(
            error_code(&refused),
            "ResourceNotFoundFault",
            "case {case:?}: expected the {end} endpoint to leave the account, got {refused}"
        );
    }
}

// ---- ReplicationTaskSettings, and the defaults a task runs with --------------------
//
// RULE: `ReplicationTaskSettings` is optional and may name only some settings, so DMS
// resolves what a create did not say against its service defaults at create time and
// reports the RESOLVED document on every `ReplicationTask` it answers with afterwards —
// which is what makes reading a task's settings back the way to discover the logging,
// error behaviour and buffering it runs with, and what keeps a settings round trip from
// wiping the defaults.
//
// The rows span what the rule generalises over: no settings at all, one member of one
// section, several sections at once, a whole section replaced, a member no default
// publishes, and a document that names no setting at all.

/// The settings document the task at `pointer` reports, parsed.
///
/// `ReplicationTaskSettings` travels as a JSON `String`, so what a client does with it is
/// parse it — a member that is absent, or text that is not a document, fails the case here
/// the same way it fails that client.
fn settings_document(response: &Value, pointer: &str) -> Value {
    let settings = response.pointer(&format!("{pointer}/ReplicationTaskSettings"));
    let settings = settings
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected a ReplicationTaskSettings member, got {response}"));
    serde_json::from_str(settings)
        .unwrap_or_else(|error| panic!("expected a readable settings document: {error}"))
}

/// One settings-resolution case: what the create supplies, and what the task must then
/// report.
struct SettingsCase {
    /// What the case is, named in every assertion of it.
    case: &'static str,
    /// The `ReplicationTaskSettings` the create carries, if any.
    supplied: Option<Value>,
    /// Members the reported document must carry, as a JSON pointer and the value.
    reported: &'static [(&'static str, &'static str)],
}

/// Every case, each with the members it pins in the document the task reports.
///
/// The expected values are written as JSON text so one table can carry booleans, numbers
/// and strings; `reported` is read through `serde_json`.
fn settings_cases() -> Vec<SettingsCase> {
    vec![
        SettingsCase {
            case: "no settings at all",
            supplied: None,
            reported: &[
                ("/Logging/EnableLogging", "false"),
                ("/Logging/CloudWatchLogGroup", "null"),
                ("/StreamBufferSettings/StreamBufferCount", "3"),
                ("/ErrorBehavior/DataErrorPolicy", "\"LOG_ERROR\""),
                ("/FullLoadSettings/TargetTablePrepMode", "\"DO_NOTHING\""),
                ("/TargetMetadata/LobMaxSize", "32"),
                ("/ChangeProcessingTuning/MemoryKeepTime", "60"),
                ("/PostProcessingRules", "null"),
            ],
        },
        SettingsCase {
            case: "one member of one section",
            supplied: Some(json!({ "TargetMetadata": { "LobMaxSize": 64 } })),
            reported: &[
                // The member the caller named is the caller's ...
                ("/TargetMetadata/LobMaxSize", "64"),
                // ... and every other member of that section keeps its default.
                ("/TargetMetadata/SupportLobs", "true"),
                ("/TargetMetadata/LobChunkSize", "64"),
                ("/Logging/EnableLogging", "false"),
            ],
        },
        SettingsCase {
            case: "several sections at once",
            supplied: Some(json!({
                "Logging": { "EnableLogging": true },
                "ErrorBehavior": { "FailOnNoTablesCaptured": false },
            })),
            reported: &[
                ("/Logging/EnableLogging", "true"),
                ("/Logging/EnableLogContext", "false"),
                ("/ErrorBehavior/FailOnNoTablesCaptured", "false"),
                ("/ErrorBehavior/DataErrorPolicy", "\"LOG_ERROR\""),
                ("/StreamBufferSettings/StreamBufferSizeInMB", "8"),
            ],
        },
        SettingsCase {
            case: "a whole section replaced",
            supplied: Some(json!({
                "StreamBufferSettings": {
                    "StreamBufferCount": 5,
                    "StreamBufferSizeInMB": 16,
                    "CtrlStreamBufferSizeInMB": 10,
                },
            })),
            reported: &[
                ("/StreamBufferSettings/StreamBufferCount", "5"),
                ("/StreamBufferSettings/StreamBufferSizeInMB", "16"),
                ("/StreamBufferSettings/CtrlStreamBufferSizeInMB", "10"),
                ("/ControlTablesSettings/HistoryTimeslotInMinutes", "5"),
            ],
        },
        SettingsCase {
            case: "a member no default publishes",
            supplied: Some(json!({ "BeforeImageSettings": { "ColumnFilter": "pk-only" } })),
            reported: &[
                ("/BeforeImageSettings/ColumnFilter", "\"pk-only\""),
                ("/TargetMetadata/TargetSchema", "\"\""),
            ],
        },
    ]
}

/// A created task reports the settings it RUNS with: the caller's document resolved
/// against the service defaults, on every operation that answers with the task.
#[tokio::test]
async fn task_settings_are_resolved_against_the_service_defaults() {
    let emulator = Emulator::new();
    let instance_arn = emulator.available_instance("settings-rep").await;
    let source_arn = emulator.endpoint("settings-src", "source", "mysql").await;
    let target_arn = emulator.endpoint("settings-tgt", "target", "postgres").await;

    for (index, case_row) in settings_cases().into_iter().enumerate() {
        let case = case_row.case;
        let identifier = format!("settings-task-{index}");
        let mut body = json!({
            "ReplicationTaskIdentifier": identifier,
            "SourceEndpointArn": source_arn,
            "TargetEndpointArn": target_arn,
            "ReplicationInstanceArn": instance_arn,
            "MigrationType": "full-load",
            "TableMappings": table_mappings(),
        });
        if let Some(supplied) = &case_row.supplied {
            body["ReplicationTaskSettings"] = Value::from(supplied.to_string());
        }

        // The create reports the resolved document, and so does a describe of the task it
        // created: one record, one projection.
        let created = emulator.ok(case, "CreateReplicationTask", &body).await;
        let task_arn = text(&created, "/ReplicationTask/ReplicationTaskArn");
        let described = emulator
            .ok(
                case,
                "DescribeReplicationTasks",
                &json!({
                    "Filters": [{ "Name": "replication-task-arn", "Values": [task_arn] }],
                }),
            )
            .await;

        for (reader, reported) in [
            ("CreateReplicationTask", settings_document(&created, "/ReplicationTask")),
            (
                "DescribeReplicationTasks",
                settings_document(&described, "/ReplicationTasks/0"),
            ),
        ] {
            for (pointer, expected) in case_row.reported {
                let expected: Value =
                    serde_json::from_str(expected).expect("the expected value is JSON");
                assert_eq!(
                    reported.pointer(pointer).cloned().unwrap_or(Value::Null),
                    expected,
                    "case {case:?} on {reader}: expected {pointer} to be {expected}, got \
                     {reported}"
                );
            }
            // Every log component DMS logs is in the document, so a caller can read the
            // severity each one runs at.
            let components: Vec<String> = reported
                .pointer("/Logging/LogComponents")
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| text(entry, "/Id"))
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                components,
                LOG_COMPONENTS
                    .iter()
                    .map(|id| (*id).to_string())
                    .collect::<Vec<String>>(),
                "case {case:?} on {reader}: expected one entry per log component, got \
                 {reported}"
            );
            for entry in reported
                .pointer("/Logging/LogComponents")
                .and_then(Value::as_array)
                .unwrap_or(&Vec::new())
            {
                assert_eq!(
                    text(entry, "/Severity"),
                    LOG_SEVERITY_DEFAULT,
                    "case {case:?} on {reader}: expected the default severity, got {entry}"
                );
            }
        }

        // The task carries its resolved settings through its whole life: a start and the
        // delete that ends it answer with the same document.
        wait_for_transition();
        let started = emulator
            .ok(
                case,
                "StartReplicationTask",
                &json!({
                    "ReplicationTaskArn": task_arn,
                    "StartReplicationTaskType": "start-replication",
                }),
            )
            .await;
        assert_eq!(
            settings_document(&started, "/ReplicationTask"),
            settings_document(&created, "/ReplicationTask"),
            "case {case:?}: a start must report the settings the task was created with, got \
             {started}"
        );

        // `WithoutSettings` is the one way the member is left out.
        let without = emulator
            .ok(
                case,
                "DescribeReplicationTasks",
                &json!({
                    "Filters": [{ "Name": "replication-task-arn", "Values": [task_arn] }],
                    "WithoutSettings": true,
                }),
            )
            .await;
        assert!(
            without
                .pointer("/ReplicationTasks/0/ReplicationTaskSettings")
                .is_none(),
            "case {case:?}: WithoutSettings must drop the document, got {without}"
        );
    }
}

/// Settings that are not a document of settings at all name nothing to resolve, so the
/// create is refused rather than storing the text the caller sent.
#[tokio::test]
async fn settings_that_name_no_setting_are_refused() {
    let emulator = Emulator::new();
    let instance_arn = emulator.available_instance("bad-settings-rep").await;
    let source_arn = emulator.endpoint("bad-settings-src", "source", "mysql").await;
    let target_arn = emulator
        .endpoint("bad-settings-tgt", "target", "postgres")
        .await;

    for (index, (case, supplied)) in [
        ("text that is not a document", "not a settings document"),
        ("a truncated document", "{\"Logging\":"),
        ("a list of settings", "[{\"Logging\":{}}]"),
        ("a bare number", "7"),
    ]
    .into_iter()
    .enumerate()
    {
        let (status, response) = emulator
            .call(
                "CreateReplicationTask",
                &json!({
                    "ReplicationTaskIdentifier": format!("bad-settings-{index}"),
                    "SourceEndpointArn": source_arn,
                    "TargetEndpointArn": target_arn,
                    "ReplicationInstanceArn": instance_arn,
                    "MigrationType": "full-load",
                    "TableMappings": table_mappings(),
                    "ReplicationTaskSettings": supplied,
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&response).contains("ValidationException")),
            (400, true),
            "case {case:?}: expected the create to be refused, got {status} {response}"
        );
    }
}

// ---- DeleteReplicationInstance, and what a deleted instance leaves behind -----------
//
// RULE: a delete takes the replication instance it names out of the account — the instance
// moves to the transient `deleting` status whose removal the clock owes, and once that
// removal is applied the account no longer holds it: a describe narrowed to the instance
// raises the one error `DescribeReplicationInstances` models, a second delete is refused as
// an ARN that names nothing, and the `ReplicationInstanceIdentifier` it carried is free
// again. An instance a migration task still names is not deleted at all: "You must delete
// any migration tasks that are associated with the replication instance before you can
// delete it".

/// A `Filters` request narrowing a describe to one replication instance ARN.
fn by_instance_arn(arn: &str) -> Value {
    json!({ "Filters": [{ "Name": "replication-instance-arn", "Values": [arn] }] })
}

/// Poll `DescribeReplicationInstances` narrowed by `request` until it is refused because
/// nothing matches, and report the refusal body.
async fn poll_until_no_instance_matches(emulator: &Emulator, case: &str, request: &Value) -> Value {
    let deadline = Instant::now() + POLL_BUDGET;
    loop {
        let (status, response) = emulator
            .call("DescribeReplicationInstances", request)
            .await;
        if status == 400 {
            return response;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the deleted replication instance never left the account, {response}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Push every removal the store still owes a replication instance an hour out, so a case
/// about the `deleting` state a delete creates reads that state.
///
/// This writes only the due instant the emulator itself stores; the removal is still
/// applied by the store's own settling on the next request, exactly as in production.
fn hold_instance_removals(engine: &engine::InMemoryEngine) {
    let store = engine.state::<
        service_com_amazonaws_databasemigrationservice_amazondmsv20160101::ComAmazonawsDatabasemigrationserviceAmazondmsv20160101State,
    >();
    let mut store = store.lock();
    let held = SystemTime::now() + HELD_FOR;
    for instance in store.replication_instances.values_mut() {
        if let Some(pending) = instance.pending.as_mut() {
            pending.due = held;
        }
    }
}

/// A delete takes the replication instance it names out of the account and frees its
/// identifier, while every other instance the account holds stays where it was.
#[tokio::test]
async fn a_deleted_replication_instance_leaves_the_account_and_releases_its_identifier() {
    let emulator = Emulator::new();

    // An instance no case deletes: a delete removes the instance it names, not the store.
    let bystander_arn = emulator.available_instance("kept-instance").await;

    // The rows span the instance classes a create accepts and both ways the ARN's resource
    // id is arrived at — the rule is the same for every instance the account holds.
    let cases: &[(&str, &str, Option<&str>)] = &[
        ("a minted resource id", "dms.t3.medium", None),
        ("a supplied resource id", "dms.c5.large", Some("REUSEDINSTANCEID")),
        ("another instance class", "dms.r5.large", None),
    ];

    for (index, (case, instance_class, resource_identifier)) in cases.iter().enumerate() {
        let identifier = format!("reused-instance-{index}");
        let mut body = json!({
            "ReplicationInstanceIdentifier": identifier,
            "ReplicationInstanceClass": instance_class,
        });
        if let Some(resource_identifier) = resource_identifier {
            body["ResourceIdentifier"] = Value::from(*resource_identifier);
        }
        let created = emulator
            .ok(case, "CreateReplicationInstance", &body)
            .await;
        let first_arn = text(&created, "/ReplicationInstance/ReplicationInstanceArn");
        poll_until_instance_available(&emulator, case, &first_arn).await;

        // ---- the delete itself -------------------------------------------------------
        let deleted = emulator
            .ok(
                case,
                "DeleteReplicationInstance",
                &json!({ "ReplicationInstanceArn": first_arn }),
            )
            .await;
        assert_eq!(
            (
                text(&deleted, "/ReplicationInstance/ReplicationInstanceArn"),
                text(&deleted, "/ReplicationInstance/ReplicationInstanceStatus"),
            ),
            (first_arn.clone(), INSTANCE_DELETING.to_string()),
            "case {case:?}: expected the deleted instance to come back deleting, got {deleted}"
        );

        // ---- and the removal the clock owes ------------------------------------------
        let refused = poll_until_no_instance_matches(
            &emulator,
            case,
            &by_instance_arn(&first_arn),
        )
        .await;
        assert_eq!(
            (
                error_code(&refused),
                text(&refused, "/message").as_str()
            ),
            (
                "ResourceNotFoundFault".to_string(),
                "No instances found matching provided filters"
            ),
            "case {case:?}: expected the instance to leave the account, got {refused}"
        );

        // Deleting the same ARN twice finds nothing at all.
        let (status, response) = emulator
            .call(
                "DeleteReplicationInstance",
                &json!({ "ReplicationInstanceArn": first_arn }),
            )
            .await;
        assert_eq!(
            (status, error_code(&response).contains("ResourceNotFoundFault")),
            (400, true),
            "case {case:?}: expected the second delete to find nothing, got {status} {response}"
        );

        // The identifier is free again, and the new instance is its own resource.
        let recreated = emulator
            .ok(case, "CreateReplicationInstance", &body)
            .await;
        let second_arn = text(&recreated, "/ReplicationInstance/ReplicationInstanceArn");
        assert_eq!(
            resource_identifier.is_some(),
            first_arn == second_arn,
            "case {case:?}: a minted resource id must differ and a supplied one must not, \
             got {first_arn} then {second_arn}"
        );
        poll_until_instance_available(&emulator, case, &second_arn).await;
    }

    // Every case above left the bystander where it was.
    let bystander = emulator
        .ok(
            "bystander",
            "DescribeReplicationInstances",
            &by_instance_arn(&bystander_arn),
        )
        .await;
    assert_eq!(
        column(&bystander, "/ReplicationInstances", "ReplicationInstanceStatus"),
        vec![INSTANCE_AVAILABLE.to_string()],
        "case \"bystander\": expected the instance no case deleted to stay available, got \
         {bystander}"
    );
}

/// The span between the delete and the removal it owes is observable: the instance reads
/// `deleting`, a second delete is refused in that status, and the identifier is still
/// taken — the delete is not a change the caller cannot see.
#[tokio::test]
async fn a_deleting_replication_instance_is_still_held_until_its_removal_falls_due() {
    let case = "an instance inside its deleting span";
    let emulator = Emulator::new();
    let arn = emulator.available_instance("leaving-instance").await;

    emulator
        .ok(
            case,
            "DeleteReplicationInstance",
            &json!({ "ReplicationInstanceArn": arn }),
        )
        .await;
    // Hold the removal the store owes, so the case reads the state the delete created
    // rather than whichever side of the span the host happened to land on.
    hold_instance_removals(emulator.engine());

    let described = emulator
        .ok(
            case,
            "DescribeReplicationInstances",
            &by_instance_arn(&arn),
        )
        .await;
    assert_eq!(
        column(
            &described,
            "/ReplicationInstances",
            "ReplicationInstanceStatus"
        ),
        vec![INSTANCE_DELETING.to_string()],
        "case {case:?}: expected the instance to read as leaving, got {described}"
    );

    let (status, refused) = emulator
        .call(
            "DeleteReplicationInstance",
            &json!({ "ReplicationInstanceArn": arn }),
        )
        .await;
    let message = text(&refused, "/message");
    assert_eq!(
        (
            status,
            error_code(&refused).contains("InvalidResourceStateFault"),
            message.contains(&arn) && message.contains(INSTANCE_DELETING),
        ),
        (400, true, true),
        "case {case:?}: expected a delete of an instance already leaving to be refused in the \
         state it is in, naming the instance and that status, got {status} {refused}"
    );

    let (taken_status, taken) = emulator
        .call(
            "CreateReplicationInstance",
            &json!({
                "ReplicationInstanceIdentifier": "leaving-instance",
                "ReplicationInstanceClass": "dms.t3.medium",
            }),
        )
        .await;
    assert_eq!(
        (
            taken_status,
            error_code(&taken).contains("ResourceAlreadyExistsFault")
        ),
        (400, true),
        "case {case:?}: expected the identifier of an instance that has not left yet to still \
         be in use, got {taken_status} {taken}"
    );
}

/// An instance that has not finished provisioning cannot be deleted yet: a delete is
/// refused in the state the instance is in.
#[tokio::test]
async fn a_creating_replication_instance_cannot_be_deleted_yet() {
    let case = "an instance still provisioning";
    let emulator = Emulator::new();
    let created = emulator
        .ok(
            case,
            "CreateReplicationInstance",
            &json!({
                "ReplicationInstanceIdentifier": "provisioning-instance",
                "ReplicationInstanceClass": "dms.t3.medium",
            }),
        )
        .await;
    let arn = text(&created, "/ReplicationInstance/ReplicationInstanceArn");

    let (status, refused) = emulator
        .call(
            "DeleteReplicationInstance",
            &json!({ "ReplicationInstanceArn": arn }),
        )
        .await;
    let message = text(&refused, "/message");
    assert_eq!(
        (
            status,
            error_code(&refused).contains("InvalidResourceStateFault"),
            message.contains(&arn),
        ),
        (400, true, true),
        "case {case:?}: expected the delete to be refused while the instance is creating, got \
         {status} {refused}"
    );

    // Once it has finished provisioning the same delete is accepted.
    poll_until_instance_available(&emulator, case, &arn).await;
    emulator
        .ok(
            case,
            "DeleteReplicationInstance",
            &json!({ "ReplicationInstanceArn": arn }),
        )
        .await;
}

/// An ARN that names no replication instance is refused as a resource that could not be
/// found, whatever shape the ARN has.
#[tokio::test]
async fn deleting_an_instance_arn_that_names_nothing_is_refused_as_not_found() {
    let emulator = Emulator::new();
    let live = emulator.available_instance("live-instance").await;

    for (case, arn) in [
        (
            "a resource id nobody created",
            format!("arn:aws:dms:{REGION}:{ACCOUNT}:rep:NOSUCHINSTANCERESOURCEID"),
        ),
        (
            "the same instance in another region",
            live.replace(REGION, "eu-west-1"),
        ),
        ("an ARN of another resource type", live.replace(":rep:", ":task:")),
        ("text that is not an ARN", "not-an-arn".to_string()),
    ] {
        let (status, refused) = emulator
            .call(
                "DeleteReplicationInstance",
                &json!({ "ReplicationInstanceArn": arn }),
            )
            .await;
        assert_eq!(
            (status, error_code(&refused).contains("ResourceNotFoundFault")),
            (400, true),
            "case {case:?}: expected 400 ResourceNotFoundFault for {arn:?}, got {status} \
             {refused}"
        );
    }
}

/// "You must delete any migration tasks that are associated with the replication instance
/// before you can delete it": an instance a task runs on stays, and goes once the task has
/// left the account.
#[tokio::test]
async fn an_instance_a_replication_task_runs_on_is_not_deleted_until_the_task_is_gone() {
    let case = "an instance carrying a task";
    let emulator = Emulator::new();
    let instance_arn = emulator.available_instance("carrying-rep").await;
    let source_arn = emulator.endpoint("carrying-src", "source", "mysql").await;
    let target_arn = emulator
        .endpoint("carrying-tgt", "target", "postgres")
        .await;
    let task = emulator
        .ok(
            case,
            "CreateReplicationTask",
            &json!({
                "ReplicationTaskIdentifier": "carried-task",
                "SourceEndpointArn": source_arn,
                "TargetEndpointArn": target_arn,
                "ReplicationInstanceArn": instance_arn,
                "MigrationType": "cdc",
                "TableMappings": table_mappings(),
            }),
        )
        .await;
    let task_arn = text(&task, "/ReplicationTask/ReplicationTaskArn");

    let (status, response) = emulator
        .call(
            "DeleteReplicationInstance",
            &json!({ "ReplicationInstanceArn": instance_arn }),
        )
        .await;
    assert_eq!(
        (status, error_code(&response).contains("InvalidResourceStateFault")),
        (400, true),
        "case {case:?}: expected the delete to be refused while a task names the instance, \
         got {status} {response}"
    );
    assert!(
        text(&response, "/message").contains(&task_arn),
        "case {case:?}: expected the refusal to name the task that holds the instance, got \
         {response}"
    );

    // The refused delete left the instance as it was.
    let still_there = emulator
        .ok(
            case,
            "DescribeReplicationInstances",
            &by_instance_arn(&instance_arn),
        )
        .await;
    assert_eq!(
        column(
            &still_there,
            "/ReplicationInstances",
            "ReplicationInstanceStatus"
        ),
        vec![INSTANCE_AVAILABLE.to_string()],
        "case {case:?}: expected the refused delete to leave the instance available, got \
         {still_there}"
    );

    // Once the task has left the account, the instance it ran on can go.
    wait_for_transition();
    emulator
        .ok(
            case,
            "DeleteReplicationTask",
            &json!({ "ReplicationTaskArn": task_arn }),
        )
        .await;
    let gone = poll_until_task_is_gone(&emulator, case, &task_arn).await;
    assert_eq!(
        (error_code(&gone), text(&gone, "/message").as_str()),
        (
            "ResourceNotFoundFault".to_string(),
            "No ReplicationTasks found matching provided filters"
        ),
        "case {case:?}: expected the describe to be refused once the task is gone, got {gone}"
    );

    emulator
        .ok(
            case,
            "DeleteReplicationInstance",
            &json!({ "ReplicationInstanceArn": instance_arn }),
        )
        .await;
    let refused =
        poll_until_no_instance_matches(&emulator, case, &by_instance_arn(&instance_arn)).await;
    assert_eq!(
        error_code(&refused),
        "ResourceNotFoundFault",
        "case {case:?}: expected the instance to leave the account, got {refused}"
    );
}

// ---- the precision every reported date carries --------------------------------------
//
// RULE: a date a DMS response carries travels as epoch seconds at MILLISECOND precision,
// whatever resolution the clock behind it has — so a client that reads one back gets a
// timestamp of the same shape on every operation and every member, instead of the host
// clock's sub-millisecond digits.
//
// The rows span the resources and the members the rule generalises over: an instance's
// create time, a task's creation and start dates, and the dates of the run a start puts
// the task on.

/// How far a reported timestamp may sit from a whole millisecond and still count as one.
///
/// Scaled to milliseconds a current epoch-seconds value is around 1.8e12, where the
/// spacing of a 64-bit float is under a thousandth, so this tolerance absorbs the
/// representation while a value carrying microseconds misses a whole millisecond by far
/// more than it.
const MILLISECOND_TOLERANCE: f64 = 0.01;

/// Whether the epoch-seconds timestamp at `pointer` carries no more precision than the
/// milliseconds a DMS date is reported at. Fails the case when there is no timestamp
/// there at all: an absent date is not a date of the right shape.
fn is_millisecond_precision(value: &Value, pointer: &str) -> (bool, f64) {
    let seconds = value
        .pointer(pointer)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("expected an epoch-seconds timestamp at {pointer}, got {value}"));
    let millis = seconds * 1000.0;
    ((millis - millis.round()).abs() < MILLISECOND_TOLERANCE, seconds)
}

/// Every date this service reports is an epoch-seconds value at millisecond precision.
#[tokio::test]
async fn every_reported_date_carries_millisecond_precision() {
    let case = "reported dates";
    let emulator = Emulator::new();

    let created_instance = emulator
        .ok(
            case,
            "CreateReplicationInstance",
            &json!({
                "ReplicationInstanceIdentifier": "precision-rep",
                "ReplicationInstanceClass": "dms.t3.medium",
            }),
        )
        .await;
    let instance_arn = text(&created_instance, "/ReplicationInstance/ReplicationInstanceArn");
    poll_until_instance_available(&emulator, case, &instance_arn).await;
    let described_instance = emulator
        .ok(
            case,
            "DescribeReplicationInstances",
            &by_instance_arn(&instance_arn),
        )
        .await;

    let source_arn = emulator.endpoint("precision-src", "source", "mysql").await;
    let target_arn = emulator
        .endpoint("precision-tgt", "target", "postgres")
        .await;
    let created_task = emulator
        .ok(
            case,
            "CreateReplicationTask",
            &json!({
                "ReplicationTaskIdentifier": "precision-task",
                "SourceEndpointArn": source_arn,
                "TargetEndpointArn": target_arn,
                "ReplicationInstanceArn": instance_arn,
                "MigrationType": "cdc",
                "TableMappings": table_mappings(),
            }),
        )
        .await;
    let task_arn = text(&created_task, "/ReplicationTask/ReplicationTaskArn");
    wait_for_transition();
    let started_task = emulator
        .ok(
            case,
            "StartReplicationTask",
            &json!({
                "ReplicationTaskArn": task_arn,
                "StartReplicationTaskType": "start-replication",
            }),
        )
        .await;
    let described_task = emulator
        .ok(
            case,
            "DescribeReplicationTasks",
            &json!({
                "Filters": [{ "Name": "replication-task-arn", "Values": [task_arn] }],
            }),
        )
        .await;

    let rows: &[(&str, &Value, &str)] = &[
        (
            "CreateReplicationInstance",
            &created_instance,
            "/ReplicationInstance/InstanceCreateTime",
        ),
        (
            "DescribeReplicationInstances",
            &described_instance,
            "/ReplicationInstances/0/InstanceCreateTime",
        ),
        (
            "CreateReplicationTask",
            &created_task,
            "/ReplicationTask/ReplicationTaskCreationDate",
        ),
        (
            "StartReplicationTask",
            &started_task,
            "/ReplicationTask/ReplicationTaskStartDate",
        ),
        (
            "DescribeReplicationTasks",
            &described_task,
            "/ReplicationTasks/0/ReplicationTaskCreationDate",
        ),
        (
            "DescribeReplicationTasks",
            &described_task,
            "/ReplicationTasks/0/ReplicationTaskStartDate",
        ),
        (
            "DescribeReplicationTasks",
            &described_task,
            "/ReplicationTasks/0/ReplicationTaskStats/StartDate",
        ),
        (
            "DescribeReplicationTasks",
            &described_task,
            "/ReplicationTasks/0/ReplicationTaskStats/FreshStartDate",
        ),
    ];

    for (operation, response, pointer) in rows {
        let (whole_millis, seconds) = is_millisecond_precision(response, pointer);
        assert!(
            whole_millis,
            "case {operation} {pointer}: expected a timestamp at millisecond precision, got \
             {seconds}"
        );
    }
}
