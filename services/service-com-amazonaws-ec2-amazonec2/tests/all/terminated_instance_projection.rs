// SPDX-License-Identifier: CC0-1.0

//! What a caller-asked state change RECORDS on an instance, and what a TERMINATED instance
//! still holds.
//!
//! The rules under test, each asserted over a table of inputs rather than over one instance:
//!
//! 1. THE REASON NAMES THE INSTANT IN AWS'S CIVIL FORM. "The reason for the most recent
//!    state transition" (`Instance$StateTransitionReason`) is worded `User initiated
//!    (<date> <time> GMT)` for a change a caller asked for — the form the operation's own
//!    `reason` filter documents ("shows \"User Initiated [date]\" when you stop or terminate
//!    the instance") — a civil UTC calendar date and time of day, not the ISO 8601 instant
//!    the timestamp members of this surface carry. The INSTANT is per-run; only its form is
//!    asserted here.
//! 2. A TERMINATED INSTANCE HOLDS NOTHING. The resources a termination takes away are not
//!    reported as still held: its network interfaces are deleted with it, so it reports no
//!    `subnetId` and no `privateIpAddress` and the EMPTY `privateDnsName`; its EBS volumes
//!    are answered for by "indicates whether the volume is deleted on instance termination"
//!    (`EbsBlockDevice$DeleteOnTermination`), so a volume marked for it is GONE from the
//!    account's volume store and a volume marked otherwise is detached and `available`,
//!    which leaves the instance with no block device mapping at all; and its metadata
//!    options are not `applied`, an instance that no longer runs being one that is "not
//!    ready to process metadata traffic" (`InstanceMetadataOptionsResponse$State`).
//!
//! Every expectation is read back out of the emulator through its own reads —
//! `DescribeInstances` for the instance and `DescribeVolumes` for the volumes it held — so
//! no row of a table is itself the specification.

use ec2::state::{METADATA_OPTIONS_STATE_PENDING, VOLUME_STATE_AVAILABLE};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, indexed};
use super::wire::{child, child_text, document, set_items};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-west-2";

/// An AMI id of the right FORM that this account holds no image for: the launch is served
/// and the instance reports the facts of the image this crate publishes by default.
const UNHELD_AMI: &str = "ami-0123456789abcdef0";

/// The fixed text the reason of a caller-asked change opens with.
const USER_INITIATED: &str = "User initiated (";

/// The zone the date inside it is named in.
const GMT: &str = " GMT)";

fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One call that must be served, as its body.
async fn ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Launch one instance with `extra` beside the AMI and answer its id.
async fn launch(router: &MultiServiceRouter, case: &str, extra: &[(String, String)]) -> String {
    let mut params = vec![
        p("ImageId", UNHELD_AMI),
        p("MinCount", "1"),
        p("MaxCount", "1"),
    ];
    params.extend_from_slice(extra);
    let body = ok(router, case, "RunInstances", &params).await;
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: expected an instanceId in {body}"))
}

/// The one instance item `DescribeInstances` reports for `instance_id`, as the `(member,
/// text)` pairs of its DIRECT children — which is what tells a member reported EMPTY from a
/// member left out.
async fn described_members(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Vec<(String, String)> {
    let body = ok(
        router,
        case,
        "DescribeInstances",
        &indexed("InstanceId", &[instance_id]),
    )
    .await;
    let parsed = document(case, &body);
    let instance = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no instance in {body}"));
    instance
        .children()
        .filter(roxmltree::Node::is_element)
        .map(|member| {
            (
                member.tag_name().name().to_string(),
                member.text().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The value of `member` among `members` — `None` when the instance reported no such member.
fn pick(members: &[(String, String)], member: &str) -> Option<String> {
    members
        .iter()
        .find(|(name, _)| name == member)
        .map(|(_, value)| value.clone())
}

/// The state the one volume of a `DescribeVolumes` body reports — read off the VOLUME's own
/// node, because an attachment inside it carries a `status` of its own.
fn reported_volume_state(case: &str, body: &str) -> String {
    let parsed = document(case, body);
    let volume = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "volumeId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no volume in {body}"));
    child_text(volume, "status")
        .unwrap_or_else(|| panic!("case {case:?}: the volume reports no state in {body}"))
}

/// The state `DescribeInstances` reports for `instance_id`.
async fn state_of(router: &MultiServiceRouter, region: &str, instance_id: &str) -> String {
    super::ec2_query::described_instance_state(router, region, instance_id).await
}

/// Whether `reason` is worded the way EC2 words a caller-asked change: the fixed text, then
/// a civil UTC date and time of day, then the zone.
fn is_user_initiated_in_civil_form(reason: &str) -> bool {
    let Some(rest) = reason.strip_prefix(USER_INITIATED) else {
        return false;
    };
    let Some(stamp) = rest.strip_suffix(GMT) else {
        return false;
    };
    let Some((date, time)) = stamp.split_once(' ') else {
        return false;
    };
    let digits = |text: &str, separator: char, groups: &[usize]| -> bool {
        let parts: Vec<&str> = text.split(separator).collect();
        parts.len() == groups.len()
            && parts
                .iter()
                .zip(groups)
                .all(|(part, width)| {
                    part.len() == *width && part.chars().all(|digit| digit.is_ascii_digit())
                })
    };
    digits(date, '-', &[4, 2, 2]) && digits(time, ':', &[2, 2, 2])
}

/// Rule 1, over both changes a caller can ask for: `StopInstances` and `TerminateInstances`
/// each record a reason in AWS's civil form, and neither records the ISO 8601 instant the
/// timestamp members of the same response carry.
#[tokio::test]
async fn a_caller_asked_change_records_the_reason_in_aws_civil_form() {
    // (case, the action that asks for the change)
    let rows: [(&str, &str); 2] = [
        ("a stop", "StopInstances"),
        ("a terminate", "TerminateInstances"),
    ];
    for (case, action) in rows {
        let (router, _engine) = super::ec2_query::emulator();
        let instance_id = launch(&router, case, &[]).await;
        ok(&router, case, action, &indexed("InstanceId", &[&instance_id])).await;

        let members = described_members(&router, case, &instance_id).await;
        let reason = pick(&members, "reason").unwrap_or_else(|| {
            panic!("case {case:?}: expected a reason on the instance, got {members:?}")
        });
        assert!(
            is_user_initiated_in_civil_form(&reason),
            "case {case:?}: expected a reason of the form \
             \"{USER_INITIATED}YYYY-MM-DD HH:MM:SS{GMT}\", got {reason:?}"
        );
        // The ISO 8601 rendering the timestamp members of the same response carry is NOT
        // what the reason names: the date inside it is a civil one, with a space between the
        // day and the time of day and no `T` or `Z` of its own.
        let stamp = reason
            .trim_start_matches(USER_INITIATED)
            .trim_end_matches(GMT)
            .to_string();
        assert!(
            !stamp.contains('T') && !stamp.contains('Z'),
            "case {case:?}: the reason names a civil date, not an ISO 8601 instant, \
             got {reason:?}"
        );
    }
}

/// Rule 2: once the termination has completed, the instance reports none of the placement
/// and storage it held, and the volumes it held are where the termination left them.
#[tokio::test]
async fn a_terminated_instance_reports_no_placement_and_no_storage() {
    let case = "a terminated instance";
    let (router, _engine) = super::ec2_query::emulator();
    let instance_id = launch(&router, case, &[]).await;

    // The launch's own volume, and a volume the caller attached afterwards: the first is
    // marked to be deleted with the instance (the documented default for a root device), the
    // second is not, so one row of the store has to disappear and the other has to survive
    // detached.
    let zone = {
        let body = ok(
            &router,
            case,
            "DescribeInstances",
            &indexed("InstanceId", &[&instance_id]),
        )
        .await;
        element(&body, "availabilityZone")
            .unwrap_or_else(|| panic!("case {case:?}: expected a zone in {body}"))
    };
    let created = ok(
        &router,
        case,
        "CreateVolume",
        &[p("AvailabilityZone", &zone), p("Size", "1")],
    )
    .await;
    let extra_volume = element(&created, "volumeId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a volumeId in {created}"));

    // The instance has to be running and the volume available before the attach: both are
    // moves the clock owes, so the suite waits for the CONDITION.
    super::rbin_retention_rules::poll_until(case, "the launch to complete", || async {
        state_of(&router, REGION, &instance_id).await == "running"
    })
    .await;
    super::rbin_retention_rules::poll_until(case, "the volume to become available", || async {
        let body = ok(
            &router,
            case,
            "DescribeVolumes",
            &indexed("VolumeId", &[&extra_volume]),
        )
        .await;
        reported_volume_state(case, &body) == VOLUME_STATE_AVAILABLE
    })
    .await;
    ok(
        &router,
        case,
        "AttachVolume",
        &[
            p("VolumeId", &extra_volume),
            p("InstanceId", &instance_id),
            p("Device", "/dev/sdf"),
        ],
    )
    .await;

    // The root volume the launch minted, as the instance itself reports it.
    let root_volume = {
        let body = ok(
            &router,
            case,
            "DescribeInstances",
            &indexed("InstanceId", &[&instance_id]),
        )
        .await;
        let parsed = document(case, &body);
        let instance = parsed
            .descendants()
            .find(|node| node.is_element() && child(*node, "blockDeviceMapping").is_some())
            .unwrap_or_else(|| panic!("case {case:?}: no instance in {body}"));
        set_items(instance, "blockDeviceMapping")
            .into_iter()
            .filter_map(|mapping| child(mapping, "ebs"))
            .filter_map(|ebs| child_text(ebs, "volumeId"))
            .find(|volume_id| volume_id != &extra_volume)
            .unwrap_or_else(|| panic!("case {case:?}: expected the launch's own volume in {body}"))
    };

    ok(
        &router,
        case,
        "TerminateInstances",
        &indexed("InstanceId", &[&instance_id]),
    )
    .await;
    super::rbin_retention_rules::poll_until(case, "the termination to complete", || async {
        state_of(&router, REGION, &instance_id).await == "terminated"
    })
    .await;

    let members = described_members(&router, case, &instance_id).await;
    // (member, what the instance must report for it)
    assert_eq!(
        pick(&members, "privateDnsName").as_deref(),
        Some(""),
        "case {case:?}: a terminated instance holds no address, so it reports the empty \
         private hostname, got {members:?}"
    );
    for absent in ["subnetId", "privateIpAddress"] {
        assert_eq!(
            pick(&members, absent),
            None,
            "case {case:?}: a terminated instance reports no {absent}, got {members:?}"
        );
    }
    let metadata_state = {
        let body = ok(
            &router,
            case,
            "DescribeInstances",
            &indexed("InstanceId", &[&instance_id]),
        )
        .await;
        let parsed = document(case, &body);
        parsed
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "metadataOptions")
            .and_then(|options| child_text(options, "state"))
            .unwrap_or_else(|| panic!("case {case:?}: expected metadataOptions in {body}"))
    };
    assert_eq!(
        metadata_state, METADATA_OPTIONS_STATE_PENDING,
        "case {case:?}: a terminated instance processes no metadata traffic, so its options \
         are not applied"
    );

    // The volume the attachment marked to be deleted with the instance is GONE from the
    // account's volume store, which is what the empty block device mapping follows from.
    let (status, body) = call(
        &router,
        REGION,
        "DescribeVolumes",
        &indexed("VolumeId", &[&root_volume]),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the volume deleted with the instance must no longer resolve, got \
         {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidVolume.NotFound",
        "case {case:?}: expected the not-found refusal for a deleted volume, got {body}"
    );

    // The volume the attachment did NOT mark survives, attached to nothing.
    let surviving = ok(
        &router,
        case,
        "DescribeVolumes",
        &indexed("VolumeId", &[&extra_volume]),
    )
    .await;
    assert_eq!(
        reported_volume_state(case, &surviving),
        VOLUME_STATE_AVAILABLE,
        "case {case:?}: a volume not deleted with the instance is detached and available, \
         got {surviving}"
    );
    let parsed = document(case, &surviving);
    let volume = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "volumeId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no volume in {surviving}"));
    assert!(
        set_items(volume, "attachmentSet").is_empty(),
        "case {case:?}: the surviving volume holds no attachment to a terminated instance, \
         got {surviving}"
    );

    // And the instance itself reports no storage at all any more.
    let body = ok(
        &router,
        case,
        "DescribeInstances",
        &indexed("InstanceId", &[&instance_id]),
    )
    .await;
    let parsed = document(case, &body);
    let instance = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no instance in {body}"));
    assert!(
        set_items(instance, "blockDeviceMapping").is_empty(),
        "case {case:?}: a terminated instance owns no volume, so it reports no block device \
         mapping entry, got {body}"
    );
}
