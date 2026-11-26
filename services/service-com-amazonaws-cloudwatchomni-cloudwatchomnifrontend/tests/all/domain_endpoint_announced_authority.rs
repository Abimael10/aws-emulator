// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the AUTHORITY the two endpoint members of a CloudWatchOmni domain are
//! announced on — `Domain.domainEndpointUrl` ("The HTTPS endpoint URL for accessing the
//! domain", required) and every `Domain.customEndpointUrls` entry ("Additional endpoint URLs
//! derived from the domain name") (`smithy_query(query="shape",shape="Domain")`). The model
//! fixes no hostname, so what is required of these members is that a caller can OPEN them.
//!
//! The RULE this suite asserts: a URL an operation hands a caller to fetch later is rooted on
//! an authority THIS emulator answers on — the `host[:port]` the request arrived at, byte for
//! byte — and on nothing else: not a host in a zone AWS publishes its own endpoints in, and
//! not a name of this emulator's own invention beneath the address it was reached at, neither
//! of which resolves to this process. WHICH resource the URL addresses is carried in the path
//! this emulator serves that resource under, so the announced address and the served address
//! are the same address.
//!
//! The rows span what the rule generalises over: the address shapes a client reaches this
//! emulator at (an IPv4 literal with a port, the loopback name on another port, a bracketed
//! IPv6 literal, an address with no port, a second address of the interface), the form the
//! request carried its host in (a `Host` header, an absolute-form target, neither), a caller
//! that addressed one of AWS's own hosts, and all three operations that report a domain. Every
//! assertion names its case and the actual value.

use crate::domain_endpoint_addressing::{signed_parts, Omni, MODELLED_TARGET, REGION};

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::types::Domain;

/// The DNS suffixes AWS publishes its own endpoints under, which no address this emulator
/// announces may name: a client that resolves one leaves the emulated environment altogether.
/// The workspace's one statement of that question.
const AWS_ENDPOINT_SUFFIXES: [&str; 4] =
    ::service_common::data_plane_addressing::AWS_ENDPOINT_DNS_SUFFIXES;

/// The `host[:port]` and the path `url` carries, as a client dialling it reads them — taken
/// off the URL as written rather than through a parser that normalises the default port away,
/// because the published value is exactly what a client dials.
fn authority_and_path(case: &str, url: &str) -> (String, String) {
    let parsed = ::url::Url::parse(url)
        .unwrap_or_else(|error| panic!("case {case:?}: {url:?} is not a URL: {error}"));
    assert_eq!(
        parsed.scheme(),
        "https",
        "case {case:?}: both members are documented HTTPS endpoints, got {url:?}"
    );
    let rest = url
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("case {case:?}: {url:?} carries no https authority"));
    let (authority, path) = match rest.find('/') {
        Some(end) => (&rest[..end], &rest[end..]),
        None => (rest, "/"),
    };
    (authority.to_string(), path.to_string())
}

/// Assert the rule over one reported `domain`: both members are rooted on `reached` — the
/// address the request was answered at — and each names its own identity in its path.
fn assert_rooted_on_the_reached_address(case: &str, domain: &Domain, reached: &str) {
    let name = domain
        .name
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: the reported domain carries no name"));
    let custom = domain.custom_endpoint_urls.clone().unwrap_or_default();
    assert_eq!(
        custom.len(),
        1,
        "case {case:?}: a domain carrying a name publishes exactly one custom endpoint, got \
         {custom:?}"
    );
    for (member, url, expected_path) in [
        (
            "domainEndpointUrl",
            domain.domain_endpoint_url.clone(),
            omni::state::domain_endpoint_path(&domain.domain_id),
        ),
        (
            "customEndpointUrls[0]",
            custom[0].clone(),
            omni::state::domain_name_endpoint_path(&name),
        ),
    ] {
        let (authority, path) = authority_and_path(case, &url);
        assert_eq!(
            authority, reached,
            "case {case:?}: {member} must be dialled at the address this emulator was reached \
             at ({reached:?}), got {authority:?} in {url:?}"
        );
        assert_eq!(
            path, expected_path,
            "case {case:?}: {member} must address the path this emulator serves the domain \
             under ({expected_path:?}), got {path:?} in {url:?}"
        );
        for suffix in AWS_ENDPOINT_SUFFIXES {
            assert!(
                !authority.contains(suffix),
                "case {case:?}: {member} must name no host AWS publishes its own endpoints at, \
                 got {url:?}"
            );
        }
        // Nothing of the resource may reach the authority: a label in front of the address
        // this emulator was reached at is a name nothing here answers for.
        for identity in [domain.domain_id.as_str(), name.as_str()] {
            assert!(
                !authority.contains(identity),
                "case {case:?}: {member}'s authority must name no resource identity \
                 ({identity:?}), got {url:?}"
            );
        }
    }
}

/// RULE: the create, the read and the patch all announce both endpoint members on the address
/// the request they answer arrived at, whatever shape that address has and whichever way the
/// request carried it.
#[tokio::test]
async fn every_announced_endpoint_is_rooted_on_the_address_the_request_arrived_at() {
    let unserved = smithy_server_runtime::UNSERVED_URL_BASE
        .split_once("://")
        .map_or(smithy_server_runtime::UNSERVED_URL_BASE, |(_, rest)| rest)
        .to_string();
    // Rows pair the request's host (as a header, in an absolute-form target, or absent) with
    // the address the answer must therefore be rooted on.
    let rows: Vec<(&str, Option<&str>, &str, String)> = vec![
        (
            "an IPv4 literal with a port",
            Some("127.0.0.1:9297"),
            MODELLED_TARGET,
            "127.0.0.1:9297".to_string(),
        ),
        (
            "the loopback name on another port",
            Some("localhost:19297"),
            MODELLED_TARGET,
            "localhost:19297".to_string(),
        ),
        (
            "a bracketed IPv6 literal",
            Some("[::1]:443"),
            MODELLED_TARGET,
            "[::1]:443".to_string(),
        ),
        (
            "an address with no port",
            Some("127.0.0.1"),
            MODELLED_TARGET,
            "127.0.0.1".to_string(),
        ),
        (
            "another loopback address of the interface",
            Some("127.0.0.2:19297"),
            MODELLED_TARGET,
            "127.0.0.2:19297".to_string(),
        ),
        (
            "an absolute-form target and no header",
            None,
            "https://192.0.2.10:8443/service/CloudWatchOmniFrontend/operation/Unused",
            "192.0.2.10:8443".to_string(),
        ),
        (
            "a caller that addressed one of AWS's own hosts",
            Some("cloudwatch-omni.us-west-2.amazonaws.com"),
            MODELLED_TARGET,
            unserved.clone(),
        ),
        ("no host at all", None, MODELLED_TARGET, unserved.clone()),
    ];

    for (case, host, uri, reached) in rows {
        // One emulator per row: an account holds at most one domain, so each row creates its
        // own.
        let omni = Omni::new();
        let parts = signed_parts(host, uri);
        let created = omni.create_domain(&parts, "omni-reached-address").await;
        assert_rooted_on_the_reached_address(&format!("{case} (CreateDomain)"), &created, &reached);

        let fetched = omni.get_domain(&parts, &created.domain_id).await;
        assert_rooted_on_the_reached_address(&format!("{case} (GetDomain)"), &fetched, &reached);

        let renamed = omni
            .rename_domain(&parts, &created.domain_id, "omni-reached-renamed")
            .await;
        assert_rooted_on_the_reached_address(&format!("{case} (UpdateDomain)"), &renamed, &reached);
    }
}

/// RULE: the announced address is the SERVED address — a fetch of the URL exactly as the
/// create handed it out reaches the stored domain, for each member and on whichever address
/// the client dials this emulator at.
#[tokio::test]
async fn a_fetch_of_the_announced_url_as_handed_out_reaches_the_stored_domain() {
    // Rows differ along the address the creating request arrived at, so the URL that is
    // fetched is rooted on a different authority in each one.
    for (case, host) in [
        ("an IPv4 literal with a port", "127.0.0.1:9297"),
        ("the loopback name on another port", "localhost:19297"),
        ("an address with no port", "127.0.0.1"),
    ] {
        let omni = Omni::new();
        let parts = signed_parts(Some(host), MODELLED_TARGET);
        let created = omni.create_domain(&parts, "omni-fetch-announced").await;
        let custom = created
            .custom_endpoint_urls
            .clone()
            .unwrap_or_default()
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the create reports a custom endpoint"));
        for (member, url) in [
            ("domainEndpointUrl", created.domain_endpoint_url.clone()),
            ("customEndpointUrls[0]", custom),
        ] {
            let (status, media_type, body) = omni.fetch("GET", &url).await;
            assert_eq!(
                status, 200,
                "case {case:?}: fetching {member} at {url:?} answered {status}: {body:?}"
            );
            assert!(
                media_type.starts_with("text/html"),
                "case {case:?}: expected a web page at {member}, got content-type {media_type:?}"
            );
            for stated in [created.domain_id.as_str(), REGION] {
                assert!(
                    body.contains(stated),
                    "case {case:?}: expected the page at {member} to state {stated:?}, got \
                     {body:?}"
                );
            }
        }
    }
}
