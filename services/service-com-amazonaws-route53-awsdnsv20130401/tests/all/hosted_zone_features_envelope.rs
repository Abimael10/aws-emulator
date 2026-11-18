// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of ONE rule: every response that projects a
//! `HostedZone` serialises the modelled `Features` container for it.
//!
//! The rule under test: `HostedZone$Features` is part of the shape Route 53
//! projects, so every operation answering with a hosted zone carries the
//! container, and the container reports one child per feature status that zone
//! HOLDS, read from the zone's own state — the public-DNS fallback status on a
//! zone the feature applies to, which is a zone answered on the public internet
//! and so never a private one, plus `AcceleratedRecoveryStatus` once an accepted
//! `UpdateHostedZoneFeatures` has configured one. A client deserialising
//! `HostedZone` into a strict shape therefore always finds the member, a feature
//! a zone HAS is never reported as an empty container, and a zone holding no
//! feature status reports the container empty rather than inventing one.
//!
//! One zone read by one operation is ONE row of that class, so the tables below
//! span it: zones differing in label count, in public/private, in whether they
//! carry a comment and in whether their features were ever configured, each
//! asserted on EVERY operation that projects a `HostedZone` — the create, the
//! single read, the comment update and both listings — because the rule is
//! about the projection of the shape, not about one operation.
//!
//! Every test drives the service through the real router, using the SAME
//! harness the sibling hosted-zone suites use, so what is asserted is the
//! HTTP/restXml behaviour a Route 53 client sees.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_route53_awsdnsv20130401::state::ACCELERATED_RECOVERY_ENABLING;

use crate::hosted_zone_records::{call, create_zone_body, escape_xml, Reply};
use crate::resource_tags::element_text;

/// How long a test keeps looking for an accepted features update to have
/// settled before it gives up. Generous: the service's own span is far shorter,
/// and the assertion waits for the CONDITION rather than for a span, so a
/// loaded host only polls more often.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long one look at a settling zone waits before the next.
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// The members `HostedZoneFeatures` declares, from
/// `smithy_query(query="shape", shape="com.amazonaws.route53#HostedZoneFeatures")`:
/// the accelerated-recovery status and the failure reasons of a features
/// change, neither of which a zone has a status for until a features update
/// configures one, plus the public-DNS fallback status a zone the feature
/// applies to has.
const FEATURES_MEMBERS: [&str; 3] = [
    "AcceleratedRecoveryStatus",
    "FailureReasons",
    PUBLIC_DNS_FALLBACK_STATUS,
];

/// The `HostedZoneFeatures` member reporting whether a zone falls back to
/// public DNS.
const PUBLIC_DNS_FALLBACK_STATUS: &str = "PublicDNSFallbackStatus";

/// The status Route 53 reports for a hosted-zone feature that is turned off,
/// which is what every zone this API version creates reports for public DNS
/// fallback: no operation it models turns that feature on.
const FEATURE_OFF: &str = "DISABLED";

/// A router with just this service mounted on a fresh shared engine.
fn router() -> MultiServiceRouter {
    crate::hosted_zone_records::router()
}

/// Create a zone with an arbitrary `HostedZoneConfig`/`VPC` block.
async fn create_zone(
    router: &MultiServiceRouter,
    name: &str,
    caller_reference: &str,
    extra: &str,
) -> Reply {
    call(
        router,
        "POST",
        "/2013-04-01/hostedzone",
        &create_zone_body(name, caller_reference, extra),
    )
    .await
}

/// The bare (`Z…`) id from a reply carrying a `HostedZone`.
fn bare_zone_id(reply: &Reply) -> String {
    let id = reply
        .first("HostedZone/Id")
        .unwrap_or_else(|| panic!("the reply must carry HostedZone/Id; body {}", reply.body));
    id.trim_start_matches("/hostedzone/").to_string()
}

/// The contents of every `<Features>` container in a body, in document order.
///
/// Read out of the RAW body rather than out of the flattened document, because
/// the question this file asks is whether the CONTAINER is serialised — and an
/// empty container carries no text to flatten. `<Features/>` and
/// `<Features></Features>` both count, as the empty container they are.
fn features_blocks(body: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find("<Features") {
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("<Features/>") {
            blocks.push("");
            rest = after;
        } else if let Some(inner) = tail.strip_prefix("<Features>") {
            let end = inner.find("</Features>").unwrap_or_else(|| {
                panic!("a Features container must be closed; body {body}")
            });
            blocks.push(&inner[..end]);
            rest = &inner[end + "</Features>".len()..];
        } else {
            panic!("unexpected element starting with <Features; body {body}");
        }
    }
    blocks
}

/// Assert a reply carries exactly `zones` `Features` containers — one per
/// hosted zone it projects — and hand them back.
fn features_of<'a>(case: &str, reply: &'a Reply, zones: usize) -> Vec<&'a str> {
    let blocks = features_blocks(&reply.body);
    assert_eq!(
        blocks.len(),
        zones,
        "case {case:?}: expected one Features container per projected hosted zone ({zones}), \
         got {}; body {}",
        blocks.len(),
        reply.body
    );
    blocks
}

/// Assert one `Features` container reports exactly `status` for
/// `AcceleratedRecoveryStatus`, `fallback` for the zone's public-DNS fallback
/// status, and nothing for the members the zone's state does not fill.
fn assert_features(case: &str, block: &str, status: Option<&str>, fallback: Option<&str>) {
    // Reported exactly where the feature applies, with the off status — nothing
    // this API version models turns it on.
    let reported_fallback = element_text(block, PUBLIC_DNS_FALLBACK_STATUS);
    assert_eq!(
        reported_fallback.as_deref(),
        fallback,
        "case {case:?}: expected {PUBLIC_DNS_FALLBACK_STATUS} {fallback:?} in the Features \
         container, got {reported_fallback:?}; container {block:?}"
    );
    let reported = element_text(block, "AcceleratedRecoveryStatus");
    assert_eq!(
        reported.as_deref(),
        status,
        "case {case:?}: expected AcceleratedRecoveryStatus {status:?} in the Features \
         container, got {reported:?}; container {block:?}"
    );
    assert!(
        element_text(block, "FailureReasons").is_none(),
        "case {case:?}: a features change this service accepts reports no FailureReasons, \
         got {:?}; container {block:?}",
        element_text(block, "FailureReasons")
    );
    // Nothing the model does not declare is serialised inside the container.
    for element in element_names(block) {
        assert!(
            FEATURES_MEMBERS.contains(&element),
            "case {case:?}: {element:?} is not a HostedZoneFeatures member; container {block:?}"
        );
    }
}

/// The name of every element opened directly or indirectly inside one stretch
/// of XML, in document order.
fn element_names(block: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = block;
    while let Some(at) = rest.find('<') {
        let tail = &rest[at + 1..];
        let end = tail.find('>').unwrap_or_else(|| {
            panic!("the container has an unterminated tag; container {block:?}")
        });
        let tag = tail[..end].trim_start_matches('/').trim_end_matches('/');
        if let Some(name) = tag.split_whitespace().next() {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        rest = &tail[end + 1..];
    }
    names
}

/// `GetHostedZone` for one zone.
async fn get_zone(router: &MultiServiceRouter, zone_id: &str) -> Reply {
    call(router, "GET", &format!("/2013-04-01/hostedzone/{zone_id}"), "").await
}

/// `UpdateHostedZoneComment` for one zone, which answers with the zone too.
async fn update_comment(router: &MultiServiceRouter, zone_id: &str, comment: &str) -> Reply {
    call(
        router,
        "POST",
        &format!("/2013-04-01/hostedzone/{zone_id}"),
        &format!(
            "<UpdateHostedZoneCommentRequest \
             xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\"><Comment>{}</Comment>\
             </UpdateHostedZoneCommentRequest>",
            escape_xml(comment)
        ),
    )
    .await
}

/// `UpdateHostedZoneFeatures` for one zone.
async fn update_features(router: &MultiServiceRouter, zone_id: &str, enable: bool) -> Reply {
    call(
        router,
        "POST",
        &format!("/2013-04-01/hostedzone/{zone_id}/features"),
        &format!(
            "<UpdateHostedZoneFeaturesRequest \
             xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
             <EnableAcceleratedRecovery>{enable}</EnableAcceleratedRecovery>\
             </UpdateHostedZoneFeaturesRequest>"
        ),
    )
    .await
}

/// One row of the zone table: how the zone is created, and whether the feature
/// applies to it.
struct ZoneRow {
    case: &'static str,
    name: &'static str,
    /// The `HostedZoneConfig` (and, for a private zone, `VPC`) block sent.
    extra: &'static str,
    /// Whether the zone is PRIVATE — visible only in the VPCs it names, and so
    /// never answered on the public internet, which is what decides whether it
    /// has a public-DNS fallback status at all.
    private: bool,
}

impl ZoneRow {
    /// The public-DNS fallback status this zone must report: the off status on a
    /// public zone, and none at all on a private one.
    fn expected_fallback(&self) -> Option<&'static str> {
        (!self.private).then_some(FEATURE_OFF)
    }
}

/// The zones the tables below span: label count, public/private and comment
/// differ from row to row.
fn zone_rows() -> [ZoneRow; 6] {
    [
        ZoneRow {
            case: "a public zone created with no HostedZoneConfig",
            name: "features-plain.example.com",
            extra: "",
            private: false,
        },
        ZoneRow {
            case: "a public zone created with a comment",
            name: "features-commented.example.com",
            extra: "<HostedZoneConfig><Comment>commented</Comment></HostedZoneConfig>",
            private: false,
        },
        ZoneRow {
            case: "a three-label public zone",
            name: "zone.sub.features.example.net",
            extra: "",
            private: false,
        },
        ZoneRow {
            case: "a public zone named with a trailing dot",
            name: "features-dotted.example.org.",
            extra: "",
            private: false,
        },
        ZoneRow {
            case: "a private zone with one VPC",
            name: "features-private.example.com",
            extra: "<HostedZoneConfig><PrivateZone>true</PrivateZone></HostedZoneConfig>\
                    <VPC><VPCRegion>us-east-1</VPCRegion><VPCId>vpc-1a2b3c4d</VPCId></VPC>",
            private: true,
        },
        ZoneRow {
            case: "a private zone with a comment, in another region",
            name: "features-private-commented.example.co.uk",
            extra: "<HostedZoneConfig><Comment>private</Comment>\
                    <PrivateZone>true</PrivateZone></HostedZoneConfig>\
                    <VPC><VPCRegion>eu-west-1</VPCRegion><VPCId>vpc-9f8e7d6c</VPCId></VPC>",
            private: true,
        },
    ]
}

#[tokio::test]
async fn every_hosted_zone_projection_carries_the_features_container() {
    // The rule: the `Features` container is serialised for every projected
    // hosted zone, carrying the status of each feature the zone holds — for a
    // public zone nothing has configured, the public-DNS fallback status alone,
    // and for a private one, which that feature does not apply to, nothing. The
    // rows span the zones; each one is asserted on the create, the single read
    // and the comment update, and the listings are asserted over all of them at
    // once.
    let router = router();
    let rows = zone_rows();
    let mut ids = Vec::new();

    for (index, row) in rows.iter().enumerate() {
        let created = create_zone(
            &router,
            row.name,
            &format!("features-envelope-{index}"),
            row.extra,
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: expected 201 from CreateHostedZone, got {} with body {}",
            row.case, created.status, created.body
        );
        let blocks = features_of(row.case, &created, 1);
        assert_features(row.case, blocks[0], None, row.expected_fallback());

        let zone_id = bare_zone_id(&created);
        let read = get_zone(&router, &zone_id).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: expected 200 from GetHostedZone, got {} with body {}",
            row.case, read.status, read.body
        );
        let read_blocks = features_of(row.case, &read, 1);
        assert_eq!(
            read_blocks, blocks,
            "case {:?}: GetHostedZone must report the Features the create did, got {:?} \
             after {:?}; body {}",
            row.case, read_blocks, blocks, read.body
        );

        let updated = update_comment(&router, &zone_id, "a later comment").await;
        assert_eq!(
            updated.status, 200,
            "case {:?}: expected 200 from UpdateHostedZoneComment, got {} with body {}",
            row.case, updated.status, updated.body
        );
        let updated_blocks = features_of(row.case, &updated, 1);
        assert_features(row.case, updated_blocks[0], None, row.expected_fallback());

        ids.push(zone_id);
    }

    // The same projection on both LIST paths: one container per listed zone, and
    // the fallback status on exactly the zones the feature applies to.
    let public_zones = rows.iter().filter(|row| !row.private).count();
    for (case, uri) in [
        ("ListHostedZones", "/2013-04-01/hostedzone"),
        (
            "ListHostedZonesByName",
            "/2013-04-01/hostedzonesbyname",
        ),
    ] {
        let listed = call(&router, "GET", uri, "").await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected 200, got {} with body {}",
            listed.status, listed.body
        );
        let blocks = features_of(case, &listed, ids.len());
        let reporting = blocks
            .iter()
            .filter_map(|block| fallback_status(block))
            .collect::<Vec<String>>();
        assert_eq!(
            reporting.len(),
            public_zones,
            "case {case:?}: expected the {public_zones} public zones of the listing to \
             report {PUBLIC_DNS_FALLBACK_STATUS}, got {reporting:?}; body {}",
            listed.body
        );
        for seen in &reporting {
            assert_eq!(
                seen, FEATURE_OFF,
                "case {case:?}: expected {FEATURE_OFF:?} for {PUBLIC_DNS_FALLBACK_STATUS}, \
                 got {seen:?}; body {}",
                listed.body
            );
        }
        for block in &blocks {
            assert!(
                element_text(block, "AcceleratedRecoveryStatus").is_none(),
                "case {case:?}: a zone nothing configured reports no \
                 AcceleratedRecoveryStatus; container {block:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_features_container_reports_the_configured_feature_on_every_projection() {
    // The rule's other half: the container is the zone's own state, so once an
    // accepted update has configured a zone, EVERY projection of that zone
    // reports the status in the same always-present container — while the zones
    // beside it report only the fallback status. The rows are the public zones of
    // the table above, which are the zones accelerated recovery applies to.
    let router = router();
    let rows = zone_rows();
    let mut configured = Vec::new();
    let mut untouched = Vec::new();

    for (index, row) in rows.iter().enumerate() {
        let created = create_zone(
            &router,
            row.name,
            &format!("features-configured-{index}"),
            row.extra,
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: expected 201 from CreateHostedZone, got {} with body {}",
            row.case, created.status, created.body
        );
        let zone_id = bare_zone_id(&created);
        if row.extra.contains("<PrivateZone>true</PrivateZone>") {
            untouched.push((row.case, zone_id));
        } else {
            configured.push((row.case, zone_id));
        }
    }
    assert!(
        !configured.is_empty() && !untouched.is_empty(),
        "the table must span both configured and unconfigured zones"
    );

    for (case, zone_id) in &configured {
        let reply = update_features(&router, zone_id, true).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected 200 from UpdateHostedZoneFeatures, got {} with body {}",
            reply.status, reply.body
        );
        // Straight after the call the zone is still moving to the setting.
        let read = get_zone(&router, zone_id).await;
        let blocks = features_of(case, &read, 1);
        assert_features(
            case,
            blocks[0],
            Some(ACCELERATED_RECOVERY_ENABLING),
            Some(FEATURE_OFF),
        );
    }

    // A zone nobody configured still carries the container, with the statuses
    // the zone holds and nothing else in it — for these private zones, none.
    for (case, zone_id) in &untouched {
        let read = get_zone(&router, zone_id).await;
        let blocks = features_of(case, &read, 1);
        assert_features(case, blocks[0], None, None);
    }

    // And the listing projects both kinds: one container per zone, carrying the
    // status exactly for the zones that were configured.
    let listed = call(&router, "GET", "/2013-04-01/hostedzone", "").await;
    let blocks = features_of(
        "ListHostedZones",
        &listed,
        configured.len() + untouched.len(),
    );
    let with_status = blocks
        .iter()
        .filter(|block| element_text(block, "AcceleratedRecoveryStatus").is_some())
        .count();
    assert_eq!(
        with_status,
        configured.len(),
        "case {:?}: expected {} of the {} listed zones to report a status, got {with_status}; \
         body {}",
        "ListHostedZones",
        configured.len(),
        blocks.len(),
        listed.body
    );

    // The propagation span is the service's own, and it is never zero: a read
    // before it elapses saw the moving status above, and a read once the zone
    // has settled sees the settled status in the same container. Looked for
    // until it holds, so the assertion does not depend on how fast the host is.
    for (case, zone_id) in &configured {
        let (status, body) = settled_status(&router, zone_id).await;
        assert!(
            status
                .as_deref()
                .is_some_and(|value| value != ACCELERATED_RECOVERY_ENABLING),
            "case {case:?}: the settled zone must report a settled status in its Features \
             container, got {status:?}; body {body}"
        );
    }
}

/// Look at one zone's `AcceleratedRecoveryStatus` until it reports something
/// other than the moving status, or [`SETTLE_DEADLINE`] passes, and hand back
/// what the last read reported, with that read's body for the message.
async fn settled_status(router: &MultiServiceRouter, zone_id: &str) -> (Option<String>, String) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = get_zone(router, zone_id).await;
        let status = features_blocks(&read.body)
            .first()
            .and_then(|block| element_text(block, "AcceleratedRecoveryStatus"));
        let settled = status
            .as_deref()
            .is_some_and(|value| value != ACCELERATED_RECOVERY_ENABLING);
        if settled || Instant::now() >= deadline {
            return (status, read.body);
        }
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// The public-DNS fallback status one `Features` container reports, or `None`
/// for a container that reports none — which is what a zone the feature does not
/// apply to answers with.
fn fallback_status(block: &str) -> Option<String> {
    element_text(block, PUBLIC_DNS_FALLBACK_STATUS)
}

#[tokio::test]
async fn every_hosted_zone_projection_reports_the_zones_public_dns_fallback_status() {
    // The other half of the container rule, and the one an empty `<Features/>`
    // breaks: a feature status a zone HOLDS is reported as a child of the
    // container by EVERY projection of that zone, with the value read off the
    // zone's own state — and a zone that holds none reports none. Public DNS
    // fallback is a feature of a zone answered on the public internet, so every
    // public row of the table reports it and every private row reports nothing,
    // on the create, the single read, the comment update and both listings, and
    // all of them agree.
    let router = router();
    let rows = zone_rows();
    let mut ids = Vec::new();

    for (index, row) in rows.iter().enumerate() {
        let created = create_zone(
            &router,
            row.name,
            &format!("features-fallback-{index}"),
            row.extra,
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: expected 201 from CreateHostedZone, got {} with body {}",
            row.case, created.status, created.body
        );
        let reported = fallback_status(features_of(row.case, &created, 1)[0]);
        assert_eq!(
            reported.as_deref(),
            row.expected_fallback(),
            "case {:?}: expected {PUBLIC_DNS_FALLBACK_STATUS} {:?} on the create, got \
             {reported:?}; body {}",
            row.case,
            row.expected_fallback(),
            created.body
        );

        let zone_id = bare_zone_id(&created);
        for (reader, reply) in [
            ("GetHostedZone", get_zone(&router, &zone_id).await),
            (
                "UpdateHostedZoneComment",
                update_comment(&router, &zone_id, "a later comment").await,
            ),
        ] {
            let case = format!("{} read by {reader}", row.case);
            let seen = fallback_status(features_of(&case, &reply, 1)[0]);
            assert_eq!(
                seen, reported,
                "case {case:?}: every projection of one zone must report the same \
                 {PUBLIC_DNS_FALLBACK_STATUS}, got {seen:?} after {reported:?}; body {}",
                reply.body
            );
        }

        ids.push(zone_id);
    }

    // Both listings report it for every zone the feature applies to, so a client
    // enumerating the account's zones reads each public zone's fallback state off
    // the listing.
    let public_zones = rows.iter().filter(|row| !row.private).count();
    for (case, uri) in [
        ("ListHostedZones", "/2013-04-01/hostedzone"),
        ("ListHostedZonesByName", "/2013-04-01/hostedzonesbyname"),
    ] {
        let listed = call(&router, "GET", uri, "").await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected 200, got {} with body {}",
            listed.status, listed.body
        );
        let reporting = features_of(case, &listed, ids.len())
            .iter()
            .filter_map(|block| fallback_status(block))
            .collect::<Vec<String>>();
        assert_eq!(
            reporting,
            vec![FEATURE_OFF.to_string(); public_zones],
            "case {case:?}: expected {public_zones} listed zones to report \
             {FEATURE_OFF:?} for {PUBLIC_DNS_FALLBACK_STATUS}, got {reporting:?}; body {}",
            listed.body
        );
    }

    // A zone whose OTHER feature was configured reports both statuses in the
    // one container: reporting one feature never drops another.
    let public = ids
        .first()
        .expect("the table creates at least one zone")
        .clone();
    let updated = update_features(&router, &public, true).await;
    assert_eq!(
        updated.status, 200,
        "expected 200 from UpdateHostedZoneFeatures, got {} with body {}",
        updated.status, updated.body
    );
    let (accelerated, body) = settled_status(&router, &public).await;
    assert!(
        accelerated.is_some(),
        "the configured zone must report an AcceleratedRecoveryStatus; body {body}"
    );
    let block = features_blocks(&body)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("the read must carry a Features container; body {body}"))
        .to_string();
    let seen = fallback_status(&block);
    assert_eq!(
        seen.as_deref(),
        Some(FEATURE_OFF),
        "a configured public zone must still report {FEATURE_OFF:?} for \
         {PUBLIC_DNS_FALLBACK_STATUS}, got {seen:?}; body {body}"
    );
}
