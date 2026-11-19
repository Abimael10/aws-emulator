// SPDX-License-Identifier: CC0-1.0

//! A started DMS replication task's RUN, across two services on one engine: Database
//! Migration Service holds the task and its endpoints, and Amazon S3 holds the buckets
//! those endpoints name.
//!
//! RULE: a replication task whose migration type includes a full load carries that load
//! out on the clock once `StartReplicationTask` has accepted it — it reaches `running`,
//! moves the rows of every table its `TableMappings` selection rules select from the source
//! endpoint's data plane to the target endpoint's, reports what moved in
//! `ReplicationTask$ReplicationTaskStats` and, per table, through
//! `DescribeTableStatistics`, and then, only where the migration type has no change-data
//! capture phase after that load, STOPS itself with `StopReason` "Stop Reason
//! FULL_LOAD_ONLY_FINISHED". For an endpoint pair whose data plane is Amazon S3, a table's
//! rows are the bytes of the `LOAD…` objects under `<BucketFolder>/<schema>/<table>/` at
//! the source, and they arrive at the target as the single object `LOAD00000001.csv` under
//! that table's prefix of the target's own bucket and folder. A table the mappings select
//! and the source holds no such object for loads no rows and is reported in the documented
//! state for a table the migration found nowhere, never as one that moved.
//!
//! Only a cross-service suite can pin this: the task, its endpoints and its statistics are
//! DMS's, and the objects the load reads and writes are Amazon S3's, so the rule is
//! observable only with both mounted on one engine. The DMS crate's own suites reach the
//! control plane alone.
//!
//! The rows vary what the rule generalises over: the migration type (`full-load`,
//! `full-load-and-cdc`, `cdc`), how many tables the mappings select and whether they are
//! named outright or by a `%` pattern, whether a rule EXCLUDES one of the tables a pattern
//! admits, how many full-load files one table's rows are spread over, how many rows each
//! file holds, whether either endpoint carries a `BucketFolder`, a table the mappings
//! select that the source does not hold, and a source object that is not a full-load file
//! at all.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which is what these calls are signed with.
/// The signature itself is never checked: this router runs with request authentication
/// off, as every suite in this crate runs it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The `X-Amz-Target` prefix of DMS, which is its Smithy service shape name.
const DMS_TARGET_PREFIX: &str = "AmazonDMSv20160101";

/// The one full-load object a table's rows arrive at an Amazon S3 target as.
const FULL_LOAD_OBJECT: &str = "LOAD00000001.csv";

/// The stop reason a task that had only a full load to do reports, as
/// `ReplicationTask$StopReason` documents it.
const FULL_LOAD_ONLY_FINISHED: &str = "Stop Reason FULL_LOAD_ONLY_FINISHED";

/// How long one poll of a clock-owed transition waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a poll loop waits for a clock-owed transition before giving up. Far past every
/// span this emulator gives one, so a slow machine does not fail a case the clock would
/// have settled.
const POLL_BUDGET: Duration = Duration::from_secs(60);

/// The two services this rule spans, over ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_databasemigrationservice_amazondmsv20160101::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// What one call answered.
struct Answer {
    status: u16,
    body: Bytes,
}

impl Answer {
    /// The body as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The body as JSON, `Null` when it is neither JSON nor present.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body,
    }
}

/// One DMS call, as an awsJson1_1 client makes it.
async fn dms(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answer {
    let target = format!("{DMS_TARGET_PREFIX}.{operation}");
    dispatch(
        router,
        "POST",
        &format!("dms.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &target),
            ("authorization", &authorization("dms")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// One DMS call that has to succeed, answering its body as JSON.
async fn dms_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let answer = dms(router, operation, body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected {operation} to answer 200, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.json()
}

/// One Amazon S3 REST call, path-style as the emulator serves it.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut with_credential: Vec<(&str, &str)> = headers.to_vec();
    let credential = authorization("s3");
    with_credential.push(("authorization", &credential));
    dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &with_credential,
        body,
    )
    .await
}

/// Create one bucket, which has to succeed.
async fn bucket(router: &MultiServiceRouter, case: &str, name: &str) {
    let created = s3(router, "PUT", &format!("/{name}"), &[], Bytes::new()).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateBucket({name}) must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
}

/// Stage one object, which has to succeed.
async fn stage(router: &MultiServiceRouter, case: &str, name: &str, key: &str, body: &str) {
    let written = s3(
        router,
        "PUT",
        &format!("/{name}/{key}"),
        &[("content-type", "text/csv")],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        written.status, 200,
        "case {case:?}: PutObject({name}/{key}) must succeed, got {} with {:?}",
        written.status,
        written.text()
    );
}

/// Every element of `name` in an XML body — how a `ListObjectsV2` answer is read.
fn xml_elements(body: &str, name: &str) -> Vec<String> {
    let (open, close) = (format!("<{name}>"), format!("</{name}>"));
    let mut found = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find(&open) {
        let from = start + open.len();
        let Some(end) = rest[from..].find(&close) else {
            break;
        };
        found.push(rest[from..from + end].to_string());
        rest = &rest[from + end + close.len()..];
    }
    found
}

/// Every key the object store holds under `prefix` of `name`.
async fn keys_under(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    prefix: &str,
) -> BTreeSet<String> {
    let listed = s3(
        router,
        "GET",
        &format!("/{name}?list-type=2&prefix={prefix}"),
        &[],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: ListObjectsV2({name}/{prefix}) must succeed, got {} with {:?}",
        listed.status,
        listed.text()
    );
    xml_elements(&listed.text(), "Key").into_iter().collect()
}

/// One object's body, which has to be there.
async fn object_body(router: &MultiServiceRouter, case: &str, name: &str, key: &str) -> String {
    let read = s3(router, "GET", &format!("/{name}/{key}"), &[], Bytes::new()).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetObject({name}/{key}) must succeed, got {} with {:?}",
        read.status,
        read.text()
    );
    read.text()
}

/// A replication instance that has finished provisioning, and its ARN.
async fn available_instance(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let created = dms_ok(
        router,
        case,
        "CreateReplicationInstance",
        &json!({
            "ReplicationInstanceIdentifier": identifier,
            "ReplicationInstanceClass": "dms.t3.medium",
        }),
    )
    .await;
    let arn = text(&created, "/ReplicationInstance/ReplicationInstanceArn");
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let described = dms_ok(
            router,
            case,
            "DescribeReplicationInstances",
            &json!({
                "Filters": [{ "Name": "replication-instance-arn", "Values": [arn] }],
            }),
        )
        .await;
        let status = text(&described, "/ReplicationInstances/0/ReplicationInstanceStatus");
        if status == "available" {
            return arn;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the replication instance never reached available, last {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// An s3 endpoint over `name`/`folder`, and its ARN.
async fn s3_endpoint(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    endpoint_type: &str,
    name: &str,
    folder: Option<&str>,
) -> String {
    let mut settings = json!({ "BucketName": name });
    if let Some(folder) = folder {
        settings["BucketFolder"] = json!(folder);
    }
    let created = dms_ok(
        router,
        case,
        "CreateEndpoint",
        &json!({
            "EndpointIdentifier": identifier,
            "EndpointType": endpoint_type,
            "EngineName": "s3",
            "S3Settings": settings,
        }),
    )
    .await;
    text(&created, "/Endpoint/EndpointArn")
}

/// What one task is created over: the identifier it carries, the instance it runs on, the
/// two endpoints it moves between, and the migration type and table mappings it carries.
struct TaskSetup<'a> {
    identifier: &'a str,
    instance_arn: &'a str,
    source_arn: &'a str,
    target_arn: &'a str,
    migration_type: &'a str,
    table_mappings: &'a str,
}

/// A replication task that has finished being created, and its ARN.
async fn ready_task(router: &MultiServiceRouter, case: &str, setup: &TaskSetup<'_>) -> String {
    let created = dms_ok(
        router,
        case,
        "CreateReplicationTask",
        &json!({
            "ReplicationTaskIdentifier": setup.identifier,
            "SourceEndpointArn": setup.source_arn,
            "TargetEndpointArn": setup.target_arn,
            "ReplicationInstanceArn": setup.instance_arn,
            "MigrationType": setup.migration_type,
            "TableMappings": setup.table_mappings,
        }),
    )
    .await;
    let arn = text(&created, "/ReplicationTask/ReplicationTaskArn");
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let status = text(&described_task(router, case, &arn).await, "/Status");
        if status == "ready" {
            return arn;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the replication task never reached ready, last {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One task, as `DescribeReplicationTasks` reports it.
async fn described_task(router: &MultiServiceRouter, case: &str, task_arn: &str) -> Value {
    let described = dms_ok(
        router,
        case,
        "DescribeReplicationTasks",
        &json!({
            "Filters": [{ "Name": "replication-task-arn", "Values": [task_arn] }],
        }),
    )
    .await;
    described
        .pointer("/ReplicationTasks/0")
        .cloned()
        .unwrap_or(Value::Null)
}

/// Poll the task until its run's full load reports itself complete, and answer the task.
///
/// The condition is the LOAD finishing, not the task stopping: whether the task then stops
/// is the part of the rule each row asserts for itself.
async fn loaded_task(router: &MultiServiceRouter, case: &str, task_arn: &str) -> Value {
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let task = described_task(router, case, task_arn).await;
        if task
            .pointer("/ReplicationTaskStats/FullLoadProgressPercent")
            .and_then(Value::as_i64)
            == Some(100)
        {
            return task;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the task's full load never reported itself complete, last {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the task until it reports a `running` status, and answer it.
async fn running_task(router: &MultiServiceRouter, case: &str, task_arn: &str) -> Value {
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let task = described_task(router, case, task_arn).await;
        if text(&task, "/Status") == "running" {
            return task;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the task never reached running, last {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The string at `pointer` in `value`, or a marker naming its absence.
fn text(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or("<missing>")
        .to_string()
}

/// The integer at `pointer` in `value`.
fn number(value: &Value, pointer: &str) -> Option<i64> {
    value.pointer(pointer).and_then(Value::as_i64)
}

/// One selection rule of a table-mappings document.
struct Rule {
    schema: &'static str,
    table: &'static str,
    action: &'static str,
}

/// A table-mappings document carrying `rules` as its selection rules.
fn table_mappings(rules: &[Rule]) -> String {
    let rules: Vec<Value> = rules
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            json!({
                "rule-type": "selection",
                "rule-id": (index + 1).to_string(),
                "rule-name": (index + 1).to_string(),
                "object-locator": { "schema-name": rule.schema, "table-name": rule.table },
                "rule-action": rule.action,
            })
        })
        .collect();
    json!({ "rules": rules }).to_string()
}

/// One source object a row stages: the table it belongs to, the file it is, and its rows.
struct Staged {
    schema: &'static str,
    table: &'static str,
    file: &'static str,
    body: &'static str,
}

/// What a row expects of ONE table after the load.
struct Expected {
    schema: &'static str,
    table: &'static str,
    /// The rows the table's full load moved.
    rows: i64,
    /// The `TableState` the load left it in.
    state: &'static str,
    /// The body the target holds for it, or `None` where the target holds no object for
    /// this table at all.
    body: Option<&'static str>,
}

/// One row of the rule: a task of one migration type over one pair of Amazon S3 endpoints.
struct Row {
    case: &'static str,
    migration_type: &'static str,
    source_folder: Option<&'static str>,
    target_folder: Option<&'static str>,
    staged: &'static [Staged],
    rules: &'static [Rule],
    expected: &'static [Expected],
    /// Whether the task stops itself when its full load finishes.
    stops: bool,
    /// Whether the task has a full-load phase at all.
    loads: bool,
}

/// RULE: a started task with a full-load phase moves the rows of every table its selection
/// rules select from the source endpoint's bucket to the target endpoint's — one
/// `LOAD00000001.csv` per table under the target's own prefix — reports the load in
/// `ReplicationTaskStats` and per table through `DescribeTableStatistics`, and stops itself
/// with `Stop Reason FULL_LOAD_ONLY_FINISHED` exactly where no change-capture phase
/// follows that load.
#[tokio::test]
async fn a_started_task_runs_its_full_load_and_reports_what_it_moved() {
    let rows: &[Row] = &[
        Row {
            case: "one table named outright, under a folder at either end",
            migration_type: "full-load",
            source_folder: Some("inbox"),
            target_folder: Some("landing"),
            staged: &[Staged {
                schema: "sales",
                table: "orders",
                file: "LOAD00000001.csv",
                body: "1,alice,12.50\n2,bob,8.00\n3,cleo,31.25\n",
            }],
            rules: &[Rule {
                schema: "sales",
                table: "orders",
                action: "include",
            }],
            expected: &[Expected {
                schema: "sales",
                table: "orders",
                rows: 3,
                state: "Table completed",
                body: Some("1,alice,12.50\n2,bob,8.00\n3,cleo,31.25\n"),
            }],
            stops: true,
            loads: true,
        },
        Row {
            case: "two tables selected by a pattern, with no folder at either end",
            migration_type: "full-load",
            source_folder: None,
            target_folder: None,
            staged: &[
                Staged {
                    schema: "sales",
                    table: "orders",
                    file: "LOAD00000001.csv",
                    body: "1,alice\n2,bob\n",
                },
                Staged {
                    schema: "sales",
                    table: "customers",
                    file: "LOAD00000001.csv",
                    body: "7,zoe\n",
                },
            ],
            rules: &[Rule {
                schema: "sales",
                table: "%",
                action: "include",
            }],
            expected: &[
                Expected {
                    schema: "sales",
                    table: "customers",
                    rows: 1,
                    state: "Table completed",
                    body: Some("7,zoe\n"),
                },
                Expected {
                    schema: "sales",
                    table: "orders",
                    rows: 2,
                    state: "Table completed",
                    body: Some("1,alice\n2,bob\n"),
                },
            ],
            stops: true,
            loads: true,
        },
        Row {
            case: "one table whose rows are spread over two full-load files",
            migration_type: "full-load",
            source_folder: Some("inbox"),
            target_folder: None,
            staged: &[
                Staged {
                    schema: "stock",
                    table: "items",
                    file: "LOAD00000001.csv",
                    body: "a,1\nb,2\n",
                },
                Staged {
                    schema: "stock",
                    table: "items",
                    file: "LOAD00000002.csv",
                    body: "c,3\n",
                },
            ],
            rules: &[Rule {
                schema: "stock",
                table: "items",
                action: "include",
            }],
            expected: &[Expected {
                schema: "stock",
                table: "items",
                rows: 3,
                state: "Table completed",
                body: Some("a,1\nb,2\nc,3\n"),
            }],
            stops: true,
            loads: true,
        },
        Row {
            case: "a pattern with one of its tables excluded",
            migration_type: "full-load",
            source_folder: None,
            target_folder: Some("landing"),
            staged: &[
                Staged {
                    schema: "hr",
                    table: "people",
                    file: "LOAD00000001.csv",
                    body: "1,ada\n",
                },
                Staged {
                    schema: "hr",
                    table: "secrets",
                    file: "LOAD00000001.csv",
                    body: "1,hidden\n",
                },
            ],
            rules: &[
                Rule {
                    schema: "hr",
                    table: "%",
                    action: "include",
                },
                Rule {
                    schema: "hr",
                    table: "secrets",
                    action: "exclude",
                },
            ],
            expected: &[Expected {
                schema: "hr",
                table: "people",
                rows: 1,
                state: "Table completed",
                body: Some("1,ada\n"),
            }],
            stops: true,
            loads: true,
        },
        Row {
            case: "a table the mappings select that the source holds nothing for",
            migration_type: "full-load",
            source_folder: None,
            target_folder: None,
            staged: &[
                Staged {
                    schema: "ops",
                    table: "events",
                    file: "LOAD00000001.csv",
                    body: "boot\nrun\n",
                },
                // Not a full-load file, so it is no table's rows.
                Staged {
                    schema: "ops",
                    table: "journal",
                    file: "20260101-120000.csv",
                    body: "ignored\n",
                },
            ],
            rules: &[
                Rule {
                    schema: "ops",
                    table: "events",
                    action: "include",
                },
                Rule {
                    schema: "ops",
                    table: "journal",
                    action: "include",
                },
                Rule {
                    schema: "ops",
                    table: "archive",
                    action: "include",
                },
            ],
            expected: &[
                Expected {
                    schema: "ops",
                    table: "archive",
                    rows: 0,
                    state: "Table does not exist",
                    body: None,
                },
                Expected {
                    schema: "ops",
                    table: "events",
                    rows: 2,
                    state: "Table completed",
                    body: Some("boot\nrun\n"),
                },
                Expected {
                    schema: "ops",
                    table: "journal",
                    rows: 0,
                    state: "Table does not exist",
                    body: None,
                },
            ],
            stops: true,
            loads: true,
        },
        Row {
            case: "a task that captures changes after its full load keeps running",
            migration_type: "full-load-and-cdc",
            source_folder: Some("inbox"),
            target_folder: Some("landing"),
            staged: &[Staged {
                schema: "web",
                table: "sessions",
                file: "LOAD00000001.csv",
                body: "s1\ns2\ns3\ns4\n",
            }],
            rules: &[Rule {
                schema: "web",
                table: "sessions",
                action: "include",
            }],
            expected: &[Expected {
                schema: "web",
                table: "sessions",
                rows: 4,
                state: "Table completed",
                body: Some("s1\ns2\ns3\ns4\n"),
            }],
            stops: false,
            loads: true,
        },
        Row {
            case: "a change-capture task has no full-load phase to run",
            migration_type: "cdc",
            source_folder: None,
            target_folder: None,
            staged: &[Staged {
                schema: "live",
                table: "feed",
                file: "LOAD00000001.csv",
                body: "x\ny\n",
            }],
            rules: &[Rule {
                schema: "live",
                table: "feed",
                action: "include",
            }],
            expected: &[],
            stops: false,
            loads: false,
        },
    ];

    let router = emulator();
    let instance_arn = available_instance(&router, "setup", "full-load-runner").await;

    // The rows that must still be running once every other row's load has long finished,
    // checked after the loop: a task with a change-capture phase does not stop at the end
    // of its full load, and one that never had a load does not stop either.
    let mut keeps_running: Vec<(&str, String)> = Vec::new();

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let source_bucket = format!("dms-full-load-source-{index}");
        let target_bucket = format!("dms-full-load-target-{index}");
        bucket(&router, case, &source_bucket).await;
        bucket(&router, case, &target_bucket).await;
        for object in row.staged {
            let key = match row.source_folder {
                Some(folder) => format!(
                    "{folder}/{}/{}/{}",
                    object.schema, object.table, object.file
                ),
                None => format!("{}/{}/{}", object.schema, object.table, object.file),
            };
            stage(&router, case, &source_bucket, &key, object.body).await;
        }

        let source_arn = s3_endpoint(
            &router,
            case,
            &format!("full-load-src-{index}"),
            "source",
            &source_bucket,
            row.source_folder,
        )
        .await;
        let target_arn = s3_endpoint(
            &router,
            case,
            &format!("full-load-tgt-{index}"),
            "target",
            &target_bucket,
            row.target_folder,
        )
        .await;
        let task_arn = ready_task(
            &router,
            case,
            &TaskSetup {
                identifier: &format!("full-load-task-{index}"),
                instance_arn: &instance_arn,
                source_arn: &source_arn,
                target_arn: &target_arn,
                migration_type: row.migration_type,
                table_mappings: &table_mappings(row.rules),
            },
        )
        .await;

        let started = dms_ok(
            &router,
            case,
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
            "case {case:?}: expected a started task to report starting, got {started}"
        );

        // A task with no full-load phase is owed `running` and nothing after it.
        if !row.loads {
            let task = running_task(&router, case, &task_arn).await;
            assert_eq!(
                (
                    task.pointer("/StopReason").is_some(),
                    task.pointer("/ReplicationTaskStats/FullLoadStartDate").is_some(),
                ),
                (false, false),
                "case {case:?}: expected a change-capture task to report neither a stop \
                 reason nor a full-load start, got {task}"
            );
            let statistics = dms_ok(
                &router,
                case,
                "DescribeTableStatistics",
                &json!({ "ReplicationTaskArn": task_arn }),
            )
            .await;
            assert_eq!(
                statistics
                    .pointer("/TableStatistics")
                    .and_then(Value::as_array)
                    .map(Vec::len),
                Some(0),
                "case {case:?}: expected no per-table statistics for a task with no full \
                 load, got {statistics}"
            );
            keeps_running.push((case, task_arn));
            continue;
        }

        let task = loaded_task(&router, case, &task_arn).await;

        // ---- the task's own account of the run ---------------------------------------
        let loaded = i32::try_from(
            row.expected
                .iter()
                .filter(|table| table.body.is_some())
                .count(),
        )
        .unwrap_or_default();
        assert_eq!(
            (
                number(&task, "/ReplicationTaskStats/TablesLoaded"),
                number(&task, "/ReplicationTaskStats/TablesLoading"),
                number(&task, "/ReplicationTaskStats/TablesQueued"),
                number(&task, "/ReplicationTaskStats/TablesErrored"),
            ),
            (Some(i64::from(loaded)), Some(0), Some(0), Some(0)),
            "case {case:?}: expected {loaded} tables loaded and nothing loading, queued or \
             errored, got {task}"
        );
        for member in [
            "/ReplicationTaskStats/StartDate",
            "/ReplicationTaskStats/FreshStartDate",
            "/ReplicationTaskStats/FullLoadStartDate",
            "/ReplicationTaskStats/FullLoadFinishDate",
        ] {
            assert!(
                task.pointer(member).is_some(),
                "case {case:?}: expected {member} on a finished full load, got {task}"
            );
        }

        if row.stops {
            let deadline = tokio::time::Instant::now() + POLL_BUDGET;
            let stopped = loop {
                let task = described_task(&router, case, &task_arn).await;
                if text(&task, "/Status") == "stopped" {
                    break task;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "case {case:?}: the task never stopped itself, last {task}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            };
            assert_eq!(
                text(&stopped, "/StopReason"),
                FULL_LOAD_ONLY_FINISHED,
                "case {case:?}: expected the documented full-load stop reason, got {stopped}"
            );
            assert!(
                stopped.pointer("/ReplicationTaskStats/StopDate").is_some(),
                "case {case:?}: expected a StopDate on a stopped task, got {stopped}"
            );
        } else {
            assert_eq!(
                (text(&task, "/Status"), task.pointer("/StopReason").is_some()),
                ("running".to_string(), false),
                "case {case:?}: expected a task with a change-capture phase to keep running \
                 with no stop reason once its load finished, got {task}"
            );
            keeps_running.push((case, task_arn.clone()));
        }

        // ---- the per-table accounting -------------------------------------------------
        let statistics = dms_ok(
            &router,
            case,
            "DescribeTableStatistics",
            &json!({ "ReplicationTaskArn": task_arn }),
        )
        .await;
        assert_eq!(
            text(&statistics, "/ReplicationTaskArn"),
            task_arn,
            "case {case:?}: expected the statistics to name their task, got {statistics}"
        );
        let entries = statistics
            .pointer("/TableStatistics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            entries.len(),
            row.expected.len(),
            "case {case:?}: expected one entry per selected table, got {statistics}"
        );
        for (position, expected) in row.expected.iter().enumerate() {
            let entry = entries.get(position).cloned().unwrap_or(Value::Null);
            assert_eq!(
                (
                    text(&entry, "/SchemaName"),
                    text(&entry, "/TableName"),
                    number(&entry, "/FullLoadRows"),
                    number(&entry, "/FullLoadErrorRows"),
                    text(&entry, "/TableState"),
                ),
                (
                    expected.schema.to_string(),
                    expected.table.to_string(),
                    Some(expected.rows),
                    Some(0),
                    expected.state.to_string(),
                ),
                "case {case:?}: expected {}.{} to report {} rows in state {:?}, got {entry}",
                expected.schema,
                expected.table,
                expected.rows,
                expected.state
            );
        }

        // ---- what landed at the target ------------------------------------------------
        for expected in row.expected {
            let prefix = match row.target_folder {
                Some(folder) => format!("{folder}/{}/{}/", expected.schema, expected.table),
                None => format!("{}/{}/", expected.schema, expected.table),
            };
            let held = keys_under(&router, case, &target_bucket, &prefix).await;
            match expected.body {
                Some(body) => {
                    let key = format!("{prefix}{FULL_LOAD_OBJECT}");
                    assert_eq!(
                        held,
                        BTreeSet::from([key.clone()]),
                        "case {case:?}: expected exactly one full-load object under \
                         {prefix}, got {held:?}"
                    );
                    let landed = object_body(&router, case, &target_bucket, &key).await;
                    assert_eq!(
                        landed, body,
                        "case {case:?}: expected the source rows at {key}, got {landed:?}"
                    );
                }
                None => assert!(
                    held.is_empty(),
                    "case {case:?}: expected nothing under {prefix} for a table the source \
                     does not hold, got {held:?}"
                ),
            }
        }
    }

    // Every row above polled its own task through a load, so the clock has long passed the
    // instant these tasks would have stopped at had they been owed a stop.
    for (case, task_arn) in &keeps_running {
        let task = described_task(&router, case, task_arn).await;
        assert_eq!(
            (text(&task, "/Status"), task.pointer("/StopReason").is_some()),
            ("running".to_string(), false),
            "case {case:?}: expected the task to be running still, with no stop reason, got \
             {task}"
        );
    }
}

/// RULE: `DescribeTableStatistics` answers for the task its `ReplicationTaskArn` names —
/// refusing an ARN that names no task of the account with the modelled
/// `ResourceNotFoundFault`, a filter name it does not document and a `MaxRecords` outside
/// the range its member publishes — and pages the entries it answers with, each page
/// resuming where the last left off.
#[tokio::test]
async fn table_statistics_are_filtered_paged_and_refused_as_the_model_says() {
    let case = "table statistics of a finished load";
    let router = emulator();
    let instance_arn = available_instance(&router, case, "statistics-runner").await;
    let source_bucket = "dms-statistics-source";
    let target_bucket = "dms-statistics-target";
    bucket(&router, case, source_bucket).await;
    bucket(&router, case, target_bucket).await;
    // Thirty tables of one schema, which is more than one page of the smallest `MaxRecords`
    // the member admits.
    let tables: Vec<String> = (0..30).map(|index| format!("t{index:02}")).collect();
    for table in &tables {
        stage(
            &router,
            case,
            source_bucket,
            &format!("warehouse/{table}/LOAD00000001.csv"),
            "1,one\n",
        )
        .await;
    }
    let source_arn = s3_endpoint(&router, case, "statistics-src", "source", source_bucket, None).await;
    let target_arn = s3_endpoint(&router, case, "statistics-tgt", "target", target_bucket, None).await;
    let task_arn = ready_task(
        &router,
        case,
        &TaskSetup {
            identifier: "statistics-task",
            instance_arn: &instance_arn,
            source_arn: &source_arn,
            target_arn: &target_arn,
            migration_type: "full-load",
            table_mappings: &table_mappings(&[Rule {
                schema: "warehouse",
                table: "%",
                action: "include",
            }]),
        },
    )
    .await;
    dms_ok(
        &router,
        case,
        "StartReplicationTask",
        &json!({
            "ReplicationTaskArn": task_arn,
            "StartReplicationTaskType": "start-replication",
        }),
    )
    .await;
    loaded_task(&router, case, &task_arn).await;

    // ---- paging ---------------------------------------------------------------------
    let mut seen: Vec<String> = Vec::new();
    let mut marker: Option<String> = None;
    for _ in 0..tables.len() {
        let mut request = json!({ "ReplicationTaskArn": task_arn, "MaxRecords": 20 });
        if let Some(marker) = &marker {
            request["Marker"] = json!(marker);
        }
        let page = dms_ok(&router, case, "DescribeTableStatistics", &request).await;
        let entries = page
            .pointer("/TableStatistics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            entries.len() <= 20,
            "case {case:?}: expected at most the requested 20 entries, got {page}"
        );
        for entry in &entries {
            seen.push(text(entry, "/TableName"));
        }
        marker = page
            .pointer("/Marker")
            .and_then(Value::as_str)
            .map(str::to_string);
        if marker.is_none() {
            break;
        }
    }
    assert_eq!(
        seen, tables,
        "case {case:?}: expected the pages to cover every table once, in order, got {seen:?}"
    );

    // ---- filtering -------------------------------------------------------------------
    let rows: &[(&str, &str, &str, usize)] = &[
        ("one table by name", "table-name", "t07", 1),
        ("a table no load covers", "table-name", "t99", 0),
        ("every table of the schema", "schema-name", "warehouse", tables.len()),
        ("another schema", "schema-name", "elsewhere", 0),
        ("the state a finished load leaves", "table-state", "Table completed", tables.len()),
        ("a state no table is in", "table-state", "Table error", 0),
    ];
    for (name, filter, value, expected) in rows {
        let answered = dms_ok(
            &router,
            name,
            "DescribeTableStatistics",
            &json!({
                "ReplicationTaskArn": task_arn,
                "MaxRecords": 100,
                "Filters": [{ "Name": filter, "Values": [value] }],
            }),
        )
        .await;
        let count = answered
            .pointer("/TableStatistics")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
        assert_eq!(
            count, *expected,
            "case {name:?}: expected {expected} entries for {filter}={value}, got {answered}"
        );
    }

    // ---- refusals ---------------------------------------------------------------------
    let refusals: &[(&str, Value, &str)] = &[
        (
            "an ARN that names no task",
            json!({
                "ReplicationTaskArn":
                    format!("arn:aws:dms:{REGION}:000000000000:task:NOSUCHTASKRESOURCEID"),
            }),
            "ResourceNotFoundFault",
        ),
        (
            "a filter name the operation does not document",
            json!({
                "ReplicationTaskArn": task_arn,
                "Filters": [{ "Name": "table-colour", "Values": ["blue"] }],
            }),
            "InvalidParameterValueException",
        ),
        (
            "a page smaller than the documented minimum",
            json!({ "ReplicationTaskArn": task_arn, "MaxRecords": 19 }),
            "ValidationException",
        ),
        (
            "a page larger than the documented maximum",
            json!({ "ReplicationTaskArn": task_arn, "MaxRecords": 501 }),
            "ValidationException",
        ),
        (
            "a marker no answer of this operation minted",
            json!({ "ReplicationTaskArn": task_arn, "Marker": "not-a-marker" }),
            "ValidationException",
        ),
    ];
    for (name, request, expected) in refusals {
        let answer = dms(&router, "DescribeTableStatistics", request).await;
        let body = answer.json();
        let code = body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<no __type>")
            .to_string();
        assert_eq!(
            (answer.status, code.contains(expected)),
            (400, true),
            "case {name:?}: expected 400 {expected}, got {} with {body}",
            answer.status
        );
    }

    // An unrecognized filter name is reported in DMS's own sentence for it, which names
    // the name the request carried and nothing else.
    let unrecognized = dms(
        &router,
        "DescribeTableStatistics",
        &json!({
            "ReplicationTaskArn": task_arn,
            "Filters": [{ "Name": "table-colour", "Values": ["blue"] }],
        }),
    )
    .await;
    let body = unrecognized.json();
    assert_eq!(
        body.get("message").and_then(Value::as_str),
        Some("Unrecognized filter name: table-colour"),
        "case \"a filter name the operation does not document\": expected DMS's wording for \
         an unrecognized filter name, got {body}"
    );

    // The documented maximum page is accepted, which is what makes the two refusals above
    // a bound rather than a blanket refusal.
    for accepted in [20, 500] {
        let answered = dms(
            &router,
            "DescribeTableStatistics",
            &json!({ "ReplicationTaskArn": task_arn, "MaxRecords": accepted }),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case \"MaxRecords {accepted}\": expected the documented bound to be accepted, \
             got {} with {:?}",
            answered.status,
            answered.text()
        );
    }
}
