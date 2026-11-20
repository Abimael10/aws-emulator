// SPDX-License-Identifier: CC0-1.0

//! The `AWS/Cognito` sign-in series as a client that WATCHES it reads it back: through both of
//! CloudWatch's metric reads, at whatever granularity the read aggregates into, and over a
//! window that really spans the traffic.
//!
//! The rule under test: a call that terminates a sign-in against an app client STORES its
//! outcome as one datapoint, stamped at the instant of the attempt, in that pool's
//! `SignInSuccesses` series keyed on the dimension pair `UserPool` + `UserPoolClient` — so
//! every metric read over a window holding those instants reports the same totals, `Sum` the
//! completions and `SampleCount` the attempts, whichever of the two read APIs asks
//! (`GetMetricStatistics`, which folds the series into period windows, and `GetMetricData`,
//! which answers a query array) and whatever period it folds into; and a window that does NOT
//! hold those instants reports none of them. A refusal raised by the app client's own flow
//! configuration — before any credential is checked — terminates the attempt like any other
//! refusal, so it is a zero-valued sample and not a missing one.
//!
//! The rows span what the rule generalises over rather than one recorded read: HOW MANY
//! attempts the app client's flow gate refused before it was reconfigured and how many
//! completed after (including none of either), WHICH read API asks, and WHICH period the read
//! folds the window into (a one-minute fold, a five-minute one and an hour, so the same
//! samples are reported as several datapoints or as one).
//!
//! Both services are mounted on ONE engine exactly as the assembled emulator mounts them,
//! which is what makes the store the pool writes the same one CloudWatch reads.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// The `X-Amz-Target` prefix of this service, which is its Smithy shape's local name.
const COGNITO_TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// The `X-Amz-Target` prefix CloudWatch serves its `awsJson1_0` wire under (that crate's
/// OPERATIONS.md "Wire protocol").
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// The namespace a user pool's own metrics live in, and the counter a sign-in is observed in.
const NAMESPACE: &str = "AWS/Cognito";
const SIGN_IN_SUCCESSES: &str = "SignInSuccesses";

/// The dimension names the authentication counters are keyed on.
const USER_POOL: &str = "UserPool";
const USER_POOL_CLIENT: &str = "UserPoolClient";

/// A password satisfying a default pool's password policy, and the user that holds it.
const GOOD_PASSWORD: &str = "Passw0rd!2024";
const USERNAME: &str = "series-reader";

/// The `ExplicitAuthFlows` the app client admits BEFORE it is reconfigured: everything except
/// the client-side password flow the attempts ask for, so the gate refuses them
/// (`CreateUserPoolClient.ExplicitAuthFlows`).
const BLOCKED_FLOWS: [&str; 2] = ["ALLOW_REFRESH_TOKEN_AUTH", "ALLOW_USER_SRP_AUTH"];

/// The same list with the flow the attempts ask for added, which is what the reconfiguration
/// stores so the next attempt is served.
const REPAIRED_FLOWS: [&str; 3] = [
    "ALLOW_REFRESH_TOKEN_AUTH",
    "ALLOW_USER_SRP_AUTH",
    "ALLOW_USER_PASSWORD_AUTH",
];

/// The periods a read folds the window into. Each is a whole number of minutes, the resolution
/// a metric has unless its publisher asked for the high-resolution one
/// (`MetricDatum$StorageResolution`).
const PERIODS: [i64; 3] = [60, 300, 3600];

/// How far either side of the traffic the read window reaches, so it holds every attempt's
/// instant whatever minute the calls land in.
const WINDOW_SLACK_SECS: i64 = 1_200;

/// One emulator with this service and CloudWatch mounted on ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(cloudwatch::EmulatedService::new(engine));
    router.set_request_authentication(false);
    router
}

/// Wall-clock now as epoch seconds, the clock a metric window is measured against.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// Dispatch one `awsJson` call and return its status and JSON body.
async fn json_rpc(
    router: &MultiServiceRouter,
    host: &str,
    signing_name: &str,
    content_type: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/us-east-1/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// One cognito-idp call, as an SDK drives it.
async fn cognito(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    json_rpc(
        router,
        "cognito-idp.us-east-1.amazonaws.com",
        "cognito-idp",
        "application/x-amz-json-1.1",
        &format!("{COGNITO_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await
}

/// The same call, requiring success.
async fn cognito_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = cognito(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// One CloudWatch call, requiring success.
async fn cloudwatch_ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = json_rpc(
        router,
        "monitoring.us-east-1.amazonaws.com",
        "monitoring",
        "application/x-amz-json-1.0",
        &format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// The dimension pair the counters are keyed on, as a read states it.
fn keyed(user_pool_id: &str, client_id: &str) -> Value {
    json!([
        { "Name": USER_POOL, "Value": user_pool_id },
        { "Name": USER_POOL_CLIENT, "Value": client_id },
    ])
}

/// Which read API a row asks through — the two reads a dashboard or an alarm is built on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadPath {
    /// `GetMetricStatistics`, which folds one series into period windows.
    Statistics,
    /// `GetMetricData`, which answers an array of queries.
    Data,
}

/// The window a read spans: the `(StartTime, EndTime)` around the traffic that opened at
/// `opened`, each at a whole minute ("Start time less than 15 days ago - Round down to the
/// nearest whole minute", `GetMetricDataInput$StartTime`).
fn window(opened: i64) -> (i64, i64) {
    let start = (opened - WINDOW_SLACK_SECS) / 60 * 60;
    let end = (now_secs() + WINDOW_SLACK_SECS + 59) / 60 * 60;
    (start, end)
}

/// The `(Sum, SampleCount)` totals one read reports for the series over `(start, end)` at
/// `period`, summed over every datapoint the window folded into — and how many datapoints
/// carried them.
async fn totals(
    router: &MultiServiceRouter,
    path: ReadPath,
    dimensions: &Value,
    period: i64,
    (start, end): (i64, i64),
) -> (f64, f64, usize) {
    match path {
        ReadPath::Statistics => {
            let answered = cloudwatch_ok(
                router,
                "GetMetricStatistics",
                &json!({
                    "Namespace": NAMESPACE,
                    "MetricName": SIGN_IN_SUCCESSES,
                    "Dimensions": dimensions,
                    "StartTime": start,
                    "EndTime": end,
                    "Period": period,
                    "Statistics": ["Sum", "SampleCount"],
                }),
            )
            .await;
            let points = answered["Datapoints"].as_array().cloned().unwrap_or_default();
            let total = |member: &str| -> f64 {
                points
                    .iter()
                    .filter_map(|point| point.get(member).and_then(Value::as_f64))
                    .sum()
            };
            (total("Sum"), total("SampleCount"), points.len())
        }
        ReadPath::Data => {
            // No `Unit` is named, so the read selects the datapoints whatever unit they were
            // published with ("If you omit Unit, all data that was collected with any unit is
            // returned", `GetMetricDataInput`).
            let query = |id: &str, stat: &str| {
                json!({
                    "Id": id,
                    "MetricStat": {
                        "Metric": {
                            "Namespace": NAMESPACE,
                            "MetricName": SIGN_IN_SUCCESSES,
                            "Dimensions": dimensions,
                        },
                        "Period": period,
                        "Stat": stat,
                    },
                    "ReturnData": true,
                })
            };
            let answered = cloudwatch_ok(
                router,
                "GetMetricData",
                &json!({
                    "StartTime": start,
                    "EndTime": end,
                    "MetricDataQueries": [query("sum", "Sum"), query("samples", "SampleCount")],
                }),
            )
            .await;
            let results = answered["MetricDataResults"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let values = |id: &str| -> Vec<f64> {
                results
                    .iter()
                    .filter(|result| result["Id"] == json!(id))
                    .flat_map(|result| {
                        result["Values"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default()
                            .iter()
                            .filter_map(Value::as_f64)
                            .collect::<Vec<f64>>()
                    })
                    .collect()
            };
            let sums = values("sum");
            let samples = values("samples");
            (
                sums.iter().sum(),
                samples.iter().sum(),
                sums.len().max(samples.len()),
            )
        }
    }
}

/// Create a user pool holding one `CONFIRMED` user with [`GOOD_PASSWORD`], and return its id.
async fn given_pool(router: &MultiServiceRouter, pool_name: &str) -> String {
    let created = cognito_ok(router, "CreateUserPool", &json!({ "PoolName": pool_name })).await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string();
    cognito_ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "MessageAction": "SUPPRESS",
        }),
    )
    .await;
    cognito_ok(
        router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    user_pool_id
}

/// Create an app client admitting exactly `flows`, and return its minted id.
async fn given_client(router: &MultiServiceRouter, user_pool_id: &str, flows: &[&str]) -> String {
    let created = cognito_ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "series-reader-client",
            "GenerateSecret": false,
            "ExplicitAuthFlows": flows,
        }),
    )
    .await;
    created["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string()
}

/// One client-side password sign-in attempt.
async fn sign_in(router: &MultiServiceRouter, client_id: &str) -> (u16, Value) {
    cognito(
        router,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await
}

/// One row of the traffic table: how many attempts the app client's flow gate refuses before
/// it is reconfigured, and how many complete after.
struct Row {
    case: &'static str,
    refused: usize,
    completed: usize,
}

/// The traffic a row drives, with each call asserted the way the row claims it was answered —
/// so a row can only pass while the attempts it counts were really refused or really served.
async fn drive(router: &MultiServiceRouter, row: &Row, user_pool_id: &str, client_id: &str) {
    for attempt in 0..row.refused {
        let (status, payload) = sign_in(router, client_id).await;
        assert_eq!(
            (status, payload["__type"].as_str().unwrap_or_default()),
            (400, "InvalidParameterException"),
            "case {:?}: attempt {attempt} must be refused by the app client's flow gate, got \
             {status} with {payload}",
            row.case
        );
    }
    cognito_ok(
        router,
        "UpdateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "ExplicitAuthFlows": REPAIRED_FLOWS,
        }),
    )
    .await;
    for attempt in 0..row.completed {
        let (status, payload) = sign_in(router, client_id).await;
        assert!(
            status == 200 && payload["AuthenticationResult"]["AccessToken"].is_string(),
            "case {:?}: attempt {attempt} must complete once the client enables the flow, got \
             {status} with {payload}",
            row.case
        );
    }
}

/// The rows of the traffic table: what the flow gate refused before the app client was
/// reconfigured, and what completed after.
fn rows() -> [Row; 5] {
    [
        Row {
            case: "attempts refused by the flow gate, then one repaired sign-in",
            refused: 3,
            completed: 1,
        },
        Row {
            case: "one refusal and one completion",
            refused: 1,
            completed: 1,
        },
        Row {
            case: "nothing refused, two completions",
            refused: 0,
            completed: 2,
        },
        Row {
            case: "refusals and nothing after the reconfiguration",
            refused: 2,
            completed: 0,
        },
        Row {
            case: "more completions than refusals",
            refused: 1,
            completed: 3,
        },
    ]
}

/// RULE: a terminated sign-in is one stored sample in the pool's sign-in series, valued 1 when
/// it completed and 0 when it was refused — so every read over a window holding the attempts
/// reports `Sum` as the completions and `SampleCount` as the attempts, whichever read API asks
/// and whatever period it folds the window into.
#[tokio::test]
async fn both_metric_reads_report_the_same_sign_in_totals_at_every_period() {
    for row in rows() {
        let router = emulator();
        let opened = now_secs();
        let user_pool_id = given_pool(&router, "sign-in-read-paths").await;
        let client_id = given_client(&router, &user_pool_id, &BLOCKED_FLOWS).await;
        drive(&router, &row, &user_pool_id, &client_id).await;

        let dimensions = keyed(&user_pool_id, &client_id);
        let span = window(opened);
        let expected = (row.completed as f64, (row.refused + row.completed) as f64);
        for path in [ReadPath::Statistics, ReadPath::Data] {
            for period in PERIODS {
                let (sum, sample_count, points) =
                    totals(&router, path, &dimensions, period, span).await;
                assert!(
                    (sum - expected.0).abs() < f64::EPSILON
                        && (sample_count - expected.1).abs() < f64::EPSILON,
                    "case {:?}: expected {path:?} at period {period} to report (Sum, \
                     SampleCount) {expected:?}, got {:?}",
                    row.case,
                    (sum, sample_count)
                );
                // Every attempt terminated, so the window folded at least one datapoint —
                // a refused attempt is a zero-valued sample, never a missing one.
                assert!(
                    points >= 1,
                    "case {:?}: expected {path:?} at period {period} to fold the \
                     {} attempt(s) into at least one datapoint, got {points}",
                    row.case,
                    row.refused + row.completed
                );
            }
        }
    }
}

/// RULE: a sample is stamped at the instant of the attempt it observes, so a read whose window
/// does not hold that instant reports none of the traffic — through either read API, and
/// whatever period it folds into.
#[tokio::test]
async fn a_window_that_does_not_hold_the_attempts_reports_none_of_them() {
    let router = emulator();
    let opened = now_secs();
    let user_pool_id = given_pool(&router, "sign-in-read-window").await;
    let client_id = given_client(&router, &user_pool_id, &BLOCKED_FLOWS).await;
    let row = Row {
        case: "a refused attempt and a repaired sign-in",
        refused: 1,
        completed: 1,
    };
    drive(&router, &row, &user_pool_id, &client_id).await;

    let dimensions = keyed(&user_pool_id, &client_id);
    // A whole hour of windows, each a whole minute wide, that closed before the pool was even
    // created, and one that opens well after the traffic.
    let before = ((opened - 7_200) / 60 * 60, (opened - 3_600) / 60 * 60);
    let after = (
        (now_secs() + 3_600) / 60 * 60,
        (now_secs() + 7_200) / 60 * 60,
    );
    for (case, span) in [
        ("a window that closed first", before),
        ("a later window", after),
    ] {
        for path in [ReadPath::Statistics, ReadPath::Data] {
            for period in PERIODS {
                let found = totals(&router, path, &dimensions, period, span).await;
                assert_eq!(
                    found,
                    (0.0, 0.0, 0),
                    "case {case:?}: expected {path:?} at period {period} to report no \
                     datapoint outside the attempts' window, got {found:?}"
                );
            }
        }
    }

    // The window that DOES hold them still reports both attempts, so the empty reads above are
    // the window's doing and not an empty series.
    let span = window(opened);
    let found = totals(&router, ReadPath::Statistics, &dimensions, 3_600, span).await;
    assert!(
        (found.0 - 1.0).abs() < f64::EPSILON && (found.1 - 2.0).abs() < f64::EPSILON,
        "the window holding the attempts must report (Sum, SampleCount) (1.0, 2.0), got {found:?}"
    );
}
