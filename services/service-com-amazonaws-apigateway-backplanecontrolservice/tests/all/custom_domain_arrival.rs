// SPDX-License-Identifier: CC0-1.0

//! What a request that arrived on a CUSTOM DOMAIN NAME of the account is served by, on the
//! address that domain name publishes — the two rules a front door in front of an api has to
//! keep, driven end to end over a real socket in one process.
//!
//! RULE 1: **`disableExecuteApiEndpoint` turns off the api's DEFAULT endpoint alone.**
//! "Specifies whether clients can invoke your API by using the default `execute-api`
//! endpoint… To require that clients use a custom domain name to invoke your API, disable
//! the default endpoint" (`RestApi$disableExecuteApiEndpoint`), so an api that turned it off
//! answers nothing on
//! `https://{api-id}.execute-api.{region}.amazonaws.com` (nor on the reserved data-plane path
//! a client of this emulator reaches that endpoint by) and keeps answering on every custom
//! domain name a `BasePathMapping` put in front of it.
//!
//! RULE 2: **a request that arrived on a custom domain name is AUTHENTICATED as the client
//! sent it.** A SigV4 signature covers the verb, the path, the query, the headers the
//! request's own `SignedHeaders` element declares and the payload, so what the front door
//! hands the stage has to be that request: an `AWS_IAM` method is served a credential that
//! signed the custom domain name's own host and path, and refuses a signature over anything
//! else with the sentence AWS words that class with — which is also what makes the served
//! row evidence that nothing here skips the verification.
//!
//! Both rules are about the ENDPOINT a request arrived at rather than about one recorded
//! call, so the rows span what that generalises over: which value `disableExecuteApiEndpoint`
//! carries, which endpoint the request was addressed to (the custom domain name's own
//! published address, the api's own `execute-api` host, and the reserved data-plane path on
//! the emulator's own endpoint), which base path the mapping answers under (`(none)` and a
//! named one), and — for the signed rows — which credential signed and what it signed over.
//!
//! Nothing is staged in a store: every api, stage, domain name and mapping is created through
//! this service's own API, and the client is a real socket client that dials the address the
//! record published, so an answer not computed from those records cannot pass.

use std::time::{Duration, SystemTime};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use bytes::Bytes;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::base_path_mapping::create_mapping;
use crate::domain_name_collection::created_domain;
use crate::execute_api_data_plane::{deploy, invoke_on_host, json_template, mock_resource};
use crate::regional_custom_domain_front_door::dial;
use crate::rest_api_lifecycle::{string_member, Gateway};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-east-1";

/// The stage every api of this suite is deployed to.
const STAGE: &str = "prod";

/// The resource path the deployed method answers under.
const RESOURCE: &str = "pets";

/// The body the deployed `MOCK` method answers with, so a `200` is the STAGE's own answer
/// rather than anything this suite sent.
const STAGE_BODY: &str = r#"{"at":"pets"}"#;

/// The signing name an invoke of a deployed stage is signed under: the `execute-api` service
/// AWS publishes the invoke endpoint and the `execute-api:Invoke` action under, which is not
/// this service's control-plane name (`apigateway`).
const INVOKE_SIGNING_NAME: &str = "execute-api";

/// The emulator's own endpoint, which a client that resolves the api's invoke host nowhere
/// sends as the `Host` instead.
const EMULATOR_ENDPOINT: &str = "localhost:9297";

/// How long one exchange on a published address may take. A generous bound on work that is
/// all in this process, so a loaded host is not mistaken for a defect: an exchange that
/// cannot finish inside it has found one.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(60);

/// The sentence AWS refuses a signature that is not the one the credential's secret produces
/// with.
const SIGNATURE_MISMATCH: &str = "The request signature we calculated does not match the \
                                  signature you provided. Check your AWS Secret Access Key \
                                  and signing method. Consult the service documentation for \
                                  details.";

/// The sentence AWS refuses a credential whose scope names another service than the one the
/// invoke endpoint is signed under with.
const MISSCOPED_CREDENTIAL: &str =
    "Credential should be scoped to correct service: 'execute-api'.";

/// The sentence an `AWS_IAM` method reached with no credential at all is answered with.
const MISSING_TOKEN: &str = "Missing Authentication Token";

// ---------------------------------------------------------------------------
// The client that dials the published address
// ---------------------------------------------------------------------------

/// Send `request` to `published` in the clear and answer the raw HTTP response.
///
/// A record enforcing no mutual TLS serves the half of its address a client that cannot
/// verify a self-signed certificate dials, which is what lets these rows be about the
/// ADDRESSING and the credential rather than about a handshake (the handshake itself is the
/// `regional_custom_domain_front_door` suite's subject).
async fn exchange(case: &str, published: &str, request: &http::Request<Bytes>) -> String {
    let mut stream = dial(case, published).await;
    stream
        .write_all(&rendered(request))
        .await
        .unwrap_or_else(|error| panic!("case {case}: the request was not sent: {error}"));
    let mut raw = Vec::new();
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.read_to_end(&mut raw))
        .await
        .unwrap_or_else(|_| panic!("case {case}: the address did not answer in time"))
        .unwrap_or_else(|error| panic!("case {case}: the answer was not read: {error}"));
    String::from_utf8_lossy(&raw).into_owned()
}

/// `request` on the wire: the request line, every header it carries, the framing this leg
/// adds, and the body.
///
/// `connection` and `content-length` are added here rather than signed, because they are not
/// part of what a signature covers: the canonical request covers the headers the request's
/// own `SignedHeaders` element declares.
fn rendered(request: &http::Request<Bytes>) -> Vec<u8> {
    let target = request
        .uri()
        .path_and_query()
        .map(http::uri::PathAndQuery::as_str)
        .unwrap_or("/");
    let mut head = format!("{} {target} HTTP/1.1\r\n", request.method());
    for (name, value) in request.headers() {
        let value = value.to_str().expect("a text header");
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("connection: close\r\n");
    head.push_str(&format!("content-length: {}\r\n\r\n", request.body().len()));
    let mut wire = head.into_bytes();
    wire.extend_from_slice(request.body());
    wire
}

/// The status line of a raw HTTP answer.
fn status_line(answer: &str) -> &str {
    answer.lines().next().unwrap_or_default()
}

/// The body of a raw HTTP answer: everything after the blank line that ends its headers.
fn body_of(answer: &str) -> &str {
    answer.split_once("\r\n\r\n").map_or("", |(_, body)| body)
}

/// One unsigned `GET` of `path`, addressed to `host`.
fn plain_get(host: &str, path: &str) -> http::Request<Bytes> {
    http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .body(Bytes::new())
        .expect("the request is well formed")
}

// ---------------------------------------------------------------------------
// RULE 1: the default endpoint is the only endpoint the flag turns off
// ---------------------------------------------------------------------------

/// How one row addresses the api.
#[derive(Clone, Copy, Debug)]
enum Addressing {
    /// The address the custom domain name published, which a client dials.
    CustomDomain(&'static str),
    /// The api's own default `execute-api` endpoint host.
    OwnHost,
    /// The reserved data-plane path a client of this emulator reaches that same default
    /// endpoint by.
    ReservedPath,
}

/// One row of the addressing table.
struct Row {
    case: &'static str,
    /// Whether the row addresses the api that turned its default endpoint OFF.
    disabled: bool,
    addressing: Addressing,
    /// The status the endpoint owes the row, and whether the STAGE's own body is expected.
    expected: u16,
    served: bool,
}

/// `CreateRestApi` for a REGIONAL api, with `disableExecuteApiEndpoint` as the row wants it,
/// answering `(api id, root resource id)`.
async fn regional_api(gateway: &Gateway, name: &str, disabled: bool) -> (String, String) {
    let answer = gateway
        .call(
            "POST",
            "/restapis",
            Some(json!({
                "name": name,
                "disableExecuteApiEndpoint": disabled,
                "endpointConfiguration": { "types": ["REGIONAL"] },
            })),
        )
        .await;
    assert_eq!(
        answer.status, 201,
        "setup CreateRestApi({name:?}): expected 201, got {} with {:?}",
        answer.status, answer.body
    );
    let case = format!("CreateRestApi({name:?})");
    assert_eq!(
        answer.body["disableExecuteApiEndpoint"].as_bool(),
        Some(disabled),
        "setup {case}: expected the flag the request set, got {:?}",
        answer.body
    );
    (
        string_member(&answer.body, "id", &case),
        string_member(&answer.body, "rootResourceId", &case),
    )
}

/// One api with `GET /pets` behind a `MOCK` integration, deployed to [`STAGE`].
async fn deployed_api(gateway: &Gateway, name: &str, disabled: bool) -> String {
    let (api, root) = regional_api(gateway, name, disabled).await;
    mock_resource(
        gateway,
        &api,
        &root,
        RESOURCE,
        "GET",
        "200",
        json_template(STAGE_BODY),
    )
    .await;
    deploy(gateway, &api, STAGE).await;
    api
}

/// A REGIONAL custom domain name in front of `api`, with one mapping per base path, and the
/// address it published.
async fn fronted_by(
    gateway: &Gateway,
    domain_name: &str,
    api: &str,
    base_paths: &[&str],
) -> String {
    let created = created_domain(
        gateway,
        domain_name,
        json!({ "endpointConfiguration": { "types": ["REGIONAL"] } }),
    )
    .await;
    let published = string_member(&created, "regionalDomainName", domain_name);
    assert!(
        published.parse::<std::net::SocketAddr>().is_ok(),
        "setup CreateDomainName({domain_name:?}): expected an address a client can dial, got \
         {published:?}"
    );
    for base_path in base_paths {
        let answer = create_mapping(
            gateway,
            domain_name,
            json!({ "basePath": base_path, "restApiId": api, "stage": STAGE }),
        )
        .await;
        assert_eq!(
            answer.status, 201,
            "setup CreateBasePathMapping({base_path:?}): expected 201, got {} with {:?}",
            answer.status, answer.body
        );
    }
    published
}

/// RULE 1: an api that disabled its default `execute-api` endpoint is refused there and keeps
/// being served on the custom domain names mapped to it, while one that left the default
/// endpoint on is served on both.
#[tokio::test]
async fn disabling_the_default_endpoint_leaves_the_custom_domain_name_serving() {
    let gateway = Gateway::new();

    let disabled_api = deployed_api(&gateway, "default-endpoint-off", true).await;
    let enabled_api = deployed_api(&gateway, "default-endpoint-on", false).await;
    // The disabled api's domain name answers under BOTH base-path spellings, so a row can
    // vary the base path without varying anything else.
    let disabled_address = fronted_by(
        &gateway,
        "off.arrival.example.com",
        &disabled_api,
        &["(none)", "shop"],
    )
    .await;
    let enabled_address =
        fronted_by(&gateway, "on.arrival.example.com", &enabled_api, &["(none)"]).await;

    let rows = [
        Row {
            case: "the default endpoint off, on the custom domain name under (none)",
            disabled: true,
            addressing: Addressing::CustomDomain("/pets"),
            expected: 200,
            served: true,
        },
        Row {
            case: "the default endpoint off, on the custom domain name under a base path",
            disabled: true,
            addressing: Addressing::CustomDomain("/shop/pets"),
            expected: 200,
            served: true,
        },
        Row {
            case: "the default endpoint off, on the api's own execute-api host",
            disabled: true,
            addressing: Addressing::OwnHost,
            expected: 403,
            served: false,
        },
        Row {
            case: "the default endpoint off, on the reserved data-plane path of that endpoint",
            disabled: true,
            addressing: Addressing::ReservedPath,
            expected: 403,
            served: false,
        },
        Row {
            case: "the default endpoint on, on the custom domain name",
            disabled: false,
            addressing: Addressing::CustomDomain("/pets"),
            expected: 200,
            served: true,
        },
        Row {
            case: "the default endpoint on, on the api's own execute-api host",
            disabled: false,
            addressing: Addressing::OwnHost,
            expected: 200,
            served: true,
        },
        Row {
            case: "the default endpoint on, on the reserved data-plane path of that endpoint",
            disabled: false,
            addressing: Addressing::ReservedPath,
            expected: 200,
            served: true,
        },
    ];

    for row in rows {
        let case = row.case;
        let api = match row.disabled {
            true => &disabled_api,
            false => &enabled_api,
        };
        let (status, body) = match row.addressing {
            Addressing::CustomDomain(path) => {
                let (address, domain_name) = match row.disabled {
                    true => (&disabled_address, "off.arrival.example.com"),
                    false => (&enabled_address, "on.arrival.example.com"),
                };
                let answered = exchange(case, address, &plain_get(domain_name, path)).await;
                let status = status_line(&answered)
                    .split_whitespace()
                    .nth(1)
                    .and_then(|code| code.parse::<u16>().ok())
                    .unwrap_or_else(|| panic!("case {case}: expected an HTTP answer, got {answered:?}"));
                (status, body_of(&answered).to_string())
            }
            Addressing::OwnHost => {
                let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
                let invoked =
                    invoke_on_host(&gateway, &host, "GET", &format!("/{STAGE}/{RESOURCE}"), &[])
                        .await;
                (invoked.status, invoked.body)
            }
            Addressing::ReservedPath => {
                let path = format!("/_aws/execute-api/{api}/{STAGE}/{RESOURCE}");
                let invoked =
                    invoke_on_host(&gateway, EMULATOR_ENDPOINT, "GET", &path, &[]).await;
                (invoked.status, invoked.body)
            }
        };
        assert_eq!(
            status, row.expected,
            "case {case:?}: expected HTTP {}, got {status} with {body:?}",
            row.expected
        );
        assert_eq!(
            body.contains(STAGE_BODY),
            row.served,
            "case {case:?}: expected the stage's own answer to be served={}, got {body:?}",
            row.served
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 2: the credential is verified against the request the client sent
// ---------------------------------------------------------------------------

/// The credential a signed row signs with.
struct Signer {
    key_id: String,
    secret: String,
}

impl Signer {
    /// `request` signed for `signing_name` with this credential, in its `Authorization`
    /// header.
    fn sign(&self, request: http::Request<Bytes>, signing_name: &str) -> http::Request<Bytes> {
        let mut request = request;
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(REGION)
            .name(signing_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }
}

/// One signed control-plane call through the router, which must answer `expected`.
async fn control(
    gateway: &Gateway,
    signer: &Signer,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let payload = body
        .map(|body| Bytes::from(body.to_string()))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .body(payload)
        .expect("the request is well formed");
    let response = gateway.send(signer.sign(request, "apigateway")).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(&response.into_body()).unwrap_or(Value::Null);
    assert_eq!(
        status, expected,
        "setup {what}: expected HTTP {expected}, got {status} with {answered}"
    );
    answered
}

/// What one signed row presents.
enum Credential {
    /// The key this emulator published, signing the request that is sent.
    Published,
    /// The right key id with another secret.
    OtherSecret,
    /// The right key and secret, over another PATH than the one sent.
    OverAnotherPath,
    /// The right key and secret, scoped to the control plane's own signing name.
    Misscoped,
    /// No credential at all.
    None,
}

/// RULE 2: a request that arrived on a custom domain name is authenticated against the
/// request the client sent — the host it addressed and the path under it — so an `AWS_IAM`
/// method serves a correctly signed caller there and refuses every other credential with the
/// sentence AWS words its class with.
#[tokio::test]
async fn a_signed_invoke_on_a_custom_domain_name_is_authenticated_as_the_client_sent_it() {
    let mut gateway = Gateway::new();
    // Authentication is on BEFORE anything is created, as it is in an emulator started with
    // it on: the front door a custom domain name opens is opened by the create, so a create
    // made on an emulator that verifies nothing would open a front door that verifies
    // nothing either.
    gateway.router.set_request_authentication(true);
    let published_credential = ::emulator_server::SigV4Authenticator::minted_credentials()
        .expect("turning authentication on publishes this process's keys");
    let signer = Signer {
        key_id: published_credential.access_key_id.clone(),
        secret: published_credential.secret_access_key.clone(),
    };

    let domain_name = "signed.arrival.example.com";
    let api = control(
        &gateway,
        &signer,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({
            "name": "signed-arrival",
            "disableExecuteApiEndpoint": true,
            "endpointConfiguration": { "types": ["REGIONAL"] },
        })),
        201,
    )
    .await;
    let api_id = string_member(&api, "id", "CreateRestApi");
    let root = string_member(&api, "rootResourceId", "CreateRestApi");
    let resource = control(
        &gateway,
        &signer,
        "CreateResource",
        "POST",
        &format!("/restapis/{api_id}/resources/{root}"),
        Some(json!({ "pathPart": RESOURCE })),
        201,
    )
    .await;
    let resource_id = string_member(&resource, "id", "CreateResource");
    let method = format!("/restapis/{api_id}/resources/{resource_id}/methods/GET");
    control(
        &gateway,
        &signer,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "AWS_IAM" })),
        201,
    )
    .await;
    control(
        &gateway,
        &signer,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({ "type": "MOCK" })),
        201,
    )
    .await;
    control(
        &gateway,
        &signer,
        "PutMethodResponse",
        "PUT",
        &format!("{method}/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    control(
        &gateway,
        &signer,
        "PutIntegrationResponse",
        "PUT",
        &format!("{method}/integration/responses/200"),
        Some(json!({ "responseTemplates": json_template(STAGE_BODY) })),
        201,
    )
    .await;
    control(
        &gateway,
        &signer,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api_id}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    let created = control(
        &gateway,
        &signer,
        "CreateDomainName",
        "POST",
        "/domainnames",
        Some(json!({
            "domainName": domain_name,
            "endpointConfiguration": { "types": ["REGIONAL"] },
            "regionalCertificateArn": crate::domain_name_collection::ACCOUNT_CERTIFICATE_ARN,
        })),
        201,
    )
    .await;
    let published = string_member(&created, "regionalDomainName", "CreateDomainName");
    control(
        &gateway,
        &signer,
        "CreateBasePathMapping",
        "POST",
        &format!("/domainnames/{domain_name}/basepathmappings"),
        Some(json!({ "basePath": "(none)", "restApiId": api_id, "stage": STAGE })),
        201,
    )
    .await;

    let path = format!("/{RESOURCE}");
    // The rows span what the rule generalises over: which credential signs, what it signed
    // over, which service its scope names, and a request carrying none at all.
    let rows: &[(&str, Credential, u16, &str)] = &[
        (
            "the key this emulator published, over the request that is sent",
            Credential::Published,
            200,
            STAGE_BODY,
        ),
        (
            "the right key id with another secret",
            Credential::OtherSecret,
            403,
            SIGNATURE_MISMATCH,
        ),
        (
            "the right key and secret, over another path than the one sent",
            Credential::OverAnotherPath,
            403,
            SIGNATURE_MISMATCH,
        ),
        (
            "the right key and secret, scoped to the control plane's signing name",
            Credential::Misscoped,
            403,
            MISSCOPED_CREDENTIAL,
        ),
        ("no credential at all", Credential::None, 403, MISSING_TOKEN),
    ];

    for (case, credential, expected, sentence) in rows {
        let request = match credential {
            Credential::Published => {
                signer.sign(plain_get(domain_name, &path), INVOKE_SIGNING_NAME)
            }
            Credential::OtherSecret => Signer {
                key_id: signer.key_id.clone(),
                secret: "0123456789abcdef0123456789abcdef01234567".to_string(),
            }
            .sign(plain_get(domain_name, &path), INVOKE_SIGNING_NAME),
            Credential::OverAnotherPath => {
                // Signed over another path of the same host, then sent to the path above:
                // the signature is a real one, over a request the client did not send.
                let signed = signer.sign(
                    plain_get(domain_name, "/elsewhere"),
                    INVOKE_SIGNING_NAME,
                );
                let (parts, body) = signed.into_parts();
                let mut request = http::Request::from_parts(parts, body);
                *request.uri_mut() = format!("https://{domain_name}{path}")
                    .parse()
                    .expect("the uri is a uri");
                request
            }
            Credential::Misscoped => signer.sign(plain_get(domain_name, &path), "apigateway"),
            Credential::None => plain_get(domain_name, &path),
        };
        let answered = exchange(case, &published, &request).await;
        let status = status_line(&answered);
        assert!(
            status.contains(&expected.to_string()),
            "case {case:?}: expected HTTP {expected}, got {answered:?}"
        );
        let body = body_of(&answered);
        assert!(
            body.contains(sentence),
            "case {case:?}: expected the answer to carry {sentence:?}, got {body:?}"
        );
        if *expected != 200 {
            assert!(
                !body.contains(STAGE_BODY),
                "case {case:?}: the integration must not have been performed, got {body:?}"
            );
        }
    }
}
