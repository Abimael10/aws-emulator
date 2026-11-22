// SPDX-License-Identifier: CC0-1.0

//! The gallery ADDRESS this service publishes — for the registry and for every repository
//! in it — driven end to end over the wire.
//!
//! ONE rule is under test, because the model states one kind of value twice:
//! `Registry$registryUri` ("The URI of a public registry. The URI contains a universal
//! prefix and the registry alias") and `Repository$repositoryUri` ("The URI for the
//! repository. You can use this URI for container image `push` and `pull` operations") are
//! each DIALLED by a container client and PARSED by a client that has to tell which registry
//! holds the repository, so each carries the address this emulator answers that caller at,
//! then the account's primary registry alias (`RegistryAlias$primaryRegistryAlias`,
//! "displayed in both the repository URL and the image URI used in the docker pull commands")
//! — the opaque eight-character identifier the registry was assigned — and, for a repository,
//! its own name. Nothing else belongs in either: the owning account is reported in
//! `registryId`, and a public registry is global, so no Region appears either.
//!
//! The address published is never a host AWS serves — the gallery's own name, or any host AWS
//! publishes its endpoints at — since a name under either resolves to AWS rather than to this
//! process and would send a push or a pull out of the emulated environment, and never a host
//! the front door resolves to some service, which no registry client can use. It is the
//! endpoint the CALLER reached, whenever this gallery answers requests there, and otherwise
//! the address this emulator is served at.
//!
//! The class the rule generalises over is the endpoint a call arrived on crossed with the
//! repository name and the operation reporting the URI, so the rows span the endpoint forms
//! a caller reaches this emulator by — the emulator's own socket, a loopback name with
//! another port, a bracketed IPv6 literal, an endpoint carried only on the request URI, this
//! service's own endpoint host (which AWS publishes, so no registry client can use it), the
//! gallery AWS serves, a `Host` that is no endpoint at all, a routable address a wildcard
//! bind was reached by, a host name an operator points a client at, and no `Host` at all —
//! crossed
//! with the repository-name forms the modelled `RepositoryName` pattern admits. No row is the
//! recorded request.
//!
//! The composition AWS documents is preserved throughout: a repository's URI is the
//! registry's URI followed by the repository's name, and the four operations that publish
//! one (`CreateRepository`, `DescribeRepositories` filtered and unfiltered,
//! `DeleteRepository`, `DescribeRegistries`) agree for the same repository. The last two
//! tests are what the rule is FOR: the authority the published URI names is dialled and this
//! emulator answers the registry API there, and an image pushed to a repository is pulled
//! back through a container reference composed on the `repositoryUri` the API handed out.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_ecrpublic_spencerfrontendservice::state::registry_address;
use service_com_amazonaws_ecrpublic_spencerfrontendservice::state::{gallery_pull, GALLERY_HOST};
use service_com_amazonaws_ecrpublic_spencerfrontendservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::repository_image_lifecycle::{
    call_addressed, registry_alias, ARN_PREFIX, PUBLIC_REGISTRY_LABELS, REGISTRY_ID,
};

/// The address this emulator is SERVED at in this suite: the base a call that reached it on
/// no usable endpoint of its own is told to come back to.
const SERVED_URL_BASE: &str = "https://127.0.0.1:9297";

/// The `host[:port]` half of [`SERVED_URL_BASE`], which is the authority published to such a
/// call.
const SERVED_ENDPOINT: &str = "127.0.0.1:9297";

/// The Region every call in this suite is signed for, and the one Region string a published
/// URI is checked against: a public registry is global, so no Region may appear in one.
const SIGNED_REGION: &str = "us-east-1";

/// One endpoint a caller reaches this emulator on: where the endpoint travels (the `Host`
/// header, the request URI's authority, or neither) and the authority every URI published to
/// that call is built on.
struct Endpoint {
    case: &'static str,
    host: Option<&'static str>,
    uri_authority: Option<&'static str>,
    /// The authority this registry publishes to such a call. Spelled out per row rather than
    /// derived, so what is asserted is the address itself and not a recomputation of the
    /// implementation's own reading.
    published: &'static str,
}

/// The endpoints the table drives, spanning the forms a caller reaches this emulator by.
///
/// The endpoint of a call a registry client can dial is the one published; every other form
/// falls back to the address this emulator is served at.
fn endpoints() -> Vec<Endpoint> {
    vec![
        Endpoint {
            case: "the emulator's own socket",
            host: Some(SERVED_ENDPOINT),
            uri_authority: None,
            published: SERVED_ENDPOINT,
        },
        Endpoint {
            case: "a loopback name with another port",
            host: Some("localhost:39355"),
            uri_authority: None,
            published: "localhost:39355",
        },
        Endpoint {
            case: "a bracketed IPv6 loopback literal",
            host: Some("[::1]:38001"),
            uri_authority: None,
            published: "[::1]:38001",
        },
        Endpoint {
            case: "an endpoint carried only on the request URI",
            host: None,
            uri_authority: Some("127.0.0.1:8443"),
            published: "127.0.0.1:8443",
        },
        Endpoint {
            // A host AWS publishes its own endpoints at routes to a service rather than to a
            // registry, so the served address is published instead.
            case: "this service's endpoint host, which AWS publishes",
            host: Some("api.ecr-public.us-east-1.amazonaws.com"),
            uri_authority: None,
            published: SERVED_ENDPOINT,
        },
        Endpoint {
            // AWS's own gallery resolves to the real public registry, so it is not an address
            // this emulator may publish: the served address is published instead.
            case: "the gallery AWS serves",
            host: Some(GALLERY_HOST),
            uri_authority: None,
            published: SERVED_ENDPOINT,
        },
        Endpoint {
            case: "a Host that is no endpoint at all",
            host: Some("elsewhere.example.com/gallery"),
            uri_authority: None,
            published: SERVED_ENDPOINT,
        },
        Endpoint {
            // A routable literal a caller reached a wildcard bind by is not an address the
            // reading of a container reference recognises, so no URI is published on it: the
            // served address is published instead, and publishing and reading agree.
            case: "a routable address a wildcard bind was reached by",
            host: Some("10.1.2.3:5000"),
            uri_authority: None,
            published: SERVED_ENDPOINT,
        },
        Endpoint {
            // A name an operator points a client at carries a first label the front door
            // reads as a service, so it is of no use to a registry client either.
            case: "a host name an operator points a client at",
            host: Some("emulator.internal:8080"),
            uri_authority: None,
            published: SERVED_ENDPOINT,
        },
        Endpoint {
            case: "no endpoint on the request at all",
            host: None,
            uri_authority: None,
            published: SERVED_ENDPOINT,
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
/// [`SERVED_URL_BASE`], with the engine it keeps its registry in handed back — so a pull
/// decided in process reads the very registry the wire calls wrote.
fn emulator_with_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature itself is not what these cases are about; the credential scope below
    // still routes the request to `ecr-public`.
    router.set_request_authentication(false);
    router.set_url_base(
        ::smithy_server_runtime::EndpointUrl::parse(SERVED_URL_BASE)
            .expect("the base URL must parse"),
    );
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// One emulator, for a case that drives it over the wire alone.
fn emulator() -> MultiServiceRouter {
    emulator_with_engine().0
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
/// asserted to be the published endpoint scoped by the registry's PRIMARY alias — the one
/// flagged `RegistryAlias$primaryRegistryAlias` on that very answer, which is the alias a
/// pull URL shows — and to carry neither the owning account nor a Region.
async fn published_registry_uri(router: &MultiServiceRouter, endpoint: &Endpoint) -> String {
    let described = call_at(router, endpoint, "DescribeRegistries", json!({})).await;
    let registry = &described["registries"][0];
    let uri = string_at(endpoint.case, &registry["registryUri"], "registryUri");
    let primary = registry["aliases"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the registry's alias records, got {described}",
                endpoint.case
            )
        })
        .iter()
        .find(|alias| alias["primaryRegistryAlias"] == json!(true))
        .map(|alias| string_at(endpoint.case, &alias["name"], "aliases[].name"))
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: expected one alias flagged primaryRegistryAlias, got {described}",
                endpoint.case
            )
        });
    assert_eq!(
        uri,
        format!("{}/{primary}", endpoint.published),
        "case {:?}: expected the registry URI on the endpoint this emulator answers at, \
         scoped by the primary alias, got {uri:?}",
        endpoint.case
    );
    assert!(
        !uri.contains(REGISTRY_ID),
        "case {:?}: the owning account is reported in registryId, never in {uri:?}",
        endpoint.case
    );
    assert!(
        !uri.contains(SIGNED_REGION),
        "case {:?}: a public registry is global, so no Region belongs in {uri:?}",
        endpoint.case
    );
    uri
}

/// The authority a published URI names, and the path a client extends on it.
fn dialled(case: &str, uri: &str) -> (String, String) {
    let (authority, path) = uri
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: {uri:?} names no path"));
    // What makes the authority publishable at all: this emulator answers on it, by the one
    // reading of that question this service states.
    assert!(
        registry_address::names_this_emulator(authority),
        "case {case:?}: expected {uri:?} on an address this emulator answers on"
    );
    // The addresses a published URI may never name: a host AWS serves and this emulator does
    // not — the gallery's own name, and any zone AWS publishes its endpoints in.
    let [public, ecr] = PUBLIC_REGISTRY_LABELS;
    assert_ne!(
        authority, GALLERY_HOST,
        "case {case:?}: {uri:?} names the real gallery host ({public}.{ecr}.…), which this \
         emulator does not answer on"
    );
    for suffix in AWS_ENDPOINT_DNS_SUFFIXES {
        // The authority may carry a `:port`, and it is the HOST that says whose endpoint
        // this is, so the port goes first — by the tree's one reading of that.
        let host = service_common::data_plane_addressing::host_only(authority);
        assert!(
            !host.ends_with(suffix),
            "case {case:?}: {uri:?} is served in a zone AWS publishes its own endpoints in, \
             which this emulator does not answer in"
        );
    }
    (authority.to_string(), path.to_string())
}

/// The DNS suffixes AWS publishes its own endpoints under, spelled out here rather than
/// imported so that a published address slipping into one of them fails this suite.
///
/// Source: `service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`, the tree's one
/// list of them, which this service's reading of the addresses it may publish is written on.
const AWS_ENDPOINT_DNS_SUFFIXES: [&str; 4] =
    [".amazonaws.com", ".amazonaws.com.cn", ".api.aws", ".on.aws"];

/// The form of the alias AWS assigns an account's public registry, asserted on the value
/// reported — spelled out here rather than taken from the crate so a reworded alias fails:
/// an OPAQUE identifier of eight characters alternating a lowercase letter and a digit, well
/// inside the modelled `RegistryAliasName` (`^[a-z][a-z0-9]+(?:[._-][a-z0-9]+)*$`, length
/// 2..50 — source: `smithy_query(query="shape", shape="RegistryAliasName")`).
fn assert_alias_form(case: &str, alias: &str) {
    let characters: Vec<char> = alias.chars().collect();
    assert_eq!(
        characters.len(),
        8,
        "case {case:?}: expected an eight-character registry alias, got {alias:?}"
    );
    for (position, character) in characters.iter().enumerate() {
        let (wanted, holds) = if position % 2 == 0 {
            ("a lowercase letter", character.is_ascii_lowercase())
        } else {
            ("a digit", character.is_ascii_digit())
        };
        assert!(
            holds,
            "case {case:?}: expected {wanted} at position {position} of {alias:?}, got \
             {character:?}"
        );
    }
}

#[tokio::test]
async fn the_registry_alias_every_published_uri_carries_is_the_identifier_aws_assigns() {
    // RULE's other half: the alias scoping every published URI is the opaque identifier the
    // registry was assigned — reported on `Registry$aliases` as both the primary and the
    // default alias — and the same one in every operation that publishes a URI.
    let router = emulator();
    let endpoint = &endpoints()[0];
    let described = call_at(&router, endpoint, "DescribeRegistries", json!({})).await;
    let registry = &described["registries"][0];
    let reported = string_at(
        endpoint.case,
        &registry["aliases"][0]["name"],
        "aliases[0].name",
    );
    assert_alias_form("the alias the registry reports", &reported);
    assert_eq!(
        reported,
        registry_alias(),
        "expected the alias this account's registry is assigned, got {reported:?}"
    );
    for flag in ["primaryRegistryAlias", "defaultRegistryAlias"] {
        assert_eq!(
            registry["aliases"][0][flag],
            json!(true),
            "expected the assigned alias to be flagged {flag}, got {described}"
        );
    }

    // Every operation that publishes a URI scopes it by that very alias, for each repository
    // name form the modelled pattern admits.
    for name in REPOSITORY_NAMES {
        let case = format!("repository {name}");
        let created = call_at(
            &router,
            endpoint,
            "CreateRepository",
            json!({"repositoryName": name}),
        )
        .await;
        let listed = call_at(&router, endpoint, "DescribeRepositories", json!({})).await;
        let deleted = call_at(
            &router,
            endpoint,
            "DeleteRepository",
            json!({"repositoryName": name}),
        )
        .await;
        let published = [
            ("CreateRepository", created["repository"].clone()),
            (
                "DescribeRepositories",
                listed["repositories"]
                    .as_array()
                    .and_then(|page| {
                        page.iter()
                            .find(|repository| repository["repositoryName"] == json!(name))
                            .cloned()
                    })
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected it listed, got {listed}")
                    }),
            ),
            ("DeleteRepository", deleted["repository"].clone()),
        ];
        for (operation, repository) in published {
            let uri = string_at(&case, &repository["repositoryUri"], "repositoryUri");
            let (_authority, path) = dialled(&case, &uri);
            let alias = path
                .split_once('/')
                .map(|(alias, _repository)| alias.to_string())
                .unwrap_or_else(|| panic!("case {case:?}: {uri:?} names no repository"));
            assert_alias_form(&format!("{operation} on {case}"), &alias);
            assert_eq!(
                alias, reported,
                "case {case:?}: expected {operation} to scope {uri:?} by the alias the \
                 registry reports, got {alias:?}"
            );
        }
    }
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
                authority, endpoint.published,
                "case {case:?}: expected the dialled authority to be the endpoint this \
                 emulator answers at, got {authority:?}"
            );
            assert_eq!(
                path,
                format!("{}/{name}", registry_alias()),
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
                    listed_authority, endpoint.published,
                    "case {case:?}: every discovered push target is on the endpoint this \
                     emulator answers at, got {uri:?}"
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
        format!("{}/{}/{name}", creating.published, registry_alias()),
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

#[tokio::test]
async fn a_reference_composed_on_a_published_uri_pulls_the_image_pushed_to_it() {
    // The other half of what the rule is FOR: a client pushes to the URI the API handed it,
    // and a container runtime pulls the reference it composed on that URI — so the address
    // this registry publishes and the address it answers for are the same one. The rows span
    // the reference forms a runtime writes (a tag, the implicit tag, and a digest) over a
    // namespaced repository name, and the reference a container definition spells by hand on
    // the gallery AWS serves, which names this registry by its alias.
    let (router, engine) = emulator_with_engine();
    let endpoints = endpoints();
    let endpoint = &endpoints[0];
    let case = endpoint.case;
    let name = "team/app";
    let created = call_at(
        &router,
        endpoint,
        "CreateRepository",
        json!({"repositoryName": name}),
    )
    .await;
    let published = string_at(
        case,
        &created["repository"]["repositoryUri"],
        "repository.repositoryUri",
    );
    let pushed = call_at(
        &router,
        endpoint,
        "PutImage",
        json!({
            "repositoryName": name,
            "imageManifest": json!({"schemaVersion": 2, "layers": []}).to_string(),
            "imageTag": "latest",
        }),
    )
    .await;
    let digest = string_at(
        case,
        &pushed["image"]["imageId"]["imageDigest"],
        "image.imageId.imageDigest",
    );

    for reference in [
        format!("{published}:latest"),
        published.clone(),
        format!("{published}@{digest}"),
        format!("{GALLERY_HOST}/{}/{name}:latest", registry_alias()),
    ] {
        let parsed = gallery_pull::parse_gallery_reference(&reference).unwrap_or_else(|| {
            panic!("case {reference:?}: expected a reference into this gallery")
        });
        assert_eq!(
            parsed.registry_alias,
            registry_alias(),
            "case {reference:?}: expected the alias the URI was published with, got {:?}",
            parsed.registry_alias
        );
        assert_eq!(
            parsed.repository_name, name,
            "case {reference:?}: expected the repository the URI names, got {:?}",
            parsed.repository_name
        );
        assert!(
            gallery_pull::serves_registry_alias(&*engine, &parsed.registry_alias),
            "case {reference:?}: expected this registry to serve the alias it published"
        );
        let served = gallery_pull::serve_pull(&*engine, &parsed);
        assert_eq!(
            served,
            Ok(Some(digest.clone())),
            "case {reference:?}: expected the pushed manifest digest, got {served:?}"
        );
    }

    // A reference on another registry's authority is not this gallery's to answer, however
    // the one this registry published is spelled.
    for foreign in [
        format!("quay.io/{}/{name}:latest", registry_alias()),
        format!("{REGISTRY_ID}.dkr.ecr.{SIGNED_REGION}.amazonaws.com/{name}:latest"),
    ] {
        assert!(
            gallery_pull::parse_gallery_reference(&foreign).is_none(),
            "case {foreign:?}: expected no reference into this gallery"
        );
    }
}
