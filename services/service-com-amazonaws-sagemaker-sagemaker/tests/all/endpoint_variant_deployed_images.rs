// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for what a HOSTED model's deployment reports about the compute and
//! the container images behind it: `DescribeEndpoint`'s `ProductionVariants` entries and
//! `DescribeInferenceComponent`'s container summary.
//!
//! Two RULES, each asserted over a TABLE of inputs rather than one recorded call:
//!
//! * every variant summary of a created endpoint carries `CurrentInstanceCount` — "the
//!   number of instances associated with the variant" (`TaskCount`, `@range min 0`), which is
//!   what the variant's configuration provisions and zero for a variant that provisions none;
//! * wherever a response reports the inference images a deployment is serving
//!   (`DeployedImage`), it reports one entry per container of what is deployed, carrying the
//!   image path as SPECIFIED, that path at the digest it RESOLVED to — the documented
//!   `registry/repository[@digest]` form, the digest coming from the registry that holds the
//!   image where one does — and the INSTANT of that resolution; and the resolution is made
//!   once, by the create, so every later read replays it.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`, `awsJson1_1`,
//! and Amazon ECR's own target form) through the same `MultiServiceRouter` the production
//! router uses, with Amazon ECR mounted on the SAME engine — the digest of a referenced image
//! belongs to the registry holding it, so a push and a resolution have to meet in one store.
//!
//! Nothing here is weighed against how fast this host ran it: a deployment is waited for by
//! looking again until the endpoint reports what was asked of it, under a deadline far longer
//! than the span this state gives a deployment.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The canonical account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// How long a deployment is polled for before a case fails, and how often it is looked at.
/// What is waited for is the CONDITION, re-read in a loop, so a loaded host cannot fail a
/// case the clock would have served.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const SETTLE_POLL: Duration = Duration::from_millis(20);

/// SageMaker and Amazon ECR on ONE shared engine, so an image reference SageMaker resolves is
/// resolved against the registry a push really reached.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These tests exercise handler behaviour, not SigV4; a request below carries a
        // credential scope only so the Region is the one it names.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_sagemaker_sagemaker::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
                engine,
            ),
        );
        Self { router }
    }

    /// One SageMaker call, with whatever that service answered.
    async fn sagemaker(&self, target: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("SageMaker.{target}"))
            .header("authorization", authorization("sagemaker"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One SageMaker call that must be served.
    async fn ok(&self, case: &str, target: &str, body: &Value) -> Value {
        let (status, answered) = self.sagemaker(target, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {target}, got {status} with {answered}"
        );
        answered
    }

    /// One Amazon ECR call, over that service's own `awsJson1_1` target.
    async fn ecr(&self, operation: &str, body: &Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("api.ecr.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header(
                "x-amz-target",
                format!("AmazonEC2ContainerRegistry_V20150921.{operation}"),
            )
            .header("authorization", authorization("ecr"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, answered)
    }

    /// Push a manifest to `repository` under `tag` through Amazon ECR's OWN
    /// `CreateRepository`/`PutImage`, and answer the manifest digest that registry minted.
    ///
    /// The manifest declares no layer and no configuration blob, so the push needs no layer
    /// upload behind it; `content` is what makes two pushes two different images.
    async fn push_image(&self, case: &str, repository: &str, tag: &str, content: &str) -> String {
        let manifest = json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
            "layers": [],
            "annotations": { "org.opencontainers.image.ref.name": content },
        })
        .to_string();
        // A repository a push reached before is the one this push reaches too, so a second
        // create being refused is not a failure of the case.
        let _ = self
            .ecr("CreateRepository", &json!({"repositoryName": repository}))
            .await;
        let (status, pushed) = self
            .ecr(
                "PutImage",
                &json!({
                    "repositoryName": repository,
                    "imageTag": tag,
                    "imageManifest": manifest,
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the push of {repository}:{tag} must be served, got {status} with \
             {pushed}"
        );
        pushed
            .pointer("/image/imageId/imageDigest")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a pushed digest, got {pushed}"))
            .to_string()
    }

    /// Read `endpoint` until it reports `wanted`, answering that reading. The wait is on the
    /// CONDITION, so nothing here depends on how fast this host runs.
    async fn poll_until(&self, case: &str, endpoint: &str, wanted: &str) -> Value {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let described = self
                .ok(case, "DescribeEndpoint", &json!({"EndpointName": endpoint}))
                .await;
            if described.get("EndpointStatus").and_then(Value::as_str) == Some(wanted) {
                return described;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the endpoint {endpoint} must report {wanted}, last read \
                 {described}"
            );
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }
}

/// A SigV4 credential scope naming `service`: it is what routes a request to that service.
/// The key id and signature are not credentials — this harness turns request authentication
/// off, so nothing is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// The repository part of an image reference: everything before the `:tag` or `@digest` of its
/// LAST path component, so a registry host carrying a port keeps it.
///
/// Written here independently of the service's own reading of a reference, so what the
/// assertions compare against is not the code under test.
fn repository_of(image: &str) -> String {
    let (prefix, last) = match image.rsplit_once('/') {
        Some((prefix, last)) => (format!("{prefix}/"), last),
        None => (String::new(), image),
    };
    let repository = match last.split_once(['@', ':']) {
        Some((repository, _)) => repository,
        None => last,
    };
    format!("{prefix}{repository}")
}

/// Whether `digest` is in the form every reported image digest carries — the `ImageDigest`
/// shape's modelled pattern (`smithy_query(query="shape",shape="ImageDigest")`:
/// `^[Ss][Hh][Aa]256:[0-9a-fA-F]{64}$`).
fn is_image_digest(digest: &str) -> bool {
    match digest.split_once(':') {
        Some((algorithm, hex)) => {
            algorithm.eq_ignore_ascii_case("sha256")
                && hex.len() == 64
                && hex.chars().all(|digit| digit.is_ascii_hexdigit())
        }
        None => false,
    }
}

/// The digest the entry `deployed` resolved its image to, taken off its `ResolvedImage`, with
/// every member of the entry checked against the rule first: the specified path is `image`,
/// the resolved path is that path's repository at a digest in the modelled form, and the
/// resolution instant is reported.
fn asserted_digest(case: &str, deployed: &Value, image: &str) -> String {
    assert_eq!(
        deployed.get("SpecifiedImage").and_then(Value::as_str),
        Some(image),
        "case {case:?}: expected SpecifiedImage {image:?}, got {deployed}"
    );
    let resolved = deployed
        .get("ResolvedImage")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a ResolvedImage, got {deployed}"));
    let repository = repository_of(image);
    let digest = resolved
        .strip_prefix(&format!("{repository}@"))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected ResolvedImage {repository}@<digest>, got {resolved:?}"
            )
        });
    assert!(
        is_image_digest(digest),
        "case {case:?}: expected a sha256 digest in {resolved:?}, got {digest:?}"
    );
    assert!(
        deployed.get("ResolutionTime").and_then(Value::as_f64).is_some(),
        "case {case:?}: expected a ResolutionTime, got {deployed}"
    );
    digest.to_string()
}

/// The `DeployedImages` of the first production variant of a describe answer, and `None` when
/// the member is absent.
fn deployed_images(described: &Value) -> Option<Vec<Value>> {
    described
        .pointer("/ProductionVariants/0/DeployedImages")
        .and_then(Value::as_array)
        .cloned()
}

/// `CurrentInstanceCount` of the first production variant, and `None` when it is absent.
fn current_instance_count(described: &Value) -> Option<i64> {
    described
        .pointer("/ProductionVariants/0/CurrentInstanceCount")
        .and_then(Value::as_i64)
}

/// A `CreateModel` body hosting `images` — one container per entry, the first as
/// `PrimaryContainer` and the rest as `Containers`; an entry of `None` is a container that
/// names no image at all.
fn model_body(name: &str, images: &[Option<&str>]) -> Value {
    let container = |image: &Option<&str>| match image {
        Some(image) => json!({ "Image": image }),
        None => json!({ "Environment": { "MODE": "none" } }),
    };
    let mut body = json!({ "ModelName": name });
    if let Some((primary, rest)) = images.split_first() {
        body["PrimaryContainer"] = container(primary);
        if !rest.is_empty() {
            body["Containers"] = Value::Array(rest.iter().map(container).collect());
        }
    }
    body
}

/// One row of the table below: a variant hosting a model whose containers are `images` (an
/// entry of `None` being a container that names no image at all), provisioning `instances`
/// instances or serverless. `images` of `None` is a variant that hosts no model.
struct VariantRow {
    case: &'static str,
    images: Option<Vec<Option<String>>>,
    instances: Option<i64>,
    serverless: bool,
}

/// RULE: every variant summary of a created endpoint reports `CurrentInstanceCount` — the
/// instances the variant provisions, and 0 for a variant that provisions none — and reports
/// `DeployedImages` with one entry per container image of the model it hosts, each carrying
/// the specified path, that path at the digest it resolved to, and the resolution instant.
#[tokio::test]
async fn a_variant_reports_its_instance_count_and_the_images_of_the_model_it_hosts() {
    let pinned = format!("sha256:{}", "8ebddbc1".repeat(8));
    let digest_path = format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/pinned@{pinned}");
    // Rows differ along what the rule generalises over: how many containers the hosted model
    // has, the FORM of each image reference (tagged, untagged, digest-pinned, a registry with
    // a port, no registry at all), whether the variant provisions instances and how many, and
    // a variant that hosts no model at all.
    let rows: Vec<VariantRow> = vec![
        VariantRow {
            case: "one tagged container, one instance",
            images: Some(vec![Some(format!(
                "{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/xgboost:1"
            ))]),
            instances: Some(1),
            serverless: false,
        },
        VariantRow {
            case: "one untagged container, three instances",
            images: Some(vec![Some(format!(
                "{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/inference"
            ))]),
            instances: Some(3),
            serverless: false,
        },
        VariantRow {
            case: "an inference pipeline of three containers",
            images: Some(vec![
                Some(format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/pre:2")),
                Some(digest_path.clone()),
                Some("registry.example.com:5000/team/inference:v1.2".to_string()),
            ]),
            instances: Some(2),
            serverless: false,
        },
        VariantRow {
            case: "a bare repository with no registry",
            images: Some(vec![Some("inference:latest".to_string())]),
            instances: Some(1),
            serverless: false,
        },
        VariantRow {
            case: "a serverless variant, which provisions no instances",
            images: Some(vec![Some(format!(
                "{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/xgboost:1"
            ))]),
            instances: None,
            serverless: true,
        },
        VariantRow {
            case: "a container that names no image",
            images: Some(vec![None]),
            instances: Some(1),
            serverless: false,
        },
        VariantRow {
            case: "a variant hosting no model",
            images: None,
            instances: Some(1),
            serverless: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let VariantRow {
            case,
            images,
            instances,
            serverless,
        } = row;
        let emulator = Emulator::new();
        let model = format!("deployed-images-model-{index}");
        let config = format!("deployed-images-config-{index}");
        let endpoint = format!("deployed-images-endpoint-{index}");

        let images: Option<Vec<Option<&str>>> = images
            .as_ref()
            .map(|listed| listed.iter().map(Option::as_deref).collect());
        let mut variant = json!({ "VariantName": "AllTraffic" });
        if let Some(images) = images.as_deref() {
            emulator
                .ok(case, "CreateModel", &model_body(&model, images))
                .await;
            variant["ModelName"] = json!(model);
        }
        if *serverless {
            variant["ServerlessConfig"] = json!({ "MemorySizeInMB": 2048, "MaxConcurrency": 1 });
        } else {
            variant["InstanceType"] = json!("ml.m5.large");
            if let Some(instances) = instances {
                variant["InitialInstanceCount"] = json!(instances);
            }
        }
        emulator
            .ok(
                case,
                "CreateEndpointConfig",
                &json!({ "EndpointConfigName": config, "ProductionVariants": [variant] }),
            )
            .await;
        emulator
            .ok(
                case,
                "CreateEndpoint",
                &json!({ "EndpointName": endpoint, "EndpointConfigName": config }),
            )
            .await;

        let described = emulator
            .ok(case, "DescribeEndpoint", &json!({"EndpointName": endpoint}))
            .await;
        // The count is REPORTED for every variant, whatever it provisions: a serverless
        // variant holds no instances, which is zero of them rather than an absent member.
        assert_eq!(
            current_instance_count(&described),
            Some(instances.unwrap_or(0)),
            "case {case:?}: expected CurrentInstanceCount {:?}, got {described}",
            instances.unwrap_or(0)
        );

        // One entry per container that names an image, in the model's own container order.
        let expected: Vec<&str> = images
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|image| *image)
            .collect();
        let reported = deployed_images(&described);
        if expected.is_empty() {
            assert!(
                reported.is_none(),
                "case {case:?}: a variant deploying no container image reports none, got \
                 {described}"
            );
            continue;
        }
        let reported =
            reported.unwrap_or_else(|| panic!("case {case:?}: expected DeployedImages in {described}"));
        assert_eq!(
            reported.len(),
            expected.len(),
            "case {case:?}: expected {} deployed image(s), got {reported:?}",
            expected.len()
        );
        let mut digests = Vec::new();
        for (entry, image) in reported.iter().zip(expected.iter()) {
            digests.push(asserted_digest(case, entry, image));
        }
        // A reference that PINS a digest names the image by it, so the path it resolves to is
        // the path itself.
        for (entry, image) in reported.iter().zip(expected.iter()) {
            if image.contains("@sha256:") {
                assert_eq!(
                    entry.get("ResolvedImage").and_then(Value::as_str),
                    Some(*image),
                    "case {case:?}: a digest-pinned path resolves to itself, got {entry}"
                );
            }
        }
        // One digest per reference: two containers naming different images never report the
        // same digest.
        let distinct: std::collections::BTreeSet<&String> = digests.iter().collect();
        assert_eq!(
            distinct.len(),
            digests.len(),
            "case {case:?}: each reference has its own digest, got {digests:?}"
        );
        // And the resolution is the one the create made: a second read replays it.
        let reread = emulator
            .ok(case, "DescribeEndpoint", &json!({"EndpointName": endpoint}))
            .await;
        assert_eq!(
            deployed_images(&reread),
            deployed_images(&described),
            "case {case:?}: every read replays one resolution, got {reread}"
        );
    }
}

/// RULE: the digest a variant's image resolves to is the one the REGISTRY holding that image
/// reports for it, taken when the model was created — so an image pushed to the same tag
/// afterwards does not change what an endpoint already serving that model reports.
#[tokio::test]
async fn a_variant_reports_the_digest_the_registry_holds_for_the_image_it_serves() {
    let case = "an image this account's registry holds";
    let emulator = Emulator::new();
    let repository = "registry-held-inference";
    let pushed = emulator
        .push_image(case, repository, "1", "the image the endpoint serves")
        .await;
    assert!(
        is_image_digest(&pushed),
        "case {case:?}: the registry must report a sha256 digest, got {pushed:?}"
    );

    let image = format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/{repository}:1");
    emulator
        .ok(
            case,
            "CreateModel",
            &model_body("registry-held-model", &[Some(&image)]),
        )
        .await;
    emulator
        .ok(
            case,
            "CreateEndpointConfig",
            &json!({
                "EndpointConfigName": "registry-held-config",
                "ProductionVariants": [{
                    "VariantName": "AllTraffic",
                    "ModelName": "registry-held-model",
                    "InstanceType": "ml.m5.large",
                    "InitialInstanceCount": 1,
                }],
            }),
        )
        .await;
    emulator
        .ok(
            case,
            "CreateEndpoint",
            &json!({
                "EndpointName": "registry-held-endpoint",
                "EndpointConfigName": "registry-held-config",
            }),
        )
        .await;

    let described = emulator
        .ok(
            case,
            "DescribeEndpoint",
            &json!({"EndpointName": "registry-held-endpoint"}),
        )
        .await;
    let entry = deployed_images(&described)
        .unwrap_or_else(|| panic!("case {case:?}: expected DeployedImages in {described}"));
    assert_eq!(
        asserted_digest(case, &entry[0], &image),
        pushed,
        "case {case:?}: expected the registry's own digest {pushed:?}, got {entry:?}"
    );

    // A SECOND image pushed to the same tag is a different image, and the registry reports a
    // digest of its own for it; what the endpoint serves was resolved when its model was
    // created, so the endpoint keeps reporting that resolution.
    let repushed = emulator
        .push_image(case, repository, "1", "an image pushed to the tag later")
        .await;
    assert_ne!(
        repushed, pushed,
        "case {case:?}: a different manifest must have a different digest, got {repushed:?}"
    );
    let reread = emulator
        .ok(
            case,
            "DescribeEndpoint",
            &json!({"EndpointName": "registry-held-endpoint"}),
        )
        .await;
    assert_eq!(
        deployed_images(&reread),
        deployed_images(&described),
        "case {case:?}: the resolution the create made is what every read replays, got {reread}"
    );
}

/// RULE: a deployment reports the images IT deploys while it is in flight, and once the clock
/// owes it complete the serving fleet reports them with the instance count that deployment
/// asked for.
#[tokio::test]
async fn a_deployment_reports_the_images_it_deploys_and_then_serves_them() {
    let case = "a deployment onto a serving endpoint";
    let emulator = Emulator::new();
    let serving_image = format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/serving:1");
    let deployed_image_path = format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/deploying:2");
    emulator
        .ok(
            case,
            "CreateModel",
            &model_body("deployment-model-a", &[Some(&serving_image)]),
        )
        .await;
    emulator
        .ok(
            case,
            "CreateModel",
            &model_body("deployment-model-b", &[Some(&deployed_image_path)]),
        )
        .await;
    for (config, model, instances) in [
        ("deployment-config-a", "deployment-model-a", 1),
        ("deployment-config-b", "deployment-model-b", 2),
    ] {
        emulator
            .ok(
                case,
                "CreateEndpointConfig",
                &json!({
                    "EndpointConfigName": config,
                    "ProductionVariants": [{
                        "VariantName": "AllTraffic",
                        "ModelName": model,
                        "InstanceType": "ml.m5.large",
                        "InitialInstanceCount": instances,
                    }],
                }),
            )
            .await;
    }
    emulator
        .ok(
            case,
            "CreateEndpoint",
            &json!({
                "EndpointName": "deployment-endpoint",
                "EndpointConfigName": "deployment-config-a",
            }),
        )
        .await;
    emulator
        .poll_until(case, "deployment-endpoint", "InService")
        .await;

    let updated = emulator
        .ok(
            case,
            "UpdateEndpoint",
            &json!({
                "EndpointName": "deployment-endpoint",
                "EndpointConfigName": "deployment-config-b",
            }),
        )
        .await;
    assert!(
        updated.get("EndpointArn").and_then(Value::as_str).is_some(),
        "case {case:?}: the deployment must be accepted, got {updated}"
    );

    // While the deployment is in flight the fleet that is SERVING still reports the images it
    // was created with, and the deployment reports the ones it is standing up.
    let in_flight = emulator
        .ok(
            case,
            "DescribeEndpoint",
            &json!({"EndpointName": "deployment-endpoint"}),
        )
        .await;
    if in_flight.get("EndpointStatus").and_then(Value::as_str) == Some("Updating") {
        let serving = deployed_images(&in_flight)
            .unwrap_or_else(|| panic!("case {case:?}: expected DeployedImages in {in_flight}"));
        asserted_digest(case, &serving[0], &serving_image);
        let pending = in_flight
            .pointer("/PendingDeploymentSummary/ProductionVariants/0/DeployedImages")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the deployment's DeployedImages in {in_flight}")
            });
        asserted_digest(case, &pending[0], &deployed_image_path);
    }

    // Once the deployment is owed complete, the new fleet is what is reported.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let served = loop {
        let described = emulator
            .poll_until(case, "deployment-endpoint", "InService")
            .await;
        if described.get("EndpointConfigName").and_then(Value::as_str)
            == Some("deployment-config-b")
        {
            break described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the deployment must settle, last read {described}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    };
    let entry = deployed_images(&served)
        .unwrap_or_else(|| panic!("case {case:?}: expected DeployedImages in {served}"));
    asserted_digest(case, &entry[0], &deployed_image_path);
    assert_eq!(
        current_instance_count(&served),
        Some(2),
        "case {case:?}: the deployment's own instance count is what serves, got {served}"
    );
    assert!(
        served.get("PendingDeploymentSummary").is_none(),
        "case {case:?}: a settled endpoint has no deployment in flight, got {served}"
    );
}

/// RULE: the same reading of a deployed image holds for an inference component's own
/// container: it reports the path specified, that path at the digest it resolved to, and the
/// instant of that resolution.
#[tokio::test]
async fn an_inference_component_reports_the_image_its_container_deploys() {
    let case = "an inference component carrying its own container";
    let emulator = Emulator::new();
    let image = format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com/component:3");
    emulator
        .ok(
            case,
            "CreateEndpointConfig",
            &json!({
                "EndpointConfigName": "component-config",
                "ProductionVariants": [{
                    "VariantName": "AllTraffic",
                    "InstanceType": "ml.m5.large",
                    "InitialInstanceCount": 1,
                }],
            }),
        )
        .await;
    emulator
        .ok(
            case,
            "CreateEndpoint",
            &json!({
                "EndpointName": "component-endpoint",
                "EndpointConfigName": "component-config",
            }),
        )
        .await;
    emulator
        .ok(
            case,
            "CreateInferenceComponent",
            &json!({
                "InferenceComponentName": "image-component",
                "EndpointName": "component-endpoint",
                "VariantName": "AllTraffic",
                "Specification": {
                    "Container": { "Image": image },
                    "ComputeResourceRequirements": { "MinMemoryRequiredInMb": 1024 },
                },
                "RuntimeConfig": { "CopyCount": 1 },
            }),
        )
        .await;

    let described = emulator
        .ok(
            case,
            "DescribeInferenceComponent",
            &json!({"InferenceComponentName": "image-component"}),
        )
        .await;
    let deployed = described
        .pointer("/Specification/Container/DeployedImage")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a DeployedImage in {described}"));
    asserted_digest(case, &deployed, &image);
}
