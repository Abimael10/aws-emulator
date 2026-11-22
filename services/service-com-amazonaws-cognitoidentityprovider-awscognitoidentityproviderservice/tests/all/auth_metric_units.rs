// SPDX-License-Identifier: CC0-1.0

//! The UNIT the `AWS/Cognito` series a user pool publishes about its own traffic are read
//! under, asserted where a client reads them: through CloudWatch's `GetMetricStatistics` and
//! `GetMetricData`, with both services mounted on ONE engine exactly as the assembled emulator
//! mounts them.
//!
//! The rule under test: a user pool declares NO unit for the observations it publishes about
//! its own traffic, so every datapoint of those series is read back under the value the
//! enumerated unit type declares for a measurement that has no unit
//! (`smithy_query(query="shape",shape="StandardUnit")`). The unit is part of a datapoint's read
//! identity — "If you omit Unit, all data that was collected with any unit is returned, along
//! with the corresponding units that were specified when the data was reported to CloudWatch.
//! If you specify a unit, the operation returns only data that was collected with that unit
//! specified. If you specify a unit that does not match the data collected, the results of the
//! operation are null." (`GetMetricStatisticsInput$Unit`, in the same words on
//! `GetMetricDataInput`) — so a read naming that value selects exactly these datapoints, a
//! read naming any OTHER declared unit selects none of them, and a read naming no unit at all
//! reports them under it.
//!
//! The rows span what the rule generalises over rather than one recorded read: WHICH of the
//! namespace's series the traffic publishes into (each authentication counter and the risk
//! outcome of an evaluated event), HOW the attempts ended (completed, refused, a mixture),
//! HOW MANY attempts were made, and which DIMENSION set the series is keyed on. The units a
//! read may ask for are the whole declared set, so the rows are asserted against units no
//! report of this behaviour cites.

use bytes::Bytes;
use serde_json::{json, Map, Value};
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

/// The namespace a user pool's own metrics live in.
const NAMESPACE: &str = "AWS/Cognito";

/// The series this suite reads.
const SIGN_IN_SUCCESSES: &str = "SignInSuccesses";
const TOKEN_REFRESH_SUCCESSES: &str = "TokenRefreshSuccesses";
const SIGN_UP_SUCCESSES: &str = "SignUpSuccesses";
const NO_RISK: &str = "NoRisk";

/// The dimension names an `AWS/Cognito` series is keyed on: the counters carry the pool as
/// `UserPool` beside the app client, and a risk outcome carries it as `UserPoolId` beside the
/// operation it was evaluated for.
const USER_POOL: &str = "UserPool";
const USER_POOL_CLIENT: &str = "UserPoolClient";
const OPERATION: &str = "Operation";
const USER_POOL_ID: &str = "UserPoolId";

/// The `Operation` a sign-in is evaluated under.
const SIGN_IN_OPERATION: &str = "SignIn";

/// The values `StandardUnit` declares, in the order the model declares them
/// (`smithy_query(query="shape",shape="StandardUnit")`): every unit a datapoint may be
/// published under, and every unit a read may select by.
const STANDARD_UNITS: [&str; 27] = [
    "Seconds",
    "Microseconds",
    "Milliseconds",
    "Bytes",
    "Kilobytes",
    "Megabytes",
    "Gigabytes",
    "Terabytes",
    "Bits",
    "Kilobits",
    "Megabits",
    "Gigabits",
    "Terabits",
    "Percent",
    "Count",
    "Bytes/Second",
    "Kilobytes/Second",
    "Megabytes/Second",
    "Gigabytes/Second",
    "Terabytes/Second",
    "Bits/Second",
    "Kilobits/Second",
    "Megabits/Second",
    "Gigabits/Second",
    "Terabits/Second",
    "Count/Second",
    "None",
];

/// The declared value for a measurement that has no unit, which is what a publisher declaring
/// none has its datapoints read under.
const NO_UNIT_DECLARED: &str = "None";

/// A password satisfying a default pool's password policy, and one no user holds.
const GOOD_PASSWORD: &str = "Passw0rd!2024";
const WRONG_PASSWORD: &str = "N0tTheP4ssword!";

/// The user every sign-in row signs in.
const USERNAME: &str = "unit-metrics-user";

/// The `ExplicitAuthFlows` the fixture's app client admits, so one client serves every row
/// (`CreateUserPoolClient.ExplicitAuthFlows`).
const FLOWS: [&str; 2] = ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"];

/// The period a read folds the window into, and how far either side of the traffic the window
/// reaches, so it holds every attempt's instant whatever minute the calls land in.
const PERIOD_SECS: i64 = 3600;
const WINDOW_SLACK_SECS: i64 = 1_200;

/// One emulator with this service and CloudWatch mounted on ONE engine, which is what makes the
/// metric store the pool writes the same one CloudWatch reads.
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
        .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
}

/// The `(StartTime, EndTime)` a read spans around traffic that opened at `opened`, each at a
/// whole minute ("Start time less than 15 days ago - Round down to the nearest whole minute",
/// `GetMetricDataInput$StartTime`).
fn window(opened: i64) -> (i64, i64) {
    let start = (opened - WINDOW_SLACK_SECS) / 60 * 60;
    let end = (now_secs() + WINDOW_SLACK_SECS + 59) / 60 * 60;
    (start, end)
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

/// The pool, app client and user a row drives its traffic against.
struct Fixture {
    user_pool_id: String,
    client_id: String,
}

/// Create a pool with `tier` and the advanced-security `mode` when the row names them, one app
/// client admitting [`FLOWS`] and one `CONFIRMED` user holding [`GOOD_PASSWORD`].
async fn given_pool(
    router: &MultiServiceRouter,
    tier: Option<&str>,
    mode: Option<&str>,
) -> Fixture {
    let mut pool = Map::new();
    pool.insert("PoolName".to_string(), json!("unit-metrics-pool"));
    if let Some(tier) = tier {
        pool.insert("UserPoolTier".to_string(), json!(tier));
    }
    if let Some(mode) = mode {
        pool.insert(
            "UserPoolAddOns".to_string(),
            json!({ "AdvancedSecurityMode": mode }),
        );
    }
    let created = cognito_ok(router, "CreateUserPool", &Value::Object(pool)).await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string();
    let client = cognito_ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "unit-metrics-client",
            "GenerateSecret": false,
            "ExplicitAuthFlows": FLOWS,
        }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
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
    Fixture {
        user_pool_id,
        client_id,
    }
}

/// One call a row drives, each of which terminates an attempt and so owes its series a sample.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// `InitiateAuth` with the correct password, which completes the sign-in.
    SignIn,
    /// The same with a password no user holds, which the pool refuses.
    WrongPassword,
    /// `InitiateAuth` with the refresh-token flow, presenting the token the last completed
    /// sign-in issued.
    Refresh,
    /// `SignUp` of a username nothing registered yet.
    SignUp,
}

/// Drive `steps`, asserting each call was answered the way its own step says — so a row can
/// only pass while the traffic it claims to have driven was really served or really refused.
async fn drive(router: &MultiServiceRouter, fixture: &Fixture, case: &str, steps: &[Step]) {
    let mut refresh_token: Option<String> = None;
    let mut registered = 0_usize;
    for step in steps {
        match step {
            Step::SignIn => {
                let answered = cognito_ok(
                    router,
                    "InitiateAuth",
                    &json!({
                        "ClientId": fixture.client_id,
                        "AuthFlow": "USER_PASSWORD_AUTH",
                        "AuthParameters": { "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD },
                    }),
                )
                .await;
                let token = answered["AuthenticationResult"]["RefreshToken"].as_str();
                assert!(
                    token.is_some(),
                    "case {case:?}: a completed sign-in must issue a refresh token, got \
                     {answered}"
                );
                refresh_token = token.map(str::to_string);
            }
            Step::WrongPassword => {
                let (status, payload) = cognito(
                    router,
                    "InitiateAuth",
                    &json!({
                        "ClientId": fixture.client_id,
                        "AuthFlow": "USER_PASSWORD_AUTH",
                        "AuthParameters": { "USERNAME": USERNAME, "PASSWORD": WRONG_PASSWORD },
                    }),
                )
                .await;
                assert_eq!(
                    (status, payload["__type"].as_str().unwrap_or_default()),
                    (400, "NotAuthorizedException"),
                    "case {case:?}: a wrong password must be refused on the credential, got \
                     {status} with {payload}"
                );
            }
            Step::Refresh => {
                let token = refresh_token
                    .clone()
                    .unwrap_or_else(|| panic!("case {case:?}: no sign-in issued a token yet"));
                cognito_ok(
                    router,
                    "InitiateAuth",
                    &json!({
                        "ClientId": fixture.client_id,
                        "AuthFlow": "REFRESH_TOKEN_AUTH",
                        "AuthParameters": { "REFRESH_TOKEN": token },
                    }),
                )
                .await;
            }
            Step::SignUp => {
                registered += 1;
                cognito_ok(
                    router,
                    "SignUp",
                    &json!({
                        "ClientId": fixture.client_id,
                        "Username": format!("registered-{registered}"),
                        "Password": GOOD_PASSWORD,
                    }),
                )
                .await;
            }
        }
    }
}

/// Which dimension set a series is keyed on.
#[derive(Clone, Copy, Debug)]
enum Keyed {
    /// The authentication counters: the pool as `UserPool`, beside the app client.
    Counters,
    /// A risk outcome: the operation evaluated, beside the pool as `UserPoolId`.
    Risk,
}

impl Keyed {
    /// The dimension set as a read states it.
    fn dimensions(self, fixture: &Fixture) -> Value {
        match self {
            Self::Counters => json!([
                { "Name": USER_POOL, "Value": fixture.user_pool_id },
                { "Name": USER_POOL_CLIENT, "Value": fixture.client_id },
            ]),
            Self::Risk => json!([
                { "Name": OPERATION, "Value": SIGN_IN_OPERATION },
                { "Name": USER_POOL_ID, "Value": fixture.user_pool_id },
            ]),
        }
    }
}

/// A `GetMetricStatistics` request for one series over `span`, carrying `unit` when the read
/// names one.
fn statistics_request(
    metric: &str,
    dimensions: &Value,
    unit: Option<&str>,
    (start, end): (i64, i64),
) -> Value {
    let mut request = json!({
        "Namespace": NAMESPACE,
        "MetricName": metric,
        "Dimensions": dimensions,
        "StartTime": start,
        "EndTime": end,
        "Period": PERIOD_SECS,
        "Statistics": ["Sum", "SampleCount"],
    });
    if let (Some(unit), Value::Object(members)) = (unit, &mut request) {
        members.insert("Unit".to_string(), json!(unit));
    }
    request
}

/// The `(Unit, Sum, SampleCount)` of every datapoint one `GetMetricStatistics` answer carries.
async fn statistics(
    router: &MultiServiceRouter,
    metric: &str,
    dimensions: &Value,
    unit: Option<&str>,
    span: (i64, i64),
) -> Vec<(String, f64, f64)> {
    let answered = cloudwatch_ok(
        router,
        "GetMetricStatistics",
        &statistics_request(metric, dimensions, unit, span),
    )
    .await;
    let mut points: Vec<(String, f64, f64)> = answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| {
            (
                point
                    .get("Unit")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                point.get("Sum").and_then(Value::as_f64).unwrap_or_default(),
                point
                    .get("SampleCount")
                    .and_then(Value::as_f64)
                    .unwrap_or_default(),
            )
        })
        .collect();
    points.sort_by(|left, right| {
        (&left.0, left.1, left.2)
            .partial_cmp(&(&right.0, right.1, right.2))
            .expect("the readings are comparable")
    });
    points
}

/// The `(Sum, SampleCount)` totals a `GetMetricStatistics` answer folds to, and how many
/// datapoints carried them.
fn totals(points: &[(String, f64, f64)]) -> (f64, f64, usize) {
    (
        points.iter().map(|(_, sum, _)| sum).sum(),
        points.iter().map(|(_, _, samples)| samples).sum(),
        points.len(),
    )
}

/// The `Sum` values `GetMetricData` answers for one series, its `MetricStat` carrying `unit`
/// when the read names one.
async fn retrieved(
    router: &MultiServiceRouter,
    metric: &str,
    dimensions: &Value,
    unit: Option<&str>,
    (start, end): (i64, i64),
) -> Vec<f64> {
    let mut stat = json!({
        "Metric": {
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions,
        },
        "Period": PERIOD_SECS,
        "Stat": "Sum",
    });
    if let (Some(unit), Value::Object(members)) = (unit, &mut stat) {
        members.insert("Unit".to_string(), json!(unit));
    }
    let answered = cloudwatch_ok(
        router,
        "GetMetricData",
        &json!({
            "StartTime": start,
            "EndTime": end,
            "MetricDataQueries": [{ "Id": "sum", "MetricStat": stat, "ReturnData": true }],
        }),
    )
    .await;
    answered["MetricDataResults"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
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
}

/// One row of the table: the traffic driven, the series it publishes into, and what that series
/// then holds.
struct Row {
    case: &'static str,
    /// The pool's tier and advanced-security mode, when the row needs the pool to EVALUATE the
    /// events it serves.
    tier: Option<&'static str>,
    mode: Option<&'static str>,
    steps: &'static [Step],
    metric: &'static str,
    keyed: Keyed,
    /// The `(Sum, SampleCount)` the window holds once the traffic is driven.
    expected: (f64, f64),
}

/// The rows: each kind of attempt the namespace counts, each outcome a call can have, several
/// attempt counts, and the risk outcome keyed on its own dimension set.
fn rows() -> [Row; 6] {
    [
        Row {
            case: "a completed sign-in",
            tier: None,
            mode: None,
            steps: &[Step::SignIn],
            metric: SIGN_IN_SUCCESSES,
            keyed: Keyed::Counters,
            expected: (1.0, 1.0),
        },
        Row {
            case: "sign-ins refused on the credential alone",
            tier: None,
            mode: None,
            steps: &[Step::WrongPassword, Step::WrongPassword, Step::WrongPassword],
            metric: SIGN_IN_SUCCESSES,
            keyed: Keyed::Counters,
            expected: (0.0, 3.0),
        },
        Row {
            case: "four completed and three refused sign-ins",
            tier: None,
            mode: None,
            steps: &[
                Step::SignIn,
                Step::WrongPassword,
                Step::SignIn,
                Step::WrongPassword,
                Step::SignIn,
                Step::WrongPassword,
                Step::SignIn,
            ],
            metric: SIGN_IN_SUCCESSES,
            keyed: Keyed::Counters,
            expected: (4.0, 7.0),
        },
        Row {
            case: "two refresh-token exchanges",
            tier: None,
            mode: None,
            steps: &[Step::SignIn, Step::Refresh, Step::Refresh],
            metric: TOKEN_REFRESH_SUCCESSES,
            keyed: Keyed::Counters,
            expected: (2.0, 2.0),
        },
        Row {
            case: "a registration",
            tier: None,
            mode: None,
            steps: &[Step::SignUp],
            metric: SIGN_UP_SUCCESSES,
            keyed: Keyed::Counters,
            expected: (1.0, 1.0),
        },
        Row {
            case: "the risk outcome of an evaluated sign-in",
            tier: Some("PLUS"),
            mode: Some("AUDIT"),
            steps: &[Step::SignIn],
            metric: NO_RISK,
            keyed: Keyed::Risk,
            expected: (1.0, 1.0),
        },
    ]
}

/// RULE: a user pool declares no unit for the series it publishes about its own traffic, so
/// every datapoint of them is read under the declared value for a measurement that has no unit
/// — reported under it by a read that names no unit, selected by a read that names it, and
/// selected by no read that names another unit, on both metric read paths.
#[tokio::test]
async fn every_user_pool_observation_is_read_under_the_no_unit_value() {
    for row in rows() {
        let router = emulator();
        let opened = now_secs();
        let fixture = given_pool(&router, row.tier, row.mode).await;
        drive(&router, &fixture, row.case, row.steps).await;

        let dimensions = row.keyed.dimensions(&fixture);
        let span = window(opened);

        // A read naming NO unit reports the traffic, and every datapoint it reports carries
        // the value for "no unit".
        let unfiltered = statistics(&router, row.metric, &dimensions, None, span).await;
        let found = totals(&unfiltered);
        assert!(
            (found.0 - row.expected.0).abs() < f64::EPSILON
                && (found.1 - row.expected.1).abs() < f64::EPSILON
                && found.2 >= 1,
            "case {:?}: expected {} to hold (Sum, SampleCount) {:?} over at least one \
             datapoint, got {found:?} from {unfiltered:?}",
            row.case,
            row.metric,
            row.expected
        );
        assert!(
            unfiltered.iter().all(|(unit, _, _)| unit == NO_UNIT_DECLARED),
            "case {:?}: every {} datapoint must carry the unit {NO_UNIT_DECLARED:?}, got \
             {unfiltered:?}",
            row.case,
            row.metric
        );

        // Naming that value selects exactly those datapoints; naming any other declared unit
        // selects none of them.
        for unit in STANDARD_UNITS {
            let selected = statistics(&router, row.metric, &dimensions, Some(unit), span).await;
            let expected = if unit == NO_UNIT_DECLARED {
                unfiltered.clone()
            } else {
                Vec::new()
            };
            assert_eq!(
                selected, expected,
                "case {:?}: a read of {} naming the unit {unit:?} must answer with {expected:?}, \
                 got {selected:?}",
                row.case, row.metric
            );
        }

        // The retrieving read selects on the unit the same way: the value for "no unit" answers
        // with the series' values, and another declared unit answers with none.
        let retrieved_unitless = retrieved(&router, row.metric, &dimensions, None, span).await;
        let retrieved_none =
            retrieved(&router, row.metric, &dimensions, Some(NO_UNIT_DECLARED), span).await;
        assert_eq!(
            retrieved_none, retrieved_unitless,
            "case {:?}: retrieving {} under {NO_UNIT_DECLARED:?} must answer with the values a \
             read naming no unit answers with {retrieved_unitless:?}, got {retrieved_none:?}",
            row.case, row.metric
        );
        assert!(
            (retrieved_none.iter().sum::<f64>() - row.expected.0).abs() < f64::EPSILON,
            "case {:?}: retrieving {} under {NO_UNIT_DECLARED:?} must sum to {}, got \
             {retrieved_none:?}",
            row.case,
            row.metric,
            row.expected.0
        );
        for unit in STANDARD_UNITS
            .iter()
            .filter(|unit| **unit != NO_UNIT_DECLARED)
        {
            let selected = retrieved(&router, row.metric, &dimensions, Some(unit), span).await;
            assert!(
                selected.is_empty(),
                "case {:?}: retrieving {} under the unit {unit:?} must answer with no value, \
                 got {selected:?}",
                row.case,
                row.metric
            );
        }
    }
}
