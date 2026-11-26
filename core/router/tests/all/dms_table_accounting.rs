// SPDX-License-Identifier: CC0-1.0

//! The per-table accounting a run of a DMS replication task reports, across two services on
//! one engine: Database Migration Service holds the task and its endpoints, and Amazon S3
//! holds the buckets those endpoints name.
//!
//! RULE: `DescribeTableStatistics` reports the WHOLE accounting of every table a task's run
//! covers — the change rows the run applied to the target table, what its resynchronisation
//! of validation mismatches attempted, fixed and failed together with the progress over
//! them, and the documented state of validation and of resync for the table, each derived
//! from the run and from the settings document the task runs with — so a member whose count
//! is zero or whose feature the task does not enable is REPORTED with that value rather than
//! left out of the entry. A caller reading the accounting can then tell "no change applied"
//! and "not enabled" from "not reported".
//!
//! Only a cross-service suite can pin this: the task, its endpoints and its statistics are
//! DMS's, and the objects its full load reads and writes are Amazon S3's, so a run that has
//! anything to account for is observable only with both mounted on one engine.
//!
//! The rows vary what the rule generalises over: whether the task's settings enable data
//! validation (the service defaults, a document that enables it, and a partial document that
//! names another section only), the migration type, how many tables the mappings select and
//! how many rows each holds, and a table the mappings select that the source does not hold at
//! all.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which is what these calls are signed with.
/// The signature itself is never checked: this router runs with request authentication off,
/// as every suite in this crate runs it.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The `X-Amz-Target` prefix of DMS, which is its Smithy service shape name.
const DMS_TARGET_PREFIX: &str = "AmazonDMSv20160101";

/// How long one poll of a clock-owed transition waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a poll loop waits for a clock-owed transition before giving up. Far past every
/// span this emulator gives one, so a slow machine does not fail a case the clock would have
/// settled.
const POLL_BUDGET: Duration = Duration::from_secs(120);

/// The `ValidationState` and `ResyncState` of a table whose task enables neither feature, as
/// both members' docs publish it.
const NOT_ENABLED: &str = "Not enabled";

/// The `ValidationState` of a table whose records a validating task still owes validation.
const PENDING_RECORDS: &str = "Pending records";

/// The `ValidationState` of a table a validating task holds no record waiting validation
/// for.
const PENDING_VALIDATION: &str = "Pending validation";

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
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: Bytes) -> Answer {
    let credential = authorization("s3");
    dispatch(
        router,
        method,
        &format!("s3.{REGION}.amazonaws.com"),
        path,
        &[("authorization", &credential)],
        body,
    )
    .await
}

/// Create one bucket, which has to succeed.
async fn bucket(router: &MultiServiceRouter, case: &str, name: &str) {
    let created = s3(router, "PUT", &format!("/{name}"), Bytes::new()).await;
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

/// An s3 endpoint over `name`, and its ARN.
async fn s3_endpoint(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    endpoint_type: &str,
    name: &str,
) -> String {
    let created = dms_ok(
        router,
        case,
        "CreateEndpoint",
        &json!({
            "EndpointIdentifier": identifier,
            "EndpointType": endpoint_type,
            "EngineName": "s3",
            "S3Settings": { "BucketName": name },
        }),
    )
    .await;
    text(&created, "/Endpoint/EndpointArn")
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

/// Poll the task until it reports the `status` it is owed, and answer it.
async fn task_in_status(
    router: &MultiServiceRouter,
    case: &str,
    task_arn: &str,
    status: &str,
) -> Value {
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let task = described_task(router, case, task_arn).await;
        if text(&task, "/Status") == status {
            return task;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the task never reached {status:?}, last {task}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll the task until its run's full load reports itself complete.
async fn loaded_task(router: &MultiServiceRouter, case: &str, task_arn: &str) {
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let task = described_task(router, case, task_arn).await;
        if task
            .pointer("/ReplicationTaskStats/FullLoadProgressPercent")
            .and_then(Value::as_i64)
            == Some(100)
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: the task's full load never reported itself complete, last {task}"
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

/// The number at `pointer` in `value` as an integer, or `None` where the member is absent —
/// the difference this rule is about.
fn number(value: &Value, pointer: &str) -> Option<i64> {
    value.pointer(pointer).and_then(Value::as_i64)
}

/// One selection rule of a table-mappings document.
struct Rule {
    schema: &'static str,
    table: &'static str,
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
                "rule-action": "include",
            })
        })
        .collect();
    json!({ "rules": rules }).to_string()
}

/// One source object a row stages: the table it belongs to and its rows.
struct Staged {
    schema: &'static str,
    table: &'static str,
    body: &'static str,
}

/// What a row expects of ONE table's accounting once the load has run.
struct Expected {
    schema: &'static str,
    table: &'static str,
    /// The rows the table's full load moved.
    rows: i64,
    /// The `ValidationState` the entry reports.
    validation_state: &'static str,
    /// The `ValidationPendingRecords` it reports.
    validation_pending: i64,
}

/// One row of the rule: a task of one migration type, carrying one settings document, over
/// one pair of Amazon S3 endpoints.
struct Row {
    case: &'static str,
    migration_type: &'static str,
    /// The `ReplicationTaskSettings` the create carries, or `None` for a task created with
    /// none at all.
    settings: Option<&'static str>,
    staged: &'static [Staged],
    rules: &'static [Rule],
    expected: &'static [Expected],
    /// The status the task reports once its load has finished, which the migration type
    /// decides.
    status_after_load: &'static str,
}

/// The accounting members every entry of a run of this emulator carries, whatever the row:
/// the applied-change counters and the resync counters a run that applied and resynced
/// nothing accounted for.
const ZERO_COUNTERS: &[&str] = &[
    "AppliedInserts",
    "AppliedDeletes",
    "AppliedUpdates",
    "AppliedDdls",
    "ResyncRowsAttempted",
    "ResyncRowsSucceeded",
    "ResyncRowsFailed",
];

/// RULE: every table a run covers is reported with its WHOLE accounting — the applied-change
/// counters, the resync counters and progress, and the validation and resync states the
/// task's own settings decide — rather than with those members left out.
#[tokio::test]
async fn a_runs_per_table_accounting_reports_every_member() {
    let rows: &[Row] = &[
        Row {
            case: "a full load created with no settings of its own",
            migration_type: "full-load",
            settings: None,
            staged: &[Staged {
                schema: "sales",
                table: "orders",
                body: "1,alice\n2,bob\n3,cleo\n",
            }],
            rules: &[
                Rule {
                    schema: "sales",
                    table: "orders",
                },
                Rule {
                    schema: "sales",
                    table: "archive",
                },
            ],
            expected: &[
                // A table the mappings select and the source holds nothing for is accounted
                // for too: the member set is reported, with nothing loaded.
                Expected {
                    schema: "sales",
                    table: "archive",
                    rows: 0,
                    validation_state: NOT_ENABLED,
                    validation_pending: 0,
                },
                Expected {
                    schema: "sales",
                    table: "orders",
                    rows: 3,
                    validation_state: NOT_ENABLED,
                    validation_pending: 0,
                },
            ],
            status_after_load: "stopped",
        },
        Row {
            case: "a full load whose settings enable data validation",
            migration_type: "full-load",
            settings: Some(r#"{"ValidationSettings":{"EnableValidation":true}}"#),
            staged: &[Staged {
                schema: "stock",
                table: "items",
                body: "a,1\nb,2\n",
            }],
            rules: &[
                Rule {
                    schema: "stock",
                    table: "items",
                },
                Rule {
                    schema: "stock",
                    table: "ledger",
                },
            ],
            expected: &[
                // Nothing of a table the source does not hold is waiting validation, so the
                // table itself is what waits.
                Expected {
                    schema: "stock",
                    table: "ledger",
                    rows: 0,
                    validation_state: PENDING_VALIDATION,
                    validation_pending: 0,
                },
                Expected {
                    schema: "stock",
                    table: "items",
                    rows: 2,
                    validation_state: PENDING_RECORDS,
                    validation_pending: 2,
                },
            ],
            status_after_load: "stopped",
        },
        Row {
            case: "a task that captures changes after its load, with a partial settings \
                   document naming another section",
            migration_type: "full-load-and-cdc",
            settings: Some(r#"{"Logging":{"EnableLogging":true}}"#),
            staged: &[Staged {
                schema: "web",
                table: "sessions",
                body: "s1\n",
            }],
            rules: &[Rule {
                schema: "web",
                table: "sessions",
            }],
            expected: &[Expected {
                schema: "web",
                table: "sessions",
                rows: 1,
                // The partial document named no validation setting, so the task keeps the
                // default: validation off.
                validation_state: NOT_ENABLED,
                validation_pending: 0,
            }],
            status_after_load: "running",
        },
        Row {
            case: "two tables selected by a pattern, validated",
            migration_type: "full-load",
            settings: Some(
                r#"{"ValidationSettings":{"EnableValidation":true,"ThreadCount":9}}"#,
            ),
            staged: &[
                Staged {
                    schema: "hr",
                    table: "people",
                    body: "1,ada\n2,ben\n3,cyd\n4,dee\n",
                },
                Staged {
                    schema: "hr",
                    table: "roles",
                    body: "admin\n",
                },
            ],
            rules: &[Rule {
                schema: "hr",
                table: "%",
            }],
            expected: &[
                Expected {
                    schema: "hr",
                    table: "people",
                    rows: 4,
                    validation_state: PENDING_RECORDS,
                    validation_pending: 4,
                },
                Expected {
                    schema: "hr",
                    table: "roles",
                    rows: 1,
                    validation_state: PENDING_RECORDS,
                    validation_pending: 1,
                },
            ],
            status_after_load: "stopped",
        },
    ];

    let router = emulator();
    let instance_arn = available_instance(&router, "the shared instance", "accounting-ri").await;

    // Every row's task is created and started before any of them is polled, so the spans the
    // clock owes them run side by side rather than one row's after another's.
    let mut started: Vec<(&Row, String)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let source_bucket = format!("dms-accounting-source-{index}");
        let target_bucket = format!("dms-accounting-target-{index}");
        bucket(&router, case, &source_bucket).await;
        bucket(&router, case, &target_bucket).await;
        for object in row.staged {
            let key = format!("{}/{}/LOAD00000001.csv", object.schema, object.table);
            stage(&router, case, &source_bucket, &key, object.body).await;
        }

        let source_arn = s3_endpoint(
            &router,
            case,
            &format!("accounting-src-{index}"),
            "source",
            &source_bucket,
        )
        .await;
        let target_arn = s3_endpoint(
            &router,
            case,
            &format!("accounting-tgt-{index}"),
            "target",
            &target_bucket,
        )
        .await;

        let mut create = json!({
            "ReplicationTaskIdentifier": format!("accounting-task-{index}"),
            "SourceEndpointArn": source_arn,
            "TargetEndpointArn": target_arn,
            "ReplicationInstanceArn": instance_arn,
            "MigrationType": row.migration_type,
            "TableMappings": table_mappings(row.rules),
        });
        if let Some(settings) = row.settings {
            create["ReplicationTaskSettings"] = json!(settings);
        }
        let created = dms_ok(&router, case, "CreateReplicationTask", &create).await;
        let task_arn = text(&created, "/ReplicationTask/ReplicationTaskArn");
        started.push((row, task_arn));
    }

    for (row, task_arn) in &started {
        let case = row.case;
        task_in_status(&router, case, task_arn, "ready").await;
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
    }

    for (row, task_arn) in &started {
        let case = row.case;
        loaded_task(&router, case, task_arn).await;
        let task = task_in_status(&router, case, task_arn, row.status_after_load).await;
        assert_eq!(
            text(&task, "/Status"),
            row.status_after_load,
            "case {case:?}: expected the task to report {:?} once its load finished, got {task}",
            row.status_after_load
        );

        let statistics = dms_ok(
            &router,
            case,
            "DescribeTableStatistics",
            &json!({ "ReplicationTaskArn": task_arn }),
        )
        .await;
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

        for expected in row.expected {
            let entry = entries
                .iter()
                .find(|entry| {
                    text(entry, "/SchemaName") == expected.schema
                        && text(entry, "/TableName") == expected.table
                })
                .cloned()
                .unwrap_or(Value::Null);
            assert!(
                !entry.is_null(),
                "case {case:?}: expected an entry for {}.{}, got {statistics}",
                expected.schema,
                expected.table
            );
            assert_eq!(
                number(&entry, "/FullLoadRows"),
                Some(expected.rows),
                "case {case:?}: expected {}.{} to report {} loaded rows, got {entry}",
                expected.schema,
                expected.table,
                expected.rows
            );

            // Every counter the run accounted nothing for is REPORTED as that nothing.
            for member in ZERO_COUNTERS {
                assert_eq!(
                    number(&entry, &format!("/{member}")),
                    Some(0),
                    "case {case:?}: expected {}.{} to report {member} as 0, got {entry}",
                    expected.schema,
                    expected.table
                );
            }
            assert_eq!(
                entry.pointer("/ResyncProgress").and_then(Value::as_f64),
                Some(0.0),
                "case {case:?}: expected {}.{} to report ResyncProgress as 0.0, got {entry}",
                expected.schema,
                expected.table
            );
            assert_eq!(
                text(&entry, "/ResyncState"),
                NOT_ENABLED,
                "case {case:?}: expected {}.{} to report the documented resync state of a \
                 table no resync covers, got {entry}",
                expected.schema,
                expected.table
            );
            assert_eq!(
                (
                    text(&entry, "/ValidationState"),
                    number(&entry, "/ValidationPendingRecords"),
                ),
                (
                    expected.validation_state.to_string(),
                    Some(expected.validation_pending),
                ),
                "case {case:?}: expected {}.{} to report validation state {:?} with {} \
                 records pending, got {entry}",
                expected.schema,
                expected.table,
                expected.validation_state,
                expected.validation_pending
            );
        }
    }
}

/// RULE: a task that has not run has no per-table accounting at all, so the reporting
/// operation answers with no entry rather than with an entry carrying zeroed members.
#[tokio::test]
async fn a_task_that_has_never_run_reports_no_entry() {
    let case = "a task that was never started";
    let router = emulator();
    let instance_arn = available_instance(&router, case, "accounting-idle-ri").await;
    bucket(&router, case, "dms-accounting-idle-source").await;
    bucket(&router, case, "dms-accounting-idle-target").await;
    stage(
        &router,
        case,
        "dms-accounting-idle-source",
        "sales/orders/LOAD00000001.csv",
        "1,alice\n",
    )
    .await;
    let source_arn = s3_endpoint(
        &router,
        case,
        "accounting-idle-src",
        "source",
        "dms-accounting-idle-source",
    )
    .await;
    let target_arn = s3_endpoint(
        &router,
        case,
        "accounting-idle-tgt",
        "target",
        "dms-accounting-idle-target",
    )
    .await;
    let created = dms_ok(
        &router,
        case,
        "CreateReplicationTask",
        &json!({
            "ReplicationTaskIdentifier": "accounting-idle-task",
            "SourceEndpointArn": source_arn,
            "TargetEndpointArn": target_arn,
            "ReplicationInstanceArn": instance_arn,
            "MigrationType": "full-load",
            "TableMappings": table_mappings(&[Rule {
                schema: "sales",
                table: "orders",
            }]),
        }),
    )
    .await;
    let task_arn = text(&created, "/ReplicationTask/ReplicationTaskArn");
    task_in_status(&router, case, &task_arn, "ready").await;

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
        "case {case:?}: expected no per-table accounting for a task that never ran, got \
         {statistics}"
    );
}
