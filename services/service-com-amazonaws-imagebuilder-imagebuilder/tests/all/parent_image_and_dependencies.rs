// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the base image a recipe builds from, and for the dependency
//! that keeps a referenced recipe from being deleted.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is
//! what a signed AWS SDK request observes. Where the base image has to be RESOLVED, the
//! emulator's own Amazon EC2 is mounted beside imagebuilder on one engine, exactly the way
//! the assembled emulator runs them, so an AMI the account's image catalog holds is the AMI
//! Image Builder resolves.
//!
//! The rules under test:
//!
//! * `parentImage` carries one of the identifier forms its own documentation lists — an "AMI
//!   ID", an "Image Builder image Amazon Resource Name (ARN)", an "Amazon Web Services
//!   Systems Manager (SSM) Parameter Store Parameter, prefixed by `ssm:`, followed by the
//!   parameter name or ARN", or an "Amazon Web Services Marketplace product ID" — and a
//!   value in none of them is refused HTTP 400 `InvalidParameterValueException` naming that
//!   member, with nothing stored: a value written as an ARN is told the form a base image
//!   identifier has to follow, and one that is no identifier at all is told which kinds of
//!   identifier a base image may be named by. The refusal is a fact about the VALUE, so it
//!   is answered before the request is matched against what the store already holds.
//! * "If you enter an AMI ID or an SSM parameter that contains the AMI ID, you must have
//!   access to the AMI. The AMI must also be in the Region where you're creating the recipe"
//!   — an AMI is EC2's resource, so a create naming an AMI id resolves it against EC2 before
//!   writing anything: an id the account holds an image under is accepted, and one it holds
//!   none under is refused HTTP 404 `ResourceNotFoundException` naming the `Image` resource
//!   and the identifier, with nothing stored. A refusal is GROUNDED in EC2's answer: where
//!   no EC2 answers at all, nothing about the image is established and the create is not
//!   refused.
//! * a delete of a resource another stored resource still REFERENCES is refused HTTP 400
//!   `ResourceDependencyException` naming the ARN the request carried, and removes nothing:
//!   an image pipeline holds the recipe reference each of its runs builds, and a recipe
//!   holds the component references it applies.
//!
//! The rows span what the rules generalise over: which identifier form the value carries,
//! whether the image it names exists, and which form the standing reference takes — an exact
//! version ARN or one whose trailing version nodes are `x` wildcards.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

/// The Region every call here is signed for; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// The error this service refuses a parameter value with
/// (`smithy_query(query="shape",shape="InvalidParameterValueException")`: a client error,
/// `@httpError` 400).
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValueException";

/// The error this service reports a resource a request references and it cannot find with
/// (`smithy_query(query="shape",shape="ResourceNotFoundException")`: a client error,
/// `@httpError` 404).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The error this service refuses a delete blocked by a standing reference with
/// (`smithy_query(query="shape",shape="ResourceDependencyException")`: a client error,
/// `@httpError` 400).
const RESOURCE_DEPENDENCY: &str = "ResourceDependencyException";

/// The sentence a `parentImage` written as an ARN that is no image ARN of this service is
/// refused with: the identifier's KIND is one the service reads, so what it is told is that
/// its form is not one a base image reference admits.
const UNSUPPORTED_IDENTIFIER_FORMAT: &str = "The value supplied for parameter 'parentImage' is \
                                             not valid. The supplied image identifier is not \
                                             in a supported format.";

/// The sentence a `parentImage` that is no identifier of any kind is refused with: nothing
/// about the value says what it meant to be, so it is told which kinds a base image may be
/// named by.
const UNSUPPORTED_PARENT_IMAGE_KIND: &str = "The value supplied for parameter 'parentImage' is \
                                             not valid. The provided parent image must be \
                                             either an EC2 AMI Id, SSM Parameter, Marketplace \
                                             Product Id, or a valid Image Builder Image Arn.";

/// A build component document: one `build` phase, which is all a recipe these cases name
/// needs.
const BUILD_DOCUMENT: &str = "name: HelloWorld\nschemaVersion: 1.0\nphases:\n  - name: build\n    \
                              steps:\n      - name: Greet\n        action: ExecuteBash\n        \
                              inputs:\n          commands:\n            - echo hello\n";

/// One emulator with imagebuilder ALONE, so nothing answers a lookup of another service's
/// resource.
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
/// router — what `build_router` assembles, narrowed to the two services under test.
fn emulator_with_ec2() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_imagebuilder_imagebuilder::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// An EC2 client over the router's own in-process callback: no socket is opened and nothing
/// leaves the process, and the endpoint below is only the address that transport routes on.
fn ec2_client(router: &MultiServiceRouter) -> ::aws_sdk_ec2::Client {
    let callback = EmbeddedEmulator::from_router(router.clone());
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .region(::aws_sdk_ec2::config::Region::new(REGION.to_string()))
        .endpoint_url(format!("https://ec2.{REGION}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_ec2::Client::from_conf(config)
}

/// One AMI id the account's own image catalog holds, read from EC2 rather than written here:
/// the published images a fresh account already has are the ones it can build from.
async fn held_ami_id(router: &MultiServiceRouter) -> String {
    let answered = ec2_client(router)
        .describe_images()
        .owners("amazon")
        .send()
        .await
        .expect("ec2:DescribeImages answers");
    answered
        .images()
        .iter()
        .filter_map(|image| image.image_id().map(str::to_string))
        .next()
        .expect("the account's own image catalog holds at least one image")
}

/// A request signed for `imagebuilder` in [`REGION`].
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

/// Status and parsed body of one imagebuilder call. An empty body reads as `Value::Null`.
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

/// The error code a refusal carries, from `__type`.
fn error_code(case: &str, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an error shape, got {body}"))
        .rsplit(['#', '/'])
        .next()
        .expect("a non-empty error type")
        .to_string()
}

/// The text one member of a response carries.
fn text(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected text in {name} of {body}"))
        .to_string()
}

/// Assert the answer is a refusal carrying `status`, `code` and `message`.
fn assert_refused(case: &str, status: u16, body: &Value, expected: u16, code: &str, message: &str) {
    assert_eq!(
        status, expected,
        "case {case:?}: expected HTTP {expected}, got {status} with {body}"
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

/// Assert a call succeeded, and answer its body.
fn assert_ok(case: &str, status: u16, body: Value) -> Value {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {body}"
    );
    body
}

/// A `CreateImageRecipe` body for `name` at `version`, building from `parent_image`.
fn recipe_body(name: &str, version: &str, token: &str, parent_image: &str) -> Value {
    json!({
        "name": name,
        "semanticVersion": version,
        "parentImage": parent_image,
        "clientToken": token,
    })
}

/// `CreateImageRecipe` with `body`.
async fn create_recipe(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "PUT", "/CreateImageRecipe", Some(body)).await
}

/// `GetImageRecipe` for `arn`.
async fn get_recipe(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/GetImageRecipe?imageRecipeArn={arn}"),
        None,
    )
    .await
}

/// `DeleteImageRecipe` for `arn`.
async fn delete_recipe(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    call(
        router,
        "DELETE",
        &format!("/DeleteImageRecipe?imageRecipeArn={arn}"),
        None,
    )
    .await
}

/// `DeleteComponent` for `arn`.
async fn delete_component(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    call(
        router,
        "DELETE",
        &format!("/DeleteComponent?componentBuildVersionArn={arn}"),
        None,
    )
    .await
}

/// The ARN an image recipe of `name` at `version` is stored under.
fn recipe_arn(name: &str, version: &str) -> String {
    format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:image-recipe/{name}/{version}")
}

/// The sentence a delete blocked by a standing reference to `arn` is refused with.
fn dependency_message(arn: &str) -> String {
    format!("Resource dependency error: The resource ARN '{arn}' has other resources depended on it.")
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

/// `CreateImagePipeline` for `name`, holding `recipe_reference`, answering its ARN.
async fn create_pipeline(
    router: &MultiServiceRouter,
    name: &str,
    recipe_reference: &str,
    configuration_arn: &str,
) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("pipeline-{name}"),
        "imageRecipeArn": recipe_reference,
        "infrastructureConfigurationArn": configuration_arn,
        "status": "DISABLED",
    });
    let case = format!("create the image pipeline {name}");
    let (status, answer) = call(router, "PUT", "/CreateImagePipeline", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imagePipelineArn")
}

/// RULE: a `parentImage` is one of the identifier forms the member documents, and a value in
/// none of them is refused as that parameter's value with nothing stored — told which kinds
/// a base image may be when it was written as an ARN, and that it could not be read when it
/// is no identifier at all.
///
/// The rows differ along which form the value carries — and, for the refused half, along how
/// the value misses every form: a bare word, a prefix with no identifier after it, a
/// non-hexadecimal AMI id, an ARN of another vendor, and an ARN of this service naming a
/// resource that is no image.
#[tokio::test]
async fn only_the_documented_identifier_forms_name_a_parent_image() {
    // imagebuilder alone: nothing resolves an AMI id, so each row is placed by the FORM of
    // the value and by nothing else.
    let router = emulator();

    // (case, the recipe name, the value, the sentence it is refused with — none when it
    // names a base image)
    let rows: &[(&str, &str, String, Option<&str>)] = &[
        (
            "an AMI id",
            "ami-id-recipe",
            "ami-0123456789abcdef0".to_string(),
            None,
        ),
        (
            "a short-form AMI id",
            "short-ami-id-recipe",
            "ami-1a2b3c4d".to_string(),
            None,
        ),
        (
            "a managed image's wildcard version ARN",
            "managed-image-recipe",
            format!("arn:aws:imagebuilder:{REGION}:aws:image/amazon-linux-2-x86/x.x.x"),
            None,
        ),
        (
            "an image build version ARN of this account",
            "own-image-recipe",
            format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:image/my-image/1.0.0/1"),
            None,
        ),
        (
            "an SSM parameter name",
            "ssm-parameter-recipe",
            "ssm:/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64"
                .to_string(),
            None,
        ),
        (
            "a Marketplace product id",
            "marketplace-recipe",
            "prod-7y6s3l5i4rrgo".to_string(),
            None,
        ),
        (
            "a value in none of the documented forms",
            "bare-word-recipe",
            "not-an-image-identifier".to_string(),
            Some(UNSUPPORTED_PARENT_IMAGE_KIND),
        ),
        (
            "the AMI prefix with no identifier after it",
            "empty-ami-id-recipe",
            "ami-".to_string(),
            Some(UNSUPPORTED_PARENT_IMAGE_KIND),
        ),
        (
            "an AMI id carrying a non-hexadecimal node",
            "non-hex-ami-id-recipe",
            "ami-zzzzzzzzzzzzzzzzz".to_string(),
            Some(UNSUPPORTED_PARENT_IMAGE_KIND),
        ),
        (
            "the SSM prefix with no parameter after it",
            "empty-ssm-recipe",
            "ssm:".to_string(),
            Some(UNSUPPORTED_PARENT_IMAGE_KIND),
        ),
        (
            "an image ARN of another vendor",
            "foreign-image-recipe",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:image/ami-0123456789abcdef0"),
            Some(UNSUPPORTED_IDENTIFIER_FORMAT),
        ),
        (
            "an ARN of this service naming a resource that is no image",
            "recipe-arn-recipe",
            recipe_arn("another-recipe", "1.0.0"),
            Some(UNSUPPORTED_IDENTIFIER_FORMAT),
        ),
    ];

    for (row, (case, name, parent_image, refusal)) in rows.iter().enumerate() {
        let body = recipe_body(name, "1.0.0", &format!("prtok-{row:04}"), parent_image);
        let (status, answered) = create_recipe(&router, &body).await;
        let arn = recipe_arn(name, "1.0.0");
        if let Some(message) = refusal {
            assert_refused(
                case,
                status,
                &answered,
                400,
                INVALID_PARAMETER_VALUE,
                message,
            );
            // The refused create stored nothing: the ARN it would have minted resolves to
            // no recipe.
            let (status, read) = get_recipe(&router, &arn).await;
            assert_eq!(
                status, 404,
                "case {case:?}: the refused create must store nothing, got {status} with {read}"
            );
        } else {
            let answered = assert_ok(case, status, answered);
            let minted = text(case, &answered, "imageRecipeArn");
            assert_eq!(
                minted, arn,
                "case {case:?}: expected the create to mint {arn:?}, got {minted:?}"
            );
            // Round trip: the stored recipe reports the base image it was created with.
            let (status, read) = get_recipe(&router, &arn).await;
            let read = assert_ok(case, status, read);
            let reported = read["imageRecipe"]["parentImage"]
                .as_str()
                .unwrap_or_default();
            assert_eq!(
                reported, parent_image,
                "case {case:?}: expected the stored parent image {parent_image:?}, got \
                 {reported:?}"
            );
        }
    }
}

/// RULE: an AMI id named as the base image is resolved against EC2 before anything is
/// stored — an id the account holds an image under is accepted, one it holds none under is
/// reported missing, and an identifier of another form is not asked about at all.
#[tokio::test]
async fn an_ami_id_is_resolved_against_ec2_before_the_recipe_is_stored() {
    let router = emulator_with_ec2();
    let held = held_ami_id(&router).await;

    // (case, the recipe name, the value, whether the image it names exists)
    let rows: Vec<(&str, &str, String, bool)> = vec![
        (
            "an AMI id the account's catalog holds",
            "held-ami-recipe",
            held.clone(),
            true,
        ),
        (
            "an AMI id the account holds no image under",
            "absent-ami-recipe",
            "ami-0123456789abcdef0".to_string(),
            false,
        ),
        (
            "another AMI id the account holds no image under",
            "second-absent-ami-recipe",
            "ami-00000000000000000".to_string(),
            false,
        ),
        (
            "a short-form AMI id the account holds no image under",
            "short-absent-ami-recipe",
            "ami-1a2b3c4d".to_string(),
            false,
        ),
        (
            // EC2 owns AMI ids and nothing else here, so a managed image ARN is a form it
            // is never asked about: nothing about it is established and the create stands.
            "a managed image ARN, which EC2 is not asked about",
            "managed-ami-recipe",
            format!("arn:aws:imagebuilder:{REGION}:aws:image/amazon-linux-2-x86/x.x.x"),
            true,
        ),
    ];

    for (row, (case, name, parent_image, exists)) in rows.iter().enumerate() {
        let body = recipe_body(name, "1.0.0", &format!("amitok-{row:04}"), parent_image);
        let (status, answered) = create_recipe(&router, &body).await;
        let arn = recipe_arn(name, "1.0.0");
        if *exists {
            let answered = assert_ok(case, status, answered);
            let minted = text(case, &answered, "imageRecipeArn");
            assert_eq!(
                minted, arn,
                "case {case:?}: expected the create to mint {arn:?}, got {minted:?}"
            );
            // Round trip: the stored recipe reports the base image it resolved.
            let (status, read) = get_recipe(&router, &arn).await;
            let read = assert_ok(case, status, read);
            let reported = read["imageRecipe"]["parentImage"]
                .as_str()
                .unwrap_or_default();
            assert_eq!(
                reported, parent_image,
                "case {case:?}: expected the stored parent image {parent_image:?}, got \
                 {reported:?}"
            );
        } else {
            assert_refused(
                case,
                status,
                &answered,
                404,
                RESOURCE_NOT_FOUND,
                &format!(
                    "The following required resource 'Image' cannot be found: '{parent_image}'."
                ),
            );
            let (status, read) = get_recipe(&router, &arn).await;
            assert_eq!(
                status, 404,
                "case {case:?}: the refused create must store nothing, got {status} with {read}"
            );
        }
    }
}

/// RULE: the base image is a fact about the request's own value, so it is examined before
/// the request is matched against what the store already holds — a create whose name and
/// version are taken is still refused for its `parentImage`, and the stored recipe is left
/// exactly as it was.
#[tokio::test]
async fn the_parent_image_is_examined_before_the_name_collision() {
    let router = emulator();
    let name = "collision-recipe";
    let arn = recipe_arn(name, "1.0.0");
    let stored_parent_image = "ami-0123456789abcdef0";

    let created = create_recipe(
        &router,
        &recipe_body(name, "1.0.0", "collide-0001", stored_parent_image),
    )
    .await;
    assert_ok("the recipe that holds the name", created.0, created.1);

    // (case, the value the second create carries, the refusal it earns)
    let rows: &[(&str, &str, u16, &str, &str)] = &[
        (
            "a second create carrying a value in no documented form",
            "not-an-image-identifier",
            400,
            INVALID_PARAMETER_VALUE,
            UNSUPPORTED_PARENT_IMAGE_KIND,
        ),
        (
            "a second create carrying a base image this service can read",
            "ami-0123456789abcdef1",
            400,
            "ResourceAlreadyExistsException",
            &format!("The following resource 'ImageRecipe' already exists: '{arn}'."),
        ),
    ];

    for (row, (case, parent_image, status, code, message)) in rows.iter().enumerate() {
        let body = recipe_body(name, "1.0.0", &format!("collide-{row:04}-retry"), parent_image);
        let (answered_status, answered) = create_recipe(&router, &body).await;
        assert_refused(case, answered_status, &answered, *status, code, message);

        // The stored recipe is untouched: it still reports the base image its own create
        // carried.
        let (status, read) = get_recipe(&router, &arn).await;
        let read = assert_ok(case, status, read);
        let reported = read["imageRecipe"]["parentImage"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(
            reported, stored_parent_image,
            "case {case:?}: expected the stored parent image {stored_parent_image:?}, got \
             {reported:?}"
        );
    }
}

/// RULE: while another stored resource still references it, a delete removes nothing and is
/// refused with the dependency error naming the ARN the request carried — and a resource
/// nothing references deletes.
///
/// The rows differ along the FORM the standing reference takes: an exact version ARN, and
/// one whose trailing version nodes are the `x` wildcards a reference may carry, each of
/// which reaches the recipe it is refused for.
#[tokio::test]
async fn a_resource_another_resource_still_references_is_not_deleted() {
    let router = emulator();
    let configuration = create_configuration(&router, "dependency-infra").await;

    // (case, the recipe name, how the pipeline names the recipe)
    let rows: &[(&str, &str, &str)] = &[
        ("an exact version reference", "exact-reference-recipe", "1.0.0"),
        ("a patch wildcard reference", "patch-wildcard-recipe", "1.0.x"),
        ("a minor wildcard reference", "minor-wildcard-recipe", "1.x.x"),
        ("a full wildcard reference", "full-wildcard-recipe", "x.x.x"),
    ];

    for (row, (case, name, reference_version)) in rows.iter().enumerate() {
        let component = create_component(&router, &format!("{name}-component")).await;
        let arn = recipe_arn(name, "1.0.0");
        let created = create_recipe(
            &router,
            &json!({
                "name": name,
                "semanticVersion": "1.0.0",
                "parentImage": "ami-0123456789abcdef0",
                "clientToken": format!("deptok-{row:04}"),
                "components": [{ "componentArn": component.clone() }],
            }),
        )
        .await;
        let created = assert_ok(case, created.0, created.1);
        let minted = text(case, &created, "imageRecipeArn");
        assert_eq!(
            minted, arn,
            "case {case:?}: expected the create to mint {arn:?}, got {minted:?}"
        );

        let reference = recipe_arn(name, reference_version);
        create_pipeline(&router, &format!("{name}-pipeline"), &reference, &configuration).await;

        // The pipeline stands, so the recipe it builds is in use.
        let (status, answered) = delete_recipe(&router, &arn).await;
        assert_refused(
            case,
            status,
            &answered,
            400,
            RESOURCE_DEPENDENCY,
            &dependency_message(&arn),
        );
        let (status, read) = get_recipe(&router, &arn).await;
        assert_ok(case, status, read);

        // And the recipe stands, so the component it applies is in use — the same rule over
        // the other reference this service holds.
        let (status, answered) = delete_component(&router, &component).await;
        assert_refused(
            case,
            status,
            &answered,
            400,
            RESOURCE_DEPENDENCY,
            &dependency_message(&component),
        );
    }

    // A recipe no pipeline names is deleted, and is then gone: the refusals above are the
    // standing reference, not a delete that never works.
    let case = "a recipe nothing references";
    let unreferenced = "unreferenced-recipe";
    let arn = recipe_arn(unreferenced, "1.0.0");
    let created = create_recipe(
        &router,
        &recipe_body(unreferenced, "1.0.0", "deptok-free", "ami-0123456789abcdef0"),
    )
    .await;
    assert_ok(case, created.0, created.1);
    let (status, answered) = delete_recipe(&router, &arn).await;
    let answered = assert_ok(case, status, answered);
    let removed = text(case, &answered, "imageRecipeArn");
    assert_eq!(
        removed, arn,
        "case {case:?}: expected the delete to answer {arn:?}, got {removed:?}"
    );
    let (status, read) = get_recipe(&router, &arn).await;
    assert_eq!(
        status, 404,
        "case {case:?}: the deleted recipe must be gone, got {status} with {read}"
    );
}
