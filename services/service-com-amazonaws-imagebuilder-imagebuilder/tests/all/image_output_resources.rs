// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the AMIs an Image Builder image produces, and for the two
//! controls of a build version: the retry of a build that ended, and the cancellation of one
//! that has not.
//!
//! Every case drives the real wire path (the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers), so what is asserted is what
//! a signed AWS SDK request observes. The emulator's own Amazon EC2 is mounted beside
//! imagebuilder on one engine, exactly the way the assembled emulator runs them, because an
//! AMI is EC2's resource: every AMI asserted here is one EC2 itself answers for.
//!
//! The rules under test:
//!
//! * `Image`'s `outputResources` is "the output resources that Image Builder produces for
//!   this image", whose `amis` lists "the Amazon EC2 AMIs created by this image", each entry
//!   carrying the Region of the AMI, its id, its name, its description, its state and the
//!   account that owns it (`smithy_query(query="shape",shape="Ami")`). So an AMI-type build
//!   that has reached its terminal available status reports the AMI it registered with EC2 —
//!   in the Region the build ran in, under the account that owns it — and a build that is
//!   still outstanding has produced nothing yet.
//! * `amiDistributionConfiguration.name` is "the name of the output AMI … If you don't
//!   specify a name, Image Builder names the output AMI with the image name followed by the
//!   build timestamp", and its `description`: "if you don't specify a description, the AMI in
//!   the build Region uses the image recipe's description, if the recipe has one" — so both
//!   are read from the configuration the build ran under, with those defaults.
//! * An image AWS PUBLISHES is an AMI-type image AWS serves as available, so a read of one
//!   reports the Amazon-owned AMI that backs it in the Region the read is served in; an entry
//!   EC2 publishes no image under reports none, rather than an id nothing resolves.
//! * `RetryImage` "retries a failed or canceled image build … The image must be in the FAILED
//!   or CANCELLED state", and it RESUMES that build: "the same build version returns to the
//!   test or distribution phase where it failed and continues from there. No new image build
//!   version is created". So a build that has not ended is refused HTTP 400
//!   `InvalidParameterValueException` and keeps running; a build that ended is resumed under
//!   its own ARN only when the phase it stopped in is one of the two a retry resumes from, and
//!   one that stopped in any other phase is refused with that phase named.
//! * `CancelImageCreation` "can only be used on images in a non-terminal state", and
//!   "cancellation is asynchronous: the request returns immediately, then Image Builder stops
//!   the running build and moves the image to the CANCELLED state": a build version that has
//!   not ended is accepted whichever path queued it and reports CANCELLED once its
//!   cancellation is due; one that has already ended — in any of the statuses a build ends in
//!   — is refused HTTP 400 `InvalidParameterValueException` with its own status left
//!   untouched. A reference reaching no build version of the calling Region is reported
//!   missing, and the `clientToken` makes a repeat of the request answer the original
//!   response. That the build is still the running one until its cancellation is due is
//!   asserted against the store's own clock in `src/state/tests.rs`, which needs no wall
//!   clock to observe the transient state.
//!
//! The rows span what the rules generalise over: which path queued the build (a build asked
//! for by hand, a pipeline execution), whether a distribution configuration names the output
//! AMI's name and description, whether the recipe describes itself, which kind of recipe the
//! build built, which published base image is read and in which Region, whether an EC2
//! answers at all, and which status a retried build is in.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_imagebuilder_imagebuilder::state::{
    image_build_version_arn, ComAmazonawsImagebuilderImagebuilderState, ImageBuildVersionRecord,
};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

/// The Region a case signs for when it only needs one; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

/// A second Region, for the cases that check an answer carries the Region it was served in.
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

/// The status a build something cancelled reports, and the one a build that failed reports
/// (`crate::state::IMAGE_STATUS_CANCELLED` / `IMAGE_STATUS_FAILED`) — the two a build may be
/// retried from.
const CANCELLED_STATUS: &str = "CANCELLED";

/// See [`CANCELLED_STATUS`].
const FAILED_STATUS: &str = "FAILED";

/// The error this service refuses a parameter value with
/// (`smithy_query(query="shape",shape="InvalidParameterValueException")`: a client error,
/// `@httpError` 400).
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValueException";

/// The sentence a retry of a build that has not ENDED is refused with: fixed text for this
/// class, naming the parameter the service's own validator addresses.
const NOT_RETRYABLE_MESSAGE: &str = "The value supplied for parameter 'imageArn' is not valid. \
                                     The provided image is not in a failed or cancelled state \
                                     and cannot be retried.";

/// The sentence a retry of a build that stopped in a phase no retry resumes from is refused
/// with: fixed text for this class, whose one variable part is the phase that build stopped in,
/// stated beside the phases a retry does resume from.
fn not_resumable_message(stopped_in_phase: &str) -> String {
    format!(
        "The value supplied for parameter 'imageArn' is not valid. The provided image cannot be \
         retried. Retry is not supported for images that failed during '{stopped_in_phase}' \
         phase. Supported phases: TESTING, DISTRIBUTING"
    )
}

/// The phase Image Builder runs the build of an image in, which is the phase a build of this
/// emulator is in from the instant it was queued until it is due — so the phase a cancellation
/// of a running build lands in. One of the stages of the image creation process the modelled
/// `ImageStatus` enum names (`smithy_query(query="shape",shape="ImageFailureContext")`:
/// `imageStatus` is "the status that the image had when the failure occurred").
const BUILD_PHASE: &str = "BUILDING";

/// The two phases a retry RESUMES a stopped build from: "the same build version returns to the
/// test or distribution phase where it failed and continues from there" (`RetryImage`).
const TEST_PHASE: &str = "TESTING";

/// See [`TEST_PHASE`].
const DISTRIBUTION_PHASE: &str = "DISTRIBUTING";

/// The sentence a cancellation of a build that has already ended is refused with: fixed text
/// for this class, naming the parameter the service's own validator addresses — the same one a
/// retry's own state refusal names.
const NOT_CANCELLABLE_MESSAGE: &str = "The value supplied for parameter 'imageArn' is not \
                                       valid. The provided image is in a terminal state and \
                                       cannot be cancelled.";

/// The error a request whose `clientToken` was already spent on other parameters earns
/// (`smithy_query(query="operation",operation="CancelImageCreation")`: a client error,
/// `@httpError` 400).
const IDEMPOTENT_PARAMETER_MISMATCH: &str = "IdempotentParameterMismatchException";

/// The error a reference reaching no resource of the calling Region earns.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The prefix EC2 prints an AMI id with.
const AMI_ID_PREFIX: &str = "ami-";

/// How long a case waits for a condition it polls for before giving up. Generous: the build
/// span itself is a few seconds (`crate::state::IMAGE_BUILD_SPAN`), and what a case asserts is
/// the CONDITION it polls for, never how fast the host reached it.
const CONDITION_DEADLINE: Duration = Duration::from_secs(120);

/// How long a case waits between two reads while polling for that condition.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One emulator: imagebuilder and Amazon EC2 on a single shared engine behind the production
/// router — what `build_router` assembles, narrowed to the two services under test — beside
/// the engine the build path files its records in.
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

/// One emulator with imagebuilder ALONE, so nothing answers a lookup of EC2's resources.
fn emulator_without_ec2() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_imagebuilder_imagebuilder::EmulatedService::new(engine),
    );
    router
}

/// An EC2 client over the router's own in-process callback, addressed to `region`: no socket
/// is opened and nothing leaves the process, and the endpoint below is only the address that
/// transport routes on.
fn ec2_client(router: &MultiServiceRouter, region: &str) -> ::aws_sdk_ec2::Client {
    let callback = EmbeddedEmulator::from_router(router.clone());
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
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
    let answered = ec2_client(router, REGION)
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

/// What EC2 answers for the AMI `image_id` in `region`: its name and its state, or `None`
/// when EC2 holds no image under that id.
async fn ec2_image(
    router: &MultiServiceRouter,
    region: &str,
    image_id: &str,
) -> Option<(String, String)> {
    let answered = ec2_client(router, region)
        .describe_images()
        .image_ids(image_id)
        .send()
        .await
        .expect("ec2:DescribeImages answers");
    answered.images().first().map(|image| {
        (
            image.name().unwrap_or_default().to_string(),
            image
                .state()
                .map(|state| state.as_str().to_string())
                .unwrap_or_default(),
        )
    })
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
        .rsplit(['#', '/'])
        .next()
        .unwrap_or(rendered)
        .to_string()
}

/// Assert the answer is a refusal carrying `expected` status, `code` and `message`.
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

/// `CreateImageRecipe` for `name` in `region` over `parent_image`, with `description` when it
/// has one, answering the recipe's own ARN.
async fn create_recipe(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    parent_image: &str,
    component_arn: &str,
    description: Option<&str>,
) -> String {
    let mut body = json!({
        "name": name,
        "semanticVersion": "1.0.0",
        "parentImage": parent_image,
        "clientToken": format!("recipe-{region}-{name}"),
        "components": [{ "componentArn": component_arn }],
    });
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    let case = format!("create the image recipe {name} in {region}");
    let (status, answer) = call_in(router, region, "PUT", "/CreateImageRecipe", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imageRecipeArn")
}

/// `CreateContainerRecipe` for `name` in `region`, answering the recipe's own ARN.
async fn create_container_recipe(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    component_arn: &str,
) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": "1.0.0",
        "containerType": "DOCKER",
        "parentImage": "amazonlinux:latest",
        "clientToken": format!("container-recipe-{region}-{name}"),
        "components": [{ "componentArn": component_arn }],
        "dockerfileTemplateData": DOCKERFILE_TEMPLATE,
        "targetRepository": { "service": "ECR", "repositoryName": "team-a/my-repo" },
    });
    let case = format!("create the container recipe {name} in {region}");
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

/// `CreateDistributionConfiguration` for `name` in `region`, whose one entry targets that
/// Region with `ami_name` and `ami_description` where it names them, answering its ARN.
async fn create_distribution(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    ami_name: Option<&str>,
    ami_description: Option<&str>,
) -> String {
    let mut settings = json!({});
    if let Some(ami_name) = ami_name {
        settings["name"] = json!(ami_name);
    }
    if let Some(ami_description) = ami_description {
        settings["description"] = json!(ami_description);
    }
    let body = json!({
        "name": name,
        "clientToken": format!("distribution-{region}-{name}"),
        "distributions": [{
            "region": region,
            "amiDistributionConfiguration": settings,
        }],
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

/// `CreateImage` of `recipe_arn` under `configuration_arn`, with `distribution_arn` when the
/// build has one, answering the build version ARN it queued.
async fn create_image(
    router: &MultiServiceRouter,
    region: &str,
    recipe_arn: &str,
    configuration_arn: &str,
    distribution_arn: Option<&str>,
    token: &str,
) -> String {
    let mut body = json!({
        "imageRecipeArn": recipe_arn,
        "infrastructureConfigurationArn": configuration_arn,
        "clientToken": token,
    });
    if let Some(distribution_arn) = distribution_arn {
        body["distributionConfigurationArn"] = json!(distribution_arn);
    }
    let case = format!("create the image of {recipe_arn} in {region}");
    let (status, answer) = call_in(router, region, "PUT", "/CreateImage", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imageBuildVersionArn")
}

/// `CreateImage` of a CONTAINER recipe, answering the build version ARN it queued.
async fn create_container_image(
    router: &MultiServiceRouter,
    region: &str,
    recipe_arn: &str,
    configuration_arn: &str,
    token: &str,
) -> String {
    let body = json!({
        "containerRecipeArn": recipe_arn,
        "infrastructureConfigurationArn": configuration_arn,
        "clientToken": token,
    });
    let case = format!("create the container image of {recipe_arn} in {region}");
    let (status, answer) = call_in(router, region, "PUT", "/CreateImage", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imageBuildVersionArn")
}

/// `CreateImagePipeline` for `name` in `region`, answering the pipeline's own ARN.
async fn create_pipeline(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    recipe_arn: &str,
    configuration_arn: &str,
    distribution_arn: Option<&str>,
) -> String {
    let mut body = json!({
        "name": name,
        "clientToken": format!("pipeline-{region}-{name}"),
        "imageRecipeArn": recipe_arn,
        "infrastructureConfigurationArn": configuration_arn,
    });
    if let Some(distribution_arn) = distribution_arn {
        body["distributionConfigurationArn"] = json!(distribution_arn);
    }
    let case = format!("create the image pipeline {name} in {region}");
    let (status, answer) = call_in(router, region, "PUT", "/CreateImagePipeline", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imagePipelineArn")
}

/// `StartImagePipelineExecution` of `pipeline_arn`, answering the build version ARN it queued.
async fn start_pipeline(
    router: &MultiServiceRouter,
    region: &str,
    pipeline_arn: &str,
    token: &str,
) -> String {
    let body = json!({ "imagePipelineArn": pipeline_arn, "clientToken": token });
    let case = format!("start the pipeline {pipeline_arn}");
    let (status, answer) = call_in(
        router,
        region,
        "PUT",
        "/StartImagePipelineExecution",
        Some(&body),
    )
    .await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imageBuildVersionArn")
}

/// `GetImage` of `reference` in `region`.
async fn get_image(router: &MultiServiceRouter, region: &str, reference: &str) -> (u16, Value) {
    let uri = format!(
        "/GetImage?imageBuildVersionArn={}",
        percent_encoding::utf8_percent_encode(reference, percent_encoding::NON_ALPHANUMERIC)
    );
    call_in(router, region, "GET", &uri, None).await
}

/// `CancelImageCreation` of `reference` in `region`, with `token`.
async fn cancel_image_creation(
    router: &MultiServiceRouter,
    region: &str,
    reference: &str,
    token: &str,
) -> (u16, Value) {
    let body = json!({ "imageBuildVersionArn": reference, "clientToken": token });
    call_in(router, region, "PUT", "/CancelImageCreation", Some(&body)).await
}

/// `RetryImage` of `reference` in `region`, with `token`.
async fn retry_image(
    router: &MultiServiceRouter,
    region: &str,
    reference: &str,
    token: &str,
) -> (u16, Value) {
    let body = json!({ "imageBuildVersionArn": reference, "clientToken": token });
    call_in(router, region, "PUT", "/RetryImage", Some(&body)).await
}

/// The `image` of a successful `GetImage` answer.
async fn read_image(router: &MultiServiceRouter, region: &str, reference: &str) -> Value {
    let case = format!("read the image {reference} in {region}");
    let (status, answer) = get_image(router, region, reference).await;
    let answer = assert_ok(&case, status, answer);
    answer
        .get("image")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an image member in {answer}"))
}

/// The status one image reports.
fn image_status(case: &str, image: &Value) -> String {
    image
        .get("state")
        .and_then(|state| state.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected state.status in {image}"))
        .to_string()
}

/// The `outputResources.amis` one image reports, or `None` when it reports no output
/// resources at all.
fn output_amis(image: &Value) -> Option<Vec<Value>> {
    image
        .get("outputResources")
        .map(|resources| {
            resources
                .get("amis")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
}

/// Read `reference` until it reports exactly one output AMI, and answer the image it reported.
///
/// Waits for the CONDITION rather than for a time: the build is due after a span of its own,
/// so the read is repeated until the AMI the build produced is reported or
/// [`CONDITION_DEADLINE`] passes.
async fn poll_for_output_ami(router: &MultiServiceRouter, region: &str, reference: &str) -> Value {
    let case = format!("poll {reference} for its output AMI");
    let deadline = Instant::now() + CONDITION_DEADLINE;
    loop {
        let image = read_image(router, region, reference).await;
        let amis = output_amis(&image).unwrap_or_default();
        if !amis.is_empty() {
            return image;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the build reported no output AMI before the deadline, last read \
             {image}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read `reference` until it reports `status`, and answer the image it reported.
async fn poll_for_status(
    router: &MultiServiceRouter,
    region: &str,
    reference: &str,
    status: &str,
) -> Value {
    let case = format!("poll {reference} for the status {status}");
    let deadline = Instant::now() + CONDITION_DEADLINE;
    loop {
        let image = read_image(router, region, reference).await;
        if image_status(&case, &image) == status {
            return image;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the image did not reach {status} before the deadline, last read \
             {image}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The one AMI an image reports, with its members as text.
fn single_ami(case: &str, image: &Value) -> Value {
    let amis = output_amis(image)
        .unwrap_or_else(|| panic!("case {case:?}: expected an outputResources member in {image}"));
    assert_eq!(
        amis.len(),
        1,
        "case {case:?}: expected exactly one output AMI, got {amis:?}"
    );
    amis.into_iter().next().expect("one output AMI")
}

/// Assert `ami` names an AMI of `region` that `router`'s EC2 resolves as available, named
/// the way EC2 names it.
async fn assert_backed_by_ec2(case: &str, router: &MultiServiceRouter, region: &str, ami: &Value) {
    let image_id = text(case, ami, "image");
    assert!(
        image_id.starts_with(AMI_ID_PREFIX),
        "case {case:?}: expected an AMI id, got {image_id:?}"
    );
    let reported_region = text(case, ami, "region");
    assert_eq!(
        reported_region, region,
        "case {case:?}: expected the AMI in {region}, got {reported_region:?}"
    );
    let state = ami
        .get("state")
        .and_then(|state| state.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected state.status in {ami}"));
    assert_eq!(
        state, AVAILABLE_STATUS,
        "case {case:?}: expected the AMI {AVAILABLE_STATUS}, got {state:?}"
    );
    let described = ec2_image(router, region, &image_id).await.unwrap_or_else(|| {
        panic!("case {case:?}: EC2 resolves no image under {image_id:?} in {region}")
    });
    let (ec2_name, ec2_state) = described;
    assert_eq!(
        ec2_state, "available",
        "case {case:?}: expected EC2 to report {image_id} available, got {ec2_state:?}"
    );
    let reported_name = text(case, ami, "name");
    assert_eq!(
        reported_name, ec2_name,
        "case {case:?}: expected the AMI named {ec2_name:?}, got {reported_name:?}"
    );
}

/// RULE: an AMI-type build that reached its terminal available status reports the one AMI it
/// registered with Amazon EC2 — in the Region the build ran in, named and described the way
/// its distribution configuration and its recipe say, owned by the account that built it —
/// and EC2 resolves that id as an available image.
#[tokio::test]
async fn an_available_build_reports_the_ec2_ami_it_registered() {
    let (router, _engine) = emulator_with_ec2();
    let parent_image = held_ami_id(&router).await;
    let component = create_component(&router, REGION, "greet").await;
    let configuration = create_configuration(&router, REGION, "build-infra").await;
    // (case, the image name, the recipe's description, the output AMI name the distribution
    // configuration names, the description it names, the name the AMI carries as a prefix,
    // the description it carries)
    type Row<'row> = (
        &'row str,
        &'row str,
        Option<&'row str>,
        Option<&'row str>,
        Option<&'row str>,
        &'row str,
        Option<&'row str>,
    );
    let rows: &[Row<'_>] = &[
        (
            "a build with no distribution configuration",
            "golden-plain",
            None,
            None,
            None,
            "golden-plain ",
            None,
        ),
        (
            "a build whose recipe describes itself",
            "golden-described",
            Some("the team's base image"),
            None,
            None,
            "golden-described ",
            Some("the team's base image"),
        ),
        (
            "a build whose configuration names the output AMI",
            "golden-named",
            Some("the team's base image"),
            Some("named-{{ imagebuilder:buildDate }}"),
            Some("the distributed image"),
            "named-",
            Some("the distributed image"),
        ),
        (
            "a build whose configuration names the AMI compactly",
            "golden-compact",
            None,
            Some("compact-{{imagebuilder:buildDate}}"),
            None,
            "compact-",
            None,
        ),
    ];

    for (case, image_name, recipe_description, ami_name, ami_description, name_prefix, description) in
        rows
    {
        let recipe = create_recipe(
            &router,
            REGION,
            image_name,
            &parent_image,
            &component,
            *recipe_description,
        )
        .await;
        let distribution = match (ami_name, ami_description) {
            (None, None) => None,
            _ => Some(
                create_distribution(
                    &router,
                    REGION,
                    &format!("{image_name}-distribution"),
                    *ami_name,
                    *ami_description,
                )
                .await,
            ),
        };
        let build = create_image(
            &router,
            REGION,
            &recipe,
            &configuration,
            distribution.as_deref(),
            &format!("create-{image_name}"),
        )
        .await;

        // The build is outstanding when it is queued, and a build that has produced nothing
        // yet reports the empty list rather than an AMI it has not produced.
        let queued = read_image(&router, REGION, &build).await;
        if image_status(case, &queued) == QUEUED_STATUS {
            assert_eq!(
                output_amis(&queued),
                Some(Vec::new()),
                "case {case:?}: expected no output AMI while the build is queued, got {queued}"
            );
        }

        let available = poll_for_output_ami(&router, REGION, &build).await;
        let status = image_status(case, &available);
        assert_eq!(
            status, AVAILABLE_STATUS,
            "case {case:?}: expected {AVAILABLE_STATUS} with the output AMI, got {status:?}"
        );
        let ami = single_ami(case, &available);
        assert_backed_by_ec2(case, &router, REGION, &ami).await;
        let reported_name = text(case, &ami, "name");
        assert!(
            reported_name.starts_with(name_prefix) && !reported_name.contains("{{"),
            "case {case:?}: expected an AMI name starting {name_prefix:?} with the build date \
             filled in, got {reported_name:?}"
        );
        let account = text(case, &ami, "accountId");
        assert_eq!(
            account, ACCOUNT,
            "case {case:?}: expected the AMI owned by {ACCOUNT}, got {account:?}"
        );
        let reported_description = ami.get("description").and_then(Value::as_str);
        assert_eq!(
            reported_description, *description,
            "case {case:?}: expected the description {description:?}, got \
             {reported_description:?}"
        );
    }
}

/// RULE: the AMI a build produces is reported whichever call queued the build, and a build of
/// a CONTAINER recipe produces no AMI at all.
#[tokio::test]
async fn a_pipeline_run_produces_an_ami_and_a_container_build_produces_none() {
    let (router, _engine) = emulator_with_ec2();
    let parent_image = held_ami_id(&router).await;
    let component = create_component(&router, REGION, "greet").await;
    let configuration = create_configuration(&router, REGION, "build-infra").await;
    let recipe = create_recipe(
        &router,
        REGION,
        "pipeline-image",
        &parent_image,
        &component,
        None,
    )
    .await;
    let pipeline = create_pipeline(
        &router,
        REGION,
        "nightly",
        &recipe,
        &configuration,
        None,
    )
    .await;
    let run = start_pipeline(&router, REGION, &pipeline, "start-nightly").await;
    let built = poll_for_output_ami(&router, REGION, &run).await;
    let case = "a build a pipeline execution queued";
    let ami = single_ami(case, &built);
    assert_backed_by_ec2(case, &router, REGION, &ami).await;

    let container_recipe =
        create_container_recipe(&router, REGION, "container-image", &component).await;
    let container_build = create_container_image(
        &router,
        REGION,
        &container_recipe,
        &configuration,
        "create-container-image",
    )
    .await;
    let container_case = "a build of a container recipe";
    let settled = poll_for_status(&router, REGION, &container_build, AVAILABLE_STATUS).await;
    assert_eq!(
        output_amis(&settled),
        None,
        "case {container_case:?}: expected no output resources of the AMI kind, got {settled}"
    );
}

/// RULE: a read of an image AWS publishes reports the Amazon-owned EC2 AMI that backs it in
/// the Region the read is served in; an entry EC2 publishes no image under reports none.
#[tokio::test]
async fn a_published_base_image_reports_the_amazon_owned_ami_that_backs_it() {
    let (router, _engine) = emulator_with_ec2();
    // (case, the published image's name as its ARN carries it, the Region the read is served
    // in, whether EC2 publishes an image that backs it)
    let rows: &[(&str, &str, &str, bool)] = &[
        (
            "the Amazon Linux 2 x86 base image",
            "amazon-linux-2-x86",
            REGION,
            true,
        ),
        (
            "the same base image in another Region",
            "amazon-linux-2-x86",
            OTHER_REGION,
            true,
        ),
        (
            "the Amazon Linux 2023 x86 base image",
            "amazon-linux-2023-x86",
            REGION,
            true,
        ),
        (
            "the Windows Server 2022 base image",
            "windows-server-2022-english-full-base-x86",
            REGION,
            true,
        ),
        (
            "a base image EC2 publishes no image for",
            "ubuntu-server-20-lts-x86",
            REGION,
            false,
        ),
    ];

    for (case, name, region, backed) in rows {
        let reference = format!("arn:aws:imagebuilder:{region}:aws:image/{name}/x.x.x");
        let image = read_image(&router, region, &reference).await;
        let status = image_status(case, &image);
        assert_eq!(
            status, AVAILABLE_STATUS,
            "case {case:?}: expected a published image to be {AVAILABLE_STATUS}, got {status:?}"
        );
        let amis = output_amis(&image)
            .unwrap_or_else(|| panic!("case {case:?}: expected an outputResources member in \
                                      {image}"));
        if !*backed {
            assert!(
                amis.is_empty(),
                "case {case:?}: expected no backing AMI, got {amis:?}"
            );
            continue;
        }
        let ami = single_ami(case, &image);
        assert_backed_by_ec2(case, &router, region, &ami).await;
    }
}

/// RULE: a published base image reports no backing AMI where no EC2 answers for one at all —
/// what is reported is what EC2 answered, never an id nothing resolves.
#[tokio::test]
async fn a_published_base_image_reports_no_ami_where_no_ec2_answers() {
    let router = emulator_without_ec2();
    let case = "a published base image read with no EC2 mounted";
    let reference = format!("arn:aws:imagebuilder:{REGION}:aws:image/amazon-linux-2-x86/x.x.x");
    let image = read_image(&router, REGION, &reference).await;
    assert_eq!(
        output_amis(&image),
        Some(Vec::new()),
        "case {case:?}: expected no backing AMI, got {image}"
    );
}

/// File one build version directly in the store, as a build that ended in `terminal_status`
/// after stopping in `stopped_in_phase` would have left it, and answer its ARN.
///
/// The phase a build stopped in is what decides whether a retry resumes it, and this
/// emulator's own builds only ever stop in their build phase (nothing runs the later phases
/// yet), so a case that needs a build stopped in one of them writes the record the way the
/// build path files one.
fn file_ended_build(
    engine: &Arc<engine::InMemoryEngine>,
    region: &str,
    name: &str,
    terminal_status: Option<&str>,
    stopped_in_phase: Option<&str>,
) -> String {
    let arn = image_build_version_arn(region, name, "1.0.0", 1);
    let filed = engine
        .state::<ComAmazonawsImagebuilderImagebuilderState>()
        .lock()
        .insert_image(ImageBuildVersionRecord {
            arn: arn.clone(),
            region: region.to_string(),
            name: name.to_string(),
            version: "1.0.0".to_string(),
            build_version: 1,
            platform: Some("Linux".to_string()),
            image_type: Some("AMI".to_string()),
            build_type: Some("USER_INITIATED".to_string()),
            date_created: "2026-01-01T00:00:00Z".to_string(),
            terminal_status: terminal_status.map(ToString::to_string),
            stopped_in_phase: stopped_in_phase.map(ToString::to_string),
            ..ImageBuildVersionRecord::default()
        });
    assert!(filed, "case recording {arn}: expected the store to file it");
    arn
}

/// RULE: `RetryImage` re-runs a build that stopped in a phase a retry resumes from in place,
/// under its own ARN, and refuses a build that has not ENDED at all with HTTP 400
/// `InvalidParameterValueException`, leaving that build's status untouched.
#[tokio::test]
async fn a_retry_re_runs_a_stopped_build_and_refuses_one_that_is_not_ended() {
    let (router, engine) = emulator_with_ec2();
    let parent_image = held_ami_id(&router).await;
    let component = create_component(&router, REGION, "greet").await;
    let configuration = create_configuration(&router, REGION, "build-infra").await;
    let recipe = create_recipe(
        &router,
        REGION,
        "retried-image",
        &parent_image,
        &component,
        None,
    )
    .await;

    // A build this call queued is running, so it may not be retried — and it keeps running.
    let running = create_image(
        &router,
        REGION,
        &recipe,
        &configuration,
        None,
        "create-retried-image",
    )
    .await;
    let case = "a build that is still running";
    let (status, answer) = retry_image(&router, REGION, &running, "retry-running").await;
    assert_refused(
        case,
        status,
        &answer,
        400,
        INVALID_PARAMETER_VALUE,
        NOT_RETRYABLE_MESSAGE,
    );
    let after = read_image(&router, REGION, &running).await;
    let reported = image_status(case, &after);
    assert!(
        reported == QUEUED_STATUS || reported == AVAILABLE_STATUS,
        "case {case:?}: expected the build to keep running, got {reported:?}"
    );

    // A build that has settled on its available status is not a failed or cancelled one
    // either, so it may not be retried.
    let settled_case = "a build that reached its available status";
    poll_for_status(&router, REGION, &running, AVAILABLE_STATUS).await;
    let (status, answer) = retry_image(&router, REGION, &running, "retry-available").await;
    assert_refused(
        settled_case,
        status,
        &answer,
        400,
        INVALID_PARAMETER_VALUE,
        NOT_RETRYABLE_MESSAGE,
    );

    // (case, the status the build ended in, the phase it stopped in, the name its build
    // version carries) — both statuses a build ends in, each paired with one of the two phases
    // a retry resumes from.
    let retryable: &[(&str, &str, &str, &str)] = &[
        (
            "a build that failed in the test phase",
            FAILED_STATUS,
            TEST_PHASE,
            "failed-image",
        ),
        (
            "a build that was cancelled in the distribution phase",
            CANCELLED_STATUS,
            DISTRIBUTION_PHASE,
            "cancelled-image",
        ),
    ];
    for (case, ended_in, stopped_in_phase, name) in retryable {
        let build = file_ended_build(&engine, REGION, name, Some(ended_in), Some(stopped_in_phase));
        let ended = read_image(&router, REGION, &build).await;
        let before = image_status(case, &ended);
        assert_eq!(
            before, *ended_in,
            "case {case:?}: expected the build to report {ended_in:?}, got {before:?}"
        );

        let (status, answer) = retry_image(&router, REGION, &build, &format!("retry-{name}")).await;
        let answer = assert_ok(case, status, answer);
        let retried_arn = text(case, &answer, "imageBuildVersionArn");
        assert_eq!(
            retried_arn, build,
            "case {case:?}: expected the same build version to re-run, got {retried_arn:?}"
        );
        let token = text(case, &answer, "clientToken");
        assert_eq!(
            token,
            format!("retry-{name}"),
            "case {case:?}: expected the request's own token, got {token:?}"
        );

        // "The image re-runs asynchronously in place": the build version is outstanding again
        // and produces an AMI of its own when it is due.
        let produced = poll_for_output_ami(&router, REGION, &build).await;
        let status = image_status(case, &produced);
        assert_eq!(
            status, AVAILABLE_STATUS,
            "case {case:?}: expected the re-run to reach {AVAILABLE_STATUS}, got {status:?}"
        );
        let ami = single_ami(case, &produced);
        assert_backed_by_ec2(case, &router, REGION, &ami).await;

        // A repeat of the same request answers what the first call answered.
        let (status, repeat) = retry_image(&router, REGION, &build, &format!("retry-{name}")).await;
        let repeat = assert_ok(case, status, repeat);
        assert_eq!(
            text(case, &repeat, "imageBuildVersionArn"),
            build,
            "case {case:?}: expected the repeat to answer the same build version, got {repeat}"
        );
    }
}

/// RULE: a retry whose reference reaches no build version of the calling Region is reported
/// MISSING, and a token already spent on another build version is a mismatch.
#[tokio::test]
async fn a_retry_reports_a_reference_it_cannot_reach() {
    let (router, engine) = emulator_with_ec2();
    let held = file_ended_build(
        &engine,
        REGION,
        "ended-image",
        Some(FAILED_STATUS),
        Some(TEST_PHASE),
    );
    let elsewhere = file_ended_build(
        &engine,
        OTHER_REGION,
        "other-region-image",
        Some(FAILED_STATUS),
        Some(TEST_PHASE),
    );
    let never_built = image_build_version_arn(REGION, "never-built", "9.9.9", 7);
    // (case, the reference, the Region the retry is signed for, the status, the code, the
    // token the retry carries)
    let rows: &[(&str, &str, &str, u16, &str, &str)] = &[
        (
            "a build version nothing created",
            &never_built,
            REGION,
            404,
            "ResourceNotFoundException",
            "retry-never-built",
        ),
        (
            "a build version of another Region",
            &elsewhere,
            REGION,
            404,
            "ResourceNotFoundException",
            "retry-other-region",
        ),
    ];
    for (case, reference, region, expected, code, token) in rows {
        let (status, answer) = retry_image(&router, region, reference, token).await;
        assert_refused(
            case,
            status,
            &answer,
            *expected,
            code,
            &format!("The following required resource 'Image' cannot be found: '{reference}'."),
        );
    }

    // One token, two different build versions: the second request is a mismatch.
    let token = "one-retry-token";
    let (status, answer) = retry_image(&router, REGION, &held, token).await;
    assert_ok("the first retry of a token", status, answer);
    let second = file_ended_build(
        &engine,
        REGION,
        "second-ended-image",
        Some(CANCELLED_STATUS),
        Some(DISTRIBUTION_PHASE),
    );
    let (status, answer) = retry_image(&router, REGION, &second, token).await;
    assert_eq!(
        status, 400,
        "case \"a token spent on another build version\": expected HTTP 400, got {status} with \
         {answer}"
    );
    let code = error_code("a token spent on another build version", &answer);
    assert_eq!(
        code, "IdempotentParameterMismatchException",
        "case \"a token spent on another build version\": got {code:?} with {answer}"
    );
}

/// RULE: the build timestamp an output AMI's name carries names the instant the build was
/// queued, so two builds of one image name produce two differently named AMIs.
#[tokio::test]
async fn two_builds_of_one_image_produce_differently_named_amis() {
    let (router, _engine) = emulator_with_ec2();
    let parent_image = held_ami_id(&router).await;
    let component = create_component(&router, REGION, "greet").await;
    let configuration = create_configuration(&router, REGION, "build-infra").await;
    let recipe = create_recipe(
        &router,
        REGION,
        "twice-built",
        &parent_image,
        &component,
        None,
    )
    .await;
    let case = "two builds of one image name";
    let mut names: Vec<String> = Vec::new();
    for token in ["create-twice-built-1", "create-twice-built-2"] {
        let build = create_image(&router, REGION, &recipe, &configuration, None, token).await;
        let produced = poll_for_output_ami(&router, REGION, &build).await;
        let ami = single_ami(case, &produced);
        names.push(text(case, &ami, "name"));
    }
    assert_ne!(
        names[0], names[1],
        "case {case:?}: expected two differently named output AMIs, got {names:?}"
    );
    // The instant the AMI name carries is the one the build was queued at, so both names are
    // of this run rather than of a fixed date.
    let today = ::aws_smithy_types::DateTime::from(SystemTime::now())
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("render the current instant")
        .chars()
        .take(4)
        .collect::<String>();
    for name in &names {
        assert!(
            name.contains(&today),
            "case {case:?}: expected the build timestamp of this run in {name:?}"
        );
    }
}

/// RULE: a cancellation of a build version that has NOT ended is accepted, whichever path
/// queued that build; the build is still the running one when the call returns, reports the
/// cancelled status once its cancellation is due, and is then past cancelling. A repeat of the
/// same request answers the original response.
#[tokio::test]
async fn a_cancel_ends_a_build_that_has_not_ended() {
    let (router, _engine) = emulator_with_ec2();
    let parent_image = held_ami_id(&router).await;
    let component = create_component(&router, REGION, "greet").await;
    let configuration = create_configuration(&router, REGION, "build-infra").await;
    let recipe = create_recipe(
        &router,
        REGION,
        "cancelled-build",
        &parent_image,
        &component,
        None,
    )
    .await;
    let pipeline = create_pipeline(
        &router,
        REGION,
        "cancelled-build-pipeline",
        &recipe,
        &configuration,
        None,
    )
    .await;

    // (case, whether a pipeline execution queued the build, the token that queued it, the
    // token the cancellation carries) — one row per path that queues a build: the one a caller
    // asks for by hand, and the one a pipeline execution starts.
    let rows: &[(&str, bool, &str, &str)] = &[
        (
            "a build a create queued",
            false,
            "create-cancelled-build",
            "cancel-created-build",
        ),
        (
            "a build a pipeline execution queued",
            true,
            "start-cancelled-build",
            "cancel-started-build",
        ),
    ];

    for (case, by_pipeline, queue_token, token) in rows {
        // The build is queued right before it is cancelled, so the cancellation reaches a
        // build that is still outstanding however long the row before it took.
        let build = if *by_pipeline {
            start_pipeline(&router, REGION, &pipeline, queue_token).await
        } else {
            create_image(&router, REGION, &recipe, &configuration, None, queue_token).await
        };
        let build = &build;
        let (status, answer) = cancel_image_creation(&router, REGION, build, token).await;
        let answer = assert_ok(case, status, answer);
        let cancelled_arn = text(case, &answer, "imageBuildVersionArn");
        assert_eq!(
            cancelled_arn, *build,
            "case {case:?}: expected the build version the request named, got {cancelled_arn:?}"
        );
        let echoed = text(case, &answer, "clientToken");
        assert_eq!(
            echoed, *token,
            "case {case:?}: expected the request's own token, got {echoed:?}"
        );
        let request_id = text(case, &answer, "requestId");
        assert!(
            !request_id.is_empty(),
            "case {case:?}: expected a request id in {answer}"
        );

        // "Cancellation is asynchronous: the request returns immediately, then Image Builder
        // stops the running build and moves the image to the CANCELLED state": the build
        // version reports that status once the cancellation is due, which is waited for by
        // polling for the CONDITION — never by a fixed sleep, and never by asserting how fast
        // the host got there. That the build is still the running one until that instant is
        // asserted against the store's own clock, where no wall clock can flake it
        // (`the_status_of_a_build_version_follows_the_clock_of_its_build_and_its_cancellation`
        // in `src/state/tests.rs`).
        //
        // A repeat of the request answers the original response rather than cancelling again.
        let (status, repeat) = cancel_image_creation(&router, REGION, build, token).await;
        let repeat = assert_ok(case, status, repeat);
        assert_eq!(
            repeat, answer,
            "case {case:?}: expected the original response, got {repeat}"
        );

        let cancelled = poll_for_status(&router, REGION, build, CANCELLED_STATUS).await;
        let reported = image_status(case, &cancelled);
        assert_eq!(
            reported, CANCELLED_STATUS,
            "case {case:?}: expected the build version to report {CANCELLED_STATUS}, got \
             {reported:?}"
        );

        // The build has ended now, so a cancellation under a token of its own is refused and
        // the build version keeps the status it ended in.
        let (status, answer) =
            cancel_image_creation(&router, REGION, build, &format!("{token}-again")).await;
        assert_refused(
            case,
            status,
            &answer,
            400,
            INVALID_PARAMETER_VALUE,
            NOT_CANCELLABLE_MESSAGE,
        );
        let after = read_image(&router, REGION, build).await;
        let kept = image_status(case, &after);
        assert_eq!(
            kept, CANCELLED_STATUS,
            "case {case:?}: expected the refused cancellation to change nothing, got {kept:?}"
        );
    }
}

/// RULE: a cancellation of a build version that has ALREADY ended — in any of the statuses a
/// build ends in — is refused HTTP 400 `InvalidParameterValueException`, and that build
/// version keeps the status it ended in.
#[tokio::test]
async fn a_cancel_refuses_a_build_that_has_already_ended() {
    let (router, engine) = emulator_with_ec2();
    let parent_image = held_ami_id(&router).await;
    let component = create_component(&router, REGION, "greet").await;
    let configuration = create_configuration(&router, REGION, "build-infra").await;
    let recipe = create_recipe(
        &router,
        REGION,
        "settled-build",
        &parent_image,
        &component,
        None,
    )
    .await;

    // A build the clock has settled on its available status has ended, and the other two
    // statuses a build ends in are ones something ended it in.
    let available = create_image(
        &router,
        REGION,
        &recipe,
        &configuration,
        None,
        "create-settled-build",
    )
    .await;
    poll_for_status(&router, REGION, &available, AVAILABLE_STATUS).await;
    let failed = file_ended_build(
        &engine,
        REGION,
        "failed-build",
        Some(FAILED_STATUS),
        Some(BUILD_PHASE),
    );
    let cancelled = file_ended_build(
        &engine,
        REGION,
        "ended-cancelled-build",
        Some(CANCELLED_STATUS),
        Some(BUILD_PHASE),
    );

    // (case, the build version, the status it ended in) — every status the modelled
    // `ImageState`'s own documentation names a build as ending in: "ends in the AVAILABLE,
    // FAILED, or CANCELLED state" (`crate::state::TERMINAL_IMAGE_STATUSES`).
    let rows: &[(&str, &str, &str)] = &[
        (
            "a build the clock settled as available",
            &available,
            AVAILABLE_STATUS,
        ),
        ("a build that failed", &failed, FAILED_STATUS),
        (
            "a build something already cancelled",
            &cancelled,
            CANCELLED_STATUS,
        ),
    ];
    for (case, build, ended_in) in rows {
        let (status, answer) =
            cancel_image_creation(&router, REGION, build, &format!("cancel-{ended_in}")).await;
        assert_refused(
            case,
            status,
            &answer,
            400,
            INVALID_PARAMETER_VALUE,
            NOT_CANCELLABLE_MESSAGE,
        );
        let after = read_image(&router, REGION, build).await;
        let reported = image_status(case, &after);
        assert_eq!(
            reported, *ended_in,
            "case {case:?}: expected the build version to keep reporting {ended_in:?}, got \
             {reported:?}"
        );
    }
}

/// RULE: a cancellation whose reference reaches no build version of the calling Region is
/// reported MISSING, naming the kind of resource the ARN identifies and the ARN the request
/// carried; and a `clientToken` already spent on another build version is a mismatch that
/// cancels nothing.
#[tokio::test]
async fn a_cancel_reports_a_reference_it_cannot_reach() {
    let (router, engine) = emulator_with_ec2();
    let parent_image = held_ami_id(&router).await;
    let component = create_component(&router, REGION, "greet").await;
    let configuration = create_configuration(&router, REGION, "build-infra").await;
    let recipe = create_recipe(
        &router,
        REGION,
        "unreachable-build",
        &parent_image,
        &component,
        None,
    )
    .await;

    let elsewhere = file_ended_build(&engine, OTHER_REGION, "other-region-build", None, None);
    let never_built = image_build_version_arn(REGION, "never-built", "9.9.9", 7);
    // An image of Amazon's published catalog: an owner this store holds no build of, which the
    // modelled `ImageBuildVersionArn` admits as the account field.
    let published = format!("arn:aws:imagebuilder:{REGION}:aws:image/amazon-linux-2-x86/2023.0.1/1");
    // (case, the reference)
    let rows: &[(&str, &str)] = &[
        ("a build version nothing created", &never_built),
        ("a build version of another Region", &elsewhere),
        ("a build version of another owner", &published),
    ];
    for (case, reference) in rows {
        let (status, answer) = cancel_image_creation(
            &router,
            REGION,
            reference,
            &format!("cancel-unreachable-{}", reference.len()),
        )
        .await;
        assert_refused(
            case,
            status,
            &answer,
            404,
            RESOURCE_NOT_FOUND,
            &format!("The following required resource 'Image' cannot be found: '{reference}'."),
        );
    }

    // One token, two different build versions: the second request is a mismatch, and the build
    // version it named is not cancelled.
    let token = "one-cancel-token";
    let first = create_image(
        &router,
        REGION,
        &recipe,
        &configuration,
        None,
        "create-unreachable-first",
    )
    .await;
    let (status, answer) = cancel_image_creation(&router, REGION, &first, token).await;
    assert_ok("the first cancellation of a token", status, answer);

    let second = create_image(
        &router,
        REGION,
        &recipe,
        &configuration,
        None,
        "create-unreachable-second",
    )
    .await;
    let case = "a token spent on another build version";
    let (status, answer) = cancel_image_creation(&router, REGION, &second, token).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {answer}"
    );
    let code = error_code(case, &answer);
    assert_eq!(
        code, IDEMPOTENT_PARAMETER_MISMATCH,
        "case {case:?}: expected {IDEMPOTENT_PARAMETER_MISMATCH}, got {code:?} with {answer}"
    );
    let after = read_image(&router, REGION, &second).await;
    let reported = image_status(case, &after);
    assert_ne!(
        reported, CANCELLED_STATUS,
        "case {case:?}: expected the refused request to cancel nothing, got {reported:?}"
    );
}

/// RULE: a cancellation stops the build in the phase it was running, so a build this service
/// stopped while it was being BUILT is a dead end: no retry resumes it, the refusal names the
/// phase it stopped in, and the build version keeps the status the cancellation left it in and
/// produces nothing.
#[tokio::test]
async fn a_cancelled_build_is_a_dead_end_no_retry_resumes() {
    let (router, _engine) = emulator_with_ec2();
    let parent_image = held_ami_id(&router).await;
    let component = create_component(&router, REGION, "greet").await;
    let configuration = create_configuration(&router, REGION, "build-infra").await;
    let recipe = create_recipe(
        &router,
        REGION,
        "cancelled-then-retried",
        &parent_image,
        &component,
        None,
    )
    .await;
    // One row per path that queues a build: the phase a cancellation lands in is the phase the
    // build was running, whichever call queued it.
    let pipeline = create_pipeline(
        &router,
        REGION,
        "cancelled-then-retried-pipeline",
        &recipe,
        &configuration,
        None,
    )
    .await;
    // (case, whether a pipeline execution queued the build, the token that queued it, the
    // token the cancellation carries, the token the retry carries)
    let rows: &[(&str, bool, &str, &str, &str)] = &[
        (
            "a build a create queued, cancelled while it was being built",
            false,
            "create-cancelled-then-retried",
            "cancel-created-then-retry",
            "retry-after-cancel-created",
        ),
        (
            "a build a pipeline execution queued, cancelled while it was being built",
            true,
            "start-cancelled-then-retried",
            "cancel-started-then-retry",
            "retry-after-cancel-started",
        ),
    ];

    for (case, by_pipeline, queue_token, cancel_token, retry_token) in rows {
        let build = if *by_pipeline {
            start_pipeline(&router, REGION, &pipeline, queue_token).await
        } else {
            create_image(&router, REGION, &recipe, &configuration, None, queue_token).await
        };
        let (status, answer) = cancel_image_creation(&router, REGION, &build, cancel_token).await;
        assert_ok(case, status, answer);

        // The build version reports the cancelled status once the cancellation is due, which is
        // waited for by polling for the CONDITION rather than for a time.
        let cancelled = poll_for_status(&router, REGION, &build, CANCELLED_STATUS).await;
        let reported = image_status(case, &cancelled);
        assert_eq!(
            reported, CANCELLED_STATUS,
            "case {case:?}: expected {CANCELLED_STATUS} before the retry, got {reported:?}"
        );

        // The build stopped in the phase Image Builder was building it in, which is not a phase
        // a retry resumes from, so the retry is refused and names that phase.
        let (status, answer) = retry_image(&router, REGION, &build, retry_token).await;
        assert_refused(
            case,
            status,
            &answer,
            400,
            INVALID_PARAMETER_VALUE,
            &not_resumable_message(BUILD_PHASE),
        );

        // The refused retry wrote nothing: the build version is still the cancelled one, and a
        // build that never ran again produced no output AMI.
        let after = read_image(&router, REGION, &build).await;
        let kept = image_status(case, &after);
        assert_eq!(
            kept, CANCELLED_STATUS,
            "case {case:?}: expected the refused retry to change nothing, got {kept:?}"
        );
        let amis = output_amis(&after).unwrap_or_default();
        assert!(
            amis.is_empty(),
            "case {case:?}: expected the cancelled build to produce no AMI, got {amis:?}"
        );
    }
}

/// RULE: a retry RESUMES a stopped build from the phase that build stopped in, so it is
/// accepted only where that phase is one of the two a retry resumes from — the test and the
/// distribution phase — and a build that stopped in any other phase is refused HTTP 400
/// `InvalidParameterValueException` naming the phase it stopped in beside the phases a retry
/// supports, with its own status left untouched.
#[tokio::test]
async fn a_retry_resumes_only_a_build_that_stopped_in_a_resumable_phase() {
    let (router, engine) = emulator_with_ec2();
    // The phases are the stages of the image creation process the modelled `ImageStatus` enum
    // names (`smithy_query(query="shape",shape="ImageStatus")`: PENDING, CREATING, BUILDING,
    // TESTING, DISTRIBUTING, INTEGRATING, before the statuses a build ENDS in), which is the
    // enum `ImageFailureContext`'s `imageStatus` reports a stopped build's stage from.
    //
    // (the phase the build stopped in, whether a retry resumes from it)
    let phases: &[(&str, bool)] = &[
        ("PENDING", false),
        ("CREATING", false),
        (BUILD_PHASE, false),
        (TEST_PHASE, true),
        (DISTRIBUTION_PHASE, true),
        ("INTEGRATING", false),
    ];
    // Both statuses a stopped build reports: the question is the PHASE, so a build that failed
    // and one that was cancelled in the same phase are answered alike.
    for (phase, resumable) in phases {
        for ended_in in [FAILED_STATUS, CANCELLED_STATUS] {
            let case = format!("a build that stopped in {phase} and reports {ended_in}");
            let name = format!(
                "stopped-in-{}-{}",
                phase.to_ascii_lowercase(),
                ended_in.to_ascii_lowercase()
            );
            let build = file_ended_build(&engine, REGION, &name, Some(ended_in), Some(phase));
            let (status, answer) =
                retry_image(&router, REGION, &build, &format!("retry-{name}")).await;
            if *resumable {
                let answer = assert_ok(&case, status, answer);
                let retried = text(&case, &answer, "imageBuildVersionArn");
                assert_eq!(
                    retried, build,
                    "case {case:?}: expected the same build version to re-run, got {retried:?}"
                );
                // "The image re-runs asynchronously in place": the build version is outstanding
                // again and produces an AMI of its own once it is due.
                let produced = poll_for_output_ami(&router, REGION, &build).await;
                let settled = image_status(&case, &produced);
                assert_eq!(
                    settled, AVAILABLE_STATUS,
                    "case {case:?}: expected the re-run to reach {AVAILABLE_STATUS}, got \
                     {settled:?}"
                );
                let ami = single_ami(&case, &produced);
                assert_backed_by_ec2(&case, &router, REGION, &ami).await;
            } else {
                assert_refused(
                    &case,
                    status,
                    &answer,
                    400,
                    INVALID_PARAMETER_VALUE,
                    &not_resumable_message(phase),
                );
                let after = read_image(&router, REGION, &build).await;
                let kept = image_status(&case, &after);
                assert_eq!(
                    kept, ended_in,
                    "case {case:?}: expected the refused retry to change nothing, got {kept:?}"
                );
                let amis = output_amis(&after).unwrap_or_default();
                assert!(
                    amis.is_empty(),
                    "case {case:?}: expected the stopped build to produce no AMI, got {amis:?}"
                );
            }
        }
    }
}
