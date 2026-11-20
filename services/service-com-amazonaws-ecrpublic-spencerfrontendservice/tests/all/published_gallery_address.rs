// SPDX-License-Identifier: CC0-1.0

//! The gallery ADDRESS this service publishes — for the registry and for every repository
//! in it — driven end to end over the wire.
//!
//! ONE rule is under test, because the model states one kind of value twice:
//! `Registry$registryUri` ("The URI of a public registry. The URI contains a universal
//! prefix and the registry alias") and `Repository$repositoryUri` ("The URI for the
//! repository. You can use this URI for container image `push` and `pull` operations") are
//! both ADDRESSES a container client DIALS, so each names an endpoint THIS emulator answers
//! on: the endpoint the CALLER reached the emulator at when a registry client can use it
//! (it names no service), and the address the emulator is served at otherwise. The real
//! gallery host resolves to AWS's own public registry rather than to this emulator, so a
//! URI naming it sends a push or a pull out of the emulator entirely.
//!
//! The class the rule generalises over is the endpoint a call arrived on crossed with the
//! repository name and the operation reporting the URI, so the rows span the endpoint forms
//! a caller reaches this emulator by — the emulator's own socket, a loopback name with
//! another port, an endpoint carried only on the request URI, this service's own endpoint
//! host (which names a service, so no registry client can use it), a `Host` that is no
//! endpoint at all, and no `Host` at all — crossed with the repository-name forms the
//! modelled `RepositoryName` pattern admits. No row is the recorded request.
//!
//! The composition AWS documents is preserved throughout: a repository's URI is the
//! registry's URI followed by the repository's name, and the four operations that publish
//! one (`CreateRepository`, `DescribeRepositories` filtered and unfiltered,
//! `DeleteRepository`, `DescribeRegistries`) agree for the same repository. The last test
//! is what the rule is FOR: the authority the published URI names is dialled, and this
//! emulator answers the registry API there.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_ecrpublic_spencerfrontendservice::state::{
    DEFAULT_REGISTRY_ALIAS, GALLERY_HOST,
};
use service_com_amazonaws_ecrpublic_spencerfrontendservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::repository_image_lifecycle::{call_addressed, ARN_PREFIX, REGISTRY_ID};

/// The address this emulator is SERVED at in this suite: the base a call that reached it on
/// no usable endpoint of its own is told to come back to.
const SERVED_URL_BASE: &str = "https://127.0.0.1:9297";

/// The `host[:port]` half of [`SERVED_URL_BASE`].
const SERVED_ENDPOINT: &str = "127.0.0.1:9297";

/// One endpoint a caller reaches this emulator on: where the endpoint travels (the `Host`
/// header, the request URI's authority, or neither) and the address every URI published to
/// that call is built on.
struct Endpoint {
    case: &'static str,
    host: Option<&'static str>,
    uri_authority: Option<&'static str>,
    announced: &'static str,
}

/// The endpoints the table drives, spanning the forms a caller reaches this emulator by.
fn endpoints() -> Vec<Endpoint> {
    vec![
        Endpoint {
            case: "the emulator's own socket",
            host: Some(SERVED_ENDPOINT),
            uri_authority: None,
            announced: SERVED_ENDPOINT,
        },
        Endpoint {
            case: "a loopback name with another port",
            host: Some("localhost:39355"),
            uri_authority: None,
            announced: "localhost:39355",
        },
        Endpoint {
            case: "an endpoint carried only on the request URI",
            host: None,
            uri_authority: Some("127.0.0.1:8443"),
            announced: "127.0.0.1:8443",
        },
        Endpoint {
            case: "this service's endpoint host, which names a service",
            host: Some("api.ecr-public.us-east-1.amazonaws.com"),
            uri_authority: None,
            announced: SERVED_ENDPOINT,
        },
        Endpoint {
            case: "a Host that is no endpoint at all",
            host: Some("elsewhere.example.com/gallery"),
            uri_authority: None,
            announced: SERVED_ENDPOINT,
        },
        Endpoint {
            case: "no endpoint on the request at all",
            host: None,
            uri_authority: None,
            announced: SERVED_ENDPOINT,
        },
    ]
}

/// The repository names the table creates, spanning the forms the modelled `RepositoryName`
/// pattern `^(?:[a-z0-9]+(?:[._-][a-z0-9]+)*/)*[a-z0-9]+(?:[._-][a-z0-9]+)*$` (length
/// 2..205) admits — a name at the shortest the bound allows, a flat name, one carrying each
/// separator, and namespaced names of two and three segments. Source: the model, via
/// `smithy_query(query="shape", shape="Repository")`.
const REPOSITORY_NAMES: [&str; 5] = [
    "ab",
    "nginx",
    "team.app_v1-2",
    "project-a/nginx-web-app",
    "org/team/app",
];

/// One emulator: a router carrying only this service over one shared engine, served at
/// [`SERVED_URL_BASE`].
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature itself is not what these cases are about; the credential scope below
    // still routes the request to `ecr-public`.
    router.set_request_authentication(false);
    router.set_url_base(
        ::smithy_server_runtime::EndpointUrl::parse(SERVED_URL_BASE)
            .expect("the base URL must parse"),
    );
    router.register_service(EmulatedService::new(engine));
    router
}

/// One `awsJson1_1` call that must answer 200, addressed to the endpoint `endpoint`
/// describes.
async fn call_at(
    router: &MultiServiceRouter,
    endpoint: &Endpoint,
    operation: &str,
    body: Value,
) -> Value {
    call_addressed(
        router,
        endpoint.case,
        endpoint.host,
        endpoint.uri_authority,
        operation,
        body,
    )
    .await
}

/// The string `value` carries, or a panic naming the case and what was read instead.
fn string_at(case: &str, value: &Value, label: &str) -> String {
    value
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {label}, got {value}"))
        .to_string()
}

/// The registry URI `DescribeRegistries` publishes to a call that arrived at `endpoint`,
/// asserted to be the announced endpoint scoped by the registry's alias.
async fn published_registry_uri(router: &MultiServiceRouter, endpoint: &Endpoint) -> String {
    let described = call_at(router, endpoint, "DescribeRegistries", json!({})).await;
    let uri = string_at(
        endpoint.case,
        &described["registries"][0]["registryUri"],
        "registryUri",
    );
    assert_eq!(
        uri,
        format!("{}/{DEFAULT_REGISTRY_ALIAS}", endpoint.announced),
        "case {:?}: expected the registry URI on the endpoint this emulator answers on, \
         got {uri:?}",
        endpoint.case
    );
    uri
}

/// The authority a published URI names, and the path a client extends on it.
fn dialled(case: &str, uri: &str) -> (String, String) {
    let (authority, path) = uri
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: {uri:?} names no path"));
    // The one thing a published address may never be: a host AWS serves and this emulator
    // does not.
    assert_ne!(
        authority, GALLERY_HOST,
        "case {case:?}: {uri:?} names the real gallery host, which this emulator does not \
         answer on"
    );
    assert!(
        !authority.ends_with(".amazonaws.com"),
        "case {case:?}: {uri:?} names an AWS endpoint host, which this emulator does not \
         answer on"
    );
    (authority.to_string(), path.to_string())
}

#[tokio::test]
async fn every_published_repository_uri_names_an_endpoint_this_emulator_answers_on() {
    for endpoint in endpoints() {
        let router = emulator();
        let registry_uri = published_registry_uri(&router, &endpoint).await;
        for name in REPOSITORY_NAMES {
            let case = format!("{}, repository {name}", endpoint.case);
            // The registry's URI followed by the repository's own name, which is the
            // composition a client splits and extends.
            let expected = format!("{registry_uri}/{name}");

            let created = call_at(
                &router,
                &endpoint,
                "CreateRepository",
                json!({"repositoryName": name}),
            )
            .await;
            let created_uri = string_at(
                &case,
                &created["repository"]["repositoryUri"],
                "repository.repositoryUri",
            );
            assert_eq!(
                created_uri, expected,
                "case {case:?}: expected CreateRepository to publish {expected:?}, got \
                 {created_uri:?}"
            );
            let (authority, path) = dialled(&case, &created_uri);
            assert_eq!(
                authority, endpoint.announced,
                "case {case:?}: expected the dialled authority to be the endpoint this \
                 emulator answers on, got {authority:?}"
            );
            assert_eq!(
                path,
                format!("{DEFAULT_REGISTRY_ALIAS}/{name}"),
                "case {case:?}: expected the alias-scoped repository path, got {path:?}"
            );

            // The ARN and the registry id are LABELS of the stored record, so the endpoint
            // the call arrived on does not touch them.
            let arn = string_at(
                &case,
                &created["repository"]["repositoryArn"],
                "repository.repositoryArn",
            );
            assert_eq!(
                arn,
                format!("{ARN_PREFIX}{name}"),
                "case {case:?}: expected the stored repository ARN, got {arn:?}"
            );
            assert_eq!(
                string_at(
                    &case,
                    &created["repository"]["registryId"],
                    "repository.registryId"
                ),
                REGISTRY_ID,
                "case {case:?}: expected the registry id of this account, got {created}"
            );

            // The named read, and the unfiltered listing a client discovers push targets
            // through, publish the same address.
            let named = call_at(
                &router,
                &endpoint,
                "DescribeRepositories",
                json!({"repositoryNames": [name]}),
            )
            .await;
            let named_uri = string_at(
                &case,
                &named["repositories"][0]["repositoryUri"],
                "repositories[0].repositoryUri",
            );
            assert_eq!(
                named_uri, expected,
                "case {case:?}: expected the named DescribeRepositories to publish \
                 {expected:?}, got {named_uri:?}"
            );
            let listed = call_at(&router, &endpoint, "DescribeRepositories", json!({})).await;
            let listed_uris: Vec<String> = listed["repositories"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: expected a page, got {listed}"))
                .iter()
                .map(|repository| {
                    string_at(&case, &repository["repositoryUri"], "repositoryUri")
                })
                .collect();
            assert!(
                listed_uris.contains(&expected),
                "case {case:?}: expected the listing to publish {expected:?}, got \
                 {listed_uris:?}"
            );
            for uri in &listed_uris {
                let (listed_authority, _) = dialled(&case, uri);
                assert_eq!(
                    listed_authority, endpoint.announced,
                    "case {case:?}: every discovered push target is on the endpoint this \
                     emulator answers on, got {uri:?}"
                );
            }

            // The echoed repository a delete reports carries the same address.
            let deleted = call_at(
                &router,
                &endpoint,
                "DeleteRepository",
                json!({"repositoryName": name}),
            )
            .await;
            let deleted_uri = string_at(
                &case,
                &deleted["repository"]["repositoryUri"],
                "repository.repositoryUri",
            );
            assert_eq!(
                deleted_uri, expected,
                "case {case:?}: expected DeleteRepository to echo {expected:?}, got \
                 {deleted_uri:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_published_uri_names_the_endpoint_the_read_was_answered_at() {
    // The stored repository is one record, and its ARN — a LABEL — follows from that record
    // alone. Its URI is an ADDRESS, so it follows the endpoint the reader reached this
    // emulator on: a client is never handed a push target it is not already talking to.
    let router = emulator();
    let endpoints = endpoints();
    let creating = &endpoints[0];
    let name = "published/per-reader";
    let created = call_at(
        &router,
        creating,
        "CreateRepository",
        json!({"repositoryName": name}),
    )
    .await;
    let created_at = created["repository"]["createdAt"].clone();
    assert_eq!(
        string_at(
            creating.case,
            &created["repository"]["repositoryUri"],
            "repository.repositoryUri"
        ),
        format!("{}/{DEFAULT_REGISTRY_ALIAS}/{name}", creating.announced),
        "case {:?}: expected the create to publish the address it was answered at, got \
         {created}",
        creating.case
    );

    for endpoint in &endpoints {
        let case = endpoint.case;
        let registry_uri = published_registry_uri(&router, endpoint).await;
        let listed = call_at(&router, endpoint, "DescribeRepositories", json!({})).await;
        let listed_uri = string_at(
            case,
            &listed["repositories"][0]["repositoryUri"],
            "repositories[0].repositoryUri",
        );
        assert_eq!(
            listed_uri,
            format!("{registry_uri}/{name}"),
            "case {case:?}: expected the read to publish the address it was answered at, \
             got {listed_uri:?}"
        );
        // A read publishes an address; it changes nothing a later caller sees, so the
        // record's own labels are the ones the create stored.
        let arn = string_at(
            case,
            &listed["repositories"][0]["repositoryArn"],
            "repositories[0].repositoryArn",
        );
        assert_eq!(
            arn,
            format!("{ARN_PREFIX}{name}"),
            "case {case:?}: expected the stored ARN to be unchanged by the read's endpoint, \
             got {arn:?}"
        );
        assert_eq!(
            listed["repositories"][0]["createdAt"], created_at,
            "case {case:?}: expected the stored createdAt to be unchanged by the read's \
             endpoint, got {listed}"
        );
    }
}

#[tokio::test]
async fn the_authority_a_published_uri_names_answers_the_registry_api() {
    // What the rule is FOR: a client takes the authority it was handed and dials it. Each
    // row dials the authority the URI published and asks this registry about the very
    // repository that URI names, so the address is proved to lead back to this emulator
    // rather than merely to carry the right labels.
    for endpoint in endpoints() {
        let router = emulator();
        let name = "followed/by-a-client";
        let case = endpoint.case;
        let created = call_at(
            &router,
            &endpoint,
            "CreateRepository",
            json!({"repositoryName": name}),
        )
        .await;
        let published = string_at(
            case,
            &created["repository"]["repositoryUri"],
            "repository.repositoryUri",
        );
        let (authority, _path) = dialled(case, &published);

        let answered = call_addressed(
            &router,
            case,
            Some(&authority),
            None,
            "DescribeRepositories",
            json!({"repositoryNames": [name]}),
        )
        .await;
        let reported = string_at(
            case,
            &answered["repositories"][0]["repositoryUri"],
            "repositories[0].repositoryUri",
        );
        assert_eq!(
            reported, published,
            "case {case:?}: expected the registry dialled at {authority:?} to publish the \
             repository {published:?} names, got {reported:?}"
        );
        assert_eq!(
            string_at(
                case,
                &answered["repositories"][0]["repositoryName"],
                "repositories[0].repositoryName"
            ),
            name,
            "case {case:?}: expected the repository the published URI names, got {answered}"
        );
    }
}
