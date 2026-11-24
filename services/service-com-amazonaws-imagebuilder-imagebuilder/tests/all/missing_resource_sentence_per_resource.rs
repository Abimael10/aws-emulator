// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for WHICH missing-resource sentence this service reports an ARN that
//! resolves to nothing in, over the kinds of resource one of its ARNs can identify.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what
//! a signed AWS SDK request observes.
//!
//! The rule under test: which of this service's two not-found sentences a refusal carries
//! follows the RESOURCE the ARN identifies, not the route that looked it up. Under the error
//! the model defines for a resource the service does not hold
//! (`smithy_query(query="shape",shape="ResourceNotFoundException")`: client error, HTTP 404,
//! one `message` member), every resource of this service is reported as the required resource
//! it is, naming the kind its ARN identifies — "The following required resource '<Kind>'
//! cannot be found: '<arn>'." — on every route that resolves it; the INFRASTRUCTURE
//! configuration's own read and delete are the one recorded exception, quoting the ARN by
//! itself: "The resource ARN '<arn>' was not found."
//!
//! The rows span what the rule generalises over: the kind the ARN names (the alternation of
//! the modelled `ImageBuilderArn` pattern, `smithy_query(query="shape",shape="ImageBuilderArn")`),
//! the shape of that kind's resource field (a plain name, a name with a version, a name with a
//! version and a build number, a staged workflow name), which route performs the lookup (a
//! read, a delete, an update, a tagging write, a tagging delete) and why the ARN resolves to
//! nothing (nothing ever created it, it was deleted, it is stored in another Region than the
//! endpoint the call reached).

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call is signed for unless a row says otherwise.
const REGION: &str = "us-west-2";

/// A second Region, so a stored configuration of one Region can be asked for through
/// another's endpoint.
const OTHER_REGION: &str = "eu-west-1";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// Which of this service's two missing-resource sentences a report has to carry.
#[derive(Clone, Copy, Debug)]
enum Sentence {
    /// The kind-naming sentence every resource of this service is reported in.
    Kind,
    /// The ARN-only sentence the INFRASTRUCTURE configuration's own read and delete carry.
    Arn,
}

/// The whole message `sentence` requires of a report about `kind` and `arn`. Spelled out here
/// rather than taken from the crate, so a reworded refusal fails.
fn expected_message(sentence: Sentence, kind: &str, arn: &str) -> String {
    match sentence {
        Sentence::Kind => {
            format!("The following required resource '{kind}' cannot be found: '{arn}'.")
        }
        Sentence::Arn => format!("The resource ARN '{arn}' was not found."),
    }
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

/// The text one member of an answer carries.
fn text(case: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected text in {member} of {body}"))
        .to_string()
}

/// One route that looks a resource up: how to send it, and the sentence its report has to
/// carry.
struct Route {
    /// The operation this route calls, named in every assertion it makes.
    operation: &'static str,
    /// The HTTP method the modelled operation binds.
    method: &'static str,
    /// The request target, the ARN included where the operation takes it in the URI.
    uri: String,
    /// The request body, for an operation that takes the ARN in one.
    body: Option<Value>,
    /// Which of the two sentences this route's report has to carry.
    sentence: Sentence,
}

/// An ARN of this service naming `resource` in `region`.
fn arn_in(region: &str, resource: &str) -> String {
    format!("arn:aws:imagebuilder:{region}:{ACCOUNT}:{resource}")
}

/// The idempotency token a mutating row carries: inside the modelled `ClientToken` bound of 64
/// characters, so what the row observes is the lookup rather than a constraint refusal.
fn token(prefix: &str, arn: &str) -> String {
    let name = arn.rsplit('/').next().unwrap_or(arn);
    format!("{prefix}-{name}")
}

/// Every route that resolves the `kind` resource `arn`, with the sentence each one's report
/// has to carry. The tagging pair takes an ARN of ANY kind, so it is where every kind reaches
/// a lookup; the other routes are the ones the model binds for that kind.
fn routes_of(kind: &str, region: &str, arn: &str) -> Vec<Route> {
    let mut routes = match kind {
        "Component" => vec![
            Route {
                operation: "GetComponent",
                method: "GET",
                uri: format!("/GetComponent?componentBuildVersionArn={arn}"),
                body: None,
                sentence: Sentence::Kind,
            },
            Route {
                operation: "DeleteComponent",
                method: "DELETE",
                uri: format!("/DeleteComponent?componentBuildVersionArn={arn}"),
                body: None,
                sentence: Sentence::Kind,
            },
        ],
        "Image" => vec![Route {
            operation: "GetImage",
            method: "GET",
            uri: format!("/GetImage?imageBuildVersionArn={arn}"),
            body: None,
            sentence: Sentence::Kind,
        }],
        "ImageRecipe" => vec![
            Route {
                operation: "GetImageRecipe",
                method: "GET",
                uri: format!("/GetImageRecipe?imageRecipeArn={arn}"),
                body: None,
                sentence: Sentence::Kind,
            },
            Route {
                operation: "DeleteImageRecipe",
                method: "DELETE",
                uri: format!("/DeleteImageRecipe?imageRecipeArn={arn}"),
                body: None,
                sentence: Sentence::Kind,
            },
        ],
        "ContainerRecipe" => vec![Route {
            operation: "GetContainerRecipe",
            method: "GET",
            uri: format!("/GetContainerRecipe?containerRecipeArn={arn}"),
            body: None,
            sentence: Sentence::Kind,
        }],
        "ImagePipeline" => vec![
            Route {
                operation: "GetImagePipeline",
                method: "GET",
                uri: format!("/GetImagePipeline?imagePipelineArn={arn}"),
                body: None,
                sentence: Sentence::Kind,
            },
            Route {
                operation: "DeleteImagePipeline",
                method: "DELETE",
                uri: format!("/DeleteImagePipeline?imagePipelineArn={arn}"),
                body: None,
                sentence: Sentence::Kind,
            },
        ],
        "Workflow" => vec![Route {
            operation: "GetWorkflow",
            method: "GET",
            uri: format!("/GetWorkflow?workflowBuildVersionArn={arn}"),
            body: None,
            sentence: Sentence::Kind,
        }],
        "DistributionConfiguration" => vec![
            Route {
                operation: "GetDistributionConfiguration",
                method: "GET",
                uri: format!("/GetDistributionConfiguration?distributionConfigurationArn={arn}"),
                body: None,
                sentence: Sentence::Kind,
            },
            Route {
                operation: "DeleteDistributionConfiguration",
                method: "DELETE",
                uri: format!(
                    "/DeleteDistributionConfiguration?distributionConfigurationArn={arn}"
                ),
                body: None,
                sentence: Sentence::Kind,
            },
            Route {
                operation: "UpdateDistributionConfiguration",
                method: "PUT",
                uri: "/UpdateDistributionConfiguration".to_string(),
                body: Some(json!({
                    "distributionConfigurationArn": arn,
                    "clientToken": token("update-dist", arn),
                    "distributions": [{ "region": region, "amiDistributionConfiguration": {} }],
                })),
                sentence: Sentence::Kind,
            },
        ],
        "InfrastructureConfiguration" => vec![
            Route {
                operation: "GetInfrastructureConfiguration",
                method: "GET",
                uri: format!(
                    "/GetInfrastructureConfiguration?infrastructureConfigurationArn={arn}"
                ),
                body: None,
                sentence: Sentence::Arn,
            },
            Route {
                operation: "DeleteInfrastructureConfiguration",
                method: "DELETE",
                uri: format!(
                    "/DeleteInfrastructureConfiguration?infrastructureConfigurationArn={arn}"
                ),
                body: None,
                sentence: Sentence::Arn,
            },
            Route {
                operation: "UpdateInfrastructureConfiguration",
                method: "PUT",
                uri: "/UpdateInfrastructureConfiguration".to_string(),
                body: Some(json!({
                    "infrastructureConfigurationArn": arn,
                    "clientToken": token("update-infra", arn),
                    "instanceProfileName": "EC2InstanceProfileForImageBuilder",
                })),
                sentence: Sentence::Kind,
            },
        ],
        "LifecyclePolicy" => Vec::new(),
        other => panic!("no route is declared for the resource kind {other:?}"),
    };
    routes.push(Route {
        operation: "TagResource",
        method: "POST",
        uri: format!("/tags/{}", encode_label(arn)),
        body: Some(json!({ "tags": { "owner": "platform" } })),
        sentence: Sentence::Kind,
    });
    routes.push(Route {
        operation: "UntagResource",
        method: "DELETE",
        uri: format!("/tags/{}?tagKeys=owner", encode_label(arn)),
        body: None,
        sentence: Sentence::Kind,
    });
    routes
}

/// Assert every route of `kind` reports `arn` missing, each in the sentence that resource
/// carries on that route.
async fn assert_every_route_reports_it_missing(
    router: &MultiServiceRouter,
    row: &str,
    region: &str,
    kind: &str,
    arn: &str,
) {
    for route in routes_of(kind, region, arn) {
        let case = format!("{} on {row}", route.operation);
        let (status, answer) =
            call_in(router, region, route.method, &route.uri, route.body.as_ref()).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} with {answer}"
        );
        let code = error_code(&case, &answer);
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {code:?} with {answer}"
        );
        let expected = expected_message(route.sentence, kind, arn);
        let actual = text(&case, &answer, "message");
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the message {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: an ARN this Region holds nothing under is reported as the required resource it is,
/// naming the kind that ARN identifies, on every route that resolves it — for every kind an
/// ARN of this service may name — while the infrastructure configuration's own read and delete
/// quote the ARN alone.
#[tokio::test]
async fn every_route_reports_a_missing_resource_in_the_sentence_that_resource_carries() {
    let router = emulator();

    // (row, the kind the ARN names, the resource field of the ARN). The kinds are the
    // alternation of the modelled `ImageBuilderArn` pattern
    // (`smithy_query(query="shape",shape="ImageBuilderArn")`), each paired with the name this
    // model gives that kind — which is the token a report names — and with a resource field of
    // the form that kind's own ARN pattern spells out.
    let rows: &[(&str, &str, &str)] = &[
        (
            "a component build version nothing created",
            "Component",
            "component/absent-component/1.0.0/1",
        ),
        (
            "an image build version nothing created",
            "Image",
            "image/absent-image/1.0.0/1",
        ),
        (
            "an image recipe nothing created",
            "ImageRecipe",
            "image-recipe/absent-recipe/1.0.0",
        ),
        (
            "a container recipe nothing created",
            "ContainerRecipe",
            "container-recipe/absent-boxed/1.0.0",
        ),
        (
            "an image pipeline nothing created",
            "ImagePipeline",
            "image-pipeline/absent-pipeline",
        ),
        (
            "a build workflow nothing created",
            "Workflow",
            "workflow/build/absent-workflow/1.0.0/1",
        ),
        (
            "a distribution configuration nothing created",
            "DistributionConfiguration",
            "distribution-configuration/absent-dist",
        ),
        (
            "a one-token distribution configuration name nothing created",
            "DistributionConfiguration",
            "distribution-configuration/absent_dist_2",
        ),
        (
            "an infrastructure configuration nothing created",
            "InfrastructureConfiguration",
            "infrastructure-configuration/absent-infra",
        ),
        (
            "a lifecycle policy nothing created",
            "LifecyclePolicy",
            "lifecycle-policy/absent-policy",
        ),
    ];

    for (row, kind, resource) in rows {
        let arn = arn_in(REGION, resource);
        assert_every_route_reports_it_missing(&router, row, REGION, kind, &arn).await;
    }
}

/// `CreateDistributionConfiguration` for `name` in `region`, answering the ARN it minted.
async fn create_distribution(router: &MultiServiceRouter, region: &str, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("dist-{region}-{name}"),
        "distributions": [{ "region": region, "amiDistributionConfiguration": {} }],
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
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {answer}"
    );
    text(&case, &answer, "distributionConfigurationArn")
}

/// `CreateInfrastructureConfiguration` for `name` in `region`, answering the ARN it minted.
async fn create_infrastructure(router: &MultiServiceRouter, region: &str, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("infra-{region}-{name}"),
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
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with {answer}"
    );
    text(&case, &answer, "infrastructureConfigurationArn")
}

/// RULE applied to the two CONFIGURATION families side by side, over the whole lifecycle of
/// one configuration of each: while it is stored its read answers it, and once it is gone — or
/// asked for through another Region's endpoint — every route reports it missing in the sentence
/// that family carries, the distribution configuration naming its kind on its READ as on every
/// other route.
#[tokio::test]
async fn the_two_configuration_families_keep_their_own_sentence_through_their_lifecycle() {
    // (the kind, how one is created, how one is read, how one is deleted)
    for (kind, read, delete) in [
        (
            "DistributionConfiguration",
            "/GetDistributionConfiguration?distributionConfigurationArn=",
            "/DeleteDistributionConfiguration?distributionConfigurationArn=",
        ),
        (
            "InfrastructureConfiguration",
            "/GetInfrastructureConfiguration?infrastructureConfigurationArn=",
            "/DeleteInfrastructureConfiguration?infrastructureConfigurationArn=",
        ),
    ] {
        let router = emulator();

        // The rows differ in the form of the configuration name, which the modelled ARN
        // patterns admit as `[a-z0-9-_]+`, so the ARN each sentence below carries differs
        // between them and cannot be a constant a handler holds.
        for name in ["kept-one", "kept_two_2", "k33"] {
            let stored = if kind == "DistributionConfiguration" {
                create_distribution(&router, REGION, name).await
            } else {
                create_infrastructure(&router, REGION, name).await
            };

            // It reads back while it is stored, so the refusals below report an ARN that
            // resolved to something a moment earlier.
            let case = format!("{kind} {name} read back after its create");
            let (status, answer) =
                call_in(&router, REGION, "GET", &format!("{read}{stored}"), None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with {answer}"
            );

            // A stored configuration is held by ONE Region, so the same ARN reached through
            // another Region's endpoint resolves to nothing — and is reported in the same
            // sentence as one that was deleted.
            assert_every_route_reports_it_missing(
                &router,
                &format!("the stored {kind} {name} reached through another Region's endpoint"),
                OTHER_REGION,
                kind,
                &stored,
            )
            .await;

            let case = format!("the delete of {kind} {name}");
            let (status, answer) =
                call_in(&router, REGION, "DELETE", &format!("{delete}{stored}"), None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with {answer}"
            );

            assert_every_route_reports_it_missing(
                &router,
                &format!("{kind} {name} after its delete"),
                REGION,
                kind,
                &stored,
            )
            .await;
        }
    }
}
