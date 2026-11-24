// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API whose fields are resolved by an `HTTP` data source pointed at a
//! DEPLOYED API Gateway stage, driven against the emulator a customer STARTS: the built binary,
//! on its own port, with request authentication ON, every call made over TLS, and the GraphQL
//! documents POSTed to the address the API itself PUBLISHED.
//!
//! The rule under test: **the address a GraphQL API publishes for its data plane
//! (`GraphqlApi$uris`, keyed `GRAPHQL`) is a socket this emulator ANSWERS a POSTed GraphQL
//! document on, and a field whose `HTTP` data source configures `authorizationConfig`
//! `AWS_IAM` has AppSync's own outbound call SigV4-signed for the service and the Region
//! `awsIamConfig` names — so an endpoint method that requires that signature admits the call
//! and resolves the field, while the same method refuses the call of a data source that
//! configures no authorization, and the field reports that refusal as the status it resolved
//! to.** Every row is ANSWERED: a published address that blocks is as unusable as one nothing
//! serves, so each POST is sent with a generous deadline and a dial that does not answer is a
//! failure this suite reports.
//!
//! The sibling suite `appsync_http_data_source_sigv4` spans what the HTTP data source
//! generalises over — the verbs, what the request carries, the paths, both definition forms and
//! the refusals of a request that cannot be made — on a router the test assembles in its own
//! process, with request authentication OFF, so what it reads off the stage's echo is that the
//! call ARRIVED signed and the credential scope it carries. The emulator a client runs is a
//! different configuration of the same code: the signature of every request is VERIFIED, so the
//! guarded method admits the signed egress only if that signature holds up, and the whole chain
//! crosses two services and the API's own published listener in one process. This suite is the
//! one that pins it THERE. ONE start is paid for, since starting the binary costs what starting
//! the emulator costs, and the rows that span the rule are driven on that single estate.
//!
//! The rows differ along what the rule generalises over: the authorization the data source
//! configures (`AWS_IAM`, none), the authorization the method requires (`AWS_IAM`, `NONE`), the
//! verb the request states (`GET`, `POST`) and what it carries (a query parameter, a header, a
//! body).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::{Started, REGION};
use crate::query_wire::element;

/// The stage the REST API is deployed to.
const STAGE: &str = "live";

/// The signing name an invoke of a deployed stage is signed under: the `execute-api` service
/// AWS publishes the invoke endpoint and the `execute-api:Invoke` action under, which is not
/// the control plane's own name.
const INVOKE_SIGNING_NAME: &str = "execute-api";

/// The three resources of the stage: a `GET` open to everyone, a `GET` behind `AWS_IAM`, and a
/// `POST` behind `AWS_IAM` the request's own body reaches.
const OPEN_PATH: &str = "open";
const GUARDED_PATH: &str = "guarded";
const ORDERS_PATH: &str = "orders";

/// The two data sources the API holds: the IAM-authorized endpoint, and the same endpoint with
/// no authorization configured at all.
const SIGNED_SOURCE: &str = "signed";
const PLAIN_SOURCE: &str = "plain";

/// The role the IAM-authorized data source makes its calls under, and the service principal
/// that assumes it.
const ROLE: &str = "appsync-http-signed-egress";
const APPSYNC_PRINCIPAL: &str = "appsync.amazonaws.com";

/// The query parameter, the header and the body a request carries, and the values this suite
/// sends under them — what the stage echoes back, so a row reads whether they travelled.
const QUERY_NAME: &str = "tenant";
const QUERY_VALUE: &str = "acme and co";
const HEADER_NAME: &str = "x-echo";
const HEADER_VALUE: &str = "from-appsync";
const SENT_BODY: &str = r#"{"id":"A-1"}"#;

/// The HTTP field a SigV4 signature travels in, which the stage echoes back so a row reads the
/// SCOPE the signature was computed for off the request that arrived.
const AUTHORIZATION_FIELD: &str = "Authorization";

/// What a SigV4 signature opens with: the scheme, followed by the credential scope naming the
/// service and the Region the signature was computed for, which is what admits the call at a
/// method that requires one.
const SIGV4_SCHEME: &str = "AWS4-HMAC-SHA256";

/// The sentence API Gateway refuses a request it can place no credential for with.
const MISSING_TOKEN: &str = "Missing Authentication Token";

/// How long a wait on a CONDITION may take, and how often this suite looks again. Generous:
/// what is under test is what the published endpoint answers, never how fast the host reaches a
/// transition.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// The schema the API is created with: one field per row, each resolving to the answer the
/// stage gave.
const SCHEMA: &str = "\
type Answer {\n  statusCode: Int\n  body: String\n}\n\
\n\
type Query {\n  \
  openViaUnsigned: Answer\n  \
  openViaSigned: Answer\n  \
  guardedViaSigned: Answer\n  \
  guardedViaUnsigned: Answer\n  \
  postedViaSigned: Answer\n}\n";

/// The response half every resolver carries: the step's own failure is re-raised where there is
/// one, and otherwise the field takes the status and the body the endpoint answered — whatever
/// that status is.
const RESPONSE_TEMPLATE: &str = "#if($ctx.error)$util.error($ctx.error.message, \
                                 $ctx.error.type)#end\
                                 {\"statusCode\": $ctx.result.statusCode, \
                                 \"body\": $util.toJson($ctx.result.body)}";

/// One row: the field selected through the published endpoint, the status the stage answers it
/// with, whether the call ARRIVED signed for the configured service and Region, and whether the
/// stage echoes the account its authorization authenticated — which only a method requiring one
/// has a value for.
struct Row<'a> {
    case: &'a str,
    field: &'a str,
    status: u64,
    verb: &'a str,
    path: &'a str,
    signed: bool,
    authenticated: bool,
}

#[test]
fn an_iam_authorized_http_data_source_signs_its_call_to_a_deployed_stage_on_a_started_emulator() {
    let emulator = Started::start();
    let endpoint = deployed_stage(&emulator);
    let role_arn = create_role(&emulator);
    let api = create_api(&emulator, &endpoint, &role_arn);

    // The address the API published for its data plane is one THIS emulator serves: a host
    // belonging to another server is unreachable from a client's process, and so is a URI that
    // is not the endpoint's own `dns` record.
    let case = "the published GraphQL address";
    let uri = api.api["uris"]["GRAPHQL"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the api must publish its GRAPHQL endpoint"));
    assert!(
        !uri.contains("amazonaws.com") && uri.contains("127.0.0.1:"),
        "case {case:?}: expected an address this emulator serves, got {uri:?}"
    );

    // And a later read of the API publishes the same addresses, so a client that reads the
    // record back dials what the create announced.
    let fetched = appsync(
        &emulator,
        "GetGraphqlApi",
        "GET",
        &format!("/v1/apis/{}", api.api_id),
        &Value::Null,
    );
    assert_eq!(
        fetched["graphqlApi"]["uris"], api.api["uris"],
        "case {case:?}: GetGraphqlApi must publish the addresses the create announced, got {}",
        fetched["graphqlApi"]
    );

    let guarded = format!("/{GUARDED_PATH}");
    let open = format!("/{OPEN_PATH}");
    let orders = format!("/{ORDERS_PATH}");
    let rows: &[Row<'_>] = &[
        Row {
            case: "a data source configuring no authorization, on a method requiring none",
            field: "openViaUnsigned",
            status: 200,
            verb: "GET",
            path: &open,
            signed: false,
            authenticated: false,
        },
        Row {
            case: "an AWS_IAM data source on that same open method",
            field: "openViaSigned",
            status: 200,
            verb: "GET",
            path: &open,
            signed: true,
            authenticated: false,
        },
        Row {
            case: "an AWS_IAM data source on an AWS_IAM-guarded method",
            field: "guardedViaSigned",
            status: 200,
            verb: "GET",
            path: &guarded,
            signed: true,
            authenticated: true,
        },
        Row {
            case: "an AWS_IAM data source POSTing a body to a guarded method",
            field: "postedViaSigned",
            status: 200,
            verb: "POST",
            path: &orders,
            signed: true,
            authenticated: true,
        },
    ];

    for row in rows {
        let (value, errors) = selected(&emulator, row.case, &api, row.field);
        assert!(
            errors.is_empty(),
            "case {:?}: expected the field to resolve, got errors {errors:?}; the emulator's \
             log:\n{}",
            row.case,
            emulator.log()
        );
        assert_eq!(
            value["statusCode"].as_u64(),
            Some(row.status),
            "case {:?}: expected the status the endpoint answered, got {value}",
            row.case
        );
        let echo = echoed(row.case, &value);
        assert_eq!(
            (echo["method"].as_str(), echo["path"].as_str()),
            (Some(row.verb), Some(row.path)),
            "case {:?}: expected the verb and the path the request stated, got {:?} and {:?}",
            row.case,
            echo["method"],
            echo["path"]
        );
        // WHETHER the call arrived signed, and FOR WHAT: the credential scope names the service
        // and the Region the data source's `awsIamConfig` configures, so a call signed for
        // anything else is a different value here rather than merely a present one.
        let scope = echo["scope"].as_str().unwrap_or_default().to_string();
        let scoped = format!("/{REGION}/{INVOKE_SIGNING_NAME}/aws4_request");
        assert_eq!(
            scope.starts_with(SIGV4_SCHEME) && scope.contains(&scoped),
            row.signed,
            "case {:?}: expected a request signed={} for {scoped}, got a scope of {} characters",
            row.case,
            row.signed,
            scope.len()
        );
        // And the method that REQUIRES a signature authenticated an account off it, which is
        // what a verified signature buys and what an unsigned call has no value for.
        let account = echo["account"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            !account.is_empty(),
            row.authenticated,
            "case {:?}: expected an echoed account={}, got account {account:?}",
            row.case,
            row.authenticated
        );
    }

    // What the request CARRIED reached the endpoint: the query parameter and the header of the
    // `GET` rows, and the body of the `POST` row.
    let case = "the parameters a signed GET carried";
    let (value, _) = selected(&emulator, case, &api, "guardedViaSigned");
    let echo = echoed(case, &value);
    for (member, expected) in [(QUERY_NAME, QUERY_VALUE), (HEADER_NAME, HEADER_VALUE)] {
        assert_eq!(
            echo[member].as_str(),
            Some(expected),
            "case {case:?}: expected {member} to reach the endpoint as {expected:?}, got {:?}",
            echo[member]
        );
    }
    let case = "the body a signed POST carried";
    let (value, _) = selected(&emulator, case, &api, "postedViaSigned");
    let echo = echoed(case, &value);
    assert_eq!(
        echo["sent"],
        serde_json::from_str::<Value>(SENT_BODY).expect("the sent body is a document"),
        "case {case:?}: expected the body the request carried, got {:?}",
        echo["sent"]
    );

    // The difference the configuration buys, on the very same method: the unsigned call is
    // REFUSED by the endpoint itself, and the field reports that refusal as the status it
    // resolved to rather than as a failure of the step.
    let case = "a data source configuring no authorization, on an AWS_IAM-guarded method";
    let (refused, errors) = selected(&emulator, case, &api, "guardedViaUnsigned");
    assert!(
        errors.is_empty(),
        "case {case:?}: expected the field to resolve, got errors {errors:?}"
    );
    assert_eq!(
        refused["statusCode"].as_u64(),
        Some(403),
        "case {case:?}: expected the endpoint's own refusal, got {refused}"
    );
    let stated = echoed(case, &refused);
    assert_eq!(
        stated["message"].as_str(),
        Some(MISSING_TOKEN),
        "case {case:?}: expected the endpoint's own sentence, got {stated}"
    );
}

/// One created GraphQL API: how it is addressed, the key a client presents, and the record that
/// publishes its addresses.
struct Api {
    api_id: String,
    api_key: String,
    api: Value,
}

/// Select `field` by POSTing one GraphQL document to the URL the API PUBLISHED, as a client
/// that was handed that URL does, and answer the field's value and the errors the result
/// carries. The dial is given a generous deadline: an address that answers nothing inside it is
/// an address no client can use, which is the failure this suite exists to report.
fn selected(
    emulator: &Started,
    case: &str,
    api: &Api,
    field: &str,
) -> (Value, Vec<(String, String)>) {
    let uri = api.api["uris"]["GRAPHQL"].as_str().unwrap_or_default();
    let document = json!({ "query": format!("{{ {field} {{ statusCode body }} }}") });
    let response = emulator
        .client()
        .post(uri)
        .header("content-type", "application/json")
        .header("x-api-key", &api.api_key)
        .body(document.to_string())
        .send()
        .unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the published address {uri} must answer, got {error}; the \
                 emulator's log:\n{}",
                emulator.log()
            )
        });
    let status = response.status().as_u16();
    let body = response.text().unwrap_or_default();
    assert_eq!(
        status, 200,
        "case {case:?}: the published endpoint must serve the document, got {status} and {body}"
    );
    let answered: Value = serde_json::from_str(&body)
        .unwrap_or_else(|_| panic!("case {case:?}: expected a GraphQL answer, got {body:?}"));
    let errors = answered["errors"]
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
        .unwrap_or_default();
    (answered["data"][field].clone(), errors)
}

/// The document the stage echoed, parsed off the `body` the field reports.
fn echoed(case: &str, value: &Value) -> Value {
    let body = value["body"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the field must report a body, got {value}"));
    serde_json::from_str(body)
        .unwrap_or_else(|_| panic!("case {case:?}: expected the endpoint's document, got {body:?}"))
}

/// One API Gateway control-plane call, asserting it answered `expected`.
fn apigateway(
    emulator: &Started,
    case: &str,
    method: &str,
    path: &str,
    body: &Value,
    expected: u16,
) -> Value {
    let (status, answered) = emulator.call(
        "apigateway",
        method,
        path,
        &[("content-type", "application/json")],
        body.to_string().into_bytes(),
    );
    assert_eq!(
        status, expected,
        "setup: {case} (apigateway {method} {path}) must answer {expected}, got {status} and \
         {answered}"
    );
    serde_json::from_str(&answered).unwrap_or(Value::Null)
}

/// One AppSync control-plane call, asserting it was served.
fn appsync(emulator: &Started, case: &str, method: &str, path: &str, body: &Value) -> Value {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    let (status, answered) = emulator.call(
        "appsync",
        method,
        path,
        &[("content-type", "application/json")],
        payload,
    );
    assert_eq!(
        status, 200,
        "setup: {case} (appsync {method} {path}) must answer 200, got {status} and {answered}"
    );
    serde_json::from_str(&answered).unwrap_or(Value::Null)
}

/// A required string member of a response body.
fn member(what: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what} must carry a non-empty {name:?}, got {body}"))
}

/// The integration response template a `GET` method of the stage echoes its own method request
/// with: the verb and the resource it reached, the query parameter and the header the caller
/// sent, and the `Authorization` field a SigV4 signature travels in — which names the service
/// and the Region the signature was computed for, so a row reads whether the call arrived
/// signed and for what. A request that arrived unsigned presents no such field at all.
///
/// A method whose `authorizationType` is `AWS_IAM` also echoes the account its authorization
/// authenticated for the call (`$context.identity.accountId`, "the AWS account ID associated
/// with the request"), which API Gateway resolves off the credential the request signed with. A
/// method that authenticates nobody has no such value, and a template referencing one the call
/// has no value for is a template the api cannot render, so the open method's template leaves
/// the member out.
fn echo_template(account: bool) -> String {
    let authenticated = match account {
        true => "\n\"account\":\"$context.identity.accountId\",",
        false => "",
    };
    format!(
        r#"{{"method":"$context.httpMethod","path":"$context.resourcePath",{authenticated}
"scope":"$input.params('{AUTHORIZATION_FIELD}')",
"{QUERY_NAME}":"$input.params('{QUERY_NAME}')","{HEADER_NAME}":"$input.params('{HEADER_NAME}')"}}"#
    )
}

/// The same echo for the `POST` method, which also carries the BODY the caller sent: a `MOCK`
/// integration answers its own rendered request, so the request template carries the body
/// through and the response template reads it back off that answer.
fn posted_templates() -> (String, String) {
    (
        r#"{"statusCode":200,"sent":$input.json('$')}"#.to_string(),
        r#"{"method":"$context.httpMethod","path":"$context.resourcePath",
"account":"$context.identity.accountId","scope":"$input.params('Authorization')",
"sent":$input.json('$.sent')}"#
            .to_string(),
    )
}

/// One `MOCK` method of the stage: the resource it sits on, the verb it serves, the
/// authorization in front of it, and the templates either side of its integration.
struct MockMethod<'a> {
    path_part: &'a str,
    verb: &'a str,
    authorization_type: &'a str,
    request_template: &'a str,
    response_template: &'a str,
}

/// Put one `MOCK` method on its own resource under `root`.
fn mock_method(emulator: &Started, api: &str, root: &str, method: &MockMethod<'_>) {
    let case = format!("the {} method on /{}", method.verb, method.path_part);
    let resource = apigateway(
        emulator,
        &case,
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        &json!({ "pathPart": method.path_part }),
        201,
    );
    let resource_id = member("CreateResource", &resource, "id");
    let base = format!(
        "/restapis/{api}/resources/{resource_id}/methods/{}",
        method.verb
    );
    apigateway(
        emulator,
        &case,
        "PUT",
        &base,
        &json!({ "authorizationType": method.authorization_type }),
        201,
    );
    apigateway(
        emulator,
        &case,
        "PUT",
        &format!("{base}/integration"),
        &json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": method.request_template },
        }),
        201,
    );
    apigateway(
        emulator,
        &case,
        "PUT",
        &format!("{base}/responses/200"),
        &json!({}),
        201,
    );
    apigateway(
        emulator,
        &case,
        "PUT",
        &format!("{base}/integration/responses/200"),
        &json!({
            "statusCode": "200",
            "responseTemplates": { "application/json": method.response_template },
        }),
        201,
    );
}

/// Stand the REST API up — the open `GET`, the guarded `GET` and the guarded `POST`, each
/// behind a `MOCK` integration that echoes the method request — deploy it to a stage, and
/// answer the URL that stage is invoked at, which is the endpoint the HTTP data sources are
/// configured with.
fn deployed_stage(emulator: &Started) -> String {
    let created = apigateway(
        emulator,
        "the rest api",
        "POST",
        "/restapis",
        &json!({ "name": "appsync-signed-egress", "endpointConfiguration": {"types": ["REGIONAL"]} }),
        201,
    );
    let api_id = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");
    let open_echo = echo_template(false);
    let guarded_echo = echo_template(true);
    let (posted_request, posted_response) = posted_templates();
    let methods = [
        MockMethod {
            path_part: OPEN_PATH,
            verb: "GET",
            authorization_type: "NONE",
            request_template: r#"{"statusCode":200}"#,
            response_template: &open_echo,
        },
        MockMethod {
            path_part: GUARDED_PATH,
            verb: "GET",
            authorization_type: "AWS_IAM",
            request_template: r#"{"statusCode":200}"#,
            response_template: &guarded_echo,
        },
        MockMethod {
            path_part: ORDERS_PATH,
            verb: "POST",
            authorization_type: "AWS_IAM",
            request_template: &posted_request,
            response_template: &posted_response,
        },
    ];
    for method in &methods {
        mock_method(emulator, &api_id, &root, method);
    }
    apigateway(
        emulator,
        "the deployment",
        "POST",
        &format!("/restapis/{api_id}/deployments"),
        &json!({ "stageName": STAGE }),
        201,
    );
    format!("https://{api_id}.execute-api.{REGION}.amazonaws.com/{STAGE}")
}

/// Create the role the IAM-authorized data source names: trusted by AppSync and allowed to
/// invoke a deployed stage, which is what an `AWS_IAM`-authorized HTTP endpoint's call
/// performs.
fn create_role(emulator: &Started) -> String {
    let trust = format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{APPSYNC_PRINCIPAL}\"}},\
         \"Action\":\"sts:AssumeRole\"}}]}}"
    );
    let (status, created) = emulator.form_call(
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", ROLE), ("AssumeRolePolicyDocument", &trust)],
    );
    assert_eq!(
        status, 200,
        "setup: CreateRole must answer 200, got {status} and {created}"
    );
    let role_arn = element(&created, "Arn");
    let policy = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
                  \"Action\":\"execute-api:Invoke\",\"Resource\":\"*\"}]}";
    let (status, answered) = emulator.form_call(
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", "invoke-the-stage"),
            ("PolicyDocument", policy),
        ],
    );
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy must answer 200, got {status} and {answered}"
    );
    role_arn
}

/// The request half of one field, as the HTTP data source's own request object: the verb, the
/// path under the configured endpoint, and the parameters it carries.
fn request_template(verb: &str, resource_path: &str, body: bool) -> String {
    let params = match body {
        false => format!(
            r#", "params": {{"query": {{"{QUERY_NAME}": "{QUERY_VALUE}"}},
"headers": {{"{HEADER_NAME}": "{HEADER_VALUE}"}}}}"#
        ),
        true => format!(
            r#", "params": {{"headers": {{"content-type": "application/json"}},
"body": {}}}"#,
            json!(SENT_BODY)
        ),
    };
    format!(
        r#"{{"version": "2018-05-29", "method": "{verb}", "resourcePath": "{resource_path}"{params}}}"#
    )
}

/// Create the GraphQL API with an API key, install [`SCHEMA`], attach the two HTTP data sources
/// at `endpoint` and one UNIT resolver per field.
fn create_api(emulator: &Started, endpoint: &str, role_arn: &str) -> Api {
    let created = appsync(
        emulator,
        "CreateGraphqlApi",
        "POST",
        "/v1/apis",
        &json!({"name": "signed-egress", "authenticationType": "API_KEY"}),
    );
    let api = created["graphqlApi"].clone();
    let api_id = member("CreateGraphqlApi", &api, "apiId");
    let key = appsync(
        emulator,
        "CreateApiKey",
        "POST",
        &format!("/v1/apis/{api_id}/apikeys"),
        &json!({"description": "the suite's key"}),
    );
    let api_key = member("CreateApiKey", &key["apiKey"], "id");

    appsync(
        emulator,
        "StartSchemaCreation",
        "POST",
        &format!("/v1/apis/{api_id}/schemacreation"),
        &json!({ "definition": ::aws_smithy_types::base64::encode(SCHEMA) }),
    );
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let settled = appsync(
            emulator,
            "GetSchemaCreationStatus",
            "GET",
            &format!("/v1/apis/{api_id}/schemacreation"),
            &Value::Null,
        );
        if settled["status"].as_str() == Some("SUCCESS") {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: the schema creation must be performed, last read {settled}"
        );
        std::thread::sleep(LOOK_INTERVAL);
    }

    // The two data sources: the endpoint AppSync signs for, and the same endpoint with no
    // authorization configured. "The system assumes this role when accessing the data source",
    // so the IAM-authorized one names the role; the plain one needs none.
    appsync(
        emulator,
        "CreateDataSource(signed)",
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        &json!({
            "name": SIGNED_SOURCE,
            "type": "HTTP",
            "serviceRoleArn": role_arn,
            "httpConfig": {
                "endpoint": endpoint,
                "authorizationConfig": {
                    "authorizationType": "AWS_IAM",
                    "awsIamConfig": {
                        "signingRegion": REGION,
                        "signingServiceName": INVOKE_SIGNING_NAME,
                    },
                },
            },
        }),
    );
    appsync(
        emulator,
        "CreateDataSource(plain)",
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        &json!({
            "name": PLAIN_SOURCE,
            "type": "HTTP",
            "httpConfig": { "endpoint": endpoint },
        }),
    );

    let resolvers: &[(&str, &str, String)] = &[
        (
            "openViaUnsigned",
            PLAIN_SOURCE,
            request_template("GET", &format!("/{OPEN_PATH}"), false),
        ),
        (
            "openViaSigned",
            SIGNED_SOURCE,
            request_template("GET", &format!("/{OPEN_PATH}"), false),
        ),
        (
            "guardedViaSigned",
            SIGNED_SOURCE,
            request_template("GET", &format!("/{GUARDED_PATH}"), false),
        ),
        (
            "guardedViaUnsigned",
            PLAIN_SOURCE,
            request_template("GET", &format!("/{GUARDED_PATH}"), false),
        ),
        (
            "postedViaSigned",
            SIGNED_SOURCE,
            request_template("POST", &format!("/{ORDERS_PATH}"), true),
        ),
    ];
    for (field, data_source, request) in resolvers {
        appsync(
            emulator,
            "CreateResolver",
            "POST",
            &format!("/v1/apis/{api_id}/types/Query/resolvers"),
            &json!({
                "fieldName": field,
                "kind": "UNIT",
                "dataSourceName": data_source,
                "requestMappingTemplate": request,
                "responseMappingTemplate": RESPONSE_TEMPLATE,
            }),
        );
    }
    Api {
        api_id,
        api_key,
        api,
    }
}
