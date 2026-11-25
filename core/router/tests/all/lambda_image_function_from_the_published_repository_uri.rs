// SPDX-License-Identifier: CC0-1.0

//! A CONTAINER-IMAGE Lambda function created from the URI Amazon ECR ITSELF published for the
//! repository the image was pushed to — the flow a client follows, across the three services
//! it spans.
//!
//! RULE: `CreateFunction` with `PackageType=Image` FOLLOWS `Code.ImageUri` into the registry
//! of the function's OWN Region — "For a container image, the code property must include the
//! URI of a container image in the Amazon ECR registry. You do not need to specify the handler
//! and runtime properties" — so a reference whose authority addresses a registry this emulator
//! serves and whose repository and tag or digest resolve to a manifest that registry HOLDS is
//! stored: HTTP 201 with `PackageType=Image` and `State=Pending` settling to `Active`, pinned
//! to the manifest digest the registry minted (`FunctionCodeLocation$ResolvedImageUri`).
//! `InvalidParameterValueException` (HTTP 400) is for the opposite case alone — a reference
//! that resolves to no image of that registry — and then nothing is created.
//!
//! The canonical source of such a reference is `Repository$repositoryUri` ("The URI for the
//! repository. You can use this URI for container image `push` and `pull` operations"), which
//! Amazon ECR's own control plane publishes for every read of a repository, so a URI ECR
//! published for a repository that holds the pushed reference is BY CONSTRUCTION one Lambda
//! follows: a client that creates a repository, keeps the URI the create reported, pushes to
//! it and then names `<that URI>:<tag>` must be served, whatever zone and port this emulator
//! publishes that URI in. That is what this suite spans the three services for — Amazon ECR
//! publishes the address and holds the image, IAM holds the execution role, Lambda follows the
//! reference — and a crate-local suite can pin none of it, because the URI under test is one
//! ECR composed rather than one the test wrote.
//!
//! Every image goes in through Amazon ECR's OWN push sequence on the same engine —
//! `InitiateLayerUpload`/`UploadLayerPart`/`CompleteLayerUpload` for the configuration blob
//! and the layer blob, then `PutImage` for a Docker image manifest V2 schema 2 referencing
//! both — so the digest a function is pinned to is the one the registry minted, and a manifest
//! referencing a blob the repository does not hold would be refused by `PutImage` itself
//! (`LayersNotFoundException`). No function here names a `Runtime` or a `Handler`: a
//! from-scratch image carrying its own runtime interface client names neither.
//!
//! The rows differ along what the rule generalises over: which Region's registry holds the
//! image (and therefore which Region the function is created in), the form of the repository's
//! name (flat, namespaced, carrying the punctuation the name allows), how the reference
//! addresses the image inside it (a tag, no tag at all — which names the default tag — and the
//! digest the push minted), which ECR read the client took the URI from
//! (`CreateRepository`'s own answer and `DescribeRepositories`), the endpoint the client
//! reached this emulator at (the emulator's own socket, the loopback name, and AWS's own
//! service hosts, which is what decides the zone and port ECR publishes the URI in), and — in
//! the second table — which part of a reference that resolves to no image is wrong.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element reader this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, form_encode};

/// The API version an IAM awsQuery request carries.
const IAM_VERSION: &str = "2010-05-08";

/// Amazon ECR's `X-Amz-Target` prefix, from that service's own model.
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// The service principal a Lambda execution role has to admit, and the role every function
/// below runs as.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";
/// See [`LAMBDA_PRINCIPAL`].
const EXECUTION_ROLE: &str = "cirexecution";

/// The media type a Docker image manifest V2 schema 2 declares, which is what the pushes
/// below write and what the recorded client pushed.
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";
/// The media type of the image configuration blob such a manifest references.
const CONFIG_MEDIA_TYPE: &str = "application/vnd.docker.container.image.v1+json";
/// The media type of a gzipped layer blob such a manifest references.
const LAYER_MEDIA_TYPE: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";

/// The tag the images below are pushed under, where the reference names one.
const PUSHED_TAG: &str = "v1";

/// The tag a reference naming none addresses, which is the registry convention a container
/// client's `docker push` of a bare repository writes.
const DEFAULT_TAG: &str = "latest";

/// The wire error code a reference resolving to no image is refused with.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The emulator's own socket, which a client that resolves no AWS hostname reaches, and the
/// loopback name of the same interface.
const OWN_ENDPOINT: &str = "127.0.0.1:9297";
/// See [`OWN_ENDPOINT`].
const LOOPBACK_ENDPOINT: &str = "localhost:9297";

/// How often a poll loop looks again for a transition the clock owes, and how long it keeps
/// looking. The span itself is Lambda's own, so nothing here sleeps a fixed span on its
/// behalf, and the deadline is far past it so a loaded host cannot fail a row.
const POLL: Duration = Duration::from_millis(20);
/// See [`POLL`].
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// IAM, Amazon ECR and Lambda over ONE shared engine, as `router/src/lib.rs` mounts them:
/// the registry a Lambda create resolves a reference against is the very store Amazon ECR
/// pushed the image into.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These rows exercise the three services, not request signing.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming `signing_name` in `region`, which is what routes a request
/// here and what says which Region it is made in. Neither the key id nor the signature is a
/// credential: this harness has request authentication off.
fn authorization(signing_name: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{region}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One Amazon ECR call on its awsJson1_1 wire, addressed at `host` — the endpoint the client
/// is configured with — and signed for `region`, whose registry holds the repository.
async fn ecr(
    router: &MultiServiceRouter,
    host: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("http://{host}/"))
        .header("host", host)
        .header("authorization", authorization("ecr", region))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ECR_TARGET_PREFIX}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One Amazon ECR call that has to succeed.
async fn ecr_ok(
    router: &MultiServiceRouter,
    case: &str,
    host: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answer) = ecr(router, host, region, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ecr {operation} in {region} to answer 200, got {status} with \
         {answer}"
    );
    answer
}

/// One IAM awsQuery action that has to succeed, answered with its XML body.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={IAM_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("http://iam.amazonaws.com/")
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization("iam", "us-east-1"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected IAM {action} to answer 200, got {status} with {text}"
    );
    text
}

/// One Lambda control-plane call on the restJson1 wire, addressed at `host` and signed for
/// `region` — the Region the function is created in and the one its image is resolved in.
/// Answered with the status, the wire error code (`""` on a success) and the body.
async fn lambda(
    router: &MultiServiceRouter,
    host: &str,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, String, Value) {
    let payload = body
        .map(|value| Bytes::from(value.to_string()))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://{host}{uri}"))
        .header("host", host)
        .header("authorization", authorization("lambda", region))
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let bytes = response.into_body();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({ "unparsed": String::from_utf8_lossy(&bytes) }))
    };
    let code = parsed
        .get("__type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            headers
                .get("x-amzn-errortype")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        })
        .unwrap_or_default();
    (status, code, parsed)
}

/// A required string at `pointer` of a body, or a panic naming the case.
fn string(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a string at {pointer}, got {body}"))
        .to_string()
}

/// The `message` an error answer carries, under either casing the envelopes use.
fn error_message(body: &Value) -> String {
    ["/message", "/Message"]
        .iter()
        .find_map(|pointer| body.pointer(pointer).and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// The `sha256:<hex>` digest of `content` — the form a registry addresses a blob by, computed
/// here rather than read back out of the registry's own answer.
fn content_digest(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex::encode(Sha256::digest(content)))
}

/// `len` deterministic bytes that depend on `label` and `len`, so the configuration blob and
/// the layer blob of one image are different content under different digests.
fn blob_bytes(label: &str, len: usize) -> Vec<u8> {
    let seed = usize::from(label.bytes().fold(0_u8, |acc, byte| acc.wrapping_add(byte)));
    (0..len)
        .map(|index| u8::try_from((index * 31 + seed) % 256).unwrap_or_default())
        .collect()
}

/// Push `content` into `repository` as ONE blob, through the sequence a registry client uses,
/// and answer the digest the registry registered it under.
async fn push_blob(
    router: &MultiServiceRouter,
    case: &str,
    host: &str,
    region: &str,
    repository: &str,
    content: &[u8],
) -> String {
    let opened = ecr_ok(
        router,
        case,
        host,
        region,
        "InitiateLayerUpload",
        &json!({ "repositoryName": repository }),
    )
    .await;
    let upload_id = string(case, &opened, "/uploadId");
    let last = i64::try_from(content.len()).expect("a test blob fits in an i64") - 1;
    let part = ecr_ok(
        router,
        case,
        host,
        region,
        "UploadLayerPart",
        &json!({
            "repositoryName": repository,
            "uploadId": upload_id,
            "partFirstByte": 0,
            "partLastByte": last,
            "layerPartBlob": aws_smithy_types::base64::encode(content),
        }),
    )
    .await;
    assert_eq!(
        part.pointer("/lastByteReceived").and_then(Value::as_i64),
        Some(last),
        "case {case:?}: the registry must report the last byte it accepted ({last}), got {part}"
    );
    let digest = content_digest(content);
    let completed = ecr_ok(
        router,
        case,
        host,
        region,
        "CompleteLayerUpload",
        &json!({
            "repositoryName": repository,
            "uploadId": upload_id,
            "layerDigests": [&digest],
        }),
    )
    .await;
    let registered = string(case, &completed, "/layerDigest");
    assert_eq!(
        registered, digest,
        "case {case:?}: the registry must register the blob under the digest of its own bytes \
         ({digest}), got {registered}"
    );
    registered
}

/// Push one image into `repository` under `tag`: its configuration blob, its layer blob and
/// then the manifest referencing both. Answers the manifest digest the registry MINTED, which
/// is what a function created from this image must be pinned to.
async fn push_image(
    router: &MultiServiceRouter,
    case: &str,
    host: &str,
    region: &str,
    repository: &str,
    tag: &str,
) -> String {
    let config = blob_bytes("config", 512);
    let layer = blob_bytes("layer", 4096);
    let config_digest = push_blob(router, case, host, region, repository, &config).await;
    let layer_digest = push_blob(router, case, host, region, repository, &layer).await;
    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST_MEDIA_TYPE,
        "config": {
            "mediaType": CONFIG_MEDIA_TYPE,
            "digest": config_digest,
            "size": config.len(),
        },
        "layers": [{
            "mediaType": LAYER_MEDIA_TYPE,
            "digest": layer_digest,
            "size": layer.len(),
        }],
    })
    .to_string();
    let pushed = ecr_ok(
        router,
        case,
        host,
        region,
        "PutImage",
        &json!({
            "repositoryName": repository,
            "imageManifest": manifest,
            "imageManifestMediaType": MANIFEST_MEDIA_TYPE,
            "imageTag": tag,
        }),
    )
    .await;
    string(case, &pushed, "/image/imageId/imageDigest")
}

/// Create the execution role every function below runs as, trusted by this service's own
/// principal, and answer its ARN.
async fn seed_execution_role(router: &MultiServiceRouter, case: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": LAMBDA_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        case,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("AssumeRolePolicyDocument", trust.as_str()),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// A `CreateFunction` body for a container-image package: no `Runtime` and no `Handler`, which
/// is what an image carrying its own runtime interface client names.
fn image_create(name: &str, role: &str, image_uri: &str) -> Value {
    json!({
        "FunctionName": name,
        "Role": role,
        "PackageType": "Image",
        "Code": { "ImageUri": image_uri },
        "Timeout": 10,
        "MemorySize": 256,
    })
}

/// `GetFunction` on a function, with no qualifier.
async fn get_function(
    router: &MultiServiceRouter,
    host: &str,
    region: &str,
    name: &str,
) -> (u16, Value) {
    let (status, _code, body) = lambda(
        router,
        host,
        region,
        "GET",
        &format!("/2015-03-31/functions/{name}"),
        None,
    )
    .await;
    (status, body)
}

/// The `GetFunction` answer once Lambda first reports the function `Active` ("the function is
/// ready to be invoked", `FunctionConfiguration$State`) — looked for again until it holds, so
/// nothing here sleeps the activation span on that service's behalf.
async fn active_function(
    router: &MultiServiceRouter,
    case: &str,
    host: &str,
    region: &str,
    name: &str,
) -> Value {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) = get_function(router, host, region, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetFunction {name}, got {status} with {body}"
        );
        if body.pointer("/Configuration/State").and_then(Value::as_str) == Some("Active") {
            return body;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the function to settle Active, last read {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// How a row's reference addresses the image inside the repository it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressed {
    /// `<repositoryUri>:<tag>`, which is what a client that pushed a tag holds.
    Tag,
    /// `<repositoryUri>` on its own, which a container client reads as the default tag.
    DefaultTag,
    /// `<repositoryUri>@<digest>`, the digest the push itself minted.
    Digest,
}

/// WHERE the row's client got the registry address it composed the reference on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Published {
    /// `Repository$repositoryUri` as `CreateRepository` reported it.
    CreateAnswer,
    /// The same member as `DescribeRepositories` reports it — a client that creates the
    /// repository in one step and reads it back in another.
    DescribeAnswer,
    /// The registry host AWS PUBLISHES for this account's registry in that Region,
    /// `<account-id>.dkr.ecr.<region>.amazonaws.com` — the registry URL format
    /// `AuthorizationData$proxyEndpoint` documents — composed by a client that resolves AWS's
    /// own hostnames rather than read off a repository. The rule is about the registry a
    /// reference addresses, not the spelling it is addressed by.
    AwsRegistryHost,
}

/// One row of the acceptance table.
struct Row {
    case: &'static str,
    /// The Region whose registry holds the image, which is the Region the function is created
    /// in: Lambda resolves a package in the function's own Region.
    region: &'static str,
    /// The repository the image is pushed to.
    repository: &'static str,
    /// The endpoint the client is configured with, or `None` for a client that addresses AWS's
    /// own service hosts — which is what decides the zone and port ECR publishes a repository
    /// URI in.
    reached: Option<&'static str>,
    addressed: Addressed,
    published: Published,
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded flow: the emulator's own socket, by the tag just pushed",
        region: "us-east-1",
        repository: "cir-app",
        reached: Some(OWN_ENDPOINT),
        addressed: Addressed::Tag,
        published: Published::CreateAnswer,
    },
    Row {
        case: "a namespaced repository, pinned by the digest the push minted",
        region: "us-east-1",
        repository: "team/cir-app",
        reached: Some(OWN_ENDPOINT),
        addressed: Addressed::Digest,
        published: Published::DescribeAnswer,
    },
    Row {
        case: "the loopback name, with no tag at all, in another Region",
        region: "eu-west-1",
        repository: "cir-default",
        reached: Some(LOOPBACK_ENDPOINT),
        addressed: Addressed::DefaultTag,
        published: Published::CreateAnswer,
    },
    Row {
        case: "a client that resolves AWS's own service hostnames, in a third Region",
        region: "ap-southeast-2",
        repository: "cir-far",
        reached: None,
        addressed: Addressed::Tag,
        published: Published::CreateAnswer,
    },
    Row {
        case: "a repository name carrying the punctuation a name allows",
        region: "us-west-2",
        repository: "a.b_c-d",
        reached: Some(OWN_ENDPOINT),
        addressed: Addressed::Tag,
        published: Published::DescribeAnswer,
    },
    Row {
        case: "the registry host AWS publishes, composed by the client itself",
        region: "us-east-1",
        repository: "cir-aws-spelling",
        reached: Some(OWN_ENDPOINT),
        addressed: Addressed::Tag,
        published: Published::AwsRegistryHost,
    },
];

/// The host a row's Amazon ECR calls are addressed to.
fn ecr_host(row: &Row) -> String {
    row.reached
        .map_or_else(|| format!("ecr.{}.amazonaws.com", row.region), str::to_string)
}

/// The host a row's Lambda calls are addressed to.
fn lambda_host(row: &Row) -> String {
    row.reached
        .map_or_else(|| format!("lambda.{}.amazonaws.com", row.region), str::to_string)
}

/// Rule: a reference built on the URI Amazon ECR published for a repository that holds the
/// pushed image is STORED — 201, `PackageType=Image`, `State=Pending` settling to `Active`,
/// pinned to the digest the registry minted.
#[tokio::test]
async fn a_function_is_created_from_the_uri_ecr_published_for_a_pushed_image() {
    let router = emulator();
    let role = seed_execution_role(&router, "setup").await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let ecr_host = ecr_host(row);
        let lambda_host = lambda_host(row);

        // The client's own flow: create the repository, keep the URI the registry published
        // for it, and push the image to that repository.
        let created = ecr_ok(
            &router,
            case,
            &ecr_host,
            row.region,
            "CreateRepository",
            &json!({ "repositoryName": row.repository }),
        )
        .await;
        let published = string(case, &created, "/repository/repositoryUri");
        let tag = match row.addressed {
            Addressed::DefaultTag => DEFAULT_TAG,
            Addressed::Tag | Addressed::Digest => PUSHED_TAG,
        };
        let digest =
            push_image(&router, case, &ecr_host, row.region, row.repository, tag).await;
        assert!(
            digest.starts_with("sha256:"),
            "case {case:?}: expected the registry to mint a sha256 manifest digest, got \
             {digest:?}"
        );

        // The address the row's client composes its reference on.
        let registry_address = match row.published {
            Published::CreateAnswer => published.clone(),
            Published::DescribeAnswer => {
                let described = ecr_ok(
                    &router,
                    case,
                    &ecr_host,
                    row.region,
                    "DescribeRepositories",
                    &json!({ "repositoryNames": [row.repository] }),
                )
                .await;
                let read_back = string(case, &described, "/repositories/0/repositoryUri");
                assert_eq!(
                    read_back, published,
                    "case {case:?}: expected both ECR reads to publish one URI for the \
                     repository, got {published:?} and {read_back:?}"
                );
                read_back
            }
            Published::AwsRegistryHost => format!(
                "{}.dkr.ecr.{}.amazonaws.com/{}",
                engine::emulator_account_id(),
                row.region,
                row.repository
            ),
        };
        let image_uri = match row.addressed {
            Addressed::Tag => format!("{registry_address}:{PUSHED_TAG}"),
            Addressed::DefaultTag => registry_address.clone(),
            Addressed::Digest => format!("{registry_address}@{digest}"),
        };

        // THE CREATE: the reference the client holds, named by a function that states no
        // runtime and no handler.
        let name = format!("cir-{index}");
        let (status, code, answer) = lambda(
            &router,
            &lambda_host,
            row.region,
            "POST",
            "/2015-03-31/functions",
            Some(&image_create(&name, &role, &image_uri)),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (201, ""),
            "case {case:?}: expected 201 from CreateFunction for {image_uri:?}, got {status} \
             {code:?} with {answer}"
        );
        assert_eq!(
            answer.pointer("/PackageType").and_then(Value::as_str),
            Some("Image"),
            "case {case:?}: expected PackageType Image, got {answer}"
        );
        assert_eq!(
            answer.pointer("/State").and_then(Value::as_str),
            Some("Pending"),
            "case {case:?}: expected the created function to be Pending, got {answer}"
        );

        // The round trip: the function settles Active and reports the package it was created
        // with, pinned to the digest the registry minted for the reference.
        let read = active_function(&router, case, &lambda_host, row.region, &name).await;
        assert_eq!(
            read.pointer("/Configuration/PackageType")
                .and_then(Value::as_str),
            Some("Image"),
            "case {case:?}: expected the stored function to report PackageType Image, got {read}"
        );
        assert_eq!(
            read.pointer("/Code/ImageUri").and_then(Value::as_str),
            Some(image_uri.as_str()),
            "case {case:?}: expected the ImageUri as supplied ({image_uri:?}), got {read}"
        );
        let expected_resolved = format!("{registry_address}@{digest}");
        assert_eq!(
            read.pointer("/Code/ResolvedImageUri").and_then(Value::as_str),
            Some(expected_resolved.as_str()),
            "case {case:?}: expected the function pinned to {expected_resolved:?}, got {read}"
        );
        // Neither managed member is reported for an image package: the image brings both.
        for member in ["Runtime", "Handler"] {
            let reported = read.pointer(&format!("/Configuration/{member}"));
            assert!(
                reported.is_none_or(Value::is_null),
                "case {case:?}: expected no {member} on a container-image function, got \
                 {reported:?}"
            );
        }
    }
}

/// Rule: only a reference that resolves to no image of the function's own Region's registry is
/// refused — and then nothing is created. The refusals are the two sentences this service
/// states for the two ways a reference resolves to nothing: a reference that is no image URI
/// of that registry at all is "not valid", and one that is but names nothing pushed "does not
/// exist".
#[tokio::test]
async fn a_reference_resolving_to_no_pushed_image_is_refused_and_creates_nothing() {
    /// The Region this table's registry and functions live in.
    const REGION: &str = "us-east-1";
    /// The repository the one pushed image of this table lives in.
    const REPOSITORY: &str = "cir-refused";

    let router = emulator();
    let role = seed_execution_role(&router, "setup").await;
    let created = ecr_ok(
        &router,
        "setup",
        OWN_ENDPOINT,
        REGION,
        "CreateRepository",
        &json!({ "repositoryName": REPOSITORY }),
    )
    .await;
    let published = string("setup", &created, "/repository/repositoryUri");
    let digest = push_image(
        &router,
        "setup",
        OWN_ENDPOINT,
        REGION,
        REPOSITORY,
        PUSHED_TAG,
    )
    .await;
    let authority = published
        .split_once('/')
        .map(|(authority, _path)| authority.to_string())
        .unwrap_or_else(|| panic!("expected {published:?} to carry a registry authority"));

    // (case, the reference the create names, the clause the refusal states)
    let rows: [(&str, String, &str); 6] = [
        (
            "the published URI, with a tag nothing was pushed under",
            format!("{published}:never"),
            "does not exist",
        ),
        (
            "the published URI, with a digest nothing was pushed under",
            format!("{published}@sha256:{}", "0".repeat(64)),
            "does not exist",
        ),
        (
            "the published authority, with a repository this registry does not hold",
            format!("{authority}/absent:{PUSHED_TAG}"),
            "does not exist",
        ),
        (
            "this account's registry in another Region",
            format!(
                "{}.dkr.ecr.eu-west-1.amazonaws.com/{REPOSITORY}:{PUSHED_TAG}",
                engine::emulator_account_id()
            ),
            "is not valid",
        ),
        (
            "a registry this emulator does not serve",
            format!("registry.example.test:5000/{REPOSITORY}:{PUSHED_TAG}"),
            "is not valid",
        ),
        (
            "the public registry, which is not this one",
            "public.ecr.aws/nginx/nginx:latest".to_string(),
            "is not valid",
        ),
    ];

    for (index, (case, image_uri, clause)) in rows.iter().enumerate() {
        let name = format!("cir-refused-{index}");
        let (status, code, answer) = lambda(
            &router,
            OWN_ENDPOINT,
            REGION,
            "POST",
            "/2015-03-31/functions",
            Some(&image_create(&name, &role, image_uri)),
        )
        .await;
        assert_eq!(
            (status, code.as_str()),
            (400, INVALID_PARAMETER),
            "case {case:?}: expected 400 {INVALID_PARAMETER} for {image_uri:?}, got {status} \
             {code:?} with {answer}"
        );
        let expected = format!("Source image {image_uri} {clause}. Provide a valid source image.");
        assert_eq!(
            error_message(&answer),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            error_message(&answer)
        );
        // Nothing was created: the read of the function it would have been is the modelled
        // not-found.
        let (read_status, read) = get_function(&router, OWN_ENDPOINT, REGION, &name).await;
        assert_eq!(
            read_status, 404,
            "case {case:?}: expected the refused function to be absent, got {read_status} with \
             {read}"
        );
    }

    // And the reference that DOES resolve, at that very address, is served — so every row
    // above was refused for the image it names and not for the address it carries.
    let case = "the published URI, with the tag that was pushed";
    let image_uri = format!("{published}:{PUSHED_TAG}");
    let (status, code, answer) = lambda(
        &router,
        OWN_ENDPOINT,
        REGION,
        "POST",
        "/2015-03-31/functions",
        Some(&image_create("cir-served", &role, &image_uri)),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (201, ""),
        "case {case:?}: expected 201 from CreateFunction for {image_uri:?}, got {status} \
         {code:?} with {answer}"
    );
    let read = active_function(&router, case, OWN_ENDPOINT, REGION, "cir-served").await;
    let expected_resolved = format!("{published}@{digest}");
    assert_eq!(
        read.pointer("/Code/ResolvedImageUri").and_then(Value::as_str),
        Some(expected_resolved.as_str()),
        "case {case:?}: expected the function pinned to {expected_resolved:?}, got {read}"
    );
}
