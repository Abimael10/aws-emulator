// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the LABEL this service's missing-resource refusal names, over the
//! kinds of resource an ARN of this service can identify.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what a
//! signed AWS SDK request observes.
//!
//! The rule under test: a call that resolves one of this service's own ARNs to nothing in the
//! calling Region is refused with the not-found error the model defines for it
//! (`ResourceNotFoundException`, HTTP 404; `smithy_query(query="shape",shape="ResourceNotFoundException")`
//! gives the shape and no message text) and reports it as the required resource it is, naming
//! the KIND that ARN identifies: "The following required resource '<Kind>' cannot be found:
//! '<arn>'." — whichever route performed the lookup. The INFRASTRUCTURE configuration's own
//! read and delete are the recorded exception: they
//! quote the ARN alone, "The resource ARN '<arn>' was not found.", naming no kind.
//!
//! The rows span what the rule generalises over: the kind the ARN names (every kind of the
//! modelled `ImageBuilderArn` alternation whose read is implemented,
//! `smithy_query(query="shape",shape="ImageBuilderArn")`), the shape of that kind's resource
//! field (a plain name, a name with a version, a name with a version and a build number, a
//! staged workflow name), and — for the distribution configuration the second test follows
//! through its whole lifecycle — why the ARN resolves to nothing and which route looked it up.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call is signed for unless a case says otherwise.
const REGION: &str = "us-west-2";

/// A second Region, so a configuration of one Region can be asked for through another's
/// endpoint and a distribution can name a target Region of its own.
const OTHER_REGION: &str = "eu-west-1";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// The sentence a missing resource of this service is reported in: the kind its ARN identifies,
/// and the ARN the request carried. Spelled out here rather than taken from the crate, so a
/// reworded refusal fails.
fn kind_sentence(kind: &str, arn: &str) -> String {
    format!("The following required resource '{kind}' cannot be found: '{arn}'.")
}

/// The sentence the infrastructure configuration's own lookups carry: the ARN alone.
fn arn_sentence(arn: &str) -> String {
    format!("The resource ARN '{arn}' was not found.")
}

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
/// the header and the body are read as one.
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

/// Percent-encode one path segment, as an AWS SDK encodes a NON-greedy `@httpLabel`: the `:`
/// and `/` of an ARN travel as `%3A` and `%2F`, so the whole ARN stays ONE segment.
fn encode_label(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// The `message` a refusal carries.
fn error_message(case: &str, body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message in {body}"))
        .to_string()
}

/// Assert one call was refused as the modelled not-found error carrying `expected`.
async fn assert_reports_missing(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
    expected: &str,
) {
    let (status, answer) = call_in(router, region, method, uri, body).await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected HTTP 404, got {status} with {answer}"
    );
    let code = answer
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        code, "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {code:?} with {answer}"
    );
    let actual = error_message(case, &answer);
    assert_eq!(
        actual, expected,
        "case {case:?}: expected the message {expected:?}, got {actual:?}"
    );
}

/// An ARN of this service naming `resource` in `region`.
fn arn(region: &str, resource: &str) -> String {
    format!("arn:aws:imagebuilder:{region}:{ACCOUNT}:{resource}")
}

/// RULE: the read of an ARN this Region holds nothing under names the KIND that ARN identifies,
/// for every kind this service mints one for — the INFRASTRUCTURE configuration's own read being
/// the recorded exception, which quotes the ARN alone.
#[tokio::test]
async fn a_read_of_an_absent_arn_names_the_kind_the_arn_identifies() {
    let router = emulator();

    // (case, the read, the absent ARN, the sentence the answer has to carry). The resource
    // field of each ARN takes the form the modelled pattern for that kind spells out
    // (`smithy_query(query="shape",shape="ImageBuilderArn")`).
    let distribution = arn(REGION, "distribution-configuration/absent-dist");
    let infrastructure = arn(REGION, "infrastructure-configuration/absent-infra");
    let component = arn(REGION, "component/absent-component/1.0.0/1");
    let image_recipe = arn(REGION, "image-recipe/absent-recipe/1.0.0");
    let container_recipe = arn(REGION, "container-recipe/absent-boxed/1.0.0");
    let pipeline = arn(REGION, "image-pipeline/absent-pipeline");
    let image = arn(REGION, "image/absent-image/1.0.0/1");
    let workflow = arn(REGION, "workflow/build/absent-workflow/1.0.0/1");

    let rows: Vec<(&str, String, String)> = vec![
        (
            "a distribution configuration nothing created",
            format!("/GetDistributionConfiguration?distributionConfigurationArn={distribution}"),
            kind_sentence("DistributionConfiguration", &distribution),
        ),
        (
            "a component build version nothing created",
            format!("/GetComponent?componentBuildVersionArn={component}"),
            kind_sentence("Component", &component),
        ),
        (
            "an image recipe nothing created",
            format!("/GetImageRecipe?imageRecipeArn={image_recipe}"),
            kind_sentence("ImageRecipe", &image_recipe),
        ),
        (
            "a container recipe nothing created",
            format!("/GetContainerRecipe?containerRecipeArn={container_recipe}"),
            kind_sentence("ContainerRecipe", &container_recipe),
        ),
        (
            "an image pipeline nothing created",
            format!("/GetImagePipeline?imagePipelineArn={pipeline}"),
            kind_sentence("ImagePipeline", &pipeline),
        ),
        (
            "an image build version nothing created",
            format!("/GetImage?imageBuildVersionArn={image}"),
            kind_sentence("Image", &image),
        ),
        (
            "a workflow build version nothing created",
            format!("/GetWorkflow?workflowBuildVersionArn={workflow}"),
            kind_sentence("Workflow", &workflow),
        ),
        (
            "an infrastructure configuration nothing created",
            format!("/GetInfrastructureConfiguration?infrastructureConfigurationArn={infrastructure}"),
            arn_sentence(&infrastructure),
        ),
    ];

    for (case, uri, expected) in &rows {
        assert_reports_missing(&router, case, REGION, "GET", uri, None, expected).await;
    }
}

/// One `distributions` entry naming `region` as its target, with the AMI settings that are the
/// least an entry may carry.
fn entry(region: &str) -> Value {
    json!({ "region": region, "amiDistributionConfiguration": {} })
}

/// `CreateDistributionConfiguration` for `name` with `distributions`, answering the minted ARN.
async fn create_distribution(
    router: &MultiServiceRouter,
    name: &str,
    distributions: Value,
) -> String {
    let case = format!("create the distribution configuration {name}");
    let body = json!({
        "name": name,
        "clientToken": format!("create-{name}"),
        "distributions": distributions,
    });
    let (status, answer) = call_in(
        router,
        REGION,
        "PUT",
        "/CreateDistributionConfiguration",
        Some(&body),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {answer}"
    );
    answer
        .get("distributionConfigurationArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected the minted ARN in {answer}"))
        .to_string()
}

/// RULE applied to the whole lifecycle of a distribution configuration: while it is stored
/// every read answers it, and once it is gone every route that looks it up reports it missing,
/// each naming the kind its ARN identifies.
#[tokio::test]
async fn every_lookup_of_a_gone_distribution_configuration_reports_it_missing() {
    let router = emulator();

    // The rows differ in the form of the configuration name and in the distributions list the
    // configuration carried while it lived (one target Region, two, none at all).
    let rows: Vec<(&str, &str, Value)> = vec![
        ("one target Region", "lifecycle-one", json!([entry(REGION)])),
        (
            "a second Region added by the update",
            "lifecycle_two_2",
            json!([entry(REGION), entry(OTHER_REGION)]),
        ),
        ("no target Region at all", "l33", json!([])),
    ];

    for (row, name, distributions) in rows {
        let stored = create_distribution(&router, name, json!([entry(REGION)])).await;

        // It reads back while it is stored, so the refusals below report an ARN that resolved
        // to something a moment earlier.
        let read = format!("/GetDistributionConfiguration?distributionConfigurationArn={stored}");
        let (status, answer) = call_in(&router, REGION, "GET", &read, None).await;
        assert_eq!(
            status, 200,
            "case {row:?}: expected HTTP 200 reading it back, got {status} with {answer}"
        );

        // The update the issue's trace performs: the distributions list is replaced, and the
        // read still answers.
        let (status, answer) = call_in(
            &router,
            REGION,
            "PUT",
            "/UpdateDistributionConfiguration",
            Some(&json!({
                "distributionConfigurationArn": stored,
                "clientToken": format!("update-{name}"),
                "distributions": distributions,
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {row:?}: expected the update to be accepted, got {status} with {answer}"
        );
        let (status, answer) = call_in(&router, REGION, "GET", &read, None).await;
        assert_eq!(
            status, 200,
            "case {row:?}: expected HTTP 200 after the update, got {status} with {answer}"
        );

        let (status, answer) = call_in(
            &router,
            REGION,
            "DELETE",
            &format!("/DeleteDistributionConfiguration?distributionConfigurationArn={stored}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {row:?}: expected the delete to succeed, got {status} with {answer}"
        );

        // (which route looks it up, method, target, body, the sentence it carries)
        let kind = kind_sentence("DistributionConfiguration", &stored);
        let lookups: Vec<(&str, &str, String, Option<Value>, String)> = vec![
            (
                "GetDistributionConfiguration",
                "GET",
                read.clone(),
                None,
                kind.clone(),
            ),
            (
                "DeleteDistributionConfiguration",
                "DELETE",
                format!("/DeleteDistributionConfiguration?distributionConfigurationArn={stored}"),
                None,
                kind.clone(),
            ),
            (
                "UpdateDistributionConfiguration",
                "PUT",
                "/UpdateDistributionConfiguration".to_string(),
                Some(json!({
                    "distributionConfigurationArn": stored,
                    "clientToken": format!("after-{name}"),
                    "distributions": [entry(REGION)],
                })),
                kind.clone(),
            ),
            (
                "TagResource",
                "POST",
                format!("/tags/{}", encode_label(&stored)),
                Some(json!({ "tags": { "owner": "platform" } })),
                kind,
            ),
        ];

        for (operation, method, uri, body, expected) in lookups {
            let case = format!("{operation} after the delete of the configuration with {row}");
            assert_reports_missing(
                &router,
                &case,
                REGION,
                method,
                &uri,
                body.as_ref(),
                &expected,
            )
            .await;
        }
    }
}
