// SPDX-License-Identifier: CC0-1.0

//! The `AWS/IoT` series the RULES ENGINE publishes about the actions it attempted, end to end
//! over the wire through the real `MultiServiceRouter` — the IoT control plane, the IoT data
//! plane, AWS Lambda, IAM and CloudWatch on ONE shared engine, the shape
//! `core/router/src/lib.rs` mounts them in.
//!
//! The RULE asserted here: every action of a topic rule that this engine ATTEMPTS for a
//! selected message is counted once on that rule's own `AWS/IoT` series, dimensioned by the
//! rule's name — on the success series when the action was carried out and on the failure
//! series when it was not — and an action nothing attempted is counted on neither. Nothing
//! about an action's outcome is reported to the publisher (every publish below is answered
//! 200), so these series are the only place a client can read what its rules did, which is why
//! every assertion is made through CloudWatch's own `ListMetrics` and `GetMetricStatistics`.
//!
//! The rows span what the rule generalises over rather than one recorded case:
//!
//! * the first table varies WHY a `lambda` action succeeds or fails — a function whose
//!   resource policy admits this rule, one that admits this service for any source, one
//!   carrying no policy at all, one admitting another service principal, one admitting a
//!   DIFFERENT rule, and an action naming a function that was never created — and how many
//!   messages are published, so what is asserted is a count and not merely a series that
//!   exists. Each row's outcome is cross-checked where it is independently observable: the
//!   function's own `AWS/Lambda` invocation count;
//! * the second varies the ACTION TYPE and what its role says, including an `errorAction`
//!   performed because the rule's own action failed, so one message leaves one failure and one
//!   success on the same rule;
//! * the third is the differential a client reads a rule's health by: of three sibling rules on
//!   one topic, the permitted one reports successes and no failures, the unpermitted one
//!   failures and no successes;
//! * the fourth pins what carries NO series at all — a disabled rule and a rule whose topic
//!   filter does not cover the published topic attempt nothing, so neither has a series.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::rules_engine::action_metrics::{
    COUNT_UNIT, FAILURE_METRIC, NAMESPACE, RULE_NAME_DIMENSION, SUCCESS_METRIC,
};
use service_com_amazonaws_iot_awsiotservice::state::rules_engine::IOT_PUBLISH_ACTION;
use service_com_amazonaws_iot_awsiotservice::state::rules_engine::MAX_ACTIONS_PER_MESSAGE;
use service_com_amazonaws_iot_awsiotservice::state::topic_rule_destinations::{
    DESTINATION_DISABLED, DESTINATION_ENABLED,
};
use service_com_amazonaws_iot_awsiotservice::state::{service_state, SERVICE_PRINCIPAL};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use service_com_amazonaws_lambda_awsgirapiservice::state::metrics::{
    FUNCTION_NAME_DIMENSION, INVOCATIONS_METRIC, METRIC_NAMESPACE as LAMBDA_NAMESPACE,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::describe_endpoint::percent_encode;
use crate::topic_rule_kinesis_action::{form_encode, xml_text};

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// Where Lambda's `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and entry point of every deployment package this suite uploads.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The Lambda runtime every function here declares.
const RUNTIME: &str = "python3.12";

/// A well-formed Lambda execution role ARN, which is not what any case here is about.
const EXECUTION_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/rule-target";

/// The IAM action an invocation of a function performs: `Invoke` "requires permission for the
/// `lambda:InvokeFunction` action".
const INVOKE_FUNCTION_ACTION: &str = "lambda:InvokeFunction";

/// A service principal that is NOT this service, for a grant made to somebody else.
const OTHER_PRINCIPAL: &str = "events.amazonaws.com";

/// The target function of every row: it reads the message it was handed, so a row can only
/// succeed if the rules engine really handed one over.
const TARGET_SOURCE: &str = r#"
def lambda_handler(event, context):
    return {"saw": event}
"#;

/// How long a function is given to reach `Active`, the state an invocation needs. Generous:
/// the suite waits for that CONDITION, read through the function's own configuration, and
/// never for a span of its own choosing.
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);

/// How often the suite looks again while waiting for a condition to hold.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The half-window every statistics read covers, in seconds, and the period it reads at: wide
/// enough that the datapoints published while the case ran are all inside it.
const WINDOW_SECS: f64 = 900.0;
const PERIOD_SECS: i64 = 60;

/// The SQL version a client writes on a rule.
const SQL_VERSION: &str = "2016-03-23";

/// The emulator: the IoT control plane, the IoT data plane, AWS Lambda, IAM and CloudWatch on
/// ONE engine.
///
/// The engine is kept because one case reads the confirmation token the service ISSUED to a
/// destination's endpoint, which no response hands back.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_iotdataplane_iotmoonrakerservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    Emulator { router, engine }
}

/// One HTTP answer: the status and the body, read as JSON where there is one.
struct Answer {
    status: u16,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

impl Emulator {
    /// One request to the service whose signing name is `signing_name`: the credential scope is
    /// how the router picks the claiming service, and the signature itself is never checked —
    /// the harness turns request authentication off.
    async fn send(
        &self,
        method: &str,
        uri: &str,
        signing_name: &str,
        content_type: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Answer {
        let mut request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDTESTIOT/20260101/{REGION}/{signing_name}/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .header("content-type", content_type);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = self
            .router
            .dispatch(
                request
                    .body(bytes::Bytes::from(body))
                    .expect("a well-formed request"),
            )
            .await;
        Answer {
            status: response.status().as_u16(),
            body: response.into_body().to_vec(),
        }
    }

    /// `CreateTopicRule`: the restJson1 `POST /rules/{ruleName}` of the IoT control plane,
    /// whose body IS the `topicRulePayload`.
    async fn create_topic_rule(&self, case: &str, rule_name: &str, payload: &Value) {
        let answer = self
            .send(
                "POST",
                &format!("/rules/{rule_name}"),
                "iot",
                "application/json",
                &[],
                serde_json::to_vec(payload).expect("the payload serializes"),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: CreateTopicRule({rule_name}) answered {} {}",
            answer.status,
            answer.text()
        );
    }

    /// `GetTopicRule` (`GET /rules/{ruleName}`), which is where the rule's own ARN comes from:
    /// nothing here builds one.
    async fn rule_arn(&self, case: &str, rule_name: &str) -> String {
        let answer = self
            .send(
                "GET",
                &format!("/rules/{rule_name}"),
                "iot",
                "application/json",
                &[],
                Vec::new(),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: GetTopicRule({rule_name}) answered {} {}",
            answer.status,
            answer.text()
        );
        answer.json()["ruleArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: GetTopicRule({rule_name}) reported no ruleArn, got {}",
                    answer.text()
                )
            })
            .to_string()
    }

    /// `Publish` of the IoT data plane (`POST /topics/{topic}` at QoS 1), the call the rules
    /// engine runs on. An accepted publish is a 200 whatever the rules did with it.
    async fn publish(&self, case: &str, topic: &str, payload: &[u8]) {
        let answer = self
            .send(
                "POST",
                &format!("/topics/{}?qos=1", percent_encode(topic)),
                "iotdata",
                "application/octet-stream",
                &[],
                payload.to_vec(),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: Publish({topic}) answered {} {}",
            answer.status,
            answer.text()
        );
    }

    /// One AWS Lambda call over its own restJson1 routes.
    async fn lambda(&self, method: &str, uri: &str, body: Option<&Value>) -> Answer {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        self.send(method, uri, "lambda", "application/json", &[], payload)
            .await
    }

    /// One CloudWatch call, answered with its body.
    async fn cloudwatch(&self, case: &str, operation: &str, body: &Value) -> Value {
        let answer = self
            .send(
                "POST",
                "/",
                "monitoring",
                "application/x-amz-json-1.0",
                &[(
                    "x-amz-target",
                    &format!("GraniteServiceVersion20100801.{operation}"),
                )],
                serde_json::to_vec(body).expect("the request serializes"),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: {operation} answered {} {}",
            answer.status,
            answer.text()
        );
        answer.json()
    }

    /// `CreateTopicRuleDestination` for an HTTP destination whose confirmation URL is
    /// `confirmation_url`, answered with the ARN it minted. Creating it sends the confirmation
    /// challenge to that URL, exactly as AWS IoT does.
    async fn create_destination(&self, case: &str, confirmation_url: &str) -> String {
        let answer = self
            .send(
                "POST",
                "/destinations",
                "iot",
                "application/json",
                &[],
                serde_json::to_vec(&json!({
                    "destinationConfiguration": {
                        "httpUrlConfiguration": { "confirmationUrl": confirmation_url },
                    },
                }))
                .expect("the payload serializes"),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: CreateTopicRuleDestination({confirmation_url}) answered {} {}",
            answer.status,
            answer.text()
        );
        answer.json()["topicRuleDestination"]["arn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: CreateTopicRuleDestination minted no ARN, got {}",
                    answer.text()
                )
            })
            .to_string()
    }

    /// The token the destination's OUTSTANDING challenge carries, read off the store the way
    /// the owner of a confirmation endpoint reads it off the challenge message it was sent.
    fn outstanding_token(&self, case: &str, arn: &str) -> String {
        let state = service_state(&*self.engine)
            .unwrap_or_else(|| panic!("case {case:?}: the IoT service store"));
        let store = state.lock();
        store
            .topic_rule_destinations
            .get(arn)
            .unwrap_or_else(|| panic!("case {case:?}: the store holds the destination {arn:?}"))
            .confirmation_token
            .clone()
    }

    /// `ConfirmTopicRuleDestination` (`GET /confirmdestination/{confirmationToken+}`), which is
    /// what carries a destination to `ENABLED` — the status its traffic gate reads.
    async fn confirm_destination(&self, case: &str, token: &str) {
        let answer = self
            .send(
                "GET",
                &format!("/confirmdestination/{token}"),
                "iot",
                "application/json",
                &[],
                Vec::new(),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: ConfirmTopicRuleDestination answered {} {}",
            answer.status,
            answer.text()
        );
    }

    /// `UpdateTopicRuleDestination` (`PATCH /destinations`): the call that moves a confirmed
    /// destination to `status`.
    async fn update_destination(&self, case: &str, arn: &str, status: &str) {
        let answer = self
            .send(
                "PATCH",
                "/destinations",
                "iot",
                "application/json",
                &[],
                serde_json::to_vec(&json!({ "arn": arn, "status": status }))
                    .expect("the payload serializes"),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: UpdateTopicRuleDestination({status}) answered {} {}",
            answer.status,
            answer.text()
        );
    }

    /// Wait for the destination `arn` to REST at `status`, read through its own
    /// `GetTopicRuleDestination` — the wait is for that CONDITION, since the move out of
    /// `IN_PROGRESS` is one the clock owes.
    async fn destination_rests_at(&self, case: &str, arn: &str, status: &str) {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let answer = self
                .send(
                    "GET",
                    &format!("/destinations/{arn}"),
                    "iot",
                    "application/json",
                    &[],
                    Vec::new(),
                )
                .await;
            let reported = answer.json()["topicRuleDestination"]["status"]
                .as_str()
                .map(str::to_string);
            if reported.as_deref() == Some(status) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the destination {arn:?} never reached {status}: {} {}",
                answer.status,
                answer.text()
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// One IAM call over the `awsQuery` protocol, answered with its XML body.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, &str)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let answer = self
            .send(
                "POST",
                "/",
                "iam",
                "application/x-www-form-urlencoded",
                &[],
                body.into_bytes(),
            )
            .await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: {action} answered {} {}",
            answer.status,
            answer.text()
        );
        answer.text()
    }
}

/// A .zip deployment package holding `source` as `lambda_function.py`, written in memory the
/// way a caller uploads one.
fn package(source: &str) -> Vec<u8> {
    use std::io::Write as _;
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// `CreateFunction` for `name`, answered with the ARN Lambda minted.
async fn create_function(emulator: &Emulator, case: &str, name: &str) -> String {
    let created = emulator
        .lambda(
            "POST",
            FUNCTIONS_URI,
            Some(&json!({
                "FunctionName": name,
                "Role": EXECUTION_ROLE,
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Timeout": 10,
                "Code": {
                    "ZipFile":
                        base64::engine::general_purpose::STANDARD.encode(package(TARGET_SOURCE)),
                },
            })),
        )
        .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction({name}) answered {} {}",
        created.status,
        created.text()
    );
    created.json()["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: CreateFunction({name}) minted no ARN, got {}",
                created.text()
            )
        })
        .to_string()
}

/// Wait for `name` to reach `Active`, the state an invocation needs — the wait is for that
/// CONDITION, read through the function's own configuration.
async fn wait_active(emulator: &Emulator, case: &str, name: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = emulator
            .lambda(
                "GET",
                &format!("{FUNCTIONS_URI}/{name}/configuration"),
                None,
            )
            .await;
        if read.status == 200 && read.json()["State"] == json!("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function {name:?} never became Active: {} {}",
            read.status,
            read.text()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `AddPermission`: one statement in the function's resource-based policy, which is what admits
/// an invocation made as a service principal.
async fn add_permission(
    emulator: &Emulator,
    case: &str,
    function: &str,
    principal: &str,
    source_arn: Option<&str>,
) {
    let mut body = json!({
        "StatementId": "rules-engine",
        "Action": INVOKE_FUNCTION_ACTION,
        "Principal": principal,
    });
    if let Some(source_arn) = source_arn {
        body["SourceArn"] = json!(source_arn);
    }
    let added = emulator
        .lambda(
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/policy"),
            Some(&body),
        )
        .await;
    assert_eq!(
        added.status,
        201,
        "case {case:?}: AddPermission({function}) answered {} {}",
        added.status,
        added.text()
    );
}

/// Epoch seconds, the instant the statistics windows below are centred on.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// The `Sum` `GetMetricStatistics` reports for one series over the window around now: the
/// magnitude a client asserts a rule's health on.
async fn metric_sum(
    emulator: &Emulator,
    case: &str,
    namespace: &str,
    metric: &str,
    dimensions: &Value,
) -> f64 {
    let now = now_secs();
    let stats = emulator
        .cloudwatch(
            case,
            "GetMetricStatistics",
            &json!({
                "Namespace": namespace,
                "MetricName": metric,
                "Dimensions": dimensions,
                "StartTime": now - WINDOW_SECS,
                "EndTime": now + WINDOW_SECS,
                "Period": PERIOD_SECS,
                "Statistics": ["Sum"],
                "Unit": COUNT_UNIT,
            }),
        )
        .await;
    stats["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .sum()
}

/// The dimension set a rule-action series is identified by.
fn rule_dimensions(rule_name: &str) -> Value {
    json!([{ "Name": RULE_NAME_DIMENSION, "Value": rule_name }])
}

/// What the rule `rule_name` reports on its two rule-action series: `(successes, failures)`.
async fn outcomes(emulator: &Emulator, case: &str, rule_name: &str) -> (f64, f64) {
    let dimensions = rule_dimensions(rule_name);
    let successes = metric_sum(emulator, case, NAMESPACE, SUCCESS_METRIC, &dimensions).await;
    let failures = metric_sum(emulator, case, NAMESPACE, FAILURE_METRIC, &dimensions).await;
    (successes, failures)
}

/// The invocations AWS LAMBDA itself counted of `function`, which is where an action's outcome
/// is independently observable: the function's own `AWS/Lambda` `Invocations` series.
async fn lambda_invocations(emulator: &Emulator, case: &str, function: &str) -> f64 {
    metric_sum(
        emulator,
        case,
        LAMBDA_NAMESPACE,
        INVOCATIONS_METRIC,
        &json!([{ "Name": FUNCTION_NAME_DIMENSION, "Value": function }]),
    )
    .await
}

/// Every series `ListMetrics` discovers in the `AWS/IoT` namespace, as
/// `(MetricName, dimension pairs)` — how a client enumerating what it can alarm on reads them.
async fn listed_series(emulator: &Emulator, case: &str) -> Vec<(String, Vec<(String, String)>)> {
    let listed = emulator
        .cloudwatch(case, "ListMetrics", &json!({ "Namespace": NAMESPACE }))
        .await;
    let mut series: Vec<(String, Vec<(String, String)>)> = listed["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|metric| {
            let mut dimensions: Vec<(String, String)> = metric["Dimensions"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|dimension| {
                    (
                        dimension["Name"].as_str().unwrap_or_default().to_string(),
                        dimension["Value"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            dimensions.sort();
            (
                metric["MetricName"].as_str().unwrap_or_default().to_string(),
                dimensions,
            )
        })
        .collect();
    series.sort();
    series
}

/// What the resource policy of a row's target function carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// No resource policy at all.
    None,
    /// This service, narrowed to the rule that makes the invocation.
    ThisRule,
    /// This service, narrowed to no source at all.
    AnySource,
    /// This service, narrowed to a DIFFERENT rule of this account.
    AnotherRule,
    /// Another service principal entirely.
    OtherPrincipal,
}

/// One row of the `lambda` action table.
struct LambdaRow {
    case: &'static str,
    /// The rule the row creates, which is also the dimension its series is read by.
    rule: &'static str,
    /// The topic the row publishes to, which that rule's `FROM` clause names.
    topic: &'static str,
    /// Whether the function the action names is created at all.
    created: bool,
    /// What that function's resource policy carries.
    grant: Grant,
    /// How many messages the row publishes.
    messages: u32,
    /// Whether each message's action is carried out.
    performed: bool,
}

impl LambdaRow {
    /// The function name the row's action targets: one per row, so no row's grant can admit
    /// another row's invocation.
    fn function(&self) -> String {
        format!("target_{}", self.rule)
    }

    /// The successes and failures the row's rule must report afterwards: one per attempted
    /// action, on the series its outcome names.
    fn expected(&self) -> (f64, f64) {
        let messages = f64::from(self.messages);
        if self.performed {
            (messages, 0.0)
        } else {
            (0.0, messages)
        }
    }
}

fn lambda_rows() -> Vec<LambdaRow> {
    vec![
        LambdaRow {
            case: "a function whose policy admits this rule",
            rule: "admitted_rule",
            topic: "metrics/admitted",
            created: true,
            grant: Grant::ThisRule,
            messages: 2,
            performed: true,
        },
        LambdaRow {
            case: "a function whose policy admits this service for any source",
            rule: "unnarrowed_rule",
            topic: "metrics/unnarrowed",
            created: true,
            grant: Grant::AnySource,
            messages: 1,
            performed: true,
        },
        LambdaRow {
            case: "a function carrying no resource policy",
            rule: "unpermissioned_rule",
            topic: "metrics/unpermissioned",
            created: true,
            grant: Grant::None,
            messages: 3,
            performed: false,
        },
        LambdaRow {
            case: "a function whose policy admits another service principal",
            rule: "other_principal_rule",
            topic: "metrics/other-principal",
            created: true,
            grant: Grant::OtherPrincipal,
            messages: 1,
            performed: false,
        },
        LambdaRow {
            case: "a function whose policy admits a different rule",
            rule: "other_source_rule",
            topic: "metrics/other-source",
            created: true,
            grant: Grant::AnotherRule,
            messages: 2,
            performed: false,
        },
        LambdaRow {
            case: "an action naming a function that was never created",
            rule: "ghost_rule",
            topic: "metrics/ghost",
            created: false,
            grant: Grant::None,
            messages: 1,
            performed: false,
        },
    ]
}

/// RULE: every `lambda` action the rules engine attempts is counted once on its rule's own
/// `AWS/IoT` series — the success series for an invocation that was made, the failure series
/// for one that was refused — whatever refused it, and the counts are per rule.
#[tokio::test]
async fn every_attempted_lambda_action_is_counted_on_its_rules_own_series() {
    let emulator = emulator();
    let rows = lambda_rows();

    // Every function first, so the creations settle alongside each other rather than one
    // after the other; the ARN a row's action names is the one Lambda minted for it, and a
    // row that creates no function names an ARN of this account that nothing holds.
    let mut targets: Vec<String> = Vec::new();
    for row in &rows {
        if row.created {
            targets.push(create_function(&emulator, row.case, &row.function()).await);
        } else {
            targets.push(format!(
                "arn:aws:lambda:{REGION}:{}:function:{}",
                engine::emulator_account_id(),
                row.function()
            ));
        }
    }
    for row in &rows {
        if row.created {
            wait_active(&emulator, row.case, &row.function()).await;
        }
    }

    for (row, target) in rows.iter().zip(&targets) {
        emulator
            .create_topic_rule(
                row.case,
                row.rule,
                &json!({
                    "sql": format!("SELECT * FROM '{}'", row.topic),
                    "awsIotSqlVersion": SQL_VERSION,
                    "ruleDisabled": false,
                    "actions": [{ "lambda": { "functionArn": target } }],
                }),
            )
            .await;
        if !row.created {
            continue;
        }
        let rule_arn = emulator.rule_arn(row.case, row.rule).await;
        match row.grant {
            Grant::None => {}
            Grant::ThisRule => {
                add_permission(
                    &emulator,
                    row.case,
                    &row.function(),
                    SERVICE_PRINCIPAL,
                    Some(&rule_arn),
                )
                .await;
            }
            Grant::AnySource => {
                add_permission(&emulator, row.case, &row.function(), SERVICE_PRINCIPAL, None).await;
            }
            Grant::AnotherRule => {
                // Another rule's ARN, derived from this rule's own so no literal from any one
                // run appears here.
                let other = rule_arn.replace(row.rule, "a_rule_of_another_pipeline");
                add_permission(
                    &emulator,
                    row.case,
                    &row.function(),
                    SERVICE_PRINCIPAL,
                    Some(&other),
                )
                .await;
            }
            Grant::OtherPrincipal => {
                add_permission(
                    &emulator,
                    row.case,
                    &row.function(),
                    OTHER_PRINCIPAL,
                    Some(&rule_arn),
                )
                .await;
            }
        }
    }

    for row in &rows {
        for index in 0..row.messages {
            emulator
                .publish(
                    row.case,
                    row.topic,
                    json!({ "marker": index }).to_string().as_bytes(),
                )
                .await;
        }
    }

    for row in &rows {
        let (expected_successes, expected_failures) = row.expected();
        let (successes, failures) = outcomes(&emulator, row.case, row.rule).await;
        assert_eq!(
            successes, expected_successes,
            "case {:?}: expected {expected_successes} on the {SUCCESS_METRIC} series of \
             {:?}, got {successes}",
            row.case, row.rule
        );
        assert_eq!(
            failures, expected_failures,
            "case {:?}: expected {expected_failures} on the {FAILURE_METRIC} series of \
             {:?}, got {failures}",
            row.case, row.rule
        );
        // The same count, from the other side: AWS Lambda counts exactly the invocations
        // that were made, so a counted success is an invocation that happened and a counted
        // failure is one that did not.
        if row.created {
            let invoked = lambda_invocations(&emulator, row.case, &row.function()).await;
            assert_eq!(
                invoked, expected_successes,
                "case {:?}: expected {expected_successes} invocation(s) of {:?} on its own \
                 {LAMBDA_NAMESPACE} series, got {invoked}",
                row.case,
                row.function()
            );
        }
    }

    // What a client enumerating the namespace finds: one series per outcome each rule had,
    // dimensioned by that rule's name and nothing else.
    let listed = listed_series(&emulator, "the listing of the namespace").await;
    let mut expected: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for row in &rows {
        let metric = if row.performed {
            SUCCESS_METRIC
        } else {
            FAILURE_METRIC
        };
        expected.push((
            metric.to_string(),
            vec![(RULE_NAME_DIMENSION.to_string(), row.rule.to_string())],
        ));
    }
    expected.sort();
    assert_eq!(
        listed, expected,
        "the listing must discover exactly the rule-action series the rules engine published, \
         expected {expected:?}, got {listed:?}"
    );
}

/// One row of the role table: a `republish` action, what its role says about the publish, and
/// what the rule therefore reports.
struct RepublishRow {
    case: &'static str,
    rule: &'static str,
    topic: &'static str,
    /// The service principal the action's role trusts.
    principal: &'static str,
    /// The `(action, resource)` the role's inline policy allows, or `None` for a role carrying
    /// no inline policy at all.
    allowed: Option<(&'static str, &'static str)>,
    /// The successes and failures the rule must report.
    expected: (f64, f64),
}

/// A trust policy admitting one service principal.
fn trust_policy(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// An identity policy allowing one action on one resource.
fn identity_policy(action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
    .to_string()
}

/// A role trusting `principal` and carrying an inline policy allowing `allowed` when the row
/// names one; answered with the ARN IAM persisted.
async fn given_role(
    emulator: &Emulator,
    case: &str,
    name: &str,
    principal: &str,
    allowed: Option<(&str, &str)>,
) -> String {
    let trust = trust_policy(principal);
    let created = emulator
        .iam(
            case,
            "CreateRole",
            &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
        )
        .await;
    let arn = xml_text(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole reported no Arn: {created}"));
    if let Some((action, resource)) = allowed {
        let document = identity_policy(action, resource);
        emulator
            .iam(
                case,
                "PutRolePolicy",
                &[
                    ("RoleName", name),
                    ("PolicyName", "rule-action"),
                    ("PolicyDocument", &document),
                ],
            )
            .await;
    }
    arn
}

/// RULE, over another action type and over an `errorAction`: the outcome counted is the one the
/// attempt had, so a `republish` its role admits counts a success and one its role bars counts a
/// failure — and a rule whose action failed counts its `errorAction`'s own attempt too.
#[tokio::test]
async fn the_outcome_counted_is_the_one_each_attempted_action_had() {
    let rows = vec![
        RepublishRow {
            case: "a republish action whose role grants the publish",
            rule: "republish_granted",
            topic: "staging/granted",
            principal: SERVICE_PRINCIPAL,
            allowed: Some((IOT_PUBLISH_ACTION, "*")),
            expected: (1.0, 0.0),
        },
        RepublishRow {
            case: "a republish action whose role grants another action",
            rule: "republish_other_action",
            topic: "staging/other-action",
            principal: SERVICE_PRINCIPAL,
            allowed: Some((INVOKE_FUNCTION_ACTION, "*")),
            expected: (0.0, 1.0),
        },
        RepublishRow {
            case: "a republish action whose role carries no inline policy",
            rule: "republish_no_policy",
            topic: "staging/no-policy",
            principal: SERVICE_PRINCIPAL,
            allowed: None,
            expected: (0.0, 1.0),
        },
        RepublishRow {
            case: "a republish action whose role trusts another service",
            rule: "republish_other_principal",
            topic: "staging/other-principal",
            principal: OTHER_PRINCIPAL,
            allowed: Some((IOT_PUBLISH_ACTION, "*")),
            expected: (0.0, 1.0),
        },
    ];

    for row in &rows {
        let emulator = emulator();
        let role_arn = given_role(
            &emulator,
            row.case,
            &format!("role_{}", row.rule),
            row.principal,
            row.allowed,
        )
        .await;
        emulator
            .create_topic_rule(
                row.case,
                row.rule,
                &json!({
                    "sql": format!("SELECT * FROM '{}'", row.topic),
                    "awsIotSqlVersion": SQL_VERSION,
                    "ruleDisabled": false,
                    // A topic no rule of this store covers, so the republished message runs
                    // no further rule and leaves no further count.
                    "actions": [{
                        "republish": { "roleArn": role_arn, "topic": "restaged/out", "qos": 1 },
                    }],
                }),
            )
            .await;
        emulator
            .publish(row.case, row.topic, br#"{"marker":"one"}"#)
            .await;

        let (expected_successes, expected_failures) = row.expected;
        let (successes, failures) = outcomes(&emulator, row.case, row.rule).await;
        assert_eq!(
            successes, expected_successes,
            "case {:?}: expected {expected_successes} on the {SUCCESS_METRIC} series of {:?}, \
             got {successes}",
            row.case, row.rule
        );
        assert_eq!(
            failures, expected_failures,
            "case {:?}: expected {expected_failures} on the {FAILURE_METRIC} series of {:?}, \
             got {failures}",
            row.case, row.rule
        );
    }
}

/// RULE, applied to the action a rule performs BECAUSE another failed: the `errorAction` is an
/// action the engine attempted too, so its own outcome is counted on the same rule's series —
/// one failure for the action that failed and one success for the error action that reported it.
#[tokio::test]
async fn an_error_actions_own_attempt_is_counted_too() {
    let emulator = emulator();
    let case = "a rule whose action fails and whose errorAction is performed";
    let barred = given_role(&emulator, case, "role_barred", SERVICE_PRINCIPAL, None).await;
    let granting = given_role(
        &emulator,
        case,
        "role_granting",
        SERVICE_PRINCIPAL,
        Some((IOT_PUBLISH_ACTION, "*")),
    )
    .await;
    let rule = "reports_its_failures";
    emulator
        .create_topic_rule(
            case,
            rule,
            &json!({
                "sql": "SELECT * FROM 'staging/reported'",
                "awsIotSqlVersion": SQL_VERSION,
                "ruleDisabled": false,
                "actions": [{
                    "republish": { "roleArn": barred, "topic": "restaged/blocked", "qos": 1 },
                }],
                "errorAction": {
                    "republish": { "roleArn": granting, "topic": "restaged/failures", "qos": 1 },
                },
            }),
        )
        .await;

    emulator
        .publish(case, "staging/reported", br#"{"marker":"one"}"#)
        .await;

    let (successes, failures) = outcomes(&emulator, case, rule).await;
    assert_eq!(
        failures, 1.0,
        "case {case:?}: the refused action must be counted once on the {FAILURE_METRIC} series \
         of {rule:?}, got {failures}"
    );
    assert_eq!(
        successes, 1.0,
        "case {case:?}: the errorAction that reported it must be counted once on the \
         {SUCCESS_METRIC} series of {rule:?}, got {successes}"
    );
}

/// RULE as a DIFFERENTIAL, the shape a client verifies a pipeline with: sibling rules on ONE
/// topic, one permitted and one not, each reporting only its own outcome — however many
/// messages are published.
#[tokio::test]
async fn sibling_rules_on_one_topic_each_report_only_their_own_outcome() {
    let emulator = emulator();
    let case = "sibling rules on one topic";
    let topic = "fleet/telemetry";
    let permitted_rule = "permitted_sibling";
    let unpermitted_rule = "unpermitted_sibling";
    let permitted_function = "sibling_permitted";
    let unpermitted_function = "sibling_unpermitted";

    let permitted_arn = create_function(&emulator, case, permitted_function).await;
    let unpermitted_arn = create_function(&emulator, case, unpermitted_function).await;
    wait_active(&emulator, case, permitted_function).await;
    wait_active(&emulator, case, unpermitted_function).await;

    for (rule, target) in [
        (permitted_rule, &permitted_arn),
        (unpermitted_rule, &unpermitted_arn),
    ] {
        emulator
            .create_topic_rule(
                case,
                rule,
                &json!({
                    "sql": format!("SELECT * FROM '{topic}'"),
                    "awsIotSqlVersion": SQL_VERSION,
                    "ruleDisabled": false,
                    "actions": [{ "lambda": { "functionArn": target } }],
                }),
            )
            .await;
    }
    let permitted_rule_arn = emulator.rule_arn(case, permitted_rule).await;
    add_permission(
        &emulator,
        case,
        permitted_function,
        SERVICE_PRINCIPAL,
        Some(&permitted_rule_arn),
    )
    .await;

    let messages = 4;
    for index in 0..messages {
        emulator
            .publish(
                case,
                topic,
                json!({ "marker": index }).to_string().as_bytes(),
            )
            .await;
    }

    let expected = f64::from(messages);
    let (permitted_successes, permitted_failures) = outcomes(&emulator, case, permitted_rule).await;
    assert_eq!(
        (permitted_successes, permitted_failures),
        (expected, 0.0),
        "case {case:?}: the permitted sibling must report {expected} success(es) and no \
         failure, got {permitted_successes} and {permitted_failures}"
    );
    let (unpermitted_successes, unpermitted_failures) =
        outcomes(&emulator, case, unpermitted_rule).await;
    assert_eq!(
        (unpermitted_successes, unpermitted_failures),
        (0.0, expected),
        "case {case:?}: the unpermitted sibling must report {expected} failure(s) and no \
         success, got {unpermitted_successes} and {unpermitted_failures}"
    );
}

/// One row of the uncounted table: a rule that attempts NOTHING for the message published to
/// its own topic, and why.
struct UncountedRow {
    case: &'static str,
    /// The rule the row creates, and the dimension its (absent) series would be read by.
    rule: &'static str,
    /// The topic the row publishes to.
    topic: &'static str,
    /// The topic filter the rule's statement names, which need not cover that topic.
    filter: &'static str,
    /// Whether the rule is created disabled.
    disabled: bool,
    /// The action the rule carries, which is what decides WHERE the message is dropped.
    action: Value,
}

/// RULE from the other side: a rule that ATTEMPTED nothing has no rule-action series at all —
/// whether the engine never reached its actions (a disabled rule, a filter that does not cover
/// the topic) or reached one it does not perform for this message (a parameter whose
/// substitution template it does not evaluate, an endpoint no topic rule destination admits).
/// The control rule in the same store, whose action IS attempted, is what keeps the assertion
/// from passing on an engine that publishes nothing at all.
#[tokio::test]
async fn a_rule_that_attempted_nothing_has_no_series() {
    let emulator = emulator();
    let case = "rules that attempt nothing";
    let function = "never_reached";

    let target = create_function(&emulator, case, function).await;
    wait_active(&emulator, case, function).await;
    // The role is not what any row below is about, so it grants the republish: what stops the
    // action is the parameter, which is read before the role is.
    let role = given_role(
        &emulator,
        case,
        "role_uncounted",
        SERVICE_PRINCIPAL,
        Some((IOT_PUBLISH_ACTION, "*")),
    )
    .await;
    // A template that never closes is one this engine does not evaluate, so the parameter
    // carrying it has no value and the action is not performed for the message.
    let unrenderable = "staged/${";

    let rows = vec![
        UncountedRow {
            case: "a disabled rule whose filter covers the topic",
            rule: "disabled_rule",
            topic: "fleet/disabled",
            filter: "fleet/disabled",
            disabled: true,
            action: json!({ "lambda": { "functionArn": target } }),
        },
        UncountedRow {
            case: "an enabled rule whose filter does not cover the topic",
            rule: "elsewhere_rule",
            topic: "fleet/elsewhere",
            filter: "fleet/another",
            disabled: false,
            action: json!({ "lambda": { "functionArn": target } }),
        },
        UncountedRow {
            case: "a republish action whose topic carries a template this engine does not render",
            rule: "unrenderable_republish_rule",
            topic: "fleet/unrenderable-topic",
            filter: "fleet/unrenderable-topic",
            disabled: false,
            action: json!({ "republish": { "roleArn": role, "topic": unrenderable, "qos": 1 } }),
        },
        UncountedRow {
            case: "a kinesis action whose streamName carries a template this engine does not \
                   render",
            rule: "unrenderable_kinesis_rule",
            topic: "fleet/unrenderable-stream",
            filter: "fleet/unrenderable-stream",
            disabled: false,
            action: json!({
                "kinesis": {
                    "roleArn": role,
                    "streamName": unrenderable,
                    "partitionKey": "by-device",
                },
            }),
        },
        UncountedRow {
            case: "an http action whose endpoint has no topic rule destination",
            rule: "unconfirmed_endpoint_rule",
            topic: "fleet/unconfirmed-endpoint",
            filter: "fleet/unconfirmed-endpoint",
            disabled: false,
            action: json!({ "http": { "url": "https://endpoint.invalid/events" } }),
        },
    ];

    // The CONTROL: one rule of the same store whose `lambda` action IS attempted — the function
    // carries no resource policy, so the invocation is refused and the attempt is a failure.
    let control_rule = "attempted_control_rule";
    let control_topic = "fleet/control";
    emulator
        .create_topic_rule(
            case,
            control_rule,
            &json!({
                "sql": format!("SELECT * FROM '{control_topic}'"),
                "awsIotSqlVersion": SQL_VERSION,
                "ruleDisabled": false,
                "actions": [{ "lambda": { "functionArn": target } }],
            }),
        )
        .await;

    for row in &rows {
        emulator
            .create_topic_rule(
                row.case,
                row.rule,
                &json!({
                    "sql": format!("SELECT * FROM '{}'", row.filter),
                    "awsIotSqlVersion": SQL_VERSION,
                    "ruleDisabled": row.disabled,
                    "actions": [row.action],
                }),
            )
            .await;
    }

    for row in &rows {
        emulator
            .publish(row.case, row.topic, br#"{"marker":"one"}"#)
            .await;
    }
    emulator
        .publish(case, control_topic, br#"{"marker":"one"}"#)
        .await;

    for row in &rows {
        let (successes, failures) = outcomes(&emulator, row.case, row.rule).await;
        assert_eq!(
            (successes, failures),
            (0.0, 0.0),
            "case {:?}: the rule {:?} attempted nothing, so it must report no outcome, got \
             {successes} and {failures}",
            row.case,
            row.rule
        );
    }

    // The namespace holds exactly the control's one failure: every other rule above attempted
    // nothing, so none of them has a series at all.
    let listed = listed_series(&emulator, case).await;
    assert_eq!(
        listed,
        vec![(
            FAILURE_METRIC.to_string(),
            vec![(RULE_NAME_DIMENSION.to_string(), control_rule.to_string())],
        )],
        "case {case:?}: only the rule whose action was attempted may have a series, got \
         {listed:?}"
    );
}

/// RULE applied to the chain an `http` action can close: a rule whose endpoint is this
/// emulator's own publish path runs the engine again for every delivery, so the actions one
/// published message sets off are counted on the rule's own series — and the whole chain stays
/// inside the budget ONE message carries, which is what the counted series makes readable.
///
/// Without that one budget the chain would be bounded per level rather than in total, so this
/// case is what tells a terminating chain from a chain each of whose levels starts over.
#[tokio::test]
async fn an_endpoint_chain_that_feeds_itself_stays_inside_one_messages_action_budget() {
    let emulator = emulator();
    let case = "a rule whose endpoint is this emulator's own publish path";
    let topic = "loopback";
    // The endpoint is a retained `Publish` of this emulator's own IoT data plane, so every
    // delivery is a new publish on the very topic the rule selects: the chain feeds itself.
    let endpoint = format!("https://iotdata.{REGION}.amazonaws.com/topics/{topic}?qos=1&retain=true");

    // The destination the action's URL is admitted by, confirmed with the token the challenge
    // carried and enabled by that confirmation — the gate `http_action_is_allowed` reads.
    let arn = emulator.create_destination(case, &endpoint).await;
    let token = emulator.outstanding_token(case, &arn);
    emulator.confirm_destination(case, &token).await;
    // A confirmed destination rests at DISABLED — the move is the clock's — and it is the
    // update to ENABLED that opens its traffic gate.
    emulator
        .destination_rests_at(case, &arn, DESTINATION_DISABLED)
        .await;
    emulator
        .update_destination(case, &arn, DESTINATION_ENABLED)
        .await;
    emulator
        .destination_rests_at(case, &arn, DESTINATION_ENABLED)
        .await;

    let rule = "loopback_rule";
    emulator
        .create_topic_rule(
            case,
            rule,
            &json!({
                "sql": format!("SELECT * FROM '{topic}'"),
                "awsIotSqlVersion": SQL_VERSION,
                "ruleDisabled": false,
                "actions": [{ "http": { "url": endpoint } }],
            }),
        )
        .await;

    // ONE publish. It returns, which is already the first half of the rule: the chain it sets
    // off ends rather than nesting for ever.
    emulator
        .publish(case, topic, br#"{"marker":"one"}"#)
        .await;

    let (successes, failures) = outcomes(&emulator, case, rule).await;
    let performed = successes + failures;
    assert!(
        performed > 1.0,
        "case {case:?}: the chain must really nest — the rule's own endpoint publishes to the \
         topic it selects — so more than one action must be counted, got {successes} \
         success(es) and {failures} failure(s)"
    );
    let budget = f64::from(u32::try_from(MAX_ACTIONS_PER_MESSAGE).expect("the budget as a count"));
    assert!(
        performed <= budget,
        "case {case:?}: every action of the chain one message sets off comes out of that \
         message's budget of {budget}, got {performed} counted ({successes} success(es) and \
         {failures} failure(s))"
    );
}
