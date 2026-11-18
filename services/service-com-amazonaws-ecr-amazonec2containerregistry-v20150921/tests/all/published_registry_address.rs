// SPDX-License-Identifier: CC0-1.0

//! The registry address this service reports for a repository and for a `docker login`,
//! driven end to end over the wire.
//!
//! ONE rule is under test, because the model states one kind of value twice: both
//! `Repository$repositoryUri` ("The URI for the repository. You can use this URI for
//! container image `push` and `pull` operations") and `AuthorizationData$proxyEndpoint`
//! ("The registry URL to use for this authorization token in a `docker login` command") are
//! ADDRESSES a container client DIALS next — a `docker login`, then a push or a pull — so
//! each names the endpoint THIS emulator answers that caller at, with the repository's own
//! name as the path of the URI and the same endpoint behind a scheme for the login URL. AWS
//! reports them on its own registry host (`https://aws_account_id.dkr.ecr.region.amazonaws.com`),
//! which resolves nowhere near this process, and a name under a zone this emulator is
//! reached in resolves nowhere either; the endpoint the caller is already talking to is the
//! one address it can reach. The two agree, which is what lets a client log in where it
//! pushes.
//!
//! The class the rule generalises over is the endpoint the call arrived on crossed with the
//! repository name and the operation reporting it, so the rows span the endpoint forms a
//! caller reaches this emulator by — the registry host ECR publishes (a host in AWS's own
//! zone, which this emulator does not answer on), the emulator's own socket, a loopback name
//! with a port, an endpoint carried only on the request URI, this service's control-plane
//! endpoint host (which names a service, so no registry client can use it) and a `Host` that
//! is no endpoint at all — crossed with the repository-name forms `RepositoryName`'s pattern
//! allows. No row is the recorded request; the recorded case is one name at one endpoint.
//!
//! The last test follows the reported URI the way a container client does: it pulls the
//! manifest at the very address the `repositoryUri` named, with the credential
//! `GetAuthorizationToken` issued, so the URI is proved to address the repository in this
//! registry rather than merely to carry the right shape.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{
    at, endpoint_host, given_layer, ok, registry_get, ANNOUNCED_REGISTRY_ENDPOINT, REGION,
    REGISTRY_ID,
};

/// One endpoint a caller reaches this emulator on: where the endpoint travels (the `Host`
/// header, the request URI's authority, or neither) and the address a registry address
/// reported to that call is built on, port included.
struct Endpoint {
    case: &'static str,
    host: Option<String>,
    uri_authority: Option<&'static str>,
    announced_endpoint: String,
}

impl Endpoint {
    /// The registry URL a `docker login` of such a call is pointed at.
    fn login_url(&self) -> String {
        format!("https://{}", self.announced_endpoint)
    }
}

/// The endpoints the table drives, spanning the forms a caller reaches this emulator by.
fn endpoints() -> Vec<Endpoint> {
    vec![
        Endpoint {
            case: "the registry endpoint host ECR publishes for this account and Region",
            host: Some(format!("{REGISTRY_ID}.dkr.ecr.{REGION}.amazonaws.com")),
            uri_authority: None,
            // A host in a zone AWS publishes its OWN endpoints in resolves nowhere near
            // this process, so this caller is reported the address this emulator is served
            // at instead.
            announced_endpoint: ANNOUNCED_REGISTRY_ENDPOINT.to_string(),
        },
        Endpoint {
            case: "the emulator's own socket",
            host: Some("127.0.0.1:9297".to_string()),
            uri_authority: None,
            announced_endpoint: "127.0.0.1:9297".to_string(),
        },
        Endpoint {
            case: "a loopback name with another port",
            host: Some("localhost:39355".to_string()),
            uri_authority: None,
            announced_endpoint: "localhost:39355".to_string(),
        },
        Endpoint {
            case: "an endpoint carried only on the request URI",
            host: None,
            uri_authority: Some("127.0.0.1:8443"),
            announced_endpoint: "127.0.0.1:8443".to_string(),
        },
        Endpoint {
            case: "this service's control-plane endpoint host, which names a service",
            host: Some(endpoint_host(REGION)),
            uri_authority: None,
            // No registry client can use a host that routes to a service, so the address
            // this emulator is served at is reported instead.
            announced_endpoint: ANNOUNCED_REGISTRY_ENDPOINT.to_string(),
        },
        Endpoint {
            case: "a Host that is no endpoint at all",
            host: Some("elsewhere.example.com/registry".to_string()),
            uri_authority: None,
            announced_endpoint: ANNOUNCED_REGISTRY_ENDPOINT.to_string(),
        },
    ]
}

/// The media type a Docker v2 manifest is pushed and served under.
const DOCKER_V2_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";

/// The media type of the config blob a Docker v2 manifest references.
const DOCKER_CONFIG_MEDIA_TYPE: &str = "application/vnd.docker.container.image.v1+json";

/// The media type of a layer blob a Docker v2 manifest references.
const DOCKER_LAYER_MEDIA_TYPE: &str =
    "application/vnd.docker.image.rootfs.diff.tar.gzip";

/// The repository names the table creates, spanning the forms `RepositoryName`'s pattern
/// allows — `^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$`
/// with `@length(min: 2, max: 256)` — read with:
///
/// ```text
/// smithy_query(query="shape",shape="Repository",model_path="services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/smithy-model/com-amazonaws-ecr-amazonec2containerregistry-v20150921.json")
/// ```
const REPOSITORY_NAMES: [&str; 5] = ["app", "ab", "team/app", "org/team/app", "a.b_c--d"];

/// One emulator: a router carrying only this service, over one shared engine, served at the
/// address this harness's emulator answers on (`crate::wire::SERVED_URL_BASE`).
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.set_url_base(
        ::smithy_server_runtime::EndpointUrl::parse(crate::wire::SERVED_URL_BASE)
            .expect("the base URL must parse"),
    );
    router.register_service(EmulatedService::new(engine));
    router
}

/// Call one operation of this service, addressed to `endpoint` — the harness in
/// `crate::wire` fixes the endpoint a call arrives on, and this suite is what varies it.
async fn call_at(
    router: &MultiServiceRouter,
    endpoint: &Endpoint,
    operation: &str,
    body: &Value,
) -> Value {
    let uri = match endpoint.uri_authority {
        Some(authority) => format!("https://{authority}/"),
        None => "/".to_string(),
    };
    let mut request = ::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/ecr/aws4_request, \
                 SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AmazonEC2ContainerRegistry_V20150921.{operation}"),
        );
    if let Some(host) = &endpoint.host {
        request = request.header("host", host.clone());
    }
    let request = request
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let answered: Value = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "case {:?}: {operation} answered a body that is not JSON ({error}): {:?}",
            endpoint.case,
            String::from_utf8_lossy(&bytes)
        )
    });
    assert_eq!(
        status, 200,
        "case {:?}: expected HTTP 200 from {operation}, got {status} with {answered}",
        endpoint.case
    );
    answered
}

/// The string at `pointer`, or a panic naming the case and the body that carried none.
fn text(case: &str, body: &Value, pointer: &str) -> String {
    at(body, pointer)
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string at {pointer}, got {body}"))
        .to_string()
}

#[tokio::test]
async fn every_reported_repository_uri_addresses_this_emulators_registry() {
    for endpoint in endpoints() {
        let router = emulator();
        for name in REPOSITORY_NAMES {
            let case = format!("{}, repository {name}", endpoint.case);
            // The address this emulator answers this caller at, plus the repository's own
            // name.
            let expected = format!("{}/{name}", endpoint.announced_endpoint);

            let created = call_at(
                &router,
                &endpoint,
                "CreateRepository",
                &json!({"repositoryName": name}),
            )
            .await;
            let created_uri = text(&case, &created, "/repository/repositoryUri");
            assert_eq!(
                created_uri, expected,
                "case {case:?}: expected CreateRepository to report {expected:?}, got \
                 {created_uri:?}"
            );

            // The two parts a container client splits the value into: the address it dials,
            // which is never a host in AWS's own zone, and the repository path it composes
            // a tag, a digest or a registry v2 path on.
            let (authority, path) = created_uri
                .split_once('/')
                .unwrap_or_else(|| panic!("case {case:?}: {created_uri:?} names no path"));
            assert!(
                !authority.contains(".amazonaws.com"),
                "case {case:?}: the authority {authority:?} of {created_uri:?} is an AWS \
                 host this emulator does not answer on"
            );
            assert_eq!(
                path, name,
                "case {case:?}: expected the path of {created_uri:?} to be the repository \
                 name {name:?}, got {path:?}"
            );

            // Which registry holds the repository is reported by the members that carry
            // that identity, read off the stored record: they are LABELS, so no endpoint
            // moves them.
            let registry_id = text(&case, &created, "/repository/registryId");
            assert_eq!(
                registry_id, REGISTRY_ID,
                "case {case:?}: expected the owning account {REGISTRY_ID:?}, got \
                 {registry_id:?}"
            );
            let arn = text(&case, &created, "/repository/repositoryArn");
            assert_eq!(
                arn,
                format!("arn:aws:ecr:{REGION}:{registry_id}:repository/{name}"),
                "case {case:?}: expected the ARN to name the signed Region, got {arn:?}"
            );

            // The list read is how a client discovers where to pull from, so every entry
            // carries the same URI.
            let listed = call_at(
                &router,
                &endpoint,
                "DescribeRepositories",
                &json!({"repositoryNames": [name]}),
            )
            .await;
            let listed_uri = text(&case, &listed, "/repositories/0/repositoryUri");
            assert_eq!(
                listed_uri, expected,
                "case {case:?}: expected DescribeRepositories to report {expected:?}, got \
                 {listed_uri:?}"
            );

            // The echoed repository a delete reports carries the same URI.
            let deleted = call_at(
                &router,
                &endpoint,
                "DeleteRepository",
                &json!({"repositoryName": name}),
            )
            .await;
            let deleted_uri = text(&case, &deleted, "/repository/repositoryUri");
            assert_eq!(
                deleted_uri, expected,
                "case {case:?}: expected DeleteRepository to report {expected:?}, got \
                 {deleted_uri:?}"
            );

            // The `docker login` endpoint is the same address behind a scheme: the client
            // logs in where it pushes, and never at a host in AWS's own zone.
            let issued = call_at(&router, &endpoint, "GetAuthorizationToken", &json!({})).await;
            let proxy_endpoint = text(&case, &issued, "/authorizationData/0/proxyEndpoint");
            let expected_login = endpoint.login_url();
            assert_eq!(
                proxy_endpoint, expected_login,
                "case {case:?}: expected the token's registry URL to be \
                 {expected_login:?}, got {proxy_endpoint:?}"
            );
            assert!(
                !proxy_endpoint.contains(".amazonaws.com"),
                "case {case:?}: the login URL {proxy_endpoint:?} is an AWS host this \
                 emulator does not answer on"
            );
        }
    }
}

#[tokio::test]
async fn a_repository_uri_names_the_endpoint_the_read_was_answered_at() {
    // The stored repository is one record, and its ARN — a LABEL — follows from that record
    // alone. Its URI is an ADDRESS, so what the reader's endpoint changes is the endpoint
    // the value names: a client is never handed a push address it cannot reach.
    let router = emulator();
    let endpoints = endpoints();
    let creating = &endpoints[0];
    let name = "reported-per-reader";
    let created = call_at(
        &router,
        creating,
        "CreateRepository",
        &json!({"repositoryName": name}),
    )
    .await;
    let registry_id = text(creating.case, &created, "/repository/registryId");
    let created_uri = text(creating.case, &created, "/repository/repositoryUri");
    assert_eq!(
        created_uri,
        format!("{}/{name}", creating.announced_endpoint),
        "case {:?}: expected the create to report the endpoint it was answered at, got \
         {created_uri:?}",
        creating.case
    );

    for endpoint in &endpoints {
        let expected = format!("{}/{name}", endpoint.announced_endpoint);
        let listed = call_at(&router, endpoint, "DescribeRepositories", &json!({})).await;
        let listed_uri = text(endpoint.case, &listed, "/repositories/0/repositoryUri");
        assert_eq!(
            listed_uri, expected,
            "case {:?}: expected the read to report {expected:?}, got {listed_uri:?}",
            endpoint.case
        );
        let arn = text(endpoint.case, &listed, "/repositories/0/repositoryArn");
        assert_eq!(
            arn,
            format!("arn:aws:ecr:{REGION}:{registry_id}:repository/{name}"),
            "case {:?}: expected the stored ARN to be unchanged by the read's endpoint, \
             got {arn:?}",
            endpoint.case
        );
        let issued = call_at(&router, endpoint, "GetAuthorizationToken", &json!({})).await;
        let proxy_endpoint = text(endpoint.case, &issued, "/authorizationData/0/proxyEndpoint");
        assert_eq!(
            proxy_endpoint,
            endpoint.login_url(),
            "case {:?}: expected the login URL to name the endpoint this read was answered \
             at, got {proxy_endpoint:?}",
            endpoint.case
        );
        // The one thing the pairing is FOR: a client logs in where it pushes, so the host
        // half of the URI is the host half of the login URL.
        let login_host = proxy_endpoint
            .strip_prefix("https://")
            .unwrap_or(&proxy_endpoint)
            .to_string();
        assert_eq!(
            listed_uri.split_once('/').map(|(host, _)| host),
            Some(login_host.as_str()),
            "case {:?}: expected {listed_uri:?} to be pushed at the registry the login URL \
             {proxy_endpoint:?} names",
            endpoint.case
        );
    }
}

#[tokio::test]
async fn a_pull_at_the_reported_repository_uri_reaches_this_registry() {
    // What the rule is FOR: the URI must address the repository in THIS registry, so a
    // client that splits it into the address and the repository path exactly as a container
    // client does, and pulls the manifest there with the credential the token names,
    // arrives at the image this registry holds.
    for endpoint in endpoints() {
        let router = emulator();
        let name = "followed/by-a-client";
        let case = endpoint.case;

        let created = call_at(
            &router,
            &endpoint,
            "CreateRepository",
            &json!({"repositoryName": name}),
        )
        .await;
        let reported = text(case, &created, "/repository/repositoryUri");
        let (reported_host, reported_path) = reported
            .split_once('/')
            .unwrap_or_else(|| panic!("case {case:?}: {reported:?} names no repository path"));

        // One image, pushed through the documented sequence, so the pull serves what the
        // push stored.
        let config_bytes = b"announced-config".to_vec();
        let config = given_layer(&router, case, name, &config_bytes).await;
        let layer_bytes = b"announced-layer".to_vec();
        let layer = given_layer(&router, case, name, &layer_bytes).await;
        let manifest = json!({
            "schemaVersion": 2,
            "mediaType": DOCKER_V2_MANIFEST,
            "config": {
                "mediaType": DOCKER_CONFIG_MEDIA_TYPE,
                "digest": config,
                "size": config_bytes.len(),
            },
            "layers": [{
                "mediaType": DOCKER_LAYER_MEDIA_TYPE,
                "digest": layer,
                "size": layer_bytes.len(),
            }],
        })
        .to_string();
        ok(
            &router,
            case,
            "PutImage",
            &json!({
                "repositoryName": name,
                "imageManifest": manifest,
                "imageTag": "v1",
            }),
        )
        .await;

        // `docker login` presents the issued token as a Basic credential, which is what
        // the token decodes to (`AWS:<password>`).
        let issued = call_at(&router, &endpoint, "GetAuthorizationToken", &json!({})).await;
        let token = text(case, &issued, "/authorizationData/0/authorizationToken");

        let (status, body) = registry_get(
            &router,
            reported_host,
            &format!("/v2/{reported_path}/manifests/v1"),
            &token,
            Some(DOCKER_V2_MANIFEST),
        )
        .await;        assert_eq!(
            status, 200,
            "case {case:?}: a pull at the reported URI {reported:?} must reach this \
             registry, got HTTP {status} with {body:?}"
        );
        assert_eq!(
            body, manifest,
            "case {case:?}: expected the pushed manifest from the reported URI, got \
             {body:?}"
        );
    }
}
