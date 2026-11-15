// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the PULL this service makes when it STORES a container-image
//! package, and for the two refusals that come out of it.
//!
//! The rule, one level above the recorded case that filed the issue (a repository policy
//! denying the pull actions to this service, on a digest-addressed reference carrying the
//! URI the registry announced):
//!
//! > Lambda fetches a container image with its OWN service principal when the package is
//! > stored, so the write is decided by the repository's own policy: a policy that
//! > explicitly denies that principal one of the pull calls refuses the write as a
//! > permission failure — HTTP 403 `AccessDeniedException`, `Lambda does not have
//! > permission to access the ECR image. Check the ECR permissions.` — and stores nothing,
//! > while a repository that denies it nothing is served. The reference itself is followed
//! > wherever the registry is addressed: at one of AWS's own registry hosts, and at the
//! > address the registry ANNOUNCED for the repository, which is what a client that pushed
//! > the image here was handed and what it then names the image by. Only a reference that
//! > resolves to no image of this Region's registry at all stays the modelled 400
//! > `InvalidParameterValueException`.
//!
//! The rows span what the rule generalises over: which pull action a policy denies and how
//! it spells it (one action, both, a wildcard), which principal it denies (this service, a
//! wildcard, another service), whether the statement carries a `Sid`, whether an `Allow`
//! stands beside the deny, the endpoint the
//! client is configured with (an AWS service host, a loopback literal with a port, a
//! loopback name), and the write that stores the package (`CreateFunction` and
//! `UpdateFunctionCode`). No row scopes its deny with a `Resource` element: a repository
//! policy is attached to the repository its statements apply to, so Amazon ECR refuses a
//! document that names a resource of its own.
//!
//! The rows also span how the reference NAMES the image, because the decision has to be the
//! same however a client spells the same image: the address it carries (AWS's own
//! `<account-id>.dkr.ecr.<region>.<domain>` host and the address the registry announced) by
//! the way it addresses the image inside the repository (a tag, a bare reference naming the
//! default tag, a digest) — and, on the other side of that boundary, a reference into
//! another Region's or another host's registry, which is no image of this one.
//!
//! ECR is mounted BESIDE Lambda on one engine, and every image here is pushed through ECR's
//! own `CreateRepository`/`PutImage` with the policy applied through its own
//! `SetRepositoryPolicy` — so what Lambda resolves and decides is the registry's own state
//! rather than anything this test wrote directly. The host-parameterised sender is
//! `function_url_data_plane.rs`'s, because the announced repository URI depends on the
//! endpoint the client is configured with.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{function_arn, make_activation_due, ACCOUNT, REGION, ROLE};
use crate::function_url_data_plane::{send, Answer};

/// The service hosts an SDK that resolves AWS's own hostnames is configured with.
const LAMBDA_HOST: &str = "lambda.us-east-1.amazonaws.com";
/// See [`LAMBDA_HOST`].
const ECR_HOST: &str = "api.ecr.us-east-1.amazonaws.com";

/// The `X-Amz-Target` prefix Amazon ECR routes on (that crate's `OPERATIONS.md`, "Wire
/// protocol").
const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// The sentence AWS refuses a container-image write its own principal may not pull with —
/// wholly fixed text (the Lambda Developer Guide's container-image permissions section).
const ACCESS_DENIED_MESSAGE: &str =
    "Lambda does not have permission to access the ECR image. Check the ECR permissions.";

/// The error code that refusal carries, and its status.
const ACCESS_DENIED: &str = "AccessDeniedException";
/// See [`ACCESS_DENIED`].
const ACCESS_DENIED_STATUS: u16 = 403;

/// The code a reference that resolves to no image at all is refused with.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// This service's own principal, which is the one a repository policy decides its pull for
/// ("Principal": {"Service": "lambda.amazonaws.com"}).
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// Another compute service's principal, which a deny aimed at it leaves this service alone.
const OTHER_PRINCIPAL: &str = "ecs-tasks.amazonaws.com";

/// The two calls a pull of a private image makes on the repository, as an ECR repository
/// policy names them (`ecr:BatchGetImage` fetches the manifest and
/// `ecr:GetDownloadUrlForLayer` the layers behind it).
const MANIFEST_ACTION: &str = "ecr:BatchGetImage";
/// See [`MANIFEST_ACTION`].
const LAYER_ACTION: &str = "ecr:GetDownloadUrlForLayer";

/// Lambda and ECR on one engine: the registry a function's image is resolved against is
/// ECR's own store, so it is written through ECR's own surface — the shape
/// `core/router/src/lib.rs` mounts them in.
fn emulator(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(
        service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService::new(
            Arc::clone(engine),
        ),
    );
    router
}

/// A SigV4 credential scope naming `service` in the Region the whole suite signs for.
/// Neither the key id nor the signature is a credential: the router above has request
/// authentication off, exactly as the emulator runs it unless started with it on.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Amazon ECR operation on the awsJson1_1 wire, addressed at `host`.
async fn ecr_call(
    router: &MultiServiceRouter,
    host: &str,
    operation: &str,
    body: &Value,
) -> Answer {
    let scope = authorization("ecr");
    let target = format!("{ECR_TARGET_PREFIX}.{operation}");
    send(
        router,
        "POST",
        host,
        "/",
        body.to_string().as_bytes(),
        &[
            ("authorization", scope.as_str()),
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", target.as_str()),
        ],
    )
    .await
}

/// One Lambda control-plane call on the restJson1 wire, addressed at `host`.
async fn lambda_call(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answer {
    let scope = authorization("lambda");
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    send(
        router,
        method,
        host,
        uri,
        &payload,
        &[
            ("authorization", scope.as_str()),
            ("content-type", "application/json"),
        ],
    )
    .await
}

/// The wire error code of an error answer, under either place the envelopes carry it.
fn error_code(answer: &Answer) -> String {
    answer
        .json()
        .get("__type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| answer.header("x-amzn-errortype"))
        .unwrap_or_else(|| answer.summary())
}

/// The `message` of an error answer, under either casing the envelopes use.
fn error_message(answer: &Answer) -> Option<String> {
    let body = answer.json();
    ["message", "Message"]
        .iter()
        .find_map(|member| body.get(*member).and_then(Value::as_str))
        .map(str::to_string)
}

/// A manifest a push can store with no layer upload behind it: a schema-2 image manifest
/// that declares no filesystem layer and no configuration blob, so the repository is
/// missing none of the blobs it references.
fn manifest(marker: &str) -> String {
    json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "layers": [],
        "annotations": { "org.opencontainers.image.title": marker },
    })
    .to_string()
}

/// What pushing an image to a fresh repository through ECR's own surface established.
struct Pushed {
    /// The URI the REGISTRY announced for the repository (`Repository$repositoryUri`),
    /// which is what a client that pushed here names the image by.
    announced_uri: String,
    /// The manifest digest the registry minted for the push.
    digest: String,
}

/// Create the repository `name` through `host`, push one image to it under `tag`, and
/// answer what the registry announced and minted.
async fn push_image(
    router: &MultiServiceRouter,
    host: &str,
    name: &str,
    tag: &str,
) -> Pushed {
    let created = ecr_call(router, host, "CreateRepository", &json!({"repositoryName": name})).await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: expected 200 from CreateRepository, got {}",
        created.summary()
    );
    let announced_uri = created
        .json()
        .pointer("/repository/repositoryUri")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {name:?}: expected CreateRepository to announce a repositoryUri, got {}",
                created.summary()
            )
        })
        .to_string();
    let pushed = ecr_call(
        router,
        host,
        "PutImage",
        &json!({
            "repositoryName": name,
            "imageTag": tag,
            "imageManifest": manifest(name),
        }),
    )
    .await;
    assert_eq!(
        pushed.status, 200,
        "case {name:?}: expected 200 from PutImage, got {}",
        pushed.summary()
    );
    let digest = pushed
        .json()
        .pointer("/image/imageId/imageDigest")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {name:?}: expected PutImage to mint a manifest digest, got {}",
                pushed.summary()
            )
        })
        .to_string();
    Pushed {
        announced_uri,
        digest,
    }
}

/// Apply `policy` to the repository `name` through ECR's own `SetRepositoryPolicy`.
async fn set_repository_policy(
    router: &MultiServiceRouter,
    host: &str,
    name: &str,
    policy: &Value,
) {
    let applied = ecr_call(
        router,
        host,
        "SetRepositoryPolicy",
        &json!({ "repositoryName": name, "policyText": policy.to_string() }),
    )
    .await;
    assert_eq!(
        applied.status, 200,
        "case {name:?}: expected 200 from SetRepositoryPolicy, got {}",
        applied.summary()
    );
}

/// A `CreateFunction` request body for an image package.
fn image_create(name: &str, image_uri: &str) -> Value {
    json!({
        "FunctionName": name,
        "Role": ROLE,
        "PackageType": "Image",
        "Code": { "ImageUri": image_uri },
    })
}

/// `GetFunction` on a function, with no qualifier.
async fn get_function(router: &MultiServiceRouter, host: &str, name: &str) -> Answer {
    let uri = format!("/2015-03-31/functions/{name}");
    lambda_call(router, host, "GET", &uri, &Value::Null).await
}

/// Assert that `answer` is the 403 refusal AWS words for an image this service may not
/// pull, and that `name` names no function afterwards.
async fn assert_refused_access(
    router: &MultiServiceRouter,
    host: &str,
    case: &str,
    name: &str,
    answer: &Answer,
) {
    assert_eq!(
        answer.status, ACCESS_DENIED_STATUS,
        "case {case:?}: expected {ACCESS_DENIED_STATUS}, got {}",
        answer.summary()
    );
    assert_eq!(
        error_code(answer),
        ACCESS_DENIED,
        "case {case:?}: expected {ACCESS_DENIED}, got {}",
        error_code(answer)
    );
    assert_eq!(
        error_message(answer).as_deref(),
        Some(ACCESS_DENIED_MESSAGE),
        "case {case:?}: expected {ACCESS_DENIED_MESSAGE:?}, got {:?}",
        error_message(answer)
    );
    let read = get_function(router, host, name).await;
    assert_eq!(
        read.status, 404,
        "case {case:?}: expected the refused function to be absent, got {}",
        read.summary()
    );
    let expected = format!("Function not found: {}", function_arn(name));
    assert_eq!(
        error_message(&read).as_deref(),
        Some(expected.as_str()),
        "case {case:?}: expected {expected:?}, got {:?}",
        error_message(&read)
    );
}

/// One row of the policy table: the document the repository carries, and whether it denies
/// this service the pull.
struct PolicyCase {
    /// What makes this row its own case.
    case: &'static str,
    /// The repository policy, or `None` for a repository carrying none at all.
    policy: Option<Value>,
    /// Whether the write must be refused as a permission failure.
    denied: bool,
}

/// The policy table.
fn policy_cases() -> Vec<PolicyCase> {
    // A repository policy is attached to the repository its statements apply to, so it names
    // no resource of its own and Amazon ECR refuses a document that does (pinned by the ECR
    // crate's `policy_document_scope` suite) — which is why no row below scopes its deny by a
    // `Resource` element. A deny reaches THIS pull exactly when it is attached to THIS
    // repository, and each row gets its own repository.
    vec![
        PolicyCase {
            case: "no repository policy at all",
            policy: None,
            denied: false,
        },
        PolicyCase {
            case: "a policy that allows this service the pull",
            policy: Some(json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": LAMBDA_PRINCIPAL },
                    "Action": [MANIFEST_ACTION, LAYER_ACTION],
                }],
            })),
            denied: false,
        },
        PolicyCase {
            case: "a deny of the manifest call alone",
            policy: Some(json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "Service": LAMBDA_PRINCIPAL },
                    "Action": MANIFEST_ACTION,
                }],
            })),
            denied: true,
        },
        PolicyCase {
            case: "a deny of the layer call alone",
            policy: Some(json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "Service": LAMBDA_PRINCIPAL },
                    "Action": LAYER_ACTION,
                }],
            })),
            denied: true,
        },
        PolicyCase {
            case: "the recorded case: a deny of both pull calls to this service",
            policy: Some(json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "Service": LAMBDA_PRINCIPAL },
                    "Action": [MANIFEST_ACTION, LAYER_ACTION],
                }],
            })),
            denied: true,
        },
        PolicyCase {
            case: "a deny written with an action wildcard",
            policy: Some(json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "Service": LAMBDA_PRINCIPAL },
                    "Action": "ecr:*",
                }],
            })),
            denied: true,
        },
        PolicyCase {
            case: "a deny carrying a Sid, which names the statement and nothing else",
            policy: Some(json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Sid": "DenyThisServiceThePull",
                    "Effect": "Deny",
                    "Principal": { "Service": LAMBDA_PRINCIPAL },
                    "Action": [MANIFEST_ACTION, LAYER_ACTION],
                }],
            })),
            denied: true,
        },
        PolicyCase {
            case: "a deny aimed at the wildcard principal, which covers this service too",
            policy: Some(json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": "*",
                    "Action": [MANIFEST_ACTION, LAYER_ACTION],
                }],
            })),
            denied: true,
        },
        PolicyCase {
            case: "a deny aimed at another service's principal",
            policy: Some(json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "Service": OTHER_PRINCIPAL },
                    "Action": [MANIFEST_ACTION, LAYER_ACTION],
                }],
            })),
            denied: false,
        },
        PolicyCase {
            case: "a deny of an action no pull makes",
            policy: Some(json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "Service": LAMBDA_PRINCIPAL },
                    "Action": "ecr:PutImage",
                }],
            })),
            denied: false,
        },
        PolicyCase {
            case: "an allow and a deny of the same pull, where the explicit deny wins",
            policy: Some(json!({
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Principal": { "Service": LAMBDA_PRINCIPAL },
                        "Action": [MANIFEST_ACTION, LAYER_ACTION],
                    },
                    {
                        "Effect": "Deny",
                        "Principal": { "Service": LAMBDA_PRINCIPAL },
                        "Action": MANIFEST_ACTION,
                    },
                ],
            })),
            denied: true,
        },
    ]
}

/// The rule: a `CreateFunction` naming an image whose repository policy denies this
/// service's own principal the pull is refused 403 `AccessDeniedException` with AWS's fixed
/// sentence and creates nothing; a repository that denies it nothing is served and the
/// function is pinned to the digest the registry minted.
#[tokio::test]
async fn a_repository_policy_may_deny_this_service_the_image_pull() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);

    let rows = policy_cases();
    for (index, row) in rows.into_iter().enumerate() {
        // Each row gets its own repository, which is what a deny of it reaches.
        let repository = format!("policy-{index}");
        let pushed = push_image(&router, ECR_HOST, &repository, "v1").await;
        if let Some(policy) = &row.policy {
            set_repository_policy(&router, ECR_HOST, &repository, policy).await;
        }

        let name = format!("pull-{index}");
        let image_uri = format!("{}:v1", pushed.announced_uri);
        let answer = lambda_call(
            &router,
            LAMBDA_HOST,
            "POST",
            "/2015-03-31/functions",
            &image_create(&name, &image_uri),
        )
        .await;
        if row.denied {
            assert_refused_access(&router, LAMBDA_HOST, row.case, &name, &answer).await;
            continue;
        }
        assert_eq!(
            answer.status, 201,
            "case {:?}: expected 201 from CreateFunction, got {}",
            row.case,
            answer.summary()
        );
        make_activation_due(&engine);
        let read = get_function(&router, LAMBDA_HOST, &name).await;
        let expected = format!("{}@{}", pushed.announced_uri, pushed.digest);
        let resolved = read
            .json()
            .pointer("/Code/ResolvedImageUri")
            .and_then(Value::as_str)
            .map(str::to_string);
        assert_eq!(
            resolved.as_deref(),
            Some(expected.as_str()),
            "case {:?}: expected {expected:?}, got {resolved:?}",
            row.case
        );
    }
}

/// One row of the addressing table: the endpoint the client is configured with.
struct EndpointCase {
    case: &'static str,
    /// The `Host` the ECR calls carry.
    registry_host: &'static str,
    /// The `Host` the Lambda calls carry.
    lambda_host: &'static str,
}

/// The rule's addressing half: the reference is followed wherever the registry is
/// addressed, so the URI the registry ANNOUNCED resolves exactly like AWS's own registry
/// host — whichever endpoint the client is configured with — and the pull is decided for
/// it the same way.
#[tokio::test]
async fn the_announced_repository_uri_resolves_like_the_aws_registry_host() {
    let rows = [
        EndpointCase {
            case: "a client that resolves AWS's own service hosts",
            registry_host: ECR_HOST,
            lambda_host: LAMBDA_HOST,
        },
        EndpointCase {
            case: "the recorded case: a client configured with the emulator's loopback \
                   endpoint",
            registry_host: "127.0.0.1:39355",
            lambda_host: "127.0.0.1:39355",
        },
        EndpointCase {
            case: "the same endpoint named by its loopback name",
            registry_host: "localhost:9297",
            lambda_host: "localhost:9297",
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator(&engine);

        // One repository the registry denies this service nothing on, and one it denies the
        // pull on, both named by the URI the registry announced for them.
        let served = push_image(&router, row.registry_host, "served", "v1").await;
        let refused = push_image(&router, row.registry_host, "refused", "v1").await;
        set_repository_policy(
            &router,
            row.registry_host,
            "refused",
            &json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": { "Service": LAMBDA_PRINCIPAL },
                    "Action": [MANIFEST_ACTION, LAYER_ACTION],
                }],
            }),
        )
        .await;

        // The DIGEST-addressed form of the announced URI, which is what a client that just
        // pushed the image holds.
        let name = format!("announced-{index}");
        let image_uri = format!("{}@{}", served.announced_uri, served.digest);
        let answer = lambda_call(
            &router,
            row.lambda_host,
            "POST",
            "/2015-03-31/functions",
            &image_create(&name, &image_uri),
        )
        .await;
        assert_eq!(
            answer.status, 201,
            "case {:?}: expected 201 from CreateFunction, got {}",
            row.case,
            answer.summary()
        );
        make_activation_due(&engine);
        let read = get_function(&router, row.lambda_host, &name).await;
        let resolved = read
            .json()
            .pointer("/Code/ResolvedImageUri")
            .and_then(Value::as_str)
            .map(str::to_string);
        assert_eq!(
            resolved.as_deref(),
            Some(image_uri.as_str()),
            "case {:?}: expected {image_uri:?}, got {resolved:?}",
            row.case
        );

        // The denied repository, addressed the same way, is the permission failure.
        let denied_name = format!("announced-denied-{index}");
        let denied_uri = format!("{}@{}", refused.announced_uri, refused.digest);
        let denied = lambda_call(
            &router,
            row.lambda_host,
            "POST",
            "/2015-03-31/functions",
            &image_create(&denied_name, &denied_uri),
        )
        .await;
        assert_refused_access(&router, row.lambda_host, row.case, &denied_name, &denied).await;

        // And an address that is neither this registry's announced one nor an AWS registry
        // host names no image of this Region's registry at all, which stays the modelled
        // 400 about the parameter.
        let absent_name = format!("announced-absent-{index}");
        let absent_uri = "registry.example:5000/served:v1";
        let absent = lambda_call(
            &router,
            row.lambda_host,
            "POST",
            "/2015-03-31/functions",
            &image_create(&absent_name, absent_uri),
        )
        .await;
        assert_eq!(
            absent.status, 400,
            "case {:?}: expected 400 from CreateFunction, got {}",
            row.case,
            absent.summary()
        );
        assert_eq!(
            error_code(&absent),
            INVALID_PARAMETER,
            "case {:?}: expected {INVALID_PARAMETER}, got {}",
            row.case,
            error_code(&absent)
        );
        let expected =
            format!("Source image {absent_uri} is not valid. Provide a valid source image.");
        assert_eq!(
            error_message(&absent).as_deref(),
            Some(expected.as_str()),
            "case {:?}: expected {expected:?}, got {:?}",
            row.case,
            error_message(&absent)
        );
    }
}

/// The same rule on the OTHER write that stores a container-image package: a code update
/// naming an image this service may not pull is refused the same way, and the function
/// keeps the package it had.
#[tokio::test]
async fn a_code_update_to_a_denied_image_is_refused_and_changes_nothing() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);

    let first = push_image(&router, ECR_HOST, "updatable", "v1").await;
    let next = push_image(&router, ECR_HOST, "locked-down", "v1").await;
    set_repository_policy(
        &router,
        ECR_HOST,
        "locked-down",
        &json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Deny",
                "Principal": { "Service": LAMBDA_PRINCIPAL },
                "Action": [MANIFEST_ACTION, LAYER_ACTION],
            }],
        }),
    )
    .await;

    let created = lambda_call(
        &router,
        LAMBDA_HOST,
        "POST",
        "/2015-03-31/functions",
        &image_create("updated", &format!("{}:v1", first.announced_uri)),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "expected 201 from CreateFunction, got {}",
        created.summary()
    );
    make_activation_due(&engine);

    let answer = lambda_call(
        &router,
        LAMBDA_HOST,
        "PUT",
        "/2015-03-31/functions/updated/code",
        &json!({ "ImageUri": format!("{}@{}", next.announced_uri, next.digest) }),
    )
    .await;
    let case = "a code update to an image this service may not pull";
    assert_eq!(
        answer.status, ACCESS_DENIED_STATUS,
        "case {case:?}: expected {ACCESS_DENIED_STATUS}, got {}",
        answer.summary()
    );
    assert_eq!(
        error_code(&answer),
        ACCESS_DENIED,
        "case {case:?}: expected {ACCESS_DENIED}, got {}",
        error_code(&answer)
    );
    assert_eq!(
        error_message(&answer).as_deref(),
        Some(ACCESS_DENIED_MESSAGE),
        "case {case:?}: expected {ACCESS_DENIED_MESSAGE:?}, got {:?}",
        error_message(&answer)
    );

    // The package the function had is the one it still has.
    make_activation_due(&engine);
    let read = get_function(&router, LAMBDA_HOST, "updated").await;
    let expected = format!("{}@{}", first.announced_uri, first.digest);
    let resolved = read
        .json()
        .pointer("/Code/ResolvedImageUri")
        .and_then(Value::as_str)
        .map(str::to_string);
    assert_eq!(
        resolved.as_deref(),
        Some(expected.as_str()),
        "case {case:?}: expected the function to keep {expected:?}, got {resolved:?}"
    );
}

/// The tag a reference that names one carries in the rows below.
const PUSHED_TAG: &str = "v1";

/// The tag a reference that names NONE addresses, which is the one a bare `docker push`
/// writes (the ECR crate's `registry_pull::DEFAULT_IMAGE_TAG`).
const DEFAULT_TAG: &str = "latest";

/// The host AWS serves THIS account's registry in this suite's Region at
/// (`<account-id>.dkr.ecr.<region>.<domain>`), which is the other spelling a private
/// repository is named by beside the address the registry announces here.
fn aws_registry_host() -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com")
}

/// A repository policy that denies this service both pull calls, which is the deny the rule
/// turns into the permission failure.
fn deny_pull_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": { "Service": LAMBDA_PRINCIPAL },
            "Action": [MANIFEST_ACTION, LAYER_ACTION],
        }],
    })
}

/// Which address a reference carries the repository at.
#[derive(Debug, Clone, Copy)]
enum Authority {
    /// AWS's own registry host for this account and Region.
    AwsRegistryHost,
    /// The address the registry ANNOUNCED for the repository, which is what a client that
    /// pushed the image here was handed.
    Announced,
}

/// How a reference addresses the image inside the repository.
#[derive(Debug, Clone, Copy)]
enum Addressed {
    /// `<repository>:<tag>`.
    Tag,
    /// A bare reference, which names the registry's default tag.
    DefaultTag,
    /// `<repository>@<digest>`.
    Digest,
}

impl Addressed {
    /// The tag an image has to be pushed under for this form to address it.
    fn pushed_tag(self) -> &'static str {
        match self {
            Addressed::DefaultTag => DEFAULT_TAG,
            Addressed::Tag | Addressed::Digest => PUSHED_TAG,
        }
    }

    /// `repository_uri` addressed this way, for the image the registry minted `digest` for.
    fn reference(self, repository_uri: &str, digest: &str) -> String {
        match self {
            Addressed::Tag => format!("{repository_uri}:{PUSHED_TAG}"),
            Addressed::DefaultTag => repository_uri.to_string(),
            Addressed::Digest => format!("{repository_uri}@{digest}"),
        }
    }
}

/// The repository `name` at the address `authority` names, where `announced_uri` is what the
/// registry announced for it.
fn repository_uri(authority: Authority, announced_uri: &str, name: &str) -> String {
    match authority {
        Authority::AwsRegistryHost => format!("{}/{name}", aws_registry_host()),
        Authority::Announced => announced_uri.to_string(),
    }
}

/// The rule holds over how the reference is WRITTEN, not just over the policy: the pull is
/// decided for a reference carrying AWS's own registry host exactly as for the address the
/// registry announced, and for a tag, a bare reference naming the default tag, and a digest
/// alike — a repository that denies this service the pull refuses the write 403 with AWS's
/// fixed sentence and stores nothing, and one that denies it nothing is served and pins the
/// function to the digest the registry minted.
#[tokio::test]
async fn the_pull_is_decided_however_the_reference_names_the_image() {
    // (case, the address the reference carries, how it addresses the image)
    let rows: [(&str, Authority, Addressed); 6] = [
        (
            "AWS's registry host with a tag",
            Authority::AwsRegistryHost,
            Addressed::Tag,
        ),
        (
            "AWS's registry host with no tag, which names the default one",
            Authority::AwsRegistryHost,
            Addressed::DefaultTag,
        ),
        (
            "AWS's registry host with a digest",
            Authority::AwsRegistryHost,
            Addressed::Digest,
        ),
        (
            "the announced address with a tag",
            Authority::Announced,
            Addressed::Tag,
        ),
        (
            "the announced address with no tag",
            Authority::Announced,
            Addressed::DefaultTag,
        ),
        (
            "the recorded case: the announced address with a digest",
            Authority::Announced,
            Addressed::Digest,
        ),
    ];

    for (index, (case, authority, addressed)) in rows.into_iter().enumerate() {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator(&engine);

        // One repository this registry denies nothing on, and one whose own policy denies
        // this service the pull; each row pushes under the tag its reference addresses.
        let served_name = format!("served-{index}");
        let denied_name = format!("denied-{index}");
        let served = push_image(&router, ECR_HOST, &served_name, addressed.pushed_tag()).await;
        let denied = push_image(&router, ECR_HOST, &denied_name, addressed.pushed_tag()).await;
        set_repository_policy(&router, ECR_HOST, &denied_name, &deny_pull_policy()).await;

        let served_repository = repository_uri(authority, &served.announced_uri, &served_name);
        let served_reference = addressed.reference(&served_repository, &served.digest);
        let function = format!("reference-{index}");
        let answer = lambda_call(
            &router,
            LAMBDA_HOST,
            "POST",
            "/2015-03-31/functions",
            &image_create(&function, &served_reference),
        )
        .await;
        assert_eq!(
            answer.status,
            201,
            "case {case:?}: expected 201 from CreateFunction for {served_reference:?}, got {}",
            answer.summary()
        );
        make_activation_due(&engine);
        let read = get_function(&router, LAMBDA_HOST, &function).await;
        // The repository as the request spelled it, pinned to the digest the registry
        // minted for the image the reference resolved to.
        let expected = format!("{served_repository}@{}", served.digest);
        let resolved = read
            .json()
            .pointer("/Code/ResolvedImageUri")
            .and_then(Value::as_str)
            .map(str::to_string);
        assert_eq!(
            resolved.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {resolved:?}"
        );

        let denied_repository = repository_uri(authority, &denied.announced_uri, &denied_name);
        let denied_reference = addressed.reference(&denied_repository, &denied.digest);
        let refused_function = format!("reference-denied-{index}");
        let refused = lambda_call(
            &router,
            LAMBDA_HOST,
            "POST",
            "/2015-03-31/functions",
            &image_create(&refused_function, &denied_reference),
        )
        .await;
        assert_refused_access(&router, LAMBDA_HOST, case, &refused_function, &refused).await;
    }
}

/// The boundary of the rule's resolution half: a reference carrying AWS's registry host for
/// ANOTHER Region, or another account's registry id, names no image of the function's own
/// Region's registry, so it stays the modelled 400 about the parameter — a deny on a
/// repository of that name here is not what such a reference is refused for.
#[tokio::test]
async fn a_reference_into_another_registry_is_no_image_of_this_one() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator(&engine);

    let pushed = push_image(&router, ECR_HOST, "elsewhere", PUSHED_TAG).await;
    set_repository_policy(&router, ECR_HOST, "elsewhere", &deny_pull_policy()).await;

    // Another Region of this account's registries, and another account's registry in this
    // Region: an Amazon ECR registry is one account's in one Region.
    let rows = [
        (
            "another Region's registry host",
            format!("{ACCOUNT}.dkr.ecr.eu-west-1.amazonaws.com/elsewhere:{PUSHED_TAG}"),
        ),
        (
            "a host that is no registry host at all",
            format!("registry.example:5000/elsewhere:{PUSHED_TAG}"),
        ),
    ];

    for (index, (case, image_uri)) in rows.into_iter().enumerate() {
        let name = format!("elsewhere-{index}");
        let answer = lambda_call(
            &router,
            LAMBDA_HOST,
            "POST",
            "/2015-03-31/functions",
            &image_create(&name, &image_uri),
        )
        .await;
        assert_eq!(
            answer.status,
            400,
            "case {case:?}: expected 400 from CreateFunction, got {}",
            answer.summary()
        );
        assert_eq!(
            error_code(&answer),
            INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {}",
            error_code(&answer)
        );
        let expected =
            format!("Source image {image_uri} is not valid. Provide a valid source image.");
        assert_eq!(
            error_message(&answer).as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {:?}",
            error_message(&answer)
        );
        let read = get_function(&router, LAMBDA_HOST, &name).await;
        assert_eq!(
            read.status,
            404,
            "case {case:?}: expected the refused function to be absent, got {}",
            read.summary()
        );
    }

    // The repository the deny IS attached to, named the way this registry announced it, is
    // still the permission failure — so the rows above were refused for the reference and
    // not for the policy.
    let reference = format!("{}:{PUSHED_TAG}", pushed.announced_uri);
    let refused = lambda_call(
        &router,
        LAMBDA_HOST,
        "POST",
        "/2015-03-31/functions",
        &image_create("elsewhere-denied", &reference),
    )
    .await;
    assert_refused_access(
        &router,
        LAMBDA_HOST,
        "the repository the deny is attached to",
        "elsewhere-denied",
        &refused,
    )
    .await;
}
