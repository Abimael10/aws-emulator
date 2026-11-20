// SPDX-License-Identifier: CC0-1.0

//! The AWS Marketplace reference data `DescribeAddonVersions` publishes for the
//! add-ons a caller subscribes to, driven over the wire through the same
//! `MultiServiceRouter` the production binary mounts.
//!
//! The RULE the first two suites assert: for an add-on sold through AWS
//! Marketplace, the catalog publishes the LISTING's own data — the seller's
//! published name as `publisher`, and that listing's Marketplace product
//! identifier as `marketplaceInformation.productId` — neither of which is
//! computed from the name the add-on is installed by, and neither of which is
//! computed from the other. An add-on Amazon EKS publishes itself is on no
//! listing and reports no `marketplaceInformation` at all.
//!
//! The RULE the third suite asserts: `marketplaceInformation.productUrl` is an
//! address a SUBSCRIBER FOLLOWS, so the catalog publishes the address THIS
//! EMULATOR serves that listing's page at — the one endpoint it serves its whole
//! surface at, followed by the listing's own path — and that address really
//! answers with that listing.
//!
//! Every suite walks EVERY Marketplace add-on the catalog publishes, read from the
//! publication itself, so the rows differ along what the rules generalise over:
//! which seller publishes the add-on (single-word, multi-word and dotted
//! published names), whether the add-on is a vendor's own product or its rebuild
//! of an Amazon EKS-managed add-on, which stock-keeping unit its listing is sold
//! under, and — for the address — where this emulator is served and which host the
//! request that reads the catalog carried.

use std::collections::{BTreeMap, BTreeSet};

use service_com_amazonaws_eks_awswesleyfrontend::state::{
    addon_marketplace_listing_path, addon_marketplace_listing_route, ADDON_OWNER_MARKETPLACE,
    ADDON_PUBLISHER_EKS, ADDON_PUBLISHER_SAGEMAKER,
};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use crate::wire::{call, emulator, Answer};

const REGION: &str = "us-west-2";

/// The published add-on catalog, narrowed by `query`.
async fn describe_addon_versions(router: &MultiServiceRouter, query: &str) -> Answer {
    let uri = format!("/addons/supported-versions?{query}");
    let answer = call(router, REGION, "GET", &uri, "").await;
    assert_eq!(
        answer.status, 200,
        "setup DescribeAddonVersions({query}): expected HTTP 200, got {answer:?}"
    );
    answer
}

/// The `addons` of one listing.
fn addons(answer: &Answer) -> Vec<serde_json::Value> {
    answer.body["addons"]
        .as_array()
        .unwrap_or_else(|| panic!("expected an addons list, got {:?}", answer.body))
        .clone()
}

/// A string member of a JSON object, or the empty string when it carries none.
fn text(value: &serde_json::Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// The whole catalog, keyed by add-on name.
async fn catalog(router: &MultiServiceRouter) -> BTreeMap<String, serde_json::Value> {
    let answer = describe_addon_versions(router, "").await;
    let rows: BTreeMap<String, serde_json::Value> = addons(&answer)
        .into_iter()
        .map(|addon| (text(&addon, "addonName"), addon))
        .collect();
    assert!(
        !rows.is_empty(),
        "setup: the catalog must publish add-ons, got {:?}",
        answer.body
    );
    rows
}

/// The vendor token an add-on's `<vendor>_<product>` name starts with, for an
/// add-on sold through AWS Marketplace; the empty string for one published under
/// a bare name.
fn vendor_token(addon_name: &str) -> String {
    addon_name
        .split_once('_')
        .map(|(vendor, _)| vendor.to_string())
        .unwrap_or_default()
}

/// A published seller name as its letters and digits alone, so "EG Innovations"
/// and "Solo.io" compare against the `eg-innovations` and `solo-io` tokens the
/// add-on names carry.
fn alphanumeric(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether `product_id` is the form an AWS Marketplace product identifier takes:
/// `prod-` and thirteen lower-case alphanumerics.
///
/// Source: the product identifiers the AWS Marketplace APIs publish, whose
/// `prod-<id>` form every listing's product id takes.
pub(crate) fn is_marketplace_product_id(product_id: &str) -> bool {
    let Some(identifier) = product_id.strip_prefix("prod-") else {
        return false;
    };
    identifier.len() == 13
        && identifier
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
}

/// The stock-keeping unit one listing is sold under, read off the address the
/// catalog publishes for it — `None` for an address that leads to no listing of
/// this emulator at all.
///
/// The address is this emulator's OWN (a URL a subscriber follows has to lead back
/// to the emulator that handed it over), so the reading is the service's own route:
/// an address it published resolves back to the listing behind it.
pub(crate) fn marketplace_listing_unit(product_url: &str) -> Option<&str> {
    addon_marketplace_listing_route(product_url)
}

// ---------------------------------------------------------------------------
// RULE: a Marketplace add-on's publisher and marketplaceInformation are the
// LISTING's own published data, carried per listing by the catalog — the seller's
// published name, that listing's Marketplace product identifier and that
// listing's product address — and an add-on Amazon EKS publishes itself carries
// no listing.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_marketplace_addon_publishes_its_sellers_own_listing() {
    let router = emulator();
    let catalog = catalog(&router).await;

    let marketplace: Vec<(&String, &serde_json::Value)> = catalog
        .iter()
        .filter(|(_, addon)| text(addon, "owner") == ADDON_OWNER_MARKETPLACE)
        .collect();
    assert!(
        marketplace.len() > 1,
        "setup: the catalog must publish several Marketplace add-ons, got {:?}",
        marketplace.len()
    );

    let mut product_ids: BTreeSet<String> = BTreeSet::new();
    let mut product_urls: BTreeSet<String> = BTreeSet::new();
    for (addon_name, addon) in &marketplace {
        let case = addon_name.as_str();

        // The seller's own published name: it names the same seller the add-on's
        // vendor token does, and it is NOT that lower-case token, which is what a
        // name taken off the add-on's name would be.
        let publisher = text(addon, "publisher");
        let token = vendor_token(case);
        assert!(
            !publisher.is_empty(),
            "case {case}: expected the seller's published name, got {:?}",
            addon["publisher"]
        );
        assert!(
            alphanumeric(&token).starts_with(&alphanumeric(&publisher)),
            "case {case}: the published name must name the seller {token:?}, got {publisher:?}"
        );
        assert_ne!(
            publisher, token,
            "case {case}: expected the seller's published name, got the add-on's name token"
        );

        // The listing it is subscribed through: the Marketplace product
        // identifier and the address that product is published at.
        let listing = addon
            .get("marketplaceInformation")
            .filter(|listing| !listing.is_null())
            .unwrap_or_else(|| {
                panic!("case {case}: a Marketplace add-on reports its listing, got {addon:?}")
            });
        let product_id = text(listing, "productId");
        let product_url = text(listing, "productUrl");
        assert!(
            is_marketplace_product_id(&product_id),
            "case {case}: expected a Marketplace product identifier, got {product_id:?}"
        );
        let listing_unit = marketplace_listing_unit(&product_url).unwrap_or_else(|| {
            panic!(
                "case {case}: expected an address this emulator serves the listing at, \
                 got {product_url:?}"
            )
        });

        // Two pieces of one listing's published data, so neither is computed from
        // the other: the address does not carry the product identifier.
        let identifier = product_id.trim_start_matches("prod-");
        assert!(
            !listing_unit.contains(identifier),
            "case {case}: the listing's address must not be built from its product id \
             {product_id:?}, got {product_url:?}"
        );
        // Nor from the add-on's own name, or the name of the software a rebuild
        // rebuilds.
        for name in [case, token.as_str()] {
            assert!(
                !product_url.contains(name) && !product_id.contains(name),
                "case {case}: the listing is the seller's data, not the add-on's name \
                 {name:?}, got {product_id:?} and {product_url:?}"
            );
        }

        // One listing per add-on: no two add-ons are sold as one product.
        assert!(
            product_ids.insert(product_id.clone()),
            "case {case}: two add-ons report one product id, got {product_id:?} twice"
        );
        assert!(
            product_urls.insert(product_url.clone()),
            "case {case}: two add-ons report one product address, got {product_url:?} twice"
        );

        // The same listing whichever filter path reaches the add-on, and the same
        // on a second read: the catalog is a publication.
        let by_name = describe_addon_versions(&router, &format!("addonName={case}")).await;
        let named = addons(&by_name);
        assert_eq!(
            named.len(),
            1,
            "case {case}: a name the catalog publishes answers exactly that add-on, got {:?}",
            by_name.body["addons"]
        );
        for member in ["publisher", "owner", "marketplaceInformation"] {
            assert_eq!(
                named[0][member], addon[member],
                "case {case}: the published {member} must not move between reads, got {:?}",
                named[0][member]
            );
        }
    }

    // An add-on Amazon EKS publishes itself is sold on no listing at all.
    for (addon_name, addon) in &catalog {
        if text(addon, "owner") == ADDON_OWNER_MARKETPLACE {
            continue;
        }
        let publisher = text(addon, "publisher");
        assert!(
            [ADDON_PUBLISHER_EKS, ADDON_PUBLISHER_SAGEMAKER].contains(&publisher.as_str()),
            "case {addon_name}: an add-on that is not sold on Marketplace is published by Amazon \
             EKS or by the Amazon Web Services service team that builds it, got {publisher:?}"
        );
        assert!(
            addon
                .get("marketplaceInformation")
                .is_none_or(serde_json::Value::is_null),
            "case {addon_name}: an add-on that is not sold on Marketplace has no listing, got {:?}",
            addon["marketplaceInformation"]
        );
    }
}

// ---------------------------------------------------------------------------
// RULE: the seller's published name is the value `DescribeAddonVersions$publishers`
// filters on, so a client that discovers an add-on by its seller's published name
// finds it — and the lower-case token the add-on's name starts with is not a
// publisher anything is published by.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_publishers_filter_matches_the_sellers_published_name() {
    let router = emulator();
    let catalog = catalog(&router).await;

    // Every seller the catalog publishes a Marketplace add-on for, with the
    // add-ons it publishes: the rows are whatever the publication offers.
    let mut sellers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (addon_name, addon) in &catalog {
        if text(addon, "owner") != ADDON_OWNER_MARKETPLACE {
            continue;
        }
        sellers
            .entry(text(addon, "publisher"))
            .or_default()
            .push(addon_name.clone());
    }
    assert!(
        sellers.len() > 1,
        "setup: the catalog must publish add-ons of several sellers, got {sellers:?}"
    );

    for (publisher, published) in &sellers {
        let case = publisher.as_str();
        // A published name may carry a space, which travels percent-encoded.
        let filter = publisher.replace(' ', "%20");
        let answer = describe_addon_versions(&router, &format!("publishers={filter}")).await;
        let kept: Vec<String> = addons(&answer)
            .iter()
            .map(|addon| text(addon, "addonName"))
            .collect();
        assert_eq!(
            &kept, published,
            "case {case}: the seller's published name keeps exactly its add-ons, got {kept:?}"
        );

        // The lower-case token the add-on names start with is not what the add-on
        // is published by, so it keeps nothing.
        let token = vendor_token(&published[0]);
        if token != *publisher {
            let by_token = describe_addon_versions(&router, &format!("publishers={token}")).await;
            assert!(
                addons(&by_token).is_empty(),
                "case {case}: {token:?} publishes nothing, got {:?}",
                by_token.body["addons"]
            );
        }
    }

    // And the sellers' add-ons are exactly the Marketplace-owned catalog.
    let by_owner =
        describe_addon_versions(&router, &format!("owners={ADDON_OWNER_MARKETPLACE}")).await;
    let marketplace_owned: BTreeSet<String> = addons(&by_owner)
        .iter()
        .map(|addon| text(addon, "addonName"))
        .collect();
    let by_seller: BTreeSet<String> = sellers.values().flatten().cloned().collect();
    assert_eq!(
        marketplace_owned, by_seller,
        "the Marketplace catalogue is what its sellers publish, got {marketplace_owned:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE: `marketplaceInformation.productUrl` is an address a SUBSCRIBER FOLLOWS, so
// what the catalog publishes is the address THIS EMULATOR serves that listing's
// page at — the one endpoint it serves its whole surface at, followed by the
// listing's own path — and a fetch of it really answers with that listing. An
// address on a host this emulator is not reached on is one a client can render but
// never dereference.
// ---------------------------------------------------------------------------

/// One addressing: where this emulator is SERVED, which host the reading request
/// carried, and the authority every address that read announces must name.
struct Addressing {
    case: &'static str,
    /// The base `OperationContext::endpoint` reports — the data port's own URL for
    /// a router served on a port, and `UNSERVED_URL_BASE` for one that is not.
    served_at: Option<&'static str>,
    /// The `Host` the reading request carried, or `None` for the request an SDK
    /// sends, which names this service's published endpoint in the URI.
    host: Option<&'static str>,
    /// The `host[:port]` of the one endpoint this emulator serves its whole surface
    /// at, which is the authority a published listing address has to name.
    authority: &'static str,
}

/// The rows the address rule is read over: where this emulator is served (on no
/// port at all, on a loopback port, on an IPv6 port, at a name) crossed with the
/// host the reading request carried (a loopback address, a loopback name, one of
/// AWS's own hostnames, none at all), which no announced address depends on.
fn addressings() -> Vec<Addressing> {
    vec![
        Addressing {
            case: "an SDK client naming this service's endpoint, on an unserved emulator",
            served_at: None,
            host: None,
            // `smithy_server_runtime::UNSERVED_URL_BASE`: the one base an emulator
            // served on no port reports, and still this emulator's own.
            authority: "emulator.invalid",
        },
        Addressing {
            case: "a client on the loopback address of an unserved emulator",
            served_at: None,
            host: Some("127.0.0.1:9297"),
            authority: "emulator.invalid",
        },
        Addressing {
            case: "an SDK client on an emulator served on a loopback port",
            served_at: Some("https://127.0.0.1:40305"),
            host: None,
            authority: "127.0.0.1:40305",
        },
        Addressing {
            case: "a client that resolves AWS's own hosts, on an emulator served on a port",
            served_at: Some("https://127.0.0.1:40305"),
            host: Some("eks.us-west-2.amazonaws.com"),
            authority: "127.0.0.1:40305",
        },
        Addressing {
            case: "a client naming the loopback by name, on an emulator served elsewhere",
            served_at: Some("https://127.0.0.1:40305"),
            host: Some("localhost:9297"),
            authority: "127.0.0.1:40305",
        },
        Addressing {
            case: "a client on an IPv6 literal, on an emulator served on an IPv6 port",
            served_at: Some("https://[::1]:9443"),
            host: Some("[::1]:9443"),
            authority: "[::1]:9443",
        },
        Addressing {
            case: "an SDK client on an emulator served at a name",
            served_at: Some("https://emulator.test:8443"),
            host: None,
            authority: "emulator.test:8443",
        },
    ]
}

/// The emulator under test, served at `base` — or on no port at all, which is what
/// [`emulator`] leaves it as.
fn emulator_served_at(base: Option<&str>) -> MultiServiceRouter {
    let mut router = emulator();
    if let Some(base) = base {
        router.set_url_base(
            EndpointUrl::parse(base).expect("the test endpoint base is a bare https URL"),
        );
    }
    router
}

/// The published catalog as read by a request that carried `host`, narrowed by
/// `query`.
async fn describe_addon_versions_from(
    router: &MultiServiceRouter,
    host: Option<&str>,
    query: &str,
) -> Answer {
    let mut builder = http::Request::builder()
        .method("GET")
        // What an SDK sends: this service's published endpoint in the URI.
        .uri(format!(
            "https://eks.{REGION}.amazonaws.com/addons/supported-versions?{query}"
        ))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={}/20260101/{REGION}/eks/aws4_request, \
                 SignedHeaders=host, Signature=deadbeef",
                crate::wire::DEFAULT_ACCESS_KEY_ID
            ),
        );
    if let Some(host) = host {
        builder = builder.header("host", host);
    }
    let request = builder
        .body(bytes::Bytes::new())
        .expect("a well-formed catalog read");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body: serde_json::Value = serde_json::from_slice(response.body()).unwrap_or_default();
    assert_eq!(
        status, 200,
        "setup DescribeAddonVersions({query}) from {host:?}: expected HTTP 200, got {status} \
         {body:?}"
    );
    Answer {
        status,
        error_type: None,
        body,
    }
}

/// What a fetch of a published address answers: its status, its `content-type`, its
/// `content-length` and the document itself — a page request as a browser sends
/// one, carrying no credential at all.
async fn fetch(
    router: &MultiServiceRouter,
    method: &str,
    url: &str,
) -> (u16, String, String, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(url)
        .body(bytes::Bytes::new())
        .expect("a well-formed page request");
    let response = router.dispatch(request).await;
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    (
        response.status().as_u16(),
        header("content-type"),
        header("content-length"),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

#[tokio::test]
async fn a_listings_address_names_the_endpoint_this_emulator_is_served_at() {
    // One read per addressing, each walking every Marketplace add-on the catalog
    // publishes.
    let mut units_per_addressing: Vec<(&str, BTreeMap<String, String>)> = Vec::new();
    for Addressing {
        case: addressing,
        served_at,
        host,
        authority,
    } in addressings()
    {
        let router = emulator_served_at(served_at);
        let answer = describe_addon_versions_from(
            &router,
            host,
            &format!("owners={ADDON_OWNER_MARKETPLACE}"),
        )
        .await;
        let published = addons(&answer);
        assert!(
            published.len() > 1,
            "case {addressing:?}: the catalog must publish several Marketplace add-ons, got {:?}",
            answer.body["addons"]
        );
        let mut units: BTreeMap<String, String> = BTreeMap::new();
        for addon in &published {
            let addon_name = text(addon, "addonName");
            let case = format!("{addressing:?} {addon_name}");
            let listing = &addon["marketplaceInformation"];
            let product_url = text(listing, "productUrl");
            let unit = marketplace_listing_unit(&product_url).unwrap_or_else(|| {
                panic!(
                    "case {case}: expected an address this emulator serves the listing at, \
                     got {product_url:?}"
                )
            });
            // The whole address: the endpoint this emulator is served at, and the
            // path it serves that listing's page on. Nothing of the AWS Marketplace
            // host, which this emulator is not reached on.
            let expected = format!("https://{authority}{}", addon_marketplace_listing_path(unit));
            assert_eq!(
                product_url, expected,
                "case {case}: expected the listing published at {expected:?}, got {product_url:?}"
            );
            for foreign in ["aws.amazon.com", "amazonaws.com", "api.aws"] {
                assert!(
                    !product_url.contains(foreign),
                    "case {case}: expected no address on a {foreign} host, got {product_url:?}"
                );
            }
            units.insert(addon_name, unit.to_string());
        }
        units_per_addressing.push((addressing, units));
    }

    // The listing itself does not move with the addressing: only the address it is
    // announced on does.
    let (first_case, first) = &units_per_addressing[0];
    for (case, units) in &units_per_addressing {
        assert_eq!(
            units, first,
            "case {case:?}: the listings must be the ones {first_case:?} read too, got {units:?}"
        );
    }
}

#[tokio::test]
async fn every_published_listing_address_answers_with_that_listing() {
    let router = emulator_served_at(Some("https://127.0.0.1:40305"));
    let answer =
        describe_addon_versions_from(&router, None, &format!("owners={ADDON_OWNER_MARKETPLACE}"))
            .await;

    for addon in &addons(&answer) {
        let addon_name = text(addon, "addonName");
        let case = addon_name.as_str();
        let listing = &addon["marketplaceInformation"];
        let product_id = text(listing, "productId");
        let product_url = text(listing, "productUrl");
        let publisher = text(addon, "publisher");

        // A subscriber opens the address, so it answers that listing's page.
        let (status, media_type, length, page) = fetch(&router, "GET", &product_url).await;
        assert_eq!(
            status, 200,
            "case {case}: expected the listing page at {product_url:?} to answer 200, \
             got {status} {page:?}"
        );
        assert!(
            media_type.starts_with("text/html"),
            "case {case}: expected a web page, got {media_type:?}"
        );
        assert_eq!(
            length,
            page.len().to_string(),
            "case {case}: expected the page's own length, got {length:?}"
        );
        for named in [case, product_id.as_str(), publisher.as_str()] {
            assert!(
                page.contains(named),
                "case {case}: expected the page to state {named:?}, got {page:?}"
            );
        }

        // And asking only what is there answers the same page's length with no body.
        let (head_status, _, head_length, head_body) = fetch(&router, "HEAD", &product_url).await;
        assert_eq!(
            (head_status, head_length.as_str()),
            (200, length.as_str()),
            "case {case}: expected HEAD to report the page, got {head_status} {head_length:?}"
        );
        assert!(
            head_body.is_empty(),
            "case {case}: expected HEAD to carry no body, got {head_body:?}"
        );

        // The listing a name-filtered read reports is the same one, with the add-on's
        // builds beside it: the read a client version-pins from.
        let by_name = describe_addon_versions_from(&router, None, &format!("addonName={case}")).await;
        let named = addons(&by_name);
        assert_eq!(
            named.len(),
            1,
            "case {case}: a name the catalog publishes answers exactly that add-on, got {:?}",
            by_name.body["addons"]
        );
        assert_eq!(
            named[0]["marketplaceInformation"], *listing,
            "case {case}: expected the same listing on the filtered read, got {:?}",
            named[0]["marketplaceInformation"]
        );
        assert!(
            named[0]["addonVersions"]
                .as_array()
                .is_some_and(|versions| !versions.is_empty()),
            "case {case}: expected the add-on's published builds, got {:?}",
            named[0]["addonVersions"]
        );
    }
}

#[tokio::test]
async fn an_address_of_the_listing_shape_that_names_no_listing_serves_no_page() {
    let authority = "127.0.0.1:40305";
    let router = emulator_served_at(Some(&format!("https://{authority}")));
    let answer =
        describe_addon_versions_from(&router, None, &format!("owners={ADDON_OWNER_MARKETPLACE}"))
            .await;
    let published = addons(&answer);
    let listing = &published[0]["marketplaceInformation"];
    let product_id = text(listing, "productId");
    let unit = marketplace_listing_unit(&text(listing, "productUrl"))
        .expect("a published listing address")
        .to_string();

    // The rows span the addresses of this shape that name no listing: a unit nothing
    // is sold under, the listing's PRODUCT ID in the unit's place (the address is not
    // built from it), the unit of a published listing with something below it, and
    // the plane with no unit at all.
    let cases: Vec<(&str, String)> = vec![
        (
            "a unit nothing is sold under",
            addon_marketplace_listing_path("d4hnqz7wmvbt3xkrl9jpsc26z"),
        ),
        (
            "the listing's product id in the unit's place",
            addon_marketplace_listing_path(&product_id),
        ),
        (
            "a path below a published listing",
            format!("{}/terms", addon_marketplace_listing_path(&unit)),
        ),
        (
            "the plane with no unit at all",
            addon_marketplace_listing_path(""),
        ),
    ];

    for (case, path) in cases {
        let (status, _, _, page) =
            fetch(&router, "GET", &format!("https://{authority}{path}")).await;
        assert_ne!(
            status, 200,
            "case {case:?} ({path}): expected no listing page, got 200 {page:?}"
        );
        assert!(
            !page.contains("Product ID"),
            "case {case:?} ({path}): expected no listing's data, got {page:?}"
        );
    }
}
