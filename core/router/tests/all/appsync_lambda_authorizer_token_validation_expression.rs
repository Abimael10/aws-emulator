// SPDX-License-Identifier: CC0-1.0

//! The regex gate an AppSync GraphQL API puts in front of its Lambda AUTHORIZER, reached the way
//! a client reaches it: by POSTing to the address the API published for its GraphQL endpoint.
//!
//! RULE — `LambdaAuthorizerConfig$identityValidationExpression` is "a regular expression for
//! validation of tokens before the Lambda function is called"
//! (`smithy_query(query="shape", shape="LambdaAuthorizerConfig")`), so on an API that configures
//! one the material a request presents is VALIDATED against it before the authorizer is
//! addressed at all: a token the expression does not describe end to end is refused by the
//! endpoint itself — HTTP 401, exactly one error whose `errorType` is `UnauthorizedException`
//! and whose message is this mode's own sentence, no `data` member in the body, and no
//! invocation of the authorizer function — while a token it does describe reaches the function,
//! which then decides the request. An API that configures no expression validates nothing, so
//! every token it is presented reaches the function.
//!
//! Two services have to agree for the rule to hold — Lambda runs the authorizer, and AppSync
//! publishes the endpoint and applies the gate in front of it — and CloudWatch is where the
//! invocations the gate did NOT make are read (`AWS/Lambda` `Invocations` per `FunctionName`),
//! so this is the suite that can pin it.
//!
//! The rows span what the rule generalises over: the FORM of the expression (anchored,
//! unanchored, a character class under a length bound, an alternation, and none configured at
//! all), WHERE the API carries it (its own `lambdaAuthorizerConfig`, and the one an additional
//! authentication provider carries), and how the presented token relates to it (described whole,
//! described only in part, and not described at all). The authorizer of every row authorizes
//! UNCONDITIONALLY, so every refusal below is the gate's and could not be the function's.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appsync_lambda_data_source_and_authorizer::{
    appsync_ok, build_router, create_function, post_published_uri,
};

/// The Region every setup call here signs for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which every setup call is signed with.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`), which is
/// what routes a call to it.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The namespace Lambda publishes its own work in, the dimension it reports per, and the series
/// that counts the invocations a function was handed.
const LAMBDA_NAMESPACE: &str = "AWS/Lambda";
const FUNCTION_NAME_DIMENSION: &str = "FunctionName";
const INVOCATIONS: &str = "Invocations";

/// The period the series is read at, in seconds, and how far either side of the traffic the read
/// window reaches — generous, because which window a datapoint falls in is decided by the instant
/// the invocation was made at, and the traffic takes as long as the machine takes.
const PERIOD: i64 = 60;
const WINDOW_MARGIN_PERIODS: f64 = 2.0;

/// The `errorType` and the sentence the `AWS_LAMBDA` mode refuses an unauthorized call with.
const UNAUTHORIZED_ERROR_TYPE: &str = "UnauthorizedException";
const UNAUTHORIZED_MESSAGE: &str = "You are not authorized to make this call.";

/// The authorization types the APIs below set up (`AuthenticationType`).
const LAMBDA_MODE: &str = "AWS_LAMBDA";
const API_KEY_MODE: &str = "API_KEY";

/// The data source every resolver here resolves against: `NONE` is "there is no data source", so
/// nothing but the resolver's own definition decides the field.
const LOCAL_DATA_SOURCE: &str = "local";

/// The status a schema creation settles at once the document has been installed.
const SCHEMA_CREATED: &str = "SUCCESS";

/// How long a poll loop waits for state another service owes, and how often it looks again.
/// Generous: what is under test is what the endpoint answers, never how fast the host reaches a
/// transition or publishes a datapoint.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The schema every API of this suite serves: one field, which answers the argument it was asked
/// for, so a served request is told from a refused one by its own body.
///
/// The field is written under `@aws_lambda`, the directive that reserves a field for the
/// `AWS_LAMBDA` mode, because an API that sets up more than one mode says per field which of them
/// may reach it — the row that carries its authorizer on an additional provider is such an API.
/// An API that sets up that mode alone says nothing per field, so the directive changes nothing
/// for the rest.
const SCHEMA: &str =
    "schema { query: Query }\n\ntype Query {\n  echo(marker: String!): String @aws_lambda\n}\n";

/// The field the document selects, and the argument it carries.
const FIELD: &str = "echo";

/// The template pair that resolves that field: the request half hands the argument back as the
/// payload a `NONE` data source answers with, and the response half answers it.
const ECHO_REQUEST_TEMPLATE: &str =
    "{\"version\": \"2018-05-29\", \"payload\": $util.toJson($ctx.args.marker)}";
const ECHO_RESPONSE_TEMPLATE: &str = "$util.toJson($ctx.result)";

/// The authorizer every API of this suite is gated by: it authorizes EVERY token it is handed,
/// and states no `ttlOverride`, so with no window configured each admitted request is one
/// invocation of its own.
const AUTHORIZER_HANDLER: &str = r#"
def handler(event, context):
    return {"isAuthorized": True}
"#;

/// The function the authorizer of every API below is.
const AUTHORIZER_FUNCTION: &str = "token-validation-authorizer";

/// One API of this suite: the expression it configures its authorizer's token validation with,
/// and where it carries the configuration that states it.
struct Configured {
    case: &'static str,
    /// The expression, or `None` for an API that configures none.
    expression: Option<&'static str>,
    /// Whether the Lambda authorizer is set up as an ADDITIONAL authentication provider rather
    /// than as the API's own `authenticationType`.
    on_additional_provider: bool,
}

/// One request of a row: the token presented, and whether the expression in force describes it.
struct Presented {
    case: &'static str,
    token: &'static str,
    admitted: bool,
}

/// RULE: the token a request presents is validated against the API's
/// `identityValidationExpression` BEFORE its Lambda authorizer is invoked, so a token the
/// expression does not describe whole is refused by the endpoint — 401, one
/// `UnauthorizedException`, no `data` member and no invocation — and a token it describes reaches
/// the function and resolves.
#[tokio::test]
async fn a_token_is_validated_against_the_expression_before_the_authorizer_is_invoked() {
    let router = build_router();
    let authorizer = create_function(&router, AUTHORIZER_FUNCTION, AUTHORIZER_HANDLER).await;

    // The rows differ along the FORM of the expression and along where the API carries it.
    let configured: Vec<Configured> = vec![
        Configured {
            case: "an anchored expression",
            expression: Some("^ok-.*$"),
            on_additional_provider: false,
        },
        Configured {
            case: "an unanchored character class under a length bound",
            expression: Some("tok-[0-9]{4}"),
            on_additional_provider: false,
        },
        Configured {
            case: "an alternation",
            expression: Some("allow|permit"),
            on_additional_provider: false,
        },
        Configured {
            case: "no expression at all, which validates nothing",
            expression: None,
            on_additional_provider: false,
        },
        Configured {
            case: "an expression carried by an additional authentication provider",
            expression: Some("^ok-.*$"),
            on_additional_provider: true,
        },
    ];

    // The requests each row makes, in the same order as the rows above.
    let presented: Vec<Vec<Presented>> = vec![
        vec![
            Presented {
                case: "a token the anchored expression describes",
                token: "ok-first",
                admitted: true,
            },
            Presented {
                case: "a token of another prefix",
                token: "nope-first",
                admitted: false,
            },
            Presented {
                case: "a token that leaves the anchored expression at its start",
                token: "xok-first",
                admitted: false,
            },
        ],
        vec![
            Presented {
                case: "a token the class describes at the bound",
                token: "tok-1234",
                admitted: true,
            },
            Presented {
                case: "a token short of the length the bound asks for",
                token: "tok-12",
                admitted: false,
            },
            Presented {
                case: "a token the unanchored expression describes only in part",
                token: "prefix-tok-1234",
                admitted: false,
            },
        ],
        vec![
            Presented {
                case: "the first branch of the alternation",
                token: "allow",
                admitted: true,
            },
            Presented {
                case: "the second branch of the alternation",
                token: "permit",
                admitted: true,
            },
            Presented {
                case: "a token one branch describes only in part",
                token: "allowed",
                admitted: false,
            },
        ],
        vec![Presented {
            case: "any token at all, on an API that configures no expression",
            token: "an-unvalidated-token",
            admitted: true,
        }],
        vec![
            Presented {
                case: "a token the provider's expression describes",
                token: "ok-provider",
                admitted: true,
            },
            Presented {
                case: "a token the provider's expression does not describe",
                token: "nope-provider",
                admitted: false,
            },
        ],
    ];

    // Every API is set up before any traffic is driven, and each one's schema creation is started
    // before any of them is waited for, so what the suite waits for is one span rather than one
    // per API.
    let mut apis: Vec<String> = Vec::new();
    for (index, row) in configured.iter().enumerate() {
        apis.push(create_api(&router, index, row, &authorizer).await);
    }
    for (index, row) in configured.iter().enumerate() {
        settle_schema(&router, row.case, &apis[index]).await;
        attach_resolver(&router, &apis[index]).await;
    }

    // Taken before the first request, so the read window below covers every invocation the
    // traffic produced and none of the setup.
    let since = now_secs();
    let mut admitted_requests = 0.0_f64;
    for (index, row) in configured.iter().enumerate() {
        let graphql_uri = published_uri(&router, row.case, &apis[index]).await;
        for (attempt, request) in presented[index].iter().enumerate() {
            let case = format!("{} / {}", row.case, request.case);
            let marker = format!("marker-{index}-{attempt}");
            let (status, answered) = post_published_uri(
                &case,
                &graphql_uri,
                &json!({
                    "query": "query Echo($marker: String!) { echo(marker: $marker) }",
                    "variables": {"marker": marker},
                }),
                &[("authorization", request.token.to_string())],
            )
            .await;
            if request.admitted {
                admitted_requests += 1.0;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected a validated token to reach the authorizer and \
                     resolve, got {status} and {answered}"
                );
                assert_eq!(
                    answered["data"][FIELD].as_str(),
                    Some(marker.as_str()),
                    "case {case:?}: expected the field to answer {marker:?}, got {answered}"
                );
                assert!(
                    answered.get("errors").is_none(),
                    "case {case:?}: expected no errors member, got {answered}"
                );
                continue;
            }
            assert_eq!(
                status, 401,
                "case {case:?}: expected a token the expression does not describe to be refused \
                 before the always-authorizing function could run, got {status} and {answered}"
            );
            assert!(
                answered.get("data").is_none(),
                "case {case:?}: expected no data member in the refusal, got {answered}"
            );
            let errors = reported_errors(&answered);
            assert_eq!(
                errors.len(),
                1,
                "case {case:?}: expected exactly one error, got {errors:?}"
            );
            assert_eq!(
                (errors[0].0.as_str(), errors[0].1.as_str()),
                (UNAUTHORIZED_ERROR_TYPE, UNAUTHORIZED_MESSAGE),
                "case {case:?}: expected the error \
                 {UNAUTHORIZED_ERROR_TYPE:?}/{UNAUTHORIZED_MESSAGE:?}, got {errors:?}"
            );
        }
    }

    // And what RAN: the authorizer was handed one invocation per request whose token the
    // expression described, and none at all for the rest — so the refusals above were taken in
    // front of the function rather than by it.
    let case = format!("{INVOCATIONS} of {AUTHORIZER_FUNCTION}");
    let counted = awaited_invocations(&router, &case, since, admitted_requests).await;
    assert_eq!(
        counted, admitted_requests,
        "case {case:?}: expected {admitted_requests} invocation(s) — one per request the \
         expression admitted — got {counted}"
    );
}

/// Create one API gated by `function_arn` whose authorizer validates tokens against `row`'s
/// expression, and start the installation of [`SCHEMA`] on it. Answers the API's id.
///
/// Where the row carries the authorizer on an additional provider, the API's own
/// `authenticationType` is another mode entirely, so what the gate is read off is the PROVIDER's
/// configuration.
async fn create_api(
    router: &MultiServiceRouter,
    index: usize,
    row: &Configured,
    function_arn: &str,
) -> String {
    let mut authorizer_config = json!({"authorizerUri": function_arn});
    if let Some(expression) = row.expression {
        authorizer_config["identityValidationExpression"] = json!(expression);
    }
    let mut body = json!({"name": format!("token-validation-{index}")});
    if row.on_additional_provider {
        body["authenticationType"] = json!(API_KEY_MODE);
        body["additionalAuthenticationProviders"] = json!([{
            "authenticationType": LAMBDA_MODE,
            "lambdaAuthorizerConfig": authorizer_config,
        }]);
    } else {
        body["authenticationType"] = json!(LAMBDA_MODE);
        body["lambdaAuthorizerConfig"] = authorizer_config;
    }
    let created = appsync_ok(router, "POST", "/v1/apis", Some(body)).await;
    let api_id = created["graphqlApi"]["apiId"]
        .as_str()
        .unwrap_or_else(|| panic!("setup {:?}: CreateGraphqlApi reports no apiId", row.case))
        .to_string();
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/schemacreation"),
        Some(json!({"definition": ::aws_smithy_types::base64::encode(SCHEMA)})),
    )
    .await;
    api_id
}

/// Look until the schema creation `api_id` owes has been performed.
async fn settle_schema(router: &MultiServiceRouter, case: &str, api_id: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let settled = appsync_ok(
            router,
            "GET",
            &format!("/v1/apis/{api_id}/schemacreation"),
            None,
        )
        .await;
        if settled["status"].as_str() == Some(SCHEMA_CREATED) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup {case:?}: expected the schema creation to be performed, last read {settled}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Attach the `NONE` data source and the one resolver that answers [`FIELD`].
async fn attach_resolver(router: &MultiServiceRouter, api_id: &str) {
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({"name": LOCAL_DATA_SOURCE, "type": "NONE"})),
    )
    .await;
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Query/resolvers"),
        Some(json!({
            "fieldName": FIELD,
            "kind": "UNIT",
            "dataSourceName": LOCAL_DATA_SOURCE,
            "requestMappingTemplate": ECHO_REQUEST_TEMPLATE,
            "responseMappingTemplate": ECHO_RESPONSE_TEMPLATE,
        })),
    )
    .await;
}

/// The URL `api_id` PUBLISHED for its GraphQL endpoint, read back through `GetGraphqlApi` — the
/// address a client dials, so every request below is a request a customer of this emulator makes.
async fn published_uri(router: &MultiServiceRouter, case: &str, api_id: &str) -> String {
    let fetched = appsync_ok(router, "GET", &format!("/v1/apis/{api_id}"), None).await;
    let published = fetched["graphqlApi"]["uris"]["GRAPHQL"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("setup {case:?}: expected a published GRAPHQL endpoint, got {fetched}")
        })
        .to_string();
    assert!(
        published.starts_with("https://") && published.ends_with("/graphql"),
        "setup {case:?}: expected a URL this emulator serves, got {published:?}"
    );
    published
}

/// The `errors` a result reports, as the `(errorType, message)` pairs each entry states.
fn reported_errors(answered: &Value) -> Vec<(String, String)> {
    answered["errors"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry["errorType"].as_str().unwrap_or_default().to_string(),
                        entry["message"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Wall-clock now as epoch seconds, the clock the read window is measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// Read the authorizer's `Invocations` series until it has counted `expected` of them, and answer
/// what it then counts.
///
/// The wait is on the CONDITION — a datapoint the publication owes becoming readable — so it
/// looks again until the count is there or [`BUDGET`] passes, and the assertion is made on
/// whatever the last read saw. A count that OVERSHOOTS is answered as it stands, which is what
/// makes an invocation the gate should have withheld visible.
async fn awaited_invocations(
    router: &MultiServiceRouter,
    case: &str,
    since: f64,
    expected: f64,
) -> f64 {
    let deadline = Instant::now() + BUDGET;
    loop {
        let counted = invocations(router, case, since).await;
        if counted >= expected || Instant::now() >= deadline {
            return counted;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What the authorizer's own `AWS/Lambda` `Invocations` series holds over the whole window.
async fn invocations(router: &MultiServiceRouter, case: &str, since: f64) -> f64 {
    let margin = WINDOW_MARGIN_PERIODS * PERIOD as f64;
    let answered = monitoring(
        router,
        case,
        "GetMetricStatistics",
        json!({
            "Namespace": LAMBDA_NAMESPACE,
            "MetricName": INVOCATIONS,
            "Dimensions": [{
                "Name": FUNCTION_NAME_DIMENSION,
                "Value": AUTHORIZER_FUNCTION,
            }],
            "StartTime": since - margin,
            "EndTime": now_secs() + margin,
            "Period": PERIOD,
            "Statistics": ["Sum"],
        }),
    )
    .await;
    answered["Datapoints"]
        .as_array()
        .map(|datapoints| {
            datapoints
                .iter()
                .map(|datapoint| datapoint["Sum"].as_f64().unwrap_or_default())
                .sum()
        })
        .unwrap_or_default()
}

/// One CloudWatch call over its `awsJson1_0` wire, which has to answer 200.
async fn monitoring(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let host = format!("{CLOUDWATCH_SIGNING_NAME}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/\
                 {CLOUDWATCH_SIGNING_NAME}/aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: CloudWatch {operation} must answer 200, got {status} {text}"
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}
