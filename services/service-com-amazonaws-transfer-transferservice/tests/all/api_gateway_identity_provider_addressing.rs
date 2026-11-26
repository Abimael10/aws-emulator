// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ONE precondition an `API_GATEWAY` identity provider is
//! verified against, over every addressing a deployed API Gateway stage is reached at.
//!
//! RULE under test: the invocation-role verification resolves the endpoint the provider's
//! `Url` names into the `execute-api` resource an invoke of it is authorized against —
//! `arn:aws:execute-api:<region>:<account>:<api-id>/<stage>/<HTTP-METHOD>/<resource-path>`,
//! the four-part form AWS publishes for that plane — from EITHER addressing a deployed
//! stage is published at:
//!
//! * `https://<api-id>.execute-api.<region>.amazonaws.com/<stage>/…`, which carries the
//!   api id and the Region in the HOST; and
//! * `https://<endpoint>/_aws/execute-api/<api-id>/<stage>/…`, which carries them in the
//!   PATH behind the reserved namespace segment, because a caller that reaches one
//!   endpoint for every service and every plane cannot carry them in the host. That is
//!   the invoke URL this emulator's own API Gateway emulation publishes.
//!
//! So a role scoped to exactly the API the `Url` names verifies, whichever way the URL
//! names it, and one scoped to another API — or to another Region — does not, with the
//! modelled `InvalidRequestException` (400) and no server persisted. Because the resource
//! the decision is taken on is the CONCRETE request this service would make to that
//! endpoint, every grant whose wildcards cover that request verifies the role — the whole
//! API, every stage/method/path of it, every method and path of one stage, or the very
//! method and path — which is the second rule asserted below.
//!
//! `UpdateServer` reads the grant against the same invoke resource, so the forms that verify
//! a create verify an update, and the ones that do not leave the stored provider alone.
//!
//! The last section drives the INVOCATION itself, with the API Gateway emulation mounted
//! beside this service: a server whose `Url` names a deployed stage that emulation serves has
//! the authentication request really made against it, and the answer reports that
//! invocation's own HTTP status and response body.
//!
//! Every request goes over the real wire path (`POST /` with
//! `X-Amz-Target: TransferService.<Op>`) through the shared [`crate::harness`].

use serde_json::{json, Value};

use crate::harness::{member_of, Emulator, ACCOUNT, REGION};

/// The api id of the API each row's provider names: API Gateway's own ten-character
/// lowercase alphanumeric id shape.
const API_ID: &str = "abc1230000";

/// Another API's id, for the rows whose role is scoped away from the one the URL names.
const OTHER_API_ID: &str = "zzz9990000";

/// A Region that is NOT the one the request is signed for ([`REGION`]), so a row can
/// show which Region each addressing reads the API as being in.
const OTHER_REGION: &str = "eu-west-1";

/// A policy allowing `execute-api:Invoke` on exactly `resource`.
fn allow_invoke(resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[
            {{"Effect":"Allow","Action":"execute-api:Invoke","Resource":"{resource}"}}]}}"#
    )
}

/// `arn:aws:execute-api:<region>:<account>:<resource>` — how an IAM policy names an API
/// Gateway invoke resource.
fn execute_api(region: &str, resource: &str) -> String {
    format!("arn:aws:execute-api:{region}:{ACCOUNT}:{resource}")
}

/// One row: the case, the provider `Url`, the `execute-api` resource the seeded role is
/// allowed to invoke, and whether the server is created.
struct Row {
    case: &'static str,
    url: String,
    allowed: String,
    accepted: bool,
}

/// The rows: every addressing crossed with how the role's statement is scoped.
fn rows() -> Vec<Row> {
    // The emulator's own endpoint, as a client on this machine reaches it: a loopback
    // address with the data port. The host is a per-run value in a recording, so each row
    // spells its own rather than deriving one.
    let host_form = format!("https://{API_ID}.execute-api.{REGION}.amazonaws.com/prod");
    vec![
        Row {
            case: "the host form, with the role scoped to that API's stage",
            url: host_form.clone(),
            allowed: execute_api(REGION, &format!("{API_ID}/prod/*")),
            accepted: true,
        },
        Row {
            case: "the host form, with the role scoped to every stage of that API",
            url: host_form.clone(),
            allowed: execute_api(REGION, &format!("{API_ID}/*")),
            accepted: true,
        },
        Row {
            case: "the host form, with the role scoped to another API",
            url: host_form,
            allowed: execute_api(REGION, &format!("{OTHER_API_ID}/*")),
            accepted: false,
        },
        Row {
            case: "the host form in another Region, with the role scoped to that Region",
            url: format!("https://{API_ID}.execute-api.{OTHER_REGION}.amazonaws.com/prod"),
            allowed: execute_api(OTHER_REGION, &format!("{API_ID}/*")),
            accepted: true,
        },
        Row {
            case: "the host form in another Region, with the role scoped to the request's",
            url: format!("https://{API_ID}.execute-api.{OTHER_REGION}.amazonaws.com/prod"),
            allowed: execute_api(REGION, &format!("{API_ID}/*")),
            accepted: false,
        },
        Row {
            case: "the path form on a loopback endpoint, scoped to every stage of that API",
            url: format!("https://127.0.0.1:39477/_aws/execute-api/{API_ID}/sim2real"),
            allowed: execute_api(REGION, &format!("{API_ID}/*")),
            accepted: true,
        },
        Row {
            case: "the path form, scoped to exactly the stage it names",
            url: format!("https://127.0.0.1:46407/_aws/execute-api/{API_ID}/sim2real"),
            allowed: execute_api(REGION, &format!("{API_ID}/sim2real/*")),
            accepted: true,
        },
        Row {
            case: "the path form, scoped to a DIFFERENT stage of the same API",
            url: format!("https://127.0.0.1:34285/_aws/execute-api/{API_ID}/sim2real"),
            allowed: execute_api(REGION, &format!("{API_ID}/test/*")),
            accepted: false,
        },
        Row {
            case: "the path form naming a resource path after the stage",
            url: format!("https://localhost:9297/_aws/execute-api/{API_ID}/test/auth"),
            allowed: execute_api(REGION, &format!("{API_ID}/test/*")),
            accepted: true,
        },
        Row {
            case: "the path form over plain HTTP",
            url: format!("http://127.0.0.1:9297/_aws/execute-api/{API_ID}/prod"),
            allowed: execute_api(REGION, &format!("{API_ID}/*")),
            accepted: true,
        },
        Row {
            case: "the path form, scoped to another API",
            url: format!("https://127.0.0.1:9297/_aws/execute-api/{API_ID}/prod"),
            allowed: execute_api(REGION, &format!("{OTHER_API_ID}/*")),
            accepted: false,
        },
        Row {
            case: "the path form, scoped to the same API in another Region",
            url: format!("https://127.0.0.1:9297/_aws/execute-api/{API_ID}/prod"),
            allowed: execute_api(OTHER_REGION, &format!("{API_ID}/*")),
            accepted: false,
        },
        Row {
            case: "a URL that is no invoke endpoint, with a narrowly scoped role",
            url: "https://idp.example.invalid/auth".to_string(),
            allowed: execute_api(REGION, &format!("{API_ID}/*")),
            accepted: false,
        },
    ]
}

/// The provider members a row sends.
fn provider(url: &str, role_arn: &str) -> Value {
    json!({
        "IdentityProviderType": "API_GATEWAY",
        "IdentityProviderDetails": {"Url": url, "InvocationRole": role_arn},
    })
}

/// The role every row's provider names; a fresh emulator per row holds exactly one grant
/// for it.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/transfer-idp";

/// What ONE row asserts of `CreateServer`, for every table in this suite: a role carrying
/// exactly that row's grant, the create driven over the wire, and then either the refusal
/// the rule earns — the modelled `InvalidRequestException` (400), its fixed text followed
/// by the request's own `Url`, and no server persisted — or the accepted create's round
/// trip through `DescribeServer`.
///
/// The emulator and the `ServerId` are handed back for an accepted row, so a table can go
/// on to assert what else that server answers.
async fn create_server_verifies(row: &Row) -> Option<(Emulator, String)> {
    let emulator = Emulator::new();
    emulator.seed_role(ROLE_ARN, &allow_invoke(&row.allowed));

    let answer = emulator
        .call("CreateServer", provider(&row.url, ROLE_ARN))
        .await;

    if !row.accepted {
        assert_eq!(
            answer.status, 400,
            "case {:?}: expected HTTP 400 for grant {:?}, got {} with body {}",
            row.case, row.allowed, answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "InvalidRequestException",
            "case {:?}: expected InvalidRequestException for grant {:?}, got body {}",
            row.case,
            row.allowed,
            answer.body
        );
        let message = member_of(&answer.body, "Message")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            message,
            format!("Unable to verify access to API {}", row.url),
            "case {:?}: the refusal must name the API it could not verify",
            row.case
        );
        let listed = emulator.call("ListServers", json!({})).await;
        let servers = listed
            .get("Servers")
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            servers.is_empty(),
            "case {:?}: a refused create must persist no server, got {}",
            row.case,
            listed.body
        );
        return None;
    }

    assert_eq!(
        answer.status, 200,
        "case {:?}: expected the create to be served for grant {:?}, got {} with body {}",
        row.case, row.allowed, answer.status, answer.body
    );
    let server_id = answer
        .get("ServerId")
        .as_str()
        .unwrap_or_else(|| panic!("case {:?}: expected a ServerId", row.case))
        .to_string();

    // The round trip: the server really was persisted as an API_GATEWAY-backed one,
    // with the provider it was verified for.
    let described = emulator
        .call("DescribeServer", json!({"ServerId": &server_id}))
        .await;
    assert_eq!(
        described.status, 200,
        "case {:?}: expected DescribeServer 200, got {} with body {}",
        row.case, described.status, described.body
    );
    let server = described.get("Server").clone();
    assert_eq!(
        member_of(&server, "IdentityProviderType"),
        &json!("API_GATEWAY"),
        "case {:?}: expected the stored provider type, got {server}",
        row.case
    );
    let details = member_of(&server, "IdentityProviderDetails").clone();
    assert_eq!(
        (
            member_of(&details, "Url").as_str(),
            member_of(&details, "InvocationRole").as_str(),
        ),
        (Some(row.url.as_str()), Some(ROLE_ARN)),
        "case {:?}: expected the provider to be reported as configured, got {details}",
        row.case
    );
    Some((emulator, server_id))
}

#[tokio::test]
async fn an_api_gateway_provider_verifies_against_the_api_either_invoke_url_form_names() {
    for row in rows() {
        create_server_verifies(&row).await;
    }
}

/// RULE (the same rule, at the other operation that verifies it): `UpdateServer`
/// verifies a provider it is handed on exactly the same reading of the `Url`, so a
/// server can be moved onto an API named by either addressing.
///
/// The rows differ along the addressing and along whether the role covers the API.
#[tokio::test]
async fn an_update_verifies_a_provider_on_the_same_reading_of_the_invoke_url() {
    // (case, the Url to update onto, the resource the role may invoke, accepted)
    let cases: Vec<(&str, String, String, bool)> = vec![
        (
            "the host form, covered",
            format!("https://{API_ID}.execute-api.{REGION}.amazonaws.com/prod"),
            execute_api(REGION, &format!("{API_ID}/*")),
            true,
        ),
        (
            "the path form, covered",
            format!("https://127.0.0.1:39477/_aws/execute-api/{API_ID}/sim2real"),
            execute_api(REGION, &format!("{API_ID}/*")),
            true,
        ),
        (
            "the path form, not covered",
            format!("https://127.0.0.1:39477/_aws/execute-api/{API_ID}/sim2real"),
            execute_api(REGION, &format!("{OTHER_API_ID}/*")),
            false,
        ),
    ];

    for (case, url, allowed, accepted) in cases {
        let emulator = Emulator::new();
        emulator.seed_role(ROLE_ARN, &allow_invoke(&allowed));
        let server_id = emulator.create_server(json!({})).await;

        let mut request = provider(&url, ROLE_ARN);
        request["ServerId"] = json!(&server_id);
        let answer = emulator.call("UpdateServer", request).await;

        if accepted {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the update to be served, got {} with body {}",
                answer.status, answer.body
            );
            let described = emulator
                .call("DescribeServer", json!({"ServerId": &server_id}))
                .await;
            let details =
                member_of(described.get("Server"), "IdentityProviderDetails").clone();
            assert_eq!(
                member_of(&details, "Url").as_str(),
                Some(url.as_str()),
                "case {case:?}: expected the updated provider to be stored, got {details}"
            );
        } else {
            assert_eq!(
                answer.error_code(),
                "InvalidRequestException",
                "case {case:?}: expected InvalidRequestException, got {} with body {}",
                answer.status,
                answer.body
            );
            let described = emulator
                .call("DescribeServer", json!({"ServerId": &server_id}))
                .await;
            assert_eq!(
                member_of(described.get("Server"), "IdentityProviderType"),
                &json!("SERVICE_MANAGED"),
                "case {case:?}: a refused update must leave the stored provider alone, got {}",
                described.body
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The GRANT FORMS a caller writes for an API Gateway invoke, against the resource
// the verification resolves the provider's endpoint to.
// ---------------------------------------------------------------------------

/// The stage the grant rows' provider URL names, and another stage of the same API.
const GRANT_STAGE: &str = "test";
const OTHER_STAGE: &str = "prod";

/// The invoke URL this emulator publishes for a deployed stage of [`API_ID`], on a
/// loopback endpoint, optionally with a resource path under the stage — the addressing a
/// client of this emulator is handed.
fn published_url(resource_path: &str) -> String {
    format!("https://127.0.0.1:35819/_aws/execute-api/{API_ID}/{GRANT_STAGE}{resource_path}")
}

/// The invoke URL AWS publishes for the same deployed stage.
fn aws_url() -> String {
    format!("https://{API_ID}.execute-api.{REGION}.amazonaws.com/{GRANT_STAGE}")
}

/// The grant rows, in the same [`Row`] shape the addressing table above uses. The wildcard positions are the parts of the
/// `<api-id>/<stage>/<HTTP-METHOD>/<resource-path>` resource form AWS publishes for the
/// `execute-api` plane: the whole API, that form with stage, method and path all
/// wildcarded (the canonical whole-API grant a caller writes), one stage's methods and
/// paths, one method's paths, and the exact resource — crossed with the same forms
/// pointing at another api, stage, Region, method or path.
fn grant_rows() -> Vec<Row> {
    vec![
        Row {
            case: "every resource of every service",
            url: published_url(""),
            allowed: "*".to_string(),
            accepted: true,
        },
        Row {
            case: "the whole API",
            url: published_url(""),
            allowed: execute_api(REGION, &format!("{API_ID}/*")),
            accepted: true,
        },
        Row {
            case: "stage, method and resource path all wildcarded",
            url: published_url(""),
            allowed: execute_api(REGION, &format!("{API_ID}/*/*/*")),
            accepted: true,
        },
        Row {
            case: "every method and path of exactly the stage the URL names",
            url: published_url(""),
            allowed: execute_api(REGION, &format!("{API_ID}/{GRANT_STAGE}/*/*")),
            accepted: true,
        },
        Row {
            case: "every path of the method the authentication request is made with",
            url: published_url(""),
            allowed: execute_api(REGION, &format!("{API_ID}/{GRANT_STAGE}/GET/*")),
            accepted: true,
        },
        Row {
            case: "exactly the stage's root resource, read with that method",
            url: published_url(""),
            allowed: execute_api(REGION, &format!("{API_ID}/{GRANT_STAGE}/GET/")),
            accepted: true,
        },
        Row {
            case: "exactly the resource path the URL itself names",
            url: published_url("/auth"),
            allowed: execute_api(REGION, &format!("{API_ID}/{GRANT_STAGE}/GET/auth")),
            accepted: true,
        },
        Row {
            case: "a deeper resource path the URL names, with the path wildcarded",
            url: published_url("/servers/u/config"),
            allowed: execute_api(REGION, &format!("{API_ID}/{GRANT_STAGE}/GET/servers/*")),
            accepted: true,
        },
        Row {
            case: "another resource path of the same stage and method",
            url: published_url("/auth"),
            allowed: execute_api(REGION, &format!("{API_ID}/{GRANT_STAGE}/GET/elsewhere")),
            accepted: false,
        },
        Row {
            case: "a method the authentication request is not made with",
            url: published_url(""),
            allowed: execute_api(REGION, &format!("{API_ID}/{GRANT_STAGE}/POST/*")),
            accepted: false,
        },
        Row {
            case: "another stage of the same API",
            url: published_url(""),
            allowed: execute_api(REGION, &format!("{API_ID}/{OTHER_STAGE}/*/*")),
            accepted: false,
        },
        Row {
            case: "another API, with its stage, method and path wildcarded",
            url: published_url(""),
            allowed: execute_api(REGION, &format!("{OTHER_API_ID}/*/*/*")),
            accepted: false,
        },
        Row {
            case: "the same API in another Region",
            url: published_url(""),
            allowed: execute_api(OTHER_REGION, &format!("{API_ID}/*/*/*")),
            accepted: false,
        },
        Row {
            case: "stage, method and path wildcarded, on the URL form AWS publishes",
            url: aws_url(),
            allowed: execute_api(REGION, &format!("{API_ID}/*/*/*")),
            accepted: true,
        },
        Row {
            case: "one stage's methods and paths, on the URL form AWS publishes",
            url: aws_url(),
            allowed: execute_api(REGION, &format!("{API_ID}/{GRANT_STAGE}/*/*")),
            accepted: true,
        },
        Row {
            case: "another stage of the same API, on the URL form AWS publishes",
            url: aws_url(),
            allowed: execute_api(REGION, &format!("{API_ID}/{OTHER_STAGE}/*/*")),
            accepted: false,
        },
    ]
}

/// RULE: the verification is taken on the resource an invoke of the provider's endpoint
/// is authorized against, so EVERY grant whose wildcards cover that request verifies the
/// invocation role and `CreateServer` answers a `ServerId`, while a grant that covers
/// another api, stage, Region, method or resource path is refused with
/// `InvalidRequestException` (400) and persists no server.
#[tokio::test]
async fn an_invocation_role_verifies_on_every_grant_covering_the_endpoints_invoke_resource() {
    for row in grant_rows() {
        let Some((emulator, server_id)) = create_server_verifies(&row).await else {
            continue;
        };

        // The server an accepted grant minted is one `TestIdentityProvider` reaches: the
        // operation an API_GATEWAY server exists to exercise is no longer refused in its
        // setup.
        let tested = emulator
            .call(
                "TestIdentityProvider",
                json!({"ServerId": &server_id, "UserName": "idp-user"}),
            )
            .await;
        assert_eq!(
            tested.status, 200,
            "case {:?}: expected TestIdentityProvider 200 against the created server, \
             got {} with body {}",
            row.case, tested.status, tested.body
        );
        assert_eq!(
            tested.get("Url").as_str(),
            Some(row.url.as_str()),
            "case {:?}: expected the tested endpoint to be the configured one, got {}",
            row.case,
            tested.body
        );
    }
}

/// RULE (the same rule at the other operation that verifies a provider): `UpdateServer`
/// reads the grant against the same invoke resource, so the forms that verify a create
/// verify an update, and the ones that do not leave the stored provider alone.
#[tokio::test]
async fn an_update_reads_a_grant_against_the_same_invoke_resource() {
    // (case, the resource the role's grant allows, accepted)
    let cases: Vec<(&str, String, bool)> = vec![
        (
            "stage, method and path wildcarded",
            execute_api(REGION, &format!("{API_ID}/*/*/*")),
            true,
        ),
        (
            "every method and path of the stage the URL names",
            execute_api(REGION, &format!("{API_ID}/{GRANT_STAGE}/*/*")),
            true,
        ),
        (
            "every path of the method the authentication request is made with",
            execute_api(REGION, &format!("{API_ID}/{GRANT_STAGE}/GET/*")),
            true,
        ),
        (
            "a method the authentication request is not made with",
            execute_api(REGION, &format!("{API_ID}/{GRANT_STAGE}/POST/*")),
            false,
        ),
        (
            "another API",
            execute_api(REGION, &format!("{OTHER_API_ID}/*/*/*")),
            false,
        ),
    ];

    for (case, granted, accepted) in cases {
        let emulator = Emulator::new();
        emulator.seed_role(ROLE_ARN, &allow_invoke(&granted));
        let server_id = emulator.create_server(json!({})).await;
        let url = published_url("");

        let mut request = provider(&url, ROLE_ARN);
        request["ServerId"] = json!(&server_id);
        let answer = emulator.call("UpdateServer", request).await;

        let described = emulator
            .call("DescribeServer", json!({"ServerId": &server_id}))
            .await;
        let stored = member_of(described.get("Server"), "IdentityProviderType").clone();
        if accepted {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the update to be served for grant {granted:?}, \
                 got {} with body {}",
                answer.status, answer.body
            );
            assert_eq!(
                stored,
                json!("API_GATEWAY"),
                "case {case:?}: the verified provider must be stored, got {}",
                described.body
            );
        } else {
            assert_eq!(
                answer.error_code(),
                "InvalidRequestException",
                "case {case:?}: expected InvalidRequestException for grant {granted:?}, \
                 got {} with body {}",
                answer.status,
                answer.body
            );
            assert_eq!(
                stored,
                json!("SERVICE_MANAGED"),
                "case {case:?}: a refused update must leave the stored provider alone, got {}",
                described.body
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The AUTHENTICATION REQUEST itself: an `API_GATEWAY` provider whose endpoint this
// emulator's own API Gateway emulation serves
// ---------------------------------------------------------------------------

/// `StatusCode`'s modelled default (`@default(0)`): the value an answer carries when there is
/// no HTTP status from a provider to report.
const NO_RESPONSE_STATUS: i64 = 0;

/// The media type the authentication answer is served in, which is the key of the mapping
/// template a row's endpoint renders its body from.
const APPLICATION_JSON: &str = "application/json";

/// An API Gateway client over this emulator's own in-process callback — the very seam the
/// transfer handler invokes a provider's endpoint over, so an API a row deploys through it is
/// the one that handler reaches.
fn api_gateway(emulator: &Emulator) -> aws_sdk_apigateway::Client {
    let callback = emulator.callback();
    let config = aws_sdk_apigateway::Config::builder()
        .behavior_version(aws_sdk_apigateway::config::BehaviorVersion::latest())
        .region(aws_sdk_apigateway::config::Region::new(REGION))
        .endpoint_url(format!("https://apigateway.{REGION}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(aws_sdk_apigateway::config::retry::RetryConfig::disabled())
        .timeout_config(aws_sdk_apigateway::config::timeout::TimeoutConfig::disabled())
        .identity_cache(aws_sdk_apigateway::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            aws_sdk_apigateway::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    aws_sdk_apigateway::Client::from_conf(config)
}

/// One row of the invocation table: the REST API a row deploys, how its provider `Url`
/// addresses the deployed stage, what that endpoint answers, and the request the row tests
/// the provider with.
struct Invoked {
    case: &'static str,
    /// The resource the deployed method serves, as the path parts under the api's root.
    resource: &'static [&'static str],
    /// The verb the deployed method is declared with.
    verb: &'static str,
    /// The stage the deployment is published into, which is the first path segment of the
    /// invoke URL.
    stage: &'static str,
    /// Whether the provider's `Url` carries the api and the Region in the HOST (the invoke
    /// URL AWS publishes) rather than in the PATH (the one this emulator publishes).
    host_form: bool,
    /// The status the endpoint answers the authentication request with, which its `MOCK`
    /// integration's request template names.
    status: u16,
    /// The body the endpoint answers with, rendered from its integration response template.
    body: &'static str,
    /// The `TestIdentityProvider` members beyond `ServerId` and `UserName`.
    extra: fn() -> Value,
    /// The `UserName` authenticated, which is one segment of the resource the request reads.
    user_name: &'static str,
}

/// Create the REST API a row describes and deploy it, answering the api id it was minted
/// with.
///
/// The shape is the one a client builds for a custom identity provider: one resource serving
/// the authentication path, a method authorized by IAM on it, a `MOCK` integration whose
/// request template names the status the endpoint answers, and an integration response for
/// that status whose `application/json` template is the body it serves.
async fn deploy_authentication_api(gateway: &aws_sdk_apigateway::Client, row: &Invoked) -> String {
    let created = gateway
        .create_rest_api()
        .name(row.case)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {:?}: CreateRestApi: {error:?}", row.case));
    let api_id = created
        .id()
        .unwrap_or_else(|| panic!("case {:?}: CreateRestApi answered no id", row.case))
        .to_string();

    let resources = gateway
        .get_resources()
        .rest_api_id(&api_id)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {:?}: GetResources: {error:?}", row.case));
    let mut parent = resources
        .items()
        .iter()
        .find(|item| item.path() == Some("/"))
        .and_then(|item| item.id())
        .unwrap_or_else(|| panic!("case {:?}: the api has no root resource", row.case))
        .to_string();
    for part in row.resource {
        let child = gateway
            .create_resource()
            .rest_api_id(&api_id)
            .parent_id(&parent)
            .path_part(*part)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {:?}: CreateResource({part}): {error:?}", row.case)
            });
        parent = child
            .id()
            .unwrap_or_else(|| panic!("case {:?}: CreateResource answered no id", row.case))
            .to_string();
    }

    gateway
        .put_method()
        .rest_api_id(&api_id)
        .resource_id(&parent)
        .http_method(row.verb)
        // The authorization a custom identity provider's method declares: the caller is the
        // Transfer service, signing as the invocation role.
        .authorization_type("AWS_IAM")
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {:?}: PutMethod: {error:?}", row.case));
    gateway
        .put_integration()
        .rest_api_id(&api_id)
        .resource_id(&parent)
        .http_method(row.verb)
        .r#type(aws_sdk_apigateway::types::IntegrationType::Mock)
        .request_templates(
            APPLICATION_JSON,
            format!("{{\"statusCode\": {}}}", row.status),
        )
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {:?}: PutIntegration: {error:?}", row.case));
    gateway
        .put_integration_response()
        .rest_api_id(&api_id)
        .resource_id(&parent)
        .http_method(row.verb)
        .status_code(row.status.to_string())
        .response_templates(APPLICATION_JSON, row.body)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {:?}: PutIntegrationResponse: {error:?}", row.case));
    gateway
        .create_deployment()
        .rest_api_id(&api_id)
        .stage_name(row.stage)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {:?}: CreateDeployment: {error:?}", row.case));
    api_id
}

/// The `Url` a row's provider names for the stage it deployed: the invoke URL AWS publishes
/// for a deployed stage, or the one this emulator publishes for a client that resolves that
/// host nowhere — both of which name the same stage.
fn provider_url(row: &Invoked, api_id: &str) -> String {
    match row.host_form {
        true => format!(
            "https://{api_id}.execute-api.{REGION}.amazonaws.com/{}",
            row.stage
        ),
        false => format!(
            "https://127.0.0.1:9297/_aws/execute-api/{api_id}/{}",
            row.stage
        ),
    }
}

/// The documented authentication resource of a custom identity provider: Transfer Family's
/// `GET /servers/{serverId}/users/{username}/config`, as the path parts a client creates
/// under the api's root. Source: the method request a custom identity provider serves, which
/// this crate's invocation-role verification is also named after
/// (`src/state/authorization.rs`).
const DOCUMENTED_RESOURCE: &[&str] = &["servers", "{serverId}", "users", "{username}", "config"];

/// A resource serving EVERY path of the stage — the greedy proxy a client deploys when one
/// backend answers the whole API.
const PROXY_RESOURCE: &[&str] = &["{proxy+}"];

/// The rows: each deploys one API, and they differ along what the rule generalises over — the
/// resource the endpoint serves the authentication at (the documented tree, a greedy proxy),
/// the verb the method is declared with, the stage name, the addressing the provider's `Url`
/// uses, the status the endpoint answers, the body it returns, the user authenticated and
/// which optional request members are sent.
const INVOKED_ROWS: &[Invoked] = &[
    Invoked {
        case: "the documented authentication resource, read with GET",
        resource: DOCUMENTED_RESOURCE,
        verb: "GET",
        stage: "test",
        host_form: false,
        status: 200,
        body: r#"{"Role":"arn:aws:iam::000000000000:role/idp-mock-role","HomeDirectory":"/one"}"#,
        extra: || json!({}),
        user_name: "sim2realuser",
    },
    Invoked {
        case: "a greedy proxy resource serving every path, with ANY",
        resource: PROXY_RESOURCE,
        verb: "ANY",
        stage: "prod",
        host_form: false,
        status: 200,
        body: r#"{"Role":"arn:aws:iam::000000000000:role/idp-proxy-role","HomeDirectory":"/two"}"#,
        extra: || json!({"UserPassword": "does-not-matter", "ServerProtocol": "SFTP"}),
        user_name: "other_user",
    },
    Invoked {
        case: "the same proxy, addressed on the invoke host AWS publishes",
        resource: PROXY_RESOURCE,
        verb: "ANY",
        stage: "live",
        host_form: true,
        status: 200,
        body: r#"{"Role":"arn:aws:iam::000000000000:role/idp-host-role","HomeDirectory":"/three"}"#,
        extra: || json!({"SourceIp": "10.0.0.4", "ServerProtocol": "FTPS"}),
        user_name: "partner.account",
    },
    Invoked {
        case: "an endpoint that refuses the authentication",
        resource: PROXY_RESOURCE,
        verb: "ANY",
        stage: "test",
        host_form: false,
        status: 403,
        body: r#"{"message":"the provider declined this user"}"#,
        extra: || json!({"UserPassword": "does-not-matter", "ServerProtocol": "FTP"}),
        user_name: "declined.user",
    },
];

/// RULE: for a server whose `IdentityProviderType` names a provider endpoint this emulator
/// serves, `TestIdentityProvider` makes the documented authentication request against that
/// endpoint and reports THAT invocation's outcome — `StatusCode` is the HTTP status the
/// endpoint answered and `Response` is its response body verbatim — with `Url` the endpoint
/// the server is configured with.
#[tokio::test]
async fn testing_an_api_gateway_provider_reports_what_its_endpoint_answered() {
    for row in INVOKED_ROWS {
        let case = row.case;
        let emulator = Emulator::with_api_gateway();
        let gateway = api_gateway(&emulator);
        let api_id = deploy_authentication_api(&gateway, row).await;
        let url = provider_url(row, &api_id);
        // The role the provider is invoked as, allowed to invoke that whole API.
        emulator.seed_role(
            ROLE_ARN,
            &allow_invoke(&execute_api(REGION, &format!("{api_id}/*"))),
        );

        let server_id = emulator
            .create_server(json!({
                "Protocols": ["SFTP"],
                "Domain": "S3",
                "EndpointType": "PUBLIC",
                "IdentityProviderType": "API_GATEWAY",
                "IdentityProviderDetails": {"Url": &url, "InvocationRole": ROLE_ARN},
            }))
            .await;

        let mut request = json!({"ServerId": &server_id, "UserName": row.user_name});
        for (member, value) in (row.extra)()
            .as_object()
            .expect("the row's extra members are an object")
        {
            request[member] = value.clone();
        }
        let answer = emulator.call("TestIdentityProvider", request).await;

        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected TestIdentityProvider 200, got {} with body {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.get("StatusCode").as_i64(),
            Some(i64::from(row.status)),
            "case {case:?}: expected the status the provider's endpoint answered, got {}",
            answer.body
        );
        assert_eq!(
            answer.get("Url").as_str(),
            Some(url.as_str()),
            "case {case:?}: expected the endpoint the server names, got {}",
            answer.body
        );
        let response = answer
            .get("Response")
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the endpoint's own response body, got {}",
                    answer.body
                )
            })
            .to_string();
        let returned: Value = serde_json::from_str(&response).unwrap_or_else(|error| {
            panic!("case {case:?}: expected the endpoint's JSON body, got {error} for {response:?}")
        });
        let expected: Value =
            serde_json::from_str(row.body).expect("the row's own body is a JSON document");
        assert_eq!(
            returned, expected,
            "case {case:?}: expected the body the endpoint serves, got {returned}"
        );
    }
}

/// RULE (the same rule, where no provider answered): an `API_GATEWAY` server whose `Url`
/// names an endpoint this emulator serves nothing at is answered with the no-response
/// answer — the modelled default status, no `Response` at all, and a message saying why —
/// because no status and no body may be reported for a request nothing answered. An endpoint
/// the API Gateway emulation DOES answer, even to refuse the call, keeps reporting that
/// answer.
#[tokio::test]
async fn an_api_gateway_endpoint_nothing_serves_reports_no_response() {
    let emulator = Emulator::with_api_gateway();
    let gateway = api_gateway(&emulator);
    // One API this emulator really holds, so the rows below differ from it only in WHICH
    // endpoint they name — not in whether the API Gateway emulation is there at all.
    let served = deploy_authentication_api(&gateway, &INVOKED_ROWS[0]).await;

    // (case, the Url the provider names)
    let rows: Vec<(&str, String)> = vec![
        (
            "an api id this emulator holds no API for",
            format!("https://127.0.0.1:9297/_aws/execute-api/{OTHER_API_ID}/test"),
        ),
        (
            "an api id no API has, on the invoke host AWS publishes",
            format!("https://{OTHER_API_ID}.execute-api.{REGION}.amazonaws.com/test"),
        ),
        (
            "an endpoint outside this emulator altogether",
            "https://idp.example.invalid/auth".to_string(),
        ),
    ];

    for (case, url) in rows {
        emulator.seed_role(ROLE_ARN, &allow_invoke("*"));
        let server_id = emulator
            .create_server(json!({
                "IdentityProviderType": "API_GATEWAY",
                "IdentityProviderDetails": {"Url": &url, "InvocationRole": ROLE_ARN},
            }))
            .await;
        let answer = emulator
            .call(
                "TestIdentityProvider",
                json!({"ServerId": &server_id, "UserName": "sim2realuser"}),
            )
            .await;
        assert_eq!(
            (answer.status, answer.get("StatusCode").as_i64()),
            (200, Some(NO_RESPONSE_STATUS)),
            "case {case:?}: expected the no-status answer, got {} with body {}",
            answer.status,
            answer.body
        );
        assert_eq!(
            answer.get("Url").as_str(),
            Some(url.as_str()),
            "case {case:?}: expected the endpoint the server names, got {}",
            answer.body
        );
        assert!(
            answer.get("Response").is_null(),
            "case {case:?}: expected no Response for a request nothing answered, got {}",
            answer.body
        );
        let message = member_of(&answer.body, "Message")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !message.is_empty() && !message.contains(&server_id) && !message.contains(&url),
            "case {case:?}: expected a message fixed for the condition, got {message:?}"
        );
    }

    // The OTHER side of the same rule: a stage of an API this emulator holds that carries no
    // deployment is an endpoint the API Gateway emulation answers ITSELF — it refuses the
    // invoke — so the test reports that refusal's own status and body rather than the
    // no-response answer, which is reserved for a request nothing answered at all.
    let case = "a stage of a held API that carries no deployment";
    let url = format!("https://127.0.0.1:9297/_aws/execute-api/{served}/never-deployed");
    emulator.seed_role(ROLE_ARN, &allow_invoke("*"));
    let server_id = emulator
        .create_server(json!({
            "IdentityProviderType": "API_GATEWAY",
            "IdentityProviderDetails": {"Url": &url, "InvocationRole": ROLE_ARN},
        }))
        .await;
    let answer = emulator
        .call(
            "TestIdentityProvider",
            json!({"ServerId": &server_id, "UserName": "sim2realuser"}),
        )
        .await;
    let reported = answer.get("StatusCode").as_i64().unwrap_or_default();
    assert!(
        (400..500).contains(&reported),
        "case {case:?}: expected the status the endpoint refused the invoke with, got {}",
        answer.body
    );
    assert!(
        answer.get("Response").is_string(),
        "case {case:?}: expected the endpoint's own refusal body, got {}",
        answer.body
    );
}

/// RULE (the same rule at the request's own members): the authentication request reads the
/// resource of the SERVER and the USER it is made for, so one endpoint answering every path
/// is handed each row's own server and user — which is what the provider's body computes its
/// answer from.
#[tokio::test]
async fn the_authentication_request_names_the_server_and_the_user_it_tests() {
    let emulator = Emulator::with_api_gateway();
    let gateway = api_gateway(&emulator);
    // The DOCUMENTED resource tree, which serves `/servers/{serverId}/users/{username}/config`
    // and nothing else: an endpoint deployed this way answers only a request that really
    // addresses that path, so what it answers is evidence of the path the handler read.
    let row = &INVOKED_ROWS[0];
    let api_id = deploy_authentication_api(&gateway, row).await;
    let url = provider_url(row, &api_id);
    emulator.seed_role(
        ROLE_ARN,
        &allow_invoke(&execute_api(REGION, &format!("{api_id}/*"))),
    );

    // Two servers of the SAME provider, and two users on each: every one of the four
    // authentications addresses its own resource of that one endpoint.
    let mut servers = Vec::new();
    for _ in 0..2 {
        servers.push(
            emulator
                .create_server(json!({
                    "IdentityProviderType": "API_GATEWAY",
                    "IdentityProviderDetails": {"Url": &url, "InvocationRole": ROLE_ARN},
                }))
                .await,
        );
    }
    for server_id in &servers {
        for user_name in ["sim2realuser", "another.user"] {
            let case = format!("{server_id} authenticating {user_name}");
            let answer = emulator
                .call(
                    "TestIdentityProvider",
                    json!({"ServerId": server_id, "UserName": user_name,
                           "ServerProtocol": "SFTP"}),
                )
                .await;
            assert_eq!(
                (answer.status, answer.get("StatusCode").as_i64()),
                (200, Some(i64::from(row.status))),
                "case {case:?}: expected the status the endpoint answered, got {} with body {}",
                answer.status,
                answer.body
            );
            assert!(
                answer.get("Response").is_string(),
                "case {case:?}: expected the endpoint's own body, got {}",
                answer.body
            );
        }
    }
}
