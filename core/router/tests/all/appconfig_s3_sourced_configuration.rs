// SPDX-License-Identifier: CC0-1.0

//! A configuration profile whose configuration lives in an Amazon S3 object, across the one
//! emulator.
//!
//! RULE: a configuration profile's `LocationUri` says which store its configuration lives in,
//! and `s3://<bucket>/<objectKey>` names an Amazon S3 object (`smithy_query(query="doc",
//! shape="CreateConfigurationProfileRequest", member="LocationUri")`); a deployment of such a
//! profile names a version of THAT store ("for all other configurations, you must specify the
//! version number", `smithy_query(query="doc", shape="StartDeploymentRequest",
//! member="ConfigurationVersion")`), which for an object is its `VersionId`. So every reader
//! of the deployed configuration is served the bytes the object store holds for THAT object
//! version, described by the media type that object carries, and the deployed version stays
//! PINNED: a write at the key mints a new version and changes nothing a session is served
//! until a deployment names it. The same resolution decides whether a deployment can be
//! started at all — AppConfig reads the object's metadata before it records one, so a version
//! the object store holds no such version of is refused there and then, with the object
//! store's own answer to that read, and no deployment record is written; a location that read
//! says nothing about is left to the reader of the configuration, which reports the version it
//! could not retrieve.
//!
//! Only a cross-service suite can state that end to end: Amazon S3 owns the bucket, the object
//! and its versions, AppConfig's control plane owns the profile and the deployment, and what a
//! running application receives is read where that application reads it — out of the AppConfig
//! DATA plane, through a configuration session. `appconfig_external_source_configuration.rs`
//! states the same rule for a Parameter Store parameter; what is stated here is the object
//! store's own addressing of a version, which a parameter cannot show.
//!
//! The rows differ along what the rule generalises over: the shape of the object key (a
//! top-level key and one whose name carries separators), the media type the object was written
//! with (a type the write named and none at all), the bytes of each version, the Region every
//! call of the row is signed for, and — for the gate table — WHAT the object store answers
//! about the version a deployment names (one it holds, a version id the object never had, the
//! version id of another object, and a bucket it does not hold at all).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::form_encode;

/// The Region most rows are signed for; one row uses [`OTHER_REGION`] instead, so nothing here
/// can pass by resolving a source at a Region the request never named.
const REGION: &str = "us-east-1";

/// See [`REGION`].
const OTHER_REGION: &str = "eu-west-2";

/// The media type a JSON body travels with.
const JSON_CONTENT_TYPE: &str = "application/json";

/// The media type Amazon S3's own REST requests travel with.
const XML_CONTENT_TYPE: &str = "application/xml";

/// The namespace Amazon S3's own request documents carry.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The media type Amazon S3 describes an object stored by a write that named none by (the
/// default `Content-Type` of a stored object).
const DEFAULT_OBJECT_CONTENT_TYPE: &str = "binary/octet-stream";

/// The `DeploymentState` values a deployment can no longer leave
/// (`smithy_query(query="shape", shape="DeploymentState")`).
const TERMINAL_STATES: &[&str] = &["COMPLETE", "ROLLED_BACK", "REVERTED"];

/// How often work the clock owes — a deployment settling, a rollout reaching this session — is
/// looked for again, and how long the looking goes on.
///
/// Those spans belong to the services that own them, so nothing here sleeps on their behalf:
/// every wait is on the CONDITION, and each deadline is a ceiling on the WAIT and nothing else,
/// generous enough that a loaded host cannot decide any outcome. No assertion here is about how
/// long anything took.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const SETTLED_BY: Duration = Duration::from_secs(120);
/// See [`LOOK`].
const OBSERVED_BY: Duration = Duration::from_secs(60);

/// How many FRESH sessions are opened to check the pinning: a deployment pinned to one object
/// version must serve that version to every session started after the key was written again,
/// not merely to the one that was already polling.
const PINNED_SESSIONS: usize = 3;

/// One row of the serving table: the object a profile points at, and the three versions of it
/// the row writes.
#[derive(Debug)]
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    /// The Region every call of this row is signed for, and so the Region its bucket is
    /// created in and its session is polled at.
    region: &'static str,
    bucket: &'static str,
    /// The object key inside that bucket.
    key: &'static str,
    /// The `Content-Type` each write of the object carries; `None` for a write that names
    /// none, which the object store describes by its own default media type.
    content_type: Option<&'static str>,
    /// The media type a reader of the deployed configuration must report, which is the one
    /// the object carries.
    served_content_type: &'static str,
    /// The content of the object version the first deployment names.
    deployed: &'static str,
    /// The content of the version written next, and deployed second.
    redeployed: &'static str,
    /// The content of a version written at the key while another is in service: no deployment
    /// names it, so nothing may serve it.
    unnamed: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a JSON object at a top-level key",
        region: REGION,
        bucket: "appconfig-source-json",
        key: "config.json",
        content_type: Some(JSON_CONTENT_TYPE),
        served_content_type: JSON_CONTENT_TYPE,
        deployed: r#"{"useNewBackend": true}"#,
        redeployed: r#"{"useNewBackend": false}"#,
        unnamed: r#"{"useNewBackend": "neither"}"#,
    },
    Row {
        case: "an object under a key whose name carries separators, in another Region",
        region: OTHER_REGION,
        bucket: "appconfig-source-nested",
        key: "my-app/eu-west-2/my-config.yaml",
        content_type: Some("application/x-yaml"),
        served_content_type: "application/x-yaml",
        deployed: "useNewBackend: true\n",
        redeployed: "useNewBackend: false\n",
        unnamed: "useNewBackend: maybe\n",
    },
    Row {
        case: "an object written with no media type at all",
        region: REGION,
        bucket: "appconfig-source-plain",
        key: "flags",
        content_type: None,
        served_content_type: DEFAULT_OBJECT_CONTENT_TYPE,
        deployed: "alpha,beta",
        redeployed: "alpha,beta,gamma",
        unnamed: "alpha",
    },
];

/// Which version a row of the refusal table names in its deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Names {
    /// The version id the object store minted for the object — the positive control.
    TheObjectsVersion,
    /// A token shaped like one Amazon S3 mints that no write of this object ever minted.
    AVersionNeverMinted,
    /// The version id of ANOTHER object in the same bucket: a real id, but not one of this
    /// key's versions.
    AnotherObjectsVersion,
}

/// What the deployment of a row of the gate table must answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Accepted, and the object version's bytes served — the positive control.
    Served,
    /// Refused as the object store's own answer to the metadata read, with no record left
    /// behind.
    RefusedAtTheSource,
    /// Accepted, because the object store said nothing about a version of a bucket it does
    /// not hold, and reported by the READER of the configuration as the version it could not
    /// retrieve from that location.
    UnretrievableWhenRead,
}

/// One row of the gate table.
#[derive(Debug)]
struct GateRow {
    case: &'static str,
    bucket: &'static str,
    /// Whether the bucket and the object are written at all before the deployment.
    source_written: bool,
    names: Names,
    outcome: Outcome,
}

const GATE_ROWS: &[GateRow] = &[
    GateRow {
        case: "the version the object holds",
        bucket: "appconfig-gate-held",
        source_written: true,
        names: Names::TheObjectsVersion,
        outcome: Outcome::Served,
    },
    GateRow {
        case: "a version id the object never had",
        bucket: "appconfig-gate-unminted",
        source_written: true,
        names: Names::AVersionNeverMinted,
        outcome: Outcome::RefusedAtTheSource,
    },
    GateRow {
        case: "the version id of another object in the same bucket",
        bucket: "appconfig-gate-other-key",
        source_written: true,
        names: Names::AnotherObjectsVersion,
        outcome: Outcome::RefusedAtTheSource,
    },
    GateRow {
        case: "a bucket the object store does not hold",
        bucket: "appconfig-gate-absent",
        source_written: false,
        names: Names::AVersionNeverMinted,
        outcome: Outcome::UnretrievableWhenRead,
    },
];

/// The key every row of the refusal table points its profile at.
const GATE_KEY: &str = "config.json";

/// The other key the `AnotherObjectsVersion` row mints a real version of.
const GATE_DECOY_KEY: &str = "decoy.json";

/// What a row of the refusal table holds at [`GATE_KEY`].
const GATE_CONTENT: &str = r#"{"useNewBackend": true}"#;

/// The emulator as it is ASSEMBLED, on one engine: the object a profile points at is a record
/// of this emulator's own Amazon S3, and the configuration a client polls is the one
/// AppConfig's data plane resolves against it.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: each call carries a credential scope so the
    // router can name the target service, without a computed signature.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and `region`,
/// which is what routes the request and what each handler reads its Region from. The signature
/// is never checked: this router runs with request authentication off.
fn authorization(signing_name: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// What one dispatched request answered.
struct Answered {
    status: u16,
    headers: ::http::HeaderMap,
    body: Bytes,
}

impl Answered {
    /// The body as JSON, or `Value::Null` for an empty body.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&self.body).to_string()))
    }

    /// The body as text, for an assertion's failure message.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// One top-level string member of a JSON body.
    fn string(&self, member: &str) -> Option<String> {
        self.json()
            .get(member)
            .and_then(Value::as_str)
            .map(ToString::to_string)
    }

    /// One response header, as text.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    /// The wire error code of an error response, without its namespace.
    fn error_code(&self) -> String {
        let code = self
            .string("__type")
            .or_else(|| self.header("x-amzn-errortype"))
            .unwrap_or_else(|| self.text());
        code.split('#').next_back().unwrap_or(&code).to_string()
    }
}

/// One call to make: the endpoint host it is addressed at, the credential scope that routes it
/// to the service claiming it and the Region it is signed for, and how that service's protocol
/// carries it.
struct Call<'a> {
    service: &'a str,
    signing_name: &'a str,
    region: &'a str,
    method: &'a str,
    uri: &'a str,
}

/// Dispatch one request at `call`'s endpoint, carrying `headers` and `body`.
async fn dispatch(
    router: &MultiServiceRouter,
    call: &Call<'_>,
    headers: &[(&str, String)],
    body: &[u8],
) -> Answered {
    let host = format!("{}.{}.amazonaws.com", call.service, call.region);
    let mut request = ::http::Request::builder()
        .method(call.method)
        .uri(format!("https://{host}{}", call.uri))
        .header("host", host.clone())
        .header(
            "authorization",
            authorization(call.signing_name, call.region),
        );
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::from(body.to_vec()))
                .expect("the test request is well formed"),
        )
        .await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One call to Amazon S3's own REST wire, path-addressed.
async fn s3(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> Answered {
    dispatch(
        router,
        &Call {
            service: "s3",
            signing_name: "s3",
            region,
            method,
            uri,
        },
        headers,
        body,
    )
    .await
}

/// One AppConfig CONTROL-plane call carrying a JSON body.
async fn appconfig(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    dispatch(
        router,
        &Call {
            service: "appconfig",
            signing_name: "appconfig",
            region,
            method,
            uri,
        },
        &[("content-type", JSON_CONTENT_TYPE.to_string())],
        &payload,
    )
    .await
}

/// One AppConfig DATA-plane call, as a polling client makes one. Both of AppConfig's APIs sign
/// as `appconfig`, whichever endpoint a client addresses.
async fn appconfigdata(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answered {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    dispatch(
        router,
        &Call {
            service: "appconfigdata",
            signing_name: "appconfig",
            region,
            method,
            uri,
        },
        &[("content-type", JSON_CONTENT_TYPE.to_string())],
        &payload,
    )
    .await
}

/// Create `bucket` in `region` and turn its versioning on, so each write of one key is a
/// version of that object with an id of its own.
async fn versioned_bucket(router: &MultiServiceRouter, case: &str, region: &str, bucket: &str) {
    // "If you don't specify a Region, the bucket is created in the US East (N. Virginia)
    // Region (us-east-1) by default" (`CreateBucketConfiguration$LocationConstraint`), and
    // that Region is named by the ABSENCE of a constraint rather than by its own code.
    let configuration = match region {
        "us-east-1" => String::new(),
        region => format!(
            "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\"><LocationConstraint>{region}\
             </LocationConstraint></CreateBucketConfiguration>"
        ),
    };
    let created = s3(
        router,
        region,
        "PUT",
        &format!("/{bucket}"),
        &[("content-type", XML_CONTENT_TYPE.to_string())],
        configuration.as_bytes(),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {} with {}",
        created.status,
        created.text()
    );
    let versioned = s3(
        router,
        region,
        "PUT",
        &format!("/{bucket}?versioning"),
        &[("content-type", XML_CONTENT_TYPE.to_string())],
        format!(
            "<VersioningConfiguration xmlns=\"{S3_XMLNS}\"><Status>Enabled</Status>\
             </VersioningConfiguration>"
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        versioned.status,
        200,
        "case {case:?}: PutBucketVersioning({bucket:?}) must succeed, got {} with {}",
        versioned.status,
        versioned.text()
    );
}

/// Write one version of `bucket`/`key` through Amazon S3's own wire, and answer the `VersionId`
/// it minted for that version.
async fn put_object(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    bucket: &str,
    key: &str,
    content_type: Option<&str>,
    body: &str,
) -> String {
    let headers: Vec<(&str, String)> = content_type
        .map(|value| vec![("content-type", value.to_string())])
        .unwrap_or_default();
    let written = s3(
        router,
        region,
        "PUT",
        &format!("/{bucket}/{key}"),
        &headers,
        body.as_bytes(),
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "case {case:?}: PutObject({bucket:?}/{key:?}) must succeed, got {} with {}",
        written.status,
        written.text()
    );
    written.header("x-amz-version-id").unwrap_or_else(|| {
        panic!(
            "case {case:?}: PutObject({bucket:?}/{key:?}) must report the version it minted, got \
             no x-amz-version-id"
        )
    })
}

/// The bytes Amazon S3 itself serves for ONE version of `bucket`/`key` — the source of truth a
/// row's seeding is confirmed against, so nothing below can pass against an object version that
/// was never written.
async fn object_version(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    bucket: &str,
    key: &str,
    version_id: &str,
) -> String {
    let read = s3(
        router,
        region,
        "GET",
        &format!("/{bucket}/{key}?versionId={}", form_encode(version_id)),
        &[],
        &[],
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetObject({bucket:?}/{key:?}, {version_id:?}) must succeed, got {} with {}",
        read.status,
        read.text()
    );
    read.text()
}

/// One minted id out of a 201 answer.
fn minted_id(case: &str, what: &str, answered: &Answered) -> String {
    assert_eq!(
        answered.status,
        201,
        "case {case:?}: {what} must be created, got {} with {}",
        answered.status,
        answered.text()
    );
    answered
        .string("Id")
        .unwrap_or_else(|| panic!("case {case:?}: {what} answered no Id: {}", answered.text()))
}

/// The ARN of the role AppConfig reads an external source through. A `RetrievalRoleArn` is
/// "required for all other sources that store your configuration" (`smithy_query(query="doc",
/// shape="CreateConfigurationProfileRequest", member="RetrievalRoleArn")`), so every row carries
/// one.
fn retrieval_role_arn(suffix: &str) -> String {
    ::engine::emulator_arn("iam", "", &format!("role/appconfig-retrieval-{suffix}"))
}

/// One row's AppConfig hierarchy as the control plane minted it, with the Region its calls are
/// signed for: what a deployment is started against, and what a session polls.
struct Hierarchy {
    region: &'static str,
    application: String,
    environment: String,
    profile: String,
    strategy: String,
}

/// The AppConfig hierarchy a row deploys in: an application, an environment, a configuration
/// profile pointing at `location_uri`, and a strategy that reaches every target at once.
async fn given_hierarchy(
    router: &MultiServiceRouter,
    case: &str,
    region: &'static str,
    suffix: &str,
    location_uri: &str,
) -> Hierarchy {
    let application = minted_id(
        case,
        "the application",
        &appconfig(
            router,
            region,
            "POST",
            "/applications",
            &json!({"Name": format!("s3-sourced-app-{suffix}")}),
        )
        .await,
    );
    let environment = minted_id(
        case,
        "the environment",
        &appconfig(
            router,
            region,
            "POST",
            &format!("/applications/{application}/environments"),
            &json!({"Name": format!("s3-sourced-env-{suffix}")}),
        )
        .await,
    );
    let created_profile = appconfig(
        router,
        region,
        "POST",
        &format!("/applications/{application}/configurationprofiles"),
        &json!({
            "Name": format!("s3-sourced-profile-{suffix}"),
            "LocationUri": location_uri,
            "RetrievalRoleArn": retrieval_role_arn(suffix),
            "Type": "AWS.Freeform",
        }),
    )
    .await;
    let profile = minted_id(case, "the configuration profile", &created_profile);
    let echoed = created_profile.string("LocationUri").unwrap_or_default();
    assert_eq!(
        echoed, location_uri,
        "case {case:?}: the profile must be created at the location the request named, expected \
         {location_uri:?}, got {echoed:?}"
    );
    let created_strategy = appconfig(
        router,
        region,
        "POST",
        "/deploymentstrategies",
        &json!({
            "Name": format!("s3-sourced-fast-{suffix}"),
            "DeploymentDurationInMinutes": 0,
            "FinalBakeTimeInMinutes": 0,
            "GrowthFactor": 100,
            "GrowthType": "LINEAR",
            "ReplicateTo": "NONE",
        }),
    )
    .await;
    let strategy = minted_id(case, "the deployment strategy", &created_strategy);
    Hierarchy {
        region,
        application,
        environment,
        profile,
        strategy,
    }
}

/// `StartDeployment` of `version` in `hierarchy`, answered as it came back off the wire.
async fn start_deployment(
    router: &MultiServiceRouter,
    hierarchy: &Hierarchy,
    version: &str,
) -> Answered {
    appconfig(
        router,
        hierarchy.region,
        "POST",
        &format!(
            "/applications/{}/environments/{}/deployments",
            hierarchy.application, hierarchy.environment
        ),
        &json!({
            "DeploymentStrategyId": hierarchy.strategy,
            "ConfigurationProfileId": hierarchy.profile,
            "ConfigurationVersion": version,
        }),
    )
    .await
}

/// Read one deployment until it reaches a state it can no longer leave, and answer the document
/// it was last reported with. The wait is on the CONDITION.
async fn settled_deployment(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    number: i64,
) -> Value {
    let deadline = Instant::now() + SETTLED_BY;
    loop {
        let read = appconfig(
            router,
            hierarchy.region,
            "GET",
            &format!(
                "/applications/{}/environments/{}/deployments/{number}",
                hierarchy.application, hierarchy.environment
            ),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetDeployment({number}) must succeed, got {} with {}",
            read.status,
            read.text()
        );
        let document = read.json();
        let state = document["State"].as_str().unwrap_or_default().to_string();
        if TERMINAL_STATES.contains(&state.as_str()) {
            return document;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: deployment {number} never reached a terminal state, it is still \
             {state:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Start a deployment of `version`, wait for it to settle, and answer the document that reported
/// it COMPLETE.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    number: i64,
    version: &str,
) -> Value {
    let started = start_deployment(router, hierarchy, version).await;
    assert_eq!(
        started.status,
        201,
        "case {case:?}: StartDeployment({version:?}) must be accepted, got {} with {}",
        started.status,
        started.text()
    );
    let settled = settled_deployment(router, case, hierarchy, number).await;
    assert_eq!(
        settled["State"],
        json!("COMPLETE"),
        "case {case:?}: the deployment of version {version:?} must complete, got {settled}"
    );
    settled
}

/// Every deployment of a hierarchy's environment, newest first.
async fn deployments(router: &MultiServiceRouter, case: &str, hierarchy: &Hierarchy) -> Vec<Value> {
    let listed = appconfig(
        router,
        hierarchy.region,
        "GET",
        &format!(
            "/applications/{}/environments/{}/deployments",
            hierarchy.application, hierarchy.environment
        ),
        &Value::Null,
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "case {case:?}: ListDeployments must succeed, got {} with {}",
        listed.status,
        listed.text()
    );
    listed
        .json()
        .get("Items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// What AppConfig's data plane serves ONE fresh client right now: a session of its own, then the
/// configuration that session is handed, answered as it came back off the wire.
async fn served(router: &MultiServiceRouter, case: &str, hierarchy: &Hierarchy) -> Answered {
    let session = appconfigdata(
        router,
        hierarchy.region,
        "POST",
        "/configurationsessions",
        &json!({
            "ApplicationIdentifier": hierarchy.application,
            "EnvironmentIdentifier": hierarchy.environment,
            "ConfigurationProfileIdentifier": hierarchy.profile,
        }),
    )
    .await;
    assert_eq!(
        session.status,
        201,
        "case {case:?}: StartConfigurationSession must succeed, got {} with {}",
        session.status,
        session.text()
    );
    let token = session
        .string("InitialConfigurationToken")
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: StartConfigurationSession answered no token: {}",
                session.text()
            )
        });
    appconfigdata(
        router,
        hierarchy.region,
        "GET",
        &format!("/configuration?configuration_token={}", form_encode(&token)),
        &Value::Null,
    )
    .await
}

/// What a fresh client is served once the deployment's rollout has reached it: read again through
/// a NEW session until the bytes are `expected` or the deadline passes, then answer the last read,
/// which is what the caller asserts on.
async fn serving(
    router: &MultiServiceRouter,
    case: &str,
    hierarchy: &Hierarchy,
    expected: &str,
) -> Answered {
    let deadline = Instant::now() + OBSERVED_BY;
    loop {
        let read = served(router, case, hierarchy).await;
        if read.body.as_ref() == expected.as_bytes() || Instant::now() >= deadline {
            return read;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// RULE: the configuration a deployment of an S3-backed profile puts in service is the bytes the
/// object store holds for the object VERSION the deployment named — not the bytes the key
/// currently holds — described by the media type that object version carries, and it stays that
/// version's bytes until a deployment names another one.
#[tokio::test]
async fn an_s3_backed_profile_is_served_the_deployed_object_version() {
    let router = build_router();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let suffix = format!("obj{index}");
        versioned_bucket(&router, case, row.region, row.bucket).await;

        // Two versions of one key, written where that store's own clients write them: the
        // second is the key's CURRENT content while the first is what the deployment names.
        let first = put_object(
            &router,
            case,
            row.region,
            row.bucket,
            row.key,
            row.content_type,
            row.deployed,
        )
        .await;
        let second = put_object(
            &router,
            case,
            row.region,
            row.bucket,
            row.key,
            row.content_type,
            row.redeployed,
        )
        .await;
        assert_ne!(
            first, second,
            "case {case:?}: two writes of {:?} must mint two distinct version ids, both are \
             {first:?}",
            row.key
        );
        let held = object_version(&router, case, row.region, row.bucket, row.key, &first).await;
        assert_eq!(
            held, row.deployed,
            "case {case:?}: Amazon S3 must hold the seeded bytes at version {first:?}, expected \
             {:?}, got {held:?}",
            row.deployed
        );

        let location_uri = format!("s3://{}/{}", row.bucket, row.key);
        let hierarchy = given_hierarchy(&router, case, row.region, &suffix, &location_uri).await;

        // Version one of the object into service: the deployment records WHAT it deployed.
        let settled = deploy(&router, case, &hierarchy, 1, &first).await;
        assert_eq!(
            settled["ConfigurationVersion"],
            json!(first),
            "case {case:?}: the deployment must record the object version it deployed, got \
             {settled}"
        );
        assert_eq!(
            settled["ConfigurationLocationUri"],
            json!(location_uri),
            "case {case:?}: the deployment must record the location it deployed from, got \
             {settled}"
        );

        let read = serving(&router, case, &hierarchy, row.deployed).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetLatestConfiguration must serve the deployed object version, got {} \
             with {}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.text(),
            row.deployed,
            "case {case:?}: expected the deployed version's bytes {:?}, got {:?}",
            row.deployed,
            read.text()
        );
        let content_type = read.header("content-type").unwrap_or_default();
        assert_eq!(
            content_type, row.served_content_type,
            "case {case:?}: expected the object's media type {:?}, got {content_type:?}",
            row.served_content_type
        );

        // A third version written at the key out of band: no deployment names it, so nothing a
        // fresh session is served may change.
        let unnamed = put_object(
            &router,
            case,
            row.region,
            row.bucket,
            row.key,
            row.content_type,
            row.unnamed,
        )
        .await;
        assert_ne!(
            unnamed, first,
            "case {case:?}: the out-of-band write must mint a version of its own, got {unnamed:?}"
        );
        for look in 1..=PINNED_SESSIONS {
            let pinned = served(&router, case, &hierarchy).await;
            assert_eq!(
                pinned.text(),
                row.deployed,
                "case {case:?}: session {look} started after the key was written again must still \
                 be served the DEPLOYED version {:?}, got {:?}",
                row.deployed,
                pinned.text()
            );
        }

        // And the second version only once a deployment names it.
        deploy(&router, case, &hierarchy, 2, &second).await;
        let after = serving(&router, case, &hierarchy, row.redeployed).await;
        assert_eq!(
            after.status,
            200,
            "case {case:?}: GetLatestConfiguration must serve the redeployed object version, got \
             {} with {}",
            after.status,
            after.text()
        );
        assert_eq!(
            after.text(),
            row.redeployed,
            "case {case:?}: expected the newly deployed version's bytes {:?}, got {:?}",
            row.redeployed,
            after.text()
        );

        // The listing records the two object versions in deployment order, newest first.
        let listed: Vec<String> = deployments(&router, case, &hierarchy)
            .await
            .iter()
            .map(|item| {
                item["ConfigurationVersion"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            listed,
            vec![second.clone(), first.clone()],
            "case {case:?}: expected the two deployed object versions newest first, got {listed:?}"
        );
    }
}

/// How AppConfig reports the Amazon S3 metadata read of a configuration object that was refused,
/// for the pair of correlation ids `request_id` and `extended_request_id` that one read was named
/// by — the only parts of the sentence that differ between two calls.
fn expected_refusal(request_id: &str, extended_request_id: &str) -> String {
    format!(
        "An error occurred when calling the S3 GetObjectMetadata operation: Forbidden (Service: \
         Amazon S3; Status Code: 403; Error Code: 403 Forbidden; Request ID: {request_id}; S3 \
         Extended Request ID: {extended_request_id}; Proxy: null)"
    )
}

/// The two correlation ids `message` names, or `None` when it is not that sentence at all.
fn correlation_ids(message: &str) -> Option<(String, String)> {
    let after = message.split_once("; Request ID: ")?.1;
    let (request_id, rest) = after.split_once("; S3 Extended Request ID: ")?;
    let (extended_request_id, tail) = rest.split_once("; Proxy: null)")?;
    if !tail.is_empty() || request_id.is_empty() || extended_request_id.is_empty() {
        return None;
    }
    Some((request_id.to_string(), extended_request_id.to_string()))
}

/// RULE (the same resolution, at the moment a deployment is STARTED): AppConfig reads the object
/// version a deployment names at the source before it records anything, so a version the object
/// store holds no such version of is refused synchronously with the modelled
/// `BadRequestException` (HTTP 400, `smithy_query(query="operation", operation="StartDeployment")`)
/// reporting the object store's own answer to that read, and the environment gains no deployment
/// record; a version the store does hold is accepted and served. A location whose BUCKET the store
/// does not hold is one that read says nothing about, so it is the reader of the configuration
/// that reports the version it could not retrieve from that location — the refusal is confined to
/// the answer the store gave.
#[tokio::test]
async fn a_deployment_of_an_object_version_the_store_does_not_hold_is_refused() {
    let router = build_router();
    let mut seen_ids: Vec<(String, String)> = Vec::new();
    for (index, row) in GATE_ROWS.iter().enumerate() {
        let case = row.case;
        let suffix = format!("gate{index}");
        let mut minted = String::new();
        let mut decoy = String::new();
        if row.source_written {
            versioned_bucket(&router, case, REGION, row.bucket).await;
            minted = put_object(
                &router,
                case,
                REGION,
                row.bucket,
                GATE_KEY,
                Some(JSON_CONTENT_TYPE),
                GATE_CONTENT,
            )
            .await;
            decoy = put_object(
                &router,
                case,
                REGION,
                row.bucket,
                GATE_DECOY_KEY,
                Some(JSON_CONTENT_TYPE),
                r#"{"useNewBackend": false}"#,
            )
            .await;
        }
        // A token of the WIDTH and alphabet S3 writes a version id in, so a refusal cannot be
        // about the shape of the value: `ObjectVersionId` constrains none
        // (`smithy_query(query="shape", shape="ObjectVersionId")`), so the shape is S3's own.
        let never_minted = "x".repeat(if minted.is_empty() { 32 } else { minted.len() });
        let version = match row.names {
            Names::TheObjectsVersion => minted.clone(),
            Names::AVersionNeverMinted => never_minted,
            Names::AnotherObjectsVersion => decoy.clone(),
        };
        let location_uri = format!("s3://{}/{GATE_KEY}", row.bucket);
        let hierarchy = given_hierarchy(&router, case, REGION, &suffix, &location_uri).await;
        let started = start_deployment(&router, &hierarchy, &version).await;

        if row.outcome != Outcome::RefusedAtTheSource {
            assert_eq!(
                started.status,
                201,
                "case {case:?}: StartDeployment({version:?}) must be accepted, got {} with {}",
                started.status,
                started.text()
            );
            settled_deployment(&router, case, &hierarchy, 1).await;
            let recorded = deployments(&router, case, &hierarchy).await;
            assert_eq!(
                recorded.len(),
                1,
                "case {case:?}: the accepted deployment must be recorded once, got {recorded:?}"
            );
            if row.outcome == Outcome::Served {
                let read = serving(&router, case, &hierarchy, GATE_CONTENT).await;
                assert_eq!(
                    read.status,
                    200,
                    "case {case:?}: the accepted deployment must be served, got {} with {}",
                    read.status,
                    read.text()
                );
                assert_eq!(
                    read.text(),
                    GATE_CONTENT,
                    "case {case:?}: expected the deployed object version's bytes \
                     {GATE_CONTENT:?}, got {:?}",
                    read.text()
                );
                continue;
            }
            // The reader of the configuration reports what it could not retrieve, naming the
            // version and the location — and not the metadata read's refusal, which the object
            // store never answered for a bucket it does not hold.
            let read = served(&router, case, &hierarchy).await;
            assert_eq!(
                read.status,
                404,
                "case {case:?}: a poll whose source holds no such version must be refused with \
                 404, got {} with {}",
                read.status,
                read.text()
            );
            assert_eq!(
                read.error_code(),
                "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException, got {}",
                read.error_code()
            );
            let message = read.string("Message").unwrap_or_default();
            assert!(
                message.contains(&location_uri) && message.contains(&version),
                "case {case:?}: expected the refusal to name the version {version:?} and the \
                 location {location_uri:?}, got {message:?}"
            );
            assert!(
                correlation_ids(&message).is_none(),
                "case {case:?}: the metadata read's refusal must not be reported for a bucket \
                 the store does not hold, got {message:?}"
            );
            continue;
        }

        assert_eq!(
            started.status,
            400,
            "case {case:?}: StartDeployment({version:?}) names a version the object store does \
             not hold, so it must be refused with 400, got {} with {}",
            started.status,
            started.text()
        );
        assert_eq!(
            started.error_code(),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {}",
            started.error_code()
        );
        let message = started.string("Message").unwrap_or_default();
        let (request_id, extended_request_id) = correlation_ids(&message).unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the refusal of the source's metadata read, got \
                 {message:?}"
            )
        });
        assert_eq!(
            message,
            expected_refusal(&request_id, &extended_request_id),
            "case {case:?}: expected the metadata read's own refusal, got {message:?}"
        );
        assert!(
            !seen_ids.contains(&(request_id.clone(), extended_request_id.clone())),
            "case {case:?}: each refused read must be named by its own correlation ids, \
             {request_id:?} / {extended_request_id:?} named an earlier one too"
        );
        seen_ids.push((request_id, extended_request_id));
        let recorded = deployments(&router, case, &hierarchy).await;
        assert!(
            recorded.is_empty(),
            "case {case:?}: a refused deployment must leave no record, got {recorded:?}"
        );
    }
}
