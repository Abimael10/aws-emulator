// SPDX-License-Identifier: CC0-1.0

//! Where a kafkaconnect CONNECTOR's bring-up ends, and what its workers write while it
//! happens — with Amazon MSK and CloudWatch Logs mounted beside kafkaconnect on ONE engine,
//! so each half is asserted where a client reads it.
//!
//! RULE 1. A connector's workers connect to the Apache Kafka cluster its
//! `kafkaCluster.apacheKafkaCluster.bootstrapServers` name, so the connector leaves
//! `CREATING` for `RUNNING` only when a cluster of this account is bootstrapped at one of
//! those addresses, and for `FAILED` — a RESTING state, with a `stateDescription` saying no
//! cluster was reachable there — when it is not. Which clusters the account holds is Amazon
//! MSK's own fact: the service that owns them is asked where each one is bootstrapped
//! (`GetBootstrapBrokers`), and a connector pointed at an address none of them publishes has
//! no cluster to run against.
//!
//! RULE 2. A connector whose `logDelivery.workerLogDelivery.cloudWatchLogs` is ENABLED and
//! names a log group delivers, into that group, one log event per resting state its workers
//! reach — naming the connector and what happened to it, so a failed bring-up's reason is
//! readable from outside the connector's VPC. A destination that is not enabled, or a group
//! the account does not hold, is written to not at all and is not created.
//!
//! The rows span what the rules generalise over: the address a connector is pointed at (the
//! one a held cluster publishes, that address among several, the same host on a port nothing
//! publishes, an MSK-shaped name of a cluster nobody created, and a name outside AWS's
//! namespace), crossed for the log rules with the outcome the bring-up reaches, whether
//! delivery is enabled, and whether the destination group exists.
//!
//! Every wait below LOOKS AGAIN until the condition it needs holds, on a generous deadline,
//! and every assertion that something stays ABSENT is looked at repeatedly across its span:
//! nothing here is asserted about elapsed time, so a row reports what the emulator did and
//! never how fast the host it ran on happened to be.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_kafkaconnect_kafkaconnect::state::UNREACHABLE_KAFKA_CLUSTER_CODE;

/// The signing region every call below is scoped to, and the region every minted ARN
/// carries.
const REGION: &str = "us-east-1";

/// The canonical emulator account id every ARN carries.
const ACCOUNT: &str = "000000000000";

/// How long a wait for something the emulator's clock owes is given before a row is a
/// failure.
///
/// Generous rather than tight: every wait looks again until its condition holds, so a row
/// whose work was done spends none of this, and a host running this suite beside every other
/// one is slow at everything.
const DEADLINE: Duration = Duration::from_secs(60);

/// How often a wait looks again.
const PROBE: Duration = Duration::from_millis(50);

/// How many times an assertion that something stays absent looks, and how long it waits
/// between looks: the span is spent only by the rows that assert an absence.
const QUIET_LOOKS: usize = 6;

/// kafkaconnect, plus the two services these rules are observed through: the Amazon MSK that
/// owns this account's Apache Kafka clusters, and the CloudWatch Logs that holds the group a
/// connector's `logDelivery` names. One engine, as `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_kafkaconnect_kafkaconnect::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_kafka_kafka::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// One `restJson1` call against the service whose signing name is `signing_name`.
async fn rest(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    body: Value,
) -> (u16, Value) {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(&body).expect("the request body serializes")
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com{path}"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("the request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One kafkaconnect call.
async fn kafkaconnect(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Value,
) -> (u16, Value) {
    rest(router, "kafkaconnect", method, path, body).await
}

/// One Amazon MSK call.
async fn kafka(router: &MultiServiceRouter, method: &str, path: &str, body: Value) -> (u16, Value) {
    rest(router, "kafka", method, path, body).await
}

/// One CloudWatch Logs call, over the `awsJson` path that service serves.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/logs/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One ARN as a single non-greedy `@httpLabel` path segment, as an SDK sends it: its `%`,
/// `:` and `/` travel percent-encoded.
fn label(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

/// A member of a JSON object as a string, or `""`.
fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A custom plugin this emulator holds, once the clock has made it ACTIVE — the state a
/// connector may reference it in. Amazon S3 is not mounted here, so nothing is established
/// about the plugin's file and the create is accepted, which is the position every
/// kafkaconnect suite without S3 is in.
async fn active_plugin(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = kafkaconnect(
        router,
        "POST",
        "/v1/custom-plugins",
        json!({
            "name": name,
            "contentType": "ZIP",
            "location": {"s3Location": {
                "bucketArn": "arn:aws:s3:::connector-plugins",
                "fileKey": "plugins/p.zip",
            }},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name:?}: the custom plugin must be created, got {status} {created}"
    );
    let arn = text(&created, "customPluginArn");
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, described) = kafkaconnect(
            router,
            "GET",
            &format!("/v1/custom-plugins/{}", label(&arn)),
            Value::Null,
        )
        .await;
        if status == 200 && text(&described, "customPluginState") == "ACTIVE" {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {name:?}: the plugin never became ACTIVE, last read {status} {described}"
        );
        tokio::time::sleep(PROBE).await;
    }
}

/// An Apache Kafka cluster this account really holds, answered with the address Amazon MSK
/// publishes for it: the first `host:port` of its plaintext bootstrap string, read back the
/// way a client that is about to point a connector at a cluster reads it.
async fn published_bootstrap(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = kafka(
        router,
        "POST",
        "/v1/clusters",
        json!({
            "clusterName": name,
            "kafkaVersion": "3.9.x",
            "numberOfBrokerNodes": 3,
            "brokerNodeGroupInfo": {
                "instanceType": "kafka.m5.large",
                "clientSubnets": ["subnet-a", "subnet-b", "subnet-c"],
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name:?}: the Kafka cluster must be created, got {status} {created}"
    );
    let cluster_arn = created
        .get("clusterArn")
        .or_else(|| created.get("ClusterArn"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        !cluster_arn.is_empty(),
        "case {name:?}: the create must answer a cluster ARN, got {created}"
    );
    // `GetBootstrapBrokers` publishes nothing while the cluster is still coming up, so this
    // looks again until it does.
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, published) = kafka(
            router,
            "GET",
            &format!("/v1/clusters/{}/bootstrap-brokers", label(&cluster_arn)),
            Value::Null,
        )
        .await;
        if let Some(address) = published
            .get("BootstrapBrokerString")
            .or_else(|| published.get("bootstrapBrokerString"))
            .and_then(Value::as_str)
        {
            let first = address.split(',').next().unwrap_or(address).trim();
            assert!(
                !first.is_empty(),
                "case {name:?}: the cluster published an empty bootstrap address, got {published}"
            );
            return first.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {name:?}: the cluster never published a bootstrap address, last read {status} \
             {published}"
        );
        tokio::time::sleep(PROBE).await;
    }
}

/// A `CreateConnector` body pointed at `bootstrap_servers`, carrying `log_delivery` when one
/// is given.
fn connector_body(
    name: &str,
    plugin_arn: &str,
    bootstrap_servers: &str,
    log_delivery: Option<Value>,
) -> Value {
    let mut body = json!({
        "connectorName": name,
        "capacity": {"provisionedCapacity": {"mcuCount": 1, "workerCount": 2}},
        "connectorConfiguration": {
            "connector.class": "io.confluent.connect.s3.S3SinkConnector",
            "tasks.max": "1",
        },
        "kafkaCluster": {"apacheKafkaCluster": {
            "bootstrapServers": bootstrap_servers,
            "vpc": {"subnets": ["subnet-a", "subnet-b"], "securityGroups": ["sg-a"]},
        }},
        "kafkaClusterClientAuthentication": {"authenticationType": "IAM"},
        "kafkaClusterEncryptionInTransit": {"encryptionType": "TLS"},
        "kafkaConnectVersion": "2.7.1",
        "plugins": [{"customPlugin": {"customPluginArn": plugin_arn, "revision": 1}}],
        "serviceExecutionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/connector-role"),
    });
    if let Some(log_delivery) = log_delivery {
        body["logDelivery"] = log_delivery;
    }
    body
}

/// Create one connector and answer its ARN, asserting the create itself is served and
/// answers the transient state the bring-up starts in.
async fn create_connector(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    plugin_arn: &str,
    bootstrap_servers: &str,
    log_delivery: Option<Value>,
) -> String {
    let (status, created) = kafkaconnect(
        router,
        "POST",
        "/v1/connectors",
        connector_body(name, plugin_arn, bootstrap_servers, log_delivery),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the connector must be created, got {status} {created}"
    );
    assert_eq!(
        text(&created, "connectorState"),
        "CREATING",
        "case {case:?}: a create answers the state its bring-up starts in, got {created}"
    );
    text(&created, "connectorArn")
}

/// `DescribeConnector` for `connector_arn`.
async fn describe(router: &MultiServiceRouter, connector_arn: &str) -> (u16, Value) {
    kafkaconnect(
        router,
        "GET",
        &format!("/v1/connectors/{}", label(connector_arn)),
        Value::Null,
    )
    .await
}

/// Look again until the connector has left `CREATING`, and answer what it reads back then.
async fn rested(router: &MultiServiceRouter, case: &str, connector_arn: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, described) = describe(router, connector_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the connector must read back, got {status} {described}"
        );
        if text(&described, "connectorState") != "CREATING" {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the connector never left CREATING, last read {described}"
        );
        tokio::time::sleep(PROBE).await;
    }
}

/// Every log event message the group `log_group` holds, over every stream in it.
async fn delivered_messages(router: &MultiServiceRouter, log_group: &str) -> Vec<String> {
    let (status, streams) = logs(
        router,
        "DescribeLogStreams",
        json!({"logGroupName": log_group}),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    let mut messages = Vec::new();
    for stream in streams["logStreams"].as_array().cloned().unwrap_or_default() {
        let name = text(&stream, "logStreamName");
        let (status, events) = logs(
            router,
            "GetLogEvents",
            json!({"logGroupName": log_group, "logStreamName": name}),
        )
        .await;
        if status != 200 {
            continue;
        }
        for event in events["events"].as_array().cloned().unwrap_or_default() {
            messages.push(text(&event, "message"));
        }
    }
    messages
}

/// Look again until the group `log_group` holds at least one log event, and answer every
/// message in it.
async fn delivered(router: &MultiServiceRouter, case: &str, log_group: &str) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let messages = delivered_messages(router, log_group).await;
        if !messages.is_empty() {
            return messages;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no log event ever reached the group {log_group:?}"
        );
        tokio::time::sleep(PROBE).await;
    }
}

/// A log group this account holds.
async fn given_log_group(router: &MultiServiceRouter, name: &str) {
    let (status, created) = logs(router, "CreateLogGroup", json!({"logGroupName": name})).await;
    assert_eq!(
        status, 200,
        "case {name:?}: the log group must be created, got {status} {created}"
    );
}

/// The `logDelivery` a connector carries for the CloudWatch Logs group `log_group`.
fn cloud_watch_logs(enabled: bool, log_group: &str) -> Value {
    json!({"workerLogDelivery": {"cloudWatchLogs": {"enabled": enabled, "logGroup": log_group}}})
}

/// One row of the resting-state table: the address the connector is pointed at, and the
/// state its bring-up must come to rest in.
struct Row {
    case: &'static str,
    /// How the row's `bootstrapServers` is built from the address the held cluster
    /// publishes.
    address: fn(&str) -> String,
    /// Whether the bring-up reaches the cluster.
    reaches_the_cluster: bool,
}

/// The rows, spanning what the rule generalises over: the published address itself, that
/// address among others, the same host on a port nothing publishes, an MSK-shaped name of a
/// cluster nobody created, and a name outside AWS's namespace altogether.
const ROWS: &[Row] = &[
    Row {
        case: "the address the held cluster publishes",
        address: |published| published.to_string(),
        reaches_the_cluster: true,
    },
    Row {
        case: "the published address among others, as a broker list is written",
        address: |published| format!("b-9.unheld.example:9092,{published}"),
        reaches_the_cluster: true,
    },
    Row {
        case: "the published address with space around it, as a client may write a list",
        address: |published| format!(" {published} "),
        reaches_the_cluster: true,
    },
    Row {
        case: "the published host on a port nothing is bootstrapped at",
        address: |published| {
            let (host, port) = published
                .rsplit_once(':')
                .expect("a published address is a host:port pair");
            let port: u16 = port.parse().expect("a published port is a number");
            format!("{host}:{}", port.wrapping_add(1))
        },
        reaches_the_cluster: false,
    },
    Row {
        case: "an MSK-shaped address of a cluster nobody created",
        address: |_| format!("b-1.unheld.c2.kafka.{REGION}.amazonaws.com:9098"),
        reaches_the_cluster: false,
    },
    Row {
        case: "a name outside AWS's namespace",
        address: |_| "broker-1.internal:9092".to_string(),
        reaches_the_cluster: false,
    },
];

#[tokio::test]
async fn a_connector_rests_running_only_when_its_kafka_cluster_is_there() {
    let router = emulator();
    let plugin = active_plugin(&router, "bring-up-plugin").await;
    let published = published_bootstrap(&router, "bring-up-cluster").await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let bootstrap = (row.address)(&published);
        let name = format!("bring-up-{index}");
        let connector_arn =
            create_connector(&router, case, &name, &plugin, &bootstrap, None).await;

        let described = rested(&router, case, &connector_arn).await;
        let state = text(&described, "connectorState");
        let expected = if row.reaches_the_cluster {
            "RUNNING"
        } else {
            "FAILED"
        };
        assert_eq!(
            state, expected,
            "case {case:?}: expected the connector to come to rest {expected}, got {state:?} \
             ({described})"
        );

        if row.reaches_the_cluster {
            // A bring-up that reached its cluster has nothing to explain.
            assert_eq!(
                described["stateDescription"],
                Value::Null,
                "case {case:?}: a running connector describes no state reason, got {described}"
            );
            continue;
        }

        // A failed bring-up says WHY, which is the only signal a client outside the
        // connector's VPC has: the code of the condition, and a message naming the
        // addresses its workers were given.
        let description = &described["stateDescription"];
        let message = text(description, "message");
        assert_eq!(
            text(description, "code"),
            UNREACHABLE_KAFKA_CLUSTER_CODE,
            "case {case:?}: expected the unreachable-cluster code, got {described}"
        );
        assert!(
            !message.is_empty() && message.contains(bootstrap.trim()),
            "case {case:?}: expected a message naming the bootstrap servers {bootstrap:?}, got \
             {message:?}"
        );

        // FAILED is a RESTING state: the clock owes the connector nothing further, so it
        // stays there, and the calls only a connector with running workers can serve are
        // refused for it in the one sentence that names the states that would be served.
        for look in 0..QUIET_LOOKS {
            let (status, described) = describe(&router, &connector_arn).await;
            let state = text(&described, "connectorState");
            assert_eq!(
                status, 200,
                "case {case:?}: look {look}: the failed connector must still read back, got \
                 {status} {described}"
            );
            assert_eq!(
                state, "FAILED",
                "case {case:?}: look {look}: FAILED is a resting state, got {state:?}"
            );
            tokio::time::sleep(PROBE).await;
        }
        let (status, refused) = kafkaconnect(
            &router,
            "POST",
            &format!("/v1/connectors/{}/restart", label(&connector_arn)),
            Value::Null,
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a failed connector has no workers to restart, got {status} {refused}"
        );
        assert_eq!(
            text(&refused, "message"),
            "Invalid parameter connectorArn: This operation is only valid for resources that are \
             in one of the following states: [RUNNING, PAUSED]",
            "case {case:?}: expected the state gate's refusal, got {refused}"
        );

        // And the listing agrees with the read: one stored connector, one state.
        let (status, listed) = kafkaconnect(
            &router,
            "GET",
            &format!("/v1/connectors?connectorNamePrefix={name}"),
            Value::Null,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the listing must be served, got {status} {listed}"
        );
        let states: Vec<String> = listed["connectors"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|summary| text(summary, "connectorState"))
            .collect();
        assert_eq!(
            states,
            vec!["FAILED".to_string()],
            "case {case:?}: the listing must report the state the read does, got {listed}"
        );
    }
}

#[tokio::test]
async fn a_failed_connectors_worker_log_event_reaches_the_group_it_names() {
    let router = emulator();
    let plugin = active_plugin(&router, "worker-log-failed-plugin").await;
    let case = "a failed bring-up with CloudWatch Logs enabled";
    let group = "/aws/kafkaconnect/failed";
    given_log_group(&router, group).await;

    let connector_arn = create_connector(
        &router,
        case,
        "worker-log-failed",
        &plugin,
        "broker-1.internal:9092",
        Some(cloud_watch_logs(true, group)),
    )
    .await;
    let described = rested(&router, case, &connector_arn).await;
    assert_eq!(
        text(&described, "connectorState"),
        "FAILED",
        "case {case:?}: the row needs a bring-up that failed, got {described}"
    );
    let reason = text(&described["stateDescription"], "message");

    let messages = delivered(&router, case, group).await;
    let about_the_connector: Vec<&String> = messages
        .iter()
        .filter(|message| message.contains("worker-log-failed"))
        .collect();
    assert!(
        !about_the_connector.is_empty(),
        "case {case:?}: expected a worker log event naming the connector, got {messages:?}"
    );
    assert!(
        about_the_connector
            .iter()
            .any(|message| message.contains("ERROR") && message.contains(&reason)),
        "case {case:?}: expected the failure the connector reports to be readable in the group, \
         got {about_the_connector:?} against {reason:?}"
    );

    // The connector's `logDelivery` is reported back as it was stored, so a client can see
    // where the records it is reading came from.
    assert_eq!(
        described["logDelivery"]["workerLogDelivery"]["cloudWatchLogs"],
        json!({"enabled": true, "logGroup": group}),
        "case {case:?}: the stored log delivery must read back, got {described}"
    );
}

#[tokio::test]
async fn a_running_connectors_worker_log_event_reaches_the_group_it_names() {
    let router = emulator();
    let plugin = active_plugin(&router, "worker-log-running-plugin").await;
    let published = published_bootstrap(&router, "worker-log-cluster").await;
    let case = "a bring-up that reached its cluster, with CloudWatch Logs enabled";
    let group = "/aws/kafkaconnect/running";
    given_log_group(&router, group).await;

    let connector_arn = create_connector(
        &router,
        case,
        "worker-log-running",
        &plugin,
        &published,
        Some(cloud_watch_logs(true, group)),
    )
    .await;
    let described = rested(&router, case, &connector_arn).await;
    assert_eq!(
        text(&described, "connectorState"),
        "RUNNING",
        "case {case:?}: the row needs a bring-up that reached its cluster, got {described}"
    );

    let messages = delivered(&router, case, group).await;
    assert!(
        messages
            .iter()
            .any(|message| message.contains("worker-log-running") && message.contains("RUNNING")),
        "case {case:?}: expected a worker log event reporting the connector running, got \
         {messages:?}"
    );
}

#[tokio::test]
async fn a_destination_a_connector_did_not_ask_for_is_never_written_to() {
    let router = emulator();
    let plugin = active_plugin(&router, "worker-log-off-plugin").await;

    // Two rows, each asserting an ABSENCE: delivery turned OFF for a group the account
    // holds, and delivery turned on for a group the account does not hold — which stays
    // absent, because the destination is the caller's to create.
    let held = "/aws/kafkaconnect/disabled";
    let absent = "/aws/kafkaconnect/never-created";
    given_log_group(&router, held).await;

    let rows: &[(&str, &str, bool)] = &[
        ("delivery turned off for a group the account holds", held, false),
        ("delivery turned on for a group the account does not hold", absent, true),
    ];
    for (index, (case, group, enabled)) in rows.iter().enumerate() {
        let connector_arn = create_connector(
            &router,
            case,
            &format!("worker-log-off-{index}"),
            &plugin,
            "broker-1.internal:9092",
            Some(cloud_watch_logs(*enabled, group)),
        )
        .await;
        let described = rested(&router, case, &connector_arn).await;
        assert_eq!(
            text(&described, "connectorState"),
            "FAILED",
            "case {case:?}: the bring-up must still reach its resting state, got {described}"
        );

        // Looked at repeatedly: a record the emulator was going to deliver would have
        // arrived within the span a delivered one arrives in.
        for look in 0..QUIET_LOOKS {
            let messages = delivered_messages(&router, group).await;
            assert!(
                messages.is_empty(),
                "case {case:?}: look {look}: expected no log event in {group:?}, got {messages:?}"
            );
            // A further read of the connector is where a held record would be handed over,
            // so the absence is asserted after one.
            let (status, _) = describe(&router, &connector_arn).await;
            assert_eq!(
                status, 200,
                "case {case:?}: look {look}: the connector must still read back"
            );
            tokio::time::sleep(PROBE).await;
        }
    }

    // And the group nobody created is still not there: a destination a service was merely
    // pointed at is not one it brings into existence.
    let (status, described) = logs(
        &router,
        "DescribeLogGroups",
        json!({"logGroupNamePrefix": absent}),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeLogGroups must be served, got {status} {described}"
    );
    let names: Vec<String> = described["logGroups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|group| text(group, "logGroupName"))
        .collect();
    assert!(
        !names.iter().any(|name| name == absent),
        "the destination nobody created must stay absent, got {names:?}"
    );
}
