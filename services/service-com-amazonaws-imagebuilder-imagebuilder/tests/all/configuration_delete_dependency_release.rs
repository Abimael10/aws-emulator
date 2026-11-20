// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for WHEN a delete of one of this service's configuration resources
//! is refused as a dependency and when it removes the record — `DeleteInfrastructureConfiguration`
//! and the sibling `DeleteDistributionConfiguration`.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is
//! what a signed AWS SDK request observes.
//!
//! The rule under test: the dependency a delete asks about is the reference the store holds
//! AT THE INSTANT of the call — "You can't delete a configuration that an image pipeline
//! still references. The request fails with ResourceDependencyException. Update or delete the
//! referencing pipelines first" (`smithy_query(query="doc",shape="DeleteInfrastructureConfiguration")`,
//! which `DeleteDistributionConfiguration` repeats). So a configuration a stored pipeline
//! names cannot be deleted while that pipeline stands; once the pipeline names another
//! configuration, names none, or is itself gone, the very same delete removes the record of
//! the calling Region, answers the ARN it removed with the id of the request it answers, and
//! every later call naming that ARN reports it missing. A delete that is REFUSED, and one
//! addressed to another Region's endpoint, remove nothing.
//!
//! The rows span what the rule generalises over: which configuration family the delete acts
//! on (the infrastructure configuration, whose reference an image pipeline always holds, and
//! the distribution configuration, whose reference is optional), how the reference was
//! released (the pipeline was pointed at another configuration, the optional reference was
//! dropped, the pipeline was deleted), why an ARN resolves to nothing (deleted, or stored in
//! another Region than the endpoint the call reached) and which of this service's two
//! missing-resource sentences each answer carries — a read of either family quoting the ARN
//! the request named, the distribution configuration's delete naming the kind its ARN
//! identifies.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region a case signs for when it only needs one; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

/// A second Region, for the rows that reach a stored configuration from another Region's
/// endpoint.
const OTHER_REGION: &str = "eu-west-1";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// A build component document: one `build` phase, which is all the component the recipe
/// under every pipeline here includes needs.
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

/// A base image in one of the forms `parentImage` admits (an AMI ID). No Amazon EC2 is
/// mounted beside this service here, so nothing answers the lookup of it and the recipe
/// create proceeds — these cases are about the configuration deletes, not about the base
/// image.
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

/// A request signed for `imagebuilder` in `region` — the signal the router routes on and the
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

/// Status and parsed body of one call; the error code is folded into the body as `__type`, so
/// one accessor reads the header and the body alike. An empty body reads as `Value::Null`.
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
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// One call in [`REGION`].
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    call_in(router, REGION, method, uri, body).await
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

/// Assert a refusal carries `expect` as its status, `code` as its error class and `message`
/// as its text.
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

/// Which member of an image pipeline holds the reference a family's configuration is reached
/// by.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reference {
    /// `infrastructureConfigurationArn`, which every pipeline holds (the model makes it
    /// required of `CreateImagePipeline` and of `UpdateImagePipeline`).
    Infrastructure,
    /// `distributionConfigurationArn`, which a pipeline may hold or not.
    Distribution,
}

/// The dependency refusal the INFRASTRUCTURE configuration carries.
fn configuration_dependency_message(arn: &str) -> String {
    format!("The resource ARN '{arn}' has dependent resources")
}

/// The dependency refusal every other resource of this service carries, the distribution
/// configuration among them.
fn resource_dependency_message(arn: &str) -> String {
    format!(
        "Resource dependency error: The resource ARN '{arn}' has other resources depended on it."
    )
}

/// The missing-resource sentence a call ADDRESSED by a configuration's own ARN carries: the
/// ARN the request named, quoted, naming no kind — both families' reads, and the
/// infrastructure configuration's delete.
fn configuration_not_found_message(arn: &str) -> String {
    format!("The resource ARN '{arn}' was not found.")
}

/// The missing-resource sentence that names the kind the ARN identifies — what the
/// DISTRIBUTION configuration's delete carries, as every report of a resource a call had to
/// resolve does.
fn required_resource_message(kind: &str, arn: &str) -> String {
    format!("The following required resource '{kind}' cannot be found: '{arn}'.")
}

/// One configuration family: how one is created, read and deleted, how a pipeline reaches it,
/// and the sentences its answers carry.
#[derive(Clone, Copy)]
struct Family {
    /// What this family is, named in the cases of every row about it.
    label: &'static str,
    /// The resource type an ARN of this family carries, as the modelled ARN patterns spell it
    /// (`smithy_query(query="shape",shape="InfrastructureConfigurationArn,DistributionConfigurationArn")`).
    resource: &'static str,
    /// The create that stores one.
    create: &'static str,
    /// The body that create carries, for a name.
    body: fn(&str) -> Value,
    /// The output member the create answers the minted ARN in.
    minted: &'static str,
    /// The read, up to the ARN it takes in its query string.
    read: &'static str,
    /// The delete under test, up to the ARN it takes in its query string.
    delete: &'static str,
    /// The output member that delete answers the removed ARN in.
    echoed: &'static str,
    /// Which pipeline member reaches a configuration of this family.
    reference: Reference,
    /// The refusal a delete earns while a stored pipeline still names it.
    dependency: fn(&str) -> String,
    /// What a READ answers for an ARN this Region holds nothing under.
    missing_on_read: fn(&str) -> String,
    /// What this DELETE answers for an ARN this Region holds nothing under.
    missing_on_delete: fn(&str) -> String,
}

/// The body a `CreateInfrastructureConfiguration` carries: the name, an idempotency token of
/// its own and the instance profile the model requires.
fn infrastructure_body(name: &str) -> Value {
    json!({
        "name": name,
        "clientToken": format!("infra-{name}"),
        "instanceProfileName": "EC2InstanceProfileForImageBuilder",
    })
}

/// The body a `CreateDistributionConfiguration` carries: one target Region with one output,
/// which is the least a distribution entry may carry.
fn distribution_body(name: &str) -> Value {
    json!({
        "name": name,
        "clientToken": format!("dist-{name}"),
        "distributions": [{ "region": REGION, "amiDistributionConfiguration": {} }],
    })
}

/// The infrastructure configuration, as the model addresses it.
const INFRASTRUCTURE: Family = Family {
    label: "the infrastructure configuration",
    resource: "infrastructure-configuration",
    create: "/CreateInfrastructureConfiguration",
    body: infrastructure_body,
    minted: "infrastructureConfigurationArn",
    read: "/GetInfrastructureConfiguration?infrastructureConfigurationArn=",
    delete: "/DeleteInfrastructureConfiguration?infrastructureConfigurationArn=",
    echoed: "infrastructureConfigurationArn",
    reference: Reference::Infrastructure,
    dependency: configuration_dependency_message,
    missing_on_read: configuration_not_found_message,
    missing_on_delete: configuration_not_found_message,
};

/// The distribution configuration, as the model addresses it.
const DISTRIBUTION: Family = Family {
    label: "the distribution configuration",
    resource: "distribution-configuration",
    create: "/CreateDistributionConfiguration",
    body: distribution_body,
    minted: "distributionConfigurationArn",
    read: "/GetDistributionConfiguration?distributionConfigurationArn=",
    delete: "/DeleteDistributionConfiguration?distributionConfigurationArn=",
    echoed: "distributionConfigurationArn",
    reference: Reference::Distribution,
    dependency: resource_dependency_message,
    missing_on_read: configuration_not_found_message,
    missing_on_delete: |arn| required_resource_message("DistributionConfiguration", arn),
};

/// The ARN a configuration of `family` named `name` carries in `region`.
fn arn_of(family: Family, region: &str, name: &str) -> String {
    format!(
        "arn:aws:imagebuilder:{region}:{ACCOUNT}:{}/{name}",
        family.resource
    )
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

/// `CreateImageRecipe` for `name` over `component`, answering the recipe's own ARN.
async fn create_image_recipe(router: &MultiServiceRouter, name: &str, component: &str) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": "1.0.0",
        "parentImage": PARENT_IMAGE,
        "clientToken": format!("recipe-{name}"),
        "components": [{ "componentArn": component }],
    });
    let case = format!("create the image recipe {name}");
    let (status, answer) = call(router, "PUT", "/CreateImageRecipe", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imageRecipeArn")
}

/// `Create<Family>Configuration` for `name`, answering the ARN it minted.
async fn create_configuration(router: &MultiServiceRouter, family: Family, name: &str) -> String {
    let body = (family.body)(name);
    let case = format!("create {} {name}", family.label);
    let (status, answer) = call(router, "PUT", family.create, Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, family.minted)
}

/// The two configuration references a pipeline is to hold: the infrastructure configuration
/// it builds in, and the distribution configuration it distributes with, if any.
///
/// `target` is the configuration of the family under test; `infrastructure` is the
/// configuration a pipeline of the DISTRIBUTION rows builds in, which no row deletes.
fn references<'a>(
    family: Family,
    target: Option<&'a str>,
    infrastructure: &'a str,
) -> (&'a str, Option<&'a str>) {
    match family.reference {
        Reference::Infrastructure => (
            target.expect("an infrastructure reference every pipeline holds"),
            None,
        ),
        Reference::Distribution => (infrastructure, target),
    }
}

/// Fold the pipeline members a create and an update share into `body`.
fn pipeline_members(
    body: &mut Map<String, Value>,
    recipe: &str,
    infrastructure: &str,
    distribution: Option<&str>,
) {
    body.insert("imageRecipeArn".to_string(), json!(recipe));
    body.insert(
        "infrastructureConfigurationArn".to_string(),
        json!(infrastructure),
    );
    if let Some(distribution) = distribution {
        body.insert(
            "distributionConfigurationArn".to_string(),
            json!(distribution),
        );
    }
}

/// `CreateImagePipeline` for `name` over `recipe` and the configurations given, answering its
/// ARN.
async fn create_pipeline(
    router: &MultiServiceRouter,
    name: &str,
    recipe: &str,
    infrastructure: &str,
    distribution: Option<&str>,
) -> String {
    let mut body = Map::new();
    body.insert("name".to_string(), json!(name));
    body.insert("clientToken".to_string(), json!(format!("create-{name}")));
    pipeline_members(&mut body, recipe, infrastructure, distribution);
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

/// `UpdateImagePipeline`, which replaces the stored pipeline — so the references it carries
/// are the ones the pipeline holds afterwards, and an optional reference it omits is dropped.
async fn update_pipeline(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    token: &str,
    recipe: &str,
    infrastructure: &str,
    distribution: Option<&str>,
) {
    let mut body = Map::new();
    body.insert("imagePipelineArn".to_string(), json!(arn));
    body.insert("clientToken".to_string(), json!(token));
    pipeline_members(&mut body, recipe, infrastructure, distribution);
    let (status, answer) = call(
        router,
        "PUT",
        "/UpdateImagePipeline",
        Some(&Value::Object(body)),
    )
    .await;
    assert_ok(case, status, answer);
}

/// Assert the delete of `arn` is refused as the dependency it is, and that the configuration
/// is still there afterwards: a refused delete removes nothing.
async fn assert_delete_refused_as_dependency(
    router: &MultiServiceRouter,
    family: Family,
    case: &str,
    arn: &str,
) {
    let (status, answer) = call(router, "DELETE", &format!("{}{arn}", family.delete), None).await;
    assert_refused(
        case,
        status,
        &answer,
        400,
        "ResourceDependencyException",
        &(family.dependency)(arn),
    );
    let (status, read) = call(router, "GET", &format!("{}{arn}", family.read), None).await;
    assert_ok(&format!("{case}: still stored afterwards"), status, read);
}

/// Assert the delete of `arn` removes it, and that every later lookup of that ARN — a read
/// and a repeat of the delete — reports it missing in its own sentence.
async fn assert_delete_removes(
    router: &MultiServiceRouter,
    family: Family,
    case: &str,
    arn: &str,
) {
    let (status, answer) = call(router, "DELETE", &format!("{}{arn}", family.delete), None).await;
    assert_deleted(case, status, &answer, family.echoed, arn);

    let (status, read) = call(router, "GET", &format!("{}{arn}", family.read), None).await;
    assert_refused(
        &format!("{case}: read back after the delete"),
        status,
        &read,
        404,
        "ResourceNotFoundException",
        &(family.missing_on_read)(arn),
    );

    let (status, again) = call(router, "DELETE", &format!("{}{arn}", family.delete), None).await;
    assert_refused(
        &format!("{case}: deleted a second time"),
        status,
        &again,
        404,
        "ResourceNotFoundException",
        &(family.missing_on_delete)(arn),
    );
}

#[tokio::test]
async fn a_configuration_is_deleted_only_once_no_stored_pipeline_names_it() {
    for family in [INFRASTRUCTURE, DISTRIBUTION] {
        let router = emulator();
        let component = create_component(&router, "release-component").await;
        let recipe = create_image_recipe(&router, "release-recipe", &component).await;

        // The infrastructure configuration a pipeline of the DISTRIBUTION rows builds in: the
        // model requires that reference of every pipeline, and no row here deletes this one.
        let pipeline_infrastructure =
            create_configuration(&router, INFRASTRUCTURE, "pipeline-infra").await;

        // Three configurations of the family under test, so the ARN each sentence below
        // carries differs between the rows and cannot be a constant a handler holds.
        let named = create_configuration(&router, family, "config-named").await;
        let unnamed = create_configuration(&router, family, "config-unnamed").await;
        let renamed_to = create_configuration(&router, family, "config-renamed-to").await;

        let (infrastructure, distribution) =
            references(family, Some(&named), &pipeline_infrastructure);
        let pipeline = create_pipeline(
            &router,
            "release-pipeline",
            &recipe,
            infrastructure,
            distribution,
        )
        .await;

        // A configuration the stored pipeline names is not removed.
        assert_delete_refused_as_dependency(
            &router,
            family,
            &format!("{}: named by a stored pipeline", family.label),
            &named,
        )
        .await;

        // One of the same family nothing names is removed by the very same call, so the
        // refusal above is about the reference rather than about the family.
        assert_delete_removes(
            &router,
            family,
            &format!("{}: named by no pipeline", family.label),
            &unnamed,
        )
        .await;

        // RELEASE 1: the pipeline is pointed at another configuration of the family, which is
        // what "update the referencing pipelines first" does — so the one it named is free and
        // the one it names now is not.
        let (infrastructure, distribution) =
            references(family, Some(&renamed_to), &pipeline_infrastructure);
        update_pipeline(
            &router,
            &format!("{}: point the pipeline at another one", family.label),
            &pipeline,
            "release-update-1",
            &recipe,
            infrastructure,
            distribution,
        )
        .await;

        assert_delete_removes(
            &router,
            family,
            &format!("{}: released by an update of the pipeline", family.label),
            &named,
        )
        .await;
        assert_delete_refused_as_dependency(
            &router,
            family,
            &format!("{}: the one the update named", family.label),
            &renamed_to,
        )
        .await;

        // RELEASE 2: for a family whose reference is OPTIONAL, an update that omits it drops
        // it; for the one every pipeline holds, only deleting the pipeline releases it. Either
        // way, what the next delete observes is the store as it stands.
        match family.reference {
            Reference::Distribution => {
                update_pipeline(
                    &router,
                    &format!("{}: drop the reference entirely", family.label),
                    &pipeline,
                    "release-update-2",
                    &recipe,
                    &pipeline_infrastructure,
                    None,
                )
                .await;
            }
            Reference::Infrastructure => {
                let case = format!("{}: delete the referencing pipeline", family.label);
                let (status, answer) = call(
                    &router,
                    "DELETE",
                    &format!("/DeleteImagePipeline?imagePipelineArn={pipeline}"),
                    None,
                )
                .await;
                assert_deleted(&case, status, &answer, "imagePipelineArn", &pipeline);
            }
        }

        assert_delete_removes(
            &router,
            family,
            &format!("{}: released by the pipeline letting it go", family.label),
            &renamed_to,
        )
        .await;
    }
}

#[tokio::test]
async fn a_delete_addressed_to_another_regions_endpoint_removes_nothing() {
    for family in [INFRASTRUCTURE, DISTRIBUTION] {
        let router = emulator();
        let stored = create_configuration(&router, family, "regional-config").await;

        // A regional endpoint reaches only its own Region's resources, so the stored
        // configuration answers the other Region's endpoint as if it did not exist — in that
        // delete's own missing-resource sentence, stating the ARN the request carried.
        let case = format!("{}: deleted through another Region's endpoint", family.label);
        let (status, answer) = call_in(
            &router,
            OTHER_REGION,
            "DELETE",
            &format!("{}{stored}", family.delete),
            None,
        )
        .await;
        assert_refused(
            &case,
            status,
            &answer,
            404,
            "ResourceNotFoundException",
            &(family.missing_on_delete)(&stored),
        );

        // An ARN of the OTHER Region, which this endpoint holds nothing under either.
        let elsewhere = arn_of(family, OTHER_REGION, "regional-config");
        let (status, answer) = call(
            &router,
            "DELETE",
            &format!("{}{elsewhere}", family.delete),
            None,
        )
        .await;
        assert_refused(
            &format!("{}: an ARN of another Region", family.label),
            status,
            &answer,
            404,
            "ResourceNotFoundException",
            &(family.missing_on_delete)(&elsewhere),
        );

        // Neither call removed the stored configuration: its own Region's endpoint still
        // reads it, and the delete there still removes it.
        let (status, read) = call(&router, "GET", &format!("{}{stored}", family.read), None).await;
        assert_ok(&format!("{case}: still stored afterwards"), status, read);

        assert_delete_removes(
            &router,
            family,
            &format!("{}: deleted through its own Region's endpoint", family.label),
            &stored,
        )
        .await;
    }
}
