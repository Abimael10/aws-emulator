// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for WHERE a CloudWatchOmni domain's identity label sits in the endpoint
//! URLs it publishes — `Domain.domainEndpointUrl` ("The HTTPS endpoint URL for accessing the
//! domain", required) and `Domain.customEndpointUrls` ("Additional endpoint URLs derived from
//! the domain name"), with `Domain.domainId` patterned `^d-[0-9a-z]{1,25}$` and `Domain.name`
//! patterned `^[a-z0-9]+(-[a-z0-9]+)*$` at 3-63 characters
//! (`smithy_query(query="shape",shape="Domain")`).
//!
//! The RULE this suite asserts: each of those members is an `https` URL whose AUTHORITY is
//! the address this emulator answered the request on — nothing added in front of it and no
//! name invented beneath it — and whose PATH names the resource it belongs to: the domain's
//! id for `domainEndpointUrl`, the domain's name for each `customEndpointUrls` entry. A
//! domain carrying a name publishes exactly one custom endpoint, and both members share that
//! one authority. It follows that a rename moves only the name-derived address: the
//! id-addressed `domainEndpointUrl`, `domainArn` and `createdAt` are unchanged, and the
//! renamed domain's custom endpoint carries the new name while its authority stays the one
//! the id endpoint uses.
//!
//! The rows of each table differ along what the rule generalises over: the address this
//! emulator was reached at (an IPv4 address with a port, a loopback name, a bracketed IPv6
//! literal, an address with no port, a DNS name, no host at all), the name shapes a domain
//! carries (the shortest the model admits, a multi-segment hyphenated name, one of the
//! longest it admits) and which of the two members is read. Every assertion names its case
//! and the actual value.

use crate::domain_endpoint_addressing::{signed_parts, Omni, MODELLED_TARGET, REGION};

use service_com_amazonaws_cloudwatchomni_cloudwatchomnifrontend as omni;

use omni::ops::types::Domain;

/// How this suite READS an announced endpoint URL — the same reading a client does: the
/// scheme, the authority it dials, and the path it addresses there.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AnnouncedEndpoint {
    scheme: String,
    authority: String,
    path: String,
}

/// `url` read as an announced endpoint, through the URL parser this crate already depends on.
fn announced_endpoint(case: &str, url: &str) -> AnnouncedEndpoint {
    let parsed = ::url::Url::parse(url)
        .unwrap_or_else(|error| panic!("case {case:?}: {url:?} is not a URL: {error}"));
    let host = parsed
        .host_str()
        .unwrap_or_else(|| panic!("case {case:?}: {url:?} names no host"))
        .to_string();
    let authority = match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    };
    AnnouncedEndpoint {
        scheme: parsed.scheme().to_string(),
        authority,
        path: parsed.path().to_string(),
    }
}

/// The single custom endpoint URL `domain` publishes: `customEndpointUrls` holds exactly one
/// entry for a domain that carries a name.
fn the_custom_endpoint(case: &str, domain: &Domain) -> String {
    let urls = domain.custom_endpoint_urls.clone().unwrap_or_default();
    assert_eq!(
        urls.len(),
        1,
        "case {case:?}: a domain carrying a name publishes exactly one custom endpoint, got \
         {urls:?}"
    );
    urls.into_iter().next().unwrap_or_default()
}

/// Assert the whole rule over one reported `domain`: the id names the path of one endpoint,
/// the name the path of the other, and both are dialled at one authority.
fn assert_identity_is_in_the_path(case: &str, domain: &Domain) {
    let id_endpoint = announced_endpoint(case, &domain.domain_endpoint_url);
    let name = domain
        .name
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: the reported domain carries no name"));
    let custom = the_custom_endpoint(case, domain);
    let name_endpoint = announced_endpoint(case, &custom);

    assert_eq!(
        id_endpoint.path,
        omni::state::domain_endpoint_path(&domain.domain_id),
        "case {case:?}: the domain endpoint's path must name the domain id {:?}, got {:?}",
        domain.domain_id,
        domain.domain_endpoint_url
    );
    assert_eq!(
        name_endpoint.path,
        omni::state::domain_name_endpoint_path(&name),
        "case {case:?}: the custom endpoint's path must name the domain name {name:?}, got \
         {custom:?}"
    );
    assert_eq!(
        name_endpoint.authority, id_endpoint.authority,
        "case {case:?}: both endpoints must be dialled at one authority, got {:?} and {:?}",
        id_endpoint.authority, name_endpoint.authority
    );
    for (member, endpoint, url) in [
        ("domainEndpointUrl", &id_endpoint, &domain.domain_endpoint_url),
        ("customEndpointUrls[0]", &name_endpoint, &custom),
    ] {
        assert_eq!(
            endpoint.scheme, "https",
            "case {case:?}: {member} must be an https URL, got {url:?}"
        );
        for identity in [domain.domain_id.as_str(), name.as_str()] {
            assert!(
                !endpoint.authority.contains(identity),
                "case {case:?}: {member}'s authority must name no resource identity \
                 ({identity:?}), got {url:?}"
            );
        }
    }
}

/// RULE: every operation that reports a domain announces both endpoint members at the one
/// authority this emulator answered the request on, with the identity in the path, whatever
/// address the request reached this emulator at and whatever name the domain carries.
#[tokio::test]
async fn an_announced_endpoint_names_its_identity_in_its_path() {
    // The longest name the model admits is 63 characters; the shortest is 3.
    let longest_name = format!("omni-{}", "x".repeat(58));
    // Rows pair an address this emulator is reached at with a name shape the domain carries.
    let rows: Vec<(&str, Option<&str>, &str, String)> = vec![
        (
            "an IPv4 address with a port, shortest name",
            Some("127.0.0.1:9297"),
            MODELLED_TARGET,
            "omn".to_string(),
        ),
        (
            "a loopback name with a port, hyphenated name",
            Some("localhost:9297"),
            MODELLED_TARGET,
            "omni-first-domain".to_string(),
        ),
        (
            "a bracketed IPv6 literal, longest name",
            Some("[::1]:9297"),
            MODELLED_TARGET,
            longest_name.clone(),
        ),
        (
            "an address with no port",
            Some("127.0.0.1"),
            MODELLED_TARGET,
            "omni-no-port".to_string(),
        ),
        (
            "a DNS host with a port",
            Some("omni.endpoint.example.test:8443"),
            MODELLED_TARGET,
            "omni-dns-host".to_string(),
        ),
        (
            "an absolute-form target and no header",
            None,
            "https://192.0.2.10:8443/service/CloudWatchOmniFrontend/operation/Unused",
            "omni-absolute-form".to_string(),
        ),
        (
            "no host at all",
            None,
            MODELLED_TARGET,
            "omni-unserved".to_string(),
        ),
    ];

    for (case, host, uri, name) in rows {
        // One emulator per row: an account holds at most one domain.
        let omni = Omni::new();
        let parts = signed_parts(host, uri);
        let created = omni.create_domain(&parts, &name).await;
        assert_identity_is_in_the_path(&format!("{case} (CreateDomain)"), &created);

        // The read and the patch project the same envelope, so the rule holds through both.
        let fetched = omni.get_domain(&parts, &created.domain_id).await;
        assert_identity_is_in_the_path(&format!("{case} (GetDomain)"), &fetched);
        assert_eq!(
            (
                fetched.domain_endpoint_url.as_str(),
                fetched.custom_endpoint_urls.as_deref()
            ),
            (
                created.domain_endpoint_url.as_str(),
                created.custom_endpoint_urls.as_deref()
            ),
            "case {case:?}: the read announces the create's own addresses for one address, got \
             {fetched:?}"
        );
    }
}

/// RULE: a rename moves ONLY the name-derived address — the id-addressed endpoint, the ARN and
/// the creation instant are unchanged, and the custom endpoint keeps the authority the id
/// endpoint is dialled at.
#[tokio::test]
async fn a_rename_moves_only_the_name_derived_endpoint() {
    let omni = Omni::new();
    let parts = signed_parts(Some("127.0.0.1:9297"), MODELLED_TARGET);
    let created = omni.create_domain(&parts, "omni-rename-first").await;
    assert_identity_is_in_the_path("the created domain", &created);

    // Rows differ along the shape of the name the domain is renamed to: a short one, a
    // multi-segment hyphenated one, one of the longest the model admits, and back to a short
    // one — each applied to the domain the row before left behind.
    for (case, new_name) in [
        ("a short name", "omn".to_string()),
        (
            "a multi-segment hyphenated name",
            "omni-rename-second-domain".to_string(),
        ),
        ("one of the longest names", format!("omni-{}", "y".repeat(58))),
        ("a short name again", "omni-last".to_string()),
    ] {
        let renamed = omni
            .rename_domain(&parts, &created.domain_id, &new_name)
            .await;
        assert_identity_is_in_the_path(case, &renamed);
        assert_eq!(
            renamed.name.as_deref(),
            Some(new_name.as_str()),
            "case {case:?}: the patch did not take the new name, got {:?}",
            renamed.name
        );

        let custom = announced_endpoint(case, &the_custom_endpoint(case, &renamed));
        let id_endpoint = announced_endpoint(case, &renamed.domain_endpoint_url);
        assert_eq!(
            custom.path,
            omni::state::domain_name_endpoint_path(&new_name),
            "case {case:?}: the custom endpoint's path must name the new name, got {:?}",
            custom.path
        );
        assert_eq!(
            custom.authority, id_endpoint.authority,
            "case {case:?}: the renamed domain's custom endpoint must keep the id endpoint's \
             authority {:?}, got {:?}",
            id_endpoint.authority, custom.authority
        );
        assert_eq!(
            (
                renamed.domain_endpoint_url.as_str(),
                renamed.domain_arn.as_str(),
                renamed.created_at.secs(),
                renamed.domain_id.as_str()
            ),
            (
                created.domain_endpoint_url.as_str(),
                created.domain_arn.as_str(),
                created.created_at.secs(),
                created.domain_id.as_str()
            ),
            "case {case:?}: the rename moved something other than the name-derived endpoint, \
             got {renamed:?}"
        );

        // And the read agrees with the patch, so the moved endpoint is the stored domain's.
        let fetched = omni.get_domain(&parts, &created.domain_id).await;
        assert_eq!(
            (
                fetched.domain_endpoint_url.as_str(),
                fetched.custom_endpoint_urls.as_deref()
            ),
            (
                renamed.domain_endpoint_url.as_str(),
                renamed.custom_endpoint_urls.as_deref()
            ),
            "case {case:?}: the read does not report the renamed domain's endpoints, got \
             {fetched:?}"
        );
    }
}

/// RULE: an announced endpoint is one this emulator SERVES at the address it announced — a
/// fetch of either member renders the stored domain, an address of the same shape naming no
/// domain of this emulator renders none, and after a rename the new address is served where
/// the old one no longer is.
#[tokio::test]
async fn an_announced_endpoint_is_served_from_the_stored_domain() {
    let omni = Omni::new();
    let parts = signed_parts(Some("127.0.0.1:9297"), MODELLED_TARGET);
    let created = omni.create_domain(&parts, "omni-host-served").await;
    let first_custom = the_custom_endpoint("the created domain", &created);

    // Rows differ along which announced endpoint is opened: the id-addressed one and the
    // name-addressed one.
    for (case, url) in [
        ("the id-addressed endpoint", created.domain_endpoint_url.clone()),
        ("the name-addressed endpoint", first_custom.clone()),
    ] {
        let (status, media_type, body) = omni.fetch("GET", &url).await;
        assert_eq!(
            status, 200,
            "case {case:?}: fetching {url:?} answered {status}: {body:?}"
        );
        assert!(
            media_type.starts_with("text/html"),
            "case {case:?}: expected a web page, got content-type {media_type:?}"
        );
        for stated in [created.domain_id.as_str(), REGION] {
            assert!(
                body.contains(stated),
                "case {case:?}: expected the page to state {stated:?}, got {body:?}"
            );
        }
    }

    // An address of the same shape naming no domain of this emulator renders no domain.
    let unknown = created
        .domain_endpoint_url
        .replace(&created.domain_id, "d-nothinghere");
    let (status, _media_type, body) = omni.fetch("GET", &unknown).await;
    assert_ne!(
        status, 200,
        "case \"an id naming no domain\": fetching {unknown:?} answered {status}: {body:?}"
    );
    assert!(
        !body.contains(REGION),
        "case \"an id naming no domain\": expected no domain page, got {body:?}"
    );

    // After a rename the NEW name's address is served and the old one is not: the custom
    // endpoint really moved, rather than both addresses answering.
    let renamed = omni
        .rename_domain(&parts, &created.domain_id, "omni-host-moved")
        .await;
    let moved_custom = the_custom_endpoint("the renamed domain", &renamed);
    let (status, _media_type, body) = omni.fetch("GET", &moved_custom).await;
    assert_eq!(
        status, 200,
        "case \"the renamed domain's custom endpoint\": fetching {moved_custom:?} answered \
         {status}: {body:?}"
    );
    assert!(
        body.contains(&created.domain_id),
        "case \"the renamed domain's custom endpoint\": expected the page to state the domain, \
         got {body:?}"
    );
    let (status, _media_type, body) = omni.fetch("GET", &first_custom).await;
    assert_ne!(
        status, 200,
        "case \"the endpoint the rename moved away from\": fetching {first_custom:?} answered \
         {status}: {body:?}"
    );
    assert!(
        !body.contains(REGION),
        "case \"the endpoint the rename moved away from\": expected no domain page, got {body:?}"
    );
}
