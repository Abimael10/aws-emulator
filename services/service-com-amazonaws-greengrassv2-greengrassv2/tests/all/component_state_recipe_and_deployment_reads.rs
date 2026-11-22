// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a Greengrass V2 read reports about the state a WRITE
//! derived, rather than about the bytes the write carried.
//!
//! The rules under test:
//!
//! * a component version published from an `inlineRecipe` is VALIDATED by the time
//!   `CreateComponentVersion` answers, so the create reports the state at the
//!   instant it accepted the request (`REQUESTED`) and every call that follows —
//!   `DescribeComponent`, and the `ConflictException` a republish is refused with —
//!   reports the validated version's terminal `DEPLOYABLE`;
//! * the service stores the recipe document it DERIVED from the submitted one —
//!   every submitted member, in the submitted order, plus the `ComponentType` the
//!   service assigns, which the recipe reference places immediately after
//!   `ComponentVersion` — and serializes that document for every reader, so a
//!   recipe read back as JSON is COMPACT (no space after `:` or `,`) and carries
//!   the assigned type, whatever spacing, member order or `ComponentType` the
//!   submitted bytes had;
//! * the same stored document carries the recipe members whose default is the
//!   EMPTY collection — the recipe's `Lifecycle`, and each manifest's `Lifecycle`
//!   and `Artifacts` — whether or not the submitted document declared them, so a
//!   client reading one always finds a collection; and
//! * `GetDeployment` reports the deployment's `components` map and its `tags` map,
//!   each EMPTY rather than absent when the deployment holds nothing in it; and
//! * `GetComponentVersionArtifact` resolves the artifact a request names against the
//!   artifact URIs that same stored recipe declares, answering a pre-signed URL for the
//!   object the matching URI names and refusing a name it declares no object artifact
//!   under — and every name, when the ARN names no stored version — with the one
//!   sentence AWS words both halves of that search with; and
//! * that same read judges its `arn` against the component-version ARN SHAPE first, so a
//!   value outside that shape is refused with the sentence naming the shape while any
//!   value inside it that names no stored version is refused with the does-not-exist
//!   sentence — two client refusals, never a server fault.
//!
//! Every assertion drives the real `restJson1` front door: the request is built the
//! way an SDK builds it, dispatched through [`MultiServiceRouter`], and judged on
//! the HTTP status and the JSON body. Nothing reaches into the state struct, so a
//! handler that answered from the request instead of from what it persisted fails
//! here.
//!
//! The recipe rows differ along what the rule generalises over — the submitted
//! wire format, the spacing of the submitted bytes, the ORDER of the submitted
//! members, whether a `ComponentType` was submitted, which empty members the
//! document carries, and the kind of every scalar in it — not along the component
//! names, which the rule says nothing about.

use std::sync::Arc;

use aws_smithy_types::base64;
use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_com_amazonaws_greengrassv2_greengrassv2::state::COMPONENT_TYPE_GENERIC;
use service_com_amazonaws_greengrassv2_greengrassv2::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region these requests are signed for; every minted ARN carries it.
const REGION: &str = "us-west-2";

/// The account id every emulated ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The emulator front door with only this service mounted, over ONE shared engine —
/// exactly how `core/router` assembles it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Dispatch one signed request and return its status and parsed JSON body.
async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Vec<u8>) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://greengrass.{REGION}.amazonaws.com{path}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/greengrass/aws4_request, \
                 SignedHeaders=host, Signature=00"
            ),
        )
        .header("content-type", "application/json")
        .body(Bytes::from(body))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or(Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    (status, value)
}

async fn post(router: &MultiServiceRouter, path: &str, body: &Value) -> (u16, Value) {
    let bytes = serde_json::to_vec(body).expect("a serializable request body");
    call(router, "POST", path, bytes).await
}

async fn get(router: &MultiServiceRouter, path: &str) -> (u16, Value) {
    call(router, "GET", path, Vec::new()).await
}

/// A string member of a JSON object, or the empty string when absent.
fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The wire error code of a `restJson1` error body.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .to_string()
}

/// An ARN as a client percent-encodes a non-greedy `@httpLabel`.
fn encoded_label(arn: &str) -> String {
    arn.replace(':', "%3A").replace('/', "%2F")
}

/// The component-version ARN this emulator mints for `name`@`version`.
fn version_arn(name: &str, version: &str) -> String {
    format!("arn:aws:greengrass:{REGION}:{ACCOUNT}:components:{name}:versions:{version}")
}

/// The deployment ARN of `deployment_id`.
fn deployment_arn(deployment_id: &str) -> String {
    format!("arn:aws:greengrass:{REGION}:{ACCOUNT}:deployments:{deployment_id}")
}

/// An IoT thing ARN, which is what a deployment targets.
fn thing_target(name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT}:thing/{name}")
}

/// `CreateComponentVersion` with `document` as the inline recipe.
async fn publish(router: &MultiServiceRouter, document: &str) -> (u16, Value) {
    post(
        router,
        "/greengrass/v2/createComponentVersion",
        &json!({ "inlineRecipe": base64::encode(document.as_bytes()) }),
    )
    .await
}

/// `GetComponent` on `arn` in `format`, answering the decoded recipe document.
async fn stored_recipe(router: &MultiServiceRouter, arn: &str, format: &str) -> (u16, String) {
    let (status, body) = get(
        router,
        &format!(
            "/greengrass/v2/components/{}?recipeOutputFormat={format}",
            encoded_label(arn)
        ),
    )
    .await;
    let blob = base64::decode(text(&body, "recipe")).unwrap_or_default();
    (
        status,
        String::from_utf8(blob).unwrap_or_else(|_| format!("<not UTF-8> {body}")),
    )
}

/// The member NAMES of a recipe document, in the order it declares them.
fn member_names(document: &str) -> Vec<String> {
    yaml_rust2::YamlLoader::load_from_str(document)
        .unwrap_or_default()
        .first()
        .and_then(yaml_rust2::Yaml::as_hash)
        .map(|map| {
            map.iter()
                .map(|(key, _)| key.as_str().unwrap_or("<not a string>").to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// A scalar member of a recipe document, or `None` when it declares none.
fn member(document: &str, key: &str) -> Option<String> {
    yaml_rust2::YamlLoader::load_from_str(document)
        .ok()?
        .first()?
        .as_hash()?
        .iter()
        .find(|(member, _)| member.as_str() == Some(key))
        .and_then(|(_, value)| value.as_str())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// The recipe table
// ---------------------------------------------------------------------------

/// One row of the recipe table: a submitted recipe document and the JSON document
/// the service must store for it.
struct RecipeCase {
    label: &'static str,
    /// The bytes submitted as `inlineRecipe`.
    submitted: &'static str,
    /// The `RecipeOutputFormat` the submitted bytes are written in.
    format: &'static str,
    name: &'static str,
    version: &'static str,
    /// The document `GetComponent` must answer with for `recipeOutputFormat=JSON`,
    /// byte for byte.
    stored_json: &'static str,
}

/// Recipe member names and nesting below come from the IoT Greengrass component
/// recipe reference (`RecipeFormatVersion`, `ComponentName`, `ComponentVersion`,
/// `ComponentDescription`, `ComponentPublisher`, `ComponentConfiguration`,
/// `Manifests[].Platform`, `Manifests[].Artifacts`, `Lifecycle`), and
/// `ComponentType` is the member the service assigns.
const RECIPES: &[RecipeCase] = &[
    RecipeCase {
        label: "json spaced the way a JSON encoder writes it, with empty members",
        submitted: r#"{"RecipeFormatVersion": "2020-01-25", "ComponentName": "com.example.Spaced", "ComponentVersion": "1.0.0", "ComponentDescription": "A probe component", "ComponentPublisher": "probe", "ComponentConfiguration": {"DefaultConfiguration": {}}, "Manifests": [{"Platform": {"os": "all"}, "Lifecycle": {}, "Artifacts": []}], "Lifecycle": {}}"#,
        format: "JSON",
        name: "com.example.Spaced",
        version: "1.0.0",
        stored_json: r#"{"RecipeFormatVersion":"2020-01-25","ComponentName":"com.example.Spaced","ComponentVersion":"1.0.0","ComponentType":"aws.greengrass.generic","ComponentDescription":"A probe component","ComponentPublisher":"probe","ComponentConfiguration":{"DefaultConfiguration":{}},"Manifests":[{"Platform":{"os":"all"},"Lifecycle":{},"Artifacts":[]}],"Lifecycle":{}}"#,
    },
    RecipeCase {
        label: "the same document already compact: only the assigned type is added",
        submitted: r#"{"RecipeFormatVersion":"2020-01-25","ComponentName":"com.example.Compact","ComponentVersion":"1.0.0","ComponentConfiguration":{"DefaultConfiguration":{}},"Manifests":[{"Platform":{"os":"all"},"Lifecycle":{},"Artifacts":[]}],"Lifecycle":{}}"#,
        format: "JSON",
        name: "com.example.Compact",
        version: "1.0.0",
        stored_json: r#"{"RecipeFormatVersion":"2020-01-25","ComponentName":"com.example.Compact","ComponentVersion":"1.0.0","ComponentType":"aws.greengrass.generic","ComponentConfiguration":{"DefaultConfiguration":{}},"Manifests":[{"Platform":{"os":"all"},"Lifecycle":{},"Artifacts":[]}],"Lifecycle":{}}"#,
    },
    RecipeCase {
        label: "members submitted in another order keep it, and the type follows the version",
        submitted: r#"{"ComponentName":"com.example.Ordered","ComponentDescription":"Out of order","ComponentVersion":"2.0.0","RecipeFormatVersion":"2020-01-25"}"#,
        format: "JSON",
        name: "com.example.Ordered",
        version: "2.0.0",
        stored_json: r#"{"ComponentName":"com.example.Ordered","ComponentDescription":"Out of order","ComponentVersion":"2.0.0","ComponentType":"aws.greengrass.generic","RecipeFormatVersion":"2020-01-25","Lifecycle":{}}"#,
    },
    RecipeCase {
        label: "a submitted ComponentType does not decide the stored one",
        submitted: r#"{"RecipeFormatVersion":"2020-01-25","ComponentType":"aws.greengrass.lambda","ComponentName":"com.example.Typed","ComponentVersion":"3.0.0"}"#,
        format: "JSON",
        name: "com.example.Typed",
        version: "3.0.0",
        stored_json: r#"{"RecipeFormatVersion":"2020-01-25","ComponentName":"com.example.Typed","ComponentVersion":"3.0.0","ComponentType":"aws.greengrass.generic","Lifecycle":{}}"#,
    },
    RecipeCase {
        label: "scalars of every JSON kind, including a string that must be escaped",
        submitted: r#"{"RecipeFormatVersion":"2020-01-25","ComponentName":"com.example.Scalars","ComponentVersion":"4.5.6","ComponentConfiguration":{"DefaultConfiguration":{"Count":3,"Ratio":1.5,"Enabled":true,"Absent":null,"Quoted":"say \"hi\"\nand \\ bye"}},"Manifests":[]}"#,
        format: "JSON",
        name: "com.example.Scalars",
        version: "4.5.6",
        stored_json: r#"{"RecipeFormatVersion":"2020-01-25","ComponentName":"com.example.Scalars","ComponentVersion":"4.5.6","ComponentType":"aws.greengrass.generic","ComponentConfiguration":{"DefaultConfiguration":{"Count":3,"Ratio":1.5,"Enabled":true,"Absent":null,"Quoted":"say \"hi\"\nand \\ bye"}},"Manifests":[],"Lifecycle":{}}"#,
    },
    RecipeCase {
        label: "a manifest with nothing but a platform: the empty defaults are supplied",
        submitted: r#"{"RecipeFormatVersion":"2020-01-25","ComponentName":"com.example.Bare","ComponentVersion":"7.0.0","Manifests":[{"Platform":{"os":"all"}},{"Platform":{"os":"linux"},"Artifacts":[{"Uri":"s3://bucket/key"}]}]}"#,
        format: "JSON",
        name: "com.example.Bare",
        version: "7.0.0",
        stored_json: r#"{"RecipeFormatVersion":"2020-01-25","ComponentName":"com.example.Bare","ComponentVersion":"7.0.0","ComponentType":"aws.greengrass.generic","Manifests":[{"Platform":{"os":"all"},"Lifecycle":{},"Artifacts":[]},{"Platform":{"os":"linux"},"Artifacts":[{"Uri":"s3://bucket/key"}],"Lifecycle":{}}],"Lifecycle":{}}"#,
    },
    RecipeCase {
        label: "a yaml recipe, whose stored document carries the assigned type too",
        submitted: "RecipeFormatVersion: '2020-01-25'\nComponentName: com.example.Yaml\nComponentVersion: 5.0.0\nComponentPublisher: probe\nManifests:\n  - Platform:\n      os: linux\n    Lifecycle: {}\n    Artifacts: []\nLifecycle: {}\n",
        format: "YAML",
        name: "com.example.Yaml",
        version: "5.0.0",
        stored_json: r#"{"RecipeFormatVersion":"2020-01-25","ComponentName":"com.example.Yaml","ComponentVersion":"5.0.0","ComponentType":"aws.greengrass.generic","ComponentPublisher":"probe","Manifests":[{"Platform":{"os":"linux"},"Lifecycle":{},"Artifacts":[]}],"Lifecycle":{}}"#,
    },
    RecipeCase {
        label: "a yaml recipe that declares its own type, which the service replaces",
        submitted: "RecipeFormatVersion: '2020-01-25'\nComponentName: com.example.YamlTyped\nComponentType: aws.greengrass.nucleus\nComponentVersion: 6.0.0\n",
        format: "YAML",
        name: "com.example.YamlTyped",
        version: "6.0.0",
        stored_json: r#"{"RecipeFormatVersion":"2020-01-25","ComponentName":"com.example.YamlTyped","ComponentVersion":"6.0.0","ComponentType":"aws.greengrass.generic","Lifecycle":{}}"#,
    },
];

#[tokio::test]
async fn a_stored_recipe_is_the_document_the_service_derived_from_the_submitted_one() {
    let router = emulator();

    for case in RECIPES {
        let (status, body) = publish(&router, case.submitted).await;
        assert_eq!(
            status, 201,
            "case {:?}: CreateComponentVersion must answer 201, got {status} {body}",
            case.label
        );

        let arn = version_arn(case.name, case.version);

        // Read as JSON: the stored document, serialized compactly, member for
        // member and byte for byte.
        let (status, document) = stored_recipe(&router, &arn, "JSON").await;
        assert_eq!(
            status, 200,
            "case {:?}: GetComponent must answer 200, got {status} {document}",
            case.label
        );
        assert_eq!(
            document, case.stored_json,
            "case {:?}: expected the derived document {:?}, got {document:?}",
            case.label, case.stored_json
        );

        // Read in the format the recipe was submitted in: the same document, with
        // the same members in the same order, and the same assigned type.
        let (status, native) = stored_recipe(&router, &arn, case.format).await;
        assert_eq!(
            status, 200,
            "case {:?}: GetComponent as {} must answer 200, got {status} {native}",
            case.label, case.format
        );
        assert_eq!(
            member_names(&native),
            member_names(case.stored_json),
            "case {:?}: the stored {} document must declare the same members in the \
             same order, got {native:?}",
            case.label,
            case.format
        );
        assert_eq!(
            member(&native, "ComponentType").as_deref(),
            Some(COMPONENT_TYPE_GENERIC),
            "case {:?}: the stored {} document must carry the assigned type, got {native:?}",
            case.label,
            case.format
        );
    }
}

#[tokio::test]
async fn a_published_version_is_validated_by_the_time_the_create_answers() {
    let router = emulator();

    // One row per submitted wire format, plus a version form the state machine does
    // not depend on either.
    let json_case = RECIPES
        .iter()
        .find(|case| case.format == "JSON")
        .expect("a JSON row");
    let yaml_case = RECIPES
        .iter()
        .find(|case| case.format == "YAML")
        .expect("a YAML row");
    for (label, document, name, version) in [
        (
            "json",
            json_case.submitted.to_string(),
            json_case.name,
            json_case.version,
        ),
        (
            "yaml",
            yaml_case.submitted.to_string(),
            yaml_case.name,
            yaml_case.version,
        ),
        (
            "a prerelease version",
            r#"{"RecipeFormatVersion":"2020-01-25","ComponentName":"com.example.Prerelease","ComponentVersion":"0.1.0-alpha.1"}"#.to_string(),
            "com.example.Prerelease",
            "0.1.0-alpha.1",
        ),
    ] {
        let (status, created) = publish(&router, &document).await;
        assert_eq!(
            status, 201,
            "case {label:?}: CreateComponentVersion must answer 201, got {status} {created}"
        );
        // The create answers the state at the instant it ACCEPTED the request,
        // which is before it validated the recipe.
        assert_eq!(
            text(&created["status"], "componentState"),
            "REQUESTED",
            "case {label:?}: the create must report the state at acceptance, got {created}"
        );

        // By the time it answered, the recipe was validated — so every read that
        // follows reports the terminal state, with no wait in between, and a
        // second read reports the same thing.
        let arn = version_arn(name, version);
        for read in ["the first read", "the same read again"] {
            let (status, described) = get(
                &router,
                &format!(
                    "/greengrass/v2/components/{}/metadata",
                    encoded_label(&arn)
                ),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {label:?}, {read}: DescribeComponent must answer 200, \
                 got {status} {described}"
            );
            assert_eq!(
                text(&described["status"], "componentState"),
                "DEPLOYABLE",
                "case {label:?}, {read}: a validated version must be DEPLOYABLE, \
                 got {described}"
            );
        }

        // The refusal a republish is answered with carries that same stored state.
        let (status, conflict) = publish(&router, &document).await;
        assert_eq!(
            status, 409,
            "case {label:?}: republishing must answer 409, got {status} {conflict}"
        );
        assert_eq!(
            text(&conflict, "message"),
            format!(
                "Component [{name} : {version}] for account [{ACCOUNT}] already exists \
                 with state: [DEPLOYABLE]"
            ),
            "case {label:?}: the conflict must name the stored version's state, got {conflict}"
        );
    }
}

// ---------------------------------------------------------------------------
// Deployment reads
// ---------------------------------------------------------------------------

/// One row of the deployment table: the `components` and `tags` members the create
/// carries, and what `GetDeployment` must report for each.
struct DeploymentCase {
    label: &'static str,
    /// The `components` member of the create request, or `None` when it names none.
    components: Option<fn() -> Value>,
    /// The `tags` member of the create request, or `None` when it carries none.
    tags: Option<fn() -> Value>,
}

/// The rows differ in whether the deployment specifies components at all, how many
/// it specifies and how fully, and whether it was created with tags — the two map
/// members `GetDeployment` reports.
fn deployment_cases() -> Vec<DeploymentCase> {
    vec![
        DeploymentCase {
            label: "no components member and one tag",
            components: None,
            tags: Some(|| json!({ "owner": "platform-team" })),
        },
        DeploymentCase {
            label: "an explicitly empty components map and no tags",
            components: Some(|| json!({})),
            tags: None,
        },
        DeploymentCase {
            label: "one component and several tags",
            components: Some(|| json!({ "com.example.One": { "componentVersion": "1.0.0" } })),
            tags: Some(|| json!({ "owner": "platform-team", "tier": "test" })),
        },
        DeploymentCase {
            label: "two components, one of them configured",
            components: Some(|| {
                json!({
                    "com.example.One": { "componentVersion": "1.0.0" },
                    "com.example.Two": {
                        "componentVersion": "2.0.0",
                        "configurationUpdate": { "merge": "{\"Message\":\"hi\"}" }
                    }
                })
            }),
            tags: None,
        },
    ]
}

#[tokio::test]
async fn get_deployment_reports_the_components_and_tags_maps_it_stored() {
    let router = emulator();

    for (row, case) in deployment_cases().into_iter().enumerate() {
        let mut request = Map::new();
        request.insert(
            "targetArn".to_string(),
            Value::String(thing_target(&format!("ReadCore{row}"))),
        );
        if let Some(components) = case.components {
            request.insert("components".to_string(), components());
        }
        if let Some(tags) = case.tags {
            request.insert("tags".to_string(), tags());
        }
        let request = Value::Object(request);

        let (status, created) = post(&router, "/greengrass/v2/deployments", &request).await;
        assert_eq!(
            status, 201,
            "case {:?}: CreateDeployment must answer 201, got {status} {created}",
            case.label
        );
        let id = text(&created, "deploymentId");

        // `components` and `tags` are reported as maps whether or not the
        // deployment holds anything in them: the empty map, never an absent member.
        let expected_components = case.components.map_or_else(|| json!({}), |make| make());
        let expected_tags = case.tags.map_or_else(|| json!({}), |make| make());

        let (status, stored) = get(&router, &format!("/greengrass/v2/deployments/{id}")).await;
        assert_eq!(
            status, 200,
            "case {:?}: GetDeployment must answer 200, got {status} {stored}",
            case.label
        );
        assert_eq!(
            stored.get("components"),
            Some(&expected_components),
            "case {:?}: GetDeployment must report the stored components map, got {stored}",
            case.label
        );
        assert_eq!(
            stored.get("tags"),
            Some(&expected_tags),
            "case {:?}: GetDeployment must report the stored tags map, got {stored}",
            case.label
        );

        // The tagging route reads the same stored tags through the deployment's ARN.
        let (status, listed) = get(
            &router,
            &format!("/tags/{}", encoded_label(&deployment_arn(&id))),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: ListTagsForResource must answer 200, got {status} {listed}",
            case.label
        );
        assert_eq!(
            listed.get("tags"),
            Some(&expected_tags),
            "case {:?}: the tag listing must report the create's tags, got {listed}",
            case.label
        );
    }
}

// ---------------------------------------------------------------------------
// Artifact reads
// ---------------------------------------------------------------------------

/// The component the artifact rows read, and the version they address.
const ARTIFACT_COMPONENT: &str = "com.example.WithArtifacts";
const ARTIFACT_VERSION: &str = "1.0.0";

/// The buckets and keys the recipe below declares its artifacts at. Artifact member names
/// and nesting come from the IoT Greengrass component recipe reference
/// (`Manifests[].Artifacts[].Uri`).
const ARTIFACT_BUCKET: &str = "component-artifact-store";
const PREFIXED_KEY: &str = "releases/1.0.0/payload.zip";
const OTHER_BUCKET: &str = "component-artifact-overflow";
const PLAIN_KEY: &str = "sidecar.jar";

/// `GetComponentVersionArtifact` for `artifact_name` on `arn`: `artifactName` is a GREEDY
/// label, so a name holding `/` travels as it is written.
async fn artifact_read(
    router: &MultiServiceRouter,
    arn: &str,
    artifact_name: &str,
) -> (u16, Value) {
    get(
        router,
        &format!(
            "/greengrass/v2/components/{}/artifacts/{artifact_name}",
            encoded_label(arn)
        ),
    )
    .await
}

/// A recipe whose two manifests declare one object artifact each, plus an entry naming a
/// container image rather than an object.
fn artifact_recipe() -> String {
    format!(
        "RecipeFormatVersion: '2020-01-25'\n\
         ComponentName: {ARTIFACT_COMPONENT}\n\
         ComponentVersion: {ARTIFACT_VERSION}\n\
         Manifests:\n\
         \x20 - Platform:\n\
         \x20     os: linux\n\
         \x20   Artifacts:\n\
         \x20     - Uri: \"s3://{ARTIFACT_BUCKET}/{PREFIXED_KEY}\"\n\
         \x20     - Uri: \"docker://registry.example/image:1\"\n\
         \x20 - Platform:\n\
         \x20     os: windows\n\
         \x20   Artifacts:\n\
         \x20     - Uri: \"s3://{OTHER_BUCKET}/{PLAIN_KEY}\"\n"
    )
}

/// The sentence AWS refuses an artifact read with, written here as the literal AWS answers
/// rather than read from the implementation.
fn artifact_not_found(name: &str, version: &str, artifact_name: &str) -> String {
    format!(
        "Component ({name}:{version}) or Artifact ({artifact_name}) for account {ACCOUNT} \
         does not exist"
    )
}

#[tokio::test]
async fn an_artifact_read_answers_from_the_recipe_the_create_persisted() {
    let router = emulator();
    let (status, body) = publish(&router, &artifact_recipe()).await;
    assert_eq!(
        status, 201,
        "publishing the artifact fixture must succeed, got {status} {body}"
    );
    // A version whose recipe declares no artifact list at all, so the rows span a version
    // that has artifacts and one that has none.
    let bare = format!(
        "RecipeFormatVersion: '2020-01-25'\nComponentName: com.example.Artifactless\n\
         ComponentVersion: {ARTIFACT_VERSION}\n"
    );
    let (status, body) = publish(&router, &bare).await;
    assert_eq!(
        status, 201,
        "publishing the artifactless fixture must succeed, got {status} {body}"
    );

    // (case, the component, the version, the name asked for, the object path the URL must
    // address when the artifact is one the recipe declares)
    let rows: &[(&str, &str, &str, String, Option<String>)] = &[
        (
            "the whole section after the scheme",
            ARTIFACT_COMPONENT,
            ARTIFACT_VERSION,
            format!("{ARTIFACT_BUCKET}/{PREFIXED_KEY}"),
            Some(format!("/{ARTIFACT_BUCKET}/{PREFIXED_KEY}")),
        ),
        (
            "the file name at the end of that section",
            ARTIFACT_COMPONENT,
            ARTIFACT_VERSION,
            "payload.zip".to_string(),
            Some(format!("/{ARTIFACT_BUCKET}/{PREFIXED_KEY}")),
        ),
        (
            "an artifact declared by a later manifest, in another bucket",
            ARTIFACT_COMPONENT,
            ARTIFACT_VERSION,
            PLAIN_KEY.to_string(),
            Some(format!("/{OTHER_BUCKET}/{PLAIN_KEY}")),
        ),
        (
            "a name the recipe declares nowhere",
            ARTIFACT_COMPONENT,
            ARTIFACT_VERSION,
            "absent.zip".to_string(),
            None,
        ),
        (
            "a declared name differing only in case",
            ARTIFACT_COMPONENT,
            ARTIFACT_VERSION,
            "Payload.zip".to_string(),
            None,
        ),
        (
            "an entry naming a container image rather than an object",
            ARTIFACT_COMPONENT,
            ARTIFACT_VERSION,
            "registry.example/image:1".to_string(),
            None,
        ),
        (
            "a version whose recipe declares no artifact list",
            "com.example.Artifactless",
            ARTIFACT_VERSION,
            "payload.zip".to_string(),
            None,
        ),
        (
            "a version of that component that was never published",
            ARTIFACT_COMPONENT,
            "2.0.0",
            "payload.zip".to_string(),
            None,
        ),
        (
            "a component that was never published",
            "com.example.Absent",
            ARTIFACT_VERSION,
            "payload.zip".to_string(),
            None,
        ),
    ];

    for (case, component, version, artifact_name, object_path) in rows {
        let arn = version_arn(component, version);
        let (status, body) = artifact_read(&router, &arn, artifact_name).await;
        match object_path {
            Some(path) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: a declared artifact must answer 200, got {status} {body}"
                );
                let url = text(&body, "preSignedUrl");
                assert!(
                    url.contains(path.as_str()) && url.contains("X-Amz-Signature="),
                    "case {case:?}: expected a pre-signed URL addressing {path:?}, got {url:?}"
                );
                assert!(
                    !url.contains("amazonaws.com"),
                    "case {case:?}: the URL must lead back to this emulator, got {url:?}"
                );
            }
            None => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: an artifact that does not exist must answer 400, got \
                     {status} {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "ValidationException",
                    "case {case:?}: expected ValidationException, got {body}"
                );
                assert_eq!(
                    text(&body, "message"),
                    artifact_not_found(component, version, artifact_name),
                    "case {case:?}: expected the sentence AWS states, got {body}"
                );
            }
        }
    }

    // The read is a REPORT: asking again addresses the same object, and the recipe it
    // resolved the artifact against is unchanged.
    let arn = version_arn(ARTIFACT_COMPONENT, ARTIFACT_VERSION);
    let (status, body) = artifact_read(&router, &arn, "payload.zip").await;
    assert_eq!(
        status, 200,
        "case \"the same read again\": expected 200, got {status} {body}"
    );
    assert!(
        text(&body, "preSignedUrl").contains(&format!("/{ARTIFACT_BUCKET}/{PREFIXED_KEY}")),
        "case \"the same read again\": expected the same object, got {body}"
    );
    let (status, document) = stored_recipe(&router, &arn, "JSON").await;
    assert_eq!(
        status, 200,
        "case \"after the reads\": GetComponent must answer 200, got {status} {document}"
    );
    assert!(
        document.contains(&format!("s3://{ARTIFACT_BUCKET}/{PREFIXED_KEY}"))
            && document.contains(&format!("s3://{OTHER_BUCKET}/{PLAIN_KEY}")),
        "case \"after the reads\": the stored recipe must still declare both object \
         artifacts, got {document:?}"
    );
}

// ---------------------------------------------------------------------------
// Which component-version ARN an artifact read takes, and how it refuses the rest
// ---------------------------------------------------------------------------

/// The sentence AWS refuses a value outside the component-version ARN shape with.
fn invalid_component_version_arn(arn: &str) -> String {
    format!("Invalid Input: Component version ARN {arn} is invalid")
}

/// A component-version ARN with every field spelled out, so a row below can vary any one
/// of them independently of the ARNs this emulator happens to mint.
fn spelled_arn(partition: &str, region: &str, account: &str, name: &str, version: &str) -> String {
    format!("arn:{partition}:greengrass:{region}:{account}:components:{name}:versions:{version}")
}

/// RULE: an artifact read judges its `arn` against the component-version ARN shape alone —
/// a value outside that shape is refused with the sentence naming that shape and echoing
/// the value, and a value INSIDE it that names no stored version is refused with the one
/// sentence about the component and the artifact not existing, never with a server fault.
///
/// The shape is `ComponentVersionARN`'s `@pattern`
/// `^arn:[^:]*:greengrass:[^:]*:(aws|[0-9]+):components:[^:]+:versions:[^:]+$`
/// (`smithy_query(query="shape",shape="ComponentVersionARN")`), so the rows span what that
/// pattern generalises over: any partition and any region (including empty ones), an
/// account field that is `aws` or digits, and any colon-free name and version — beside
/// values the pattern admits nowhere.
#[tokio::test]
async fn an_artifact_read_separates_a_value_outside_the_arn_shape_from_one_that_names_nothing() {
    let router = emulator();
    let (status, body) = publish(&router, &artifact_recipe()).await;
    assert_eq!(
        status, 201,
        "publishing the artifact fixture must succeed, got {status} {body}"
    );

    // (case, partition, region, account field, component name, version, artifact name) —
    // every row matches the pattern, and none of them names a version this account
    // published, so every one is the "does not exist" refusal.
    let unknown: &[(&str, &str, &str, &str, &str, &str, &str)] = &[
        (
            "another partition",
            "aws-cn",
            REGION,
            ACCOUNT,
            "com.example.Elsewhere",
            "1.0.0",
            "payload.zip",
        ),
        (
            "an empty partition, which the pattern admits",
            "",
            REGION,
            ACCOUNT,
            "com.example.Elsewhere",
            "1.0.0",
            "payload.zip",
        ),
        (
            "another region",
            "aws",
            "eu-central-1",
            ACCOUNT,
            "com.example.Elsewhere",
            "2.3.4",
            "payload.zip",
        ),
        (
            "an empty region, which the pattern admits",
            "aws",
            "",
            ACCOUNT,
            "com.example.Elsewhere",
            "2.3.4",
            "payload.zip",
        ),
        (
            "the `aws` account field an AWS-published component's ARN carries",
            "aws",
            REGION,
            "aws",
            "aws.greengrass.Nucleus",
            "2.9.1",
            "nucleus.zip",
        ),
        (
            "another account's digits",
            "aws",
            REGION,
            "123456789012",
            "com.example.Elsewhere",
            "1.0.0",
            "payload.zip",
        ),
        (
            "a prerelease version this account never published",
            "aws",
            REGION,
            ACCOUNT,
            ARTIFACT_COMPONENT,
            "1.0.0-beta.2",
            "payload.zip",
        ),
        (
            "a name that differs from the published one only in case",
            "aws",
            REGION,
            ACCOUNT,
            "COM.EXAMPLE.WITHARTIFACTS",
            ARTIFACT_VERSION,
            "payload.zip",
        ),
        (
            "the published version addressed in another region, which is another resource",
            "aws",
            "us-east-1",
            ACCOUNT,
            ARTIFACT_COMPONENT,
            ARTIFACT_VERSION,
            "payload.zip",
        ),
        (
            "an artifact named by a nested path, which the greedy label carries",
            "aws",
            REGION,
            ACCOUNT,
            "com.example.Elsewhere",
            "1.0.0",
            "releases/1.0.0/payload.zip",
        ),
    ];

    for (case, partition, region, account, name, version, artifact_name) in unknown {
        let arn = spelled_arn(partition, region, account, name, version);
        let (status, body) = artifact_read(&router, &arn, artifact_name).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a well-formed ARN naming no stored version must answer 400, got \
             {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        assert_eq!(
            text(&body, "message"),
            artifact_not_found(name, version, artifact_name),
            "case {case:?}: expected the sentence AWS states for a search that found \
             neither, got {body}"
        );
    }

    // (case, the value the request carries) — none matches the pattern, so each is refused
    // with the sentence about the SHAPE rather than about what exists.
    let malformed: &[(&str, String)] = &[
        ("no ARN at all", "not-an-arn".to_string()),
        (
            "another vendor's ARN",
            format!("arn:aws:iot:{REGION}:{ACCOUNT}:thing/Core"),
        ),
        (
            "an account field that is neither `aws` nor digits",
            spelled_arn("aws", REGION, "my-account", ARTIFACT_COMPONENT, "1.0.0"),
        ),
        (
            "no version after `versions`",
            spelled_arn("aws", REGION, ACCOUNT, ARTIFACT_COMPONENT, ""),
        ),
        (
            "a name carrying a colon, which the pattern forbids",
            spelled_arn("aws", REGION, ACCOUNT, "com.example:Colon", "1.0.0"),
        ),
        (
            "the component ARN rather than the component-version ARN",
            format!("arn:aws:greengrass:{REGION}:{ACCOUNT}:components:{ARTIFACT_COMPONENT}"),
        ),
    ];

    for (case, arn) in malformed {
        let (status, body) = artifact_read(&router, arn, "payload.zip").await;
        assert_eq!(
            status, 400,
            "case {case:?}: a value outside the ARN shape must answer 400, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {body}"
        );
        assert_eq!(
            text(&body, "message"),
            invalid_component_version_arn(arn),
            "case {case:?}: expected the sentence naming the shape, got {body}"
        );
    }

    // None of those refusals changed what the ONE published version answers: the read is a
    // report, and the version that does exist still hands out its artifact's URL.
    let arn = version_arn(ARTIFACT_COMPONENT, ARTIFACT_VERSION);
    let (status, body) = artifact_read(&router, &arn, PLAIN_KEY).await;
    assert_eq!(
        status, 200,
        "case \"the published version after the refusals\": expected 200, got {status} {body}"
    );
    let url = text(&body, "preSignedUrl");
    assert!(
        url.contains(&format!("/{OTHER_BUCKET}/{PLAIN_KEY}")) && url.contains("X-Amz-Signature="),
        "case \"the published version after the refusals\": expected a pre-signed URL \
         addressing the declared object, got {url:?}"
    );
}
