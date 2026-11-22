// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the dependency a single image pipeline holds over EVERY resource
//! it references at once, and for the instant that dependency is released.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what
//! a signed AWS SDK request observes.
//!
//! The rule under test: a delete of a resource a stored image pipeline's reference RESOLVES to
//! is refused `ResourceDependencyException` (HTTP 400) in the refused resource's own
//! dependency wording and removes nothing, so the resource is still read back; the reference
//! that protects it is the one the store holds AT THE INSTANT of the call, so the moment no
//! stored pipeline reaches the resource — the pipeline was deleted, or pointed at another
//! resource — the very same delete removes the record, echoes the ARN it removed with the id
//! of the request it answers, and every later lookup of that ARN reports it missing. A
//! reference whose version carries trailing `x` wildcards reaches whichever matching version
//! is newest when the pipeline runs, so every version it admits is in use while it stands.
//!
//! The rows span what the rule generalises over: which of the resources one pipeline
//! references the delete acts on (the image recipe it builds, the infrastructure configuration
//! it builds in, the distribution configuration it distributes with) and so which of this
//! service's two dependency sentences and two missing-resource sentences the answer carries;
//! the version form of the recipe reference the pipeline holds (exact, and each width of
//! trailing wildcard); and how the reference was released (the pipeline deleted, or updated to
//! name another recipe name or another version of the same name).

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every case signs for; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// A build component document: one `build` phase, which is all a component a recipe here
/// includes needs.
const BUILD_DOCUMENT: &str = concat!(
    "name: HelloWorld\n",
    "schemaVersion: 1.0\n",
    "phases:\n",
    "  - name: build\n",
    "    steps:\n",
    "      - name: Greet\n",
    "        action: ExecuteBash\n",
    "        inputs:\n",
    "          commands:\n",
    "            - echo hello\n",
);

/// A base image in one of the forms `parentImage` admits (an AMI ID). No Amazon EC2 is mounted
/// beside this service here, so nothing answers the lookup of it and the recipe create
/// proceeds — these cases are about the deletes, not about the base image.
const PARENT_IMAGE: &str = "ami-0123456789abcdef0";

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

/// A request signed for `imagebuilder` in [`REGION`] — the signal the router routes on and the
/// handlers read the Region from.
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

/// Status and parsed body of one call; the error code is folded into the body as `__type`, so
/// one accessor reads the header and the body alike. An empty body reads as `Value::Null`.
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
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// The error code a refusal carries, from `__type` (restJson1 renders the shape there,
/// optionally namespace-qualified).
fn error_code(case: &str, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an error shape, got {body}"))
        .rsplit(['#', '/'])
        .next()
        .expect("a non-empty error type")
        .to_string()
}

/// The text one member of an answer carries.
fn text(case: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected text in {member} of {body}"))
        .to_string()
}

/// Assert a call succeeded, and answer its body.
fn assert_ok(case: &str, status: u16, body: Value) -> Value {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {body}"
    );
    body
}

/// Assert a refusal carries `expect` as its status, `code` as its error class and `message` as
/// its text.
fn assert_refused(case: &str, status: u16, body: &Value, expect: u16, code: &str, message: &str) {
    assert_eq!(
        status, expect,
        "case {case:?}: expected HTTP {expect}, got {status} with {body}"
    );
    let actual_code = error_code(case, body);
    assert_eq!(
        actual_code, code,
        "case {case:?}: expected {code}, got {actual_code:?} with {body}"
    );
    let actual = text(case, body, "message");
    assert_eq!(
        actual, message,
        "case {case:?}: expected the message {message:?}, got {actual:?}"
    );
}

/// Whether `id` has the 8-4-4-4-12 lowercase-hex form an AWS request id carries.
fn is_request_id(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && [8usize, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(width, group)| {
                group.len() == *width
                    && group
                        .chars()
                        .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
            })
}

/// Assert a delete succeeded, answered `arn` in `member`, and named the request it answers.
fn assert_deleted(case: &str, status: u16, body: &Value, member: &str, arn: &str) {
    let body = assert_ok(case, status, body.clone());
    let echoed = text(case, &body, member);
    assert_eq!(
        echoed, arn,
        "case {case:?}: expected the removed ARN {arn:?} in {member}, got {echoed:?}"
    );
    let request_id = text(case, &body, "requestId");
    assert!(
        is_request_id(&request_id),
        "case {case:?}: expected a request id, got {request_id:?}"
    );
}

/// The dependency refusal the INFRASTRUCTURE configuration carries.
fn configuration_dependency_message(arn: &str) -> String {
    format!("The resource ARN '{arn}' has dependent resources")
}

/// The dependency refusal every other resource of this service carries — the recipes and the
/// distribution configuration among them.
fn resource_dependency_message(arn: &str) -> String {
    format!(
        "Resource dependency error: The resource ARN '{arn}' has other resources depended on it."
    )
}

/// The missing-resource sentence a call ADDRESSED by the INFRASTRUCTURE configuration's own
/// ARN carries: the ARN the request named, quoted — its read and its delete.
fn configuration_not_found_message(arn: &str) -> String {
    format!("The resource ARN '{arn}' was not found.")
}

/// The missing-resource sentence that names the kind the ARN identifies — what every lookup of
/// a recipe and of the DISTRIBUTION configuration carries.
fn required_resource_message(kind: &str, arn: &str) -> String {
    format!("The following required resource '{kind}' cannot be found: '{arn}'.")
}

/// One resource the pipeline under test references: how it is read and deleted, and the
/// sentences its answers carry.
struct Dependent {
    /// What this resource is, named in the case of every assertion about it.
    label: &'static str,
    /// The ARN the calls name it by.
    arn: String,
    /// The delete under test, up to the ARN it takes in its query string.
    delete: &'static str,
    /// The read, up to the ARN it takes in its query string.
    read: &'static str,
    /// The output member that delete answers the removed ARN in.
    echoed: &'static str,
    /// The refusal the delete earns while the pipeline still reaches it.
    dependency: String,
    /// What a READ answers once it is gone.
    missing: String,
    /// What a repeat of the DELETE answers once it is gone, which the distribution
    /// configuration words differently from its read.
    missing_on_delete: String,
}

/// `CreateComponent` for one build version of `name`, answering its build version ARN.
async fn create_component(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": "1.0.0",
        "platform": "Linux",
        "clientToken": format!("component-{name}"),
        "data": BUILD_DOCUMENT,
    });
    let case = format!("create the component {name}");
    let (status, answer) = call(router, "PUT", "/CreateComponent", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "componentBuildVersionArn")
}

/// `CreateImageRecipe` for `name` at `version` over `component`, answering the recipe's ARN.
async fn create_image_recipe(
    router: &MultiServiceRouter,
    name: &str,
    version: &str,
    component: &str,
) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": version,
        "parentImage": PARENT_IMAGE,
        "clientToken": format!("recipe-{name}-{version}"),
        "components": [{ "componentArn": component }],
    });
    let case = format!("create the image recipe {name} at {version}");
    let (status, answer) = call(router, "PUT", "/CreateImageRecipe", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imageRecipeArn")
}

/// `CreateInfrastructureConfiguration` for `name`, answering its ARN.
async fn create_infrastructure_configuration(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("infra-{name}"),
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

/// `CreateDistributionConfiguration` for `name` with one target Region, answering its ARN.
async fn create_distribution_configuration(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("dist-{name}"),
        "distributions": [{ "region": REGION, "amiDistributionConfiguration": {} }],
    });
    let case = format!("create the distribution configuration {name}");
    let (status, answer) = call(
        router,
        "PUT",
        "/CreateDistributionConfiguration",
        Some(&body),
    )
    .await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "distributionConfigurationArn")
}

/// `CreateImagePipeline` for `name`, carrying the reference members `members` names, answering
/// the pipeline's ARN.
async fn create_pipeline(router: &MultiServiceRouter, name: &str, members: Value) -> String {
    let mut body = Map::new();
    body.insert("name".to_string(), json!(name));
    body.insert("clientToken".to_string(), json!(format!("create-{name}")));
    body.insert("status".to_string(), json!("DISABLED"));
    let Value::Object(members) = members else {
        panic!("the reference members of a pipeline are an object");
    };
    body.extend(members);
    let case = format!("create the pipeline {name}");
    let (status, answer) = call(
        router,
        "PUT",
        "/CreateImagePipeline",
        Some(&Value::Object(body)),
    )
    .await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imagePipelineArn")
}

/// `UpdateImagePipeline`, which replaces the stored pipeline — so the references `members`
/// carries are the ones it holds afterwards.
async fn update_pipeline(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    token: &str,
    members: Value,
) {
    let mut body = Map::new();
    body.insert("imagePipelineArn".to_string(), json!(arn));
    body.insert("clientToken".to_string(), json!(token));
    let Value::Object(members) = members else {
        panic!("the reference members of a pipeline are an object");
    };
    body.extend(members);
    let (status, answer) = call(
        router,
        "PUT",
        "/UpdateImagePipeline",
        Some(&Value::Object(body)),
    )
    .await;
    assert_ok(case, status, answer);
}

/// The versioned image-recipe reference that names `name` at `version`, which may carry
/// trailing `x` wildcards.
fn image_recipe_reference(name: &str, version: &str) -> String {
    format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:image-recipe/{name}/{version}")
}

/// The delete of `arn`, up to the ARN in its query string.
const DELETE_IMAGE_RECIPE: &str = "/DeleteImageRecipe?imageRecipeArn=";

/// Assert the delete of `dependent` is refused as the dependency it is, and that the resource
/// is still read back afterwards: a refused delete removes nothing.
async fn assert_protected(router: &MultiServiceRouter, case: &str, dependent: &Dependent) {
    let case = format!("{case}, {}", dependent.label);
    let (status, refused) = call(
        router,
        "DELETE",
        &format!("{}{}", dependent.delete, dependent.arn),
        None,
    )
    .await;
    assert_refused(
        &case,
        status,
        &refused,
        400,
        "ResourceDependencyException",
        &dependent.dependency,
    );
    let (status, still_there) = call(
        router,
        "GET",
        &format!("{}{}", dependent.read, dependent.arn),
        None,
    )
    .await;
    assert_ok(&format!("{case}: read after the refusal"), status, still_there);
}

/// Assert the delete of `dependent` removes it, and that every later lookup of that ARN — a
/// read and a repeat of the delete — reports it missing in its own sentence.
async fn assert_released(router: &MultiServiceRouter, case: &str, dependent: &Dependent) {
    let case = format!("{case}, {}", dependent.label);
    let uri = format!("{}{}", dependent.delete, dependent.arn);
    let (status, deleted) = call(router, "DELETE", &uri, None).await;
    assert_deleted(&case, status, &deleted, dependent.echoed, &dependent.arn);

    for (what, method, uri, expected) in [
        (
            "read after the delete",
            "GET",
            format!("{}{}", dependent.read, dependent.arn),
            &dependent.missing,
        ),
        (
            "deleted a second time",
            "DELETE",
            uri,
            &dependent.missing_on_delete,
        ),
    ] {
        let (status, missing) = call(router, method, &uri, None).await;
        assert_refused(
            &format!("{case}: {what}"),
            status,
            &missing,
            404,
            "ResourceNotFoundException",
            expected,
        );
    }
}

#[tokio::test]
async fn a_pipeline_protects_every_resource_it_references_until_it_is_gone() {
    // The rows differ in the version form of the recipe reference the one pipeline holds: an
    // exact version, and each width of trailing wildcard, every one of which reaches the
    // stored version. The configuration references a pipeline holds carry no version at all,
    // so they are the same in every row — what varies is whether the recipe is reached, and
    // all three resources are asserted against the SAME pipeline, so one delete of it
    // releases them together.
    for (case, reference_version) in [
        ("an exact recipe version", "1.0.0"),
        ("a patch wildcard recipe reference", "1.0.x"),
        ("a minor wildcard recipe reference", "1.x.x"),
        ("a fully wildcarded recipe reference", "x.x.x"),
    ] {
        let router = emulator();
        let component = create_component(&router, "teardown-all-component").await;
        let recipe =
            create_image_recipe(&router, "teardown-all-recipe", "1.0.0", &component).await;
        let infrastructure =
            create_infrastructure_configuration(&router, "Teardown All Infra").await;
        let distribution = create_distribution_configuration(&router, "Teardown All Dist").await;
        let pipeline = create_pipeline(
            &router,
            "teardown-all-pipeline",
            json!({
                "imageRecipeArn": image_recipe_reference("teardown-all-recipe", reference_version),
                "infrastructureConfigurationArn": infrastructure.clone(),
                "distributionConfigurationArn": distribution.clone(),
            }),
        )
        .await;

        let dependents = [
            Dependent {
                label: "the image recipe it builds",
                arn: recipe.clone(),
                delete: DELETE_IMAGE_RECIPE,
                read: "/GetImageRecipe?imageRecipeArn=",
                echoed: "imageRecipeArn",
                dependency: resource_dependency_message(&recipe),
                missing: required_resource_message("ImageRecipe", &recipe),
                missing_on_delete: required_resource_message("ImageRecipe", &recipe),
            },
            Dependent {
                label: "the infrastructure configuration it builds in",
                arn: infrastructure.clone(),
                delete:
                    "/DeleteInfrastructureConfiguration?infrastructureConfigurationArn=",
                read: "/GetInfrastructureConfiguration?infrastructureConfigurationArn=",
                echoed: "infrastructureConfigurationArn",
                dependency: configuration_dependency_message(&infrastructure),
                missing: configuration_not_found_message(&infrastructure),
                missing_on_delete: configuration_not_found_message(&infrastructure),
            },
            Dependent {
                label: "the distribution configuration it distributes with",
                arn: distribution.clone(),
                delete: "/DeleteDistributionConfiguration?distributionConfigurationArn=",
                read: "/GetDistributionConfiguration?distributionConfigurationArn=",
                echoed: "distributionConfigurationArn",
                dependency: resource_dependency_message(&distribution),
                missing: required_resource_message("DistributionConfiguration", &distribution),
                missing_on_delete: required_resource_message(
                    "DistributionConfiguration",
                    &distribution,
                ),
            },
        ];

        // While the pipeline stands, each of the three deletes is refused and removes nothing.
        for dependent in &dependents {
            assert_protected(&router, case, dependent).await;
        }

        // One delete of the pipeline releases all three at once.
        let (status, deleted) = call(
            &router,
            "DELETE",
            &format!("/DeleteImagePipeline?imagePipelineArn={pipeline}"),
            None,
        )
        .await;
        assert_deleted(
            &format!("{case}, the pipeline holding the references"),
            status,
            &deleted,
            "imagePipelineArn",
            &pipeline,
        );

        for dependent in &dependents {
            assert_released(&router, case, dependent).await;
        }
    }
}

#[tokio::test]
async fn a_recipe_is_released_the_instant_the_pipeline_stops_naming_it() {
    // The rows differ in how the recipe the pipeline is pointed at differs from the one it
    // lets go of: another recipe name, and another version of the same name — so the release
    // is asserted both where the name changes and where only the version does.
    for (case, replacement_name, replacement_version) in [
        ("pointed at another recipe name", "released-other-recipe", "1.0.0"),
        (
            "pointed at another version of the same name",
            "released-recipe",
            "2.0.0",
        ),
    ] {
        let router = emulator();
        let component = create_component(&router, "released-component").await;
        let held = create_image_recipe(&router, "released-recipe", "1.0.0", &component).await;
        let replacement =
            create_image_recipe(&router, replacement_name, replacement_version, &component).await;
        let infrastructure = create_infrastructure_configuration(&router, "Released Infra").await;
        let pipeline = create_pipeline(
            &router,
            "released-pipeline",
            json!({
                "imageRecipeArn": held.clone(),
                "infrastructureConfigurationArn": infrastructure.clone(),
            }),
        )
        .await;

        let held_dependent = Dependent {
            label: "the recipe the pipeline named",
            arn: held.clone(),
            delete: DELETE_IMAGE_RECIPE,
            read: "/GetImageRecipe?imageRecipeArn=",
            echoed: "imageRecipeArn",
            dependency: resource_dependency_message(&held),
            missing: required_resource_message("ImageRecipe", &held),
            missing_on_delete: required_resource_message("ImageRecipe", &held),
        };
        let replacement_dependent = Dependent {
            label: "the recipe the pipeline was pointed at",
            arn: replacement.clone(),
            delete: DELETE_IMAGE_RECIPE,
            read: "/GetImageRecipe?imageRecipeArn=",
            echoed: "imageRecipeArn",
            dependency: resource_dependency_message(&replacement),
            missing: required_resource_message("ImageRecipe", &replacement),
            missing_on_delete: required_resource_message("ImageRecipe", &replacement),
        };

        // Before the update, the recipe the pipeline names is protected and the other is not
        // yet reached by anything — which the update below reverses.
        assert_protected(&router, case, &held_dependent).await;

        update_pipeline(
            &router,
            &format!("{case}, the update"),
            &pipeline,
            "released-update",
            json!({
                "imageRecipeArn": replacement.clone(),
                "infrastructureConfigurationArn": infrastructure.clone(),
            }),
        )
        .await;

        // The dependency the delete asks about is the reference the store holds now: the
        // released recipe deletes, and the one the pipeline was pointed at is protected in its
        // place.
        assert_released(&router, case, &held_dependent).await;
        assert_protected(&router, case, &replacement_dependent).await;
    }
}
