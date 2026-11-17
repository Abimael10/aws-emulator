// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the consecutive-failure count an image pipeline reports.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what
//! a signed AWS SDK request observes.
//!
//! The rule under test: `consecutiveFailures` is the count Image Builder "tracks ... for
//! scheduled pipeline executions"
//! (`smithy_query(query="doc",shape="ImagePipeline",member="consecutiveFailures")`), so a
//! pipeline no scheduled execution has run for carries no count at all and a read of it
//! reports no such member — which is what tells "never ran on its schedule" from "ran and has
//! zero consecutive failures". A run a caller starts BY HAND is not a scheduled execution
//! ("if the pipeline runs manually and fails, the count remains the same"), so it tracks
//! nothing either.
//!
//! The rows span what the rule generalises over: whether the pipeline carries a schedule at
//! all, whether it is enabled, whether an update has replaced it and whether a caller has
//! started a build of it by hand. The listing is read beside the pipeline's own read, since a
//! listing entry is the same view.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// A build component document: one `build` phase, which is all the recipe these cases name
/// needs.
const BUILD_DOCUMENT: &str = "name: HelloWorld\nschemaVersion: 1.0\nphases:\n  - name: build\n    \
                              steps:\n      - name: Greet\n        action: ExecuteBash\n        \
                              inputs:\n          commands:\n            - echo hello\n";

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

/// Status and parsed body of one call.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let response = router.dispatch(request(method, uri, body)).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    (status, parsed)
}

/// Assert a call succeeded, and answer its body.
fn assert_ok(case: &str, status: u16, body: Value) -> Value {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {body}"
    );
    body
}

/// The text one member of a response carries.
fn text(case: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected text in {member} of {body}"))
        .to_string()
}

/// The resources a pipeline needs: one component, one recipe built from it and one
/// infrastructure configuration, answered as (recipe ARN, infrastructure ARN).
async fn pipeline_dependencies(router: &MultiServiceRouter) -> (String, String) {
    let component = {
        let body = json!({
            "name": "failures-component",
            "semanticVersion": "1.0.0",
            "platform": "Linux",
            "clientToken": "token-component",
            "data": BUILD_DOCUMENT,
        });
        let (status, answer) = call(router, "PUT", "/CreateComponent", Some(&body)).await;
        let answer = assert_ok("create the component", status, answer);
        text("create the component", &answer, "componentBuildVersionArn")
    };
    let recipe = {
        let body = json!({
            "name": "failures-recipe",
            "semanticVersion": "1.0.0",
            "parentImage": "ami-0123456789abcdef0",
            "clientToken": "token-recipe",
            "components": [{ "componentArn": component }],
        });
        let (status, answer) = call(router, "PUT", "/CreateImageRecipe", Some(&body)).await;
        let answer = assert_ok("create the image recipe", status, answer);
        text("create the image recipe", &answer, "imageRecipeArn")
    };
    let infrastructure = {
        let body = json!({
            "name": "failures-infra",
            "clientToken": "token-infra",
            "instanceProfileName": "EC2InstanceProfileForImageBuilder",
        });
        let (status, answer) =
            call(router, "PUT", "/CreateInfrastructureConfiguration", Some(&body)).await;
        let answer = assert_ok("create the infrastructure configuration", status, answer);
        text(
            "create the infrastructure configuration",
            &answer,
            "infrastructureConfigurationArn",
        )
    };
    (recipe, infrastructure)
}

/// `GetImagePipeline` for `arn`, answering the `imagePipeline` it reports.
async fn get_pipeline(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, answer) = call(
        router,
        "GET",
        &format!("/GetImagePipeline?imagePipelineArn={arn}"),
        None,
    )
    .await;
    let answer = assert_ok(case, status, answer);
    answer
        .get("imagePipeline")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an imagePipeline in {answer}"))
}

/// The entry `ListImagePipelines` reports for `arn`.
async fn listed_pipeline(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, answer) = call(router, "POST", "/ListImagePipelines", Some(&json!({}))).await;
    let answer = assert_ok(case, status, answer);
    answer
        .get("imagePipelineList")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected an imagePipelineList in {answer}"))
        .iter()
        .find(|entry| entry.get("arn").and_then(Value::as_str) == Some(arn))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected {arn} in the listing {answer}"))
}

/// RULE: a pipeline no scheduled execution has run for reports no consecutive-failure count,
/// however it is configured and whatever a caller has done to it by hand.
#[tokio::test]
async fn a_pipeline_no_schedule_has_run_reports_no_failure_count() {
    let router = emulator();
    let (recipe, infrastructure) = pipeline_dependencies(&router).await;

    // (case, the members the create carried beside the required ones)
    let rows: Vec<(&str, Value)> = vec![
        ("no schedule at all", json!({})),
        (
            "a daily schedule",
            json!({ "schedule": {"scheduleExpression": "cron(0 9 * * ? *)"} }),
        ),
        (
            "a schedule on a disabled pipeline",
            json!({
                "schedule": {"scheduleExpression": "rate(1 day)"},
                "status": "DISABLED",
            }),
        ),
        (
            "a schedule with a timezone and a start condition",
            json!({
                "schedule": {
                    "scheduleExpression": "cron(0 3 ? * SUN *)",
                    "timezone": "Etc/UTC",
                    "pipelineExecutionStartCondition": "EXPRESSION_MATCH_ONLY",
                },
            }),
        ),
    ];

    for (index, (case, extra)) in rows.into_iter().enumerate() {
        let name = format!("failures-pipeline-{index}");
        let mut body = json!({
            "name": name,
            "clientToken": format!("token-pipeline-{index}"),
            "imageRecipeArn": recipe,
            "infrastructureConfigurationArn": infrastructure,
        });
        if let (Some(members), Value::Object(extra)) = (body.as_object_mut(), extra) {
            for (member, value) in extra {
                members.insert(member, value);
            }
        }
        let (status, created) = call(&router, "PUT", "/CreateImagePipeline", Some(&body)).await;
        let created = assert_ok(case, status, created);
        let arn = text(case, &created, "imagePipelineArn");

        let pipeline = get_pipeline(&router, case, &arn).await;
        assert_eq!(
            pipeline.get("consecutiveFailures"),
            None,
            "case {case:?}: expected no consecutive-failure count before any scheduled \
             execution, got {pipeline}"
        );
        // A listing entry is the same view, so the two cannot disagree about one pipeline.
        let listed = listed_pipeline(&router, case, &arn).await;
        assert_eq!(
            listed.get("consecutiveFailures"),
            None,
            "case {case:?}: expected the listing to report no count either, got {listed}"
        );

        // An update replaces the pipeline, and a build a caller starts BY HAND is not a
        // scheduled execution: neither tracks a count.
        let update = json!({
            "imagePipelineArn": arn,
            "clientToken": format!("token-update-{index}"),
            "imageRecipeArn": recipe,
            "infrastructureConfigurationArn": infrastructure,
            "description": "updated by hand",
        });
        let (status, updated) = call(&router, "PUT", "/UpdateImagePipeline", Some(&update)).await;
        assert_ok(case, status, updated);
        let pipeline = get_pipeline(&router, case, &arn).await;
        assert_eq!(
            pipeline.get("consecutiveFailures"),
            None,
            "case {case:?}: expected no count after an update, got {pipeline}"
        );

        let start = json!({
            "imagePipelineArn": arn,
            "clientToken": format!("token-run-{index}"),
        });
        let (status, started) = call(
            &router,
            "PUT",
            "/StartImagePipelineExecution",
            Some(&start),
        )
        .await;
        assert_ok(case, status, started);
        let pipeline = get_pipeline(&router, case, &arn).await;
        assert_eq!(
            pipeline.get("consecutiveFailures"),
            None,
            "case {case:?}: expected a run started by hand to track no scheduled failures, \
             got {pipeline}"
        );
        // The run IS reported, so the absent count is not an absent read: the pipeline knows
        // its last run.
        assert!(
            pipeline.get("dateLastRun").is_some(),
            "case {case:?}: expected the started run to be reported in {pipeline}"
        );
    }
}
