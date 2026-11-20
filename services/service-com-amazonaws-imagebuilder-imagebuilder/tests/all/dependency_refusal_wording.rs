// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the WORDING of this service's dependency and already-exists
//! refusals, and for the delete path of the infrastructure configuration that carries one of
//! them.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what
//! a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! * a delete of a resource another stored resource still references is refused
//!   `ResourceDependencyException` (HTTP 400) whose message is the FIXED template of that
//!   resource's family with exactly ONE variable part, the ARN the request named: it quotes
//!   that ARN and nothing else, so it never names the resource that holds the reference;
//! * a refused delete leaves the store as it was, so the resource is still read back;
//! * once nothing references it, the delete removes it and echoes the ARN it removed, after
//!   which both a read and a repeat of the delete report that ARN missing in its family's own
//!   not-found template;
//! * a create whose key this Region already holds is refused `ResourceAlreadyExistsException`
//!   (HTTP 400) whose message is the FIXED already-exists template of that resource's family:
//!   where the family's template states an ARN, its only variable part is the ARN of the
//!   STORED resource that already holds the key — never a name, a version or any other value
//!   the request carried, and never a second ARN.
//!
//! The rows span what the rules generalise over: which kind of resource the call acts on, in
//! which kind of resource the reference that protects it lives, and which of the message
//! families the kind belongs to — and each row carries its own name, so the ARN the message
//! quotes differs from row to row and cannot be a constant.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every case signs for; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

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

/// A base image in one of the forms `parentImage` admits (an AMI id). No Amazon EC2 is
/// mounted beside this service here, so nothing answers the lookup of it and the create
/// proceeds — which is the point: these cases are about the delete, not about the base image.
const PARENT_IMAGE: &str = "ami-0123456789abcdef0";

/// The ARN prefix every resource of this service in [`REGION`] shares; a message is searched
/// for it to prove it quotes no second ARN.
const ARN_PREFIX: &str = "arn:aws:imagebuilder:";

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
            bytes::Bytes::from(serde_json::to_vec(value).expect("a serializable test body"))
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

/// The dependency refusal the components, recipes, pipelines, images and workflows carry, and
/// the distribution configuration with them.
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

/// The already-exists refusal the components, recipes, pipelines and the distribution
/// configuration carry: the LABEL of the kind the create would have minted and the ARN of the
/// resource that already holds the key, and nothing else.
fn already_exists_message(label: &str, arn: &str) -> String {
    format!("The following resource '{label}' already exists: '{arn}'.")
}

/// The already-exists refusal the INFRASTRUCTURE configuration carries: the one kind of this
/// service whose sentence is entirely fixed text, stating no ARN at all.
fn configuration_already_exists_message() -> String {
    "The resource already exists.".to_string()
}

/// The not-found report this service's authoring and build resources carry, naming the kind
/// the ARN identifies.
fn required_resource_message(resource_type: &str, arn: &str) -> String {
    format!("The following required resource '{resource_type}' cannot be found: '{arn}'.")
}

/// The not-found report a call ADDRESSED by a configuration's own ARN carries, quoting the ARN
/// the request named: both families' reads word a missing configuration this way, and so does
/// the infrastructure configuration's delete.
fn configuration_not_found_message(arn: &str) -> String {
    format!("The resource ARN '{arn}' was not found.")
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

/// `CreateImageRecipe` for `name` over `component`, answering its ARN.
async fn create_image_recipe(
    router: &MultiServiceRouter,
    name: &str,
    component: &str,
) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": "1.0.0",
        "parentImage": PARENT_IMAGE,
        "clientToken": format!("recipe-{name}"),
        "components": [{ "componentArn": component }],
    });
    let case = format!("create the image recipe {name}");
    let (status, answered) = call(router, "PUT", "/CreateImageRecipe", Some(&body)).await;
    let answered = assert_ok(&case, status, answered);
    text(&case, &answered, "imageRecipeArn")
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

/// `CreateImagePipeline` for `name`, DISABLED so nothing it reaches is building, over the
/// infrastructure configuration and whatever `extra` names beside it.
async fn create_disabled_pipeline(
    router: &MultiServiceRouter,
    name: &str,
    configuration: &str,
    extra: Value,
) -> String {
    let mut body = Map::new();
    body.insert("name".to_string(), json!(name));
    body.insert("clientToken".to_string(), json!(format!("pipeline-{name}")));
    body.insert("status".to_string(), json!("DISABLED"));
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

/// Assert `message` is the family's template with the ARN the request named as its only
/// variable part: it quotes that ARN, and no second ARN of this service — notably not
/// `referrer`, the ARN of the resource whose reference earned the refusal.
fn assert_quotes_only(case: &str, message: &str, arn: &str, referrer: &str) {
    let quoted = message.matches(&format!("'{arn}'")).count();
    assert_eq!(
        quoted, 1,
        "case {case:?}: expected the message to quote {arn:?} exactly once, got {quoted} in \
         {message:?}"
    );
    let arns = message.matches(ARN_PREFIX).count();
    assert_eq!(
        arns, 1,
        "case {case:?}: expected the message to carry exactly one ARN, got {arns} in \
         {message:?}"
    );
    assert!(
        !message.contains(referrer),
        "case {case:?}: expected the message not to name the referring resource {referrer:?}, \
         got {message:?}"
    );
}

/// Assert an already-exists `message` quotes exactly two parts — the kind's label and `arn`,
/// the ARN of the resource that already holds the key — so beside what that stored ARN itself
/// spells, no value the refused request carried and no second ARN can be in it.
fn assert_quotes_the_label_and_the_stored_arn(case: &str, message: &str, arn: &str) {
    let quoted = message.matches(&format!("'{arn}'")).count();
    assert_eq!(
        quoted, 1,
        "case {case:?}: expected the message to quote {arn:?} exactly once, got {quoted} in \
         {message:?}"
    );
    let parts = message.matches('\'').count();
    assert_eq!(
        parts, 4,
        "case {case:?}: expected the message to quote exactly two parts, the label and the ARN, \
         got {} in {message:?}",
        parts / 2
    );
    let arns = message.matches(ARN_PREFIX).count();
    assert_eq!(
        arns, 1,
        "case {case:?}: expected the message to carry exactly one ARN, got {arns} in \
         {message:?}"
    );
}

/// One row: a create whose key this Region already holds, and what its refusal has to say.
struct Collision {
    /// What this row is, named in every assertion it makes.
    case: &'static str,
    /// The create under test, called a second time with a client token of its own.
    uri: &'static str,
    /// The body that second create carries: the same key, a different token.
    body: Value,
    /// The ARN the first create minted, read from its own answer.
    existing: String,
    /// The whole sentence the second create's refusal has to carry.
    message: String,
    /// Whether this family's template states the stored ARN at all.
    quotes_arn: bool,
    /// The read that proves the ARN the refusal states is the stored resource's own, and the
    /// member it wraps that resource in.
    read: String,
    wrapper: &'static str,
}

#[tokio::test]
async fn an_already_exists_refusal_states_only_the_stored_resources_arn() {
    // The rows differ along the KIND the create would have minted — and so along the label
    // the refusal gives it, which is the kind the stored ARN names except for the pipeline,
    // whose recorded refusal labels it `Pipeline` rather than the `image-pipeline` its ARN
    // spells (see `crate::state`'s already-exists wording) — along which part of the key is
    // already taken (a name, a name and a semantic version, a build version's content), and
    // along which of the two already-exists families the kind belongs to. Each row names its
    // own resources, so the ARN the message states differs from row to row.
    for row_case in [
        "a component build version whose content is already stored",
        "an image recipe name and version that is already taken",
        "a container recipe name and version that is already taken",
        "a pipeline name that is already taken",
        "a distribution configuration name that is already taken",
        "an infrastructure configuration name that is already taken",
    ] {
        let router = emulator();
        let row = match row_case {
            "a component build version whose content is already stored" => {
                let component = create_component(&router, "collide-component").await;
                Collision {
                    case: row_case,
                    uri: "/CreateComponent",
                    body: json!({
                        "name": "collide-component",
                        "semanticVersion": "1.0.0",
                        "platform": "Linux",
                        "clientToken": "collide-component-again",
                        "data": BUILD_DOCUMENT,
                    }),
                    message: already_exists_message("Component", &component),
                    quotes_arn: true,
                    read: format!("/GetComponent?componentBuildVersionArn={component}"),
                    wrapper: "component",
                    existing: component,
                }
            }
            "an image recipe name and version that is already taken" => {
                let component = create_component(&router, "collide-recipe-component").await;
                let recipe = create_image_recipe(&router, "collide-recipe", &component).await;
                Collision {
                    case: row_case,
                    uri: "/CreateImageRecipe",
                    body: json!({
                        "name": "collide-recipe",
                        "semanticVersion": "1.0.0",
                        "parentImage": PARENT_IMAGE,
                        "clientToken": "collide-recipe-again",
                        "components": [{ "componentArn": component }],
                    }),
                    message: already_exists_message("ImageRecipe", &recipe),
                    quotes_arn: true,
                    read: format!("/GetImageRecipe?imageRecipeArn={recipe}"),
                    wrapper: "imageRecipe",
                    existing: recipe,
                }
            }
            "a container recipe name and version that is already taken" => {
                let component = create_component(&router, "collide-boxed-component").await;
                let recipe =
                    create_container_recipe(&router, "collide-boxed", &component).await;
                Collision {
                    case: row_case,
                    uri: "/CreateContainerRecipe",
                    body: json!({
                        "name": "collide-boxed",
                        "semanticVersion": "1.0.0",
                        "containerType": "DOCKER",
                        "parentImage": "amazonlinux:latest",
                        "clientToken": "collide-boxed-again",
                        "dockerfileTemplateData": CONTAINER_TEMPLATE,
                        "targetRepository": {
                            "service": "ECR",
                            "repositoryName": "team/images",
                        },
                        "components": [{ "componentArn": component }],
                    }),
                    message: already_exists_message("ContainerRecipe", &recipe),
                    quotes_arn: true,
                    read: format!("/GetContainerRecipe?containerRecipeArn={recipe}"),
                    wrapper: "containerRecipe",
                    existing: recipe,
                }
            }
            "a pipeline name that is already taken" => {
                let component = create_component(&router, "collide-pipe-component").await;
                let recipe = create_image_recipe(&router, "collide-pipe-recipe", &component).await;
                let configuration =
                    create_infrastructure_configuration(&router, "Collide Pipe Infra").await;
                let pipeline = create_disabled_pipeline(
                    &router,
                    "collide-pipeline",
                    &configuration,
                    json!({ "imageRecipeArn": recipe.clone() }),
                )
                .await;
                Collision {
                    case: row_case,
                    uri: "/CreateImagePipeline",
                    body: json!({
                        "name": "collide-pipeline",
                        "clientToken": "collide-pipeline-again",
                        "status": "DISABLED",
                        "infrastructureConfigurationArn": configuration,
                        "imageRecipeArn": recipe,
                    }),
                    message: already_exists_message("Pipeline", &pipeline),
                    quotes_arn: true,
                    read: format!("/GetImagePipeline?imagePipelineArn={pipeline}"),
                    wrapper: "imagePipeline",
                    existing: pipeline,
                }
            }
            "a distribution configuration name that is already taken" => {
                let distribution =
                    create_distribution_configuration(&router, "Collide Dist").await;
                Collision {
                    case: row_case,
                    uri: "/CreateDistributionConfiguration",
                    body: json!({
                        "name": "Collide Dist",
                        "clientToken": "collide-dist-again",
                        "distributions": [{
                            "region": REGION,
                            "amiDistributionConfiguration": {},
                        }],
                    }),
                    message: already_exists_message(
                        "DistributionConfiguration",
                        &distribution,
                    ),
                    quotes_arn: true,
                    read: format!(
                        "/GetDistributionConfiguration?distributionConfigurationArn={distribution}"
                    ),
                    wrapper: "distributionConfiguration",
                    existing: distribution,
                }
            }
            "an infrastructure configuration name that is already taken" => {
                let configuration =
                    create_infrastructure_configuration(&router, "Collide Infra").await;
                Collision {
                    case: row_case,
                    uri: "/CreateInfrastructureConfiguration",
                    body: json!({
                        "name": "Collide Infra",
                        "clientToken": "collide-infra-again",
                        "instanceProfileName": "EC2InstanceProfileForImageBuilder",
                    }),
                    message: configuration_already_exists_message(),
                    quotes_arn: false,
                    read: format!(
                        "/GetInfrastructureConfiguration?\
                         infrastructureConfigurationArn={configuration}"
                    ),
                    wrapper: "infrastructureConfiguration",
                    existing: configuration,
                }
            }
            // Every row above names itself, so an unlisted label is a mistake in the table
            // rather than a case to fall back on.
            other => unreachable!("no row builds the case {other:?}"),
        };

        // The second create is refused in its family's own already-exists template.
        let (status, refused) = call(&router, "PUT", row.uri, Some(&row.body)).await;
        assert_refused(
            row.case,
            status,
            &refused,
            400,
            "ResourceAlreadyExistsException",
            &row.message,
        );
        let message = text(row.case, &refused, "message");
        if row.quotes_arn {
            assert_quotes_the_label_and_the_stored_arn(row.case, &message, &row.existing);
        } else {
            assert!(
                !message.contains(ARN_PREFIX) && !message.contains('\''),
                "case {:?}: expected the message to state no ARN and quote nothing, got \
                 {message:?}",
                row.case
            );
        }

        // The ARN the refusal states is the STORED resource's own, which its read reports,
        // and the refused create registered nothing of its own.
        let (status, stored) = call(&router, "GET", &row.read, None).await;
        let stored = assert_ok(row.case, status, stored);
        let reported = stored.get(row.wrapper).unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the read to report the {} it holds, got {stored}",
                row.case, row.wrapper
            )
        });
        let reported_arn = text(row.case, reported, "arn");
        assert_eq!(
            reported_arn, row.existing,
            "case {:?}: expected the stored resource to be {:?}, got {reported_arn:?}",
            row.case, row.existing
        );
    }
}

/// One row: a resource a delete is refused for, the resource whose reference refuses it, and
/// the two sentences the refused resource's family carries.
struct Row {
    /// What this row is, named in every assertion it makes.
    case: &'static str,
    /// The ARN the delete under test names.
    arn: String,
    /// The ARN of the resource that holds the reference — deleted first to clear the way.
    referrer: String,
    /// The operation and the member the delete under test names its resource by.
    delete: &'static str,
    /// The operation and the member a read names it by.
    read: &'static str,
    /// The output member the delete echoes the removed ARN in.
    echoed: &'static str,
    /// The operation and member that deletes the referrer.
    delete_referrer: &'static str,
    /// The dependency refusal the delete earns while the reference stands.
    dependency: String,
    /// The report a READ answers once it is gone.
    missing: String,
    /// The report a REPEAT DELETE answers once it is gone: the same sentence as the read's,
    /// because the template follows the resource's kind rather than the operation.
    missing_on_delete: String,
}

#[tokio::test]
async fn a_dependency_refusal_quotes_only_the_arn_the_request_asked_to_delete() {
    // The rows differ in the kind of resource the delete acts on, in the kind of resource
    // that holds the reference protecting it, in which of the two message families the
    // refused kind belongs to, and — because each row names its own resources — in the ARN
    // the message has to quote.
    for row_case in [
        "a component an image recipe includes",
        "a component a container recipe includes",
        "an image recipe a pipeline builds",
        "a container recipe a pipeline builds",
        "an infrastructure configuration a pipeline builds in",
        "a distribution configuration a pipeline distributes with",
    ] {
        let router = emulator();
        let row = match row_case {
            "a component an image recipe includes" => {
                let component = create_component(&router, "wording-plain-component").await;
                let recipe =
                    create_image_recipe(&router, "wording-plain-recipe", &component).await;
                Row {
                    case: row_case,
                    arn: component.clone(),
                    referrer: recipe,
                    delete: "/DeleteComponent?componentBuildVersionArn=",
                    read: "/GetComponent?componentBuildVersionArn=",
                    echoed: "componentBuildVersionArn",
                    delete_referrer: "/DeleteImageRecipe?imageRecipeArn=",
                    dependency: dependency_message(&component),
                    missing: required_resource_message("Component", &component),
                    missing_on_delete: required_resource_message("Component", &component),
                }
            }
            "a component a container recipe includes" => {
                let component = create_component(&router, "wording-boxed-component").await;
                let recipe =
                    create_container_recipe(&router, "wording-boxed-recipe", &component).await;
                Row {
                    case: row_case,
                    arn: component.clone(),
                    referrer: recipe,
                    delete: "/DeleteComponent?componentBuildVersionArn=",
                    read: "/GetComponent?componentBuildVersionArn=",
                    echoed: "componentBuildVersionArn",
                    delete_referrer: "/DeleteContainerRecipe?containerRecipeArn=",
                    dependency: dependency_message(&component),
                    missing: required_resource_message("Component", &component),
                    missing_on_delete: required_resource_message("Component", &component),
                }
            }
            "an image recipe a pipeline builds" => {
                let component = create_component(&router, "wording-built-component").await;
                let recipe =
                    create_image_recipe(&router, "wording-built-recipe", &component).await;
                let configuration =
                    create_infrastructure_configuration(&router, "Wording Built Infra").await;
                let pipeline = create_disabled_pipeline(
                    &router,
                    "wording-built-pipeline",
                    &configuration,
                    json!({ "imageRecipeArn": recipe.clone() }),
                )
                .await;
                Row {
                    case: row_case,
                    arn: recipe.clone(),
                    referrer: pipeline,
                    delete: "/DeleteImageRecipe?imageRecipeArn=",
                    read: "/GetImageRecipe?imageRecipeArn=",
                    echoed: "imageRecipeArn",
                    delete_referrer: "/DeleteImagePipeline?imagePipelineArn=",
                    dependency: dependency_message(&recipe),
                    missing: required_resource_message("ImageRecipe", &recipe),
                    missing_on_delete: required_resource_message("ImageRecipe", &recipe),
                }
            }
            "a container recipe a pipeline builds" => {
                let component = create_component(&router, "wording-crate-component").await;
                let recipe =
                    create_container_recipe(&router, "wording-crate-recipe", &component).await;
                let configuration =
                    create_infrastructure_configuration(&router, "Wording Crate Infra").await;
                let pipeline = create_disabled_pipeline(
                    &router,
                    "wording-crate-pipeline",
                    &configuration,
                    json!({ "containerRecipeArn": recipe.clone() }),
                )
                .await;
                Row {
                    case: row_case,
                    arn: recipe.clone(),
                    referrer: pipeline,
                    delete: "/DeleteContainerRecipe?containerRecipeArn=",
                    read: "/GetContainerRecipe?containerRecipeArn=",
                    echoed: "containerRecipeArn",
                    delete_referrer: "/DeleteImagePipeline?imagePipelineArn=",
                    dependency: dependency_message(&recipe),
                    missing: required_resource_message("ContainerRecipe", &recipe),
                    missing_on_delete: required_resource_message("ContainerRecipe", &recipe),
                }
            }
            "an infrastructure configuration a pipeline builds in" => {
                let component = create_component(&router, "wording-infra-component").await;
                let recipe =
                    create_image_recipe(&router, "wording-infra-recipe", &component).await;
                let configuration =
                    create_infrastructure_configuration(&router, "Wording Infra").await;
                let pipeline = create_disabled_pipeline(
                    &router,
                    "wording-infra-pipeline",
                    &configuration,
                    json!({ "imageRecipeArn": recipe }),
                )
                .await;
                Row {
                    case: row_case,
                    arn: configuration.clone(),
                    referrer: pipeline,
                    delete: "/DeleteInfrastructureConfiguration?infrastructureConfigurationArn=",
                    read: "/GetInfrastructureConfiguration?infrastructureConfigurationArn=",
                    echoed: "infrastructureConfigurationArn",
                    delete_referrer: "/DeleteImagePipeline?imagePipelineArn=",
                    dependency: configuration_dependency_message(&configuration),
                    missing: configuration_not_found_message(&configuration),
                    missing_on_delete: configuration_not_found_message(&configuration),
                }
            }
            _ => {
                let component = create_component(&router, "wording-dist-component").await;
                let recipe = create_image_recipe(&router, "wording-dist-recipe", &component).await;
                let configuration =
                    create_infrastructure_configuration(&router, "Wording Dist Infra").await;
                let distribution =
                    create_distribution_configuration(&router, "Wording Dist").await;
                let pipeline = create_disabled_pipeline(
                    &router,
                    "wording-dist-pipeline",
                    &configuration,
                    json!({
                        "imageRecipeArn": recipe,
                        "distributionConfigurationArn": distribution.clone(),
                    }),
                )
                .await;
                Row {
                    case: row_case,
                    arn: distribution.clone(),
                    referrer: pipeline,
                    delete: "/DeleteDistributionConfiguration?distributionConfigurationArn=",
                    read: "/GetDistributionConfiguration?distributionConfigurationArn=",
                    echoed: "distributionConfigurationArn",
                    delete_referrer: "/DeleteImagePipeline?imagePipelineArn=",
                    dependency: dependency_message(&distribution),
                    missing: configuration_not_found_message(&distribution),
                    missing_on_delete: required_resource_message(
                        "DistributionConfiguration",
                        &distribution,
                    ),
                }
            }
        };

        // While the reference stands the delete is refused, in the family's own template,
        // quoting the requested ARN and nothing else.
        let (status, refused) =
            call(&router, "DELETE", &format!("{}{}", row.delete, row.arn), None).await;
        assert_refused(
            row.case,
            status,
            &refused,
            400,
            "ResourceDependencyException",
            &row.dependency,
        );
        let message = text(row.case, &refused, "message");
        assert_quotes_only(row.case, &message, &row.arn, &row.referrer);

        // The refused delete left the store as it was.
        let (status, still_there) =
            call(&router, "GET", &format!("{}{}", row.read, row.arn), None).await;
        assert_ok(
            &format!("{}, read after the refusal", row.case),
            status,
            still_there,
        );

        // Once the referring resource is gone the delete removes it and echoes the ARN.
        let (status, cleared) = call(
            &router,
            "DELETE",
            &format!("{}{}", row.delete_referrer, row.referrer),
            None,
        )
        .await;
        assert_ok(
            &format!("{}, delete the referring resource", row.case),
            status,
            cleared,
        );
        let (status, deleted) =
            call(&router, "DELETE", &format!("{}{}", row.delete, row.arn), None).await;
        let deleted = assert_ok(row.case, status, deleted);
        let echoed = text(row.case, &deleted, row.echoed);
        assert_eq!(
            echoed, row.arn,
            "case {:?}: expected the delete to name {:?} in {}, got {echoed:?}",
            row.case, row.arn, row.echoed
        );
        let request_id = text(row.case, &deleted, "requestId");
        assert!(
            is_request_id(&request_id),
            "case {:?}: expected the delete to name the request it answers, got {request_id:?}",
            row.case
        );

        // And both a read and a repeat of the delete report that ARN missing, each in the
        // sentence its own path carries.
        for (what, method, uri, expected) in [
            ("read after the delete", "GET", &row.read, &row.missing),
            ("deleted twice", "DELETE", &row.delete, &row.missing_on_delete),
        ] {
            let (status, missing) =
                call(&router, method, &format!("{}{}", uri, row.arn), None).await;
            assert_refused(
                &format!("{}, {what}", row.case),
                status,
                &missing,
                404,
                "ResourceNotFoundException",
                expected,
            );
        }
    }
}

#[tokio::test]
async fn an_infrastructure_configuration_delete_reports_the_arn_the_request_named() {
    // Two configurations, so the ARN every sentence carries differs between the rows and
    // cannot be a constant the handler holds.
    for (case, name) in [
        ("a configuration named in words", "Wording Teardown Infra"),
        ("a configuration named in one token", "teardown_infra_2"),
    ] {
        let router = emulator();
        let configuration = create_infrastructure_configuration(&router, name).await;
        let component = create_component(&router, "teardown-order-component").await;
        let recipe = create_image_recipe(&router, "teardown-order-recipe", &component).await;
        let pipeline = create_disabled_pipeline(
            &router,
            "teardown-order-pipeline",
            &configuration,
            json!({ "imageRecipeArn": recipe.clone() }),
        )
        .await;

        let delete = format!(
            "/DeleteInfrastructureConfiguration?infrastructureConfigurationArn={configuration}"
        );
        let read = format!(
            "/GetInfrastructureConfiguration?infrastructureConfigurationArn={configuration}"
        );

        let (status, refused) = call(&router, "DELETE", &delete, None).await;
        assert_refused(
            case,
            status,
            &refused,
            400,
            "ResourceDependencyException",
            &configuration_dependency_message(&configuration),
        );
        let message = text(case, &refused, "message");
        assert_quotes_only(case, &message, &configuration, &pipeline);

        let (status, still_there) = call(&router, "GET", &read, None).await;
        let still_there =
            assert_ok(&format!("{case}, read after the refusal"), status, still_there);
        // The refused delete had no effect: the read still reports the configuration, under
        // the ARN the request named.
        let reported = still_there
            .get("infrastructureConfiguration")
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the read to report the configuration, got {still_there}")
            })
            .clone();
        let reported_arn = text(case, &reported, "arn");
        assert_eq!(
            reported_arn, configuration,
            "case {case:?}: expected the read to report {configuration:?}, got {reported_arn:?}"
        );

        // Teardown in dependency order: the pipeline, the recipe it builds, then the
        // configuration it built in.
        for (what, uri) in [
            (
                "the pipeline",
                format!("/DeleteImagePipeline?imagePipelineArn={pipeline}"),
            ),
            (
                "the recipe",
                format!("/DeleteImageRecipe?imageRecipeArn={recipe}"),
            ),
        ] {
            let (status, cleared) = call(&router, "DELETE", &uri, None).await;
            assert_ok(&format!("{case}, delete {what}"), status, cleared);
        }

        let (status, deleted) = call(&router, "DELETE", &delete, None).await;
        let deleted = assert_ok(case, status, deleted);
        let echoed = text(case, &deleted, "infrastructureConfigurationArn");
        assert_eq!(
            echoed, configuration,
            "case {case:?}: expected the delete to name {configuration:?}, got {echoed:?}"
        );

        for (what, method, uri) in [
            ("read after the delete", "GET", &read),
            ("deleted twice", "DELETE", &delete),
        ] {
            let (status, missing) = call(&router, method, uri, None).await;
            assert_refused(
                &format!("{case}, {what}"),
                status,
                &missing,
                404,
                "ResourceNotFoundException",
                &configuration_not_found_message(&configuration),
            );
        }
    }
}
