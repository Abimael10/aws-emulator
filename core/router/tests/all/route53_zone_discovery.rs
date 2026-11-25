// SPDX-License-Identifier: CC0-1.0

//! The walk a tag-driven client makes over its OWN Route 53 hosted zones, written ONCE:
//! create a zone, write tags on it, page `ListHostedZones` to the end, read a zone's tags
//! back with `ListTagsForResource`, select the zones a tag marks, and publish a record in
//! the zone that walk resolved.
//!
//! It lives here for the reason `query_wire.rs` does: a suite that writes its own copy
//! drifts from the others. A walk that follows `NextMarker` one page differently, or that
//! reads `Config/PrivateZone` out of the wrong element, changes what the suite is actually
//! asserting — and the pagination contract itself (a page no longer than the `maxitems`
//! asked for, `NextMarker` present exactly when `IsTruncated` is true) is asserted here, in
//! one place, so every caller gets it.
//!
//! `tagged_hosted_zone_selects_a_sending_domain.rs`,
//! `ses_sending_switch_gates_sends_from_a_tag_discovered_domain.rs` and
//! `tag_discovered_zone_receives_mail_into_s3.rs` still carry their own copies of this
//! walk, written before this module existed; they are to be pointed at it by their own
//! suites' cleanup. Nothing new should write another one — call this module.
//!
//! Route 53 is global, so every call here addresses the one endpoint host that routes the
//! service and signs for one Region; the signature is never checked, since every suite in
//! this crate runs its router with request authentication off.

use std::collections::BTreeMap;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{blocks_of, form_encode, opt_element};

/// The endpoint host a Route 53 request addresses, which is also the routing signal the
/// router resolves that service from.
pub(crate) const ROUTE53_HOST: &str = "route53.amazonaws.com";

/// The `ResourceType` a hosted zone is tagged under (`TagResourceType$hostedzone`).
pub(crate) const HOSTED_ZONE: &str = "hostedzone";

/// The Region a Route 53 request is signed for. The service is global, so the value only
/// has to be a Region: nothing of a zone or a tag depends on it.
const SIGNING_REGION: &str = "us-east-1";

/// The XML namespace every request document of this API version carries.
const XMLNS: &str = "https://route53.amazonaws.com/doc/2013-04-01/";

/// Call one Route 53 operation over its restXml wire, requiring HTTP `expected`.
pub(crate) async fn route53(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    uri: &str,
    body: &str,
    expected: u16,
) -> String {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{SIGNING_REGION}/route53/\
         aws4_request, SignedHeaders=host, \
         Signature=0000000000000000000000000000000000000000000000000000000000000000"
    );
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{ROUTE53_HOST}{uri}"))
        .header(http::header::HOST, ROUTE53_HOST)
        .header(http::header::AUTHORIZATION, authorization)
        .header(http::header::CONTENT_TYPE, "application/xml")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    // A request exactly one service claims: the router's own refusals would otherwise read
    // as an empty listing or an absent tag.
    for refusal in ["AmbiguousService", "UnknownService"] {
        assert!(
            !text.contains(refusal),
            "case {case:?}: Route 53 {method} {uri} must be served by exactly one service, got \
             HTTP {status}: {text}"
        );
    }
    assert_eq!(
        status, expected,
        "case {case:?}: Route 53 {method} {uri} expected HTTP {expected}, got {status} with \
         body {text}"
    );
    text
}

/// One enumerated hosted zone, as a client reads it off `ListHostedZones`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ListedZone {
    /// The id with the `/hostedzone/` prefix stripped, which is the form every
    /// `ResourceId` member takes — what a client keeps "after the last '/'".
    pub(crate) id: String,
    /// The apex, as the service canonicalised it.
    pub(crate) name: String,
    /// `Config/PrivateZone`, which the enumeration always reports.
    pub(crate) private: bool,
}

impl ListedZone {
    /// This zone's apex in the form two spellings of one DNS name compare in: no trailing
    /// dot, lower case.
    pub(crate) fn apex(&self) -> String {
        self.name.trim_end_matches('.').to_ascii_lowercase()
    }
}

/// Page `ListHostedZones` to the end at `page_size` (`None` sends no `maxitems` at all),
/// answering every zone it enumerated.
///
/// This IS the client-side procedure under test wherever a suite resolves one of the
/// account's own zones: follow `NextMarker` while `IsTruncated` is true, exactly as an SDK
/// paginator does. The pagination contract it rests on is asserted as it goes — a page no
/// longer than the `maxitems` asked for, and `NextMarker` present exactly when
/// `IsTruncated` is true — and the walk is bounded by `page_limit` pages, which a caller
/// sets from the zones the account holds, so a marker that stops advancing fails rather
/// than hangs.
pub(crate) async fn all_zones(
    router: &MultiServiceRouter,
    case: &str,
    page_size: Option<i32>,
    page_limit: usize,
) -> Vec<ListedZone> {
    let mut seen: Vec<ListedZone> = Vec::new();
    let mut marker: Option<String> = None;
    for page_number in 1..=page_limit {
        let mut query: Vec<String> = Vec::new();
        if let Some(size) = page_size {
            query.push(format!("maxitems={size}"));
        }
        if let Some(at) = &marker {
            query.push(format!("marker={}", form_encode(at)));
        }
        let uri = if query.is_empty() {
            "/2013-04-01/hostedzone".to_string()
        } else {
            format!("/2013-04-01/hostedzone?{}", query.join("&"))
        };
        let page = route53(router, case, "GET", &uri, "", 200).await;

        let listed = blocks_of(&page, "HostedZone");
        if let Some(size) = page_size {
            assert!(
                listed.len() <= size as usize,
                "case {case:?}: a page carries at most the maxitems asked for ({size}), got {} \
                 in {page}",
                listed.len()
            );
        }
        for zone in listed {
            let id = opt_element(zone, "Id").unwrap_or_else(|| {
                panic!("case {case:?}: an enumerated zone must carry an Id: {zone}")
            });
            let private = opt_element(zone, "PrivateZone").unwrap_or_else(|| {
                panic!("case {case:?}: an enumerated zone must report Config/PrivateZone: {zone}")
            });
            seen.push(ListedZone {
                id: id.rsplit('/').next().unwrap_or_default().to_string(),
                name: opt_element(zone, "Name").unwrap_or_default(),
                private: private == "true",
            });
        }

        let truncated = opt_element(&page, "IsTruncated").unwrap_or_default() == "true";
        let next = opt_element(&page, "NextMarker");
        assert_eq!(
            truncated,
            next.is_some(),
            "case {case:?}: NextMarker is present exactly when IsTruncated is true, got \
             IsTruncated {truncated} and NextMarker {next:?}"
        );
        if !truncated {
            return seen;
        }
        marker = next;
        assert!(
            page_number < page_limit,
            "case {case:?}: the enumeration did not finish within {page_limit} page(s), so a \
             marker is not advancing"
        );
    }
    seen
}

/// The tag map `ListTagsForResource` reports for one hosted zone, which also requires that
/// the tag set names the zone it was read for.
pub(crate) async fn zone_tags(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
) -> BTreeMap<String, String> {
    let read = route53(
        router,
        case,
        "GET",
        &format!("/2013-04-01/tags/{HOSTED_ZONE}/{id}"),
        "",
        200,
    )
    .await;
    assert_eq!(
        opt_element(&read, "ResourceId").as_deref(),
        Some(id),
        "case {case:?}: the tag set must name the zone it was read for, got {read}"
    );
    blocks_of(&read, "Tag")
        .into_iter()
        .map(|tag| {
            (
                opt_element(tag, "Key").unwrap_or_default(),
                opt_element(tag, "Value").unwrap_or_default(),
            )
        })
        .collect()
}

/// Write `tags` on a hosted zone with `ChangeTagsForResource`. A batch that asks for
/// nothing is not sent: `AddTags` is `@length(min: 1, …)`.
pub(crate) async fn tag_zone(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    tags: &[(&str, &str)],
) {
    if tags.is_empty() {
        return;
    }
    let written: String = tags
        .iter()
        .map(|(key, value)| format!("<Tag><Key>{key}</Key><Value>{value}</Value></Tag>"))
        .collect();
    route53(
        router,
        case,
        "POST",
        &format!("/2013-04-01/tags/{HOSTED_ZONE}/{id}"),
        &format!(
            "<ChangeTagsForResourceRequest xmlns=\"{XMLNS}\"><AddTags>{written}</AddTags>\
             </ChangeTagsForResourceRequest>"
        ),
        200,
    )
    .await;
}

/// Create one hosted zone for `apex` and answer the BARE (`Z…`) id Route 53 minted, which
/// is the form every call that takes a zone id takes.
///
/// A PRIVATE zone is created for one VPC, which is what makes it private
/// (`HostedZoneConfig$PrivateZone`: "visible only in the VPCs that you specify").
pub(crate) async fn create_zone(
    router: &MultiServiceRouter,
    case: &str,
    apex: &str,
    private: bool,
    reference: &str,
) -> String {
    let (vpc, config) = if private {
        (
            "<VPC><VPCId>vpc-0a1b2c3d</VPCId><VPCRegion>us-west-2</VPCRegion></VPC>",
            "<HostedZoneConfig><PrivateZone>true</PrivateZone></HostedZoneConfig>",
        )
    } else {
        ("", "")
    };
    let created = route53(
        router,
        case,
        "POST",
        "/2013-04-01/hostedzone",
        &format!(
            "<CreateHostedZoneRequest xmlns=\"{XMLNS}\"><Name>{apex}</Name>{vpc}\
             <CallerReference>{reference}</CallerReference>{config}</CreateHostedZoneRequest>"
        ),
        201,
    )
    .await;
    opt_element(&created, "Id")
        .unwrap_or_else(|| panic!("case {case:?}: the create must answer HostedZone/Id: {created}"))
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The zones a tag-driven client selects, as `apex -> bare zone id`: page the enumeration
/// at `page_size`, keep the zones whose `Config/PrivateZone` says they are PUBLIC, read
/// each one's tags and keep the zones carrying `selector`.
///
/// WHICH tag a client selects by is the account's own choice, never part of the API, so the
/// selector is the caller's.
pub(crate) async fn tag_selected_public_zones(
    router: &MultiServiceRouter,
    case: &str,
    page_size: Option<i32>,
    page_limit: usize,
    selector: (&str, &str),
) -> BTreeMap<String, String> {
    let mut selected = BTreeMap::new();
    for zone in all_zones(router, case, page_size, page_limit).await {
        if zone.private {
            continue;
        }
        let tags = zone_tags(router, case, &zone.id).await;
        if tags.get(selector.0).map(String::as_str) != Some(selector.1) {
            continue;
        }
        selected.insert(zone.apex(), zone.id);
    }
    selected
}

/// The `Comment` a publication carries, which describes the change BATCH rather than the
/// record in it (`ChangeBatch$Comment`: "optional: any comments you want to include about
/// a change batch request"), so one sentence serves every caller.
const PUBLICATION_COMMENT: &str = "published by the account that holds the zone";

/// Publish one record set in `zone` with `ChangeResourceRecordSets`, which is how the
/// account answers a challenge another service handed it out.
pub(crate) async fn publish_record(
    router: &MultiServiceRouter,
    case: &str,
    zone: &str,
    name: &str,
    record_type: &str,
    ttl: &str,
    value: &str,
) {
    route53(
        router,
        case,
        "POST",
        &format!("/2013-04-01/hostedzone/{zone}/rrset"),
        &format!(
            "<ChangeResourceRecordSetsRequest xmlns=\"{XMLNS}\"><ChangeBatch>\
             <Comment>{PUBLICATION_COMMENT}</Comment><Changes><Change><Action>UPSERT</Action>\
             <ResourceRecordSet><Name>{name}</Name><Type>{record_type}</Type><TTL>{ttl}</TTL>\
             <ResourceRecords><ResourceRecord><Value>{value}</Value></ResourceRecord>\
             </ResourceRecords></ResourceRecordSet></Change></Changes></ChangeBatch>\
             </ChangeResourceRecordSetsRequest>"
        ),
        200,
    )
    .await;
}
