// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API whose requests are decided by a Lambda AUTHORIZER that WITHHOLDS some
//! of the fields they select, and whose fields are resolved by APPSYNC_JS code over a `NONE`
//! data source — end to end over the real `MultiServiceRouter`, with AppSync, Lambda, IAM and
//! CloudFormation mounted on one engine.
//!
//! RULE — for a call the `AWS_LAMBDA` mode ADMITTED, the fields the authorizer's `deniedFields`
//! names are the only fields of that call that do not resolve: each of them is `null` under its
//! own response key and is reported as exactly one error of its own path, classed `Unauthorized`
//! and worded "Not Authorized to access <field> on type <Type>", while every OTHER field the
//! call selects carries the value its resolver produced — for an APPSYNC_JS resolver, the value
//! its request handler ended its step with through `runtime.earlyReturn(value)`, whether that
//! value is a constant of the code or something read off what the authorizer published
//! (`resolverContext`, which a resolver reads as `ctx.identity.resolverContext`). So an empty
//! `deniedFields` resolves every field and the answer carries no `errors` member at all, and a
//! denial withholds the named field and nothing besides. An entry names its field either by the
//! API's own `…/types/<TypeName>/fields/<fieldName>` ARN or by the short `<TypeName>.<fieldName>`
//! form, and both withhold the same field.
//!
//! Two services have to agree for the rule to hold — Lambda runs the authorizer and AppSync
//! applies what it decided, runs the code and takes the value the code ended with — so this is
//! the suite that can pin it. The sibling suite
//! [`crate::appsync_js_early_return_carries_the_authorizer_context`] spans the same resolvers
//! under an authorizer that withholds nothing; what these rows add is the DENIAL beside them.
//!
//! The rows span what the rule generalises over: HOW MANY fields the decision withholds (none,
//! one of three, two of three), WHICH spelling the entry names the field by (the short form and
//! the field ARN), and WHAT the surviving resolvers ended their step with (a constant of the code
//! and a value read off the published context).

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appsync_lambda_data_source_and_authorizer::{
    appsync_ok, build_router, create_function, post_published_uri,
};

/// The Region the ARN form of a denied field names, which is written into the authorizer's own
/// source below (a handler has no other way to word the ARN of the API that invoked it).
const REGION: &str = "us-east-1";

/// The placeholder that Region is written in for.
const REGION_PLACEHOLDER: &str = "$REGION";

/// The path a GraphQL API's `GRAPHQL` endpoint serves.
const GRAPHQL_PATH: &str = "/graphql";

/// The `runtime` a resolver defined by `code` names. Source: `AppSyncRuntime`'s own members
/// (`smithy_query(query="shape",shape="AppSyncRuntime,RuntimeName")`).
const APPSYNC_JS: (&str, &str) = ("APPSYNC_JS", "1.0.0");

/// The data source every resolver here resolves against: `NONE` is "there is no data source", so
/// nothing but the resolver's own code can decide the field.
const LOCAL_DATA_SOURCE: &str = "local";

/// The values the two constant-returning resolvers end their step with. Distinct, so a row that
/// read one field's value under the other's response key would be seen.
const OPEN_VALUE: &str = "the value the open field was given";
const SECRET_VALUE: &str = "the value the secret field was given";

/// The class AppSync reports a field the caller may not access under, and the fixed part of the
/// sentence it states.
const DENIED_ERROR_TYPE: &str = "Unauthorized";
const DENIED_MESSAGE_PREFIX: &str = "Not Authorized to access";

/// How long a poll loop waits for a state another service owes, and how often it looks again.
/// Generous: what is under test is what the endpoint answers, never how fast the host reaches a
/// transition.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The schema every API of this suite is created with: two fields resolved from a constant of
/// their own code, and one resolved from what the authorizer published.
const SCHEMA: &str = "type Query {\n  open: String\n  secret: String\n  whoami: String\n}\n";

/// The authorizer every API of this suite is gated by.
///
/// It authorizes every call, publishes the token it read, and withholds the fields the token asks
/// it to — naming them in the spelling the token asks for, the ARN form built from the API and
/// the account the request context reports. Its decision is reusable for no later request
/// (`ttlOverride` of zero "disables caching"), so each row's own denial is the one in force.
const AUTHORIZER_HANDLER: &str = r#"
def handler(event, context):
    token = event.get("authorizationToken") or ""
    request_context = event.get("requestContext") or {}

    def field_arn(field):
        return (
            "arn:aws:appsync:$REGION:"
            + str(request_context.get("accountId"))
            + ":apis/"
            + str(request_context.get("apiId"))
            + "/types/Query/fields/"
            + field
        )

    denied = []
    if token.endswith("-short"):
        denied = ["Query.secret"]
    elif token.endswith("-arn"):
        denied = [field_arn("secret")]
    elif token.endswith("-both"):
        denied = ["Query.secret", field_arn("open")]
    return {
        "isAuthorized": True,
        "deniedFields": denied,
        "resolverContext": {"token": token},
        "ttlOverride": 0,
    }
"#;

/// That handler with the Region written in.
fn authorizer_handler() -> String {
    AUTHORIZER_HANDLER.replace(REGION_PLACEHOLDER, REGION)
}

/// The code of one resolver whose request handler ends its own step with `ended` and whose
/// response handler passes the step's result on — so the field's value is what the request
/// handler ended with.
fn ending_code(ended: &str) -> String {
    format!(
        "import {{ runtime }} from '@aws-appsync/utils';\n\
         export function request(ctx) {{ runtime.earlyReturn({ended}); }}\n\
         export function response(ctx) {{ return ctx.result; }}\n"
    )
}

/// Create one `AWS_LAMBDA` API gated by `function_arn`, install [`SCHEMA`], add the `NONE` data
/// source and attach one APPSYNC_JS resolver per field, and answer the URL the API published for
/// its GraphQL endpoint — which is what a client POSTs to.
async fn create_api(router: &MultiServiceRouter, name: &str, function_arn: &str) -> String {
    let created = appsync_ok(
        router,
        "POST",
        "/v1/apis",
        Some(json!({
            "name": name,
            "authenticationType": "AWS_LAMBDA",
            "lambdaAuthorizerConfig": {"authorizerUri": function_arn},
        })),
    )
    .await;
    let api = &created["graphqlApi"];
    let api_id = api["apiId"].as_str().unwrap_or_default().to_string();
    let graphql_uri = api["uris"]["GRAPHQL"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        graphql_uri.ends_with(GRAPHQL_PATH) && graphql_uri.contains("127.0.0.1:"),
        "setup: uris.GRAPHQL must be a URL this emulator serves, got {graphql_uri:?}"
    );
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/schemacreation"),
        Some(json!({"definition": ::aws_smithy_types::base64::encode(SCHEMA)})),
    )
    .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let settled = appsync_ok(
            router,
            "GET",
            &format!("/v1/apis/{api_id}/schemacreation"),
            None,
        )
        .await;
        if settled["status"].as_str() == Some("SUCCESS") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the schema creation must be performed, last read {settled}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({"name": LOCAL_DATA_SOURCE, "type": "NONE"})),
    )
    .await;
    let resolvers: [(&str, String); 3] = [
        ("open", ending_code(&format!("'{OPEN_VALUE}'"))),
        ("secret", ending_code(&format!("'{SECRET_VALUE}'"))),
        ("whoami", ending_code("ctx.identity.resolverContext.token")),
    ];
    for (field, code) in &resolvers {
        appsync_ok(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/types/Query/resolvers"),
            Some(json!({
                "fieldName": field,
                "kind": "UNIT",
                "dataSourceName": LOCAL_DATA_SOURCE,
                "code": code,
                "runtime": {"name": APPSYNC_JS.0, "runtimeVersion": APPSYNC_JS.1},
            })),
        )
        .await;
    }
    graphql_uri
}

/// The `errors` a result reports, as the `(path, errorType, message)` each entry states, in the
/// order the paths sort — which is what lets a row with two of them state what it expects without
/// pinning the order the entries were appended in.
fn reported_errors(answered: &Value) -> Vec<(String, String, String)> {
    let mut reported: Vec<(String, String, String)> = answered["errors"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry["path"][0].as_str().unwrap_or_default().to_string(),
                        entry["errorType"].as_str().unwrap_or_default().to_string(),
                        entry["message"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    reported.sort();
    reported
}

/// The error a field the decision withheld is reported as.
fn denied_error(field: &str) -> (String, String, String) {
    (
        field.to_string(),
        DENIED_ERROR_TYPE.to_string(),
        format!("{DENIED_MESSAGE_PREFIX} {field} on type Query"),
    )
}

/// RULE: the fields `deniedFields` names are the only ones that do not resolve — each null under
/// its own key and reported as one `Unauthorized` error of its own path — while every other field
/// carries the value its APPSYNC_JS resolver ended its step with.
#[tokio::test]
async fn denied_fields_null_only_themselves_while_early_returning_siblings_resolve() {
    let router = build_router();
    let authorizer =
        create_function(&router, "denied-fields-authorizer", &authorizer_handler()).await;
    let graphql_uri = create_api(&router, "denied-fields-early-return", &authorizer).await;

    /// One row: the token the request presents, and which fields the decision it takes withholds.
    struct Row {
        case: &'static str,
        token: &'static str,
        denied: &'static [&'static str],
    }
    // The rows differ along how many fields the decision withholds and which spelling it names
    // them by; the token itself is what the authorizer reads the row's answer off.
    let rows: [Row; 4] = [
        Row {
            case: "a decision that withholds nothing",
            token: "allow-none",
            denied: &[],
        },
        Row {
            case: "one field withheld by the short TypeName.fieldName spelling",
            token: "allow-short",
            denied: &["secret"],
        },
        Row {
            case: "the same field withheld by its own field ARN",
            token: "allow-arn",
            denied: &["secret"],
        },
        Row {
            case: "two fields withheld, one in each spelling",
            token: "allow-both",
            denied: &["open", "secret"],
        },
    ];

    let document = "query { open secret whoami }";
    for row in &rows {
        let case = row.case;
        let (status, answered) = post_published_uri(
            case,
            &graphql_uri,
            &json!({"query": document}),
            &[("authorization", row.token.to_string())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: an admitted document is answered 200, got {status} {answered}"
        );
        // Every field the decision did not withhold carries what its resolver ended its step
        // with — the constants of the two coded fields, and the token the authorizer published
        // for the third — and every field it did withhold is null.
        let value = |field: &str, resolved: &str| match row.denied.contains(&field) {
            true => Value::Null,
            false => json!(resolved),
        };
        let expected = json!({
            "open": value("open", OPEN_VALUE),
            "secret": value("secret", SECRET_VALUE),
            "whoami": value("whoami", row.token),
        });
        assert_eq!(
            answered["data"], expected,
            "case {case:?}: expected the data {expected}, got {answered}"
        );
        // One error per withheld field and none besides, so a call that withheld nothing is
        // answered with no `errors` member at all.
        let expected_errors: Vec<(String, String, String)> =
            row.denied.iter().map(|field| denied_error(field)).collect();
        if expected_errors.is_empty() {
            assert!(
                answered.get("errors").is_none(),
                "case {case:?}: expected no errors member, got {answered}"
            );
        }
        let reported = reported_errors(&answered);
        assert_eq!(
            reported, expected_errors,
            "case {case:?}: expected the errors {expected_errors:?}, got {reported:?} in \
             {answered}"
        );
    }
}
