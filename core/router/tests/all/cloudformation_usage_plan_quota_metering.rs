// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack declares a METERED REST API — an `AWS::ApiGateway::ApiKey`, an
//! `::UsagePlan` carrying a quota over the api stage, and the `::UsagePlanKey` that puts the
//! key in the plan — and the keyed calls to the invoke URL the stack published are metered
//! against that quota.
//!
//! The RULE under test: **a usage plan's `quota` is a BUDGET of requests per metering
//! period, counted per `(usage plan, API key)` pair: the deployed stage serves the requests
//! the period's budget still has room for and refuses every later request carrying that key
//! with the `QUOTA_EXCEEDED` gateway response — HTTP 429, `LimitExceededException`, the
//! message "Limit Exceeded" — and `GetUsage` reports that accounting as the pair's daily log
//! of `[used, remaining]` over the interval the request names.** It holds whoever created the
//! plan: a stack that declares those three types holds the same records a client creating
//! them through API Gateway's own API would, so the quota a template states is the one the
//! data plane enforces and the one the usage report counts down.
//!
//! Two crates have to agree for that to hold — CloudFormation makes the control-plane calls,
//! API Gateway serves them, enforces the quota at the invoke URL and answers the usage read —
//! so the suite lives here rather than in either of them.
//!
//! The rows differ along what the rule generalises over: the quota's `limit` (one, a few, and
//! none at all, which bounds nothing), and whether the template states the key's `Value` or
//! leaves it to API Gateway to mint.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    usage_date_string, usage_today,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, outputs_of};

/// The Region every request here is signed for, and so the Region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// The `StackStatus` a stack that holds every declared resource reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stage the template deploys, and the stage its usage plan meters.
const STAGE: &str = "live";

/// The `message` the `QUOTA_EXCEEDED` gateway response carries, which is what a request past
/// the period's budget is refused with.
const LIMIT_EXCEEDED: &str = "Limit Exceeded";

/// The exception class that refusal is reported in, as the `x-amzn-errortype` header spells
/// it — what a client's error handling keys off, and what tells a spent quota apart from the
/// plan's throttle.
const LIMIT_EXCEEDED_CLASS: &str = "LimitExceededException";

/// The body the stack's `MOCK` integration answers, which is what proves a request was
/// SERVED rather than refused.
const SERVED: &str = r#"{"served":true}"#;

/// The emulator as it is ASSEMBLED, on one engine: the resources CloudFormation creates are
/// the resources this suite then invokes and reads through.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Read one API Gateway control-plane resource over its restJson1 wire: that service signs
/// as `apigateway` and binds every REST operation under its own URI.
async fn apigateway(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// One answered invoke of the deployed stage: its status, its `x-amzn-errortype` class and
/// its body verbatim.
struct Invoked {
    status: u16,
    error: Option<String>,
    body: String,
}

/// Invoke the deployed stage at the invoke URL the stack published, exactly as a client
/// reaches AWS: the request's host is the api's own execute-api endpoint and its path is the
/// stage and the resource path.
async fn invoke(router: &MultiServiceRouter, invoke_url: &str, api_key: &str) -> Invoked {
    let without_scheme = invoke_url
        .strip_prefix("https://")
        .or_else(|| invoke_url.strip_prefix("http://"))
        .unwrap_or(invoke_url);
    let (host, path) = match without_scheme.split_once('/') {
        Some((host, path)) => (host.to_string(), format!("/{path}")),
        None => (without_scheme.to_string(), "/".to_string()),
    };
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", host)
        .header("content-type", "application/json")
        .header("x-api-key", api_key)
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    Invoked {
        status,
        error,
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// Today's UTC day, in the one format this surface spells a date in: `startDate` and
/// `endDate` are "the starting date (e.g., 2016-01-01)" and "the ending date (e.g.,
/// 2016-12-31) of the usage data" (`smithy_query(query="doc", shape="GetUsageRequest",
/// member="startDate")`), and a quota period "resets at midnight UTC", so the day a metered
/// request is counted against is its UTC day.
///
/// Read through the API Gateway crate's own public rendering of both, so the day this suite
/// asks about is the day the service counts a request against.
fn today() -> String {
    usage_date_string(usage_today())
}

/// One row of the table: what the template declares of the quota and of the key's value, how
/// many keyed requests the row sends, and how many of them the period admits.
struct Row {
    case: &'static str,
    /// The `Quota` the plan declares, `None` for a plan that declares none — which bounds
    /// nothing, so every keyed request is served and still counted.
    quota: Option<Value>,
    /// The `Limit` that quota states, for the `remaining` the report must then carry; `None`
    /// for the plan that declares no quota.
    limit: Option<i64>,
    /// How many keyed requests the row sends.
    sent: usize,
    /// How many of them the period admits — the rest are refused by the quota.
    served: usize,
    /// The key's `Value` where the template states one; `None` leaves the value to API
    /// Gateway to mint, which the suite then reads back through `GetApiKey`.
    stated_value: Option<&'static str>,
}

/// The template one row deploys: the api, its one key-required `MOCK` method, the deployment
/// and stage that serve it, and the key, plan and association that meter it.
fn template(stack: &str, row: &Row) -> String {
    let mut key = json!({
        "Name": format!("{stack}-key"),
        "Enabled": true,
    });
    if let Some(value) = row.stated_value {
        key["Value"] = json!(value);
    }
    let mut plan = json!({
        "UsagePlanName": format!("{stack}-plan"),
        "ApiStages": [{ "ApiId": { "Ref": "Api" }, "Stage": STAGE }],
    });
    if let Some(quota) = &row.quota {
        plan["Quota"] = quota.clone();
    }
    json!({
        "Resources": {
            "Api": {
                "Type": "AWS::ApiGateway::RestApi",
                "Properties": {
                    "Name": format!("{stack}-api"),
                    "EndpointConfiguration": { "Types": ["REGIONAL"] }
                }
            },
            "Items": {
                "Type": "AWS::ApiGateway::Resource",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ParentId": { "Fn::GetAtt": ["Api", "RootResourceId"] },
                    "PathPart": "items"
                }
            },
            "Method": {
                "Type": "AWS::ApiGateway::Method",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "ResourceId": { "Ref": "Items" },
                    "HttpMethod": "GET",
                    "AuthorizationType": "NONE",
                    "ApiKeyRequired": true,
                    "MethodResponses": [{ "StatusCode": "200" }],
                    "Integration": {
                        "Type": "MOCK",
                        "RequestTemplates": { "application/json": "{\"statusCode\": 200}" },
                        "IntegrationResponses": [{
                            "StatusCode": "200",
                            "ResponseTemplates": { "application/json": SERVED }
                        }]
                    }
                }
            },
            "Deployment": {
                "Type": "AWS::ApiGateway::Deployment",
                "DependsOn": ["Method"],
                "Properties": { "RestApiId": { "Ref": "Api" } }
            },
            "Stage": {
                "Type": "AWS::ApiGateway::Stage",
                "Properties": {
                    "RestApiId": { "Ref": "Api" },
                    "DeploymentId": { "Ref": "Deployment" },
                    "StageName": STAGE
                }
            },
            "Key": { "Type": "AWS::ApiGateway::ApiKey", "Properties": key },
            "Plan": { "Type": "AWS::ApiGateway::UsagePlan", "Properties": plan },
            "PlanKey": {
                "Type": "AWS::ApiGateway::UsagePlanKey",
                "Properties": {
                    "KeyId": { "Ref": "Key" },
                    "KeyType": "API_KEY",
                    "UsagePlanId": { "Ref": "Plan" }
                }
            }
        },
        "Outputs": {
            "KeyId": { "Value": { "Ref": "Key" } },
            "PlanId": { "Value": { "Ref": "Plan" } },
            "InvokeUrl": {
                "Value": {
                    "Fn::Sub": format!(
                        "https://${{Api}}.execute-api.${{AWS::Region}}.amazonaws.com/{STAGE}/items"
                    )
                }
            }
        }
    })
    .to_string()
}

#[tokio::test]
async fn a_stack_declared_usage_plan_meters_the_keyed_calls_of_the_stage_it_names() {
    let rows: &[Row] = &[
        Row {
            case: "a DAY quota of three, driven to exhaustion, over a key whose value the \
                   template states",
            quota: Some(json!({ "Limit": 3, "Offset": 0, "Period": "DAY" })),
            limit: Some(3),
            sent: 4,
            served: 3,
            stated_value: Some("stack-metered-api-key-value-0001"),
        },
        Row {
            case: "a DAY quota of three with one request of it spent, over a key whose value \
                   API Gateway mints",
            quota: Some(json!({ "Limit": 3, "Offset": 0, "Period": "DAY" })),
            limit: Some(3),
            sent: 1,
            served: 1,
            stated_value: None,
        },
        Row {
            case: "a DAY quota of one",
            quota: Some(json!({ "Limit": 1, "Period": "DAY" })),
            limit: Some(1),
            sent: 2,
            served: 1,
            stated_value: Some("stack-metered-api-key-value-0002"),
        },
        Row {
            case: "a MONTH quota of two",
            quota: Some(json!({ "Limit": 2, "Period": "MONTH" })),
            limit: Some(2),
            sent: 3,
            served: 2,
            stated_value: Some("stack-metered-api-key-value-0003"),
        },
        Row {
            case: "a plan that declares no quota at all",
            quota: None,
            limit: None,
            sent: 4,
            served: 4,
            stated_value: Some("stack-metered-api-key-value-0004"),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let router = build_router();
        let stack = format!("apigw-usage-plan-quota-{index}");
        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &template(&stack, row)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {}: expected HTTP 200 from CreateStack, got {status} with body {body}",
            row.case
        );
        let (status, described) = wait_for_stack_status(&router, &stack, CREATE_COMPLETE).await;
        assert_eq!(
            status, CREATE_COMPLETE,
            "case {}: expected the stack to reach {CREATE_COMPLETE}, got {described}",
            row.case
        );
        let outputs = outputs_of(&described);
        let invoke_url = outputs
            .get("InvokeUrl")
            .unwrap_or_else(|| panic!("case {}: no InvokeUrl output in {described}", row.case))
            .clone();
        let key_id = outputs
            .get("KeyId")
            .unwrap_or_else(|| panic!("case {}: no KeyId output in {described}", row.case))
            .clone();
        let plan_id = outputs
            .get("PlanId")
            .unwrap_or_else(|| panic!("case {}: no PlanId output in {described}", row.case))
            .clone();

        // The key's VALUE is what a client presents; a row that states none reads back the
        // value API Gateway minted, through that service's own `GetApiKey`.
        let key_value = match row.stated_value {
            Some(value) => value.to_string(),
            None => {
                let (status, key) =
                    apigateway(&router, &format!("/apikeys/{key_id}?includeValue=true")).await;
                assert_eq!(
                    status, 200,
                    "case {}: expected 200 from GetApiKey, got {status} with {key}",
                    row.case
                );
                key["value"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {}: GetApiKey reported no value: {key}", row.case)
                    })
                    .to_string()
            }
        };

        // The requests the row sends: every one inside the period's budget is SERVED by the
        // method's integration, and every one past it is refused by the quota.
        let sent = row.sent;
        let mut answered = Vec::new();
        for _ in 0..sent {
            answered.push(invoke(&router, &invoke_url, &key_value).await);
        }
        for (ordinal, served) in answered.iter().enumerate().take(row.served) {
            assert_eq!(
                served.status, 200,
                "case {}: request {} of {sent} is inside the period's budget and must be \
                 served, got {} with {:?}",
                row.case,
                ordinal + 1,
                served.status,
                served.body
            );
            assert_eq!(
                served.body, SERVED,
                "case {}: request {} of {sent} must carry the integration's own body, got {:?}",
                row.case,
                ordinal + 1,
                served.body
            );
        }
        for (ordinal, refused) in answered.iter().enumerate().skip(row.served) {
            assert_eq!(
                refused.status, 429,
                "case {}: request {} of {sent} is past the period's budget and must be \
                 refused 429, got {} with {:?}",
                row.case,
                ordinal + 1,
                refused.status,
                refused.body
            );
            assert_eq!(
                refused.error.as_deref(),
                Some(LIMIT_EXCEEDED_CLASS),
                "case {}: the refusal must be reported as {LIMIT_EXCEEDED_CLASS}, got {:?} \
                 with {:?}",
                row.case,
                refused.error,
                refused.body
            );
            assert!(
                refused.body.contains(LIMIT_EXCEEDED),
                "case {}: the refusal must carry the message {LIMIT_EXCEEDED:?}, got {:?}",
                row.case,
                refused.body
            );
        }

        // What the usage read reports is the accounting the data plane wrote: the requests
        // the period COUNTED for that pair on this UTC day, and what the quota has left
        // after them.
        let today = today();
        let (status, usage) = apigateway(
            &router,
            &format!(
                "/usageplans/{plan_id}/usage?startDate={today}&endDate={today}&keyId={key_id}"
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {}: expected 200 from GetUsage, got {status} with {usage}",
            row.case
        );
        assert_eq!(
            usage["usagePlanId"].as_str(),
            Some(plan_id.as_str()),
            "case {}: the report must name the plan it is of, got {usage}",
            row.case
        );
        assert_eq!(
            (
                usage["startDate"].as_str(),
                usage["endDate"].as_str()
            ),
            (Some(today.as_str()), Some(today.as_str())),
            "case {}: the report must cover the interval the request named, got {usage}",
            row.case
        );
        // A refused request consumed none of the period, so what is COUNTED is what was
        // served, and what is REMAINING is the declared limit less that count — the number
        // the next request would be admitted against. A plan that declares no quota has no
        // budget at all, and nothing this suite can cite fixes what its log reports as
        // remaining, so that row asserts the count alone.
        let counted = i64::try_from(row.served).expect("a request count");
        let log = &usage["values"][&key_id];
        match row.limit {
            Some(limit) => assert_eq!(
                *log,
                json!([[counted, (limit - counted).max(0)]]),
                "case {}: the report must index the metered key with the requests the period \
                 counted and the quota it has left, got {usage}",
                row.case
            ),
            None => assert_eq!(
                log[0][0],
                json!(counted),
                "case {}: the report must index the metered key with every request it sent, \
                 got {usage}",
                row.case
            ),
        }
    }
}
