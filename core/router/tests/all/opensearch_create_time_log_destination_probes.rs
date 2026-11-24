// SPDX-License-Identifier: CC0-1.0

//! What a `CreateDomain` that enables log publishing WRITES into the destination groups it
//! names — end to end over the real `MultiServiceRouter` with Amazon OpenSearch Service and
//! Amazon CloudWatch Logs mounted on one engine, so every stream asserted is one that
//! service really holds and every answer is what a client sees.
//!
//! The RULE pinned here: **as it resolves the destination of each enabled log type, the
//! service PROBES it — it opens a test stream carrying one test log event in every
//! destination group the account holds, and a second test stream, written under its own
//! service principal, only in those whose CloudWatch Logs resource policies let that
//! principal open a stream and put events; a destination that withholds that grant is then
//! refused `ValidationException` (HTTP 400) with the second stream never written.** So a
//! granted group holds both test streams and an ungranted one only the caller's, which is
//! what lets a client tell a correctly granted destination from one that will never receive
//! the domain's logs.
//!
//! Whether the grant is written ON THE GROUP or as an ACCOUNT-scoped policy naming the group
//! is not part of it: `PutResourcePolicy` creates either ("Creates or updates a resource
//! policy allowing other Amazon Web Services services to put log events to this account" —
//! a request naming no `resourceArn` is account-scoped), and the policies in force for a
//! group are both, so a grant written either way admits the delivery.
//!
//! The rows differ along what the rule generalises over: HOW the grant is written (on the
//! group, on the account, over the group's ARN and over its streams' ARN form), WHICH of the
//! two actions delivery needs it grants, how many log types the request enables, how many
//! groups those types are spread over, whether an option is enabled at all, and both
//! directions of the rule — a destination that admits the service and one that does not,
//! including a request that names one of each.
//!
//! Two crates have to agree for that to hold — Amazon OpenSearch Service decides and writes,
//! Amazon CloudWatch Logs decides what the groups hold afterwards — so the suite lives here.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The error a refused create carries: the operation's modelled 400 for a configuration the
/// service will not provision
/// (`smithy_query(query="operation",operation="CreateDomain")`).
const REFUSAL: &str = "ValidationException";

/// The two streams the probe is made of, as the recorded behaviour names them: the one
/// written under the identity that asked for the publishing, and the one written under the
/// service's own principal. Spelled here rather than imported, so a service that named them
/// differently fails this suite.
const CALLER_PROBE_STREAM: &str = "cust-test-log-stream";
const SERVICE_PROBE_STREAM: &str = "es-test-log-stream";

/// The one event each probe stream carries, worded as the service words it. Fixed text with
/// no variable part, so it reads the same in every destination.
const PROBE_MESSAGE: &str = "Amazon OpenSearch Service created a test log event";

/// The service principal Amazon OpenSearch Service delivers a domain's logs under, which is
/// what a destination group's resource policy has to admit.
const DELIVERY_PRINCIPAL: &str = "es.amazonaws.com";

/// What delivery into a group is made of, and so what a grant has to carry both of: the
/// stream the records are written to is opened, and the records are put.
const CREATE_LOG_STREAM: &str = "logs:CreateLogStream";
const PUT_LOG_EVENTS: &str = "logs:PutLogEvents";

/// Amazon OpenSearch Service and Amazon CloudWatch Logs over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_opensearch_amazonopensearchservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One `restJson1` call to the `es` endpoint, as its status, its `x-amzn-errortype` and its
/// parsed body.
async fn opensearch(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Option<String>, Value) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://es.{REGION}.amazonaws.com{path}"))
        .header("host", format!("es.{REGION}.amazonaws.com"))
        .header("authorization", authorization("es"));
    let payload = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            Bytes::from(body.to_string())
        }
        None => Bytes::new(),
    };
    let request = builder.body(payload).expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, error, parsed)
}

/// One CloudWatch Logs call (`awsJson1_1`, `Logs_20140328.<Operation>`) that must answer 200.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("logs"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch Logs {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// How a row's destination group is granted — or not — to the service: the one thing the
/// rows differ in about the policy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Grant {
    /// No resource policy at all, so nothing admits the service principal.
    None,
    /// A policy attached TO THE GROUP (`PutResourcePolicy` with a `resourceArn`), granting
    /// both delivery actions over the group's streams.
    OnTheGroup,
    /// An ACCOUNT-scoped policy (`PutResourcePolicy` with no `resourceArn`) naming the
    /// group's own ARN in its `Resource` list.
    OnTheAccountByGroupArn,
    /// The same, naming the WILDCARD form of the group's ARN — the resource a
    /// `logs:CreateLogStream` is taken on.
    OnTheAccountByStreamsArn,
    /// An account-scoped policy granting only ONE of the two actions delivery needs, so no
    /// record could arrive.
    OnTheAccountOneActionOnly,
}

impl Grant {
    /// Whether a group granted this way admits the service's own delivery — which is what
    /// decides both the refusal and the second probe stream.
    fn admits_delivery(self) -> bool {
        matches!(
            self,
            Self::OnTheGroup | Self::OnTheAccountByGroupArn | Self::OnTheAccountByStreamsArn
        )
    }
}

/// A log group CloudWatch Logs holds, granted to the service the way `grant` says, answered
/// with the ARN the group reports for itself (never one rebuilt here).
async fn given_group(router: &MultiServiceRouter, case: &str, name: &str, grant: Grant) -> String {
    logs(router, case, "CreateLogGroup", &json!({"logGroupName": name})).await;
    let described = logs(
        router,
        case,
        "DescribeLogGroups",
        &json!({"logGroupNamePrefix": name}),
    )
    .await;
    let reported = described["logGroups"][0]["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the group's arn, got {described}"))
        .to_string();
    let group_arn = reported.strip_suffix(":*").unwrap_or(&reported).to_string();
    let streams_arn = format!("{group_arn}:*");
    let statement = |actions: Vec<&str>, resource: &str| {
        json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {"Service": DELIVERY_PRINCIPAL},
                "Action": actions,
                "Resource": resource,
            }],
        })
        .to_string()
    };
    let put = match grant {
        Grant::None => None,
        Grant::OnTheGroup => Some(json!({
            "policyName": format!("{name}-on-the-group"),
            "policyDocument": statement(vec![CREATE_LOG_STREAM, PUT_LOG_EVENTS], &streams_arn),
            "resourceArn": group_arn.clone(),
        })),
        Grant::OnTheAccountByGroupArn => Some(json!({
            "policyName": format!("{name}-account-group-arn"),
            "policyDocument": statement(vec![CREATE_LOG_STREAM, PUT_LOG_EVENTS], &group_arn),
        })),
        Grant::OnTheAccountByStreamsArn => Some(json!({
            "policyName": format!("{name}-account-streams-arn"),
            "policyDocument": statement(vec![CREATE_LOG_STREAM, PUT_LOG_EVENTS], &streams_arn),
        })),
        Grant::OnTheAccountOneActionOnly => Some(json!({
            "policyName": format!("{name}-account-one-action"),
            "policyDocument": statement(vec![PUT_LOG_EVENTS], &streams_arn),
        })),
    };
    if let Some(request) = put {
        logs(router, case, "PutResourcePolicy", &request).await;
    }
    group_arn
}

/// The streams CloudWatch Logs holds in `group`, each with the messages of its events, read
/// back through that service's own reads.
async fn streams_in(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> BTreeMap<String, Vec<String>> {
    let described = logs(
        router,
        case,
        "DescribeLogStreams",
        &json!({"logGroupName": group}),
    )
    .await;
    let names: Vec<String> = described
        .get("logStreams")
        .and_then(Value::as_array)
        .map(|streams| {
            streams
                .iter()
                .filter_map(|stream| {
                    stream
                        .get("logStreamName")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    let mut held = BTreeMap::new();
    for name in names {
        let events = logs(
            router,
            case,
            "GetLogEvents",
            &json!({"logGroupName": group, "logStreamName": name}),
        )
        .await;
        let messages: Vec<String> = events
            .get("events")
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .filter_map(|event| {
                        event
                            .get("message")
                            .and_then(Value::as_str)
                            .map(ToString::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        held.insert(name, messages);
    }
    held
}

/// What the rule says a destination granted `grant` must hold after the create: the caller's
/// probe stream in every case, the service's one only where the grant admits it, and exactly
/// one test event in each.
fn expected_probe(grant: Grant) -> BTreeMap<String, Vec<String>> {
    let mut expected = BTreeMap::new();
    expected.insert(
        CALLER_PROBE_STREAM.to_string(),
        vec![PROBE_MESSAGE.to_string()],
    );
    if grant.admits_delivery() {
        expected.insert(
            SERVICE_PROBE_STREAM.to_string(),
            vec![PROBE_MESSAGE.to_string()],
        );
    }
    expected
}

/// One destination of a row: the log type published to it, how its group is granted, and
/// whether the option is enabled at all.
struct Destination {
    log_type: &'static str,
    grant: Grant,
    enabled: bool,
}

/// One row of the table: the destinations the create's `LogPublishingOptions` name, and
/// whether the rule refuses the create.
struct Row {
    case: &'static str,
    destinations: Vec<Destination>,
    refused: bool,
}

/// The table. Each row's groups are created per row, so what one row wrote cannot decide
/// another's verdict.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one log type on a group granted by a policy attached to the group",
            destinations: vec![Destination {
                log_type: "ES_APPLICATION_LOGS",
                grant: Grant::OnTheGroup,
                enabled: true,
            }],
            refused: false,
        },
        Row {
            case: "one log type on a group granted by an ACCOUNT-scoped policy naming it",
            destinations: vec![Destination {
                log_type: "ES_APPLICATION_LOGS",
                grant: Grant::OnTheAccountByGroupArn,
                enabled: true,
            }],
            refused: false,
        },
        Row {
            case: "an account-scoped policy naming the group's streams, over a slow log",
            destinations: vec![Destination {
                log_type: "SEARCH_SLOW_LOGS",
                grant: Grant::OnTheAccountByStreamsArn,
                enabled: true,
            }],
            refused: false,
        },
        Row {
            case: "two log types on two groups granted two different ways",
            destinations: vec![
                Destination {
                    log_type: "ES_APPLICATION_LOGS",
                    grant: Grant::OnTheAccountByStreamsArn,
                    enabled: true,
                },
                Destination {
                    log_type: "SEARCH_SLOW_LOGS",
                    grant: Grant::OnTheGroup,
                    enabled: true,
                },
            ],
            refused: false,
        },
        Row {
            case: "three log types, one group of the three granting nothing",
            destinations: vec![
                Destination {
                    log_type: "ES_APPLICATION_LOGS",
                    grant: Grant::OnTheAccountByStreamsArn,
                    enabled: true,
                },
                Destination {
                    log_type: "INDEX_SLOW_LOGS",
                    grant: Grant::None,
                    enabled: true,
                },
                Destination {
                    log_type: "SEARCH_SLOW_LOGS",
                    grant: Grant::OnTheGroup,
                    enabled: true,
                },
            ],
            refused: true,
        },
        Row {
            case: "one log type on a group with no resource policy at all",
            destinations: vec![Destination {
                log_type: "ES_APPLICATION_LOGS",
                grant: Grant::None,
                enabled: true,
            }],
            refused: true,
        },
        Row {
            case: "a grant carrying only one of the two actions delivery needs",
            destinations: vec![Destination {
                log_type: "ES_APPLICATION_LOGS",
                grant: Grant::OnTheAccountOneActionOnly,
                enabled: true,
            }],
            refused: true,
        },
        Row {
            // A DISABLED option publishes nothing, so there is no destination to resolve and
            // nothing to probe: the group stays empty.
            case: "a disabled option on a granted group",
            destinations: vec![Destination {
                log_type: "ES_APPLICATION_LOGS",
                grant: Grant::OnTheGroup,
                enabled: false,
            }],
            refused: false,
        },
    ]
}

/// A `CreateDomain` body for `name`: one small cluster on one release, plus the row's
/// options.
fn create_body(name: &str, options: Value) -> Value {
    json!({
        "DomainName": name,
        "EngineVersion": "OpenSearch_2.11",
        "ClusterConfig": {"InstanceType": "t3.small.search", "InstanceCount": 1},
        "EBSOptions": {"EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10},
        "LogPublishingOptions": options,
    })
}

#[tokio::test]
async fn a_create_probes_every_destination_it_enables() {
    let router = build_router();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        // One group per destination, so each row's assertions are about groups only this row
        // configured.
        let mut options = serde_json::Map::new();
        let mut groups: Vec<(&str, String, Grant, bool)> = Vec::new();
        for destination in &row.destinations {
            let name = format!(
                "/aws/opensearch/probe-{index}/{}",
                destination.log_type.to_ascii_lowercase()
            );
            let arn = given_group(&router, case, &name, destination.grant).await;
            options.insert(
                destination.log_type.to_string(),
                json!({
                    "Enabled": destination.enabled,
                    "CloudWatchLogsLogGroupArn": arn.clone(),
                }),
            );
            groups.push((destination.log_type, name, destination.grant, destination.enabled));
        }

        let domain = format!("probe-domain-{index}");
        let body = create_body(&domain, Value::Object(options));
        let (status, error, answered) =
            opensearch(&router, "POST", "/2021-01-01/opensearch/domain", Some(&body)).await;
        if row.refused {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the create to be refused with HTTP 400, got {status} \
                 with {answered}"
            );
            assert_eq!(
                error.as_deref(),
                Some(REFUSAL),
                "case {case:?}: expected the error type {REFUSAL}, got {error:?} with {answered}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to be accepted, got {status} with {answered}"
            );
        }

        for (log_type, group, grant, enabled) in &groups {
            let held = streams_in(&router, case, group).await;
            let expected = if *enabled {
                expected_probe(*grant)
            } else {
                BTreeMap::new()
            };
            assert_eq!(
                held, expected,
                "case {case:?}: expected the {log_type} destination {group:?} (granted \
                 {grant:?}) to hold {expected:?}, got {held:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_create_refused_over_an_unresolved_destination_probes_nothing() {
    // RULE, the other side of "as a create RESOLVES a destination it probes it": a log group
    // the account holds nothing under is a destination nothing resolved, so a create naming
    // one is refused before that step and the destination that IS there stays empty.
    let case = "one granted group beside a destination the account holds no group under";
    let router = build_router();
    let granted_group = "/aws/opensearch/probe-unresolved-granted";
    let granted = given_group(&router, case, granted_group, Grant::OnTheGroup).await;
    // A well-formed ARN of a group this account no longer holds: read off a group CloudWatch
    // Logs created, which is then deleted, so the ARN is that service's own rather than one
    // written here.
    let missing_group = "/aws/opensearch/probe-unresolved-missing";
    let missing = given_group(&router, case, missing_group, Grant::None).await;
    logs(
        &router,
        case,
        "DeleteLogGroup",
        &json!({"logGroupName": missing_group}),
    )
    .await;

    let body = create_body(
        "probe-domain-unresolved",
        json!({
            "ES_APPLICATION_LOGS": {"Enabled": true, "CloudWatchLogsLogGroupArn": granted},
            "INDEX_SLOW_LOGS": {"Enabled": true, "CloudWatchLogsLogGroupArn": missing},
        }),
    );
    let (status, error, answered) =
        opensearch(&router, "POST", "/2021-01-01/opensearch/domain", Some(&body)).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the create to be refused with HTTP 400, got {status} with \
         {answered}"
    );
    assert_eq!(
        error.as_deref(),
        Some(REFUSAL),
        "case {case:?}: expected the error type {REFUSAL}, got {error:?} with {answered}"
    );
    let held = streams_in(&router, case, granted_group).await;
    assert_eq!(
        held,
        BTreeMap::new(),
        "case {case:?}: expected the granted group to hold nothing, got {held:?}"
    );
}

#[tokio::test]
async fn a_destination_already_probed_is_not_probed_again() {
    // RULE, the same one once more: the probe is the PROOF that the destination takes the
    // writer's events, so a group named by two enabled log types, and a group a SECOND
    // create's own probe reaches again, holds exactly one test event per probe stream rather
    // than one per option or per call.
    let case = "one granted group probed by a refused create and then by an accepted one";
    let router = build_router();
    let granted_group = "/aws/opensearch/probe-shared";
    let ungranted_group = "/aws/opensearch/probe-shared-ungranted";
    let granted =
        given_group(&router, case, granted_group, Grant::OnTheAccountByStreamsArn).await;
    let ungranted = given_group(&router, case, ungranted_group, Grant::None).await;

    // The first create names both groups, so its destinations ARE resolved and probed, and
    // it is then refused on the ungranted one.
    let refused = create_body(
        "probe-domain-refused",
        json!({
            "ES_APPLICATION_LOGS": {"Enabled": true, "CloudWatchLogsLogGroupArn": granted.clone()},
            "INDEX_SLOW_LOGS": {"Enabled": true, "CloudWatchLogsLogGroupArn": ungranted},
        }),
    );
    let (status, error, answered) = opensearch(
        &router,
        "POST",
        "/2021-01-01/opensearch/domain",
        Some(&refused),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the first create to be refused, got {status} with {answered}"
    );
    assert_eq!(
        error.as_deref(),
        Some(REFUSAL),
        "case {case:?}: expected the error type {REFUSAL}, got {error:?} with {answered}"
    );

    // A SECOND create — another domain, so it is accepted rather than resolving to the one
    // the first create named — probes the same granted group again, this time through the
    // accepted path, with two log types naming it.
    let accepted = create_body(
        "probe-domain-accepted",
        json!({
            "ES_APPLICATION_LOGS": {"Enabled": true, "CloudWatchLogsLogGroupArn": granted.clone()},
            "SEARCH_SLOW_LOGS": {"Enabled": true, "CloudWatchLogsLogGroupArn": granted},
        }),
    );
    let (status, _, answered) = opensearch(
        &router,
        "POST",
        "/2021-01-01/opensearch/domain",
        Some(&accepted),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the second create to be accepted, got {status} with {answered}"
    );

    let held = streams_in(&router, case, granted_group).await;
    let expected = expected_probe(Grant::OnTheAccountByStreamsArn);
    assert_eq!(
        held, expected,
        "case {case:?}: expected {expected:?}, got {held:?}"
    );
    // And the group that grants nothing still holds the caller's probe alone, once.
    let held = streams_in(&router, case, ungranted_group).await;
    let expected = expected_probe(Grant::None);
    assert_eq!(
        held, expected,
        "case {case:?}: expected the ungranted group to hold {expected:?}, got {held:?}"
    );
}
