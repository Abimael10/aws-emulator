// SPDX-License-Identifier: CC0-1.0

//! WHERE the address `GetRepositoryEndpoint` announces comes from, and what it resolves
//! back to.
//!
//! The RULE under test: a URL this service hands a caller to dial later is rooted on an
//! authority THIS responder answers on — the address the request reached it at, read from
//! wherever the request carries that address, and the base the emulator is served at when
//! the request carries none — while the resource's identity travels in the PATH beneath it
//! and is read off the STORED record, so the announced URL leads back to exactly the
//! repository it names. A URL on a host AWS publishes resolves away from this process, so
//! nothing a client then sends to it ever arrives.
//!
//! `announced_repository_endpoint.rs` pins the SHAPE of that URL and drives the package
//! planes at it. This suite pins the three things that shape is read FROM, each over a
//! table rather than one example:
//!
//! 1. the ADDRESS: every place on the wire a request carries the address it was sent to —
//!    the `Host` header, the authority of an absolute-form request target, and neither —
//!    including a client that resolves the host AWS publishes this endpoint at;
//! 2. the IDENTITY: the stored domain, its owning account and the repository's own name,
//!    whatever account the request names them under — and the refusal, rather than a URL,
//!    for a domain or a repository this service does not hold. Reading an endpoint is
//!    reporting only, so repeated reads announce the same URL and leave the repository as
//!    it was;
//! 3. what that identity RESOLVES: the announced path serves its own repository, and the
//!    same path with another identity — an unheld domain, an account that owns it not, an
//!    owner field that is no account id, an endpoint type the model does not declare — is
//!    not claimed at all.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

// The `EndpointType` catalogue comes from this service's Smithy model
// (`smithy_query(query="shape", shape="EndpointType")`), re-exported as the generated
// `*_VALUES` constant, so a model change moves this table rather than leaving a hand-copied
// list behind.
use service_com_amazonaws_codeartifact_codeartifactcontrolplaneservice::ops::types::ENDPOINT_TYPE_VALUES;

use crate::announced_repository_endpoint::{announced, dialable};
use crate::domain_repository_lifecycle::{create_repository, describe_repository};
use crate::harness::{
    auth_for, create_domain, domain_not_found, emulator_served_at, repository_not_found, send,
    Answer, ACCOUNT, REGION,
};
use crate::npm_registry_endpoint::{token_for, Raw};

/// The base a served emulator in these cases reports, with the loopback address and port a
/// data port is published at, and that base's own authority — the address a client on this
/// machine dials when it addressed nothing else.
const SERVED_BASE: &str = "https://127.0.0.1:9297";
const SERVED_AUTHORITY: &str = "127.0.0.1:9297";

/// A well-formed `AccountId` (`^[0-9]{12}$`) that owns no domain here: the account a
/// cross-account read names.
const ANOTHER_ACCOUNT: &str = "123456789012";

/// An `EndpointType` the model does not declare, asserted against its catalogue below so
/// the row cannot go stale if the model grows one.
const UNMODELLED_ENDPOINT_TYPE: &str = "ipv5";

/// The `GetRepositoryEndpoint` request target, with `domain-owner` sent only when named.
fn endpoint_query(
    domain: &str,
    repository: &str,
    format: &str,
    domain_owner: Option<&str>,
) -> String {
    let mut uri =
        format!("/v1/repository/endpoint?domain={domain}&repository={repository}&format={format}");
    if let Some(owner) = domain_owner {
        uri.push_str(&format!("&domain-owner={owner}"));
    }
    uri
}

/// Send `GetRepositoryEndpoint` as a CLIENT wrote it on the wire: `target` is the request
/// target verbatim — origin form, or the absolute form a relay addresses its next hop with
/// — and `host` is the `Host` header, sent only when the client set one.
///
/// Only the REQUEST is built here, because what these cases vary is the request line and
/// the `Host` header, which the harness's own `call` always fills with this service's
/// published host; the answer goes through the harness's one reduction.
async fn endpoint_answer(router: &MultiServiceRouter, target: &str, host: Option<&str>) -> Answer {
    let mut request = http::Request::builder()
        .method("GET")
        .uri(target)
        .header("authorization", auth_for(REGION))
        .header("content-type", "application/json");
    if let Some(host) = host {
        request = request.header("host", host);
    }
    send(
        router,
        request.body(bytes::Bytes::new()).expect("build request"),
    )
    .await
}

/// The announced endpoint of `repository`, read the way a client on this machine reads it.
async fn announced_endpoint(
    router: &MultiServiceRouter,
    domain: &str,
    repository: &str,
    format: &str,
) -> String {
    let answer = endpoint_answer(
        router,
        &endpoint_query(domain, repository, format, None),
        Some(SERVED_AUTHORITY),
    )
    .await;
    announced(&answer, &format!("{domain}/{repository}/{format}"))
}

/// The path of an announced endpoint, as the rule states it.
fn expected_path(domain: &str, repository: &str, format: &str, endpoint_type: &str) -> String {
    format!("/_aws/codeartifact/{REGION}/{endpoint_type}/{domain}-{ACCOUNT}/{format}/{repository}/")
}

/// A domain with one repository in it, and a token for that domain.
async fn given_repository(router: &MultiServiceRouter, domain: &str, repository: &str) -> String {
    assert_eq!(
        create_domain(router, domain, None).await.status,
        200,
        "CreateDomain {domain:?} must succeed"
    );
    assert_eq!(
        create_repository(router, domain, repository, None, &[])
            .await
            .status,
        200,
        "CreateRepository {repository:?} must succeed"
    );
    token_for(router, domain).await
}

/// Whether the front door answered that nothing claimed the request, which is what an
/// address no endpoint of this service serves gets.
fn unclaimed(answer: &Raw) -> bool {
    answer.json().get("__type").and_then(Value::as_str) == Some("UnknownService")
}

/// CASE 1: the announced authority is the address the request REACHED this emulator at,
/// read from wherever the request carries it — and never a host AWS publishes.
#[tokio::test]
async fn the_announced_authority_is_the_address_the_request_was_sent_to() {
    let router = emulator_served_at(SERVED_BASE);
    let domain = "addressed";
    let repository = "packages";
    given_repository(&router, domain, repository).await;

    let query = endpoint_query(domain, repository, "npm", None);
    // The host AWS itself publishes this endpoint at, which a client reaches this process on
    // only when its DNS or its proxy for that name points here.
    let published_by_aws = format!("{domain}-{ACCOUNT}.d.codeartifact.{REGION}.amazonaws.com");

    // (case, the request target as written, the `Host` header, the authority announced)
    let rows: Vec<(&str, String, Option<&str>, &str)> = vec![
        (
            "an origin-form target and the address the client dialled",
            query.clone(),
            Some("127.0.0.1:44035"),
            "127.0.0.1:44035",
        ),
        (
            "an origin-form target and a loopback name with its port",
            query.clone(),
            Some("localhost:44036"),
            "localhost:44036",
        ),
        (
            "an origin-form target and an IPv6 literal",
            query.clone(),
            Some("[::1]:44037"),
            "[::1]:44037",
        ),
        (
            "an absolute-form target and no Host header, as HTTP/2 carries the authority",
            format!("http://127.0.0.1:44038{query}"),
            None,
            "127.0.0.1:44038",
        ),
        (
            "an absolute-form target a relay addressed, carrying the client's own Host",
            format!("http://127.0.0.1:44039{query}"),
            Some("127.0.0.1:44040"),
            "127.0.0.1:44040",
        ),
        (
            "a request that names no address at all, answered the base this emulator is \
             served at",
            query.clone(),
            None,
            SERVED_AUTHORITY,
        ),
        (
            "a client that resolved the host AWS publishes this endpoint at",
            query.clone(),
            Some(published_by_aws.as_str()),
            SERVED_AUTHORITY,
        ),
    ];

    for (case, target, host, expected_authority) in rows {
        let answer = endpoint_answer(&router, &target, host).await;
        let url = announced(&answer, case);
        let (authority, path) = dialable(&url);
        assert_eq!(
            authority, expected_authority,
            "case {case:?}: expected the authority {expected_authority:?}, got {url:?}"
        );
        assert_eq!(
            path,
            expected_path(domain, repository, "npm", "ipv4"),
            "case {case:?}: expected the documented endpoint path, got {url:?}"
        );
        // And no part of the URL names a zone AWS publishes its own endpoints in
        // (`service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES`), which is
        // where a URL this process does not answer on would send the client.
        for zone in service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES {
            assert!(
                !url.contains(zone),
                "case {case:?}: the announced URL names the AWS endpoint zone {zone:?}, \
                 which this emulator does not serve: {url:?}"
            );
        }
    }
}

/// CASE 2: the identity beneath that authority is the STORED repository's — its domain, the
/// account that owns the domain and its own name — and a domain or a repository this
/// service does not hold is answered its refusal rather than a URL. Reading an endpoint
/// reports; it changes nothing a later caller sees.
#[tokio::test]
async fn the_announced_identity_is_the_stored_repositorys_and_a_miss_is_refused() {
    let router = emulator_served_at(SERVED_BASE);
    let domain = "stored-identity";
    let repository = "Releases.one";
    given_repository(&router, domain, repository).await;

    let expected = format!(
        "{SERVED_BASE}{}",
        expected_path(domain, repository, "npm", "ipv4")
    );
    // Which account the request names the domain UNDER selects the record to read; the
    // identity announced is the one that record carries, so both readings announce one URL.
    for (case, owner) in [
        ("no domainOwner at all", None),
        ("the account that owns the domain", Some(ACCOUNT)),
    ] {
        let answer = endpoint_answer(
            &router,
            &endpoint_query(domain, repository, "npm", owner),
            Some(SERVED_AUTHORITY),
        )
        .await;
        let url = announced(&answer, case);
        assert_eq!(
            url, expected,
            "case {case:?}: expected {expected:?}, got {url:?}"
        );
    }

    // (case, the request target, the sentence the refusal states)
    let refusals: Vec<(&str, String, String)> = vec![
        (
            "the domain read under an account that does not own it",
            endpoint_query(domain, repository, "npm", Some(ANOTHER_ACCOUNT)),
            domain_not_found(domain, ANOTHER_ACCOUNT),
        ),
        (
            "a domain this service does not hold",
            endpoint_query("absent-domain", repository, "npm", None),
            domain_not_found("absent-domain", ACCOUNT),
        ),
        (
            "a repository the domain does not hold",
            endpoint_query(domain, "absent-repository", "npm", None),
            repository_not_found(domain, ACCOUNT, "absent-repository"),
        ),
    ];

    for (case, target, sentence) in refusals {
        let answer = endpoint_answer(&router, &target, Some(SERVED_AUTHORITY)).await;
        assert_eq!(
            answer.status, 404,
            "case {case:?}: expected HTTP 404, got {answer:?}"
        );
        assert!(
            answer.code().contains("ResourceNotFoundException"),
            "case {case:?}: expected ResourceNotFoundException, got {answer:?}"
        );
        assert_eq!(
            answer.message(),
            sentence,
            "case {case:?}: expected {sentence:?}, got {answer:?}"
        );
        assert!(
            answer.body.get("repositoryEndpoint").is_none(),
            "case {case:?}: a refusal announces no endpoint, got {answer:?}"
        );
    }

    // Reporting only: the repository is exactly as it was, however often its endpoint is
    // read, and every read announces the same URL.
    let before = describe_repository(&router, domain, repository).await;
    for read in 1..=3 {
        let case = format!("read {read} of the same endpoint");
        let answer = endpoint_answer(
            &router,
            &endpoint_query(domain, repository, "npm", None),
            Some(SERVED_AUTHORITY),
        )
        .await;
        let url = announced(&answer, &case);
        assert_eq!(
            url, expected,
            "case {case:?}: expected {expected:?}, got {url:?}"
        );
    }
    let after = describe_repository(&router, domain, repository).await;
    assert_eq!(
        after.body, before.body,
        "case \"the repository after its endpoint was read\": expected {:?}, got {:?}",
        before.body, after.body
    );
}

/// CASE 3: that identity is what RESOLVES the repository on the way back: the announced
/// path serves its own repository, and the same path carrying another identity — or an
/// endpoint type the model does not declare — is not claimed at all.
#[tokio::test]
async fn the_identity_in_the_announced_path_is_what_resolves_the_repository() {
    let router = emulator_served_at(SERVED_BASE);
    let domain = "path-identity";
    let repository = "releases";
    let token = given_repository(&router, domain, repository).await;

    let url = announced_endpoint(&router, domain, repository, "npm").await;
    let (host, path) = dialable(&url);
    assert_eq!(
        host, SERVED_AUTHORITY,
        "expected the address this client reached, got {url:?}"
    );

    let announced_label = format!("{domain}-{ACCOUNT}");
    // (case, the identity segment the client dials, whether this registry serves it)
    let rows: Vec<(&str, String, bool)> = vec![
        ("the announced identity", announced_label.clone(), true),
        (
            "a domain this service does not hold",
            format!("absent-domain-{ACCOUNT}"),
            false,
        ),
        (
            "the domain under an account that does not own it",
            format!("{domain}-{ANOTHER_ACCOUNT}"),
            false,
        ),
        (
            "an owner field that is no account id",
            format!("{domain}-owner"),
            false,
        ),
    ];

    for (case, label, serves) in rows {
        let dialled = path.replace(&announced_label, &label);
        let ping = registry_ping(&router, &host, &dialled, &token).await;
        if serves {
            assert_eq!(
                ping.status, 200,
                "case {case:?}: expected the announced endpoint to serve 200, got {ping:?}"
            );
            assert!(
                !unclaimed(&ping),
                "case {case:?}: expected this registry to answer, got {ping:?}"
            );
        } else {
            assert!(
                unclaimed(&ping),
                "case {case:?}: expected {dialled:?} to be claimed by nothing, got {ping:?}"
            );
        }
    }

    // The endpoint-type segment is read against the model's own catalogue, so a value
    // outside it addresses no endpoint of this service either.
    assert!(
        !ENDPOINT_TYPE_VALUES.contains(&UNMODELLED_ENDPOINT_TYPE),
        "the model declares {UNMODELLED_ENDPOINT_TYPE:?} after all: {ENDPOINT_TYPE_VALUES:?}"
    );
    let unmodelled = path.replace("/ipv4/", &format!("/{UNMODELLED_ENDPOINT_TYPE}/"));
    let ping = registry_ping(&router, &host, &unmodelled, &token).await;
    assert!(
        unclaimed(&ping),
        "case \"an endpoint type the model does not declare\": expected {unmodelled:?} to be \
         claimed by nothing, got {ping:?}"
    );
}

/// The write-authorization route an npm client checks at `endpoint`, which is the cheapest
/// request that proves the address is served by this repository's registry.
async fn registry_ping(
    router: &MultiServiceRouter,
    host: &str,
    endpoint: &str,
    token: &str,
) -> Raw {
    crate::npm_registry_endpoint::registry(
        router,
        "GET",
        host,
        &format!("{endpoint}-/ping"),
        Some(token),
        None,
    )
    .await
}
