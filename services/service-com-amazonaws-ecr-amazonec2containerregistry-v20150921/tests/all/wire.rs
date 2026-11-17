// SPDX-License-Identifier: CC0-1.0

//! The wire harness every suite in this crate shares: one emulator, and one call on the
//! awsJson1_1 protocol this service speaks.
//!
//! It lives in its own module so a new suite does not copy it: a test that exercises an
//! operation goes through the router here, so it covers routing, deserialization, the
//! handler, the persisted state and serialization — the same path a client takes.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;
use service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The access key id every call made through [`call`] presents: a key neither STS nor IAM
/// holds a record for, which speaks for the account's own published credential. Its
/// credential scope is what routes a request to ECR, and that scope's region is what a
/// repository ARN is built for.
pub const ACCOUNT_ACCESS_KEY_ID: &str = "AKIDEXAMPLE";

/// The region every call made through [`call`] and [`call_as`] is signed for, which a
/// repository ARN is built for.
pub const REGION: &str = "eu-west-1";

/// The endpoint host every call made through [`call`], [`call_as`] and [`call_in_region`]
/// is addressed to, for the Region it names.
pub fn endpoint_host(region: &str) -> String {
    format!("api.ecr.{region}.amazonaws.com")
}

/// The base URL this harness's emulator is SERVED at, which is what
/// `smithy_server_runtime::OperationContext::endpoint` reports and therefore the address a
/// registry client of these suites is handed to dial. A loopback socket, because that is the
/// only kind of address a client on the machine an emulator runs on can reach.
pub const SERVED_URL_BASE: &str = "https://127.0.0.1:9297";

/// The zone and port every registry address this harness's emulator ANNOUNCES is named in.
///
/// Its calls are addressed to [`endpoint_host`], which names a service and is therefore an
/// endpoint no registry client can use, so the address the registry is announced on is built
/// on the one this emulator is served at ([`SERVED_URL_BASE`]) — a loopback ADDRESS, and the
/// zone a resolver answers with that interface is the loopback zone RFC 6761 reserves
/// together with every name under it, on the port the emulator is served on.
pub const ANNOUNCED_REGISTRY_ZONE: &str = "localhost:9297";

/// The host this emulator announces this account's registry in `region` at: the owning
/// account's id, the two fixed labels of a registry host, the registry's own Region and the
/// zone this emulator answers in ([`ANNOUNCED_REGISTRY_ZONE`]) — the layout
/// `AuthorizationData$proxyEndpoint` documents ("The Amazon ECR registry URL format is
/// `https://aws_account_id.dkr.ecr.region.amazonaws.com`"), in the zone a client of these
/// suites can reach.
pub fn announced_registry_host(region: &str) -> String {
    format!("{REGISTRY_ID}.dkr.ecr.{region}.{ANNOUNCED_REGISTRY_ZONE}")
}

/// The registry URL a `docker login` of a call signed for `region` is pointed at
/// (`AuthorizationData$proxyEndpoint`): a scheme followed by that registry's own host
/// ([`announced_registry_host`]).
pub fn announced_registry_url(region: &str) -> String {
    format!("https://{}", announced_registry_host(region))
}

/// The account id every generated emulator vends, spelled out here rather than imported
/// so a wrong registry id in the implementation fails these tests.
pub const REGISTRY_ID: &str = "000000000000";

/// The URI Amazon ECR reports for the repository `name` of the registry in `region`
/// (`Repository$repositoryUri`: "The URI for the repository. You can use this URI for
/// container image `push` and `pull` operations"): the host of the registry that HOLDS it
/// ([`announced_registry_host`]), followed by the repository's own name.
pub fn repository_uri_in(region: &str, name: &str) -> String {
    format!("{}/{name}", announced_registry_host(region))
}

/// [`repository_uri_in`] for a repository of the Region these suites sign for
/// ([`REGION`]).
pub fn repository_uri(name: &str) -> String {
    repository_uri_in(REGION, name)
}

/// The `X-Amz-Target` prefix this service routes on (`OPERATIONS.md`, "Wire protocol").
const TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// One emulator: a router carrying only this service, over one shared engine.
pub fn emulator() -> MultiServiceRouter {
    emulator_with_engine().0
}

/// The same emulator as [`emulator`], plus the engine its handlers read and write — for
/// a case that has to arrange state no reasonable number of wire calls would.
pub fn emulator_with_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    // The emulator is SERVED somewhere ([`SERVED_URL_BASE`]), which is the address every
    // registry address this service announces is built on.
    router.set_url_base(
        ::smithy_server_runtime::EndpointUrl::parse(SERVED_URL_BASE)
            .unwrap_or_else(|| panic!("the base URL {SERVED_URL_BASE} must parse")),
    );
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// What the emulator answered: the HTTP status and the JSON body.
pub struct Answer {
    pub status: u16,
    pub body: Value,
}

impl Answer {
    /// The wire error code (`__type`), or the empty string on a success.
    pub fn error_code(&self) -> &str {
        self.body["__type"].as_str().unwrap_or_default()
    }

    /// The error message, or the empty string when the answer carries none.
    pub fn message(&self) -> &str {
        self.body["message"].as_str().unwrap_or_default()
    }
}

/// Call one operation of this service on the awsJson1_1 wire: `POST /` with
/// `X-Amz-Target: <prefix>.<operation>` and a JSON body.
pub async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answer {
    call_as(router, ACCOUNT_ACCESS_KEY_ID, operation, body).await
}

/// The same call as [`call`], presenting `access_key_id` in the SigV4 credential scope —
/// what a suite that exercises WHO a request speaks for varies, since the access key id is
/// where the calling principal is resolved from.
pub async fn call_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> Answer {
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", endpoint_host(REGION))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/ecr/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

/// A successful call, or a panic naming the case and what came back instead.
pub async fn ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let answer = call(router, operation, body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// The value at a JSON pointer, or `null` when the answer carries nothing there — so an
/// assertion reports the member that is missing rather than panicking on the lookup.
pub fn at(value: &Value, pointer: &str) -> Value {
    value.pointer(pointer).cloned().unwrap_or(Value::Null)
}

/// How long [`ok_until`] keeps looking before it gives up.
///
/// Generous on purpose: a clock-owed transition is due on the emulator's own clock, and
/// this only bounds a wait that would otherwise never end, so neither a slow nor a loaded
/// host decides the outcome of a poll.
pub const POLL_DEADLINE: ::std::time::Duration = ::std::time::Duration::from_secs(60);

/// How long [`ok_until`] waits between two looks.
pub const POLL_INTERVAL: ::std::time::Duration = ::std::time::Duration::from_millis(10);

/// Call `operation` again and again until its answer satisfies `settled`, and answer the
/// first answer that does.
///
/// A suite waiting for a clock-owed transition waits for the CONDITION rather than for a
/// span of wall clock: it looks again until the transition has happened, bounded only by
/// [`POLL_DEADLINE`].
pub async fn ok_until(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
    settled: impl Fn(&Value) -> bool,
) -> Value {
    let since = ::std::time::Instant::now();
    loop {
        let answer = ok(router, case, operation, body).await;
        if settled(&answer) {
            return answer;
        }
        assert!(
            since.elapsed() < POLL_DEADLINE,
            "case {case:?}: {operation} never reached the state this poll waits for; \
             its last answer was {answer:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The same poll as [`ok_until`], made against `region` and over the WHOLE answer — the
/// status included — so a suite can wait for a call that is refused until the state it
/// waits for exists.
///
/// The one poll loop of this harness, parameterised: [`ok_until`] waits for a successful
/// answer to say something, and this waits for an answer at all, which is what a read of a
/// resource another Region's registry is still receiving needs.
pub async fn call_until_in_region(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    operation: &str,
    body: &Value,
    settled: impl Fn(&Answer) -> bool,
) -> Answer {
    let since = ::std::time::Instant::now();
    loop {
        let answer = call_in_region(router, region, operation, body).await;
        if settled(&answer) {
            return answer;
        }
        assert!(
            since.elapsed() < POLL_DEADLINE,
            "case {case:?}: {operation} in {region} never answered what this poll waits \
             for; its last answer was HTTP {} with {:?}",
            answer.status,
            answer.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Follow a URL this emulator handed out exactly as a client follows a pre-signed one — the
/// address it names, no credential of any kind — and answer the status and the bytes served.
///
/// The address is taken apart here rather than in a suite: a URL this emulator announces
/// carries the endpoint the caller reached it on, so the authority to send and the path to
/// request both come out of the URL itself.
pub async fn fetch_url(router: &MultiServiceRouter, case: &str, url: &str) -> (u16, Vec<u8>) {
    let (authority, path) = url
        .strip_prefix("https://")
        .and_then(|rest| rest.split_once('/'))
        .unwrap_or_else(|| panic!("case {case:?}: {url:?} is not an https URL with a path"));
    let request = ::http::Request::builder()
        .method("GET")
        .uri(format!("/{path}"))
        .header("host", authority)
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, response.into_body().to_vec())
}

/// A refused call: HTTP 400 with the wire error code `code`, or a panic naming the case
/// and what came back instead. The refusal's body is returned for a caller that wants to
/// look at its message.
pub async fn refused(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
    code: &str,
) -> Value {
    let answer = call(router, operation, body).await;
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400 from {operation}, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        code,
        "case {case:?}: expected {code} from {operation}, got {:?}",
        answer.body
    );
    answer.body
}

/// The content digest of `content`: the `sha256:<hex>` form a registry addresses a blob
/// by, computed here independently of the crate's own helper.
pub fn layer_digest(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex::encode(Sha256::digest(content)))
}

/// Put `content` into `repository` as a layer, through the documented push sequence —
/// `InitiateLayerUpload`, `UploadLayerPart`, `CompleteLayerUpload` — and answer the
/// digest the registry registered it under, which is the digest a manifest references it
/// by. A manifest cannot be pushed before its blobs are in the repository, so a suite
/// that pushes one seeds its blobs through here.
pub async fn given_layer(
    router: &MultiServiceRouter,
    case: &str,
    repository: &str,
    content: &[u8],
) -> String {
    given_layer_in_region(router, case, REGION, repository, content).await
}

/// The same push sequence as [`given_layer`], made against `region`: a repository lives in
/// the registry of ONE Region, so a suite that created it in another Region pushes its
/// layers there too.
pub async fn given_layer_in_region(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    repository: &str,
    content: &[u8],
) -> String {
    let opened = ok_in_region(
        router,
        case,
        region,
        "InitiateLayerUpload",
        &serde_json::json!({"repositoryName": repository}),
    )
    .await;
    let upload_id = opened["uploadId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: InitiateLayerUpload must hand back an uploadId"))
        .to_string();
    let last = i64::try_from(content.len()).expect("a test layer fits in an i64") - 1;
    ok_in_region(
        router,
        case,
        region,
        "UploadLayerPart",
        &serde_json::json!({
            "repositoryName": repository,
            "uploadId": upload_id,
            "partFirstByte": 0,
            "partLastByte": last,
            // A blob travels base64-encoded on the awsJson1_1 wire, with the codec the
            // wire serializer itself uses.
            "layerPartBlob": aws_smithy_types::base64::encode(content),
        }),
    )
    .await;
    let completed = ok_in_region(
        router,
        case,
        region,
        "CompleteLayerUpload",
        &serde_json::json!({
            "repositoryName": repository,
            "uploadId": upload_id,
            "layerDigests": [layer_digest(content)],
        }),
    )
    .await;
    completed["layerDigest"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: CompleteLayerUpload must report the registered layerDigest")
        })
        .to_string()
}

/// Seed one pull through cache rule straight into the registry's store, under `prefix`
/// as its key.
///
/// `CreatePullThroughCacheRule` is the operation that creates one, and it refuses an
/// upstream URL it cannot resolve to a supported registry while the request boundary
/// refuses a `credentialArn` outside its modelled pattern — so a suite that has to
/// exercise a STORED record the create path cannot produce places it here, in the same
/// store the handlers read. Every other case goes through the wire.
pub fn seed_pull_through_cache_rule(
    engine: &engine::InMemoryEngine,
    prefix: &str,
    upstream_registry: Option<&str>,
    upstream_url: &str,
    credential_arn: Option<&str>,
    custom_role_arn: Option<&str>,
) {
    use service_com_amazonaws_ecr_amazonec2containerregistry_v20150921 as ecr;

    let state = ecr::state::service_state(engine).expect("the ecr store is reachable");
    state.lock().pull_through_cache_rules.insert(
        prefix.to_string(),
        ecr::ops::types::PullThroughCacheRule {
            ecr_repository_prefix: Some(prefix.to_string()),
            upstream_registry_url: Some(upstream_url.to_string()),
            created_at: Some(::aws_smithy_types::DateTime::from(
                ::std::time::SystemTime::now(),
            )),
            registry_id: Some(REGISTRY_ID.to_string()),
            credential_arn: credential_arn.map(ToString::to_string),
            custom_role_arn: custom_role_arn.map(ToString::to_string),
            upstream_repository_prefix: Some("ROOT".to_string()),
            upstream_registry: upstream_registry.map(ToString::to_string),
            updated_at: None,
        },
    );
}

/// The same call as [`call`], made against `region`: the credential scope's region is
/// the Region a request is made against, so a suite exercising regional behaviour picks
/// it per call instead of taking [`REGION`].
pub async fn call_in_region(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> Answer {
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", endpoint_host(region))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/ecr/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

/// A successful [`call_in_region`], or a panic naming the case and what came back.
pub async fn ok_in_region(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let answer = call_in_region(router, region, operation, body).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}
