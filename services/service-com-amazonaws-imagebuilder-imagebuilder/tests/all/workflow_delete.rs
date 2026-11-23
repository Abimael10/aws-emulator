// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `DeleteWorkflow`: what a delete takes out of this service's
//! store, what it echoes, and when it is refused.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what
//! a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! * a delete REMOVES exactly the workflow build version its ARN names and echoes the ARN of
//!   the record it removed, so every later read of that build reports it missing while every
//!   other build of the same name and version is still read, and a repeat of a delete that
//!   already succeeded is reported as the missing resource it names — which is also how an ARN
//!   this Region holds nothing under is reported, a regional endpoint reaching only its own
//!   Region's resources;
//! * a workflow a stored image pipeline still references is NOT removed: "You can't delete a
//!   workflow build version while an image pipeline references it. The request fails with
//!   ResourceDependencyException", and a reference carrying trailing `x` wildcards reaches
//!   whichever build is newest at the instant the pipeline runs, so every build it admits is
//!   still in use;
//! * an image BUILD VERSION is not a dependant: the workflows it records are a snapshot of
//!   what that build ran, so the workflow is deletable while the build it ran in stands.
//!
//! The rows span what the rules generalise over: the stage of the workflow (every value of
//! the modelled `WorkflowType`), its version, which build version of a name+version pair the
//! delete names, the form of the reference a pipeline holds (a build version ARN and each
//! wildcard version ARN), and which Region the call is signed for.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every case signs for unless it is about another Region.
const REGION: &str = "us-west-2";

/// A second Region, to show that a delete reaches only its own Region's resources.
const OTHER_REGION: &str = "eu-west-1";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// A workflow document declaring one step and no input parameters: the smallest document
/// this service accepts, since the steps are the process Image Builder runs.
const PLAIN_DOCUMENT: &str = concat!(
    "name: BuildImage\n",
    "schemaVersion: 1.0\n",
    "steps:\n",
    "  - name: LaunchBuildInstance\n",
    "    action: LaunchInstance\n",
    "    onFailure: Abort\n",
);

/// A build component document, for the recipe the pipeline and the image cases build.
const BUILD_COMPONENT_DOCUMENT: &str = concat!(
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
/// mounted beside this service here, so nothing answers the lookup of it and the create
/// proceeds — which is the point: these cases are about the workflow, not the base image.
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

/// Status and parsed body of one call in `region`. An empty body reads as `Value::Null`.
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

/// [`call_in`] for [`REGION`], which is where every case but the cross-Region one calls.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    call_in(router, REGION, method, uri, body).await
}

/// The text one member of a response carries.
fn text(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected text in {name} of {body}"))
        .to_string()
}

/// One member of a response, or a panic naming the case.
fn member<'body>(case: &str, body: &'body Value, name: &str) -> &'body Value {
    body.get(name)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {name} in {body}"))
}

/// Assert a call succeeded, and answer its body.
fn assert_ok(case: &str, status: u16, body: Value) -> Value {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {body}"
    );
    body
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

/// The `message` a refusal carries.
fn error_message(case: &str, body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message, got {body}"))
        .to_string()
}

/// Assert the answer is a refusal carrying `expected`, `code` and `message`.
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
    let actual_message = error_message(case, body);
    assert_eq!(
        actual_message, message,
        "case {case:?}: expected the message {message:?}, got {actual_message:?}"
    );
}

/// Assert the answer reports the workflow `arn` names as the missing required resource it is.
fn assert_missing_workflow(case: &str, status: u16, body: &Value, arn: &str) {
    assert_refused(
        case,
        status,
        body,
        404,
        "ResourceNotFoundException",
        &format!("The following required resource 'Workflow' cannot be found: '{arn}'."),
    );
}

/// Assert the answer refuses the delete as the dependency it is.
fn assert_depended_on(case: &str, status: u16, body: &Value, arn: &str) {
    assert_refused(
        case,
        status,
        body,
        400,
        "ResourceDependencyException",
        &format!(
            "Resource dependency error: The resource ARN '{arn}' has other resources depended \
             on it."
        ),
    );
}

/// The ARN of one build version of the `stage` workflow `normalized` names at `version`.
fn workflow_build_arn(stage: &str, normalized: &str, version: &str, build: u32) -> String {
    format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:workflow/{stage}/{normalized}/{version}/{build}")
}

/// The version ARN (no build number) of the `stage` workflow `normalized` names at `version`,
/// whose trailing nodes a row may spell as `x` wildcards.
fn workflow_version_arn(stage: &str, normalized: &str, version: &str) -> String {
    format!("arn:aws:imagebuilder:{REGION}:{ACCOUNT}:workflow/{stage}/{normalized}/{version}")
}

/// `CreateWorkflow` for `name` at `version` on `workflow_type`, in `region`.
async fn create_workflow(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
    workflow_type: &str,
    version: &str,
    token: &str,
) -> (u16, Value) {
    let body = json!({
        "name": name,
        "semanticVersion": version,
        "type": workflow_type,
        "clientToken": token,
        "data": PLAIN_DOCUMENT,
    });
    call_in(router, region, "PUT", "/CreateWorkflow", Some(&body)).await
}

/// `GetWorkflow` for the reference `arn`, in `region`.
async fn get_workflow(router: &MultiServiceRouter, region: &str, arn: &str) -> (u16, Value) {
    call_in(
        router,
        region,
        "GET",
        &format!("/GetWorkflow?workflowBuildVersionArn={arn}"),
        None,
    )
    .await
}

/// `DeleteWorkflow` for the build version `arn`, in `region`.
async fn delete_workflow(router: &MultiServiceRouter, region: &str, arn: &str) -> (u16, Value) {
    call_in(
        router,
        region,
        "DELETE",
        &format!("/DeleteWorkflow?workflowBuildVersionArn={arn}"),
        None,
    )
    .await
}

/// Create one workflow and answer the build version ARN it minted.
async fn given_workflow(router: &MultiServiceRouter, name: &str, version: &str) -> (String, Value) {
    let case = format!("the workflow {name:?}");
    let (status, created) = create_workflow(
        router,
        REGION,
        name,
        "BUILD",
        version,
        &format!("token-{name}"),
    )
    .await;
    let created = assert_ok(&case, status, created);
    let arn = text(&case, &created, "workflowBuildVersionArn");
    let references = member(&case, &created, "latestVersionReferences").clone();
    (arn, references)
}

/// The form of a reference that names one BUILD version exactly, beside the wildcard forms a
/// create reports in `latestVersionReferences` (each named by its own member).
const BUILD_VERSION_FORM: &str = "the build version ARN";

/// The reference of `form` to the workflow whose build version ARN is `arn` and whose create
/// reported the wildcard `references`.
fn reference_in(form: &str, arn: &str, references: &Value) -> String {
    if form == BUILD_VERSION_FORM {
        return arn.to_string();
    }
    references
        .get(form)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {form:?}: expected a reference in {references}"))
        .to_string()
}

/// The resources a build names beside its workflows.
struct Build {
    recipe: String,
    configuration: String,
}

impl Build {
    /// Stage the component, the recipe and the infrastructure configuration a build needs.
    async fn given(router: &MultiServiceRouter) -> Self {
        let body = json!({
            "name": "Workflow Delete Component",
            "semanticVersion": "1.0.0",
            "platform": "Linux",
            "clientToken": "workflow-delete-component",
            "data": BUILD_COMPONENT_DOCUMENT,
        });
        let answer = call(router, "PUT", "/CreateComponent", Some(&body)).await;
        let answer = assert_ok("the component", answer.0, answer.1);
        let component = text("the component", &answer, "componentBuildVersionArn");

        let body = json!({
            "name": "Workflow Delete Recipe",
            "semanticVersion": "1.0.0",
            "parentImage": PARENT_IMAGE,
            "clientToken": "workflow-delete-recipe",
            "components": [{ "componentArn": component }],
        });
        let answer = call(router, "PUT", "/CreateImageRecipe", Some(&body)).await;
        let answer = assert_ok("the recipe", answer.0, answer.1);
        let recipe = text("the recipe", &answer, "imageRecipeArn");

        let body = json!({
            "name": "Workflow Delete Infra",
            "clientToken": "workflow-delete-infra",
            "instanceProfileName": "EC2InstanceProfileForImageBuilder",
        });
        let answer = call(
            router,
            "PUT",
            "/CreateInfrastructureConfiguration",
            Some(&body),
        )
        .await;
        let answer = assert_ok("the configuration", answer.0, answer.1);
        let configuration = text("the configuration", &answer, "infrastructureConfigurationArn");

        Self {
            recipe,
            configuration,
        }
    }

    /// A `CreateImage` body naming `reference` as the workflow it runs.
    fn image(&self, token: &str, reference: &str) -> Value {
        json!({
            "imageRecipeArn": self.recipe,
            "infrastructureConfigurationArn": self.configuration,
            "clientToken": token,
            "executionRole": "ImageBuilderWorkflowRole",
            "workflows": [{ "workflowArn": reference }],
        })
    }

    /// A `CreateImagePipeline` body naming `reference` as the workflow it runs.
    fn pipeline(&self, name: &str, token: &str, reference: &str) -> Value {
        let mut body = self.image(token, reference);
        if let Value::Object(map) = &mut body {
            map.insert("name".to_string(), json!(name));
        }
        body
    }
}

/// One stage of a workflow, as the modelled `WorkflowType` spells it beside the segment its
/// ARN carries (`smithy_query(query="shape",shape="WorkflowType")` reports BUILD, TEST and
/// DISTRIBUTION), together with the name and version this row's workflow carries.
struct Row {
    case: &'static str,
    name: &'static str,
    normalized: &'static str,
    workflow_type: &'static str,
    stage: &'static str,
    version: &'static str,
}

/// A delete takes out exactly the build version its ARN names: the record it removed is what
/// it echoes, every later read of that build reports it missing, every OTHER build of the
/// same name and version is still read, and a repeat of the delete is reported missing too.
#[tokio::test]
async fn a_delete_removes_exactly_the_build_version_it_names() {
    // The rows differ along the stage, the name's normalization and the version — everything
    // a workflow's identity is made of beside the build number the rule is about.
    let rows = [
        Row {
            case: "a build workflow",
            name: "delete-flow",
            normalized: "delete-flow",
            workflow_type: "BUILD",
            stage: "build",
            version: "1.0.0",
        },
        Row {
            case: "a test workflow whose name normalizes",
            name: "Nightly Test_Flow",
            normalized: "nightly-test-flow",
            workflow_type: "TEST",
            stage: "test",
            version: "2.11.3",
        },
        Row {
            case: "a distribution workflow at the version bound",
            name: "dist-flow",
            normalized: "dist-flow",
            workflow_type: "DISTRIBUTION",
            stage: "distribution",
            version: "1073741823.0.1",
        },
    ];

    for row in rows {
        let router = emulator();

        // Two build versions of one stage+name+version triple, so the delete has a sibling
        // it must leave alone.
        for build in 1..=2u32 {
            let case = format!("{}: build {build}", row.case);
            let (status, created) = create_workflow(
                &router,
                REGION,
                row.name,
                row.workflow_type,
                row.version,
                &format!("{}-{build}", row.normalized),
            )
            .await;
            let created = assert_ok(&case, status, created);
            let expected = workflow_build_arn(row.stage, row.normalized, row.version, build);
            let actual = text(&case, &created, "workflowBuildVersionArn");
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the build version {expected:?}, got {actual:?}"
            );
        }

        let first = workflow_build_arn(row.stage, row.normalized, row.version, 1);
        let second = workflow_build_arn(row.stage, row.normalized, row.version, 2);

        // The delete echoes the ARN of the record it removed.
        let case = format!("{}: the delete of build 1", row.case);
        let (status, deleted) = delete_workflow(&router, REGION, &first).await;
        let deleted = assert_ok(&case, status, deleted);
        let echoed = text(&case, &deleted, "workflowBuildVersionArn");
        assert_eq!(
            echoed, first,
            "case {case:?}: expected the removed ARN {first:?}, got {echoed:?}"
        );

        // The build it removed is gone, and the one it did not is still read.
        let (status, read) = get_workflow(&router, REGION, &first).await;
        assert_missing_workflow(&case, status, &read, &first);

        let case = format!("{}: the sibling build", row.case);
        let (status, read) = get_workflow(&router, REGION, &second).await;
        let read = assert_ok(&case, status, read);
        let reached = text(&case, member(&case, &read, "workflow"), "arn");
        assert_eq!(
            reached, second,
            "case {case:?}: expected build 2 to be read, got {reached:?}"
        );

        // A wildcard reference still reaches the build that is left.
        let wildcard = workflow_version_arn(row.stage, row.normalized, "x.x.x");
        let case = format!("{}: the wildcard reference", row.case);
        let (status, read) = get_workflow(&router, REGION, &wildcard).await;
        let read = assert_ok(&case, status, read);
        let reached = text(&case, member(&case, &read, "workflow"), "arn");
        assert_eq!(
            reached, second,
            "case {case:?}: expected the wildcard to reach {second:?}, got {reached:?}"
        );

        // A repeat of a delete that already succeeded is reported as the missing resource it
        // names, exactly as a read of it is.
        let case = format!("{}: the repeated delete", row.case);
        let (status, repeated) = delete_workflow(&router, REGION, &first).await;
        assert_missing_workflow(&case, status, &repeated, &first);

        // Once the last build of the name is gone, nothing is left for the wildcard to reach.
        let case = format!("{}: the delete of build 2", row.case);
        let (status, deleted) = delete_workflow(&router, REGION, &second).await;
        let deleted = assert_ok(&case, status, deleted);
        let echoed = text(&case, &deleted, "workflowBuildVersionArn");
        assert_eq!(
            echoed, second,
            "case {case:?}: expected the removed ARN {second:?}, got {echoed:?}"
        );
        let (status, read) = get_workflow(&router, REGION, &wildcard).await;
        assert_missing_workflow(&case, status, &read, &wildcard);
    }
}

/// A build version no Region of this store holds is reported missing rather than deleted, and
/// a build version of ANOTHER Region is reported the same way: a regional endpoint reaches
/// only its own Region's resources, so the resource is left exactly as it was.
#[tokio::test]
async fn a_delete_reaches_only_its_own_region() {
    let router = emulator();
    let (stored, _) = given_workflow(&router, "Regional Flow", "1.0.0").await;

    // The rows differ along WHY the build version is out of reach: nothing was ever stored
    // under it, its name is stored at another version, or it is stored in another Region.
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "a build version nothing created",
            REGION,
            workflow_build_arn("build", "absent-flow", "1.0.0", 1),
        ),
        (
            "another version of a stored name",
            REGION,
            workflow_build_arn("build", "regional-flow", "2.0.0", 1),
        ),
        (
            "another build of a stored version",
            REGION,
            workflow_build_arn("build", "regional-flow", "1.0.0", 2),
        ),
        (
            "the stored build version, called from another Region",
            OTHER_REGION,
            stored.clone(),
        ),
    ];

    for (case, region, arn) in rows {
        let (status, answer) = delete_workflow(&router, region, &arn).await;
        assert_missing_workflow(case, status, &answer, &arn);
    }

    // Nothing above removed the stored workflow: its own Region still reads it, and still
    // deletes it.
    let case = "the stored build version in its own Region";
    let (status, read) = get_workflow(&router, REGION, &stored).await;
    let read = assert_ok(case, status, read);
    let reached = text(case, member(case, &read, "workflow"), "arn");
    assert_eq!(
        reached, stored,
        "case {case:?}: expected the stored workflow to be read, got {reached:?}"
    );
    let (status, deleted) = delete_workflow(&router, REGION, &stored).await;
    let deleted = assert_ok(case, status, deleted);
    let echoed = text(case, &deleted, "workflowBuildVersionArn");
    assert_eq!(
        echoed, stored,
        "case {case:?}: expected the removed ARN {stored:?}, got {echoed:?}"
    );
}

/// A workflow a stored image pipeline still references is deleted only once the pipeline is
/// gone — whichever form that reference takes, a reference carrying trailing `x` wildcards
/// keeping every build it admits in use.
#[tokio::test]
async fn a_workflow_a_pipeline_references_is_deleted_only_once_the_pipeline_is_gone() {
    let router = emulator();
    let build = Build::given(&router).await;

    // The rows differ along the FORM of the reference the pipeline holds: the build version
    // ARN itself, and each of the wildcard version ARNs a create reports. The forms a create
    // reports are read off one workflow and then applied to a workflow of each row's own, so
    // no row's delete is about another row's resource.
    let (first_arn, references) = given_workflow(&router, "Referenced Flow", "1.2.3").await;
    let mut forms: Vec<String> = vec![BUILD_VERSION_FORM.to_string()];
    forms.extend(
        references
            .as_object()
            .expect("the wildcard references are an object")
            .keys()
            .cloned(),
    );

    for (n, form) in forms.into_iter().enumerate() {
        let (arn, references) = given_workflow(&router, &format!("Referenced Flow {n}"), "1.2.3")
            .await;
        let reference = reference_in(&form, &arn, &references);
        let case = format!("a pipeline naming {form}");

        let pipeline = call(
            &router,
            "PUT",
            "/CreateImagePipeline",
            Some(&build.pipeline(
                &format!("Referenced Pipeline {n}"),
                &format!("referenced-pipeline-{n}"),
                &reference,
            )),
        )
        .await;
        let pipeline = assert_ok(&case, pipeline.0, pipeline.1);
        let pipeline = text(&case, &pipeline, "imagePipelineArn");

        // While the pipeline stands the delete is refused, and the workflow is still read.
        let (status, refused) = delete_workflow(&router, REGION, &arn).await;
        assert_depended_on(&case, status, &refused, &arn);
        let (status, read) = get_workflow(&router, REGION, &arn).await;
        let read = assert_ok(&case, status, read);
        let reached = text(&case, member(&case, &read, "workflow"), "arn");
        assert_eq!(
            reached, arn,
            "case {case:?}: expected the refused delete to leave {arn:?} stored, got {reached:?}"
        );

        // Once the pipeline is gone the workflow is removed.
        let (status, answer) = call(
            &router,
            "DELETE",
            &format!("/DeleteImagePipeline?imagePipelineArn={pipeline}"),
            None,
        )
        .await;
        assert_ok(&case, status, answer);

        let (status, deleted) = delete_workflow(&router, REGION, &arn).await;
        let deleted = assert_ok(&case, status, deleted);
        let echoed = text(&case, &deleted, "workflowBuildVersionArn");
        assert_eq!(
            echoed, arn,
            "case {case:?}: expected the removed ARN {arn:?}, got {echoed:?}"
        );
        let (status, read) = get_workflow(&router, REGION, &arn).await;
        assert_missing_workflow(&case, status, &read, &arn);
    }

    // The workflow the forms came from is referenced by no pipeline of its own, so it is
    // deletable all along.
    let case = "the workflow the reference forms came from";
    let (status, deleted) = delete_workflow(&router, REGION, &first_arn).await;
    let deleted = assert_ok(case, status, deleted);
    let echoed = text(case, &deleted, "workflowBuildVersionArn");
    assert_eq!(
        echoed, first_arn,
        "case {case:?}: expected the removed ARN {first_arn:?}, got {echoed:?}"
    );
}

/// An image BUILD VERSION is not a dependant: the workflows it records are a snapshot of what
/// that build ran, so the workflow is deleted while the build stands — and the build goes on
/// reporting the workflows it ran.
#[tokio::test]
async fn a_workflow_an_image_build_recorded_is_still_deletable() {
    let router = emulator();
    let build = Build::given(&router).await;
    let (arn, references) = given_workflow(&router, "Recorded Flow", "1.0.0").await;

    // The rows differ along the form of the reference the build recorded.
    let mut forms: Vec<String> = vec![BUILD_VERSION_FORM.to_string()];
    forms.extend(
        references
            .as_object()
            .expect("the wildcard references are an object")
            .keys()
            .cloned(),
    );

    let mut images: Vec<(String, String, String)> = Vec::new();
    for (n, form) in forms.into_iter().enumerate() {
        let reference = reference_in(&form, &arn, &references);
        let case = format!("an image build naming {form}");
        let answer = call(
            &router,
            "PUT",
            "/CreateImage",
            Some(&build.image(&format!("recorded-image-{n}"), &reference)),
        )
        .await;
        let answer = assert_ok(&case, answer.0, answer.1);
        let image = text(&case, &answer, "imageBuildVersionArn");
        images.push((case, reference, image));
    }

    // The delete succeeds while every one of those builds stands.
    let case = "the workflow every build recorded";
    let (status, deleted) = delete_workflow(&router, REGION, &arn).await;
    let deleted = assert_ok(case, status, deleted);
    let echoed = text(case, &deleted, "workflowBuildVersionArn");
    assert_eq!(
        echoed, arn,
        "case {case:?}: expected the removed ARN {arn:?}, got {echoed:?}"
    );

    // And each build goes on reporting the workflow reference it ran: the record is a
    // snapshot, not a reference the workflow had to outlive.
    for (case, reference, image) in images {
        let (status, read) = call(
            &router,
            "GET",
            &format!("/GetImage?imageBuildVersionArn={image}"),
            None,
        )
        .await;
        let read = assert_ok(&case, status, read);
        let recorded = member(&case, member(&case, &read, "image"), "workflows").clone();
        let actual = recorded
            .as_array()
            .and_then(|configurations| configurations.first())
            .map(|configuration| text(&case, configuration, "workflowArn"));
        assert_eq!(
            actual,
            Some(reference.clone()),
            "case {case:?}: expected the build to report the reference {reference:?}, got \
             {recorded}"
        );
    }
}
