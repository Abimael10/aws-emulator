// SPDX-License-Identifier: CC0-1.0

//! WHERE a Greengrass V2 component version's artifact is downloaded from, across the two
//! services that own the two halves of it.
//!
//! `greengrassv2:GetComponentVersionArtifact` answers "the pre-signed URL to download a
//! component artifact", and WHICH artifacts a version has is declared by its recipe:
//! `Manifests[].Artifacts[].Uri` (the IoT Greengrass component recipe reference), where
//! the name an artifact is asked for by is "the section of the URI after the scheme"
//! (`artifactName`'s own documentation). The bytes of an artifact declared at an object
//! URI are an Amazon S3 object — a record of S3, not of Greengrass — so the URL
//! Greengrass hands out is only followable because both services are mounted on one
//! emulator, which is what `core/router` ships.
//!
//! The rule these rows pin: the artifact read resolves the name against the artifact URIs
//! the addressed version's STORED recipe declares, and answers a pre-signed URL for the
//! object that URI names — a URL that leads back to this emulator and serves the very
//! bytes `s3:PutObject` wrote. A name that recipe declares no object artifact under —
//! and every name, when the ARN names no stored version at all — is refused `400
//! ValidationException` with the one sentence AWS words both halves of that search with.
//!
//! The rows differ along what the rule generalises over: which of an artifact's two names
//! is asked for, which manifest declares it, whether its key carries a prefix, which
//! bucket holds it, and which way the search comes up empty.

use std::sync::Arc;

use aws_smithy_types::base64;
use bytes::Bytes;
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

/// The account every emulated ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";
/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// The signing names this suite signs for, which is how the router picks the service that
/// serves each request.
const GREENGRASS: &str = "greengrass";
const S3: &str = "s3";

/// The bucket most of the declared artifacts live in.
const BUCKET: &str = "greengrass-artifact-store";
/// A second bucket, so an artifact's object is not resolved by its key alone.
const OTHER_BUCKET: &str = "greengrass-artifact-overflow";

/// The component the rows read artifacts of.
const COMPONENT: &str = "com.example.Artifacts";
/// The version of it they address.
const VERSION: &str = "1.0.0";
/// A component version published from a recipe declaring NO artifact list at all.
const BARE_COMPONENT: &str = "com.example.NoArtifacts";

/// The key of the artifact declared by the first manifest: a key carrying a prefix, which
/// is the common shape of a component artifact's address.
const FIRST_KEY: &str = "components/com.example.Artifacts/first.zip";
/// The bytes stored at [`FIRST_KEY`], which the followed URL must serve.
const FIRST_BYTES: &str = "the first artifact's bytes";
/// The key of the artifact declared by a LATER manifest, in the other bucket.
const SECOND_KEY: &str = "second.zip";
/// The bytes stored at [`SECOND_KEY`].
const SECOND_BYTES: &str = "the second artifact's bytes";

/// One answer, reduced to what the rows assert on.
#[derive(Debug)]
struct Answer {
    status: u16,
    error_code: String,
    body: Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// A string member of a JSON answer, addressed by JSON pointer.
    fn text(&self, pointer: &str) -> String {
        self.json()
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the URL Greengrass signs is followed
/// back into this same router, so the object S3 holds is the one the download serves.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // These rows exercise handler behaviour, not request signing: each call carries a
    // credential scope so the router can name the target service.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
/// routes the request.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response =
        Box::pin(router.dispatch(builder.body(body).expect("a well-formed request"))).await;
    let status = response.status().as_u16();
    let header_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = response.into_body();
    let error_code = if header_code.is_empty() {
        serde_json::from_slice::<Value>(&body)
            .ok()
            .as_ref()
            .and_then(|parsed| parsed.get("__type"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        header_code
    };
    Answer {
        status,
        error_code,
        body,
    }
}

/// One call on Amazon S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    dispatch(
        router,
        method,
        &host,
        &format!("https://{host}{path}"),
        &[
            ("content-type", "application/octet-stream"),
            ("authorization", &authorization(S3)),
        ],
        Bytes::from(body.to_owned()),
    )
    .await
}

/// One call on the Greengrass V2 `restJson1` surface.
async fn greengrass(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Answer {
    let host = format!("greengrass.{REGION}.amazonaws.com");
    let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
    dispatch(
        router,
        method,
        &host,
        &format!("https://{host}{path}"),
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization(GREENGRASS)),
        ],
        payload,
    )
    .await
}

/// Follow a URL a service ANNOUNCED, the way a client follows a pre-signed one: at the
/// host the URL names and carrying no `Authorization` header at all — the signature the
/// URL carries is what makes the announced address a pre-signed one.
async fn followed(router: &MultiServiceRouter, url: &str) -> Answer {
    let host = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .unwrap_or_default()
        .to_string();
    dispatch(router, "GET", &host, url, &[], Bytes::new()).await
}

/// `s3:CreateBucket`.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let created = s3(router, "PUT", &format!("/{bucket}"), "").await;
    assert_eq!(
        created.status,
        200,
        "setup: CreateBucket({bucket:?}) must answer 200, got {} {:?}",
        created.status,
        created.body_text()
    );
}

/// `s3:PutObject` — the write that puts an artifact's bytes where the announced URL finds
/// them.
async fn put_object(router: &MultiServiceRouter, bucket: &str, key: &str, body: &str) {
    let written = s3(router, "PUT", &format!("/{bucket}/{key}"), body).await;
    assert_eq!(
        written.status,
        200,
        "setup: PutObject({bucket:?}, {key:?}) must answer 200, got {} {:?}",
        written.status,
        written.body_text()
    );
}

/// `greengrassv2:CreateComponentVersion` from an inline recipe.
async fn publish(router: &MultiServiceRouter, document: &str) -> Answer {
    greengrass(
        router,
        "POST",
        "/greengrass/v2/createComponentVersion",
        Some(&serde_json::json!({ "inlineRecipe": base64::encode(document.as_bytes()) })),
    )
    .await
}

/// The component-version ARN this emulator mints for `name`@`version`.
fn version_arn(name: &str, version: &str) -> String {
    format!("arn:aws:greengrass:{REGION}:{ACCOUNT}:components:{name}:versions:{version}")
}

/// An ARN as a client percent-encodes a non-greedy `@httpLabel`: both separators it
/// carries are encoded, so a value holding either stays ONE path segment.
fn label(arn: &str) -> String {
    arn.replace(':', "%3A").replace('/', "%2F")
}

/// `greengrassv2:GetComponentVersionArtifact` for `artifact_name` on `arn`. The artifact
/// name is a GREEDY label, so a name holding `/` travels as it is written.
async fn artifact_read(router: &MultiServiceRouter, arn: &str, artifact_name: &str) -> Answer {
    greengrass(
        router,
        "GET",
        &format!(
            "/greengrass/v2/components/{}/artifacts/{artifact_name}",
            label(arn)
        ),
        None,
    )
    .await
}

/// The recipe the rows read artifacts of: two platform manifests, each declaring its own
/// artifact list, and one entry that names a container image rather than an object.
fn artifact_recipe() -> String {
    format!(
        "RecipeFormatVersion: '2020-01-25'\n\
         ComponentName: {COMPONENT}\n\
         ComponentVersion: {VERSION}\n\
         Manifests:\n\
         \x20 - Platform:\n\
         \x20     os: linux\n\
         \x20   Artifacts:\n\
         \x20     - Uri: \"s3://{BUCKET}/{FIRST_KEY}\"\n\
         \x20     - Uri: \"docker://registry.example/image:1\"\n\
         \x20 - Platform:\n\
         \x20     os: windows\n\
         \x20   Artifacts:\n\
         \x20     - Uri: \"s3://{OTHER_BUCKET}/{SECOND_KEY}\"\n"
    )
}

/// A recipe for a version that declares no artifact list at all.
fn bare_recipe() -> String {
    format!(
        "RecipeFormatVersion: '2020-01-25'\nComponentName: {BARE_COMPONENT}\n\
         ComponentVersion: {VERSION}\n"
    )
}

/// The sentence AWS refuses an artifact read with, written here as the literal AWS answers
/// rather than read from the implementation.
fn not_found_sentence(name: &str, version: &str, artifact_name: &str) -> String {
    format!(
        "Component ({name}:{version}) or Artifact ({artifact_name}) for account {ACCOUNT} \
         does not exist"
    )
}

/// The two objects and the two component versions every row reads.
async fn with_published_artifacts() -> MultiServiceRouter {
    let router = emulator();
    create_bucket(&router, BUCKET).await;
    create_bucket(&router, OTHER_BUCKET).await;
    put_object(&router, BUCKET, FIRST_KEY, FIRST_BYTES).await;
    put_object(&router, OTHER_BUCKET, SECOND_KEY, SECOND_BYTES).await;
    for document in [artifact_recipe(), bare_recipe()] {
        let published = publish(&router, &document).await;
        assert_eq!(
            published.status,
            201,
            "setup: CreateComponentVersion must answer 201, got {} {:?}",
            published.status,
            published.body_text()
        );
    }
    router
}

/// RULE: a name the recipe declares an object artifact under is answered with a pre-signed
/// URL for that object, and following it serves the bytes S3 holds.
#[tokio::test]
async fn a_declared_artifact_is_served_from_the_object_its_recipe_names() {
    let router = with_published_artifacts().await;
    let arn = version_arn(COMPONENT, VERSION);

    // (case, the name asked for, the object path the URL must address, the bytes it serves)
    let rows: &[(&str, String, String, &str)] = &[
        (
            "the whole section after the scheme",
            format!("{BUCKET}/{FIRST_KEY}"),
            format!("/{BUCKET}/{FIRST_KEY}"),
            FIRST_BYTES,
        ),
        (
            "the file name at the end of that section",
            "first.zip".to_string(),
            format!("/{BUCKET}/{FIRST_KEY}"),
            FIRST_BYTES,
        ),
        (
            "an artifact declared by a later manifest, in another bucket",
            SECOND_KEY.to_string(),
            format!("/{OTHER_BUCKET}/{SECOND_KEY}"),
            SECOND_BYTES,
        ),
        (
            "that same artifact by its whole section",
            format!("{OTHER_BUCKET}/{SECOND_KEY}"),
            format!("/{OTHER_BUCKET}/{SECOND_KEY}"),
            SECOND_BYTES,
        ),
    ];

    for (case, artifact_name, object_path, expected_bytes) in rows {
        let answered = artifact_read(&router, &arn, artifact_name).await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: a declared artifact must answer 200, got {} {:?}",
            answered.status,
            answered.body_text()
        );
        let url = answered.text("/preSignedUrl");
        assert!(
            url.contains(object_path.as_str()) && url.contains("X-Amz-Signature="),
            "case {case:?}: expected a pre-signed URL addressing {object_path:?}, got {url:?}"
        );
        assert!(
            !url.contains("amazonaws.com"),
            "case {case:?}: the URL must lead back to this emulator, got {url:?}"
        );

        let downloaded = followed(&router, &url).await;
        assert_eq!(
            downloaded.status,
            200,
            "case {case:?}: following the URL must answer 200, got {} {:?}",
            downloaded.status,
            downloaded.body_text()
        );
        assert_eq!(
            downloaded.body_text(),
            *expected_bytes,
            "case {case:?}: the download must serve the object's bytes, got {:?}",
            downloaded.body_text()
        );
    }
}

/// RULE: a search that comes up empty — a name no artifact of that version is declared
/// under, or an ARN naming no stored version — is the modelled 400 and the one sentence
/// AWS words both halves of it with.
#[tokio::test]
async fn an_artifact_the_recipe_declares_nowhere_is_refused_with_aws_sentence() {
    let router = with_published_artifacts().await;

    // (case, the component, the version, the name asked for)
    let rows: &[(&str, &str, &str, &str)] = &[
        (
            "a name the recipe declares nowhere",
            COMPONENT,
            VERSION,
            "third.zip",
        ),
        (
            "a declared name differing only in case",
            COMPONENT,
            VERSION,
            "First.zip",
        ),
        (
            "an entry naming a container image rather than an object",
            COMPONENT,
            VERSION,
            "registry.example/image:1",
        ),
        (
            "a version whose recipe declares no artifact list",
            BARE_COMPONENT,
            VERSION,
            "first.zip",
        ),
        (
            "a version of that component that was never published",
            COMPONENT,
            "2.0.0",
            "first.zip",
        ),
        (
            "a component that was never published",
            "com.example.Absent",
            VERSION,
            "first.zip",
        ),
    ];

    for (case, component, version, artifact_name) in rows {
        let arn = version_arn(component, version);
        let answered = artifact_read(&router, &arn, artifact_name).await;
        assert_eq!(
            answered.status,
            400,
            "case {case:?}: an artifact that does not exist must be refused 400, got {} {:?}",
            answered.status,
            answered.body_text()
        );
        assert_eq!(
            answered.error_code, "ValidationException",
            "case {case:?}: expected ValidationException, got {:?}",
            answered.error_code
        );
        assert_eq!(
            answered.text("/message"),
            not_found_sentence(component, version, artifact_name),
            "case {case:?}: expected the sentence AWS states, got {:?}",
            answered.body_text()
        );
    }
}

/// RULE: the artifact read is a REPORT: it addresses the same object every time and leaves
/// the recipe it read, and the object it named, exactly as they were.
#[tokio::test]
async fn reading_an_artifact_changes_nothing_a_later_caller_sees() {
    let router = with_published_artifacts().await;
    let arn = version_arn(COMPONENT, VERSION);
    let object_path = format!("/{BUCKET}/{FIRST_KEY}");

    for read in ["the first read", "the same read again"] {
        let answered = artifact_read(&router, &arn, "first.zip").await;
        assert_eq!(
            answered.status,
            200,
            "case {read:?}: the artifact read must answer 200, got {} {:?}",
            answered.status,
            answered.body_text()
        );
        let url = answered.text("/preSignedUrl");
        assert!(
            url.contains(object_path.as_str()),
            "case {read:?}: expected the same object {object_path:?}, got {url:?}"
        );
    }

    // The recipe the read resolved the artifact against still reports both of its object
    // artifacts, and the object itself still serves its bytes.
    let recipe = greengrass(
        &router,
        "GET",
        &format!(
            "/greengrass/v2/components/{}?recipeOutputFormat=JSON",
            label(&arn)
        ),
        None,
    )
    .await;
    assert_eq!(
        recipe.status,
        200,
        "case \"after the reads\": GetComponent must answer 200, got {} {:?}",
        recipe.status,
        recipe.body_text()
    );
    let document = base64::decode(recipe.text("/recipe")).unwrap_or_default();
    let document = String::from_utf8_lossy(&document).into_owned();
    for declared in [
        format!("s3://{BUCKET}/{FIRST_KEY}"),
        format!("s3://{OTHER_BUCKET}/{SECOND_KEY}"),
    ] {
        assert!(
            document.contains(declared.as_str()),
            "case \"after the reads\": the stored recipe must still declare {declared:?}, \
             got {document:?}"
        );
    }

    let held = s3(&router, "GET", &format!("/{BUCKET}/{FIRST_KEY}"), "").await;
    assert_eq!(
        (held.status, held.body_text()),
        (200, FIRST_BYTES.to_string()),
        "case \"after the reads\": the object must be untouched, got {} {:?}",
        held.status,
        held.body_text()
    );
}
