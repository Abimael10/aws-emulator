// SPDX-License-Identifier: CC0-1.0

//! A configuration profile whose configuration is an Amazon S3 object, read through
//! AppConfig's OWN reader of a deployed configuration.
//!
//! RULE: a configuration profile's `LocationUri` says which store its configuration lives in,
//! and `s3://<bucket>/<objectKey>` names an Amazon S3 object (`smithy_query(query="doc",
//! shape="CreateConfigurationProfileRequest", member="LocationUri")`); a deployment of such a
//! profile names a version of THAT store ("for all other configurations, you must specify the
//! version number", `smithy_query(query="doc", shape="StartDeploymentRequest",
//! member="ConfigurationVersion")`), which for an object is its `VersionId`. So a reader of
//! the deployed configuration is served the bytes the object store holds for THAT object
//! version — not the bytes the key currently holds — described by the media type that object
//! carries, and a deployment naming a version the object store does not hold is refused as the
//! object store's own answer to the metadata read AppConfig makes before recording one.
//!
//! The reader under test here is this service's own `GetConfiguration`, whose answer must come
//! out of the same resolution the data plane's poll serves: the object store is mounted beside
//! AppConfig on one engine, the way the assembled emulator mounts them, and the object is
//! written through Amazon S3's own wire.
//! `core/router/tests/all/appconfig_s3_sourced_configuration.rs` states the same rule for the
//! DATA plane's poll, where the whole emulator is assembled.
//!
//! The rows differ along what the rule generalises over: the shape of the object key (a
//! top-level key and one whose name carries separators), the media type the object was written
//! with (a type the write named and none at all), and the bytes of each version.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{
    create_application, create_environment, create_profile_from, deployment_in_state, json_call,
    send, Answer, RETRIEVAL_ROLE, TERMINAL_DEPLOYMENT_STATES,
};

/// The Region every call below is signed for, which is where the bucket is created too.
const REGION: &str = "us-east-1";

/// The media type Amazon S3's own REST requests travel with.
const XML_CONTENT_TYPE: &str = "application/xml";

/// The namespace Amazon S3's own request documents carry.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The media type Amazon S3 describes an object stored by a write that named none by (the
/// default `Content-Type` of a stored object).
const DEFAULT_OBJECT_CONTENT_TYPE: &str = "binary/octet-stream";

/// One row: the object a profile points at, and the two versions of it the row writes.
struct Row {
    /// The row's name, which every assertion of it reports.
    case: &'static str,
    bucket: &'static str,
    key: &'static str,
    /// The `Content-Type` each write of the object carries; `None` for a write that names
    /// none, which the object store describes by its own default media type.
    content_type: Option<&'static str>,
    /// The media type the reader must report, which is the one the object carries.
    served_content_type: &'static str,
    /// The content of the object version the deployment names.
    deployed: &'static str,
    /// The content written at the same key afterwards, which no deployment names.
    unnamed: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a JSON object at a top-level key",
        bucket: "appconfig-read-json",
        key: "config.json",
        content_type: Some("application/json"),
        served_content_type: "application/json",
        deployed: r#"{"useNewBackend": true}"#,
        unnamed: r#"{"useNewBackend": false}"#,
    },
    Row {
        case: "an object under a key whose name carries separators, written with no media type",
        bucket: "appconfig-read-nested",
        key: "my-app/us-east-1/my-config",
        content_type: None,
        served_content_type: DEFAULT_OBJECT_CONTENT_TYPE,
        deployed: "alpha,beta",
        unnamed: "alpha,beta,gamma",
    },
];

/// AppConfig and the object store on ONE engine, the way the assembled emulator mounts them:
/// the object a profile points at is a record of this emulator's own Amazon S3.
fn router_with_object_store() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = crate::appconfig_workflow::router_on(&engine);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// One call to Amazon S3's own REST wire, path-addressed. The router picks the service by the
/// credential scope, so this call carries `s3`'s rather than AppConfig's.
async fn s3(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host.clone())
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/s3/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    let response = router
        .dispatch(
            request
                .body(bytes::Bytes::from(body.to_vec()))
                .expect("the test request must build"),
        )
        .await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// Create `bucket` and turn its versioning on, so each write of one key is a version of that
/// object with an id of its own.
async fn versioned_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    // "If you don't specify a Region, the bucket is created in the US East (N. Virginia)
    // Region (us-east-1) by default" (`CreateBucketConfiguration$LocationConstraint`), and
    // that Region is named by the ABSENCE of a constraint rather than by its own code.
    let created = s3(
        router,
        "PUT",
        &format!("/{bucket}"),
        &[("content-type", XML_CONTENT_TYPE.to_string())],
        &[],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: CreateBucket({bucket:?}) must succeed, got {} / {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    let versioned = s3(
        router,
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
        "case {case:?}: PutBucketVersioning({bucket:?}) must succeed, got {} / {}",
        versioned.status,
        String::from_utf8_lossy(&versioned.body)
    );
}

/// Write one version of `bucket`/`key` through Amazon S3's own wire, and answer the `VersionId`
/// it minted for that version.
async fn put_object(
    router: &MultiServiceRouter,
    case: &str,
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
        "PUT",
        &format!("/{bucket}/{key}"),
        &headers,
        body.as_bytes(),
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "case {case:?}: PutObject({bucket:?}/{key:?}) must succeed, got {} / {}",
        written.status,
        String::from_utf8_lossy(&written.body)
    );
    written.header("x-amz-version-id").unwrap_or_else(|| {
        panic!(
            "case {case:?}: PutObject({bucket:?}/{key:?}) must report the version it minted, got \
             no x-amz-version-id"
        )
    })
}

/// A deployment strategy with no rollout window and no bake time, so the deployment it paces is
/// over as soon as the clock owes it.
async fn create_instant_strategy(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    json_call(
        router,
        "POST",
        "/deploymentstrategies",
        &json!({
            "Name": name,
            "DeploymentDurationInMinutes": 0,
            "GrowthFactor": 100.0,
            "GrowthType": "LINEAR",
            "FinalBakeTimeInMinutes": 0,
            "ReplicateTo": "NONE",
        }),
    )
    .await
    .string("Id")
    .unwrap_or_else(|| panic!("case {case:?}: CreateDeploymentStrategy answered no Id"))
}

/// `StartDeployment` of `version` of `profile` into `environment`.
async fn start_deployment(
    router: &MultiServiceRouter,
    application: &str,
    environment: &str,
    profile: &str,
    strategy: &str,
    version: &str,
) -> Answer {
    json_call(
        router,
        "POST",
        &format!("/applications/{application}/environments/{environment}/deployments"),
        &json!({
            "DeploymentStrategyId": strategy,
            "ConfigurationProfileId": profile,
            "ConfigurationVersion": version,
        }),
    )
    .await
}

/// `GetConfiguration` of the deployed configuration, as this service's own reader answers it
/// (`smithy_query(query="operation", operation="GetConfiguration")`:
/// `/applications/{Application}/environments/{Environment}/configurations/{Configuration}`,
/// with `ClientId` bound to the `client_id` query parameter).
async fn get_configuration(
    router: &MultiServiceRouter,
    application: &str,
    environment: &str,
    profile: &str,
) -> Answer {
    send(
        router,
        "GET",
        &format!(
            "/applications/{application}/environments/{environment}/configurations/{profile}\
             ?client_id=test-client"
        ),
        "application/json",
        Vec::new(),
        &[],
    )
    .await
}

/// Every deployment of `environment`, newest first.
async fn list_deployments(
    router: &MultiServiceRouter,
    application: &str,
    environment: &str,
) -> Vec<Value> {
    let answer = json_call(
        router,
        "GET",
        &format!("/applications/{application}/environments/{environment}/deployments"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "ListDeployments must answer 200, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answer
        .json()
        .get("Items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// RULE: `GetConfiguration` of a profile whose `LocationUri` names an Amazon S3 object answers
/// the bytes that object holds at the VERSION the deployment named, with that object's media
/// type — and keeps answering them after the key is written again, because a write at the key
/// mints a new version and the deployment stays pinned to the one it named.
#[tokio::test]
async fn an_s3_backed_configuration_is_read_at_the_deployed_object_version() {
    let router = router_with_object_store();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        versioned_bucket(&router, case, row.bucket).await;
        let deployed = put_object(
            &router,
            case,
            row.bucket,
            row.key,
            row.content_type,
            row.deployed,
        )
        .await;

        let application = create_application(&router, &format!("s3-read-app-{index}")).await;
        let environment = create_environment(&router, &application, "production").await;
        let strategy =
            create_instant_strategy(&router, case, &format!("instant-s3-read-{index}")).await;
        let location_uri = format!("s3://{}/{}", row.bucket, row.key);
        let created = create_profile_from(
            &router,
            &application,
            case,
            &json!({
                "Name": format!("s3-read-profile-{index}"),
                "LocationUri": location_uri,
                "RetrievalRoleArn": RETRIEVAL_ROLE,
                "Type": "AWS.Freeform",
            }),
        )
        .await;
        let profile = created
            .string("Id")
            .unwrap_or_else(|| panic!("case {case:?}: the profile answered no Id"));
        assert_eq!(
            created.string("LocationUri").as_deref(),
            Some(location_uri.as_str()),
            "case {case:?}: the profile must keep the location it was created with, got {}",
            created.json()
        );

        let started = start_deployment(
            &router,
            &application,
            &environment,
            &profile,
            &strategy,
            &deployed,
        )
        .await;
        assert_eq!(
            started.status,
            201,
            "case {case:?}: StartDeployment({deployed:?}) must be accepted, got {} / {}",
            started.status,
            String::from_utf8_lossy(&started.body)
        );
        let settled = deployment_in_state(
            &router,
            case,
            &application,
            &environment,
            1,
            TERMINAL_DEPLOYMENT_STATES,
        )
        .await;
        assert_eq!(
            settled["State"],
            json!("COMPLETE"),
            "case {case:?}: the deployment must complete, got {settled}"
        );
        assert_eq!(
            settled["ConfigurationVersion"],
            json!(deployed),
            "case {case:?}: the deployment must record the object version it deployed, got \
             {settled}"
        );
        assert_eq!(
            settled["ConfigurationLocationUri"],
            json!(location_uri),
            "case {case:?}: the deployment must record the location it deployed from, got \
             {settled}"
        );

        // A second version written at the same key, which no deployment names: the read must
        // keep answering the version in service rather than the key's current bytes.
        let unnamed = put_object(
            &router,
            case,
            row.bucket,
            row.key,
            row.content_type,
            row.unnamed,
        )
        .await;
        assert_ne!(
            unnamed, deployed,
            "case {case:?}: the second write must mint a version of its own, got {unnamed:?}"
        );

        let read = get_configuration(&router, &application, &environment, &profile).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetConfiguration must answer the deployed object version, got {} / {}",
            read.status,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            String::from_utf8_lossy(&read.body),
            row.deployed,
            "case {case:?}: expected the deployed version's bytes {:?}, got {:?}",
            row.deployed,
            String::from_utf8_lossy(&read.body)
        );
        assert_eq!(
            read.header("Content-Type").as_deref(),
            Some(row.served_content_type),
            "case {case:?}: expected the object's media type {:?}, got {:?}",
            row.served_content_type,
            read.header("Content-Type")
        );
        assert_eq!(
            read.header("Configuration-Version").as_deref(),
            Some(deployed.as_str()),
            "case {case:?}: expected the deployed object version reported back, got {:?}",
            read.header("Configuration-Version")
        );
    }
}

/// RULE (the same resolution, at the moment a deployment is STARTED): the object version a
/// deployment names is resolved at the object store before anything is written, so a version
/// that store does not hold is refused with the modelled `BadRequestException` (HTTP 400,
/// `smithy_query(query="operation", operation="StartDeployment")`) reporting the object store's
/// own answer to that read, and the environment gains no deployment record.
#[tokio::test]
async fn a_deployment_naming_an_object_version_the_store_does_not_hold_is_refused() {
    let router = router_with_object_store();
    let bucket = "appconfig-gate-bucket";
    let key = "config.json";
    versioned_bucket(&router, "setup", bucket).await;
    let held = put_object(
        &router,
        "setup",
        bucket,
        key,
        Some("application/json"),
        r#"{"useNewBackend": true}"#,
    )
    .await;
    let decoy = put_object(
        &router,
        "setup",
        bucket,
        "decoy.json",
        Some("application/json"),
        r#"{"useNewBackend": false}"#,
    )
    .await;

    let application = create_application(&router, "s3-gate-app").await;
    let environment = create_environment(&router, &application, "production").await;
    let strategy = create_instant_strategy(&router, "s3-gate", "instant-s3-gate").await;
    let profile = create_profile_from(
        &router,
        &application,
        "s3-gate",
        &json!({
            "Name": "s3-gate-profile",
            "LocationUri": format!("s3://{bucket}/{key}"),
            "RetrievalRoleArn": RETRIEVAL_ROLE,
            "Type": "AWS.Freeform",
        }),
    )
    .await
    .string("Id")
    .unwrap_or_else(|| panic!("case \"s3-gate\": the profile answered no Id"));

    // The rows differ along WHY the object store holds no such version: a token of the width
    // and alphabet S3 writes a version id in that no write minted, one written in the mixed
    // alphanumerics S3's own ids use, and the version id of another object in the same bucket.
    let refused = [
        ("a version id the object never had", "x".repeat(held.len())),
        (
            "a version id of S3's own alphabet that no write minted",
            "3HL4kqtJlcpXroDTDmjVBH40Nrjfkd3l".to_string(),
        ),
        ("the version id of another object", decoy.clone()),
    ];
    for (case, version) in &refused {
        let answer = start_deployment(
            &router,
            &application,
            &environment,
            &profile,
            &strategy,
            version,
        )
        .await;
        assert_eq!(
            answer.status,
            400,
            "case {case:?}: expected 400 for a version the object store does not hold, got {} / {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        let code = answer.error_code();
        assert!(
            code.contains("BadRequestException"),
            "case {case:?}: expected the modelled BadRequestException, got {code:?}"
        );
        let message = answer.string("Message").unwrap_or_default();
        assert!(
            message.starts_with(
                "An error occurred when calling the S3 GetObjectMetadata operation: Forbidden \
                 (Service: Amazon S3; Status Code: 403; Error Code: 403 Forbidden; Request ID: "
            ) && message.ends_with("; Proxy: null)"),
            "case {case:?}: expected the object store's own refusal of the metadata read, got \
             {message:?}"
        );
        let listed = list_deployments(&router, &application, &environment).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: a refused deployment must leave no record, got {listed:?}"
        );
    }

    // The version the object does hold deploys, and the environment's deployment numbers start
    // at one: nothing the refusals above consumed.
    let accepted = start_deployment(
        &router,
        &application,
        &environment,
        &profile,
        &strategy,
        &held,
    )
    .await;
    assert_eq!(
        accepted.status,
        201,
        "the version the object holds must deploy, got {} / {}",
        accepted.status,
        String::from_utf8_lossy(&accepted.body)
    );
    let numbers: Vec<i64> = list_deployments(&router, &application, &environment)
        .await
        .iter()
        .filter_map(|item| item.get("DeploymentNumber").and_then(Value::as_i64))
        .collect();
    assert_eq!(
        numbers,
        vec![1],
        "expected the one deployment the held object version started, got {numbers:?}"
    );
}
