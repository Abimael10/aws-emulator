// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ADDRESS a container-image reference carries, one level above
//! the recorded case that filed the issue (a function created from an image a client had
//! just pushed, named by the loopback `host:port` that client was handed).
//!
//! The rule:
//!
//! > "For a container image, the code property must include the URI of a container image in
//! > the Amazon ECR registry" (`CreateFunction`), and Lambda FOLLOWS that reference into the
//! > registry of the function's own Region whenever the reference's authority addresses a
//! > registry this emulator serves — AWS's own
//! > `<account-id>.dkr.ecr.<region>.<domain>` host, and the address the client reached this
//! > emulator at, which is the one a container client on this machine can dial and therefore
//! > the one it pushed and names the image by. An image the registry holds under the
//! > reference is stored (HTTP 201, `PackageType=Image`) and the function is frozen to the
//! > manifest digest the registry minted; only a reference that resolves to no image of that
//! > registry is refused `InvalidParameterValueException`, `Source image <uri> is not valid.
//! > Provide a valid source image.` where the authority is no registry of this Region at
//! > all, and `Source image <uri> does not exist. Provide a valid source image.` where it is
//! > but nothing was pushed under it.
//!
//! The rows span what the rule generalises over: the shape of the address the client reached
//! the emulator at (an IPv4 loopback literal, two different ports, a loopback name, a
//! bracketed IPv6 literal, the authority written back in another case) beside AWS's own
//! registry host; how the reference addresses the image inside the repository (a tag, a bare
//! reference naming the default tag, a digest); whether the repository is namespaced; which
//! part of a refused reference is wrong (the authority, the Region, the repository, the tag,
//! the digest); and the write that stores the package (`CreateFunction` and
//! `UpdateFunctionCode`).
//!
//! Every image here is pushed through Amazon ECR's OWN `CreateRepository`/`PutImage` on the
//! SAME engine (`function_lifecycle::seed_image`), so the digest asserted is the one the
//! registry minted rather than one this test computed. The host-parameterised sender is
//! `function_url_data_plane.rs`'s, because the address the reference carries is the address
//! the client is configured with.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::function_lifecycle::{
    make_activation_due, make_update_due, seed_image, ACCOUNT, REGION, ROLE,
};
use crate::function_url_data_plane::{router_on, send, Answer};

/// The service host an SDK that resolves AWS's own hostnames is configured with.
const SERVICE_HOST: &str = "lambda.us-east-1.amazonaws.com";

/// The tag every image in this suite is pushed under, where the reference names one.
const PUSHED_TAG: &str = "v1";

/// The code a reference that resolves to no image of this Region's registry is refused with.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// A SigV4 credential scope naming `lambda` in the Region the whole suite signs for — which
/// is the Region a create resolves its image in. Neither the key id nor the signature is a
/// credential: the harness router has request authentication off, exactly as the emulator
/// runs it unless started with it on.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/lambda/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda control-plane call on the restJson1 wire, addressed at `host` — the endpoint
/// the client is configured with.
async fn lambda_call(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answer {
    let scope = authorization();
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

/// A `CreateFunction` request body for a container-image package.
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

/// A string member of a JSON body, by JSON pointer.
fn at(body: &Value, pointer: &str) -> Option<String> {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// AWS's own registry host for this account and Region, which is the spelling a client that
/// resolves AWS's hostnames carries.
fn aws_registry_host() -> String {
    format!("{ACCOUNT}.dkr.ecr.{REGION}.amazonaws.com")
}

/// How a reference addresses the image inside the repository it names.
#[derive(Debug, Clone, Copy)]
enum Addressed {
    /// `<repository>:<tag>`.
    Tag,
    /// A bare reference, which names the registry's default tag.
    DefaultTag,
    /// `<repository>@<digest>`, which is what a client that just pushed the image holds.
    Digest,
}

/// One row of the resolution table.
struct AddressCase {
    /// What makes this row its own case.
    case: &'static str,
    /// The endpoint the client is configured with, which is the address its request reaches
    /// this emulator at.
    endpoint: &'static str,
    /// The authority the reference CARRIES, which is normally the endpoint above.
    authority: String,
    /// The repository the image is pushed to, namespaced in one row.
    repository: &'static str,
    /// How the reference addresses the image.
    addressed: Addressed,
}

/// Rule: a reference whose authority addresses a registry this emulator serves — the address
/// the client reached it at, or AWS's own registry host — is followed into this Region's
/// registry, so an image that registry holds is stored and the function is pinned to the
/// digest the registry minted.
#[tokio::test]
async fn an_image_named_by_the_reached_address_is_resolved_in_the_registry() {
    let rows = [
        AddressCase {
            case: "the recorded case: a loopback literal with a port, by tag",
            endpoint: "127.0.0.1:36157",
            authority: "127.0.0.1:36157".to_string(),
            repository: "app",
            addressed: Addressed::Tag,
        },
        AddressCase {
            case: "the same literal on another port",
            endpoint: "127.0.0.1:39355",
            authority: "127.0.0.1:39355".to_string(),
            repository: "app",
            addressed: Addressed::Tag,
        },
        AddressCase {
            case: "the loopback name, with no tag at all",
            endpoint: "localhost:9297",
            authority: "localhost:9297".to_string(),
            repository: "app",
            addressed: Addressed::DefaultTag,
        },
        AddressCase {
            case: "a bracketed IPv6 loopback literal, by digest",
            endpoint: "[::1]:9297",
            authority: "[::1]:9297".to_string(),
            repository: "app",
            addressed: Addressed::Digest,
        },
        AddressCase {
            case: "the reached authority written back in another case",
            endpoint: "localhost:9297",
            authority: "LOCALHOST:9297".to_string(),
            repository: "app",
            addressed: Addressed::Tag,
        },
        AddressCase {
            case: "a namespaced repository at the reached address",
            endpoint: "127.0.0.1:9297",
            authority: "127.0.0.1:9297".to_string(),
            repository: "team/app",
            addressed: Addressed::Tag,
        },
        AddressCase {
            case: "AWS's own registry host, from a client that resolves it",
            endpoint: SERVICE_HOST,
            authority: aws_registry_host(),
            repository: "app",
            addressed: Addressed::Tag,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(&engine);

        // The push the client made before it ever called Lambda: a digest-addressed row is
        // pushed under a tag too, because a digest is what the registry MINTS for a push.
        let repository = format!("{}/{}", row.authority, row.repository);
        let pushed = match row.addressed {
            Addressed::DefaultTag => repository.clone(),
            Addressed::Tag | Addressed::Digest => format!("{repository}:{PUSHED_TAG}"),
        };
        let digest = seed_image(&engine, &pushed).await;
        assert!(
            digest.starts_with("sha256:"),
            "case {:?}: expected the registry to mint a sha256 digest, got {digest:?}",
            row.case
        );

        let image_uri = match row.addressed {
            Addressed::Tag => format!("{repository}:{PUSHED_TAG}"),
            Addressed::DefaultTag => repository.clone(),
            Addressed::Digest => format!("{repository}@{digest}"),
        };
        let name = format!("reached-{index}");
        let answer = lambda_call(
            &router,
            row.endpoint,
            "POST",
            "/2015-03-31/functions",
            &image_create(&name, &image_uri),
        )
        .await;
        assert_eq!(
            answer.status, 201,
            "case {:?}: expected 201 from CreateFunction for {image_uri:?}, got {}",
            row.case,
            answer.summary()
        );
        let reported = at(&answer.json(), "/PackageType");
        assert_eq!(
            reported.as_deref(),
            Some("Image"),
            "case {:?}: expected PackageType Image, got {reported:?}",
            row.case
        );

        // The round trip: the package the function was created with is the one a read
        // reports, pinned to the digest the registry minted for the reference.
        make_activation_due(&engine);
        let read = get_function(&router, row.endpoint, &name).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: expected 200 from GetFunction, got {}",
            row.case,
            read.summary()
        );
        let body = read.json();
        let supplied = at(&body, "/Code/ImageUri");
        assert_eq!(
            supplied.as_deref(),
            Some(image_uri.as_str()),
            "case {:?}: expected the ImageUri as supplied, got {supplied:?}",
            row.case
        );
        let expected = format!("{repository}@{digest}");
        let resolved = at(&body, "/Code/ResolvedImageUri");
        assert_eq!(
            resolved.as_deref(),
            Some(expected.as_str()),
            "case {:?}: expected {expected:?}, got {resolved:?}",
            row.case
        );
    }
}

/// Rule: only a reference that resolves to no image of this Region's registry is refused —
/// "is not valid" where its authority is no registry of this Region, "does not exist" where
/// it is but nothing was pushed under the reference — and nothing is created either way.
#[tokio::test]
async fn a_reference_that_resolves_to_no_image_is_refused() {
    /// The endpoint the client in this suite reached the emulator at.
    const ENDPOINT: &str = "127.0.0.1:36157";

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let digest = seed_image(&engine, &format!("{ENDPOINT}/app:{PUSHED_TAG}")).await;

    // (case, the reference the create names, the fixed part of the refusal)
    let rows: [(&str, String, &str); 7] = [
        (
            "an address the client never reached",
            format!("127.0.0.1:1/app:{PUSHED_TAG}"),
            "is not valid",
        ),
        (
            "a registry that is not this one",
            "docker.io/library/alpine:3".to_string(),
            "is not valid",
        ),
        (
            "the public registry",
            "public.ecr.aws/nginx/nginx:latest".to_string(),
            "is not valid",
        ),
        (
            "another Region's registry",
            format!("{ACCOUNT}.dkr.ecr.eu-west-1.amazonaws.com/app:{PUSHED_TAG}"),
            "is not valid",
        ),
        (
            "the reached address, with a tag nothing was pushed under",
            format!("{ENDPOINT}/app:never"),
            "does not exist",
        ),
        (
            "the reached address, with a repository this registry does not hold",
            format!("{ENDPOINT}/absent:{PUSHED_TAG}"),
            "does not exist",
        ),
        (
            "the reached address, with a digest nothing was pushed under",
            format!("{ENDPOINT}/app@sha256:{}", "0".repeat(64)),
            "does not exist",
        ),
    ];

    for (index, (case, image_uri, fixed)) in rows.iter().enumerate() {
        let name = format!("refused-{index}");
        let answer = lambda_call(
            &router,
            ENDPOINT,
            "POST",
            "/2015-03-31/functions",
            &image_create(&name, image_uri),
        )
        .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected 400 from CreateFunction, got {}",
            answer.summary()
        );
        assert_eq!(
            error_code(&answer),
            INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {}",
            error_code(&answer)
        );
        let expected = format!("Source image {image_uri} {fixed}. Provide a valid source image.");
        assert_eq!(
            error_message(&answer).as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {:?}",
            error_message(&answer)
        );
        // Nothing was created: the read of the function it would have been is the modelled
        // 404.
        make_activation_due(&engine);
        let read = get_function(&router, ENDPOINT, &name).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: expected the refused function to be absent, got {}",
            read.summary()
        );
    }

    // And the one reference that DOES resolve, at that same endpoint, is served — so the
    // rows above were refused for what they name and not for the address they carry.
    let image_uri = format!("{ENDPOINT}/app:{PUSHED_TAG}");
    let served = lambda_call(
        &router,
        ENDPOINT,
        "POST",
        "/2015-03-31/functions",
        &image_create("served", &image_uri),
    )
    .await;
    assert_eq!(
        served.status, 201,
        "case {image_uri:?}: expected 201 from CreateFunction, got {}",
        served.summary()
    );
    make_activation_due(&engine);
    let expected = format!("{ENDPOINT}/app@{digest}");
    let resolved = at(
        &get_function(&router, ENDPOINT, "served").await.json(),
        "/Code/ResolvedImageUri",
    );
    assert_eq!(
        resolved.as_deref(),
        Some(expected.as_str()),
        "case {image_uri:?}: expected {expected:?}, got {resolved:?}"
    );
}

/// Rule: `UpdateFunctionCode` follows a new reference the same way, so a code update naming
/// an image at the reached address re-pins the function, and one naming nothing pushed is
/// refused and leaves the package the function had.
#[tokio::test]
async fn a_code_update_follows_the_reached_address_too() {
    /// The endpoint the client in this test reached the emulator at.
    const ENDPOINT: &str = "127.0.0.1:39355";

    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let first = seed_image(&engine, &format!("{ENDPOINT}/app:{PUSHED_TAG}")).await;
    let next = seed_image(&engine, &format!("{ENDPOINT}/app:v2")).await;
    assert_ne!(
        first, next,
        "expected two manifests to have two digests, got {first:?} twice"
    );

    let created = lambda_call(
        &router,
        ENDPOINT,
        "POST",
        "/2015-03-31/functions",
        &image_create("updated", &format!("{ENDPOINT}/app:{PUSHED_TAG}")),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "expected 201 from CreateFunction, got {}",
        created.summary()
    );
    make_update_due(&engine);

    // The update that names an image the registry holds re-pins the function to it.
    let case = "a new tag at the reached address";
    let updated = lambda_call(
        &router,
        ENDPOINT,
        "PUT",
        "/2015-03-31/functions/updated/code",
        &json!({ "ImageUri": format!("{ENDPOINT}/app:v2") }),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "case {case:?}: expected 200 from UpdateFunctionCode, got {}",
        updated.summary()
    );
    make_update_due(&engine);
    let expected = format!("{ENDPOINT}/app@{next}");
    let resolved = at(
        &get_function(&router, ENDPOINT, "updated").await.json(),
        "/Code/ResolvedImageUri",
    );
    assert_eq!(
        resolved.as_deref(),
        Some(expected.as_str()),
        "case {case:?}: expected {expected:?}, got {resolved:?}"
    );

    // And one that names nothing pushed is refused, leaving the package the function has.
    let case = "a tag nothing was pushed under";
    let absent = format!("{ENDPOINT}/app:never");
    let refused = lambda_call(
        &router,
        ENDPOINT,
        "PUT",
        "/2015-03-31/functions/updated/code",
        &json!({ "ImageUri": absent }),
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected 400 from UpdateFunctionCode, got {}",
        refused.summary()
    );
    assert_eq!(
        error_code(&refused),
        INVALID_PARAMETER,
        "case {case:?}: expected {INVALID_PARAMETER}, got {}",
        error_code(&refused)
    );
    let worded = format!("Source image {absent} does not exist. Provide a valid source image.");
    assert_eq!(
        error_message(&refused).as_deref(),
        Some(worded.as_str()),
        "case {case:?}: expected {worded:?}, got {:?}",
        error_message(&refused)
    );
    make_update_due(&engine);
    let resolved = at(
        &get_function(&router, ENDPOINT, "updated").await.json(),
        "/Code/ResolvedImageUri",
    );
    assert_eq!(
        resolved.as_deref(),
        Some(expected.as_str()),
        "case {case:?}: expected the function to keep {expected:?}, got {resolved:?}"
    );
}
