// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the DELETE half of this service's resource lifecycle: what a
//! delete does to the store, when it is refused, and what a later create of the same name
//! answers.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is
//! what a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! * a delete REMOVES the resource from this service's store and echoes the ARN it removed
//!   together with a request id, so every later read of that ARN reports it missing and the
//!   resources it depended on can themselves be deleted;
//! * a resource another stored resource still REFERENCES is not removed: the delete is
//!   refused as the dependency it is (`ResourceDependencyException`, HTTP 400) and the store
//!   is left as it was, so a teardown has to proceed in dependency order — the pipeline
//!   before the recipe and the configurations it reaches, and the recipe before the
//!   components it applies. A reference carrying trailing `x` wildcards reaches whichever
//!   version is newest at the time, so every version it admits is still in use;
//! * a create this service REFUSES stores nothing, so the name and version it would have
//!   taken are still free for the next request, and so is a name whose resource was deleted;
//! * a create RESOLVES what its request references before it mints anything — including the
//!   base image an AMI ID names, against the account's own Amazon EC2.
//!
//! The rows span what the rules generalise over: which kind of resource the delete acts on
//! (the two recipe kinds, the two configurations, a component, a pipeline), which stored
//! resource holds the reference, whether that reference names an exact version or a wildcard,
//! and why a create was refused.

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

/// An inline Dockerfile template, with the contextual variable Image Builder replaces at
/// build time.
const CONTAINER_TEMPLATE: &str = "FROM {{{ imagebuilder:parentImage }}}\nRUN echo hello\n";

/// A base image in one of the forms `parentImage` admits (an AMI ID). No Amazon EC2 is
/// mounted beside this service here, so nothing answers the lookup of it and the create
/// proceeds — which is the point: these cases are about the delete, not about the base image.
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

/// One emulator: imagebuilder and Amazon EC2 on a single shared engine behind the production
/// router — what `build_router` assembles, narrowed to the two services under test — together
/// with the engine, so a case can read the image catalog EC2 answers from.
fn emulator_with_ec2() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_imagebuilder_imagebuilder::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
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
fn text(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected text in {name} of {body}"))
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

/// Assert a refusal carries `status`, `code` and `message`.
fn assert_refused(case: &str, status: u16, body: &Value, expect: u16, code: &str, message: &str) {
    assert_eq!(
        status, expect,
        "case {case:?}: expected HTTP {expect}, got {status} with {body}"
    );
    let actual_code = error_code(case, body);
    assert_eq!(
        actual_code, code,
        "case {case:?}: expected {code:?}, got {actual_code:?} with {body}"
    );
    let actual_message = text(case, body, "message");
    assert_eq!(
        actual_message, message,
        "case {case:?}: expected the message {message:?}, got {actual_message:?}"
    );
}

/// Whether `id` has the 8-4-4-4-12 lowercase-hex form an AWS request id carries.
fn is_request_id(id: &str) -> bool {
    let mut groups = id.split('-');
    for width in [8usize, 4, 4, 4, 12] {
        let Some(group) = groups.next() else {
            return false;
        };
        if group.len() != width
            || !group
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return false;
        }
    }
    groups.next().is_none()
}

/// Assert a delete succeeded, echoed `arn` in `member`, and named the request it answers.
fn assert_deleted(case: &str, status: u16, body: &Value, member: &str, arn: &str) {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the delete, got {status} with {body}"
    );
    let echoed = text(case, body, member);
    assert_eq!(
        echoed, arn,
        "case {case:?}: expected the delete to name {arn:?} in {member}, got {echoed:?}"
    );
    let request_id = text(case, body, "requestId");
    assert!(
        is_request_id(&request_id),
        "case {case:?}: expected the delete to name the request it answers, got {request_id:?}"
    );
}

/// The dependency refusal the components, recipes, pipelines and the DISTRIBUTION
/// configuration carry.
fn dependency_message(arn: &str) -> String {
    format!(
        "Resource dependency error: The resource ARN '{arn}' has other resources depended on \
         it."
    )
}

/// The dependency refusal the INFRASTRUCTURE configuration carries.
fn configuration_dependency_message(arn: &str) -> String {
    format!("The resource ARN '{arn}' has dependent resources")
}

/// The not-found report this service's authoring and build resources carry, naming the kind
/// the ARN identifies.
fn required_resource_message(resource_type: &str, arn: &str) -> String {
    format!("The following required resource '{resource_type}' cannot be found: '{arn}'.")
}

/// The not-found report the INFRASTRUCTURE configuration carries, quoting the ARN the request
/// named: the one resource of this service reported by its ARN alone, for its read and for
/// its delete alike.
fn configuration_not_found_message(arn: &str) -> String {
    format!("The resource ARN '{arn}' was not found.")
}

/// The ARN one build version of the component `normalized` carries.
fn component_arn(normalized: &str, version: &str, build: u32) -> String {
    format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:component/{normalized}/{version}/{build}")
}

/// The ARN the resource `normalized` carries under `prefix`, with whatever `suffix` the kind
/// adds (a version, or nothing).
fn arn_of(prefix: &str, normalized: &str, suffix: &str) -> String {
    format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:{prefix}/{normalized}{suffix}")
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
    let (status, answered) = call(router, "PUT", "/CreateComponent", Some(&body)).await;
    let answered = assert_ok(&case, status, answered);
    text(&case, &answered, "componentBuildVersionArn")
}

/// `CreateImageRecipe` for `name` at `version` over `component`, answering the status and
/// the body.
async fn create_image_recipe(
    router: &MultiServiceRouter,
    name: &str,
    version: &str,
    component: &str,
) -> (u16, Value) {
    let body = json!({
        "name": name,
        "semanticVersion": version,
        "parentImage": PARENT_IMAGE,
        "clientToken": format!("recipe-{name}-{version}"),
        "components": [{ "componentArn": component }],
    });
    call(router, "PUT", "/CreateImageRecipe", Some(&body)).await
}

/// `CreateContainerRecipe` for `name` over `component`, answering its ARN.
async fn create_container_recipe(
    router: &MultiServiceRouter,
    name: &str,
    component: &str,
) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": "1.0.0",
        "containerType": "DOCKER",
        "parentImage": "amazonlinux:latest",
        "clientToken": format!("boxed-{name}"),
        "dockerfileTemplateData": CONTAINER_TEMPLATE,
        "targetRepository": { "service": "ECR", "repositoryName": "team/images" },
        "components": [{ "componentArn": component }],
    });
    let case = format!("create the container recipe {name}");
    let (status, answered) = call(router, "PUT", "/CreateContainerRecipe", Some(&body)).await;
    let answered = assert_ok(&case, status, answered);
    text(&case, &answered, "containerRecipeArn")
}

/// `CreateInfrastructureConfiguration` for `name`, answering its ARN.
async fn create_infrastructure_configuration(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("infra-{name}"),
        "instanceProfileName": "EC2InstanceProfileForImageBuilder",
    });
    let case = format!("create the infrastructure configuration {name}");
    let (status, answered) = call(
        router,
        "PUT",
        "/CreateInfrastructureConfiguration",
        Some(&body),
    )
    .await;
    let answered = assert_ok(&case, status, answered);
    text(&case, &answered, "infrastructureConfigurationArn")
}

/// `CreateDistributionConfiguration` for `name`, answering its ARN.
async fn create_distribution_configuration(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("dist-{name}"),
        "distributions": [{ "region": REGION, "amiDistributionConfiguration": {} }],
    });
    let case = format!("create the distribution configuration {name}");
    let (status, answered) =
        call(router, "PUT", "/CreateDistributionConfiguration", Some(&body)).await;
    let answered = assert_ok(&case, status, answered);
    text(&case, &answered, "distributionConfigurationArn")
}

/// `CreateImagePipeline` for `name` over whatever `extra` names beside the infrastructure
/// configuration, answering its ARN.
async fn create_pipeline(
    router: &MultiServiceRouter,
    name: &str,
    configuration: &str,
    extra: Value,
) -> String {
    let mut body = Map::new();
    body.insert("name".to_string(), json!(name));
    body.insert("clientToken".to_string(), json!(format!("pipeline-{name}")));
    body.insert(
        "infrastructureConfigurationArn".to_string(),
        json!(configuration),
    );
    if let Value::Object(extra) = extra {
        for (member, value) in extra {
            body.insert(member, value);
        }
    }
    let case = format!("create the pipeline {name}");
    let (status, answered) = call(
        router,
        "PUT",
        "/CreateImagePipeline",
        Some(&Value::Object(body)),
    )
    .await;
    let answered = assert_ok(&case, status, answered);
    text(&case, &answered, "imagePipelineArn")
}

/// One resource a stored pipeline reaches: how to delete it, how to read it, and the two
/// sentences its own family carries.
struct Dependent {
    /// What this row is, named in every assertion it makes.
    case: &'static str,
    /// The ARN of the resource the delete acts on.
    arn: String,
    /// The operation and the member the delete names the resource by.
    delete: &'static str,
    /// The operation and the member a read names it by.
    read: &'static str,
    /// The output member the delete echoes the removed ARN in.
    echoed: &'static str,
    /// The refusal a delete earns while the pipeline still reaches it.
    dependency: String,
    /// The report a READ answers once it is gone.
    missing: String,
    /// The report a REPEAT DELETE answers once it is gone: the same sentence as the read's,
    /// because the template follows the resource's kind rather than the operation.
    missing_on_delete: String,
}

#[tokio::test]
async fn a_resource_a_pipeline_reaches_is_deleted_only_once_the_pipeline_is_gone() {
    // Every kind of resource an image pipeline references. The rows differ in the kind of
    // resource the delete acts on, in which member of the pipeline holds the reference, and
    // in the two sentences that kind's family carries.
    for row_case in [
        "an image recipe",
        "a container recipe",
        "an infrastructure configuration",
        "a distribution configuration",
    ] {
        let router = emulator();
        let component = create_component(&router, "teardown-component").await;
        let configuration = create_infrastructure_configuration(&router, "Teardown Infra").await;
        let distribution = create_distribution_configuration(&router, "Teardown Dist").await;
        let container = create_container_recipe(&router, "teardown-boxed", &component).await;
        let (status, created) =
            create_image_recipe(&router, "teardown-recipe", "1.0.0", &component).await;
        let created = assert_ok("create the image recipe", status, created);
        let image_recipe = text("create the image recipe", &created, "imageRecipeArn");

        // The pipeline names the recipe kind this row is about, so the reference under test
        // is the one the row names.
        let recipe_member = if row_case == "a container recipe" {
            json!({ "containerRecipeArn": container.clone(), "distributionConfigurationArn": distribution.clone() })
        } else {
            json!({ "imageRecipeArn": image_recipe.clone(), "distributionConfigurationArn": distribution.clone() })
        };
        let pipeline =
            create_pipeline(&router, "teardown-pipeline", &configuration, recipe_member).await;

        let dependent = match row_case {
            "an image recipe" => Dependent {
                case: "an image recipe a pipeline builds",
                arn: image_recipe.clone(),
                delete: "/DeleteImageRecipe?imageRecipeArn=",
                read: "/GetImageRecipe?imageRecipeArn=",
                echoed: "imageRecipeArn",
                dependency: dependency_message(&image_recipe),
                missing: required_resource_message("ImageRecipe", &image_recipe),
                missing_on_delete: required_resource_message("ImageRecipe", &image_recipe),
            },
            "a container recipe" => Dependent {
                case: "a container recipe a pipeline builds",
                arn: container.clone(),
                delete: "/DeleteContainerRecipe?containerRecipeArn=",
                read: "/GetContainerRecipe?containerRecipeArn=",
                echoed: "containerRecipeArn",
                dependency: dependency_message(&container),
                missing: required_resource_message("ContainerRecipe", &container),
                missing_on_delete: required_resource_message("ContainerRecipe", &container),
            },
            "an infrastructure configuration" => Dependent {
                case: "an infrastructure configuration a pipeline builds in",
                arn: configuration.clone(),
                delete: "/DeleteInfrastructureConfiguration?infrastructureConfigurationArn=",
                read: "/GetInfrastructureConfiguration?infrastructureConfigurationArn=",
                echoed: "infrastructureConfigurationArn",
                dependency: configuration_dependency_message(&configuration),
                missing: configuration_not_found_message(&configuration),
                missing_on_delete: configuration_not_found_message(&configuration),
            },
            _ => Dependent {
                case: "a distribution configuration a pipeline distributes with",
                arn: distribution.clone(),
                delete: "/DeleteDistributionConfiguration?distributionConfigurationArn=",
                read: "/GetDistributionConfiguration?distributionConfigurationArn=",
                echoed: "distributionConfigurationArn",
                dependency: dependency_message(&distribution),
                missing: required_resource_message("DistributionConfiguration", &distribution),
                missing_on_delete: required_resource_message(
                    "DistributionConfiguration",
                    &distribution,
                ),
            },
        };

        // While the pipeline stands, the delete is refused and the resource is left alone.
        let (status, refused) = call(
            &router,
            "DELETE",
            &format!("{}{}", dependent.delete, dependent.arn),
            None,
        )
        .await;
        assert_refused(
            dependent.case,
            status,
            &refused,
            400,
            "ResourceDependencyException",
            &dependent.dependency,
        );
        let (status, still_there) = call(
            &router,
            "GET",
            &format!("{}{}", dependent.read, dependent.arn),
            None,
        )
        .await;
        assert_ok(
            &format!("{}, read after the refusal", dependent.case),
            status,
            still_there,
        );

        // Once the pipeline is gone the delete succeeds, echoing the ARN it removed, and
        // every later read of that ARN reports it missing.
        let (status, deleted) = call(
            &router,
            "DELETE",
            &format!("/DeleteImagePipeline?imagePipelineArn={pipeline}"),
            None,
        )
        .await;
        assert_deleted(
            &format!("{}, the pipeline that reached it", dependent.case),
            status,
            &deleted,
            "imagePipelineArn",
            &pipeline,
        );
        let (status, deleted) = call(
            &router,
            "DELETE",
            &format!("{}{}", dependent.delete, dependent.arn),
            None,
        )
        .await;
        assert_deleted(
            dependent.case,
            status,
            &deleted,
            dependent.echoed,
            &dependent.arn,
        );
        let (status, missing) = call(
            &router,
            "GET",
            &format!("{}{}", dependent.read, dependent.arn),
            None,
        )
        .await;
        assert_refused(
            &format!("{}, read after the delete", dependent.case),
            status,
            &missing,
            404,
            "ResourceNotFoundException",
            &dependent.missing,
        );

        // And the repeat of a delete that already succeeded is reported the same way.
        let (status, repeated) = call(
            &router,
            "DELETE",
            &format!("{}{}", dependent.delete, dependent.arn),
            None,
        )
        .await;
        assert_refused(
            &format!("{}, deleted twice", dependent.case),
            status,
            &repeated,
            404,
            "ResourceNotFoundException",
            &dependent.missing_on_delete,
        );
    }
}

#[tokio::test]
async fn a_teardown_in_dependency_order_removes_every_resource_it_created() {
    let router = emulator();

    // The chain a caller builds: a component, a recipe that applies it, and a pipeline that
    // builds the recipe in an infrastructure configuration.
    let component = create_component(&router, "chain-component").await;
    let configuration = create_infrastructure_configuration(&router, "Chain Infra").await;
    let (status, created) = create_image_recipe(&router, "chain-recipe", "1.0.0", &component).await;
    let created = assert_ok("create the recipe of the chain", status, created);
    let recipe = text("create the recipe of the chain", &created, "imageRecipeArn");
    let pipeline = create_pipeline(
        &router,
        "chain-pipeline",
        &configuration,
        json!({ "imageRecipeArn": recipe.clone() }),
    )
    .await;

    // Out of order, each delete is refused for the dependency that stands — and the resource
    // the delete named is still readable afterwards.
    for (case, uri, arn) in [
        (
            "the component the recipe applies",
            format!("/DeleteComponent?componentBuildVersionArn={component}"),
            component.clone(),
        ),
        (
            "the recipe the pipeline builds",
            format!("/DeleteImageRecipe?imageRecipeArn={recipe}"),
            recipe.clone(),
        ),
    ] {
        let (status, refused) = call(&router, "DELETE", &uri, None).await;
        assert_refused(
            case,
            status,
            &refused,
            400,
            "ResourceDependencyException",
            &dependency_message(&arn),
        );
    }

    // In order, every delete succeeds: the pipeline, then the recipe it built, then the
    // component that recipe applied, then the configuration it built in.
    for (case, uri, member, arn) in [
        (
            "the pipeline",
            format!("/DeleteImagePipeline?imagePipelineArn={pipeline}"),
            "imagePipelineArn",
            pipeline.clone(),
        ),
        (
            "the recipe",
            format!("/DeleteImageRecipe?imageRecipeArn={recipe}"),
            "imageRecipeArn",
            recipe.clone(),
        ),
        (
            "the component",
            format!("/DeleteComponent?componentBuildVersionArn={component}"),
            "componentBuildVersionArn",
            component.clone(),
        ),
        (
            "the infrastructure configuration",
            format!(
                "/DeleteInfrastructureConfiguration?infrastructureConfigurationArn={configuration}"
            ),
            "infrastructureConfigurationArn",
            configuration.clone(),
        ),
    ] {
        let (status, deleted) = call(&router, "DELETE", &uri, None).await;
        assert_deleted(case, status, &deleted, member, &arn);
    }

    // Nothing of the chain is left: each read reports its own family's missing-resource
    // sentence.
    for (case, uri, expected) in [
        (
            "the deleted pipeline",
            format!("/GetImagePipeline?imagePipelineArn={pipeline}"),
            required_resource_message("ImagePipeline", &pipeline),
        ),
        (
            "the deleted recipe",
            format!("/GetImageRecipe?imageRecipeArn={recipe}"),
            required_resource_message("ImageRecipe", &recipe),
        ),
        (
            "the deleted component",
            format!("/GetComponent?componentBuildVersionArn={component}"),
            required_resource_message("Component", &component),
        ),
        (
            "the deleted configuration",
            format!(
                "/GetInfrastructureConfiguration?infrastructureConfigurationArn={configuration}"
            ),
            configuration_not_found_message(&configuration),
        ),
    ] {
        let (status, missing) = call(&router, "GET", &uri, None).await;
        assert_refused(
            case,
            status,
            &missing,
            404,
            "ResourceNotFoundException",
            &expected,
        );
    }

    // And the names are free again: the same configuration name and the same recipe
    // name+version are created a second time, each minting the ARN it had before.
    let recreated = create_infrastructure_configuration(&router, "Chain Infra").await;
    assert_eq!(
        recreated, configuration,
        "case \"the recreated configuration\": expected {configuration:?}, got {recreated:?}"
    );
    let recreated_component = create_component(&router, "chain-component").await;
    let (status, created) =
        create_image_recipe(&router, "chain-recipe", "1.0.0", &recreated_component).await;
    let created = assert_ok("the recreated recipe", status, created);
    let recreated_recipe = text("the recreated recipe", &created, "imageRecipeArn");
    assert_eq!(
        recreated_recipe, recipe,
        "case \"the recreated recipe\": expected {recipe:?}, got {recreated_recipe:?}"
    );
}

#[tokio::test]
async fn a_wildcard_reference_keeps_every_version_it_reaches_in_use() {
    let router = emulator();

    let component = create_component(&router, "wildcard-component").await;
    let configuration = create_infrastructure_configuration(&router, "Wildcard Infra").await;
    let mut recipes: Vec<String> = Vec::new();
    for version in ["1.0.0", "1.1.0", "2.0.0"] {
        let (status, created) =
            create_image_recipe(&router, "wildcard-recipe", version, &component).await;
        let created = assert_ok(
            &format!("create the recipe at {version}"),
            status,
            created,
        );
        recipes.push(text(
            &format!("create the recipe at {version}"),
            &created,
            "imageRecipeArn",
        ));
    }

    // "Version ARNs resolve to the latest available matching" record, so a pipeline holding
    // a wildcard reference reaches whichever version is newest at the time — every version
    // the reference admits is still in use, not only the newest.
    let wildcard = arn_of("image-recipe", "wildcard-recipe", "/1.x.x");
    let pipeline = create_pipeline(
        &router,
        "wildcard-pipeline",
        &configuration,
        json!({ "imageRecipeArn": wildcard }),
    )
    .await;

    // (case, the recipe, whether the wildcard reaches it)
    let rows: Vec<(&str, &String, bool)> = vec![
        ("the version the wildcard's major node admits", &recipes[0], true),
        ("a later minor version it also admits", &recipes[1], true),
        ("a version outside the wildcard's major node", &recipes[2], false),
    ];
    for (case, arn, reached) in rows {
        let (status, answered) = call(
            &router,
            "DELETE",
            &format!("/DeleteImageRecipe?imageRecipeArn={arn}"),
            None,
        )
        .await;
        if reached {
            assert_refused(
                case,
                status,
                &answered,
                400,
                "ResourceDependencyException",
                &dependency_message(arn),
            );
        } else {
            assert_deleted(case, status, &answered, "imageRecipeArn", arn);
        }
    }

    // Once the pipeline is gone, the versions it reached delete too.
    let (status, deleted) = call(
        &router,
        "DELETE",
        &format!("/DeleteImagePipeline?imagePipelineArn={pipeline}"),
        None,
    )
    .await;
    assert_deleted(
        "the pipeline holding the wildcard",
        status,
        &deleted,
        "imagePipelineArn",
        &pipeline,
    );
    for (case, arn) in [
        ("the version the wildcard reached", &recipes[0]),
        ("the later minor version", &recipes[1]),
    ] {
        let (status, deleted) = call(
            &router,
            "DELETE",
            &format!("/DeleteImageRecipe?imageRecipeArn={arn}"),
            None,
        )
        .await;
        assert_deleted(case, status, &deleted, "imageRecipeArn", arn);
    }
}

#[tokio::test]
async fn a_refused_create_leaves_the_name_and_version_it_would_have_taken_free() {
    let router = emulator();
    let component = create_component(&router, "retry-component").await;

    // Each row is a create this service refuses, and the class it refuses it with. They span
    // the reasons a create of this resource is refused: a base image written in no form the
    // member admits, one that announces a form and is not it, and a component reference that
    // reaches nothing.
    let absent_component = component_arn("retry-component", "9.9.9", 1);
    let rows: Vec<(&str, Value, u16, &str, String)> = vec![
        (
            "a base image in no form the member admits",
            json!({
                "name": "retry-recipe",
                "semanticVersion": "1.0.0",
                "parentImage": "amazonlinux2",
                "clientToken": "retry-0001",
                "components": [{ "componentArn": component.clone() }],
            }),
            400,
            "InvalidParameterValueException",
            "The value supplied for parameter 'parentImage' is not valid. The provided parent \
             image must be either an EC2 AMI Id, SSM Parameter, Marketplace Product Id, or a \
             valid Image Builder Image Arn."
                .to_string(),
        ),
        (
            "an ARN that names no image of this service",
            json!({
                "name": "retry-recipe",
                "semanticVersion": "1.0.0",
                "parentImage":
                    format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:image-recipe/not-an-image/1.0.0"),
                "clientToken": "retry-0002",
                "components": [{ "componentArn": component.clone() }],
            }),
            400,
            "InvalidParameterValueException",
            "The value supplied for parameter 'parentImage' is not valid. The supplied image \
             identifier is not in a supported format."
                .to_string(),
        ),
        (
            "a component reference that reaches nothing",
            json!({
                "name": "retry-recipe",
                "semanticVersion": "1.0.0",
                "parentImage": PARENT_IMAGE,
                "clientToken": "retry-0003",
                "components": [{ "componentArn": absent_component.clone() }],
            }),
            404,
            "ResourceNotFoundException",
            required_resource_message("Component", &absent_component),
        ),
    ];

    let arn = arn_of("image-recipe", "retry-recipe", "/1.0.0");
    for (case, body, status_expected, code, message) in &rows {
        let (status, refused) = call(&router, "PUT", "/CreateImageRecipe", Some(body)).await;
        assert_refused(case, status, &refused, *status_expected, code, message);

        // The refused create stored nothing: the ARN it would have minted is reported
        // missing, so the name and version are still free.
        let (status, missing) = call(
            &router,
            "GET",
            &format!("/GetImageRecipe?imageRecipeArn={arn}"),
            None,
        )
        .await;
        assert_refused(
            &format!("{case}, the recipe it would have created"),
            status,
            &missing,
            404,
            "ResourceNotFoundException",
            &required_resource_message("ImageRecipe", &arn),
        );
    }

    // The create the refusals left room for: the same name and version, accepted.
    let (status, created) = create_image_recipe(&router, "retry-recipe", "1.0.0", &component).await;
    let created = assert_ok("the create after the refusals", status, created);
    let minted = text("the create after the refusals", &created, "imageRecipeArn");
    assert_eq!(
        minted, arn,
        "case \"the create after the refusals\": expected {arn:?}, got {minted:?}"
    );
}

#[tokio::test]
async fn a_build_number_counts_only_the_versions_the_store_holds() {
    let router = emulator();

    // Each row is a `CreateComponent` this service refuses, and the class it refuses it with:
    // the two halves of the document group (both members, then neither), a version outside
    // the documented bound, and a request omitting a member its shape requires. They span the
    // reasons a create of a component is refused before anything is stored.
    let rows: Vec<(&str, Value, u16, &str, &str)> = vec![
        (
            "a component naming both a document and a uri",
            json!({
                "name": "counted-component",
                "semanticVersion": "1.0.0",
                "platform": "Linux",
                "clientToken": "counted-0001",
                "data": BUILD_DOCUMENT,
                "uri": "s3://documents/component.yaml",
            }),
            400,
            "InvalidParameterCombinationException",
            "The value supplied for parameter 'data' and 'uri' is not valid. You must specify \
             either data or a uri.",
        ),
        (
            "a component naming neither a document nor a uri",
            json!({
                "name": "counted-component",
                "semanticVersion": "1.0.0",
                "platform": "Linux",
                "clientToken": "counted-0002",
            }),
            400,
            "MissingParametersException",
            "Required parameters are missing from the request: 'data' or 'uri'. Verify that you \
             have entered all of the required parameters in your request and try again.",
        ),
        (
            "a component version past the documented bound",
            json!({
                "name": "counted-component",
                "semanticVersion": "1073741824.0.0",
                "platform": "Linux",
                "clientToken": "counted-0003",
                "data": BUILD_DOCUMENT,
            }),
            400,
            "InvalidParameterValueException",
            "The value supplied for parameter 'semanticVersion' is not valid. The supplied \
             semantic version does not follow the required format.",
        ),
        (
            "a component omitting a member its shape requires",
            json!({
                "name": "counted-component",
                "semanticVersion": "1.0.0",
                "clientToken": "counted-0004",
                "data": BUILD_DOCUMENT,
            }),
            400,
            "InvalidRequestException",
            "Your request is invalid. Request must contain ['platform'] properties. Verify your \
             request and try again.",
        ),
    ];

    for (case, body, expected_status, code, message) in &rows {
        let (status, refused) = call(&router, "PUT", "/CreateComponent", Some(body)).await;
        assert_refused(case, status, &refused, *expected_status, code, message);
    }

    // "Image Builder automatically assigns the build number to the fourth node", counting the
    // build versions the store HOLDS — and a refused create holds nothing, so the first
    // create this service accepts is the first build version whatever was refused before it.
    let first = component_arn("counted-component", "1.0.0", 1);
    let minted = create_component(&router, "counted-component").await;
    assert_eq!(
        minted, first,
        "case \"the first accepted create\": expected {first:?}, got {minted:?}"
    );

    // The next create of the same name and version counts up from the one that is stored, and
    // after its build version is deleted the number it held is free again.
    let body = json!({
        "name": "counted-component",
        "semanticVersion": "1.0.0",
        "platform": "Linux",
        "clientToken": "counted-0006",
        "changeDescription": "a second build version",
        "data": BUILD_DOCUMENT,
    });
    let (status, created) = call(&router, "PUT", "/CreateComponent", Some(&body)).await;
    let created = assert_ok("the second accepted create", status, created);
    let second = component_arn("counted-component", "1.0.0", 2);
    let minted = text(
        "the second accepted create",
        &created,
        "componentBuildVersionArn",
    );
    assert_eq!(
        minted, second,
        "case \"the second accepted create\": expected {second:?}, got {minted:?}"
    );

    let (status, deleted) = call(
        &router,
        "DELETE",
        &format!("/DeleteComponent?componentBuildVersionArn={second}"),
        None,
    )
    .await;
    assert_deleted(
        "the second build version",
        status,
        &deleted,
        "componentBuildVersionArn",
        &second,
    );
    let (status, created) = call(&router, "PUT", "/CreateComponent", Some(&body)).await;
    let created = assert_ok("the create after the delete", status, created);
    let minted = text(
        "the create after the delete",
        &created,
        "componentBuildVersionArn",
    );
    assert_eq!(
        minted, second,
        "case \"the create after the delete\": expected {second:?}, got {minted:?}"
    );
}

#[tokio::test]
async fn an_ami_the_account_cannot_see_is_reported_missing_before_a_recipe_is_stored() {
    let (router, engine) = emulator_with_ec2();
    let component = create_component(&router, "resolved-component").await;

    // An AMI ID Amazon EC2 holds an image for, taken from the catalog EC2 itself answers
    // from: this is the value a caller would have resolved through `DescribeImages`, not a
    // literal this test chose.
    let held = {
        let state = service_com_amazonaws_ec2_amazonec2::state::service_state(&*engine)
            .expect("the shared engine holds Amazon EC2's store");
        let store = state.lock();
        store
            .images
            .keys()
            .next()
            .cloned()
            .expect("Amazon EC2 publishes a catalog of public AMIs")
    };

    // (case, the parentImage, what the create answers)
    let rows: Vec<(&str, String, Option<String>)> = vec![
        (
            "an AMI ID this account can see",
            held.clone(),
            None,
        ),
        (
            "an AMI ID no image of this account carries",
            "ami-00000000000000000".to_string(),
            Some(required_resource_message("Image", "ami-00000000000000000")),
        ),
        (
            "an SSM parameter, which names no AMI this lookup resolves",
            "ssm:/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64"
                .to_string(),
            None,
        ),
        (
            "a managed Image Builder image ARN, which this account does not hold",
            format!("arn:aws:imagebuilder:{REGION}:aws:image/amazon-linux-2-x86/x.x.x"),
            None,
        ),
    ];

    for (row, (case, parent_image, refusal)) in rows.iter().enumerate() {
        let name = format!("resolved-recipe-{row}");
        let body = json!({
            "name": name,
            "semanticVersion": "1.0.0",
            "parentImage": parent_image,
            "clientToken": format!("resolved-{row}"),
            "components": [{ "componentArn": component.clone() }],
        });
        let (status, answered) = call(&router, "PUT", "/CreateImageRecipe", Some(&body)).await;
        let arn = arn_of("image-recipe", &name, "/1.0.0");
        match refusal {
            // EC2 answered that the account can see no image under that id, so the create is
            // refused as the missing resource it names and stores nothing.
            Some(message) => {
                assert_refused(
                    case,
                    status,
                    &answered,
                    404,
                    "ResourceNotFoundException",
                    message,
                );
                let (status, missing) = call(
                    &router,
                    "GET",
                    &format!("/GetImageRecipe?imageRecipeArn={arn}"),
                    None,
                )
                .await;
                assert_refused(
                    &format!("{case}, the recipe it would have created"),
                    status,
                    &missing,
                    404,
                    "ResourceNotFoundException",
                    &required_resource_message("ImageRecipe", &arn),
                );
            }
            // Either EC2 confirmed the image, or the value names no AMI for this lookup to
            // resolve: nothing stands in the way of the create, and the recipe reads back.
            None => {
                let created = assert_ok(case, status, answered);
                let minted = text(case, &created, "imageRecipeArn");
                assert_eq!(
                    minted, arn,
                    "case {case:?}: expected the create to mint {arn:?}, got {minted:?}"
                );
                let (status, read) = call(
                    &router,
                    "GET",
                    &format!("/GetImageRecipe?imageRecipeArn={arn}"),
                    None,
                )
                .await;
                let read = assert_ok(&format!("{case}, read back"), status, read);
                let stored = read
                    .get("imageRecipe")
                    .and_then(|recipe| recipe.get("parentImage"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert_eq!(
                    stored, parent_image,
                    "case {case:?}: expected the stored recipe to carry {parent_image:?}, got \
                     {stored:?}"
                );
            }
        }
    }

    // With no EC2 mounted, the same unheld AMI ID establishes nothing — no lookup is
    // answered — so the create proceeds rather than refusing a value no answer placed.
    let alone = emulator();
    let component = create_component(&alone, "resolved-component").await;
    let (status, created) =
        create_image_recipe(&alone, "unresolved-recipe", "1.0.0", &component).await;
    assert_ok("an AMI ID with no Amazon EC2 to ask", status, created);
}

/// RULE: a create this service refuses stores NOTHING, so no resource the refused request
/// referenced is held by a dependency afterwards and the teardown of all of them — the
/// infrastructure configuration, then the image recipe, then the component beneath it —
/// succeeds, each delete echoing the ARN it removed.
///
/// The rows span why the create was refused: a value one of its parameters does not admit,
/// a parameter group the request left incomplete, a reference that resolves to nothing, and
/// the dry run that reports what the real create would have done without doing it.
#[tokio::test]
async fn a_refused_pipeline_create_leaves_every_resource_it_named_deletable() {
    // (case, the refusal's status, the error class it carries)
    let rows: [(&str, u16, &str); 6] = [
        (
            "a cron expression the parser cannot read",
            400,
            "InvalidParameterValueException",
        ),
        (
            "a cron month field that is not a month",
            400,
            "InvalidParameterValueException",
        ),
        (
            "a value that is no schedule expression at all",
            400,
            "InvalidParameterValueException",
        ),
        (
            "a request naming no recipe at all",
            400,
            "MissingParametersException",
        ),
        (
            "a distribution configuration reference that resolves to nothing",
            404,
            "ResourceNotFoundException",
        ),
        ("a dry run", 412, "DryRunOperationException"),
    ];

    for (case, expected_status, expected_code) in rows {
        // Each row tears down its own store, so what the deletes below answer is decided by
        // this row's refused create alone.
        let router = emulator();
        let component = create_component(&router, "teardown-component").await;
        let (status, created) =
            create_image_recipe(&router, "teardown-recipe", "1.0.0", &component).await;
        let created = assert_ok(&format!("{case}, the image recipe"), status, created);
        let recipe = text(&format!("{case}, the image recipe"), &created, "imageRecipeArn");
        let configuration = create_infrastructure_configuration(&router, "Teardown Config").await;

        let mut body = Map::new();
        body.insert("name".to_string(), json!("teardown-pipeline"));
        body.insert("clientToken".to_string(), json!("teardown-pipeline"));
        body.insert(
            "infrastructureConfigurationArn".to_string(),
            json!(configuration.clone()),
        );
        match case {
            "a cron expression the parser cannot read" => {
                body.insert("imageRecipeArn".to_string(), json!(recipe.clone()));
                body.insert(
                    "schedule".to_string(),
                    json!({"scheduleExpression": "cron(not valid)"}),
                );
            }
            "a cron month field that is not a month" => {
                body.insert("imageRecipeArn".to_string(), json!(recipe.clone()));
                body.insert(
                    "schedule".to_string(),
                    json!({"scheduleExpression": "cron(0 10 * SMARCH ? *)"}),
                );
            }
            "a value that is no schedule expression at all" => {
                body.insert("imageRecipeArn".to_string(), json!(recipe.clone()));
                body.insert(
                    "schedule".to_string(),
                    json!({"scheduleExpression": "nightly"}),
                );
            }
            // "You must specify exactly one recipe for the pipeline": a request naming
            // neither of the two members names no recipe.
            "a request naming no recipe at all" => {}
            "a distribution configuration reference that resolves to nothing" => {
                body.insert("imageRecipeArn".to_string(), json!(recipe.clone()));
                body.insert(
                    "distributionConfigurationArn".to_string(),
                    json!(arn_of("distribution-configuration", "absent-teardown", "")),
                );
            }
            _ => {
                body.insert("imageRecipeArn".to_string(), json!(recipe.clone()));
                body.insert("dryRun".to_string(), json!(true));
            }
        }

        let (status, refused) = call(
            &router,
            "PUT",
            "/CreateImagePipeline",
            Some(&Value::Object(body)),
        )
        .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected the create to be refused with HTTP {expected_status}, got \
             {status} with {refused}"
        );
        let code = error_code(case, &refused);
        assert_eq!(
            code, expected_code,
            "case {case:?}: expected {expected_code:?}, got {code:?} with {refused}"
        );

        // The refused create minted nothing: the ARN it would have taken names no pipeline.
        let pipeline = arn_of("image-pipeline", "teardown-pipeline", "");
        let (status, missing) = call(
            &router,
            "GET",
            &format!("/GetImagePipeline?imagePipelineArn={pipeline}"),
            None,
        )
        .await;
        assert_refused(
            &format!("{case}, the pipeline it would have created"),
            status,
            &missing,
            404,
            "ResourceNotFoundException",
            &required_resource_message("ImagePipeline", &pipeline),
        );

        // So nothing holds the three resources the refused request named, and the whole
        // teardown succeeds in dependency order.
        for (member, uri, arn) in [
            (
                "infrastructureConfigurationArn",
                format!(
                    "/DeleteInfrastructureConfiguration?infrastructureConfigurationArn={configuration}"
                ),
                configuration.clone(),
            ),
            (
                "imageRecipeArn",
                format!("/DeleteImageRecipe?imageRecipeArn={recipe}"),
                recipe.clone(),
            ),
            (
                "componentBuildVersionArn",
                format!("/DeleteComponent?componentBuildVersionArn={component}"),
                component.clone(),
            ),
        ] {
            let (status, deleted) = call(&router, "DELETE", &uri, None).await;
            assert_deleted(
                &format!("{case}, the delete of {arn}"),
                status,
                &deleted,
                member,
                &arn,
            );
        }
    }
}
