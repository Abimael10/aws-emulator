// SPDX-License-Identifier: CC0-1.0

//! The Recycle Bin retention rules of an account decide what a DELETE of one of its EBS
//! snapshots does, through the ASSEMBLED emulator: the rule is created in the service that
//! owns rules, and the delete is served by Amazon EC2.
//!
//! RULE: a delete of an EBS resource is decided by the Recycle Bin retention rules the
//! account holds in the Region the delete was signed for — a rule in the one state that
//! retains anything, naming that resource's type, and (when it names resource tags) matched
//! by at least one of the tag key/value pairs the resource carries, while a rule naming no
//! resource tags retains every resource of its type except one carrying an exclusion tag —
//! so a covered delete RETAINS the resource in the account's Recycle Bin for that rule's
//! retention period (it stops being one of the account's live resources, the bin reports it
//! with the two instants that period spans, and a restore gives it back) and a delete no such
//! rule covers is permanent.
//!
//! Sources: `smithy_query(query="doc",shape="CreateRuleRequest",member="ResourceTags")` and
//! `…member="ExcludeResourceTags"` on the `rbin` model for which resources a rule retains;
//! `smithy_query(query="doc",shape="CreateRuleResponse",member="Status")` for the one state
//! that retains; `smithy_query(query="shape",shape="ResourceType,RetentionPeriod")` for the
//! resource types and the period's unit and published bounds.
//!
//! This suite lives beside the router because the rule spans two services: the retention
//! rules are the `rbin` service's own resources, the snapshot and its bin are EC2's, and
//! neither crate's tests can see the pairing stood up the way `core/router/src/lib.rs` mounts
//! it — one engine, every request routed by the credential scope it was signed with. The
//! TABLES that span each half (every way a snapshot id can fail to name a bin entry, the
//! bin's paging, the end of a retention period) live in the EC2 crate's own suites; the rows
//! here differ along what decides the DELETE: the kind of rule the account holds
//! (Region-level, tag-level, with an exclusion tag), its resource type, its Region, its
//! retention period, the tags the snapshot carries, and an account holding no rule at all.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode};

/// The Region every EC2 call below is signed for, and so the Region its snapshots and their
/// deletes belong to.
const REGION: &str = "us-west-2";

/// A Region that is NOT the one the deletes are signed for, for the row about a rule another
/// Region holds.
const OTHER_REGION: &str = "us-east-1";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// Two of the `ResourceType` values a retention rule may name, as the service that owns rules
/// spells them. Source: `smithy_query(query="shape",shape="ResourceType")` on the `rbin`
/// model, whose members are `EBS_SNAPSHOT`, `EC2_IMAGE` and `EBS_VOLUME`.
const EBS_SNAPSHOT: &str = "EBS_SNAPSHOT";
const EBS_VOLUME: &str = "EBS_VOLUME";

/// The one unit a retention period is measured in: "the unit of time in which the retention
/// period is measured. Currently, only `DAYS` is supported"
/// (`RetentionPeriod$RetentionPeriodUnit`).
const DAYS: &str = "DAYS";

/// The seconds one of those days holds, so the span the bin reports can be compared with the
/// period the rule carries.
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// The longest period a rule retaining this type may carry: "EBS snapshots and EBS-backed
/// AMIs: 1 - 365 days" (`RetentionPeriod$RetentionPeriodValue`).
const MAX_SNAPSHOT_RETENTION_DAYS: i64 = 365;

/// The one rule state that retains anything: "only retention rules that are in the
/// `available` state retain resources" (`CreateRuleResponse$Status`).
const RULE_AVAILABLE: &str = "available";

/// The state a volume settles into, and the one a snapshot's content is final in.
const VOLUME_AVAILABLE: &str = "available";
const SNAPSHOT_COMPLETED: &str = "completed";

/// EC2's error code for a snapshot id the account holds no snapshot for — which is what a
/// snapshot resident in the Recycle Bin is, for as long as it is resident.
const SNAPSHOT_NOT_FOUND_CODE: &str = "InvalidSnapshot.NotFound";

/// The code EC2 answers a restore whose snapshot id the Recycle Bin holds no entry for.
const SNAPSHOT_ID_MALFORMED_CODE: &str = "InvalidSnapshotID.Malformed";

/// How long a poll keeps looking for a condition. Generous: every span either service owes is
/// far inside it, so a loaded or a slow host reaches the condition too — the loops below wait
/// for the CONDITION and never for a fixed time.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// EC2's own sentence for a snapshot id the account holds no snapshot for.
fn snapshot_absent_message(snapshot_id: &str) -> String {
    format!("The snapshot '{snapshot_id}' does not exist.")
}

/// One retention rule an account of a row holds.
#[derive(Debug, Clone, Copy)]
struct RuleSpec {
    /// `CreateRuleRequest$ResourceType`.
    resource_type: &'static str,
    /// `RetentionPeriod$RetentionPeriodValue`, in [`DAYS`].
    retention_days: i64,
    /// `CreateRuleRequest$ResourceTags` — a rule naming none is a Region-level rule.
    resource_tags: &'static [(&'static str, &'static str)],
    /// `CreateRuleRequest$ExcludeResourceTags`.
    exclude_resource_tags: &'static [(&'static str, &'static str)],
    /// The Region the rule is created in, which is the Region it retains in.
    region: &'static str,
}

/// One row: the rule the account holds (or none at all), the tags the deleted snapshot
/// carries, and whether that rule retains the delete.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    rule: Option<RuleSpec>,
    snapshot_tags: &'static [(&'static str, &'static str)],
    retained: bool,
}

/// The rows, differing along everything that decides the delete: which kind of rule the
/// account holds, its resource type, its Region, how long it retains for, and the tags the
/// snapshot carries.
const ROWS: &[Row] = &[
    Row {
        case: "a tag-level rule one of the snapshot's tags matches, retaining for one day",
        rule: Some(RuleSpec {
            resource_type: EBS_SNAPSHOT,
            retention_days: 1,
            resource_tags: &[("retain-with", "recycle-bin")],
            exclude_resource_tags: &[],
            region: REGION,
        }),
        snapshot_tags: &[("Name", "nightly"), ("retain-with", "recycle-bin")],
        retained: true,
    },
    Row {
        case: "a tag-level rule whose pair the snapshot's own tag of that key does not carry",
        rule: Some(RuleSpec {
            resource_type: EBS_SNAPSHOT,
            retention_days: 7,
            resource_tags: &[("retain-with", "recycle-bin")],
            exclude_resource_tags: &[],
            region: REGION,
        }),
        snapshot_tags: &[("retain-with", "something-else")],
        retained: false,
    },
    Row {
        case: "a Region-level rule retaining for the longest published period",
        rule: Some(RuleSpec {
            resource_type: EBS_SNAPSHOT,
            retention_days: MAX_SNAPSHOT_RETENTION_DAYS,
            resource_tags: &[],
            exclude_resource_tags: &[],
            region: REGION,
        }),
        snapshot_tags: &[],
        retained: true,
    },
    Row {
        case: "a Region-level rule whose exclusion tag the snapshot carries",
        rule: Some(RuleSpec {
            resource_type: EBS_SNAPSHOT,
            retention_days: 30,
            resource_tags: &[],
            exclude_resource_tags: &[("Temporary", "true")],
            region: REGION,
        }),
        snapshot_tags: &[("Temporary", "true")],
        retained: false,
    },
    Row {
        case: "a Region-level rule whose exclusion tag the snapshot does not carry",
        rule: Some(RuleSpec {
            resource_type: EBS_SNAPSHOT,
            retention_days: 30,
            resource_tags: &[],
            exclude_resource_tags: &[("Temporary", "true")],
            region: REGION,
        }),
        snapshot_tags: &[("Temporary", "false")],
        retained: true,
    },
    Row {
        case: "a rule for another resource type",
        rule: Some(RuleSpec {
            resource_type: EBS_VOLUME,
            retention_days: 7,
            resource_tags: &[],
            exclude_resource_tags: &[],
            region: REGION,
        }),
        snapshot_tags: &[("retain-with", "recycle-bin")],
        retained: false,
    },
    Row {
        case: "a rule another Region holds",
        rule: Some(RuleSpec {
            resource_type: EBS_SNAPSHOT,
            retention_days: 7,
            resource_tags: &[],
            exclude_resource_tags: &[],
            region: OTHER_REGION,
        }),
        snapshot_tags: &[],
        retained: false,
    },
    Row {
        case: "an account holding no retention rule at all",
        rule: None,
        snapshot_tags: &[("retain-with", "recycle-bin")],
        retained: false,
    },
];

/// Amazon EC2 and Recycle Bin over ONE shared engine, as `core/router/src/lib.rs` mounts
/// them, which is what makes the rules this account holds the rules its deletes are decided
/// by.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Nothing here signs: the `Authorization` header carries only the credential scope, which
    // is what picks the service and the Region for a request.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_rbin_amazonrecyclebin::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and `region`.
fn authorization(signing_name: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}\
         /aws4_request, SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One `(name, value)` form parameter of an ec2Query request.
fn p(name: &str, value: impl Into<String>) -> (String, String) {
    (name.to_string(), value.into())
}

/// One EC2 action on its ec2Query wire, answered with `(status, the XML body)`.
async fn ec2(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2", REGION))
        .body(Bytes::from(body))
        .expect("a well-formed EC2 request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Recycle Bin call on its restJson1 wire, signed for `region` — the Region the rule it
/// addresses exists in.
async fn rbin(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = match &body {
        Some(value) => Bytes::from(serde_json::to_vec(value).expect("a serializable body")),
        None => Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://rbin.{region}.amazonaws.com{path}"))
        .header("content-type", "application/json")
        .header("authorization", authorization("rbin", region))
        .body(payload)
        .expect("a well-formed Recycle Bin request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The first `name` element's text of an ec2Query answer, or the empty string when the answer
/// carries it empty or not at all.
///
/// Read through the shared reader that DECODES the entities the serializer escapes, rather
/// than through its raw sibling: an error message naming a resource id carries an escaped
/// apostrophe (`&apos;`), so the raw text of that element is not the sentence the service
/// worded.
fn member(body: &str, name: &str) -> String {
    elements(body, name).into_iter().next().unwrap_or_default()
}

/// Look again until `probe` answers `true`, or [`POLL_DEADLINE`] has passed: the way this
/// suite waits for a condition instead of for a span.
async fn poll_until<P, F>(case: &str, waiting_for: &str, mut probe: P)
where
    P: FnMut() -> F,
    F: Future<Output = bool>,
{
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        if probe().await {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: waited for {waiting_for} and it never happened"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The resource tags of a rule, as `CreateRule` takes them.
fn resource_tags(tags: &[(&str, &str)]) -> Value {
    Value::Array(
        tags.iter()
            .map(|(key, value)| json!({ "ResourceTagKey": key, "ResourceTagValue": value }))
            .collect(),
    )
}

/// Create the row's rule through the service that OWNS retention rules, and wait until it
/// reports the one state that retains anything.
async fn create_available_rule(router: &MultiServiceRouter, case: &str, rule: &RuleSpec) {
    let mut body = json!({
        "ResourceType": rule.resource_type,
        "RetentionPeriod": {
            "RetentionPeriodValue": rule.retention_days,
            "RetentionPeriodUnit": DAYS,
        },
    });
    if !rule.resource_tags.is_empty() {
        body["ResourceTags"] = resource_tags(rule.resource_tags);
    }
    if !rule.exclude_resource_tags.is_empty() {
        body["ExcludeResourceTags"] = resource_tags(rule.exclude_resource_tags);
    }
    let (status, answer) = rbin(router, rule.region, "POST", "/rules", Some(body)).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: expected the retention rule to be created, got {status} with {answer}"
    );
    let identifier = answer
        .get("Identifier")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: CreateRule reported no Identifier: {answer}"))
        .to_string();

    // A freshly created rule starts out `pending`, so the suite waits for the state the rule
    // reports rather than for a span.
    let region = rule.region;
    poll_until(case, "the retention rule to become available", || {
        let path = format!("/rules/{identifier}");
        async move {
            let (status, answer) = rbin(router, region, "GET", &path, None).await;
            status == 200
                && answer.get("Status").and_then(Value::as_str) == Some(RULE_AVAILABLE)
        }
    })
    .await;
}

/// The first Availability Zone EC2 reports for the Region the deletes are signed for.
async fn first_zone(router: &MultiServiceRouter, case: &str) -> String {
    let (status, body) = ec2(router, "DescribeAvailabilityZones", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAvailabilityZones must be served, got {status} with {body}"
    );
    let zone = member(&body, "zoneName");
    assert!(
        !zone.is_empty(),
        "case {case:?}: expected EC2 to report an Availability Zone, got {body}"
    );
    zone
}

/// One volume of the account, waited into the state a snapshot can be taken of it in.
async fn create_available_volume(router: &MultiServiceRouter, case: &str, zone: &str) -> String {
    let (status, body) = ec2(
        router,
        "CreateVolume",
        &[
            p("AvailabilityZone", zone),
            p("Size", "1"),
            p("VolumeType", "gp3"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateVolume must be served, got {status} with {body}"
    );
    let volume_id = member(&body, "volumeId");
    assert!(
        volume_id.starts_with("vol-"),
        "case {case:?}: expected the id EC2 mints for a volume, got {volume_id:?} in {body}"
    );

    poll_until(case, "the volume to become available", || {
        let volume_id = volume_id.clone();
        async move {
            let (status, body) =
                ec2(router, "DescribeVolumes", &[p("VolumeId.1", volume_id)]).await;
            status == 200 && member(&body, "status") == VOLUME_AVAILABLE
        }
    })
    .await;
    volume_id
}

/// One snapshot of `volume_id` carrying `tags`, waited into the state the workflow deletes it
/// in.
async fn create_completed_snapshot(
    router: &MultiServiceRouter,
    case: &str,
    volume_id: &str,
    tags: &[(&str, &str)],
) -> String {
    let mut params = vec![p("VolumeId", volume_id)];
    if !tags.is_empty() {
        params.push(p("TagSpecification.1.ResourceType", "snapshot"));
        for (index, (key, value)) in tags.iter().enumerate() {
            let at = index + 1;
            params.push(p(&format!("TagSpecification.1.Tag.{at}.Key"), *key));
            params.push(p(&format!("TagSpecification.1.Tag.{at}.Value"), *value));
        }
    }
    let (status, body) = ec2(router, "CreateSnapshot", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSnapshot must be served, got {status} with {body}"
    );
    let snapshot_id = member(&body, "snapshotId");
    assert!(
        snapshot_id.starts_with("snap-"),
        "case {case:?}: expected the id EC2 mints for a snapshot, got {snapshot_id:?} in {body}"
    );

    await_completed(router, case, &snapshot_id).await;
    snapshot_id
}

/// Wait until `DescribeSnapshots` reports `snapshot_id` with its content final.
async fn await_completed(router: &MultiServiceRouter, case: &str, snapshot_id: &str) {
    poll_until(case, "the snapshot to be completed", || {
        let snapshot_id = snapshot_id.to_string();
        async move {
            let (status, body) = describe(router, &snapshot_id).await;
            status == 200 && member(&body, "status") == SNAPSHOT_COMPLETED
        }
    })
    .await;
}

/// `DescribeSnapshots` narrowed to one snapshot id.
async fn describe(router: &MultiServiceRouter, snapshot_id: &str) -> (u16, String) {
    ec2(router, "DescribeSnapshots", &[p("SnapshotId.1", snapshot_id)]).await
}

/// One instant the Recycle Bin reports, as the seconds since the epoch it names.
fn reported_instant(case: &str, member_name: &str, body: &str) -> i64 {
    let text = member(body, member_name);
    assert!(
        !text.is_empty(),
        "case {case:?}: expected the bin to report {member_name}, got {body}"
    );
    aws_smithy_types::DateTime::from_str(&text, aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {member_name} {text:?} is not a timestamp ({error})")
        })
        .secs()
}

/// RULE: the retention rules the account holds decide the delete — a covered snapshot is
/// retained in the Recycle Bin for that rule's period and can be restored, one no rule covers
/// is permanently deleted — driven end to end through the assembled emulator.
#[tokio::test]
async fn a_retention_rule_decides_whether_a_deleted_snapshot_is_retained_or_gone() {
    for row in ROWS {
        let case = row.case;
        // One emulator per row, so the rules of one row decide only that row's delete.
        let router = emulator();
        if let Some(rule) = &row.rule {
            create_available_rule(&router, case, rule).await;
        }

        let zone = first_zone(&router, case).await;
        let volume_id = create_available_volume(&router, case, &zone).await;
        let snapshot_id =
            create_completed_snapshot(&router, case, &volume_id, row.snapshot_tags).await;

        // The delete itself is served either way: whether the snapshot is retained is not
        // something the caller is refused for.
        let (status, body) = ec2(&router, "DeleteSnapshot", &[p("SnapshotId", &snapshot_id)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 deleting {snapshot_id}, got {status} with {body}"
        );
        assert!(
            member(&body, "Code").is_empty(),
            "case {case:?}: expected the delete to carry no error, got {body}"
        );

        // And either way the snapshot has left the account's snapshots: a retained snapshot
        // has been deleted, and the bin is where it waits.
        let (status, body) = describe(&router, &snapshot_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a deleted snapshot is not one of the account's, got {status} with \
             {body}"
        );
        assert_eq!(
            member(&body, "Code"),
            SNAPSHOT_NOT_FOUND_CODE,
            "case {case:?}: expected {SNAPSHOT_NOT_FOUND_CODE}, got {body}"
        );
        assert_eq!(
            member(&body, "Message"),
            snapshot_absent_message(&snapshot_id),
            "case {case:?}: expected EC2's own sentence for an absent snapshot, got {body}"
        );

        let Some(rule) = row.rule.filter(|_| row.retained) else {
            // No rule covers the delete, so it was permanent: the bin holds nothing, and the
            // restore has nothing to give back.
            let (status, listed) = ec2(&router, "ListSnapshotsInRecycleBin", &[]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: ListSnapshotsInRecycleBin must be served, got {status} with \
                 {listed}"
            );
            assert!(
                elements(&listed, "snapshotId").is_empty(),
                "case {case:?}: expected an empty Recycle Bin after a permanent delete, got \
                 {listed}"
            );
            let (status, body) = ec2(
                &router,
                "RestoreSnapshotFromRecycleBin",
                &[p("SnapshotId", &snapshot_id)],
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: a permanently deleted snapshot must be unrestorable, got \
                 {status} with {body}"
            );
            assert_eq!(
                member(&body, "Code"),
                SNAPSHOT_ID_MALFORMED_CODE,
                "case {case:?}: expected {SNAPSHOT_ID_MALFORMED_CODE}, got {body}"
            );
            continue;
        };

        // The rule covers the delete, so the snapshot is RETAINED: the bin reports exactly
        // it, with the volume it was taken of and the two instants its retention period
        // spans.
        let (status, listed) = ec2(
            &router,
            "ListSnapshotsInRecycleBin",
            &[p("SnapshotId.1", &snapshot_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListSnapshotsInRecycleBin must be served, got {status} with {listed}"
        );
        assert_eq!(
            elements(&listed, "snapshotId"),
            vec![snapshot_id.clone()],
            "case {case:?}: expected the bin to report exactly {snapshot_id}, got {listed}"
        );
        assert_eq!(
            member(&listed, "volumeId"),
            volume_id,
            "case {case:?}: expected the bin to report the volume the snapshot was taken of, \
             got {listed}"
        );
        // "The date and time when the snapshot is to be permanently deleted from the Recycle
        // Bin" is when it entered plus the period of the rule that retained it, so the span
        // between the two instants the bin reports IS that rule's retention period.
        let entered = reported_instant(case, "recycleBinEnterTime", &listed);
        let exits = reported_instant(case, "recycleBinExitTime", &listed);
        assert_eq!(
            exits - entered,
            rule.retention_days * SECONDS_PER_DAY,
            "case {case:?}: expected the bin to keep the snapshot for the rule's {} day(s), \
             got {entered} to {exits}",
            rule.retention_days
        );

        // And it is restorable while it is resident: the restore gives the snapshot back to
        // the account, where the clock takes it to a final state again.
        let (status, body) = ec2(
            &router,
            "RestoreSnapshotFromRecycleBin",
            &[p("SnapshotId", &snapshot_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 restoring the retained {snapshot_id}, got {status} \
             with {body}"
        );
        assert_eq!(
            member(&body, "snapshotId"),
            snapshot_id,
            "case {case:?}: expected the restore to answer the snapshot it gave back, got {body}"
        );
        await_completed(&router, case, &snapshot_id).await;

        // The bin gave it up, so it reports nothing any more — the snapshot is in exactly one
        // of the two places.
        let (status, listed) = ec2(&router, "ListSnapshotsInRecycleBin", &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListSnapshotsInRecycleBin must be served, got {status} with {listed}"
        );
        assert!(
            elements(&listed, "snapshotId").is_empty(),
            "case {case:?}: expected the bin to be empty after the restore, got {listed}"
        );
        let (status, body) = describe(&router, &snapshot_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the restored snapshot to be one of the account's again, \
             got {status} with {body}"
        );
        assert_eq!(
            member(&body, "volumeId"),
            volume_id,
            "case {case:?}: expected the restored snapshot to still report its source volume, \
             got {body}"
        );
    }
}
