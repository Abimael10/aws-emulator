// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the MicroVM data plane of an image whose guest is declared ONLY by
//! its container specification: a `Dockerfile` whose command starts an interpreter's own
//! HTTP FILE-SERVER module over content the image holds, rather than a source file of the
//! code artifact.
//!
//! Each test states a RULE and exercises it over a SET of inputs; the paths, ports and
//! contents any one row carries are examples, not the specification. The control plane is
//! driven over the wire through the same router the emulator boots, the code artifact is
//! staged in the emulator's own object store, and the endpoint is driven through
//! `state::microvm_endpoint::serve_request` — the one decision the core's HTTPS listener
//! hands every request on a published MicroVM address to.
//!
//! Nothing asserted below is a value this emulator chose: each body is the content the
//! image's own build steps wrote or the artifact shipped, served by the module the command
//! started, and each port is the one that invocation named.

use std::io::Write;
use std::sync::Arc;

use serde_json::json;
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::microvm_endpoint::{
    serve_request, AUTH_TOKEN_HEADER, NO_GUEST_RESPONSE_STATUS, PROXY_PORT_HEADER,
};
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::microvm_network::connector_state;
use service_com_amazonaws_lambdamicrovms_lambdamicrovms::state::ComAmazonawsLambdamicrovmsLambdamicrovmsState as MicrovmState;
use smithy_server_runtime::MultiServiceRouter;

use crate::microvm_guest_data_plane::{emulator, stage_artifact};
use crate::microvm_lifecycle::{call, image_body, poll_until, run_microvm, REGION};

/// The base image every artifact below declares — the public image a client's own
/// specification starts from, which this emulator never fetches: what it reads of the
/// specification is the steps it declares and the command it starts.
const BASE_IMAGE: &str = "FROM public.ecr.aws/amazonlinux/amazonlinux:2023";

/// The port the first artifact's invocation names, which is therefore the port its
/// endpoint fronts.
const BAKED_PORT: i32 = 8080;

/// The port the second artifact's invocation names.
const SHIPPED_PORT: i32 = 9090;

/// The bucket prefix the artifacts below are staged in. Each test stages its own, because
/// the object store a staged artifact is read from is reached by name.
const BUCKET_PREFIX: &str = "mvm-static-guest";

/// The key a client uploads its build context under.
const ARTIFACT_KEY: &str = "code.zip";

/// What a row expects of the body the guest answered.
#[derive(Debug)]
enum Body<'a> {
    /// Exactly these bytes — the content the image holds, byte for byte.
    Is(&'a str),
    /// A body carrying this, for an answer the server composes (a directory listing).
    Carries(&'a str),
    /// No body at all, which is what a status carrying no document answers.
    None,
}

/// The `.zip` a client uploads as a MicroVM image's code artifact, holding `entries`.
pub(crate) fn artifact(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        for (name, contents) in entries {
            writer.start_file(*name, options).expect("the entry starts");
            writer
                .write_all(contents.as_bytes())
                .expect("the entry's bytes");
        }
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create the image whose code artifact is the object at `uri` and poll it until its build
/// has settled with an active version.
async fn settled_image(router: &MultiServiceRouter, name: &str, uri: &str) -> String {
    let (status, created) = call(
        router,
        "POST",
        "/2025-09-09/microvm-images",
        Some(image_body(name, json!({"codeArtifact": {"uri": uri}}))),
    )
    .await;
    assert_eq!(status, 201, "create the image {name}: got {status} {created}");
    let image_arn = created["imageArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let polled = router.clone();
    let image = name.to_string();
    poll_until(name, "the image settles with an active version", move || {
        let router = polled.clone();
        let image = image.clone();
        async move {
            let (status, read) = call(
                &router,
                "GET",
                &format!("/2025-09-09/microvm-images/{image}"),
                None,
            )
            .await;
            status == 200 && read["latestActiveImageVersion"].is_string()
        }
    })
    .await;
    image_arn
}

/// Run a MicroVM from `image` with the managed HTTP ingress connector and poll it until it
/// reports `RUNNING`, then answer its id.
async fn running_microvm(router: &MultiServiceRouter, image: &str) -> String {
    let (status, run) = run_microvm(
        router,
        json!({
            "imageIdentifier": image,
            "maximumDurationInSeconds": 600,
            "ingressNetworkConnectors": [format!(
                "arn:aws:lambda:{REGION}:aws:network-connector:aws-network-connector:HTTP_INGRESS"
            )],
        }),
    )
    .await;
    assert_eq!(status, 200, "run a MicroVM from {image}: got {status} {run}");
    let microvm_id = run["microvmId"].as_str().unwrap_or_default().to_string();
    assert!(
        run["endpoint"].as_str().is_some_and(|value| !value.is_empty()),
        "the MicroVM publishes an endpoint: got {run}"
    );
    let polled = router.clone();
    let addressed = microvm_id.clone();
    poll_until(image, "the MicroVM reaches RUNNING", move || {
        let router = polled.clone();
        let addressed = addressed.clone();
        async move {
            let (status, read) = call(
                &router,
                "GET",
                &format!("/2025-09-09/microvms/{addressed}"),
                None,
            )
            .await;
            status == 200 && read["state"].as_str() == Some("RUNNING")
        }
    })
    .await;
    microvm_id
}

/// Mint an authentication token for every port of `microvm_id`.
async fn auth_token(router: &MultiServiceRouter, microvm_id: &str) -> String {
    let (status, minted) = call(
        router,
        "POST",
        &format!("/2025-09-09/microvms/{microvm_id}/auth-token"),
        Some(json!({"expirationInMinutes": 15, "allowedPorts": [{"allPorts": {}}]})),
    )
    .await;
    assert_eq!(status, 200, "mint a token for {microvm_id}: got {status} {minted}");
    minted["authToken"][AUTH_TOKEN_HEADER]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// One request through a MicroVM's published endpoint: the guest's own path in full, the
/// token where the client presents one, and the port it addresses where it names one.
async fn through_endpoint(
    standing: &Standing,
    microvm_id: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    port: Option<i32>,
) -> (u16, String, String) {
    let mut builder = http::Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header(AUTH_TOKEN_HEADER, token);
    }
    if let Some(port) = port {
        builder = builder.header(PROXY_PORT_HEADER, port.to_string());
    }
    let request = builder
        .body(Vec::new())
        .expect("a well-formed request to a MicroVM endpoint");
    let state = standing.engine.state::<MicrovmState>();
    let connectors = connector_state(&*standing.engine);
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(standing.router.clone());
    let response = serve_request(
        &state,
        connectors.as_ref(),
        &callback,
        microvm_id,
        &request,
    )
    .await;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (
        status,
        content_type,
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// The three MicroVMs these suites stand up, each from an image whose command starts the
/// interpreter's own server module a different way.
struct Standing {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
    /// The content is written by the build's own `RUN` steps and served from the directory
    /// the command changes into.
    baked: String,
    /// The content SHIPS in the artifact and is served from the directory the command runs
    /// in, on the port the invocation names.
    shipped: String,
    /// The directory is the one the invocation names, and the port its module's own
    /// documented default.
    pointed: String,
}

/// The container specification whose build steps BAKE the content its command serves, and
/// whose command changes into that directory before starting the interpreter's module.
fn baked_specification() -> String {
    format!(
        "{BASE_IMAGE}\n\
         RUN dnf install -y python3 && dnf clean all\n\
         RUN mkdir -p /srv/pages\n\
         RUN echo baked-by-the-build > /srv/probe.txt\n\
         RUN echo the-index-document > /srv/index.html\n\
         RUN echo nested-by-the-build > /srv/pages/deep.txt\n\
         CMD [\"/bin/sh\",\"-c\",\"cd /srv && python3 -m http.server {BAKED_PORT}\"]\n"
    )
}

/// ONE MicroVM of the baked image, for a rule that needs a static guest rather than three
/// of them: the fewest resources that prove it.
async fn one_microvm(label: &str) -> Standing {
    let (router, engine) = emulator();
    let bucket = format!("{BUCKET_PREFIX}-{label}-baked");
    let specification = baked_specification();
    stage_artifact(
        &router,
        &bucket,
        ARTIFACT_KEY,
        artifact(&[("Dockerfile", specification.as_str())]),
    )
    .await;
    let image = settled_image(
        &router,
        &format!("static-baked-{label}"),
        &format!("s3://{bucket}/{ARTIFACT_KEY}"),
    )
    .await;
    let baked = running_microvm(&router, &image).await;
    Standing {
        router,
        engine,
        shipped: baked.clone(),
        pointed: baked.clone(),
        baked,
    }
}

/// Stand that up, with `label` naming this test's own buckets and images.
async fn standing(label: &str) -> Standing {
    let (router, engine) = emulator();
    let bucket = format!("{BUCKET_PREFIX}-{label}");
    let baked = baked_specification();
    let shipped = format!("{BASE_IMAGE}\nCMD [\"python3\",\"-m\",\"http.server\",\"{SHIPPED_PORT}\"]\n");
    let pointed = format!(
        "{BASE_IMAGE}\n\
         RUN mkdir -p /srv/site\n\
         RUN echo the-pointed-document > /srv/site/index.html\n\
         CMD [\"python3\",\"-u\",\"-m\",\"http.server\",\"--directory\",\"/srv/site\"]\n"
    );
    // (the image's name, the entries of the artifact it is built from). Each is staged in
    // a bucket of its own, under the key a client uploads its build context as.
    let staged: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("baked", vec![("Dockerfile", baked.as_str())]),
        (
            "shipped",
            vec![
                ("Dockerfile", shipped.as_str()),
                ("page.html", "<p>shipped in the artifact</p>\n"),
                ("data.json", "{\"shipped\":true}\n"),
            ],
        ),
        ("pointed", vec![("Dockerfile", pointed.as_str())]),
    ];
    let mut images = Vec::new();
    for (name, entries) in &staged {
        let bucket = format!("{bucket}-{name}");
        stage_artifact(&router, &bucket, ARTIFACT_KEY, artifact(entries)).await;
        let image = format!("static-{name}-{label}");
        images.push(
            settled_image(&router, &image, &format!("s3://{bucket}/{ARTIFACT_KEY}")).await,
        );
    }
    let mut running = Vec::new();
    for image in &images {
        running.push(running_microvm(&router, image).await);
    }
    Standing {
        router,
        engine,
        baked: running[0].clone(),
        shipped: running[1].clone(),
        pointed: running[2].clone(),
    }
}

/// One row of the served-document table: the case, the MicroVM asked, the method and path,
/// and the status, content type and body its guest must answer with.
type ServedRow<'a> = (&'a str, &'a str, &'a str, &'a str, u16, &'a str, Body<'a>);

/// RULE: a MicroVM whose image's command starts an interpreter's own HTTP file-server
/// module runs THAT server as its guest — an authenticated request to the published
/// endpoint is answered out of the files the image holds under the directory the command
/// runs in, with the media type the document's own name names; a path holding nothing is
/// answered with the server's not-found status, a directory holding no index document with
/// a listing of what it holds, and a method it serves no document for with its own refusal.
#[tokio::test(flavor = "multi_thread")]
async fn a_command_that_starts_the_interpreters_server_module_serves_the_images_own_files() {
    let standing = standing("served").await;
    let tokens = [
        auth_token(&standing.router, &standing.baked).await,
        auth_token(&standing.router, &standing.shipped).await,
        auth_token(&standing.router, &standing.pointed).await,
    ];
    // The rows differ in WHERE the content came from (a build step, the artifact), WHICH
    // directory the command serves (one it changed into, the one it runs in, one it names),
    // the depth and media type of the document, and what the server answers for a path it
    // holds nothing at.
    let rows: Vec<ServedRow> = vec![
        (
            "a document a build step wrote, in the directory the command changed into",
            &standing.baked,
            "GET",
            "/probe.txt",
            200,
            "text/plain",
            Body::Is("baked-by-the-build\n"),
        ),
        (
            "the index document of that directory",
            &standing.baked,
            "GET",
            "/",
            200,
            "text/html",
            Body::Is("the-index-document\n"),
        ),
        (
            "a document a step wrote deeper under it",
            &standing.baked,
            "GET",
            "/pages/deep.txt",
            200,
            "text/plain",
            Body::Is("nested-by-the-build\n"),
        ),
        (
            "a directory holding no index document",
            &standing.baked,
            "GET",
            "/pages",
            200,
            "text/html; charset=utf-8",
            Body::Carries("deep.txt"),
        ),
        (
            "a path the image holds nothing at",
            &standing.baked,
            "GET",
            "/absent.txt",
            404,
            "",
            Body::None,
        ),
        (
            "a method a static server serves no document for",
            &standing.baked,
            "POST",
            "/probe.txt",
            405,
            "",
            Body::None,
        ),
        (
            "a document the artifact shipped, from the directory the command runs in",
            &standing.shipped,
            "GET",
            "/page.html",
            200,
            "text/html",
            Body::Is("<p>shipped in the artifact</p>\n"),
        ),
        (
            "another media type beside it",
            &standing.shipped,
            "GET",
            "/data.json",
            200,
            "application/json",
            Body::Is("{\"shipped\":true}\n"),
        ),
        (
            "the index document of the directory the invocation names",
            &standing.pointed,
            "GET",
            "/",
            200,
            "text/html",
            Body::Is("the-pointed-document\n"),
        ),
        (
            "a path outside the directory the invocation names",
            &standing.pointed,
            "GET",
            "/../probe.txt",
            404,
            "",
            Body::None,
        ),
    ];
    for (case, microvm, method, path, expected_status, expected_type, expected_body) in rows {
        let token = if microvm == standing.baked {
            &tokens[0]
        } else if microvm == standing.shipped {
            &tokens[1]
        } else {
            &tokens[2]
        };
        let (status, content_type, body) = through_endpoint(
            &standing,
            microvm,
            method,
            path,
            Some(token.as_str()),
            None,
        )
        .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} {content_type:?} \
             {body:?}"
        );
        assert_eq!(
            content_type, expected_type,
            "case {case:?}: expected the content type {expected_type:?}, got {content_type:?}"
        );
        match expected_body {
            Body::Is(expected) => assert_eq!(
                body, expected,
                "case {case:?}: expected the body {expected:?}, got {body:?}"
            ),
            Body::Carries(expected) => assert!(
                body.contains(expected),
                "case {case:?}: expected a body carrying {expected:?}, got {body:?}"
            ),
            Body::None => assert!(
                body.is_empty(),
                "case {case:?}: expected no body at all, got {body:?}"
            ),
        }
    }

    // The other half of the same rule, read off the same three MicroVMs: the port each
    // invocation named is the port its endpoint fronts.
    the_fronted_port_is_the_one_each_invocation_named(&standing).await;
}

/// RULE: the port the endpoint FRONTS is the port the guest is listening on — for this
/// guest, the port its own invocation named, else that module's documented default — so a
/// request addressing that port is served and one addressing a port nothing inside the
/// MicroVM bound reaches nothing there.
///
/// Asserted over the SAME standing as the documents above: the two halves of the rule are
/// read off the same three MicroVMs rather than standing three more up.
async fn the_fronted_port_is_the_one_each_invocation_named(standing: &Standing) {
    // (case, the MicroVM, the path it serves, the port its invocation named)
    let rows: Vec<(&str, &str, &str, i32)> = vec![
        (
            "the port the shell program's invocation named",
            &standing.baked,
            "/probe.txt",
            BAKED_PORT,
        ),
        (
            "the port the direct invocation named",
            &standing.shipped,
            "/page.html",
            SHIPPED_PORT,
        ),
        (
            "the module's own default, which the invocation named nothing instead of",
            &standing.pointed,
            "/",
            service_common::static_web_servers::DEFAULT_MODULE_PORT,
        ),
    ];
    for (case, microvm, path, port) in rows {
        let token = auth_token(&standing.router, microvm).await;
        let (status, _, body) = through_endpoint(
            standing,
            microvm,
            "GET",
            path,
            Some(token.as_str()),
            Some(port),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a request addressing port {port} is the guest's own to answer, \
             got {status} {body:?}"
        );
        // A port nothing inside the MicroVM bound: the proxy reached nothing there,
        // whatever the path.
        let other = if port == BAKED_PORT { 9999 } else { BAKED_PORT };
        let (status, _, body) = through_endpoint(
            standing,
            microvm,
            "GET",
            path,
            Some(token.as_str()),
            Some(other),
        )
        .await;
        assert_eq!(
            status, NO_GUEST_RESPONSE_STATUS,
            "case {case:?}: port {other} is one nothing inside the MicroVM bound, got \
             {status} {body:?}"
        );
    }
}

/// RULE: the PROXY decides authentication before the guest is reached at all, so the same
/// request that the guest answers is refused with `403` when it carries no authentication —
/// and the refusal names what the request is missing.
#[tokio::test(flavor = "multi_thread")]
async fn the_proxy_refuses_an_unauthenticated_request_to_a_static_guest() {
    // ONE MicroVM: what this rule reads is the proxy's own decision, which is the same for
    // every static guest, so the smallest image that has one is enough.
    let standing = one_microvm("refusal").await;
    let token = auth_token(&standing.router, &standing.baked).await;
    // (case, the token the client presents, the status the endpoint answers)
    let rows: Vec<(&str, Option<&str>, u16)> = vec![
        ("a request carrying a live token", Some(token.as_str()), 200),
        ("the same request carrying no authentication", None, 403),
    ];
    for (case, presented, expected) in rows {
        let (status, _, body) = through_endpoint(
            &standing,
            &standing.baked,
            "GET",
            "/probe.txt",
            presented,
            None,
        )
        .await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected HTTP {expected}, got {status} {body:?}"
        );
        if expected == 403 {
            assert!(
                body.contains("Request missing authentication"),
                "case {case:?}: expected the refusal to name the missing authentication, \
                 got {body:?}"
            );
        }
    }
}
