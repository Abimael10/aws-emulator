// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for WHICH resources `ListImages` enumerates, and which `owner`
//! values it admits.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is
//! what a signed AWS SDK request observes. Every resource a case starts from is created
//! through its own public create operation, so nothing is planted in the store by hand.
//!
//! The rules under test:
//!
//! * `ListImages` "returns the list of images that you have access to", so an entry of its
//!   page stands for an IMAGE and for nothing else: a resource of another kind — a
//!   component, an image recipe, a container recipe, an image pipeline — is reported by its
//!   own read or listing and is never an entry here, however much of its name, version or
//!   Region it shares with an image. A name this service holds no image under therefore
//!   reports no entry at all, even while four other resources carry it.
//! * `owner` is an `Ownership`, whose members are
//!   `smithy_query(query="shape",shape="Ownership")`'s five values; a value outside that set
//!   is refused as the parameter value it is rather than read as the default ownership or as
//!   an ownership holding no images, which would answer a page the caller did not ask for.
//!
//! The rows differ along what those rules generalise over: how many kinds share one name,
//! how many versions of a name hold an image, whether the shared name holds an image at
//! all, whether entries aggregate by name, which ownership is asked for, and — for the
//! refused values — how a value misses the modelled set (wrong case, surrounding space, a
//! value of another vocabulary, a value of no vocabulary).

use std::sync::Arc;

use serde_json::{json, Map, Value};
use service_com_amazonaws_imagebuilder_imagebuilder::state::normalized_name;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every case signs for; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// The version nodes a NAME ARN carries, from the note on `ImageVersion`'s `arn`:
/// "Versionless ARNs and Name ARNs do not include specific values in any of the nodes. The
/// nodes are either left off entirely, or they are specified as wildcards, for example:
/// x.x.x."
const WILDCARD_VERSION: &str = "x.x.x";

/// A build component document: one `build` phase, which is all a recipe referencing it
/// needs in order to report a platform.
const BUILD_DOCUMENT: &str = "name: HelloWorld\nschemaVersion: 1.0\nphases:\n  - name: build\n    steps:\n      - name: Greet\n        action: ExecuteBash\n        inputs:\n          commands:\n            - echo hello\n";

/// A Dockerfile template a container recipe carries inline.
const INLINE_TEMPLATE: &str = "FROM {{{ imagebuilder:parentImage }}}\nRUN echo hello\n";

/// The ARN infix that says a reference identifies an IMAGE rather than some other kind of
/// Image Builder resource.
const IMAGE_ARN_INFIX: &str = ":image/";

/// The error this service refuses a parameter value with
/// (`smithy_query(query="shape",shape="InvalidParameterValueException")`: a client error,
/// `@httpError` 400).
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValueException";

/// The five values `Ownership` admits, in the order the model declares them
/// (`smithy_query(query="shape",shape="Ownership")`).
const MODELLED_OWNERSHIPS: &[&str] = &["Self", "Shared", "Amazon", "ThirdParty", "AWSMarketplace"];

/// One emulator with an empty store, mounted exactly as the router mounts it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_imagebuilder_imagebuilder::EmulatedService::new(engine),
    );
    router
}

/// A request signed for `imagebuilder` in [`REGION`] — the signal the router routes on and
/// the handlers read the Region from.
fn request(method: &str, uri: &str, body: Option<&Value>) -> http::Request<bytes::Bytes> {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("imagebuilder.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDIMAGEBUILDER/20260101/{REGION}/imagebuilder/\
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
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let response = router.dispatch(request(method, uri, body)).await;
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

/// One member of an answer, or a panic naming the case.
fn member<'body>(case: &str, body: &'body Value, name: &str) -> &'body Value {
    body.get(name)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {name} member in {body}"))
}

/// One string member of an answer.
fn text(case: &str, body: &Value, name: &str) -> String {
    member(case, body, name)
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected {name} to be a string in {body}"))
        .to_string()
}

/// A successful answer's body, or a panic naming the case.
fn assert_ok(case: &str, status: u16, body: Value) -> Value {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {body}"
    );
    body
}

/// The `arn` of every element of the `list_member` list one listing answered, sorted, so a
/// case compares inventories rather than page order.
fn listed_arns(case: &str, body: &Value, list_member: &str) -> Vec<String> {
    let mut arns: Vec<String> = member(case, body, list_member)
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a list in {list_member} of {body}"))
        .iter()
        .map(|entry| text(case, entry, "arn"))
        .collect();
    arns.sort();
    arns
}

/// The error code a refusal carries, from `__type` (restJson1 renders the shape there, and
/// the `x-amzn-errortype` header is folded in beside it).
fn error_code(case: &str, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an error type in {body}"))
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// `CreateComponent` for one build version of `name`, answering its build version ARN.
async fn create_component(router: &MultiServiceRouter, name: &str, version: &str) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": version,
        "platform": "Linux",
        "clientToken": format!("component-{name}-{version}"),
        "data": BUILD_DOCUMENT,
    });
    let case = format!("create the component {name} {version}");
    let (status, answer) = call(router, "PUT", "/CreateComponent", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "componentBuildVersionArn")
}

/// `CreateImageRecipe` for `name` at `version`, including `component_arn`, answering the
/// recipe's own ARN.
async fn create_image_recipe(
    router: &MultiServiceRouter,
    name: &str,
    version: &str,
    component_arn: &str,
) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": version,
        "parentImage": "ami-0123456789abcdef0",
        "clientToken": format!("image-recipe-{name}-{version}"),
        "components": [{ "componentArn": component_arn }],
    });
    let case = format!("create the image recipe {name} {version}");
    let (status, answer) = call(router, "PUT", "/CreateImageRecipe", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imageRecipeArn")
}

/// `CreateContainerRecipe` for `name` at `version`, answering the recipe's own ARN.
async fn create_container_recipe(
    router: &MultiServiceRouter,
    name: &str,
    version: &str,
    component_arn: &str,
) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": version,
        "containerType": "DOCKER",
        "parentImage": "amazonlinux:latest",
        "clientToken": format!("container-recipe-{name}-{version}"),
        "targetRepository": { "service": "ECR", "repositoryName": "team-a/my-repo" },
        "dockerfileTemplateData": INLINE_TEMPLATE,
        "components": [{ "componentArn": component_arn }],
    });
    let case = format!("create the container recipe {name} {version}");
    let (status, answer) = call(router, "PUT", "/CreateContainerRecipe", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "containerRecipeArn")
}

/// `CreateInfrastructureConfiguration` for `name`, answering its ARN.
async fn create_configuration(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("configuration-{name}"),
        "instanceProfileName": "EC2InstanceProfileForImageBuilder",
    });
    let case = format!("create the infrastructure configuration {name}");
    let (status, answer) = call(
        router,
        "PUT",
        "/CreateInfrastructureConfiguration",
        Some(&body),
    )
    .await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "infrastructureConfigurationArn")
}

/// `CreateImagePipeline` for `name`, answering the pipeline's own ARN.
async fn create_pipeline(
    router: &MultiServiceRouter,
    name: &str,
    recipe_arn: &str,
    configuration_arn: &str,
) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("pipeline-{name}"),
        "imageRecipeArn": recipe_arn,
        "infrastructureConfigurationArn": configuration_arn,
    });
    let case = format!("create the image pipeline {name}");
    let (status, answer) = call(router, "PUT", "/CreateImagePipeline", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imagePipelineArn")
}

/// `CreateImage` from `recipe_arn`, answering the build version ARN it filed.
async fn create_image(
    router: &MultiServiceRouter,
    token: &str,
    recipe_arn: &str,
    configuration_arn: &str,
) -> String {
    let body = json!({
        "imageRecipeArn": recipe_arn,
        "infrastructureConfigurationArn": configuration_arn,
        "clientToken": token,
    });
    let case = format!("create the image {token}");
    let (status, answer) = call(router, "PUT", "/CreateImage", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imageBuildVersionArn")
}

/// `ListImages` asking for `owner`, aggregating by name when `by_name`, with the page size
/// the request names.
async fn list_images(
    router: &MultiServiceRouter,
    owner: Option<&str>,
    by_name: bool,
    max_results: Option<usize>,
) -> (u16, Value) {
    let mut body = Map::new();
    if let Some(owner) = owner {
        body.insert("owner".to_string(), json!(owner));
    }
    if by_name {
        body.insert("byName".to_string(), json!(true));
    }
    if let Some(max_results) = max_results {
        body.insert("maxResults".to_string(), json!(max_results));
    }
    call(router, "POST", "/ListImages", Some(&Value::Object(body))).await
}

/// A version ARN of the image `name` names at `version` — the ARN one entry of this listing
/// reports for a version, which carries no build number.
fn image_version_arn(name: &str, version: &str) -> String {
    format!(
        "arn:aws:imagebuilder:{REGION}:{ACCOUNT}:image/{}/{version}",
        normalized_name(name)
    )
}

/// The NAME ARN of the image `name` — the ARN one entry reports when a listing aggregates
/// every version of a name.
fn image_name_arn(name: &str) -> String {
    image_version_arn(name, WILDCARD_VERSION)
}

/// Every resource one name carries, beside however many image versions it holds.
struct SharedName {
    /// The name every kind below was created under.
    name: &'static str,
    /// The versions of this name an IMAGE was built at — empty for a name no image exists
    /// under.
    image_versions: &'static [&'static str],
    component_arns: Vec<String>,
    image_recipe_arns: Vec<String>,
    container_recipe_arns: Vec<String>,
    pipeline_arn: String,
}

/// The names each case starts from. They differ along how many image versions the name
/// holds — none, one, or several — while every one of them carries a component, an image
/// recipe, a container recipe and an image pipeline of that same name at every version
/// named.
const SHARED_NAMES: &[(&str, &[&str])] = &[
    // A name whose component and recipes have no image beside them at all: the recorded
    // shape of this rule.
    ("quiet-name", &[]),
    // A name holding one image version.
    ("one-version-name", &["1.0.0"]),
    // A name holding several image versions, so a by-name listing has something to
    // aggregate and a by-version listing reports more entries than there are names.
    ("many-versions-name", &["1.0.0", "2.3.4"]),
];

/// Create, through the public create operations, every resource of [`SHARED_NAMES`] plus the
/// images those names hold, and answer what was created beside the one infrastructure
/// configuration the images and pipelines were built with.
async fn seeded(router: &MultiServiceRouter) -> Vec<SharedName> {
    let configuration = create_configuration(router, "shared-configuration").await;
    let mut created = Vec::new();
    for (name, image_versions) in SHARED_NAMES {
        let mut component_arns = Vec::new();
        let mut image_recipe_arns = Vec::new();
        let mut container_recipe_arns = Vec::new();
        // Every name carries at least one version of each kind; a name holding several
        // image versions carries one of each kind per version, so the kinds line up with
        // the image versions rather than only with the name.
        let versions: Vec<&str> = if image_versions.is_empty() {
            vec!["1.0.0"]
        } else {
            image_versions.to_vec()
        };
        for version in &versions {
            let component = create_component(router, name, version).await;
            image_recipe_arns.push(create_image_recipe(router, name, version, &component).await);
            container_recipe_arns
                .push(create_container_recipe(router, name, version, &component).await);
            component_arns.push(component);
        }
        let pipeline_arn = create_pipeline(
            router,
            name,
            image_recipe_arns.first().expect("one recipe per name"),
            &configuration,
        )
        .await;
        for (version, recipe) in versions.iter().zip(&image_recipe_arns) {
            if image_versions.contains(version) {
                create_image(
                    router,
                    &format!("image-{name}-{version}"),
                    recipe,
                    &configuration,
                )
                .await;
            }
        }
        created.push(SharedName {
            name,
            image_versions,
            component_arns,
            image_recipe_arns,
            container_recipe_arns,
            pipeline_arn,
        });
    }
    created
}

/// RULE: an entry of an image listing stands for an IMAGE and for nothing else — a
/// component, an image recipe, a container recipe or an image pipeline sharing an image's
/// name, version and Region is enumerated by its own listing and never by this one, so a
/// name holding no image reports no entry however many other kinds carry it.
#[tokio::test]
async fn an_image_listing_reports_images_and_not_other_kinds_sharing_their_name() {
    let router = emulator();
    let created = seeded(&router).await;

    // What the account's own image inventory holds: one entry per image VERSION, and one
    // per NAME that holds any image at all.
    let mut expected_versions: Vec<String> = created
        .iter()
        .flat_map(|shared| {
            shared
                .image_versions
                .iter()
                .map(|version| image_version_arn(shared.name, version))
        })
        .collect();
    expected_versions.sort();
    let mut expected_names: Vec<String> = created
        .iter()
        .filter(|shared| !shared.image_versions.is_empty())
        .map(|shared| image_name_arn(shared.name))
        .collect();
    expected_names.sort();

    // (case, the ownership asked for, whether entries aggregate by name, the ARNs expected)
    let rows: Vec<(&str, Option<&str>, bool, &Vec<String>)> = vec![
        (
            "no ownership named, one entry per version",
            None,
            false,
            &expected_versions,
        ),
        (
            "this account's own images, one entry per version",
            Some("Self"),
            false,
            &expected_versions,
        ),
        (
            "this account's own images, one entry per name",
            Some("Self"),
            true,
            &expected_names,
        ),
    ];

    for (case, owner, by_name, expected) in rows {
        let (status, answer) = list_images(&router, owner, by_name, Some(25)).await;
        let answer = assert_ok(case, status, answer);
        let arns = listed_arns(case, &answer, "imageVersionList");
        assert_eq!(
            &arns, expected,
            "case {case:?}: expected the image ARNs {expected:?}, got {arns:?}"
        );
        // Whatever the listing reported, each entry identifies an image: no entry may carry
        // another kind's ARN even when the two share a name.
        for arn in &arns {
            assert!(
                arn.contains(IMAGE_ARN_INFIX),
                "case {case:?}: expected every entry to identify an image, got {arn:?}"
            );
        }
        // And the resources of the other kinds are absent by ARN, name ARN included.
        let forbidden: Vec<&String> = created
            .iter()
            .flat_map(|shared| {
                shared
                    .component_arns
                    .iter()
                    .chain(&shared.image_recipe_arns)
                    .chain(&shared.container_recipe_arns)
                    .chain(std::iter::once(&shared.pipeline_arn))
            })
            .collect();
        for arn in forbidden {
            assert!(
                !arns.contains(arn),
                "case {case:?}: expected {arn:?} not to be an entry of an image listing, \
                 got {arns:?}"
            );
        }
    }

    // The isolation is mutual and the other resources really exist: each kind's own listing
    // reports exactly what was created under it, including the names no image was built
    // for.
    //
    // (case, method+URI, the list member the answer carries, the ARNs expected)
    let mut expected_components: Vec<String> = created
        .iter()
        // A component listing reports one entry per component VERSION, under that version's
        // ARN, which is its build version ARN without the build node.
        .flat_map(|shared| {
            shared
                .component_arns
                .iter()
                .map(|arn| arn.rsplit_once('/').expect("a build node").0.to_string())
        })
        .collect();
    expected_components.sort();
    let mut expected_image_recipes: Vec<String> = created
        .iter()
        .flat_map(|shared| shared.image_recipe_arns.clone())
        .collect();
    expected_image_recipes.sort();
    let mut expected_pipelines: Vec<String> = created
        .iter()
        .map(|shared| shared.pipeline_arn.clone())
        .collect();
    expected_pipelines.sort();

    let own_listings: Vec<(&str, &str, &str, &Vec<String>)> = vec![
        (
            "the components of those names",
            "/ListComponents",
            "componentVersionList",
            &expected_components,
        ),
        (
            "the image recipes of those names",
            "/ListImageRecipes",
            "imageRecipeSummaryList",
            &expected_image_recipes,
        ),
        (
            "the image pipelines of those names",
            "/ListImagePipelines",
            "imagePipelineList",
            &expected_pipelines,
        ),
    ];
    for (case, uri, list_member, expected) in own_listings {
        let (status, answer) = call(&router, "POST", uri, Some(&json!({ "maxResults": 25 }))).await;
        let answer = assert_ok(case, status, answer);
        let arns = listed_arns(case, &answer, list_member);
        assert_eq!(
            &arns, expected,
            "case {case:?}: expected {expected:?}, got {arns:?}"
        );
    }

    // A container recipe has no listing to read it back through yet (`ListContainerRecipes`
    // is a scaffold of its own), so each one is read back through `GetContainerRecipe`: it
    // exists under the name an image listing refused to report it under.
    for shared in &created {
        for arn in &shared.container_recipe_arns {
            let case = format!("read the container recipe {arn} back");
            let (status, answer) = call(
                &router,
                "GET",
                &format!("/GetContainerRecipe?containerRecipeArn={arn}"),
                None,
            )
            .await;
            let answer = assert_ok(&case, status, answer);
            let recipe = member(&case, &answer, "containerRecipe").clone();
            assert_eq!(
                text(&case, &recipe, "arn"),
                *arn,
                "case {case:?}: expected the container recipe to report its own ARN"
            );
            assert_eq!(
                text(&case, &recipe, "name"),
                shared.name,
                "case {case:?}: expected the container recipe to carry the shared name"
            );
        }
    }
}

/// RULE: `owner` carries an `Ownership`, so each of the five values the model declares is
/// answered with a page of that ownership's images and a value outside that set is refused
/// as the parameter value it is — never read as the default ownership and never answered
/// with an empty page.
#[tokio::test]
async fn the_owner_member_admits_only_the_modelled_ownership_values() {
    let router = emulator();
    let created = seeded(&router).await;
    let own_versions = created
        .iter()
        .filter(|shared| !shared.image_versions.is_empty())
        .count();
    assert!(
        own_versions > 0,
        "case \"the seeded account inventory\": expected the account to hold images, got none"
    );

    // Every modelled ownership is ANSWERED: this account's own images for the default and
    // for `Self`, a page of the catalog AWS publishes for `Amazon`, and an empty page for an
    // ownership this emulator holds no images of.
    for owner in MODELLED_OWNERSHIPS {
        let case = format!("the modelled ownership {owner}");
        let (status, answer) = list_images(&router, Some(owner), false, Some(25)).await;
        let answer = assert_ok(&case, status, answer);
        let arns = listed_arns(&case, &answer, "imageVersionList");
        for arn in &arns {
            assert!(
                arn.contains(IMAGE_ARN_INFIX),
                "case {case:?}: expected every entry to identify an image, got {arn:?}"
            );
        }
        if *owner == "Self" {
            assert!(
                !arns.is_empty(),
                "case {case:?}: expected this account's own images, got an empty page"
            );
        }
    }

    // The sentence this service states a refused parameter value in, about `owner` and the
    // set its value had to come from.
    let expected_message = format!(
        "The value supplied for parameter 'owner' is not valid. owner must be one of [{}]",
        MODELLED_OWNERSHIPS
            .iter()
            .map(|value| format!("'{value}'"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // The rows differ along how the value misses the modelled set: wrong case, surrounding
    // space, a value of another vocabulary of this same API, and a value of no vocabulary
    // at all.
    let refused: Vec<(&str, &str)> = vec![
        ("the same word in another case", "self"),
        ("the same word upper-cased", "SELF"),
        ("a modelled value with a trailing space", "Self "),
        ("a modelled value with a leading space", " Amazon"),
        ("another ownership lower-cased", "thirdparty"),
        ("an account id in place of an ownership", ACCOUNT),
        ("a word of no vocabulary", "Everyone"),
    ];
    for (case, owner) in refused {
        let (status, answer) = list_images(&router, Some(owner), false, Some(25)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {answer}"
        );
        let code = error_code(case, &answer);
        assert_eq!(
            code, INVALID_PARAMETER_VALUE,
            "case {case:?}: expected {INVALID_PARAMETER_VALUE}, got {code:?} in {answer}"
        );
        let message = text(case, &answer, "message");
        assert_eq!(
            message, expected_message,
            "case {case:?}: expected the message {expected_message:?}, got {message:?}"
        );
        assert!(
            answer.get("imageVersionList").is_none(),
            "case {case:?}: expected a refusal to carry no page, got {answer}"
        );
    }
}
