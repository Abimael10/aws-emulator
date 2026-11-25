// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `UpdateImagePipeline`.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is
//! what a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! * an accepted update REPLACES the stored pipeline: "UpdateImagePipeline does not support
//!   selective updates. The request replaces the pipeline's entire configuration, so include
//!   every setting that you want to keep. Any optional property that you omit is removed or
//!   reset to its default" — so every setting the request carried is what the following read
//!   reports, as submitted; every optional one it omitted comes back cleared; and the ones
//!   the service documents a value for come back carrying that value (`status` "Defaults to
//!   ENABLED when omitted", `enhancedImageMetadataEnabled` "Defaults to true", the test
//!   configuration Image Builder "stores a default configuration" for);
//! * the three members an update request cannot carry are what the pipeline keeps: the name
//!   its ARN was derived from, the resource's own tags, and the instant it was created —
//!   while `dateUpdated` reports the instant of the last WRITE, which is the create's own
//!   until an update replaces the record;
//! * "You must specify exactly one recipe for your image, using either a containerRecipeArn
//!   or an imageRecipeArn. The recipe must be the same type, image or container, as the
//!   pipeline's current recipe", and the platform the pipeline reports is "inherited from
//!   the recipe that the pipeline uses", so a replacement naming another recipe reports
//!   THAT recipe's platform;
//! * every resource the replacement references has to be a stored resource of the calling
//!   Region, so a reference naming nothing — the pipeline itself included — is reported with
//!   the not-found error this service models (`ResourceNotFoundException`, HTTP 404);
//! * `clientToken` is an idempotency token of this operation's own: a retry answers the
//!   original response without writing again, and the same token carrying different settings
//!   is the modelled mismatch.
//!
//! The rows below span what each rule generalises over: which member a replacement carries,
//! how many entries its lists and maps carry, which members it omits, which of the two
//! recipe kinds the pipeline builds, and how a reference comes to name nothing.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region a case signs for; the ARNs it creates carry it.
const REGION: &str = "us-west-2";

/// A Region no case creates anything in, used to show an update reaches only its own
/// Region's pipelines.
const OTHER_REGION: &str = "eu-west-1";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// A build component document: one `build` phase, which is all a recipe these cases
/// reference needs in order to report a platform.
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

/// A workflow document declaring one step, for the row that names a workflow.
const WORKFLOW_DOCUMENT: &str = concat!(
    "name: BuildImage\n",
    "schemaVersion: 1.0\n",
    "steps:\n",
    "  - name: LaunchBuildInstance\n",
    "    action: LaunchInstance\n",
    "    onFailure: Abort\n",
);

/// A Dockerfile template, for the container recipe the recipe-kind cases reference.
const DOCKERFILE_TEMPLATE: &str = "FROM {{{ imagebuilder:parentImage }}}\nRUN echo hello\n";

/// The two values of the modelled `PipelineStatus` enum, and the one an omitted `status`
/// means ("Defaults to ENABLED when omitted").
const ENABLED: &str = "ENABLED";

/// See [`ENABLED`].
const DISABLED: &str = "DISABLED";

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

/// A request signed for `imagebuilder` in `region` — the signal the router routes on and
/// the handlers read the Region from.
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
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// Percent-encode one path segment, as an AWS SDK encodes a NON-greedy `@httpLabel`, so
/// the `:` and `/` of an ARN travel as `%3A` and `%2F` and the whole ARN stays ONE segment.
fn encode_label(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
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

/// The `message` of an error answer.
fn error_message(case: &str, body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message member in {body}"))
        .to_string()
}

/// Assert the answer is a refusal carrying `status`, `code` and `message`.
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
    let actual_message = error_message(case, body);
    assert_eq!(
        actual_message, message,
        "case {case:?}: expected the message {message:?}, got {actual_message:?}"
    );
}

/// How this service reports a resource the calling Region holds nothing under.
fn missing_message(resource_type: &str, arn: &str) -> String {
    format!("The following required resource '{resource_type}' cannot be found: '{arn}'.")
}

/// `CreateComponent` for one build version of `name` on `platform`, answering its build
/// version ARN.
async fn create_component(router: &MultiServiceRouter, name: &str, platform: &str) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": "1.0.0",
        "platform": platform,
        "clientToken": format!("component-{name}"),
        "data": BUILD_DOCUMENT,
    });
    let case = format!("create the component {name}");
    let (status, answer) = call_in(router, REGION, "PUT", "/CreateComponent", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "componentBuildVersionArn")
}

/// `CreateImageRecipe` for `name` at `version` including `component_arn`, answering the
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
        "clientToken": format!("recipe-{name}-{version}"),
        "components": [{ "componentArn": component_arn }],
    });
    let case = format!("create the image recipe {name} {version}");
    let (status, answer) = call_in(router, REGION, "PUT", "/CreateImageRecipe", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "imageRecipeArn")
}

/// `CreateContainerRecipe` for `name`, answering the recipe's own ARN.
async fn create_container_recipe(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": "1.0.0",
        "containerType": "DOCKER",
        "parentImage": "amazonlinux:latest",
        "clientToken": format!("container-recipe-{name}"),
        "dockerfileTemplateData": DOCKERFILE_TEMPLATE,
        "targetRepository": { "service": "ECR", "repositoryName": "team-a/my-repo" },
    });
    let case = format!("create the container recipe {name}");
    let (status, answer) =
        call_in(router, REGION, "PUT", "/CreateContainerRecipe", Some(&body)).await;
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
    let (status, answer) = call_in(
        router,
        REGION,
        "PUT",
        "/CreateInfrastructureConfiguration",
        Some(&body),
    )
    .await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "infrastructureConfigurationArn")
}

/// `CreateDistributionConfiguration` for `name`, answering its ARN.
async fn create_distribution(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("distribution-{name}"),
        "distributions": [{ "region": REGION, "amiDistributionConfiguration": {} }],
    });
    let case = format!("create the distribution configuration {name}");
    let (status, answer) = call_in(
        router,
        REGION,
        "PUT",
        "/CreateDistributionConfiguration",
        Some(&body),
    )
    .await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "distributionConfigurationArn")
}

/// `CreateWorkflow` for `name` on the build stage, answering its build version ARN.
async fn create_workflow(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "name": name,
        "semanticVersion": "1.0.0",
        "type": "BUILD",
        "clientToken": format!("workflow-{name}"),
        "data": WORKFLOW_DOCUMENT,
    });
    let case = format!("create the workflow {name}");
    let (status, answer) = call_in(router, REGION, "PUT", "/CreateWorkflow", Some(&body)).await;
    let answer = assert_ok(&case, status, answer);
    text(&case, &answer, "workflowBuildVersionArn")
}

/// `CreateImagePipeline` with `body`.
async fn create_pipeline(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call_in(router, REGION, "PUT", "/CreateImagePipeline", Some(body)).await
}

/// `UpdateImagePipeline` with `body`, in `region`.
async fn update_pipeline_in(
    router: &MultiServiceRouter,
    region: &str,
    body: &Value,
) -> (u16, Value) {
    call_in(router, region, "PUT", "/UpdateImagePipeline", Some(body)).await
}

/// `UpdateImagePipeline` with `body`, in [`REGION`].
async fn update_pipeline(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    update_pipeline_in(router, REGION, body).await
}

/// The `imagePipeline` one `GetImagePipeline` answer carries.
async fn read_pipeline(case: &str, router: &MultiServiceRouter, arn: &str) -> Value {
    let (status, answer) = call_in(
        router,
        REGION,
        "GET",
        &format!("/GetImagePipeline?imagePipelineArn={}", encode_label(arn)),
        None,
    )
    .await;
    let answer = assert_ok(case, status, answer);
    answer
        .get("imagePipeline")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an imagePipeline member in {answer}"))
}

/// Fold the members of `extra` into `body`.
fn merge(body: &mut Map<String, Value>, extra: Value) {
    if let Value::Object(extra) = extra {
        for (member, value) in extra {
            body.insert(member, value);
        }
    }
}

/// An update body: the two required members, the `clientToken`, and whatever else `extra`
/// names.
fn update_body(arn: &str, client_token: &str, configuration_arn: &str, extra: Value) -> Value {
    let mut body = Map::new();
    body.insert("imagePipelineArn".to_string(), json!(arn));
    body.insert("clientToken".to_string(), json!(client_token));
    body.insert(
        "infrastructureConfigurationArn".to_string(),
        json!(configuration_arn),
    );
    merge(&mut body, extra);
    Value::Object(body)
}

/// The ARN a pipeline named `normalized` carries in `region`.
fn pipeline_arn(region: &str, normalized: &str) -> String {
    format!("arn:aws:imagebuilder:{region}:{ACCOUNT}:image-pipeline/{normalized}")
}

/// The resources one case's pipeline references, created once per emulator.
struct Fixture {
    /// The pipeline's own ARN.
    pipeline: String,
    /// The infrastructure configuration every update names.
    configuration: String,
    /// The image recipe the pipeline was created with.
    image_recipe: String,
    /// A second image recipe, of the same name at a higher version.
    newer_image_recipe: String,
    /// An image recipe whose component targets another platform, so a replacement naming it
    /// changes the platform the pipeline reports.
    windows_recipe: String,
    /// A container recipe, for the recipe-kind rows.
    container_recipe: String,
    /// A distribution configuration a replacement may name.
    distribution: String,
    /// A workflow a replacement may name.
    workflow: String,
}

/// Create the resources a case needs and one pipeline named `name` carrying `extra`,
/// answering what the case references.
async fn given_pipeline(router: &MultiServiceRouter, name: &str, extra: Value) -> Fixture {
    let linux_component = create_component(router, "linux-component", "Linux").await;
    let windows_component = create_component(router, "windows-component", "Windows").await;
    let image_recipe = create_image_recipe(router, "build-recipe", "1.0.0", &linux_component).await;
    let newer_image_recipe =
        create_image_recipe(router, "build-recipe", "1.1.0", &linux_component).await;
    let windows_recipe =
        create_image_recipe(router, "windows-recipe", "1.0.0", &windows_component).await;
    let container_recipe = create_container_recipe(router, "container-recipe").await;
    let configuration = create_configuration(router, "build-infra").await;
    let distribution = create_distribution(router, "build-distribution").await;
    let workflow = create_workflow(router, "build-workflow").await;

    let mut body = Map::new();
    body.insert("name".to_string(), json!(name));
    body.insert("clientToken".to_string(), json!(format!("create-{name}")));
    body.insert("imageRecipeArn".to_string(), json!(image_recipe));
    body.insert(
        "infrastructureConfigurationArn".to_string(),
        json!(configuration),
    );
    merge(&mut body, extra);
    let body = Value::Object(body);

    let case = format!("create the pipeline {name}");
    let (status, created) = create_pipeline(router, &body).await;
    let created = assert_ok(&case, status, created);

    Fixture {
        pipeline: text(&case, &created, "imagePipelineArn"),
        configuration,
        image_recipe,
        newer_image_recipe,
        windows_recipe,
        container_recipe,
        distribution,
        workflow,
    }
}

/// What the pipeline every replacement case starts from carries, so each row's replacement
/// is visible member by member.
fn created_settings() -> Value {
    json!({
        "description": "the pipeline as created",
        "status": DISABLED,
        "enhancedImageMetadataEnabled": false,
        "imageTestsConfiguration": {"imageTestsEnabled": false, "timeoutMinutes": 90},
        "schedule": {
            "scheduleExpression": "cron(0 0 * * ? *)",
            "pipelineExecutionStartCondition": "EXPRESSION_MATCH_ONLY",
        },
        "imageTags": {"stage": "created"},
        "tags": {"owner": "platform"},
    })
}

/// The members an update body describes the PIPELINE with that a following read reports
/// back under another name, or not at all: `clientToken` is echoed by the response and
/// `imagePipelineArn` is the resource's own `arn`.
const NOT_PIPELINE_MEMBERS: &[&str] = &["clientToken", "imagePipelineArn"];

/// Every optional member of the stored pipeline an update can carry and CLEAR by omitting:
/// "Any optional property that you omit is removed".
const CLEARED_MEMBERS: &[&str] = &[
    "description",
    "distributionConfigurationArn",
    "schedule",
    "imageTags",
    "imageScanningConfiguration",
    "executionRole",
    "workflows",
    "loggingConfiguration",
];

/// One row of the replacement table: what the update submits beside the two required
/// members.
struct Row {
    case: &'static str,
    submitted: fn(&Fixture) -> Value,
}

/// The replacement table. The rows differ along what the rule generalises over: which
/// member a replacement carries, the size and type of its value, how many entries its
/// lists and maps carry, and whether it carries the member at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "nothing but the required members and a recipe, clearing every optional one",
            submitted: |fixture| json!({"imageRecipeArn": fixture.image_recipe}),
        },
        Row {
            case: "a new description and an explicit status",
            submitted: |fixture| {
                json!({
                    "imageRecipeArn": fixture.image_recipe,
                    "description": "the pipeline as updated",
                    "status": ENABLED,
                })
            },
        },
        Row {
            case: "a distribution configuration and one image tag",
            submitted: |fixture| {
                json!({
                    "imageRecipeArn": fixture.image_recipe,
                    "distributionConfigurationArn": fixture.distribution,
                    "imageTags": {"stage": "updated"},
                })
            },
        },
        Row {
            case: "three image tags and a scanning configuration",
            submitted: |fixture| {
                json!({
                    "imageRecipeArn": fixture.image_recipe,
                    "imageTags": {"stage": "updated", "owner": "platform", "team": "builders"},
                    "imageScanningConfiguration": {
                        "imageScanningEnabled": true,
                        "ecrConfiguration": {
                            "repositoryName": "scans",
                            "containerTags": ["latest", "nightly"],
                        },
                    },
                })
            },
        },
        Row {
            case: "a schedule with every member and a logging configuration",
            submitted: |fixture| {
                json!({
                    "imageRecipeArn": fixture.image_recipe,
                    "schedule": {
                        "scheduleExpression": "cron(0 12 ? * MON *)",
                        "timezone": "Etc/UTC",
                        "pipelineExecutionStartCondition":
                            "EXPRESSION_MATCH_AND_DEPENDENCY_UPDATES_AVAILABLE",
                        "autoDisablePolicy": {"failureCount": 3},
                    },
                    "loggingConfiguration": {
                        "imageLogGroupName": "/aws/imagebuilder/images",
                        "pipelineLogGroupName": "/aws/imagebuilder/pipelines",
                    },
                    "executionRole": "ImageBuilderWorkflowRole",
                })
            },
        },
        Row {
            case: "a workflow, which requires an execution role",
            submitted: |fixture| {
                json!({
                    "imageRecipeArn": fixture.image_recipe,
                    "executionRole": format!("arn:aws:iam::{ACCOUNT}:role/ImageBuilderWorkflow"),
                    "workflows": [{
                        "workflowArn": fixture.workflow,
                        "onFailure": "ABORT",
                        "parallelGroup": "builds",
                    }],
                })
            },
        },
        Row {
            case: "a newer version of the same recipe, with the test settings turned off",
            submitted: |fixture| {
                json!({
                    "imageRecipeArn": fixture.newer_image_recipe,
                    "imageTestsConfiguration": {"imageTestsEnabled": false},
                    "enhancedImageMetadataEnabled": true,
                })
            },
        },
        Row {
            case: "a recipe reference wildcarding its trailing version nodes",
            submitted: |_| {
                json!({
                    "imageRecipeArn": format!(
                        "arn:aws:imagebuilder:{REGION}:{ACCOUNT}:image-recipe/build-recipe/1.x.x"
                    ),
                })
            },
        },
    ]
}

/// RULE: an accepted update replaces the pipeline, so the read reports what the update
/// submitted, the documented default of every property it omitted that has one, and nothing
/// of what the create had stored under any other property it omitted.
#[tokio::test]
async fn an_accepted_update_replaces_the_pipeline() {
    for (n, row) in rows().into_iter().enumerate() {
        let router = emulator();
        let name = format!("replaced-pipeline-{n:02}");
        let fixture = given_pipeline(&router, &name, created_settings()).await;

        let body = update_body(
            &fixture.pipeline,
            &format!("update-{n:04}"),
            &fixture.configuration,
            (row.submitted)(&fixture),
        );
        let (status, updated) = update_pipeline(&router, &body).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from the update, got {status} with {updated}",
            row.case
        );
        assert_eq!(
            updated.get("imagePipelineArn"),
            Some(&json!(fixture.pipeline)),
            "case {:?}: expected the update to answer with the pipeline's own arn, got {updated}",
            row.case
        );
        assert_eq!(
            updated.get("clientToken"),
            Some(&json!(format!("update-{n:04}"))),
            "case {:?}: expected the update to echo its clientToken, got {updated}",
            row.case
        );

        let pipeline = read_pipeline(row.case, &router, &fixture.pipeline).await;
        let sent = body.as_object().expect("an update body is an object");

        // Every member the request carried is reported as submitted. The test
        // configuration is the one exception: Image Builder stores a default for each of
        // its two settings, so a request naming one of them is read back carrying both.
        for (member, value) in sent.iter().filter(|(member, _)| {
            !NOT_PIPELINE_MEMBERS.contains(&member.as_str()) && *member != "imageTestsConfiguration"
        }) {
            let reported = pipeline.get(member).cloned().unwrap_or(Value::Null);
            assert_eq!(
                reported, *value,
                "case {:?}: expected the stored {member} to be {value}, got {reported}",
                row.case
            );
        }

        // "Any optional property that you omit is removed or reset to its default": a
        // property with no documented value of the service's own comes back absent …
        for member in CLEARED_MEMBERS
            .iter()
            .filter(|member| !sent.contains_key(**member))
        {
            assert!(
                pipeline.get(*member).is_none(),
                "case {:?}: expected {member} cleared by the update, got {}",
                row.case,
                pipeline[*member]
            );
        }
        // … and one the service documents a value for comes back carrying that value.
        let expected_status = sent
            .get("status")
            .cloned()
            .unwrap_or_else(|| json!(ENABLED));
        let reported_status = pipeline.get("status").cloned().unwrap_or(Value::Null);
        assert_eq!(
            reported_status, expected_status,
            "case {:?}: expected the status {expected_status}, got {reported_status}",
            row.case
        );
        let expected_metadata = sent
            .get("enhancedImageMetadataEnabled")
            .cloned()
            .unwrap_or(json!(true));
        let reported_metadata = pipeline
            .get("enhancedImageMetadataEnabled")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            reported_metadata, expected_metadata,
            "case {:?}: expected enhancedImageMetadataEnabled {expected_metadata}, got \
             {reported_metadata}",
            row.case
        );
        let submitted_tests = sent.get("imageTestsConfiguration");
        let expected_tests = json!({
            "imageTestsEnabled": submitted_tests
                .and_then(|tests| tests.get("imageTestsEnabled"))
                .cloned()
                .unwrap_or(json!(true)),
            "timeoutMinutes": submitted_tests
                .and_then(|tests| tests.get("timeoutMinutes"))
                .cloned()
                .unwrap_or_else(|| json!(720)),
        });
        let reported_tests = pipeline
            .get("imageTestsConfiguration")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            reported_tests, expected_tests,
            "case {:?}: expected the test configuration {expected_tests}, got {reported_tests}",
            row.case
        );

        // The three members an update cannot carry are what the pipeline keeps.
        assert_eq!(
            pipeline.get("name"),
            Some(&json!(name)),
            "case {:?}: expected the create's name to survive the update, got {pipeline}",
            row.case
        );
        assert_eq!(
            pipeline.get("arn"),
            Some(&json!(fixture.pipeline)),
            "case {:?}: expected the pipeline's own arn, got {pipeline}",
            row.case
        );
        assert_eq!(
            pipeline.get("tags"),
            Some(&json!({"owner": "platform"})),
            "case {:?}: expected the resource's own tags to survive the update, got {pipeline}",
            row.case
        );
        let date_created = pipeline.get("dateCreated").and_then(Value::as_str);
        assert!(
            date_created.is_some(),
            "case {:?}: expected the create's dateCreated, got {pipeline}",
            row.case
        );
        let date_updated = pipeline.get("dateUpdated").and_then(Value::as_str);
        assert!(
            date_updated.is_some_and(|stamp| stamp.contains('T') && stamp.ends_with('Z')),
            "case {:?}: expected an ISO 8601 dateUpdated, got {date_updated:?}",
            row.case
        );
    }
}

/// RULE: `status` "Defaults to ENABLED when omitted. To keep a pipeline disabled, include
/// this property set to DISABLED in your update request" — so each update's status is what
/// the following read reports, whatever the pipeline carried before it.
#[tokio::test]
async fn each_updates_status_is_what_the_pipeline_reports() {
    // (case, the status the create stored, the status the update carries, the one reported)
    let rows: &[(&str, Option<&str>, Option<&str>, &str)] = &[
        ("enabled, then disabled", Some(ENABLED), Some(DISABLED), DISABLED),
        ("disabled, then enabled", Some(DISABLED), Some(ENABLED), ENABLED),
        (
            "disabled, then an update that omits the status",
            Some(DISABLED),
            None,
            ENABLED,
        ),
        (
            "enabled, then an update that omits the status",
            Some(ENABLED),
            None,
            ENABLED,
        ),
        (
            "created without a status, then disabled",
            None,
            Some(DISABLED),
            DISABLED,
        ),
        ("disabled, then disabled again", Some(DISABLED), Some(DISABLED), DISABLED),
    ];

    for (n, (case, created, submitted, expected)) in rows.iter().enumerate() {
        let router = emulator();
        let name = format!("switched-pipeline-{n:02}");
        let created_extra = match created {
            Some(status) => json!({"status": status}),
            None => json!({}),
        };
        let fixture = given_pipeline(&router, &name, created_extra).await;

        let mut extra = Map::new();
        extra.insert(
            "imageRecipeArn".to_string(),
            json!(fixture.image_recipe.clone()),
        );
        if let Some(status) = submitted {
            extra.insert("status".to_string(), json!(status));
        }
        let (status, updated) = update_pipeline(
            &router,
            &update_body(
                &fixture.pipeline,
                &format!("switch-{n:04}"),
                &fixture.configuration,
                Value::Object(extra),
            ),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the update, got {status} with {updated}"
        );

        let pipeline = read_pipeline(case, &router, &fixture.pipeline).await;
        let reported = pipeline.get("status").cloned().unwrap_or(Value::Null);
        assert_eq!(
            reported,
            json!(expected),
            "case {case:?}: expected the status {expected:?}, got {reported}"
        );
    }
}

/// RULE: a pipeline keeps the KIND of recipe it builds — "The recipe must be the same type,
/// image or container, as the pipeline's current recipe" — and the platform it reports is
/// inherited from the recipe the replacement names.
#[tokio::test]
async fn a_replacement_names_one_recipe_of_the_pipelines_own_kind() {
    let router = emulator();
    let fixture = given_pipeline(&router, "kept-kind-pipeline", created_settings()).await;

    // An image pipeline given a CONTAINER recipe: the kinds disagree, so the replacement is
    // refused and the stored pipeline is untouched.
    let (status, body) = update_pipeline(
        &router,
        &update_body(
            &fixture.pipeline,
            "kind-mismatch",
            &fixture.configuration,
            json!({"containerRecipeArn": fixture.container_recipe}),
        ),
    )
    .await;
    assert_refused(
        "an image pipeline given a container recipe",
        status,
        &body,
        400,
        "InvalidRequestException",
        "The recipe must be the same type, image or container, as the pipeline's current \
         recipe.",
    );
    let pipeline = read_pipeline("after the refused kind change", &router, &fixture.pipeline).await;
    assert_eq!(
        pipeline.get("imageRecipeArn"),
        Some(&json!(fixture.image_recipe)),
        "case \"after the refused kind change\": expected the create's recipe, got {pipeline}"
    );
    assert_eq!(
        pipeline.get("dateUpdated"),
        pipeline.get("dateCreated"),
        "case \"after the refused kind change\": expected the pipeline to still report the \
         create's own instant as dateUpdated, got {pipeline}"
    );

    // Another IMAGE recipe is admitted, and the platform the pipeline reports is the one
    // that recipe builds for.
    let (status, updated) = update_pipeline(
        &router,
        &update_body(
            &fixture.pipeline,
            "kind-kept",
            &fixture.configuration,
            json!({"imageRecipeArn": fixture.windows_recipe}),
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"an image pipeline given another image recipe\": expected HTTP 200, got {status} \
         with {updated}"
    );
    let pipeline = read_pipeline("the replaced recipe", &router, &fixture.pipeline).await;
    assert_eq!(
        pipeline.get("imageRecipeArn"),
        Some(&json!(fixture.windows_recipe)),
        "case \"the replaced recipe\": expected the submitted recipe, got {pipeline}"
    );
    assert_eq!(
        pipeline.get("platform"),
        Some(&json!("Windows")),
        "case \"the replaced recipe\": expected the platform inherited from that recipe, got \
         {pipeline}"
    );
    assert!(
        pipeline.get("containerRecipeArn").is_none(),
        "case \"the replaced recipe\": expected no container recipe, got {pipeline}"
    );

    // The same rule from the other side: a CONTAINER pipeline keeps its container recipe
    // and refuses an image one.
    let container_router = emulator();
    let container_fixture = {
        let container_recipe = create_container_recipe(&container_router, "container-recipe").await;
        let configuration = create_configuration(&container_router, "build-infra").await;
        let image_component = create_component(&container_router, "linux-component", "Linux").await;
        let image_recipe =
            create_image_recipe(&container_router, "build-recipe", "1.0.0", &image_component).await;
        let case = "create the container pipeline";
        let (status, created) = create_pipeline(
            &container_router,
            &json!({
                "name": "container-pipeline",
                "clientToken": "create-container-pipeline",
                "containerRecipeArn": container_recipe,
                "infrastructureConfigurationArn": configuration,
            }),
        )
        .await;
        let created = assert_ok(case, status, created);
        (
            text(case, &created, "imagePipelineArn"),
            configuration,
            container_recipe,
            image_recipe,
        )
    };
    let (pipeline_arn, configuration, container_recipe, image_recipe) = container_fixture;

    let (status, body) = update_pipeline(
        &container_router,
        &update_body(
            &pipeline_arn,
            "container-kind-mismatch",
            &configuration,
            json!({"imageRecipeArn": image_recipe}),
        ),
    )
    .await;
    assert_refused(
        "a container pipeline given an image recipe",
        status,
        &body,
        400,
        "InvalidRequestException",
        "The recipe must be the same type, image or container, as the pipeline's current \
         recipe.",
    );

    let (status, updated) = update_pipeline(
        &container_router,
        &update_body(
            &pipeline_arn,
            "container-kind-kept",
            &configuration,
            json!({"containerRecipeArn": container_recipe, "description": "replaced"}),
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"a container pipeline given another container recipe\": expected HTTP 200, got \
         {status} with {updated}"
    );
    let pipeline = read_pipeline(
        "the replaced container pipeline",
        &container_router,
        &pipeline_arn,
    )
    .await;
    assert_eq!(
        pipeline.get("containerRecipeArn"),
        Some(&json!(container_recipe)),
        "case \"the replaced container pipeline\": expected the submitted recipe, got {pipeline}"
    );
}

/// RULE: every resource the replacement references — the pipeline it replaces included —
/// has to be a stored resource of the calling Region, so a reference that names none is
/// reported missing and nothing is written.
#[tokio::test]
async fn a_reference_that_names_nothing_is_reported_missing() {
    let router = emulator();
    let fixture = given_pipeline(&router, "present-pipeline", created_settings()).await;

    let absent_pipeline = pipeline_arn(REGION, "absent-pipeline");
    let other_region_pipeline = pipeline_arn(OTHER_REGION, "present-pipeline");
    let absent_configuration =
        format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:infrastructure-configuration/absent");
    let absent_recipe =
        format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:image-recipe/build-recipe/9.9.9");
    let absent_distribution =
        format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:distribution-configuration/absent");
    let absent_workflow =
        format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:workflow/build/build-workflow/9.9.9/1");

    // (case, the pipeline the update names, the Region it is signed for, the configuration
    // it names, the rest of the body, the kind and the identifier the refusal reports)
    let rows: Vec<(&str, String, &str, String, Value, &str, String)> = vec![
        (
            "a pipeline name nothing was stored under",
            absent_pipeline.clone(),
            REGION,
            fixture.configuration.clone(),
            json!({"imageRecipeArn": fixture.image_recipe}),
            "ImagePipeline",
            absent_pipeline.clone(),
        ),
        (
            "another Region's endpoint for a stored pipeline",
            fixture.pipeline.clone(),
            OTHER_REGION,
            fixture.configuration.clone(),
            json!({"imageRecipeArn": fixture.image_recipe}),
            "ImagePipeline",
            fixture.pipeline.clone(),
        ),
        (
            "another Region's own pipeline ARN",
            other_region_pipeline.clone(),
            REGION,
            fixture.configuration.clone(),
            json!({"imageRecipeArn": fixture.image_recipe}),
            "ImagePipeline",
            other_region_pipeline.clone(),
        ),
        (
            "an infrastructure configuration nothing was stored under",
            fixture.pipeline.clone(),
            REGION,
            absent_configuration.clone(),
            json!({"imageRecipeArn": fixture.image_recipe}),
            "InfrastructureConfiguration",
            absent_configuration.clone(),
        ),
        (
            "a recipe version nothing was stored under",
            fixture.pipeline.clone(),
            REGION,
            fixture.configuration.clone(),
            json!({"imageRecipeArn": absent_recipe.clone()}),
            "ImageRecipe",
            absent_recipe.clone(),
        ),
        (
            "a distribution configuration nothing was stored under",
            fixture.pipeline.clone(),
            REGION,
            fixture.configuration.clone(),
            json!({
                "imageRecipeArn": fixture.image_recipe,
                "distributionConfigurationArn": absent_distribution.clone(),
            }),
            "DistributionConfiguration",
            absent_distribution.clone(),
        ),
        (
            "a workflow version nothing was stored under",
            fixture.pipeline.clone(),
            REGION,
            fixture.configuration.clone(),
            json!({
                "imageRecipeArn": fixture.image_recipe,
                "executionRole": "ImageBuilderWorkflowRole",
                "workflows": [{"workflowArn": absent_workflow.clone()}],
            }),
            "Workflow",
            absent_workflow.clone(),
        ),
    ];

    for (n, (case, named, region, configuration, extra, resource_type, identifier)) in
        rows.into_iter().enumerate()
    {
        let (status, body) = update_pipeline_in(
            &router,
            region,
            &update_body(
                &named,
                &format!("missing-{n:04}"),
                &configuration,
                extra.clone(),
            ),
        )
        .await;
        assert_refused(
            case,
            status,
            &body,
            404,
            "ResourceNotFoundException",
            &missing_message(resource_type, &identifier),
        );
    }

    // None of them wrote anything: the pipeline is still the one the create stored.
    let pipeline = read_pipeline("the stored pipeline afterwards", &router, &fixture.pipeline).await;
    assert_eq!(
        pipeline.get("description"),
        Some(&json!("the pipeline as created")),
        "case \"the stored pipeline afterwards\": expected the create's settings, got {pipeline}"
    );
    assert_eq!(
        pipeline.get("dateUpdated"),
        pipeline.get("dateCreated"),
        "case \"the stored pipeline afterwards\": expected the pipeline to still report the \
         create's own instant as dateUpdated, got {pipeline}"
    );
}

/// RULE: the parameter combinations the operation cannot serve are refused, and a refused
/// update writes nothing — "You must specify exactly one recipe for your image", and "You
/// must also specify executionRole when you provide workflows".
#[tokio::test]
async fn a_refused_update_leaves_the_pipeline_as_it_was() {
    let router = emulator();
    let fixture = given_pipeline(&router, "refused-pipeline", created_settings()).await;

    // (case, what the update carried beside the required members, the status, the code, the
    // message)
    let rows: Vec<(&str, Value, u16, &str, String)> = vec![
        (
            "neither recipe",
            json!({}),
            400,
            "MissingParametersException",
            "Required parameters are missing from the request: 'imageRecipeArn' or \
             'containerRecipeArn'. Verify that you have entered all of the required parameters \
             in your request and try again."
                .to_string(),
        ),
        (
            "both recipes",
            json!({
                "imageRecipeArn": fixture.image_recipe,
                "containerRecipeArn": fixture.container_recipe,
            }),
            400,
            "InvalidParameterCombinationException",
            "The value supplied for parameter 'imageRecipeArn' and 'containerRecipeArn' is \
             not valid. You must specify either imageRecipeArn or containerRecipeArn."
                .to_string(),
        ),
        (
            "a workflow with no execution role",
            json!({
                "imageRecipeArn": fixture.image_recipe,
                "workflows": [{"workflowArn": fixture.workflow}],
            }),
            400,
            "InvalidRequestException",
            "The request must specify an executionRole when it specifies workflows.".to_string(),
        ),
    ];

    for (n, (case, extra, expect_status, code, message)) in rows.into_iter().enumerate() {
        let (status, body) = update_pipeline(
            &router,
            &update_body(
                &fixture.pipeline,
                &format!("refused-{n:04}"),
                &fixture.configuration,
                extra,
            ),
        )
        .await;
        assert_refused(case, status, &body, expect_status, code, &message);

        let pipeline = read_pipeline(case, &router, &fixture.pipeline).await;
        assert_eq!(
            pipeline.get("description"),
            Some(&json!("the pipeline as created")),
            "case {case:?}: expected the create's description, got {pipeline}"
        );
        assert_eq!(
            pipeline.get("status"),
            Some(&json!(DISABLED)),
            "case {case:?}: expected the create's status, got {pipeline}"
        );
        assert_eq!(
            pipeline.get("dateUpdated"),
            pipeline.get("dateCreated"),
            "case {case:?}: expected the create's own instant to still be the dateUpdated \
             after a refused update, got {pipeline}"
        );
    }
}

/// RULE: `clientToken` is an idempotency token — "If you retry a request with the same
/// client token, Image Builder returns the original response without running the operation
/// again", and the same token carrying different settings is a mismatch. The update's
/// tokens are its own: one spent on the create does not answer for it.
#[tokio::test]
async fn a_client_token_answers_for_one_update_only() {
    let router = emulator();
    let fixture = given_pipeline(&router, "idempotent-pipeline", created_settings()).await;

    let first = update_body(
        &fixture.pipeline,
        "shared-token",
        &fixture.configuration,
        json!({
            "imageRecipeArn": fixture.image_recipe,
            "description": "the original update",
        }),
    );
    let (status, original) = update_pipeline(&router, &first).await;
    assert_eq!(
        status, 200,
        "case \"the original update\": expected HTTP 200, got {status} with {original}"
    );

    // The retry answers the ORIGINAL response, down to its requestId, and writes nothing.
    let (status, retried) = update_pipeline(&router, &first).await;
    assert_eq!(
        status, 200,
        "case \"the retry\": expected HTTP 200, got {status} with {retried}"
    );
    assert_eq!(
        retried, original,
        "case \"the retry\": expected the original response {original}, got {retried}"
    );

    // Each row reuses the token with ONE setting changed, along what the rule generalises
    // over: a string, a reference, an enum value and a nested structure.
    let mismatched: Vec<(&str, Value)> = vec![
        (
            "a different description",
            json!({
                "imageRecipeArn": fixture.image_recipe,
                "description": "a different update",
            }),
        ),
        (
            "a different recipe reference",
            json!({
                "imageRecipeArn": fixture.newer_image_recipe,
                "description": "the original update",
            }),
        ),
        (
            "an added status",
            json!({
                "imageRecipeArn": fixture.image_recipe,
                "description": "the original update",
                "status": DISABLED,
            }),
        ),
        (
            "an added schedule",
            json!({
                "imageRecipeArn": fixture.image_recipe,
                "description": "the original update",
                "schedule": {"scheduleExpression": "cron(0 1 * * ? *)"},
            }),
        ),
    ];
    for (case, extra) in mismatched {
        let (status, body) = update_pipeline(
            &router,
            &update_body(
                &fixture.pipeline,
                "shared-token",
                &fixture.configuration,
                extra,
            ),
        )
        .await;
        assert_refused(
            case,
            status,
            &body,
            400,
            "IdempotentParameterMismatchException",
            "The request parameters do not match those in the original idempotent request.",
        );
    }

    // The token the CREATE spent is free for an update: the two operations file their
    // receipts apart.
    let (status, body) = update_pipeline(
        &router,
        &update_body(
            &fixture.pipeline,
            "create-idempotent-pipeline",
            &fixture.configuration,
            json!({
                "imageRecipeArn": fixture.image_recipe,
                "description": "an update carrying the create's token",
            }),
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the create's token on an update\": expected HTTP 200, got {status} with {body}"
    );
    let pipeline = read_pipeline(
        "the create's token on an update",
        &router,
        &fixture.pipeline,
    )
    .await;
    assert_eq!(
        pipeline.get("description"),
        Some(&json!("an update carrying the create's token")),
        "case \"the create's token on an update\": expected the update applied, got {pipeline}"
    );
}
