// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of ONE rule: what a response that projects a
//! `HostedZone` serialises for the optional `Features` member.
//!
//! The rule under test: `HostedZone$Features` carries the zone's features
//! configuration, so the CONTAINER is serialised exactly where the zone HAS one — by every
//! response that projects such a zone, whichever operation projected it, and by none for a
//! zone no feature of the container is configured on. Which statuses it carries is the
//! zone's state: the accelerated-recovery status of a zone an accepted
//! `UpdateHostedZoneFeatures` has configured (the one operation this API version
//! configures a feature with), and no public-DNS fallback status at all, because no
//! operation this API version models turns that feature on — a feature nothing configured
//! has no status to report.
//!
//! One zone read by one operation is ONE row of that class, so the tables below
//! span it: zones differing in label count, in public/private, in whether they
//! carry a comment and in whether their features were ever configured, each
//! asserted on EVERY operation that projects a `HostedZone` — the create, the
//! single read, the comment update and both listings — because the rule is
//! about the projection of the shape, not about one operation.
//!
//! On the account-wide listing the statuses are asserted zone by zone and page
//! by page: a listing projects many zones at once, so the last tests pair each
//! `HostedZone` with the container serialised INSIDE it, over page sizes, over
//! every SELECTION the listing's own filters narrow it to, and over zones whose
//! feature state differs, where a zone's held status reported as an empty
//! container, filled from another zone's state, or reported for a zone that has
//! none, would otherwise pass unnoticed.
//!
//! Every test drives the service through the real router, using the SAME
//! harness the sibling hosted-zone suites use, so what is asserted is the
//! HTTP/restXml behaviour a Route 53 client sees.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_route53_awsdnsv20130401::state::{
    ACCELERATED_RECOVERY_DISABLED, ACCELERATED_RECOVERY_DISABLING, ACCELERATED_RECOVERY_ENABLED,
    ACCELERATED_RECOVERY_ENABLING,
};

use crate::hosted_zone_records::{call, create_zone_body, escape_xml, Reply};
use crate::resource_tags::element_text;
// The reusable delegation set one of the listing's selections narrows to is seeded
// and read through the sibling suite's harness rather than a second copy of its
// request bodies.
use crate::reusable_delegation_sets::{bare_set_id, create_set, delegation_set_element, set_of};

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
/// configures one, plus the public-DNS fallback status every zone has.
const FEATURES_MEMBERS: [&str; 3] = [
    "AcceleratedRecoveryStatus",
    "FailureReasons",
    PUBLIC_DNS_FALLBACK_STATUS,
];

/// The `HostedZoneFeatures` member reporting whether a zone falls back to
/// public DNS. No zone of this API version has a status for it: nothing it models turns
/// that feature on, so no projection reports the member.
const PUBLIC_DNS_FALLBACK_STATUS: &str = "PublicDNSFallbackStatus";

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

/// The contents of every `<HostedZone>` element in a body, in document order.
///
/// Read out of the RAW body for the same reason [`features_blocks`] is, but per
/// ZONE rather than per container: the statuses a listing reports for a zone are
/// the children of THAT zone's container, so pairing a zone with its own
/// statuses means reading them inside the element the zone is projected as.
/// `<HostedZone>` and the `<HostedZones>` wrapper around it are told apart by
/// the closing angle bracket.
fn hosted_zone_blocks(body: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find("<HostedZone>") {
        let inner = &rest[at + "<HostedZone>".len()..];
        let end = inner
            .find("</HostedZone>")
            .unwrap_or_else(|| panic!("a HostedZone element must be closed; body {body}"));
        blocks.push(&inner[..end]);
        rest = &inner[end..];
    }
    blocks
}

/// The `(Name, contents of Features)` pair of every hosted zone a body
/// projects, in document order — `None` where the zone carries no container.
fn zone_features(case: &str, body: &str) -> Vec<(String, Option<String>)> {
    hosted_zone_blocks(body)
        .into_iter()
        .map(|zone| {
            let name = element_text(zone, "Name").unwrap_or_else(|| {
                panic!("case {case:?}: every projected hosted zone carries its Name; zone {zone:?}")
            });
            (name, element_text(zone, "Features"))
        })
        .collect()
}

/// The form Route 53 reports a hosted zone's name in: the fully qualified name,
/// so a name given without the trailing dot is reported with one.
fn canonical_name(requested: &str) -> String {
    if requested.ends_with('.') {
        requested.to_string()
    } else {
        format!("{requested}.")
    }
}

/// What one zone of the listing holds, and therefore what its own `Features`
/// container must report: the name it is reported under, its public-DNS
/// fallback status (`None` for a zone the feature does not apply to) and its
/// accelerated-recovery status (`None` while nothing has configured it).
struct Held {
    name: String,
    fallback: Option<&'static str>,
    accelerated: Option<&'static str>,
}

/// Assert a reply projects NO hosted zone at all, and so opens no `Features`
/// container: an operation whose output carries no `HostedZone` carries none of
/// the shape's members either.
fn assert_no_features(case: &str, reply: &Reply) {
    let blocks = features_blocks(&reply.body);
    assert!(
        blocks.is_empty(),
        "case {case:?}: a reply that projects no hosted zone must open no Features container, \
         got {blocks:?}; body {}",
        reply.body
    );
}

/// Assert a reply projecting ONE hosted zone reports that zone's statuses: the container is
/// serialised exactly where the zone has one, carrying `accelerated` for the
/// accelerated-recovery status and `fallback` for the public-DNS fallback status and nothing
/// else — so a zone with neither reports NO container.
fn assert_one_zone_features(
    case: &str,
    reply: &Reply,
    accelerated: Option<&str>,
    fallback: Option<&str>,
) {
    if accelerated.is_none() && fallback.is_none() {
        assert_no_features(case, reply);
        return;
    }
    let blocks = features_of(case, reply, 1);
    assert!(
        !element_names(blocks[0]).is_empty(),
        "case {case:?}: the zone holds a feature status, so its Features container must \
         carry it as a child element, got the empty container {:?}; body {}",
        blocks[0],
        reply.body
    );
    assert_features(case, blocks[0], accelerated, fallback);
}

/// Assert one zone's `Features` container is a STRUCTURE carrying THAT zone's
/// statuses: the ones it holds as children, each a modelled member — and, the
/// divergence this file exists for, no container at all for a zone that holds no status.
fn assert_zone_features(case: &str, held: &Held, block: Option<&str>) {
    if held.fallback.is_none() && held.accelerated.is_none() {
        assert_eq!(
            block, None,
            "case {case:?}: zone {:?} holds no feature status, so it carries no Features \
             container, got {block:?}",
            held.name
        );
        return;
    }
    let block = block.unwrap_or_else(|| {
        panic!(
            "case {case:?}: zone {:?} holds a feature status, so it carries its Features \
             container",
            held.name
        )
    });
    assert!(
        !element_names(block).is_empty(),
        "case {case:?}: zone {:?} holds a feature status, so its Features container must \
         carry it as a child element, got the empty container {block:?}",
        held.name
    );
    assert_features(case, block, held.accelerated, held.fallback);
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
    /// The public-DNS fallback status this zone must report: NONE, on a public zone as on a
    /// private one. A status reports a feature a zone is configured for, and no operation
    /// this API version models turns that feature on for any zone.
    fn expected_fallback(&self) -> Option<&'static str> {
        None
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
async fn no_projection_of_an_unconfigured_zone_opens_a_feature_container() {
    // The rule: the container is there exactly where the zone HAS a features
    // configuration — so a zone no features update has configured carries none of it, on
    // any projection, public or private. The rows span the zones; each one is
    // asserted on the create, the single read and the comment update, and the
    // listings are asserted over all of them at once.
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
        // A zone is created with no accelerated-recovery status whatever its
        // config block says, and with the fallback status the feature gives it.
        assert_one_zone_features(row.case, &created, None, row.expected_fallback());

        let zone_id = bare_zone_id(&created);
        let read = get_zone(&router, &zone_id).await;
        assert_eq!(
            read.status, 200,
            "case {:?}: expected 200 from GetHostedZone, got {} with body {}",
            row.case, read.status, read.body
        );
        assert_one_zone_features(row.case, &read, None, row.expected_fallback());

        let updated = update_comment(&router, &zone_id, "a later comment").await;
        assert_eq!(
            updated.status, 200,
            "case {:?}: expected 200 from UpdateHostedZoneComment, got {} with body {}",
            row.case, updated.status, updated.body
        );
        assert_one_zone_features(row.case, &updated, None, row.expected_fallback());

        ids.push(zone_id);
    }

    // Both LIST paths report the same thing: a zone nothing configured opens no
    // container, so a listing of such zones opens none at all.
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
        let zones = zone_features(case, &listed.body);
        assert_eq!(
            zones.len(),
            ids.len(),
            "case {case:?}: expected the {} created zones to be listed, got {}; body {}",
            ids.len(),
            zones.len(),
            listed.body
        );
        for (name, block) in &zones {
            assert_eq!(
                block.as_deref(),
                None,
                "case {case:?}: zone {name:?} has no feature configured, so it opens no \
                 Features container, got {block:?}; body {}",
                listed.body
            );
        }
    }
}

#[tokio::test]
async fn the_features_container_reports_the_configured_feature_on_every_projection() {
    // The rule's other half: the container is the zone's own state, so once an
    // accepted update has configured a zone, EVERY projection of that zone
    // reports the status in it — while the zones beside it, which nothing
    // configured, report the posture they hold and no status for that feature.
    // The rows are the public zones of the table above, which are the zones
    // accelerated recovery applies to.
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
        assert_features(case, blocks[0], Some(ACCELERATED_RECOVERY_ENABLING), None);
    }

    // A zone nobody configured has no features configuration at all, so its read opens no
    // container.
    for (case, zone_id) in &untouched {
        let read = get_zone(&router, zone_id).await;
        assert_one_zone_features(case, &read, None, None);
    }

    // And the listing projects both kinds: a container exactly for the zones that were
    // configured, carrying that zone's status.
    let listed = call(&router, "GET", "/2013-04-01/hostedzone", "").await;
    let blocks = features_of("ListHostedZones", &listed, configured.len());
    let with_status = blocks
        .iter()
        .filter(|block| element_text(block, "AcceleratedRecoveryStatus").is_some())
        .count();
    assert_eq!(
        with_status,
        configured.len(),
        "case {:?}: expected {} container(s) reporting a status, got {with_status}; \
         body {}",
        "ListHostedZones",
        configured.len(),
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

/// Look at one zone until its `Features` container reports `want` for
/// `AcceleratedRecoveryStatus`, or [`SETTLE_DEADLINE`] passes, and hand back
/// what the last read reported with that read's body for the message.
///
/// Waits for the CONDITION rather than for a span, so a loaded or a fast host
/// only changes how often it looks.
async fn poll_until_status(
    router: &MultiServiceRouter,
    zone_id: &str,
    want: &str,
) -> (Option<String>, String) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = get_zone(router, zone_id).await;
        let status = features_blocks(&read.body)
            .first()
            .and_then(|block| element_text(block, "AcceleratedRecoveryStatus"));
        if status.as_deref() == Some(want) || Instant::now() >= deadline {
            return (status, read.body);
        }
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// One state a zone's features configuration can be in: the settings an accepted
/// update asks for to reach it, whether the last one is waited out, and what a
/// projection then has to report.
struct FeatureState {
    case: &'static str,
    /// The settings to ask `UpdateHostedZoneFeatures` for, in order.
    updates: &'static [bool],
    /// The `AcceleratedRecoveryStatus` to wait for before asserting, for a state
    /// the zone only reaches once the propagation span is up.
    settled: Option<&'static str>,
    /// The `AcceleratedRecoveryStatus` the `Features` container must report, and
    /// `None` for a state where nothing has configured the feature — where the zone has no
    /// features configuration and so opens no container at all.
    expected: Option<&'static str>,
}

/// Every state of the one feature this API version configures, from the state a
/// zone is created in to both settled settings and both moving ones.
fn feature_states() -> [FeatureState; 5] {
    [
        FeatureState {
            case: "no features configuration at all",
            updates: &[],
            settled: None,
            expected: None,
        },
        FeatureState {
            case: "an accepted enable, still moving",
            updates: &[true],
            settled: None,
            expected: Some(ACCELERATED_RECOVERY_ENABLING),
        },
        FeatureState {
            case: "an accepted enable, settled",
            updates: &[true],
            settled: Some(ACCELERATED_RECOVERY_ENABLED),
            expected: Some(ACCELERATED_RECOVERY_ENABLED),
        },
        FeatureState {
            case: "an accepted disable, still moving",
            updates: &[true, false],
            settled: None,
            expected: Some(ACCELERATED_RECOVERY_DISABLING),
        },
        FeatureState {
            case: "an accepted disable, settled",
            updates: &[true, false],
            settled: Some(ACCELERATED_RECOVERY_DISABLED),
            expected: Some(ACCELERATED_RECOVERY_DISABLED),
        },
    ]
}

#[tokio::test]
async fn every_state_of_the_feature_is_reported_in_the_zones_own_container() {
    // The rule along the axis it generalises over: a zone with a features
    // configuration serialises its `Features` container on every projection, and the
    // accelerated-recovery status inside it is the state the zone is in — the moving status
    // while an accepted `UpdateHostedZoneFeatures` is still propagating, the settled one
    // after that — while a zone nothing configured opens no container. The rows cross every
    // zone of the table (label count, public/private, commented or not) with every state
    // the configuration can be in: none at all, both settings while they are
    // still moving and both once they have settled. A private zone cannot have
    // the feature configured, so for it every row is the no-configuration row
    // and the update is refused.
    let router = router();
    for (row_index, row) in zone_rows().iter().enumerate() {
        for (state_index, state) in feature_states().iter().enumerate() {
            let case = format!("{} with {}", row.case, state.case);
            // One zone per cell, named with its own label, so no cell can read
            // the configuration another one left.
            let created = create_zone(
                &router,
                &format!("s{row_index}x{state_index}.{}", row.name.trim_end_matches('.')),
                &format!("features-state-{row_index}-{state_index}"),
                row.extra,
            )
            .await;
            assert_eq!(
                created.status, 201,
                "case {case:?}: expected 201 from CreateHostedZone, got {} with body {}",
                created.status, created.body
            );
            let zone_id = bare_zone_id(&created);

            for enable in state.updates {
                let reply = update_features(&router, &zone_id, *enable).await;
                let expected_status = if row.private { 400 } else { 200 };
                assert_eq!(
                    reply.status, expected_status,
                    "case {case:?}: expected {expected_status} from \
                     UpdateHostedZoneFeatures(enable={enable}), got {} with body {}",
                    reply.status, reply.body
                );
                // The update answers with no hosted zone at all, so it carries
                // none of the shape's members either way.
                assert_no_features(&case, &reply);
            }

            // A private zone's update was refused, so it is still a zone with no
            // features configuration whatever the row asked for.
            let expected = if row.private { None } else { state.expected };
            if let (false, Some(want)) = (row.private, state.settled) {
                let (status, body) = poll_until_status(&router, &zone_id, want).await;
                assert_eq!(
                    status.as_deref(),
                    Some(want),
                    "case {case:?}: the zone must reach {want}, got {status:?}; body {body}"
                );
            }

            // Every single-zone projection of that zone agrees, because they
            // project the same zone.
            for (reader, reply) in [
                ("GetHostedZone", get_zone(&router, &zone_id).await),
                (
                    "UpdateHostedZoneComment",
                    update_comment(&router, &zone_id, "a later comment").await,
                ),
            ] {
                let case = format!("{case} read by {reader}");
                assert_one_zone_features(&case, &reply, expected, row.expected_fallback());
            }
        }
    }
}

#[tokio::test]
async fn no_hosted_zone_projection_reports_a_public_dns_fallback_status() {
    // The other half of the rule: a status reports a feature the zone is CONFIGURED for, so
    // a feature nothing can turn on has no status anywhere. This API version models no
    // operation that turns public DNS fallback on, so no projection of any zone — public or
    // private, configured for the other feature or not, read singly or listed — reports the
    // member; what a configured zone does report is the feature it was configured for.
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

        let zone_id = bare_zone_id(&created);
        // Configuring the one feature this API version can configure is what
        // gives the zone a features configuration to report; it is refused for a
        // private zone, which therefore keeps reporting no container.
        let configured = update_features(&router, &zone_id, true).await;
        assert_eq!(
            configured.status,
            if row.private { 400 } else { 200 },
            "case {:?}: expected {} from UpdateHostedZoneFeatures, got {} with body {}",
            row.case,
            if row.private { 400 } else { 200 },
            configured.status,
            configured.body
        );

        for (reader, reply) in [
            ("GetHostedZone", get_zone(&router, &zone_id).await),
            (
                "UpdateHostedZoneComment",
                update_comment(&router, &zone_id, "a later comment").await,
            ),
        ] {
            let case = format!("{} read by {reader}", row.case);
            for block in features_blocks(&reply.body) {
                let seen = fallback_status(block);
                assert_eq!(
                    seen, None,
                    "case {case:?}: no container reports {PUBLIC_DNS_FALLBACK_STATUS}, got \
                     {seen:?}; body {}",
                    reply.body
                );
            }
            // The public zone was configured for the other feature, so it reports that one.
            if !row.private {
                let blocks = features_of(&case, &reply, 1);
                assert!(
                    element_text(blocks[0], "AcceleratedRecoveryStatus").is_some(),
                    "case {case:?}: the configured zone reports its accelerated-recovery \
                     status; container {:?}",
                    blocks[0]
                );
            } else {
                assert_no_features(&case, &reply);
            }
        }

        ids.push(zone_id);
    }

    // Neither listing reports it either, for any of the zones it projects.
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
        let reporting = features_of(case, &listed, public_zones)
            .iter()
            .filter_map(|block| fallback_status(block))
            .collect::<Vec<String>>();
        assert!(
            reporting.is_empty(),
            "case {case:?}: expected no listed zone to report \
             {PUBLIC_DNS_FALLBACK_STATUS}, got {reporting:?}; body {}",
            listed.body
        );
    }

    // A zone whose OTHER feature was configured reports that feature's status and nothing
    // beside it: reporting one feature never invents another.
    let public = ids
        .first()
        .expect("the table creates at least one zone")
        .clone();
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
        seen, None,
        "a configured public zone reports no {PUBLIC_DNS_FALLBACK_STATUS}, got {seen:?}; \
         body {body}"
    );
}

#[tokio::test]
async fn every_page_of_the_listing_pairs_each_zone_with_its_own_feature_statuses() {
    // The same rule where a client meets it on the account-wide listing, zone by
    // zone and page by page: each projected zone's Features container reports
    // THAT zone's feature statuses as child elements, so a zone holding a status
    // can never be reported as the empty container and no zone can be handed
    // another zone's statuses. The rows span the zones of the table (label
    // count, public or private, commented or not), the page sizes the listing is
    // walked with (unpaged, and pages shorter than, equal to and longer than the
    // remainder) and the feature states a listed zone can be in: a configured
    // accelerated recovery, a fallback status the feature applies to, and a zone
    // holding neither.
    let router = router();
    let rows = zone_rows();
    // What each zone holds — the expectation every page is checked against.
    let mut expected: Vec<Held> = Vec::new();
    // The zones whose features were configured, so the test can wait for the
    // setting to have settled before the listing is asserted on.
    let mut configured_ids: Vec<(&str, String)> = Vec::new();

    for (index, row) in rows.iter().enumerate() {
        let created = create_zone(
            &router,
            row.name,
            &format!("features-listed-{index}"),
            row.extra,
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: expected 201 from CreateHostedZone, got {} with body {}",
            row.case, created.status, created.body
        );
        // Both recovery features of the container serve a zone answered on the
        // public internet, so the public rows are the ones configured here and
        // the ones with a fallback status, while the private rows hold neither:
        // the table then spans the feature states a listed zone can be in.
        let public = !row.extra.contains("<PrivateZone>true</PrivateZone>");
        if public {
            let reply = update_features(&router, &bare_zone_id(&created), true).await;
            assert_eq!(
                reply.status, 200,
                "case {:?}: expected 200 from UpdateHostedZoneFeatures, got {} with body {}",
                row.case, reply.status, reply.body
            );
            configured_ids.push((row.case, bare_zone_id(&created)));
        }
        expected.push(Held {
            name: canonical_name(row.name),
            // No zone reports a fallback status: nothing this API version models turns
            // that feature on.
            fallback: None,
            accelerated: public.then_some(ACCELERATED_RECOVERY_ENABLED),
        });
    }

    // Looked for until each configured zone has settled, so what the listing
    // must report below is one fixed status rather than whatever the propagation
    // span happens to be showing on this host.
    for (case, zone_id) in &configured_ids {
        let (status, body) = settled_status(&router, zone_id).await;
        assert_eq!(
            status.as_deref(),
            Some(ACCELERATED_RECOVERY_ENABLED),
            "case {case:?}: the configured zone must reach {ACCELERATED_RECOVERY_ENABLED}, \
             got {status:?}; body {body}"
        );
    }

    for page_size in [None, Some(1), Some(2), Some(rows.len() + 1)] {
        let case = match page_size {
            Some(size) => format!("the listing walked with maxitems={size}"),
            None => "the unpaged listing".to_string(),
        };
        let mut walked: Vec<(String, Option<String>)> = Vec::new();
        let mut marker: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut query = String::new();
            if let Some(size) = page_size {
                query.push_str(&format!("?maxitems={size}"));
            }
            if let Some(resume) = &marker {
                query.push(if query.is_empty() { '?' } else { '&' });
                query.push_str(&format!("marker={resume}"));
            }
            let page = call(
                &router,
                "GET",
                &format!("/2013-04-01/hostedzone{query}"),
                "",
            )
            .await;
            assert_eq!(
                page.status, 200,
                "case {case:?}: expected 200, got {} with body {}",
                page.status, page.body
            );
            let on_page = zone_features(&case, &page.body);
            assert!(
                page_size.is_none_or(|size| on_page.len() <= size),
                "case {case:?}: a page held {} zone(s); body {}",
                on_page.len(),
                page.body
            );
            for (name, block) in &on_page {
                let held = expected
                    .iter()
                    .find(|held| &held.name == name)
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: the listing reported a zone {name:?} nothing \
                             created; body {}",
                            page.body
                        )
                    });
                assert_zone_features(&case, held, block.as_deref());
            }
            walked.extend(on_page);
            pages += 1;
            assert!(
                pages <= rows.len() + 2,
                "case {case:?}: paging did not terminate after {pages} page(s)"
            );
            if page.first("IsTruncated") != Some("true") {
                break;
            }
            marker = Some(
                page.first("NextMarker")
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: a truncated page must carry a NextMarker; body {}",
                            page.body
                        )
                    })
                    .to_string(),
            );
        }

        // Every zone of the account is reported exactly once, each with its own
        // statuses: paging neither drops a zone nor the statuses it holds.
        let mut reported: Vec<&str> = walked.iter().map(|(name, _)| name.as_str()).collect();
        reported.sort_unstable();
        let mut wanted: Vec<&str> = expected.iter().map(|held| held.name.as_str()).collect();
        wanted.sort_unstable();
        assert_eq!(
            reported, wanted,
            "case {case:?}: paging must report every zone of the account exactly once"
        );
    }
}

// ---------------------------------------------------------------------------
// The statuses every SELECTION of the listing reports
// ---------------------------------------------------------------------------

/// The one value `HostedZoneType` declares, from
/// `smithy_query(query="shape", shape="com.amazonaws.route53#HostedZoneType")`:
/// the `hostedzonetype` filter narrows the listing to the account's PRIVATE
/// zones.
const PRIVATE_HOSTED_ZONE: &str = "PrivateHostedZone";

/// One zone the selection test seeds, and what the listing has to report for it:
/// the name it is listed under, the `config` block it was created with and
/// whether it is private — which decides which selections carry it.
struct SelectedZone {
    case: &'static str,
    name: &'static str,
    config: String,
    private: bool,
    /// Whether the zone was created in the reusable delegation set the test
    /// made, which is what the `delegationsetid` selection narrows to.
    in_set: bool,
}

impl SelectedZone {
    /// What this zone's own `Features` container must report wherever the
    /// listing projects it: NOTHING, because nothing has configured a feature on it — so
    /// the zone opens no container at all.
    fn held(&self) -> Held {
        Held {
            name: canonical_name(self.name),
            fallback: None,
            accelerated: None,
        }
    }
}

/// Assert one selection of the listing reports exactly `expected` (by zone name)
/// and pairs each reported zone with ITS OWN feature statuses.
async fn assert_selection_reports(
    router: &MultiServiceRouter,
    case: &str,
    query: &str,
    expected: &[&SelectedZone],
) {
    let listed = call(router, "GET", &format!("/2013-04-01/hostedzone{query}"), "").await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected 200 from ListHostedZones{query}, got {} with body {}",
        listed.status, listed.body
    );
    let reported = zone_features(case, &listed.body);
    let mut names: Vec<&str> = reported.iter().map(|(name, _)| name.as_str()).collect();
    names.sort_unstable();
    let wanted: Vec<String> = expected.iter().map(|zone| canonical_name(zone.name)).collect();
    let mut wanted_names: Vec<&str> = wanted.iter().map(String::as_str).collect();
    wanted_names.sort_unstable();
    assert_eq!(
        names, wanted_names,
        "case {case:?}: expected the selection to report {wanted_names:?}, got {names:?}; body {}",
        listed.body
    );
    for (name, block) in &reported {
        let zone = expected
            .iter()
            .find(|zone| &canonical_name(zone.name) == name)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the selection reported a zone {name:?} it was not asked \
                     for; body {}",
                    listed.body
                )
            });
        assert_zone_features(&format!("{case} / {}", zone.case), &zone.held(), block.as_deref());
    }
}

#[tokio::test]
async fn every_selection_of_the_zone_listing_pairs_each_zone_with_its_feature_statuses() {
    // The listing's rule holds for every SELECTION of it, not only for the
    // unnarrowed read: each row is projected from the zone it names, so whichever
    // zones a request selects, each of them carries its own statuses as children
    // of its `Features` container. The selections below are the ones the model
    // binds — the whole listing, the `hostedzonetype` filter, the
    // `delegationsetid` filter and a page resumed at a `marker` — over zones
    // differing in label count, in public/private, in whether they carry a
    // comment and in whether they were created in a reusable delegation set.
    let router = router();
    let created_set = create_set(&router, "features-selection-set", None).await;
    assert_eq!(
        created_set.status, 201,
        "seeding the delegation set: expected 201, got {} with body {}",
        created_set.status, created_set.body
    );
    let set_id = bare_set_id(&set_of(&created_set, "the delegation set").id).to_string();

    let zones = [
        SelectedZone {
            case: "a public zone created in the reusable delegation set",
            name: "selected-in-set.example.com",
            config: delegation_set_element(&set_id),
            private: false,
            in_set: true,
        },
        SelectedZone {
            case: "a three-label public zone with a comment, outside the set",
            name: "zone.sub.selected.example.net",
            config: "<HostedZoneConfig><Comment>selected</Comment></HostedZoneConfig>".to_string(),
            private: false,
            in_set: false,
        },
        SelectedZone {
            case: "a private zone with one VPC",
            name: "selected-private.example.com",
            config: "<HostedZoneConfig><PrivateZone>true</PrivateZone></HostedZoneConfig>\
                     <VPC><VPCRegion>us-east-1</VPCRegion><VPCId>vpc-1a2b3c4d</VPCId></VPC>"
                .to_string(),
            private: true,
            in_set: false,
        },
    ];
    for (index, zone) in zones.iter().enumerate() {
        let created = create_zone(
            &router,
            zone.name,
            &format!("features-selection-{index}"),
            &zone.config,
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {:?}: expected 201 from CreateHostedZone, got {} with body {}",
            zone.case, created.status, created.body
        );
    }

    let all: Vec<&SelectedZone> = zones.iter().collect();
    let private: Vec<&SelectedZone> = zones.iter().filter(|zone| zone.private).collect();
    let in_set: Vec<&SelectedZone> = zones.iter().filter(|zone| zone.in_set).collect();
    for (case, query, expected) in [
        ("the whole listing", String::new(), all.clone()),
        (
            "the private-zone selection",
            format!("?hostedzonetype={PRIVATE_HOSTED_ZONE}"),
            private,
        ),
        (
            "the delegation-set selection",
            format!("?delegationsetid={set_id}"),
            in_set,
        ),
    ] {
        assert_selection_reports(&router, case, &query, &expected).await;
    }

    // A page RESUMED at a marker is a selection too: the zone the first page
    // reports and the zones behind the marker each carry their own statuses, and
    // the two pages together are the whole listing. The expectation comes from
    // the UNNARROWED listing's own order, which the zones are stored in, rather
    // than from the page under test.
    let whole = call(&router, "GET", "/2013-04-01/hostedzone", "").await;
    assert_eq!(
        whole.status, 200,
        "case \"the listing order\": expected 200, got {} with body {}",
        whole.status, whole.body
    );
    let order: Vec<String> = zone_features("the listing order", &whole.body)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        order.len(),
        zones.len(),
        "case \"the listing order\": expected the {} seeded zones, got {order:?}; body {}",
        zones.len(),
        whole.body
    );
    let zone_named = |name: &str| -> &SelectedZone {
        all.iter()
            .copied()
            .find(|zone| canonical_name(zone.name) == name)
            .unwrap_or_else(|| panic!("the listing reported a zone {name:?} nothing created"))
    };
    let on_first = vec![zone_named(&order[0])];
    let resumed: Vec<&SelectedZone> = order[1..].iter().map(|name| zone_named(name)).collect();

    let first = call(&router, "GET", "/2013-04-01/hostedzone?maxitems=1", "").await;
    assert_eq!(
        first.status, 200,
        "case \"the first page\": expected 200, got {} with body {}",
        first.status, first.body
    );
    let marker = first
        .first("NextMarker")
        .unwrap_or_else(|| {
            panic!(
                "case \"the first page\": a listing of {} zones served one at a time must carry \
                 a NextMarker; body {}",
                zones.len(),
                first.body
            )
        })
        .to_string();
    assert_selection_reports(&router, "the first page", "?maxitems=1", &on_first).await;
    assert_selection_reports(
        &router,
        "the page resumed at the marker",
        &format!("?marker={marker}"),
        &resumed,
    )
    .await;
}
