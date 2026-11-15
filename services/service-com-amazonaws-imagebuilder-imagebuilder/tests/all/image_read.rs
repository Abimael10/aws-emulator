// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `GetImage`: the read of ONE image build version.
//!
//! Every case drives the real wire path (the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers), so what is asserted is
//! what a signed AWS SDK request observes.
//!
//! The rules under test, both stated by `GetImage`'s own `imageBuildVersionArn` member
//! (`smithy_query(query="operation",operation="GetImage")`, member documentation):
//!
//! * "You can specify a full build version ARN, or a version ARN with or without wildcards
//!   (x.x.x, 1.x.x, or 1.0.x). A version or wildcard ARN resolves to the latest matching
//!   build version that has reached AVAILABLE status. … To get an image in any other state,
//!   such as a failed or in-progress build, specify the full build version ARN." So a build
//!   version ARN reaches that one build whatever status it reports, and a version ARN
//!   reaches the greatest matching version's greatest build among those that are available.
//! * "Retrieves an image": what the read reports is what the build that produced the build
//!   version recorded, together with the resources that build referenced, read back from
//!   this service — and the wildcard references that reach the resolved build's latest
//!   version.
//!
//! A reference that reaches no build version of the calling Region is answered with the
//! not-found error this service models, naming the ARN the request carried.
//!
//! The rows span what each rule generalises over: which nodes a reference pins and which it
//! wildcards, which node a wildcard sits in, how many builds and versions one image name
//! has, whether a build has reached its available instant, which Region asks, and which kind
//! of recipe a build was built from.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Map, Value};
use service_com_amazonaws_imagebuilder_imagebuilder::state::{
    image_build_version_arn, normalized_name, ComAmazonawsImagebuilderImagebuilderState,
    ImageBuildVersionRecord, MANAGED_IMAGES, MANAGED_IMAGE_OWNER, MANAGED_IMAGE_VERSION,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region a case signs for when it only needs one; the ARNs it builds carry it.
const REGION: &str = "us-west-2";

/// A second Region, for the cases that check a regional endpoint reaches only its own
/// Region's resources.
const OTHER_REGION: &str = "eu-west-1";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// A build component document: one `build` phase, which is all a recipe these cases
/// reference needs in order to report a platform.
const BUILD_DOCUMENT: &str = "name: HelloWorld\nschemaVersion: 1.0\nphases:\n  - name: build\n    steps:\n      - name: Greet\n        action: ExecuteBash\n        inputs:\n          commands:\n            - echo hello\n";

/// A container recipe's Dockerfile template, which the recipe create requires.
const DOCKERFILE_TEMPLATE: &str = "FROM {{{ imagebuilder:parentImage }}}\n{{{ imagebuilder:environments }}}\n{{{ imagebuilder:components }}}\n";

/// The status an image reports while the build queued for it is still outstanding, and the
/// one it reports once that build is due to have finished
/// (`crate::state::IMAGE_STATUS_PENDING` / `IMAGE_STATUS_AVAILABLE`).
const QUEUED_STATUS: &str = "PENDING";

/// See [`QUEUED_STATUS`].
const AVAILABLE_STATUS: &str = "AVAILABLE";

/// How long a case waits for a queued build to reach its available instant before giving
/// up. Generous: the span itself is a few seconds (`crate::state::IMAGE_BUILD_SPAN`), and
/// what a case asserts is the CONDITION it polls for, never how fast the host reached it.
const AVAILABLE_DEADLINE: Duration = Duration::from_secs(120);

/// How long a case waits between two reads while polling for that condition.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One emulator with an empty store, mounted exactly as the router mounts it, beside the
/// shared engine a build path files its records in.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_imagebuilder_imagebuilder::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, engine)
}

/// A request signed for `imagebuilder` in `region`: the signal the router routes on and the
/// handlers read the Region from.
fn request(
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> http::Request<bytes::Bytes> {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("imagebuilder.{region}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDIMAGEBUILDER/20260101/{region}/imagebuilder/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        );
    let payload = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            bytes::Bytes::from(serde_json::to_vec(value).expect("serialize the request body"))
        }
        None => bytes::Bytes::new(),
    };
    builder.body(payload).expect("a well-formed test request")
}

/// Status and parsed body of one call. An empty body reads as `Value::Null`.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let response = router.dispatch(request(region, method, uri, body)).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let mut parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    // restJson1 carries the error shape in `__type`, and the framework also sets the
    // `x-amzn-errortype` header; fold the header in so one accessor reads either.
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// Read `name` out of an answer, panicking with the case when it is absent.
fn text(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected text in {name} of {body}"))
        .to_string()
}

/// Assert the call succeeded and answer its body.
fn assert_ok(case: &str, status: u16, body: Value) -> Value {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {body}"
    );
    body
}

/// The `__type` of an error answer, with any shape-id prefix dropped.
fn error_code(case: &str, body: &Value) -> String {
    let rendered = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a __type member in {body}"));
    rendered
        .rsplit('#')
        .next()
        .unwrap_or(rendered)
        .to_string()
}

/// Assert the answer is this service's not-found error for `reference`.
fn assert_missing(case: &str, status: u16, body: &Value, reference: &str) {
    assert_eq!(
        status, 404,
        "case {case:?}: expected HTTP 404, got {status} with {body}"
    );
    let code = error_code(case, body);
    assert_eq!(
        code, "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {code:?} with {body}"
    );
    let expected =
        format!("The following required resource 'Image' cannot be found: '{reference}'.");
    let actual = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message member in {body}"));
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the message {expected:?}, got {actual:?}"
    );
}

/// `GetImage` for `reference`, in `region`.
async fn get_image(router: &MultiServiceRouter, region: &str, reference: &str) -> (u16, Value) {
    call_in(
        router,
        region,
        "GET",
        &format!("/GetImage?imageBuildVersionArn={reference}"),
        None,
    )
    .await
}

/// The `image` member of a successful `GetImage` answer.
fn image_of(case: &str, body: &Value) -> Value {
    body.get("image")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an image in {body}"))
}

/// The status the `image` of an answer reports, or `None` when the answer carries none.
fn status_of(body: &Value) -> Option<String> {
    body.get("image")?
        .get("state")?
        .get("status")?
        .as_str()
        .map(str::to_string)
}

/// A version ARN of the image `name` names at `version` — the reference form that carries no
/// build number, whose nodes may be wildcards.
fn image_version_arn(region: &str, name: &str, version: &str) -> String {
    format!("arn:aws:imagebuilder:{region}:{ACCOUNT}:image/{name}/{version}")
}

/// One image build version, as a build that recorded nothing else would have filed it.
///
/// `available_at` is the instant its build is due to finish: `None` for a build that is not
/// outstanding, which is already available.
fn build_version(
    region: &str,
    name: &str,
    version: &str,
    build: u64,
    available_at: Option<SystemTime>,
) -> ImageBuildVersionRecord {
    ImageBuildVersionRecord {
        arn: image_build_version_arn(region, name, version, build),
        region: region.to_string(),
        name: name.to_string(),
        version: version.to_string(),
        build_version: build,
        platform: Some("Linux".to_string()),
        image_type: Some("AMI".to_string()),
        build_type: Some("USER_INITIATED".to_string()),
        date_created: "2026-01-01T00:00:00.000Z".to_string(),
        available_at,
        ..ImageBuildVersionRecord::default()
    }
}

/// File `record` in the store, through the write path the image build path files one with.
fn file(engine: &Arc<engine::InMemoryEngine>, record: ImageBuildVersionRecord) {
    let arn = record.arn.clone();
    let filed = engine
        .state::<ComAmazonawsImagebuilderImagebuilderState>()
        .lock()
        .insert_image(record);
    assert!(
        filed,
        "case recording {arn}: expected the store to file the build version"
    );
}

/// `CreateComponent` for one build version of `name` in `region`, answering its build
/// version ARN.
async fn create_component(router: &MultiServiceRouter, region: &str, name: &str) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": "1.0.0",
        "platform": "Linux",
        "clientToken": format!("component-{region}-{name}"),
        "data": BUILD_DOCUMENT,
    });
    let case = format!("create the component {name} in {region}");
    let (status, answer) = call_in(router, region, "PUT", "/CreateComponent", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "componentBuildVersionArn")
}

/// `CreateImageRecipe` for `name` at `version` in `region`, including `component_arn`,
/// answering the recipe's own ARN.
async fn create_recipe(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    version: &str,
    component_arn: &str,
) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": version,
        "parentImage": "ami-0123456789abcdef0",
        "clientToken": format!("recipe-{region}-{name}-{version}"),
        "components": [{ "componentArn": component_arn }],
    });
    let case = format!("create the image recipe {name} {version} in {region}");
    let (status, answer) = call_in(router, region, "PUT", "/CreateImageRecipe", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imageRecipeArn")
}

/// `CreateContainerRecipe` for `name` at `version` in `region`, answering the recipe's own
/// ARN.
async fn create_container_recipe(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    version: &str,
    component_arn: &str,
) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": version,
        "containerType": "DOCKER",
        "parentImage": "amazonlinux:latest",
        "clientToken": format!("container-recipe-{region}-{name}-{version}"),
        "components": [{ "componentArn": component_arn }],
        "dockerfileTemplateData": DOCKERFILE_TEMPLATE,
        "targetRepository": { "service": "ECR", "repositoryName": "team-a/my-repo" },
    });
    let case = format!("create the container recipe {name} {version} in {region}");
    let (status, answer) =
        call_in(router, region, "PUT", "/CreateContainerRecipe", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "containerRecipeArn")
}

/// `CreateInfrastructureConfiguration` for `name` in `region`, answering its ARN.
async fn create_configuration(router: &MultiServiceRouter, region: &str, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("configuration-{region}-{name}"),
        "instanceProfileName": "EC2InstanceProfileForImageBuilder",
    });
    let case = format!("create the infrastructure configuration {name} in {region}");
    let (status, answer) = call_in(
        router,
        region,
        "PUT",
        "/CreateInfrastructureConfiguration",
        Some(&body),
    )
    .await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "infrastructureConfigurationArn")
}

/// `CreateDistributionConfiguration` for `name` in `region`, answering its ARN.
async fn create_distribution(router: &MultiServiceRouter, region: &str, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("distribution-{region}-{name}"),
        "distributions": [{ "region": region }],
    });
    let case = format!("create the distribution configuration {name} in {region}");
    let (status, answer) = call_in(
        router,
        region,
        "PUT",
        "/CreateDistributionConfiguration",
        Some(&body),
    )
    .await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "distributionConfigurationArn")
}

/// `CreateImage` naming the recipe at `recipe_member`, answering the build version ARN it
/// filed.
async fn create_image(
    router: &MultiServiceRouter,
    region: &str,
    recipe_member: &str,
    recipe_reference: &str,
    configuration_arn: &str,
    token: &str,
    extra: &[(&str, Value)],
) -> String {
    let mut body = Map::new();
    body.insert(recipe_member.to_string(), json!(recipe_reference));
    body.insert(
        "infrastructureConfigurationArn".to_string(),
        json!(configuration_arn),
    );
    body.insert("clientToken".to_string(), json!(token));
    for (member, value) in extra {
        body.insert((*member).to_string(), value.clone());
    }
    let case = format!("queue the build {token} in {region}");
    let (status, answer) =
        call_in(router, region, "PUT", "/CreateImage", Some(&Value::Object(body))).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imageBuildVersionArn")
}

/// Read `reference` back until the build version it reaches reports `AVAILABLE`, and answer
/// that read.
///
/// The build a create queues is outstanding for a span this emulator chooses, so what a case
/// waits for is the CONDITION — the status the read reports — looked for again until it
/// holds or [`AVAILABLE_DEADLINE`] passes.
async fn available_image(router: &MultiServiceRouter, region: &str, reference: &str) -> Value {
    let case = format!("the available build version of {reference}");
    let giving_up = Instant::now() + AVAILABLE_DEADLINE;
    loop {
        let (status, body) = get_image(router, region, reference).await;
        if status == 200 && status_of(&body).as_deref() == Some(AVAILABLE_STATUS) {
            return body;
        }
        assert!(
            Instant::now() < giving_up,
            "case {case:?}: expected the queued build to reach {AVAILABLE_STATUS}, last read \
             answered {status} with {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What a reference is expected to reach: one build version's version and build nodes, with
/// the status that build reports — or nothing at all.
type Reached<'a> = Option<(&'a str, u64, &'a str)>;

/// RULE: a build version ARN reaches that one build whatever status it reports, and a version
/// ARN — wildcards admitted in trailing nodes — reaches the greatest matching version's
/// greatest build among the builds that have reached AVAILABLE status; a reference that
/// reaches no build version of the calling Region is reported missing.
///
/// The store is filled through the image-build path's own write path, so each row is placed
/// by the reference alone: five builds of one image name spanning two major versions, two
/// minor and two patch levels, one name+version pair with two builds, one build still
/// outstanding, and one build of the same name in another Region.
#[tokio::test]
async fn which_build_version_a_reference_reaches() {
    let (router, engine) = emulator();
    let name = "catalog-image";
    // An instant far enough ahead that the build it belongs to is outstanding for every read
    // below, whatever the host's pace: the point of the row is that an outstanding build is
    // not available, not when it becomes so.
    let outstanding = SystemTime::now().checked_add(Duration::from_secs(3_600));
    assert!(
        outstanding.is_some(),
        "case \"the outstanding build\": expected the clock to admit a later instant"
    );
    for (version, build, available_at) in [
        ("1.0.0", 1, None),
        ("1.0.0", 2, None),
        ("1.0.1", 1, None),
        ("1.1.0", 1, None),
        // The greatest version of all, and the one no version ARN may reach: its build has
        // not reached its available instant.
        ("2.0.0", 1, outstanding),
    ] {
        file(
            &engine,
            build_version(REGION, name, version, build, available_at),
        );
    }
    // The same name in another Region, so a reference of this Region that reaches nothing
    // here is not quietly answered from there.
    file(&engine, build_version(OTHER_REGION, name, "1.0.0", 1, None));

    // (case, the reference, what it reaches)
    let rows: &[(&str, String, Reached)] = &[
        (
            "a build version ARN names that one build",
            image_build_version_arn(REGION, name, "1.0.0", 1),
            Some(("1.0.0", 1, AVAILABLE_STATUS)),
        ),
        (
            "a build version ARN of the second build of one version",
            image_build_version_arn(REGION, name, "1.0.0", 2),
            Some(("1.0.0", 2, AVAILABLE_STATUS)),
        ),
        (
            "a build version ARN reaches a build that is still outstanding",
            image_build_version_arn(REGION, name, "2.0.0", 1),
            Some(("2.0.0", 1, QUEUED_STATUS)),
        ),
        (
            "an exact version ARN takes the greatest build of that version",
            image_version_arn(REGION, name, "1.0.0"),
            Some(("1.0.0", 2, AVAILABLE_STATUS)),
        ),
        (
            "a patch wildcard takes the greatest patch level",
            image_version_arn(REGION, name, "1.0.x"),
            Some(("1.0.1", 1, AVAILABLE_STATUS)),
        ),
        (
            "a minor wildcard takes the greatest minor level",
            image_version_arn(REGION, name, "1.x.x"),
            Some(("1.1.0", 1, AVAILABLE_STATUS)),
        ),
        (
            "a full wildcard skips the greater version whose build is not available",
            image_version_arn(REGION, name, "x.x.x"),
            Some(("1.1.0", 1, AVAILABLE_STATUS)),
        ),
        (
            "a version nothing was built at",
            image_version_arn(REGION, name, "3.0.0"),
            None,
        ),
        (
            "a build number nothing was built under",
            image_build_version_arn(REGION, name, "1.0.0", 9),
            None,
        ),
        (
            "a name nothing was built under",
            image_version_arn(REGION, "absent-image", "x.x.x"),
            None,
        ),
        (
            // "When you use a wildcard in any node, all nodes to the right of the first
            // wildcard must also be wildcards", so a leading wildcard reaches nothing.
            "a wildcard in a node that is not trailing",
            image_version_arn(REGION, name, "x.0.0"),
            None,
        ),
        (
            "a build version of another Region",
            image_build_version_arn(OTHER_REGION, name, "1.0.0", 1),
            None,
        ),
    ];

    for (case, reference, reached) in rows {
        let (status, body) = get_image(&router, REGION, reference).await;
        let Some((version, build, expected_status)) = reached else {
            assert_missing(case, status, &body, reference);
            continue;
        };
        let body = assert_ok(case, status, body);
        let image = image_of(case, &body);
        let expected_arn = image_build_version_arn(REGION, name, version, *build);
        let actual_arn = text(case, &image, "arn");
        assert_eq!(
            actual_arn, expected_arn,
            "case {case:?}: expected the build version {expected_arn:?}, got {actual_arn:?}"
        );
        // "The semantic version has four nodes: <major>.<minor>.<patch>/<build>", the form
        // the reported member's own shape admits (`VersionNumberWithBuild`).
        let expected_version = format!("{version}/{build}");
        let actual_version = text(case, &image, "version");
        assert_eq!(
            actual_version, expected_version,
            "case {case:?}: expected the version {expected_version:?}, got {actual_version:?}"
        );
        let actual_status = status_of(&body);
        assert_eq!(
            actual_status.as_deref(),
            Some(*expected_status),
            "case {case:?}: expected the status {expected_status:?}, got {actual_status:?}"
        );
        // "A set of wildcard version ARNs that always reference the latest version of the
        // resource", derived from the build version the read RESOLVED to rather than from the
        // reference the request carried.
        let references = body
            .get("latestVersionReferences")
            .unwrap_or_else(|| panic!("case {case:?}: expected latestVersionReferences in {body}"));
        let mut nodes = version.split('.');
        let major = nodes.next().unwrap_or_default();
        let minor = nodes.next().unwrap_or_default();
        for (member, expected) in [
            ("latestVersionArn", "x.x.x".to_string()),
            ("latestMajorVersionArn", format!("{major}.x.x")),
            ("latestMinorVersionArn", format!("{major}.{minor}.x")),
            ("latestPatchVersionArn", (*version).to_string()),
        ] {
            let expected = image_version_arn(REGION, name, &expected);
            let actual = text(case, references, member);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {member} {expected:?}, got {actual:?}"
            );
        }
    }
}

/// RULE: what a read reports is what the build recorded — the image's own members and the
/// resources the build referenced, read back from this service — so a build queued through
/// the wire reads back through `GetImage` exactly as it was configured, and the recipe a read
/// reports is of the kind the build was built from.
///
/// The two rows differ along the kind of recipe the build builds, which decides both the kind
/// of image it produces and which of the two recipe members a read reports.
#[tokio::test]
async fn a_queued_build_reads_back_as_the_build_recorded_it() {
    let (router, _engine) = emulator();
    let component = create_component(&router, REGION, "read-component").await;
    let image_recipe = create_recipe(&router, REGION, "read-ami", "1.0.0", &component).await;
    let container_recipe =
        create_container_recipe(&router, REGION, "read-container", "1.0.0", &component).await;
    let configuration = create_configuration(&router, REGION, "read-infra").await;
    let distribution = create_distribution(&router, REGION, "read-distribution").await;

    // (case, the recipe member, the recipe, the image kind, the member a read reports the
    // recipe in, the member it reports none in)
    let rows: &[(&str, &str, &str, &str, &str, &str)] = &[
        (
            "a build of an image recipe",
            "imageRecipeArn",
            &image_recipe,
            "AMI",
            "imageRecipe",
            "containerRecipe",
        ),
        (
            "a build of a container recipe",
            "containerRecipeArn",
            &container_recipe,
            "DOCKER",
            "containerRecipe",
            "imageRecipe",
        ),
    ];

    for (case, member, recipe, kind, reported_in, absent_from) in rows {
        let tags = json!({ "Project": "emulator", "Stage": "test" });
        let arn = create_image(
            &router,
            REGION,
            member,
            recipe,
            &configuration,
            &format!("build-{member}"),
            &[
                ("tags", tags.clone()),
                ("distributionConfigurationArn", json!(distribution)),
            ],
        )
        .await;

        let body = available_image(&router, REGION, &arn).await;
        let image = image_of(case, &body);
        for (reported, expected) in [
            ("arn", arn.clone()),
            ("type", (*kind).to_string()),
            ("version", "1.0.0/1".to_string()),
            ("platform", "Linux".to_string()),
            ("buildType", "USER_INITIATED".to_string()),
        ] {
            let actual = text(case, &image, reported);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {reported} {expected:?}, got {actual:?}"
            );
        }
        // The recipe the build was built from, reported in the member its kind belongs to and
        // in neither other.
        let reported_recipe = image
            .get(*reported_in)
            .unwrap_or_else(|| panic!("case {case:?}: expected a {reported_in} in {image}"));
        let reported_arn = text(case, reported_recipe, "arn");
        assert_eq!(
            reported_arn, **recipe,
            "case {case:?}: expected the recipe {recipe:?}, got {reported_arn:?}"
        );
        assert!(
            image.get(*absent_from).is_none_or(Value::is_null),
            "case {case:?}: expected no {absent_from} in {image}"
        );
        // The configurations the build ran under, read back from the service that holds them.
        for (reported, expected) in [
            ("infrastructureConfiguration", &configuration),
            ("distributionConfiguration", &distribution),
        ] {
            let held = image
                .get(reported)
                .unwrap_or_else(|| panic!("case {case:?}: expected a {reported} in {image}"));
            let actual = text(case, held, "arn");
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {reported} {expected:?}, got {actual:?}"
            );
        }
        // "Image tests are enabled by default", and of the timeout: "If you don't specify a
        // value, Image Builder stores and returns 720."
        let tests = image
            .get("imageTestsConfiguration")
            .unwrap_or_else(|| panic!("case {case:?}: expected an imageTestsConfiguration"));
        assert_eq!(
            tests,
            &json!({ "imageTestsEnabled": true, "timeoutMinutes": 720 }),
            "case {case:?}: expected the stored test defaults, got {tests}"
        );
        assert_eq!(
            image.get("tags"),
            Some(&tags),
            "case {case:?}: expected the tags the build carried in {image}"
        );
        assert_eq!(
            image.get("enhancedImageMetadataEnabled"),
            Some(&json!(true)),
            "case {case:?}: expected the documented metadata default in {image}"
        );
        // "This field is only present for images that a pipeline execution created", and a
        // caller asked for this build directly.
        assert!(
            image.get("sourcePipelineArn").is_none_or(Value::is_null),
            "case {case:?}: expected no sourcePipelineArn in {image}"
        );
        assert!(
            !text(case, &image, "dateCreated").is_empty(),
            "case {case:?}: expected the instant the build was queued in {image}"
        );
    }
}

/// RULE: a version ARN reaches the LATEST available build, so a second build of one
/// name+version pair takes over what that reference reaches, while each build version ARN
/// keeps reaching its own build.
#[tokio::test]
async fn a_version_arn_follows_the_latest_available_build() {
    let (router, _engine) = emulator();
    let component = create_component(&router, REGION, "latest-component").await;
    let recipe = create_recipe(&router, REGION, "latest-image", "1.0.0", &component).await;
    let configuration = create_configuration(&router, REGION, "latest-infra").await;

    let version_reference = image_version_arn(REGION, "latest-image", "1.0.0");
    let wildcard_reference = image_version_arn(REGION, "latest-image", "x.x.x");

    let mut filed: Vec<String> = Vec::new();
    for build in 1_u64..=2 {
        let arn = create_image(
            &router,
            REGION,
            "imageRecipeArn",
            &recipe,
            &configuration,
            &format!("latest-{build}"),
            &[],
        )
        .await;
        filed.push(arn.clone());
        // Each build is reached by its own ARN as soon as it is available, and by the version
        // references once it is the latest available build.
        let case = format!("build {build}");
        available_image(&router, REGION, &arn).await;
        for reference in [&version_reference, &wildcard_reference] {
            let body = available_image(&router, REGION, reference).await;
            let reached = text(&case, &image_of(&case, &body), "arn");
            assert_eq!(
                reached, arn,
                "case {case:?}: expected {reference} to reach {arn:?}, got {reached:?}"
            );
        }
    }

    // The earlier build is still reached by its OWN ARN: a build version ARN names one build.
    let case = "the first build after the second was queued";
    let first = filed
        .first()
        .expect("the first build version the case queued");
    let body = available_image(&router, REGION, first).await;
    let reached = text(case, &image_of(case, &body), "arn");
    assert_eq!(
        reached, *first,
        "case {case:?}: expected {first:?}, got {reached:?}"
    );
    let version = text(case, &image_of(case, &body), "version");
    assert_eq!(
        version, "1.0.0/1",
        "case {case:?}: expected the first build's own fourth node, got {version:?}"
    );
}

/// A published image's ARN in `region`, at `version` — the form a base image reference takes.
fn managed_image_arn(region: &str, name: &str, version: &str) -> String {
    format!("arn:aws:imagebuilder:{region}:{MANAGED_IMAGE_OWNER}:image/{name}/{version}")
}

/// RULE: an image ARN whose owner field is Amazon's rather than an account id names an image
/// AWS publishes, and a read of one reports that published build — the operating system it
/// carries included — rather than a build of this account; a wildcard version ARN resolves to
/// the dated build the catalog published, which is what lets a caller discover it.
///
/// Every entry of the catalog is read, so the rows are not only the one a base image
/// reference usually names: the platform each reports follows from its name, and the
/// published version is the one the catalog holds.
#[tokio::test]
async fn a_published_image_is_read_from_the_managed_catalog() {
    let (router, _engine) = emulator();
    let published = format!("{MANAGED_IMAGE_VERSION}/1");

    for entry in MANAGED_IMAGES {
        let case = format!("the published image {:?}", entry.name);
        let arn_name = normalized_name(entry.name);
        let reference = managed_image_arn(REGION, &arn_name, "x.x.x");
        let (status, body) = get_image(&router, REGION, &reference).await;
        let body = assert_ok(&case, status, body);
        let image = image_of(&case, &body);
        // The platform a base image's name states, which is how this service reads one.
        let platform = if entry.name.contains("Windows") {
            "Windows"
        } else {
            "Linux"
        };
        for (member, expected) in [
            ("arn", managed_image_arn(REGION, &arn_name, &published)),
            ("name", entry.name.to_string()),
            ("version", published.clone()),
            ("osVersion", entry.os_version.to_string()),
            ("platform", platform.to_string()),
            ("type", "AMI".to_string()),
            ("imageSource", "AMAZON_MANAGED".to_string()),
        ] {
            let actual = text(&case, &image, member);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {member} {expected:?}, got {actual:?}"
            );
        }
        let status = status_of(&body);
        assert_eq!(
            status.as_deref(),
            Some(AVAILABLE_STATUS),
            "case {case:?}: expected a published image to be available, got {status:?}"
        );
        // A published image is built from no recipe of this account, so a read reports
        // neither recipe nor the build settings of an account's own build.
        for absent in [
            "imageRecipe",
            "containerRecipe",
            "infrastructureConfiguration",
            "distributionConfiguration",
            "sourcePipelineArn",
        ] {
            assert!(
                image.get(absent).is_none_or(Value::is_null),
                "case {case:?}: expected no {absent} in {image}"
            );
        }
        // The wildcard references name the OWNER of the image they reach, which for a
        // published image is Amazon's catalog rather than this account.
        let references = body
            .get("latestVersionReferences")
            .unwrap_or_else(|| panic!("case {case:?}: expected latestVersionReferences in {body}"));
        let actual = text(&case, references, "latestVersionArn");
        let expected = managed_image_arn(REGION, &arn_name, "x.x.x");
        assert_eq!(
            actual, expected,
            "case {case:?}: expected latestVersionArn {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: a reference into the published catalog is read exactly as one to an account's own
/// image — the name selects the image, a trailing `x` wildcards a node, and a build version
/// ARN names the published build — so a reference that reaches no published image of the
/// calling Region is reported missing.
///
/// The rows differ along which nodes the reference pins, which node wildcards, which Region
/// it names, and how it misses: a version the catalog did not publish, a build number it did
/// not, and a name it does not hold.
#[tokio::test]
async fn which_published_image_a_reference_reaches() {
    let (router, _engine) = emulator();
    let entry = MANAGED_IMAGES
        .first()
        .expect("the catalog holds at least one published image");
    let arn_name = normalized_name(entry.name);
    let published = format!("{MANAGED_IMAGE_VERSION}/1");
    let resolved = managed_image_arn(REGION, &arn_name, &published);
    let mut nodes = MANAGED_IMAGE_VERSION.split('.');
    let major = nodes.next().unwrap_or_default();
    let minor = nodes.next().unwrap_or_default();

    // (case, the reference, whether it reaches the published build)
    let rows: &[(&str, String, bool)] = &[
        (
            "a full wildcard version",
            managed_image_arn(REGION, &arn_name, "x.x.x"),
            true,
        ),
        (
            "a wildcard below the published major node",
            managed_image_arn(REGION, &arn_name, &format!("{major}.x.x")),
            true,
        ),
        (
            "a wildcard below the published minor node",
            managed_image_arn(REGION, &arn_name, &format!("{major}.{minor}.x")),
            true,
        ),
        (
            "the published version, exactly",
            managed_image_arn(REGION, &arn_name, MANAGED_IMAGE_VERSION),
            true,
        ),
        (
            "the published build version ARN",
            resolved.clone(),
            true,
        ),
        (
            "a version the catalog did not publish",
            managed_image_arn(REGION, &arn_name, "1.0.0"),
            false,
        ),
        (
            "a build number the catalog did not publish",
            managed_image_arn(REGION, &arn_name, &format!("{MANAGED_IMAGE_VERSION}/2")),
            false,
        ),
        (
            "a name the catalog does not hold",
            managed_image_arn(REGION, "no-such-managed-image", "x.x.x"),
            false,
        ),
        (
            "a published image of another Region",
            managed_image_arn(OTHER_REGION, &arn_name, "x.x.x"),
            false,
        ),
    ];

    for (case, reference, reaches) in rows {
        let (status, body) = get_image(&router, REGION, reference).await;
        if !reaches {
            assert_missing(case, status, &body, reference);
            continue;
        }
        let body = assert_ok(case, status, body);
        let reached = text(case, &image_of(case, &body), "arn");
        assert_eq!(
            reached, resolved,
            "case {case:?}: expected {resolved:?}, got {reached:?}"
        );
    }
}
