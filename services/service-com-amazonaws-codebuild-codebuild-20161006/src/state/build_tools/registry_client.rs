// SPDX-License-Identifier: CC0-1.0

//! The registry DATA PLANE as a container client reaches it: the fetch a `docker pull`
//! performs against one of this emulator's own registries.
//!
//! # The rule this module implements
//!
//! A pull is not an API call. Amazon ECR serves the Docker Registry HTTP API V2 on the
//! registry endpoint it publishes, and a client pulling an image fetches the MANIFEST that
//! endpoint holds for the reference it asked for and then each BLOB that manifest names, with
//! the `docker login` credential presented as HTTP Basic authorization — the credential that
//! registry's own `GetAuthorizationToken` minted ([`super::aws_cli`]) and the login recorded
//! ([`super::docker_cli`]). So the image a pull delivers here is the image the registry really
//! holds: its stored manifest and the very bytes of the blobs that manifest references,
//! verified against the digests it names, because an image is addressed by content.
//!
//! Nothing is fetched off this emulator: the request is dispatched over the framework's
//! in-process callback, exactly as every other call a build's commands make, so the registry
//! it reaches is one this emulator serves and no outbound connection is opened. A registry
//! endpoint nothing here serves answers nothing, and the pull reports that rather than
//! inventing an image.
//!
//! WHETHER the build's role may pull is decided before any of this
//! ([`super::docker_cli`]): the registry endpoint authorizes a request by the credential it
//! presents, and the credential a build holds was minted for this emulator's own caller, so
//! the actions AWS documents a pull under are decided against the build's SERVICE ROLE where
//! every other call made for a build is.
//!
//! # Bounds
//!
//! Every bound is this emulator's and none is reachable from a command: how many blobs one
//! pulled image carries ([`MAX_PULLED_BLOBS`]) and how many bytes it may come to
//! ([`super::container_image::MAX_IMAGE_BYTES`], the same bound an image composed in this
//! process is held to), so what one `docker pull` costs this process is bounded by this
//! module rather than by what a registry answers.

use super::container_image::{self, LocalImage};
use super::BuildTools;

/// The user half of the `user:password` pair an Amazon ECR authorization token decodes to,
/// which is what a client presents as the Basic credential's user.
const TOKEN_USER: &str = "AWS";

/// The path space the Docker Registry HTTP API V2 defines its whole surface beneath.
const REGISTRY_V2_PATH_PREFIX: &str = "/v2";

/// The manifest schemas a pull accepts, as a client lists them: the Docker image manifest V2
/// schema 2 a push of this emulator stores, and the OCI image manifest a registry may answer
/// the same image in.
const MANIFEST_ACCEPT: &str = "application/vnd.docker.distribution.manifest.v2+json, \
                               application/vnd.oci.image.manifest.v1+json";

/// How many blobs ONE pulled image carries: its configuration and its filesystem layers.
///
/// The bound an image this environment holds carries either way
/// ([`container_image::MAX_IMAGE_LAYERS`], and the configuration blob beside them): every blob
/// is held in this process for the length of the build, so how many one pull may bring in is
/// bounded with how many a composition may stack. A manifest naming more is one this
/// environment does not pull rather than one it grows this process for.
const MAX_PULLED_BLOBS: usize = container_image::MAX_IMAGE_LAYERS + 1;

/// What a pull delivered: the image the registry holds, and the digest the registry addresses
/// it by (the digest of the manifest served).
#[derive(Debug, Clone)]
pub(super) struct PulledImage {
    /// The image itself, as the environment then holds it.
    pub image: LocalImage,
    /// The digest of the manifest the registry served, which is the image's own.
    pub digest: String,
}

/// Fetch the image `reference` addresses in the repository `repository` of the registry at
/// `registry`, presenting `password` — the `docker login` credential that registry issued.
///
/// `Err` carries what the command reports instead: the registry's own refusal, a manifest
/// that is not one, a blob whose bytes do not address the digest the manifest named, or an
/// image past the bounds above.
pub(super) fn fetched(
    tools: &BuildTools,
    registry: &str,
    password: &str,
    repository: &str,
    reference: &str,
) -> Result<PulledImage, String> {
    let manifest = fetched_bytes(
        tools,
        registry,
        password,
        &format!("{REGISTRY_V2_PATH_PREFIX}/{repository}/manifests/{reference}"),
        MANIFEST_ACCEPT,
    )?;
    let document: ::serde_json::Value = ::serde_json::from_slice(&manifest).map_err(|_| {
        format!("Error response from daemon: the manifest {registry}/{repository} answered for {reference} is not a manifest")
    })?;
    let config_digest = document["config"]["digest"]
        .as_str()
        .ok_or_else(|| {
            format!(
                "Error response from daemon: the manifest for {registry}/{repository}:\
                 {reference} names no image configuration"
            )
        })?
        .to_string();
    // Every entry of the manifest's own `layers`, in its order: an entry naming no digest
    // addresses no content, so a document carrying one is not a manifest this client can
    // honour rather than one it pulls part of.
    let declared = document["layers"].as_array().cloned().unwrap_or_default();
    let layer_digests: Vec<String> = declared
        .iter()
        .filter_map(|layer| layer["digest"].as_str().map(str::to_string))
        .collect();
    if layer_digests.len() != declared.len() {
        return Err(format!(
            "Error response from daemon: the manifest for {registry}/{repository}:{reference} \
             names a filesystem layer with no digest"
        ));
    }
    if layer_digests.is_empty() {
        return Err(format!(
            "Error response from daemon: the manifest for {registry}/{repository}:{reference} \
             names no filesystem layer"
        ));
    }
    if layer_digests.len() + 1 > MAX_PULLED_BLOBS {
        return Err(format!(
            "Error response from daemon: a pull carries at most {MAX_PULLED_BLOBS} blobs, and \
             the manifest for {registry}/{repository}:{reference} names more"
        ));
    }
    let mut pulled_bytes: usize = 0;
    let mut blob_of = |digest: &str| -> Result<Vec<u8>, String> {
        let blob = fetched_blob(tools, registry, password, repository, digest)?;
        pulled_bytes = pulled_bytes.saturating_add(blob.len());
        if pulled_bytes > container_image::MAX_IMAGE_BYTES {
            return Err(format!(
                "Error response from daemon: a pull carries at most {} bytes, and \
                 {registry}/{repository}:{reference} is larger",
                container_image::MAX_IMAGE_BYTES
            ));
        }
        Ok(blob)
    };
    let config = blob_of(&config_digest)?;
    let mut layers: Vec<Vec<u8>> = Vec::new();
    for digest in &layer_digests {
        layers.push(blob_of(digest)?);
    }
    Ok(PulledImage {
        image: LocalImage::pulled(config, layers),
        // The image's own digest is the digest of its manifest, which is what the registry
        // stored it under and what a client verifies the document it received by.
        digest: container_image::content_digest(&manifest),
    })
}

/// Fetch ONE blob of `repository` and answer its bytes, checking that they address `digest`:
/// a blob is addressed by content, so bytes whose digest is not the one the manifest named
/// are not the content that was asked for.
fn fetched_blob(
    tools: &BuildTools,
    registry: &str,
    password: &str,
    repository: &str,
    digest: &str,
) -> Result<Vec<u8>, String> {
    let blob = fetched_bytes(
        tools,
        registry,
        password,
        &format!("{REGISTRY_V2_PATH_PREFIX}/{repository}/blobs/{digest}"),
        "*/*",
    )?;
    let addressed = container_image::content_digest(&blob);
    if addressed != digest {
        return Err(format!(
            "Error response from daemon: filesystem layer verification failed for digest \
             {digest}"
        ));
    }
    Ok(blob)
}

/// `GET` one address of the registry at `registry`, presenting `password`, and answer the
/// bytes it served.
///
/// The request is the one a registry client makes: the Basic credential the login recorded,
/// the schemas the client accepts, and the host it dialled. It is dispatched over the
/// in-process callback, inside what this build's commands have left of their call budget
/// ([`super::TOOL_CALL_BUDGET`]).
///
/// `Err` carries the registry's own refusal, as the registry API states one: a
/// `{"errors":[{"code","message"}]}` document, which is what a client reports back.
fn fetched_bytes(
    tools: &BuildTools,
    registry: &str,
    password: &str,
    path: &str,
    accept: &str,
) -> Result<Vec<u8>, String> {
    use ::aws_smithy_runtime_api::client::http::HttpConnector as _;

    let credential = ::aws_smithy_types::base64::encode(format!("{TOKEN_USER}:{password}"));
    let request = ::http::Request::builder()
        .method("GET")
        .uri(format!("https://{registry}{path}"))
        .header(::http::header::HOST, registry)
        .header(::http::header::AUTHORIZATION, format!("Basic {credential}"))
        .header(::http::header::ACCEPT, accept)
        .body(::aws_smithy_types::body::SdkBody::empty())
        .map_err(|_| format!("Error response from daemon: {registry} is not a registry address"))?;
    let request = ::aws_smithy_runtime_api::http::Request::try_from(request)
        .map_err(|_| format!("Error response from daemon: {registry} is not a registry address"))?;
    let answer = tools.made(|| async {
        tools
            .callback
            .http_connector()
            .call(request)
            .await
            .map_err(|_| {
                format!("Error response from daemon: Get \"https://{registry}{path}\": the registry endpoint could not be reached")
            })
    })?;
    let status = answer.status().as_u16();
    let body = answer
        .body()
        .bytes()
        .map(<[u8]>::to_vec)
        .unwrap_or_default();
    if status != 200 {
        return Err(refusal(registry, path, status, &body));
    }
    // What one fetch may bring into this process, bounded here where the bytes arrive: an
    // answer larger than one image is not one this environment pulls.
    if body.len() > container_image::MAX_IMAGE_BYTES {
        return Err(format!(
            "Error response from daemon: a pull carries at most {} bytes, and \
             https://{registry}{path} answered more",
            container_image::MAX_IMAGE_BYTES
        ));
    }
    Ok(body)
}

/// What a registry answered a request it did not serve, as a client reports it: the code and
/// the sentence the registry API's own error document carries, and the status alone for an
/// answer that is no such document.
fn refusal(registry: &str, path: &str, status: u16, body: &[u8]) -> String {
    let stated = ::serde_json::from_slice::<::serde_json::Value>(body)
        .ok()
        .and_then(|document| {
            let error = document["errors"].get(0)?.clone();
            let code = error["code"].as_str()?.to_string();
            let message = error["message"].as_str().unwrap_or_default().to_string();
            Some(format!("{code}: {message}"))
        });
    match stated {
        Some(stated) => format!("Error response from daemon: {stated}"),
        None => format!(
            "Error response from daemon: Get \"https://{registry}{path}\": the registry \
             answered status {status}"
        ),
    }
}
