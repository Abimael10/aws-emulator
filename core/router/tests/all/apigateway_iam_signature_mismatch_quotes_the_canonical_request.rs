// SPDX-License-Identifier: CC0-1.0

//! What the `execute-api` data plane TELLS a caller whose signature does not match, driven end
//! to end on a router that verifies request signatures.
//!
//! The RULE the rows assert, one level above the recorded case:
//!
//! > A request to a deployed `AWS_IAM` method whose SigV4 signature is not the one the
//! > credential's secret produces is refused HTTP `403` with the exception class
//! > `InvalidSignatureException`, and its message is AWS's fixed sentence for that class
//! > FOLLOWED by the two values AWS quotes back — the canonical request and the string-to-sign
//! > the endpoint computed from the request AS RECEIVED. Both are functions of that request
//! > alone: the canonical request states the verb, the canonical URI (the stage-qualified path
//! > the caller addressed), the canonical query string, one line per header the request's own
//! > `SignedHeaders` element declares, that same signed-header list and the payload hash; the
//! > string-to-sign states the algorithm, the instant, the credential scope and the hash of
//! > that canonical request. Neither carries a secret, and the integration behind the method
//! > is not performed.
//!
//! That is the detail a SigV4 client diffs against what its own signer produced, which is why
//! the sentence alone is not the answer: without the canonical request a caller cannot tell a
//! wrong secret from a path, query or signed-header set its signer canonicalized differently.
//!
//! The rows span what the rule generalises over: WHICH credential signs (a temporary session
//! minted through `sts:AssumeRole`, carried with its session token, and the long-term
//! credential this process publishes), WHERE that credential travels (the `Authorization`
//! header, a presigned URL's query), the VERB of the refused request, WHICH path it addresses
//! (a resource of the stage, a nested resource, the stage root), WHETHER it carries a query
//! (none, and fields sent out of canonical order), WHETHER it carries a body (none, and a
//! JSON document the payload hash covers), and WHAT a header it signs carries (a plain value,
//! and one made of the characters significant in the envelope the refusal is serialized into).
//! Two positive controls — the same requests signed with each credential's REAL secret — serve
//! the integration, so every refusal row differs from a served request in the secret alone.
//!
//! Three services and one engine have to agree for that to hold, so this is a router suite:
//! IAM holds the role, the token service mints the session the request signs with and the
//! emulator's own verifier judges the signature, and API Gateway's `execute-api` data plane
//! words the refusal.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason `boot_serve.rs` does.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aws_credential_types::Credentials;
use aws_emulator_router::{build_router, InMemoryEngine};
use aws_sigv4::http_request::{
    sign, SignableBody, SignableRequest, SignatureLocation, SigningSettings,
};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is signed for, and so the Region of the api's invoke host.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to, which is the first segment of every canonical URI below.
const STAGE: &str = "test";

/// The account every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`), spelled out
/// so a wrong constant in the implementation cannot make this suite pass.
const ACCOUNT: &str = "000000000000";

/// The signing name an invoke of a deployed stage is signed under, which is the service the
/// credential scope of every row below names.
const INVOKE_SIGNING_NAME: &str = "execute-api";

/// The algorithm a SigV4 string-to-sign opens with.
const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// The awsQuery API versions the setup services are called at.
const IAM_VERSION: &str = "2010-05-08";
const STS_VERSION: &str = "2011-06-15";

/// The body the `MOCK` integration behind every method answers a served request with.
const SERVED_BODY: &str = r#"{"served":"ok"}"#;

/// The fixed sentence AWS refuses a signature that is not the one the credential's secret
/// produces with, which the message OPENS with.
const SIGNATURE_MISMATCH: &str = "The request signature we calculated does not match the \
                                  signature you provided. Check your AWS Secret Access Key \
                                  and signing method. Consult the service documentation for \
                                  details.";

/// The lines AWS introduces each quoted value with.
const CANONICAL_PREAMBLE: &str = "The Canonical String for this request should have been";
const STRING_TO_SIGN_PREAMBLE: &str = "The String-to-Sign should have been";

/// The header an SDK reads the error class of a refused request out of, and the class this
/// refusal carries.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";
const INVALID_SIGNATURE: &str = "InvalidSignatureException";

/// The query field a presigned URL carries its signature in, which is the one field SigV4
/// leaves out of the canonical query string — it is the value the signature IS, not part of
/// what it covers.
const SIGNATURE_FIELD: &str = "X-Amz-Signature";

/// One request dispatched through the router, and what came back.
struct Answer {
    status: u16,
    error_type: Option<String>,
    text: String,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or(Value::Null)
    }

    /// The `message` member every gateway response carries, or the whole body.
    fn message(&self) -> String {
        self.json()
            .get("message")
            .and_then(Value::as_str)
            .map_or_else(|| self.text.clone(), str::to_owned)
    }

    fn summary(&self) -> String {
        format!("{} {:?} {}", self.status, self.error_type, self.text)
    }
}

/// Dispatch one request through the router.
async fn dispatch(router: &MultiServiceRouter, request: http::Request<Vec<u8>>) -> Answer {
    let (parts, body) = request.into_parts();
    let request = http::Request::from_parts(parts, bytes::Bytes::from(body));
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get(ERROR_TYPE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let text = String::from_utf8_lossy(&response.into_body()).to_string();
    Answer {
        status,
        error_type,
        text,
    }
}

/// The credential scope a signed SDK request carries, with no real signature: what the SETUP
/// calls use, while request authentication is off, so the router can name the target service.
fn credential_scope(signing_name: &str) -> String {
    format!(
        "{ALGORITHM} Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One awsQuery form call to `signing_name`, which must answer `200`.
async fn query(router: &MultiServiceRouter, what: &str, signing_name: &str, form: String) -> String {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", credential_scope(signing_name))
        .body(form.into_bytes())
        .expect("the test request is well formed");
    let answer = dispatch(router, request).await;
    assert_eq!(
        answer.status,
        200,
        "setup {what}: must answer HTTP 200, got {}",
        answer.summary()
    );
    answer.text
}

/// One API Gateway REST control-plane call, which must answer `expected_status`.
async fn control_plane(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected_status: u16,
) -> Value {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", credential_scope("apigateway"))
        .body(
            body.map(|body| body.to_string().into_bytes())
                .unwrap_or_default(),
        )
        .expect("the test request is well formed");
    let answer = dispatch(router, request).await;
    assert_eq!(
        answer.status,
        expected_status,
        "setup {what}: must answer HTTP {expected_status}, got {}",
        answer.summary()
    );
    answer.json()
}

/// A required string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what} must carry a non-empty {member:?}, got {body}"))
}

/// Put one `AWS_IAM` method of `verb` on `resource`, behind a `MOCK` integration answering
/// [`SERVED_BODY`].
async fn iam_method(router: &MultiServiceRouter, api: &str, resource: &str, verb: &str) {
    let base = format!("/restapis/{api}/resources/{resource}/methods/{verb}");
    control_plane(
        router,
        "PutMethod",
        "PUT",
        &base,
        Some(json!({ "authorizationType": "AWS_IAM" })),
        201,
    )
    .await;
    control_plane(
        router,
        "PutIntegration",
        "PUT",
        &format!("{base}/integration"),
        Some(json!({ "type": "MOCK" })),
        201,
    )
    .await;
    control_plane(
        router,
        "PutMethodResponse",
        "PUT",
        &format!("{base}/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    control_plane(
        router,
        "PutIntegrationResponse",
        "PUT",
        &format!("{base}/integration/responses/200"),
        Some(json!({ "responseTemplates": { "application/json": SERVED_BODY } })),
        201,
    )
    .await;
}

/// One credential a row signs with, as a client holds it.
#[derive(Clone)]
struct Credential {
    key_id: String,
    secret: String,
    /// The session token a temporary credential is served with, which such a credential is
    /// only honoured together with.
    token: Option<String>,
}

/// A secret of the same length as `secret` and different from it, which is what "signed with
/// the wrong secret" means: nothing of the request changes but the key the signature is
/// computed with.
fn wrong_secret(secret: &str) -> String {
    secret
        .chars()
        .map(|character| if character == 'a' { 'b' } else { 'a' })
        .collect()
}

/// Where a row's credential travels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Where {
    /// The `Authorization` header a signed SDK request carries.
    Header,
    /// The query a presigned URL carries its credential in.
    PresignedQuery,
}

/// Sign `request` with `credential` for the invoke signing name, in `place`.
fn signed(
    mut request: http::Request<Vec<u8>>,
    credential: &Credential,
    secret: &str,
    place: Where,
) -> http::Request<Vec<u8>> {
    let identity = Credentials::new(
        &credential.key_id,
        secret,
        credential.token.clone(),
        None,
        "test",
    )
    .into();
    let mut settings = SigningSettings::default();
    if place == Where::PresignedQuery {
        settings.signature_location = SignatureLocation::QueryParams;
        settings.expires_in = Some(Duration::from_secs(300));
    }
    let params = SigningParams::builder()
        .identity(&identity)
        .region(REGION)
        .name(INVOKE_SIGNING_NAME)
        .time(SystemTime::now())
        .settings(settings)
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

/// The value of one field of a request's query, with the two escapes a SigV4 field carries
/// decoded.
fn query_field(uri: &str, field: &str) -> Option<String> {
    uri.split_once('?')?
        .1
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == field)
        .map(|(_, value)| value.replace("%3B", ";").replace("%2F", "/"))
}

/// The value of one element of a request's `Authorization` header.
fn authorization_element(request: &http::Request<Vec<u8>>, element: &str) -> Option<String> {
    request
        .headers()
        .get("authorization")?
        .to_str()
        .ok()?
        .split_once(element)?
        .1
        .split([',', ' '])
        .next()
        .map(str::to_string)
}

/// The headers the request's OWN signature declares it covers, read where it carries them.
fn signed_headers_of(request: &http::Request<Vec<u8>>) -> String {
    authorization_element(request, "SignedHeaders=")
        .or_else(|| query_field(&request.uri().to_string(), "X-Amz-SignedHeaders"))
        .unwrap_or_else(|| panic!("the signed request declares the headers it covers"))
}

/// The instant the request's own signature names, read where it carries it.
fn instant_of(request: &http::Request<Vec<u8>>) -> String {
    request
        .headers()
        .get("x-amz-date")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .or_else(|| query_field(&request.uri().to_string(), "X-Amz-Date"))
        .unwrap_or_else(|| panic!("the signed request names the instant it was signed at"))
}

/// The canonical query string of `uri`, to SigV4's rule: every field the request carries,
/// sorted by name and then value, with [`SIGNATURE_FIELD`] left out.
fn canonical_query(uri: &str) -> String {
    let Some((_, query)) = uri.split_once('?') else {
        return String::new();
    };
    let mut fields: Vec<(String, String)> = query
        .split('&')
        .filter(|field| !field.is_empty())
        .map(|field| match field.split_once('=') {
            Some((name, value)) => (name.to_string(), value.to_string()),
            None => (field.to_string(), String::new()),
        })
        .filter(|(name, _)| name != SIGNATURE_FIELD)
        .collect();
    fields.sort();
    fields
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<String>>()
        .join("&")
}

/// The two values the refusal quotes, read off the WHOLE delimiter each one is written with:
/// its preamble, then the opening quote, and — for the first — the next preamble behind the
/// closing one. So neither read depends on what the quoted text itself contains: a path,
/// query or header value carrying a quote character cannot cut either value short.
fn quoted_values(case: &str, message: &str) -> (String, String) {
    let opener = format!("{CANONICAL_PREAMBLE}\n'");
    let between = format!("'\n\n{STRING_TO_SIGN_PREAMBLE}\n'");
    let after = message
        .split_once(&opener)
        .unwrap_or_else(|| panic!("case {case:?}: expected {opener:?} in {message:?}"))
        .1;
    let (canonical, rest) = after
        .split_once(&between)
        .unwrap_or_else(|| panic!("case {case:?}: expected {between:?} in {message:?}"));
    let string_to_sign = rest
        .strip_suffix("'\n")
        .or_else(|| rest.strip_suffix('\''))
        .unwrap_or_else(|| panic!("case {case:?}: expected a closed quote in {message:?}"));
    (canonical.to_string(), string_to_sign.to_string())
}

/// Which credential a row signs with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// A temporary session minted through `sts:AssumeRole`, carried with its session token:
    /// the credential the recorded case signs with.
    Session,
    /// The long-term credential this process publishes once authentication is on.
    Account,
}

/// What a row expects of its secret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Secret {
    /// The credential's own: the request is served.
    Right,
    /// A same-length other one: the signature cannot match, whatever else the request says.
    Wrong,
}

/// One signed invoke of the deployed stage.
struct Row {
    case: &'static str,
    kind: Kind,
    secret: Secret,
    place: Where,
    verb: &'static str,
    /// The resource path under the stage, empty for the stage root.
    path: &'static str,
    /// The query the request carries, as the client writes it.
    query: Option<&'static str>,
    body: &'static str,
    /// The canonical query string this row pins literally, where the rule reorders what the
    /// client sent.
    canonical_query: Option<&'static str>,
    /// A header of the caller's own, signed with the rest — so the canonical request carries
    /// its value, and the refusal's envelope has to hold whatever that value is.
    extra_header: Option<(&'static str, &'static str)>,
}

impl Row {
    /// The stage-qualified path this row addresses, which is the canonical URI of its request.
    fn canonical_uri(&self) -> String {
        if self.path.is_empty() {
            format!("/{STAGE}")
        } else {
            format!("/{STAGE}/{}", self.path)
        }
    }
}

/// A header value carrying the characters that are significant in the envelope the refusal
/// is serialized into — the quote and the backslash a JSON string escapes, and the three
/// characters an XML serializer escapes — all of them legal in an HTTP header value. The
/// canonical request states a line per signed header, so this value is reflected into the
/// error body, which must still parse as the document its content type names.
const ENVELOPE_PROBE: (&str, &str) = (
    "x-envelope-probe",
    r#"{"quote":"\"","backslash":"\\","markup":"<a>&</a>"}"#,
);

/// The rows, spanning what the rule generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a session credential signed with another secret",
            kind: Kind::Session,
            secret: Secret::Wrong,
            place: Where::Header,
            verb: "GET",
            path: "widgets",
            query: None,
            body: "",
            canonical_query: Some(""),
            extra_header: None,
        },
        Row {
            case: "the same, on a nested resource path",
            kind: Kind::Session,
            secret: Secret::Wrong,
            place: Where::Header,
            verb: "GET",
            path: "widgets/items",
            query: None,
            body: "",
            canonical_query: Some(""),
            extra_header: None,
        },
        Row {
            case: "the same, with query fields sent out of canonical order",
            kind: Kind::Session,
            secret: Secret::Wrong,
            place: Where::Header,
            verb: "GET",
            path: "widgets",
            query: Some("page=2&after=a"),
            body: "",
            canonical_query: Some("after=a&page=2"),
            extra_header: None,
        },
        Row {
            case: "the account's own credential signed with another secret",
            kind: Kind::Account,
            secret: Secret::Wrong,
            place: Where::Header,
            verb: "GET",
            path: "widgets",
            query: None,
            body: "",
            canonical_query: Some(""),
            extra_header: None,
        },
        Row {
            case: "a verb carrying a body the payload hash covers",
            kind: Kind::Account,
            secret: Secret::Wrong,
            place: Where::Header,
            verb: "POST",
            path: "widgets",
            query: None,
            body: r#"{"n":1}"#,
            canonical_query: Some(""),
            extra_header: None,
        },
        Row {
            case: "the stage root",
            kind: Kind::Account,
            secret: Secret::Wrong,
            place: Where::Header,
            verb: "GET",
            path: "",
            query: None,
            body: "",
            canonical_query: Some(""),
            extra_header: None,
        },
        Row {
            case: "a presigned URL carrying the credential in its query",
            kind: Kind::Account,
            secret: Secret::Wrong,
            place: Where::PresignedQuery,
            verb: "GET",
            path: "widgets",
            query: None,
            body: "",
            canonical_query: None,
            extra_header: None,
        },
        Row {
            case: "a signed header whose value carries the envelope's own metacharacters",
            kind: Kind::Account,
            secret: Secret::Wrong,
            place: Where::Header,
            verb: "GET",
            path: "widgets",
            query: None,
            body: "",
            canonical_query: Some(""),
            extra_header: Some(ENVELOPE_PROBE),
        },
        Row {
            case: "the session credential's own secret",
            kind: Kind::Session,
            secret: Secret::Right,
            place: Where::Header,
            verb: "GET",
            path: "widgets",
            query: None,
            body: "",
            canonical_query: None,
            extra_header: None,
        },
        Row {
            case: "the account credential's own secret",
            kind: Kind::Account,
            secret: Secret::Right,
            place: Where::Header,
            verb: "GET",
            path: "widgets",
            query: None,
            body: "",
            canonical_query: None,
            extra_header: None,
        },
    ]
}

#[test]
fn a_signature_mismatch_quotes_the_canonical_request_of_the_refused_request() {
    std::thread::Builder::new()
        .name("apigateway-iam-signature-mismatch".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                // The IO and TIME drivers a service's background work needs; see the sibling
                // `apigateway_private_vpce_invoke` suite.
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);

                // SETUP, with authentication off so the control-plane calls need no real
                // signature: one api whose root, one resource and one nested resource all
                // carry `AWS_IAM` methods behind `MOCK` integrations, deployed to one stage.
                router.set_request_authentication(false);
                let api = control_plane(
                    &router,
                    "CreateRestApi",
                    "POST",
                    "/restapis",
                    Some(json!({ "name": "iam-signature-mismatch-api" })),
                    201,
                )
                .await;
                let api_id = as_str("CreateRestApi", &api, "id");
                let root = as_str("CreateRestApi", &api, "rootResourceId");
                iam_method(&router, &api_id, &root, "GET").await;
                let widgets = control_plane(
                    &router,
                    "CreateResource",
                    "POST",
                    &format!("/restapis/{api_id}/resources/{root}"),
                    Some(json!({ "pathPart": "widgets" })),
                    201,
                )
                .await;
                let widgets = as_str("CreateResource", &widgets, "id");
                iam_method(&router, &api_id, &widgets, "GET").await;
                iam_method(&router, &api_id, &widgets, "POST").await;
                let items = control_plane(
                    &router,
                    "CreateResource",
                    "POST",
                    &format!("/restapis/{api_id}/resources/{widgets}"),
                    Some(json!({ "pathPart": "items" })),
                    201,
                )
                .await;
                let items = as_str("CreateResource", &items, "id");
                iam_method(&router, &api_id, &items, "GET").await;
                control_plane(
                    &router,
                    "CreateDeployment",
                    "POST",
                    &format!("/restapis/{api_id}/deployments"),
                    Some(json!({ "stageName": STAGE })),
                    201,
                )
                .await;

                // The role a client assumes, granted the invoke, and one session of it: the
                // temporary credential — an access key id, a secret and a session token — the
                // recorded case signs with.
                let trust = json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
                        "Action": "sts:AssumeRole"
                    }]
                })
                .to_string();
                let created = query(
                    &router,
                    "iam CreateRole",
                    "iam",
                    format!(
                        "Action=CreateRole&Version={IAM_VERSION}&RoleName=invoke-widgets\
                         &AssumeRolePolicyDocument={}",
                        form_encode(&trust)
                    ),
                )
                .await;
                let role_arn = element(&created, "Arn");
                let grant = json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": "execute-api:Invoke",
                        "Resource": "*"
                    }]
                })
                .to_string();
                query(
                    &router,
                    "iam PutRolePolicy",
                    "iam",
                    format!(
                        "Action=PutRolePolicy&Version={IAM_VERSION}&RoleName=invoke-widgets\
                         &PolicyName=invoke&PolicyDocument={}",
                        form_encode(&grant)
                    ),
                )
                .await;
                let assumed = query(
                    &router,
                    "sts AssumeRole",
                    "sts",
                    format!(
                        "Action=AssumeRole&Version={STS_VERSION}&RoleArn={}\
                         &RoleSessionName=widgets-client",
                        form_encode(&role_arn)
                    ),
                )
                .await;
                let session = Credential {
                    key_id: element(&assumed, "AccessKeyId"),
                    secret: element(&assumed, "SecretAccessKey"),
                    token: Some(element(&assumed, "SessionToken")),
                };

                // From here on the SAME router verifies request signatures, as the emulator
                // does when it is started with authentication on.
                router.set_request_authentication(true);
                let published = ::emulator_server::SigV4Authenticator::minted_credentials()
                    .expect("turning authentication on publishes this process's keys");
                let account = Credential {
                    key_id: published.access_key_id.clone(),
                    secret: published.secret_access_key.clone(),
                    token: None,
                };

                let host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
                for row in rows() {
                    let credential = match row.kind {
                        Kind::Session => &session,
                        Kind::Account => &account,
                    };
                    let signing_secret = match row.secret {
                        Secret::Right => credential.secret.clone(),
                        Secret::Wrong => wrong_secret(&credential.secret),
                    };
                    let target = match row.query {
                        Some(query) => format!("{}?{query}", row.canonical_uri()),
                        None => row.canonical_uri(),
                    };
                    let mut builder = http::Request::builder()
                        .method(row.verb)
                        .uri(format!("https://{host}{target}"))
                        .header("host", &host);
                    if !row.body.is_empty() {
                        builder = builder.header("content-type", "application/json");
                    }
                    if let Some((name, value)) = row.extra_header {
                        builder = builder.header(name, value);
                    }
                    let built = builder
                        .body(row.body.as_bytes().to_vec())
                        .expect("the test request is well formed");
                    let request = signed(built, credential, &signing_secret, row.place);
                    let declared = signed_headers_of(&request);
                    let instant = instant_of(&request);
                    let sent_uri = request.uri().to_string();
                    let answer = dispatch(&router, request).await;

                    if row.secret == Secret::Right {
                        // The positive control: the same request, signed with the credential's
                        // own secret, is served — so every refusal below differs from a served
                        // request in the secret alone.
                        assert_eq!(
                            (answer.status, answer.message().as_str()),
                            (200, SERVED_BODY),
                            "case {:?}: expected the integration's own answer, got {}",
                            row.case,
                            answer.summary()
                        );
                        continue;
                    }

                    assert_eq!(
                        (answer.status, answer.error_type.as_deref()),
                        (403, Some(INVALID_SIGNATURE)),
                        "case {:?}: expected the stage's 403 for a signature mismatch, got {}",
                        row.case,
                        answer.summary()
                    );
                    let message = answer.message();
                    assert!(
                        message.starts_with(SIGNATURE_MISMATCH),
                        "case {:?}: expected AWS's sentence for the class first, got {}",
                        row.case,
                        answer.summary()
                    );
                    assert!(
                        !answer.text.contains(SERVED_BODY),
                        "case {:?}: the integration must not have been performed, got {}",
                        row.case,
                        answer.summary()
                    );

                    // The canonical request, line by line: the verb, the canonical URI the
                    // caller addressed, the canonical query string, then (past the header
                    // lines) the signed-header list the request itself declared and the
                    // payload hash of what it sent.
                    let (canonical, string_to_sign) = quoted_values(row.case, &message);
                    let lines: Vec<&str> = canonical.split('\n').collect();
                    assert!(
                        lines.len() >= 6,
                        "case {:?}: expected a canonical request of SigV4's own shape, got \
                         {canonical:?}",
                        row.case
                    );
                    assert_eq!(
                        lines[0], row.verb,
                        "case {:?}: expected the verb first, got {canonical:?}",
                        row.case
                    );
                    assert_eq!(
                        lines[1],
                        row.canonical_uri(),
                        "case {:?}: expected the stage-qualified path it addressed, got \
                         {canonical:?}",
                        row.case
                    );
                    let expected_query = canonical_query(&sent_uri);
                    assert_eq!(
                        lines[2], expected_query,
                        "case {:?}: expected the canonical query of the request, got \
                         {canonical:?}",
                        row.case
                    );
                    if let Some(pinned) = row.canonical_query {
                        assert_eq!(
                            lines[2], pinned,
                            "case {:?}: expected the canonical query {pinned:?}, got \
                             {canonical:?}",
                            row.case
                        );
                    }
                    if row.place == Where::PresignedQuery {
                        // A presigned URL's own fields are part of what its signature covers,
                        // so the canonical query opens with them, in canonical order, as the
                        // URL spelled them.
                        assert!(
                            lines[2].starts_with(&format!(
                                "X-Amz-Algorithm={ALGORITHM}&X-Amz-Credential="
                            )) && lines[2].contains("&X-Amz-SignedHeaders="),
                            "case {:?}: expected the presigned fields the signature covers, \
                             got {canonical:?}",
                            row.case
                        );
                    }
                    assert!(
                        !lines[2].contains(SIGNATURE_FIELD),
                        "case {:?}: the signature is not part of what it covers, got \
                         {canonical:?}",
                        row.case
                    );
                    assert_eq!(
                        lines[lines.len() - 2],
                        declared,
                        "case {:?}: expected the signed-header list the request declared, got \
                         {canonical:?}",
                        row.case
                    );
                    assert_eq!(
                        lines[lines.len() - 1],
                        hex::encode(Sha256::digest(row.body.as_bytes())),
                        "case {:?}: expected the payload hash of the body it sent, got \
                         {canonical:?}",
                        row.case
                    );
                    for name in declared.split(';') {
                        assert!(
                            canonical.contains(&format!("\n{name}:")),
                            "case {:?}: expected a line for the signed header {name:?}, got \
                             {canonical:?}",
                            row.case
                        );
                    }

                    // A header value of the caller's own is reflected into the canonical
                    // request, and so into the refusal's envelope: the body still has to
                    // parse as the document its content type names, whatever characters that
                    // value carries.
                    if let Some((name, value)) = row.extra_header {
                        assert!(
                            answer.json().get("message").is_some(),
                            "case {:?}: expected a body that still parses as its envelope, got \
                             {}",
                            row.case,
                            answer.summary()
                        );
                        assert!(
                            canonical.contains(&format!("\n{name}:{value}\n")),
                            "case {:?}: expected the header value it signed, got {canonical:?}",
                            row.case
                        );
                    }

                    // The string-to-sign, which is a function of that canonical request and of
                    // the scope and instant the request named.
                    let expected = format!(
                        "{ALGORITHM}\n{instant}\n{date}/{REGION}/{INVOKE_SIGNING_NAME}/\
                         aws4_request\n{hash}",
                        date = &instant[..8],
                        hash = hex::encode(Sha256::digest(canonical.as_bytes())),
                    );
                    assert_eq!(
                        string_to_sign, expected,
                        "case {:?}: expected the string-to-sign of that canonical request, got \
                         {string_to_sign:?}",
                        row.case
                    );

                    // Neither quoted value is derived from a secret, so no secret of the
                    // credential can appear in the refusal.
                    for (what, secret) in [
                        ("the credential's own secret", credential.secret.as_str()),
                        ("the secret the request signed with", signing_secret.as_str()),
                    ] {
                        assert!(
                            !answer.text.contains(secret),
                            "case {:?}: {what} must not appear in the refusal, got {}",
                            row.case,
                            answer.summary()
                        );
                    }
                }
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}
