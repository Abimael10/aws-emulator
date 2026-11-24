// SPDX-License-Identifier: CC0-1.0

//! The SESSION CONFIGURATION a step's SQL client holds, end to end over the wire.
//!
//! THE RULE every case below asserts: `set` is a command of the SQL CLIENT rather than a
//! statement for the metastore, so a `command-runner.jar` step whose command line invokes the
//! cluster's Hive or Spark SQL client with one RUNS it — `set <key>=<value>` sets that
//! property for the client's own session, `set <key>` prints `<key>=<value>` out of the
//! configuration that session holds (the cluster's own recorded `Configurations`, with
//! whatever the invocation set over it) and `set` with no property named prints the properties
//! it holds — the client exits zero, and the step therefore ends `COMPLETED` with what the
//! client printed where the command line sent it. A property no configuration of the cluster
//! carries is printed as one the session holds no value for, never as an invented value.
//!
//! The rows span what the rule generalises over: WHICH property is named (one the launch
//! configured in the Hive classification, one it configured in a nested classification, one it
//! configured nowhere), the FORM of the command (a report, an assignment, an assignment read
//! back, the listing of every property), which SQL client of the cluster the command line
//! names, and a `set` standing beside a statement the metastore really answers.
//!
//! Amazon S3 and the Glue Data Catalog are mounted BESIDE Amazon EMR on one engine: what the
//! client printed is read back as a real object the step's own command line uploaded, and the
//! one row carrying a statement reads a table the catalog really holds whose data is a real
//! object of that store. Nothing reaches into any service's state struct.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call of this suite is addressed in.
const REGION: &str = "us-east-1";

/// The bucket every row's command line uploads what the client printed into, and where the
/// one registered table's data lives.
const BUCKET: &str = "emr-sql-session-configuration";

/// The roles a launch names.
const INSTANCE_PROFILE: &str = "EMR_EC2_DefaultRole";
const SERVICE_ROLE: &str = "EMR_DefaultRole";

/// The Glue database and table the one row carrying a statement reads, the prefix its location
/// names and the object its rows live in.
const DATABASE: &str = "emr_session_db";
const TABLE: &str = "orders";
const DATA_PREFIX: &str = "orders";
const DATA_OBJECT: &str = "orders/rows.csv";
const DATA_ROWS: &str = "1\n2\n3\n";

/// The property and value that make a cluster's Hive metastore the Glue Data Catalog
/// (`crate::glue_metastore_steps::GLUE_FACTORY`).
const METASTORE_PROPERTY: &str = "hive.metastore.client.factory.class";

/// A property the launch configures in the Hive classification, and one it configures inside a
/// nested configuration — both are properties of the cluster, so the client's session holds
/// both.
const HIVE_PROPERTY: &str = "hive.exec.parallel";
const HIVE_PROPERTY_VALUE: &str = "false";
const NESTED_PROPERTY: &str = "hive.exec.dynamic.partition.mode";
const NESTED_PROPERTY_VALUE: &str = "nonstrict";

/// A property no configuration of the cluster carries.
const UNSET_PROPERTY: &str = "hive.cli.print.header";

/// How long a poll waits for a cluster or a step to settle: both provisioning phases, a step's
/// own run and one command's transfers, with room to spare on a loaded host.
const DEADLINE: Duration = Duration::from_secs(90);

/// How long a poll sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The states a step never leaves (`com.amazonaws.emr#StepState`).
const TERMINAL: [&str; 4] = ["COMPLETED", "CANCELLED", "FAILED", "INTERRUPTED"];

/// Amazon EMR, the Glue Data Catalog and Amazon S3 on one engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_emr_elasticmapreduce::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_glue_awsglue::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// One `awsJson1_1` call of `service` that must be served.
async fn json_ok(
    router: &MultiServiceRouter,
    service: &str,
    prefix: &str,
    operation: &str,
    body: Value,
) -> Value {
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host.clone())
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261110/{REGION}/{service}/\
                 aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "{operation} must answer 200, got {status} with {payload}"
    );
    payload
}

async fn emr_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    json_ok(router, "elasticmapreduce", "ElasticMapReduce", operation, body).await
}

async fn glue_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    json_ok(router, "glue", "AWSGlue", operation, body).await
}

/// One path-style Amazon S3 request: the status it answered and the bytes it carried.
async fn s3(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> (u16, Vec<u8>) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.clone())
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20261110/{REGION}/s3/\
                 aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    (parts.status.as_u16(), answered.to_vec())
}

/// The object at `key`, or `None` when the store holds none.
async fn object(router: &MultiServiceRouter, key: &str) -> Option<String> {
    let (status, body) = s3(router, "GET", &format!("/{BUCKET}/{key}"), &[]).await;
    (200..300)
        .contains(&status)
        .then(|| String::from_utf8_lossy(&body).to_string())
}

/// Create the bucket, seed the registered table's data, and register that table in the
/// catalog — the way a client prepares a Glue-metastore cluster's catalog.
async fn prepare(router: &MultiServiceRouter) {
    for (case, uri, body) in [
        ("CreateBucket", format!("/{BUCKET}"), ""),
        ("PutObject", format!("/{BUCKET}/{DATA_OBJECT}"), DATA_ROWS),
    ] {
        let (status, answered) = s3(router, "PUT", &uri, body.as_bytes()).await;
        assert!(
            (200..300).contains(&status),
            "{case} must succeed, got {status} {}",
            String::from_utf8_lossy(&answered)
        );
    }
    glue_ok(
        router,
        "CreateDatabase",
        json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    glue_ok(
        router,
        "CreateTable",
        json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": TABLE,
                "StorageDescriptor": {
                    "Location": format!("s3://{BUCKET}/{DATA_PREFIX}/"),
                    "Columns": [{ "Name": "id", "Type": "bigint" }],
                },
            },
        }),
    )
    .await;
}

/// Launch a cluster whose Hive metastore is the Glue Data Catalog, configuring one property in
/// the Hive classification and one inside a nested configuration, and wait for it to come up.
async fn cluster(router: &MultiServiceRouter) -> String {
    let out = emr_ok(
        router,
        "RunJobFlow",
        json!({
            "Name": "sql session configuration",
            "ReleaseLabel": "emr-6.15.0",
            "Applications": [{ "Name": "Hive" }, { "Name": "Spark" }],
            "JobFlowRole": INSTANCE_PROFILE,
            "ServiceRole": SERVICE_ROLE,
            "StepConcurrencyLevel": 10,
            "Configurations": [
                {
                    "Classification": "hive-site",
                    "Properties": {
                        METASTORE_PROPERTY: crate::glue_metastore_steps::GLUE_FACTORY,
                        HIVE_PROPERTY: HIVE_PROPERTY_VALUE,
                    },
                },
                {
                    "Classification": "hive",
                    "Properties": { "maximizeResourceAllocation": "true" },
                    "Configurations": [{
                        "Classification": "hive-env",
                        "Properties": { NESTED_PROPERTY: NESTED_PROPERTY_VALUE },
                    }],
                },
            ],
            "Instances": {
                "InstanceCount": 2,
                "MasterInstanceType": "m5.xlarge",
                "SlaveInstanceType": "m5.xlarge",
                "KeepJobFlowAliveWhenNoSteps": true,
            },
        }),
    )
    .await;
    let cluster_id = out["JobFlowId"]
        .as_str()
        .unwrap_or_else(|| panic!("RunJobFlow must answer a JobFlowId, got {out}"))
        .to_string();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = emr_ok(router, "DescribeCluster", json!({ "ClusterId": &cluster_id })).await;
        let state = described["Cluster"]["Status"]["State"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state == "RUNNING" || state == "WAITING" {
            return cluster_id;
        }
        assert!(
            Instant::now() < deadline,
            "the cluster must reach a state that runs steps, last saw {state:?} in {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Submit one `command-runner.jar` step whose arguments are `args`, answering its step id.
async fn add_step(router: &MultiServiceRouter, cluster_id: &str, name: &str, args: Value) -> String {
    let out = emr_ok(
        router,
        "AddJobFlowSteps",
        json!({
            "JobFlowId": cluster_id,
            "Steps": [{
                "Name": name,
                "ActionOnFailure": "CONTINUE",
                "HadoopJarStep": { "Jar": "command-runner.jar", "Args": args },
            }],
        }),
    )
    .await;
    out["StepIds"][0]
        .as_str()
        .unwrap_or_else(|| panic!("AddJobFlowSteps must answer a StepId, got {out}"))
        .to_string()
}

/// Poll `DescribeStep` until the step is terminal, answering it as it settled.
async fn terminal_step(
    router: &MultiServiceRouter,
    case: &str,
    cluster_id: &str,
    step_id: &str,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let out = emr_ok(
            router,
            "DescribeStep",
            json!({ "ClusterId": cluster_id, "StepId": step_id }),
        )
        .await;
        let step = out["Step"].clone();
        let state = step["Status"]["State"].as_str().unwrap_or_default().to_string();
        if TERMINAL.contains(&state.as_str()) {
            return step;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the step must reach a terminal state, last saw {state:?} in {step}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What a row expects the object its command line uploaded to hold.
enum Holds {
    /// Exactly these bytes.
    Exactly(String),
    /// Every one of these lines, in a listing that may hold others too.
    Lines(Vec<String>),
}

/// One row: the client the command line invokes, the text it is given, and what the object the
/// line uploads must hold.
struct Case {
    label: &'static str,
    client: &'static str,
    statements: String,
    key: &'static str,
    holds: Holds,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "a property the launch configured in the Hive classification",
            client: "hive",
            statements: format!("SET {METASTORE_PROPERTY};"),
            key: "printed/metastore.txt",
            holds: Holds::Exactly(format!(
                "{METASTORE_PROPERTY}={}\n",
                crate::glue_metastore_steps::GLUE_FACTORY
            )),
        },
        Case {
            label: "another configured property, through the cluster's other SQL client",
            client: "spark-sql",
            statements: format!("SET {HIVE_PROPERTY};"),
            key: "printed/parallel.txt",
            holds: Holds::Exactly(format!("{HIVE_PROPERTY}={HIVE_PROPERTY_VALUE}\n")),
        },
        Case {
            label: "a property the launch configured inside a nested configuration",
            client: "hive",
            statements: format!("SET {NESTED_PROPERTY};"),
            key: "printed/nested.txt",
            holds: Holds::Exactly(format!("{NESTED_PROPERTY}={NESTED_PROPERTY_VALUE}\n")),
        },
        Case {
            label: "a property no configuration of the cluster carries",
            client: "hive",
            statements: format!("SET {UNSET_PROPERTY};"),
            key: "printed/unset.txt",
            holds: Holds::Exactly(format!("{UNSET_PROPERTY} is undefined\n")),
        },
        Case {
            label: "an assignment of an unconfigured property, read back",
            client: "hive",
            statements: format!("SET {UNSET_PROPERTY}=true; SET {UNSET_PROPERTY};"),
            key: "printed/assigned.txt",
            holds: Holds::Exactly(format!("{UNSET_PROPERTY}=true\n")),
        },
        Case {
            label: "an assignment over a configured property, read back",
            client: "spark-sql",
            statements: format!("SET {HIVE_PROPERTY}=true; SET {HIVE_PROPERTY};"),
            key: "printed/overridden.txt",
            holds: Holds::Exactly(format!("{HIVE_PROPERTY}=true\n")),
        },
        Case {
            label: "every property the session holds",
            client: "hive",
            statements: "SET;".to_string(),
            key: "printed/all.txt",
            holds: Holds::Lines(vec![
                format!(
                    "{METASTORE_PROPERTY}={}",
                    crate::glue_metastore_steps::GLUE_FACTORY
                ),
                format!("{HIVE_PROPERTY}={HIVE_PROPERTY_VALUE}"),
                format!("{NESTED_PROPERTY}={NESTED_PROPERTY_VALUE}"),
            ]),
        },
        Case {
            label: "a setting beside a statement the metastore answers",
            client: "hive",
            statements: format!("SET {HIVE_PROPERTY}=true; SELECT id FROM {DATABASE}.{TABLE};"),
            key: "printed/with-statement.txt",
            // The assignment prints nothing, so the object holds the rows the statement
            // answered over the data the catalog's table points at.
            holds: Holds::Exactly(DATA_ROWS.to_string()),
        },
    ]
}

/// THE RULE, over the property named, the form of the command and the client that runs it.
#[tokio::test]
async fn a_sql_clients_set_command_runs_out_of_the_clusters_configuration() {
    let router = emulator();
    prepare(&router).await;
    let cluster_id = cluster(&router).await;

    let cases = cases();
    let mut submitted: Vec<(&Case, String)> = Vec::new();
    for case in &cases {
        let step_id = add_step(
            &router,
            &cluster_id,
            case.label,
            json!([
                "bash",
                "-c",
                format!(
                    "{} -e '{}' > /tmp/printed.txt && aws s3 cp /tmp/printed.txt s3://{BUCKET}/{}",
                    case.client, case.statements, case.key
                )
            ]),
        )
        .await;
        submitted.push((case, step_id));
    }

    for (case, step_id) in &submitted {
        let label = case.label;
        let step = terminal_step(&router, label, &cluster_id, step_id).await;
        let state = step["Status"]["State"].as_str();
        assert_eq!(
            state,
            Some("COMPLETED"),
            "case {label:?}: the client ran the command, so the step completes, got \
             {state:?} in {step}"
        );
        let held = object(&router, case.key).await;
        let actual = held.unwrap_or_else(|| {
            panic!("case {label:?}: the command line uploaded what the client printed, but the \
                    store holds nothing at {}", case.key)
        });
        match &case.holds {
            Holds::Exactly(expected) => assert_eq!(
                &actual, expected,
                "case {label:?}: expected {expected:?}, got {actual:?}"
            ),
            Holds::Lines(lines) => {
                for line in lines {
                    assert!(
                        actual.lines().any(|held| held == line),
                        "case {label:?}: the listing must hold {line:?}, got {actual:?}"
                    );
                }
            }
        }
    }
}

/// The same command with no shell around it: a step whose own argument list IS the client's
/// invocation runs it too, so the step completes rather than reporting work nothing ran.
#[tokio::test]
async fn a_bare_client_invocation_carrying_a_setting_completes() {
    let router = emulator();
    prepare(&router).await;
    let cluster_id = cluster(&router).await;

    // Rows differ along the FORM of the command the bare invocation carries: a report of a
    // configured property, a report of one nothing configured, an assignment, and the listing.
    let rows: [(&str, &str); 4] = [
        ("a configured property", METASTORE_PROPERTY),
        ("an unconfigured property", UNSET_PROPERTY),
        ("an assignment", "hive.exec.reducers.max=8"),
        ("every property", ""),
    ];
    let mut submitted: Vec<(&str, String)> = Vec::new();
    for (label, text) in rows {
        let step_id = add_step(
            &router,
            &cluster_id,
            label,
            json!(["hive", "-e", format!("SET {text};")]),
        )
        .await;
        submitted.push((label, step_id));
    }
    for (label, step_id) in &submitted {
        let step = terminal_step(&router, label, &cluster_id, step_id).await;
        let state = step["Status"]["State"].as_str();
        assert_eq!(
            state,
            Some("COMPLETED"),
            "case {label:?}: the client ran the command, so the step completes, got \
             {state:?} in {step}"
        );
    }
}
