// SPDX-License-Identifier: CC0-1.0

//! The attachment `AssociateVolume` makes IS the Amazon EBS attachment of the volume.
//!
//! RULE under test: a WorkSpaces Instances volume is an Amazon EBS volume of the caller's
//! own account and a WorkSpace Instance is a managed Amazon EC2 instance of it, so the
//! attachment `AssociateVolume` accepts is that volume's EBS attachment to that instance at
//! the `Device` the request named — recorded in the service that OWNS the volume, the same
//! way `CreateVolume` records the volume itself there. So after a successful attach
//! `ec2:DescribeVolumes` resolves the volume with exactly ONE attachment, naming the
//! instance's `EC2ManagedInstance.InstanceId` and that device, and reports the volume
//! `in-use`; `DisassociateVolume` takes that attachment down there, leaving the volume
//! existing and `available`; a REFUSED call writes nothing, so every attachment the account
//! holds is exactly as it was; and an attachment taken down through the EC2 API is one this
//! service holds no more either, because there is ONE attachment and one answer about it.
//!
//! Nothing here reaches into either store: the attachment is believed to exist only because
//! `ec2:DescribeVolumes` reported it. EC2 is driven through the AWS SDK over the router's own
//! in-process callback, the same seam the create's launch reaches it through.
//!
//! The rows span what the rule generalises over: the volume configuration (every volume type
//! the service publishes rules for, each at its own published minimum size and with the IOPS
//! that type requires), the device path (the forms AWS publishes for an attachment point and
//! both ends of the modelled `DeviceName`), whether the instance's create named a placement
//! zone, and the class of refusal (a device another volume holds, a volume already attached,
//! a volume in another zone, an id the account does not hold).//!
//! The wire helpers come from the `workspace_instance_lifecycle` suite, the EC2 client from
//! `ec2_managed_instance_propagation`, the `AssociateVolume` body and envelope reader from
//! `volume_attachment` and the instance create from `volume_zone_affinity`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_workspacesinstances_eucmifrontendapiservice as wsinst;
use smithy_server_runtime::MultiServiceRouter;

use crate::ec2_managed_instance_propagation::ec2_client;
use crate::volume_attachment::{attach_body, managed_instance_id, message};
use crate::volume_zone_affinity::create_instance;
use crate::workspace_instance_lifecycle::{
    call, error_code, harness, ok, ACCOUNT_ZONE, DEFAULT_TEST_REGION,
};

/// How long the emulator is given to bring a volume or an attachment to a state its own
/// clock owes it. Nothing here sleeps for a span: the test looks again until the condition
/// holds, with generous room so a loaded host cannot fail it.
const SETTLE_BUDGET: Duration = Duration::from_secs(60);

/// How long this suite waits between looks while polling for a condition.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The `VolumeAttachmentState` values this suite names (`smithy_query(query="shape",
/// shape="VolumeAttachmentState",
/// model_path="services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json")`).
const ATTACHMENT_ATTACHING: &str = "attaching";
const ATTACHMENT_ATTACHED: &str = "attached";

/// The `VolumeState` values this suite names (`smithy_query(query="shape",
/// shape="VolumeState",
/// model_path="services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json")`).
const VOLUME_AVAILABLE: &str = "available";
const VOLUME_IN_USE: &str = "in-use";

/// One attachment as `ec2:DescribeVolumes` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EbsAttachment {
    instance_id: String,
    device: String,
    state: String,
}

/// What `ec2:DescribeVolumes` reports about one volume: its own `State`, and the attachments
/// it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EbsVolume {
    state: String,
    attachments: Vec<EbsAttachment>,
}

impl EbsVolume {
    /// The `(InstanceId, Device)` pair of every attachment reported, which is what an attach
    /// is asserted to have made and a refusal to have left alone.
    fn attached_at(&self) -> Vec<(String, String)> {
        self.attachments
            .iter()
            .map(|attachment| (attachment.instance_id.clone(), attachment.device.clone()))
            .collect()
    }

    /// Whether every attachment reported has reached `attached`, and there is one.
    fn fully_attached(&self) -> bool {
        !self.attachments.is_empty()
            && self
                .attachments
                .iter()
                .all(|attachment| attachment.state == ATTACHMENT_ATTACHED)
    }
}

/// Device paths an attach may name.
///
/// The first four are the forms AWS publishes for a block-device attachment point (the
/// Amazon EC2 device-naming reference: `/dev/sd[f-p]` on Linux, the `xvd…` paravirtual and
/// Windows names); the last two sit on the bounds of the modelled `DeviceName` (`len 0..32`,
/// `smithy_query(query="shape",shape="DeviceName")`) — its shortest non-empty value and its
/// longest.
fn device_catalog() -> Vec<String> {
    let devices = vec![
        "/dev/sdf".to_string(),
        "/dev/sdp".to_string(),
        "/dev/xvdb".to_string(),
        "xvdh".to_string(),
        "a".to_string(),
        format!("/dev/{}", "d".repeat(27)),
    ];
    assert_eq!(
        devices[5].len(),
        32,
        "case device catalog: the longest row must sit on the modelled maximum, got {:?}",
        devices[5]
    );
    devices
}

/// One accepted-attach case: how the volume is configured, the device path the attach names,
/// and whether the instance's create named a placement zone.
struct Row {
    case: String,
    volume: Value,
    device: String,
    placed: bool,
}

/// One row per volume type the service publishes rules for
/// (`wsinst::state::VOLUME_TYPE_SPECS`, which covers every value of the modelled
/// `VolumeTypeEnum`), each at that type's own published minimum size and with the IOPS it
/// requires, spread across the device catalog and both forms of instance placement — so no
/// row shares a value with its neighbours on any dimension the rule generalises over.
///
/// Every volume sits in [`ACCOUNT_ZONE`], the zone the account's own subnets are in and so
/// the zone the managed instance is launched in: a volume is attached to an instance in its
/// own Availability Zone.
fn rows() -> Vec<Row> {
    let devices = device_catalog();
    let specs = wsinst::state::VOLUME_TYPE_SPECS;
    assert!(
        specs.len() > 1,
        "case row catalog: the rule needs more than one volume type, got {specs:?}"
    );
    specs
        .iter()
        .enumerate()
        .map(|(n, spec)| {
            let device = devices[n % devices.len()].clone();
            let mut volume = json!({
                "AvailabilityZone": ACCOUNT_ZONE,
                "SizeInGB": spec.published_size_gib.0,
                "VolumeType": spec.volume_type,
            });
            if spec.iops_required {
                let (min, _) = spec
                    .iops
                    .unwrap_or_else(|| panic!("a type that requires IOPS must publish a range"));
                volume["Iops"] = json!(min);
            }
            let placed = n % 2 == 0;
            Row {
                case: format!(
                    "a {} volume at {device}, instance {}",
                    spec.volume_type,
                    if placed {
                        "placed in that zone"
                    } else {
                        "placed by the service"
                    }
                ),
                volume,
                device,
                placed,
            }
        })
        .collect()
}

/// `CreateVolume` with `body`, asserting a 200 and returning the minted id.
async fn create_volume(router: &MultiServiceRouter, case: &str, body: Value) -> String {
    let payload = ok(router, "CreateVolume", body).await;
    payload["VolumeId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateVolume returned no VolumeId: {payload}"))
        .to_string()
}

/// A `gp3` volume of 8 GiB in [`ACCOUNT_ZONE`]: the fixture volume of the cases that vary
/// something other than the volume's own configuration.
fn fixture_volume(zone: &str) -> Value {
    json!({ "AvailabilityZone": zone, "SizeInGB": 8, "VolumeType": "gp3" })
}

/// What Amazon EC2 reports for `volume_id`, or `None` when it holds no such volume.
async fn ebs_view(ec2: &::aws_sdk_ec2::Client, volume_id: &str) -> Option<EbsVolume> {
    let answered = ec2
        .describe_volumes()
        .volume_ids(volume_id)
        .send()
        .await
        .ok()?;
    let volume = answered
        .volumes()
        .iter()
        .find(|volume| volume.volume_id() == Some(volume_id))?;
    Some(EbsVolume {
        state: volume
            .state()
            .map(|state| state.as_str().to_string())
            .unwrap_or_default(),
        attachments: volume
            .attachments()
            .iter()
            .map(|attachment| EbsAttachment {
                instance_id: attachment.instance_id().unwrap_or_default().to_string(),
                device: attachment.device().unwrap_or_default().to_string(),
                state: attachment
                    .state()
                    .map(|state| state.as_str().to_string())
                    .unwrap_or_default(),
            })
            .collect(),
    })
}

/// Look again until what Amazon EC2 reports for `volume_id` satisfies `holds`, and answer it.
/// Waits for the CONDITION, not for a span: the deadline is only the point past which the
/// condition is taken to be unreachable.
async fn settled<P>(
    ec2: &::aws_sdk_ec2::Client,
    case: &str,
    volume_id: &str,
    wanted: &str,
    holds: P,
) -> EbsVolume
where
    P: Fn(&EbsVolume) -> bool,
{
    let deadline = Instant::now() + SETTLE_BUDGET;
    loop {
        match ebs_view(ec2, volume_id).await {
            Some(reported) => {
                if holds(&reported) {
                    return reported;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected EC2 to report volume {volume_id} {wanted}, it \
                     reports {reported:?}"
                );
            }
            None => assert!(
                Instant::now() < deadline,
                "case {case:?}: expected EC2 to hold volume {volume_id}, it holds none"
            ),
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What Amazon EC2 reports for `volume_id` right now, failing the case when it holds no such
/// volume.
async fn reported_now(ec2: &::aws_sdk_ec2::Client, case: &str, volume_id: &str) -> EbsVolume {
    ebs_view(ec2, volume_id)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: EC2 holds no volume {volume_id}"))
}

#[tokio::test]
async fn every_accepted_attach_is_the_ebs_attachment_of_the_volume() {
    let (router, _engine) = harness();
    let ec2 = ec2_client(&router);

    for row in rows() {
        let case = row.case.as_str();
        let instance = create_instance(
            &router,
            DEFAULT_TEST_REGION,
            case,
            row.placed.then_some(ACCOUNT_ZONE),
        )
        .await;
        // The managed Amazon EC2 instance the attach is performed against, read back through
        // this service's own report of the instance rather than assumed.
        let managed = managed_instance_id(&router, DEFAULT_TEST_REGION, &instance).await;
        let volume = create_volume(&router, case, row.volume.clone()).await;

        // The volume exists in EBS and is attached to nothing, which is what the attach
        // changes: a client waits for `available` before attaching.
        let reported = settled(&ec2, case, &volume, "available", |reported| {
            reported.state == VOLUME_AVAILABLE
        })
        .await;
        assert!(
            reported.attachments.is_empty(),
            "case {case:?}: a freshly created volume holds no attachment, EC2 reports \
             {reported:?}"
        );

        ok(
            &router,
            "AssociateVolume",
            attach_body(&instance, &volume, &row.device),
        )
        .await;

        // The one observable effect of the call: EBS resolves the volume with exactly one
        // attachment, to this instance, at this device — and the volume is in use by it.
        let reported = settled(&ec2, case, &volume, "attached", |reported| {
            !reported.attachments.is_empty()
        })
        .await;
        assert_eq!(
            reported.attached_at(),
            vec![(managed.clone(), row.device.clone())],
            "case {case:?}: expected exactly the one attachment the attach made, to {managed:?} \
             at {:?}, got {reported:?}",
            row.device
        );
        assert!(
            [ATTACHMENT_ATTACHING, ATTACHMENT_ATTACHED]
                .contains(&reported.attachments[0].state.as_str()),
            "case {case:?}: expected an attachment coming up or up, got {reported:?}"
        );
        assert_eq!(
            reported.state, VOLUME_IN_USE,
            "case {case:?}: a volume that holds an attachment is in use, got {reported:?}"
        );

        // And the owning service's own clock brings it up, which is what makes it an
        // attachment rather than a record.
        let reported = settled(&ec2, case, &volume, "fully attached", EbsVolume::fully_attached)
            .await;
        assert_eq!(
            reported.attached_at(),
            vec![(managed.clone(), row.device.clone())],
            "case {case:?}: expected the one attachment to stay the one, got {reported:?}"
        );
        assert_eq!(
            reported.state, VOLUME_IN_USE,
            "case {case:?}: expected the volume still in use, got {reported:?}"
        );

        // The detach takes that attachment down where it lives, leaving the volume itself
        // alone: it exists, and it is available for another attach.
        ok(
            &router,
            "DisassociateVolume",
            attach_body(&instance, &volume, &row.device),
        )
        .await;
        let reported = settled(&ec2, case, &volume, "detached", |reported| {
            reported.attachments.is_empty()
        })
        .await;
        assert_eq!(
            reported.state, VOLUME_AVAILABLE,
            "case {case:?}: a detached volume is available again, got {reported:?}"
        );

        // A second attach of the same volume, at another device, lands on the same volume:
        // the detach freed it in EBS as well.
        let again = "/dev/sdq";
        ok(
            &router,
            "AssociateVolume",
            attach_body(&instance, &volume, again),
        )
        .await;
        let reported = settled(&ec2, case, &volume, "attached again", |reported| {
            !reported.attachments.is_empty()
        })
        .await;
        assert_eq!(
            reported.attached_at(),
            vec![(managed.clone(), again.to_string())],
            "case {case:?}: expected the one new attachment, got {reported:?}"
        );
    }
}

/// One refused-attach case: the request, and the modelled answer it must get.
struct RefusalRow {
    case: &'static str,
    body: Value,
    status: u16,
    code: &'static str,
    message: String,
}

#[tokio::test]
async fn a_refused_attach_leaves_every_ebs_attachment_as_it_was() {
    let (router, _engine) = harness();
    let ec2 = ec2_client(&router);
    let zones = wsinst::state::availability_zones_in_region(DEFAULT_TEST_REGION);
    let elsewhere_zone = zones
        .iter()
        .find(|zone| zone.as_str() != ACCOUNT_ZONE)
        .unwrap_or_else(|| panic!("case fixture: {DEFAULT_TEST_REGION} must publish another zone"))
        .clone();

    let instance =
        create_instance(&router, DEFAULT_TEST_REGION, "fixture", Some(ACCOUNT_ZONE)).await;
    let managed = managed_instance_id(&router, DEFAULT_TEST_REGION, &instance).await;
    let device = "/dev/sdf";
    let spare_device = "/dev/sdg";

    let volume_a = create_volume(&router, "fixture", fixture_volume(ACCOUNT_ZONE)).await;
    let volume_b = create_volume(&router, "fixture", fixture_volume(ACCOUNT_ZONE)).await;
    let elsewhere = create_volume(&router, "fixture", fixture_volume(&elsewhere_zone)).await;
    for volume in [&volume_a, &volume_b, &elsewhere] {
        settled(&ec2, "fixture", volume, "available", |reported| {
            reported.state == VOLUME_AVAILABLE
        })
        .await;
    }

    // The attach whose attachment every refusal below must leave untouched.
    ok(
        &router,
        "AssociateVolume",
        attach_body(&instance, &volume_a, device),
    )
    .await;
    let kept = settled(&ec2, "fixture", &volume_a, "attached", |reported| {
        !reported.attachments.is_empty()
    })
    .await
    .attached_at();
    assert_eq!(
        kept,
        vec![(managed.clone(), device.to_string())],
        "case fixture: expected the one attachment the attach made, got {kept:?}"
    );

    // Well-formed per its modelled shape, stored by nobody.
    let unknown_volume = "vol-abc123XYZ".to_string();
    let rows = vec![
        RefusalRow {
            // A SECOND attach at a device another volume already holds is the backing
            // service's refusal of the `Device` sent, and it writes nothing — the first
            // volume keeps its one attachment.
            case: "another volume at a device path the instance already carries one at",
            body: attach_body(&instance, &volume_b, device),
            status: 400,
            code: "ValidationException",
            message: format!(
                "Ec2 can't associate volume {volume_b} to instance {managed} \
                 (InvalidParameterValue: Invalid value '{device}' for unixDevice. Attachment \
                 point {device} is already in use)"
            ),
        },
        RefusalRow {
            case: "a volume that already holds an attachment, at a free device",
            body: attach_body(&instance, &volume_a, spare_device),
            status: 409,
            code: "ConflictException",
            message: wsinst::state::VOLUME_ALREADY_ATTACHED_MESSAGE.to_string(),
        },
        RefusalRow {
            case: "a volume in another Availability Zone",
            body: attach_body(&instance, &elsewhere, spare_device),
            status: 400,
            code: "ValidationException",
            message: wsinst::state::VOLUME_ZONE_MISMATCH_MESSAGE.to_string(),
        },
        RefusalRow {
            case: "a volume id the account does not hold",
            body: attach_body(&instance, &unknown_volume, spare_device),
            status: wsinst::state::RESOURCE_NOT_FOUND_STATUS,
            code: "ResourceNotFoundException",
            message: wsinst::state::VOLUME_NOT_FOUND_MESSAGE.to_string(),
        },
    ];

    for row in rows {
        let case = row.case;
        let (status, payload) = call(&router, "AssociateVolume", row.body.clone()).await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected HTTP {}, got {status} with {payload}",
            row.status
        );
        assert_eq!(
            error_code(&payload),
            row.code,
            "case {case:?}: expected {}, got {payload}",
            row.code
        );
        assert_eq!(
            message(&payload),
            Some(row.message.as_str()),
            "case {case:?}: expected the wording {:?}, got {payload}",
            row.message
        );

        // The invariant the refusal must keep: the attachment the accepted call made is
        // still the one attachment EBS reports, and the refused volumes hold none.
        let held = reported_now(&ec2, case, &volume_a).await;
        assert_eq!(
            held.attached_at(),
            kept,
            "case {case:?}: expected volume_a's one attachment untouched, got {held:?}"
        );
        for (name, volume) in [("volume_b", &volume_b), ("the other zone", &elsewhere)] {
            let held = reported_now(&ec2, case, volume).await;
            assert!(
                held.attachments.is_empty(),
                "case {case:?}: a refused attach attaches nothing, EC2 reports {held:?} for \
                 {name}"
            );
        }
    }

    // And the attachment the refusals left alone is the one a detach removes: the accepted
    // call's effect was never rolled back by any of them.
    ok(
        &router,
        "DisassociateVolume",
        attach_body(&instance, &volume_a, device),
    )
    .await;
    let reported = settled(&ec2, "after the refusals", &volume_a, "detached", |reported| {
        reported.attachments.is_empty()
    })
    .await;
    assert_eq!(
        reported.state, VOLUME_AVAILABLE,
        "case after the refusals: expected the detached volume available, got {reported:?}"
    );
}

#[tokio::test]
async fn a_re_attach_at_the_same_device_leaves_the_volume_one_attachment() {
    let (router, _engine) = harness();
    let ec2 = ec2_client(&router);

    // Rows differing in the device the pair of attachments is made at, so the outcome cannot
    // turn on one device path. Each row detaches and attaches again at the SAME device
    // WITHOUT waiting for the detach the owning store still owes a removal for: one
    // `(InstanceId, Device)` pair is one attachment, so the volume must end up holding
    // exactly the new one — never two rows at one device, and never none because the old
    // removal took the new attachment with it.
    for device in ["/dev/sdf", "xvdh"] {
        let case = format!("a re-attach at {device} inside the detach window");
        let case = case.as_str();
        let instance =
            create_instance(&router, DEFAULT_TEST_REGION, case, Some(ACCOUNT_ZONE)).await;
        let managed = managed_instance_id(&router, DEFAULT_TEST_REGION, &instance).await;
        let volume = create_volume(&router, case, fixture_volume(ACCOUNT_ZONE)).await;
        settled(&ec2, case, &volume, "available", |reported| {
            reported.state == VOLUME_AVAILABLE
        })
        .await;

        ok(
            &router,
            "AssociateVolume",
            attach_body(&instance, &volume, device),
        )
        .await;
        settled(&ec2, case, &volume, "fully attached", EbsVolume::fully_attached).await;

        ok(
            &router,
            "DisassociateVolume",
            attach_body(&instance, &volume, device),
        )
        .await;
        ok(
            &router,
            "AssociateVolume",
            attach_body(&instance, &volume, device),
        )
        .await;

        // The new attachment is the volume's, and the owing clock of the detach cannot sweep
        // it: it reaches `attached` and stays the one attachment reported.
        let reported = settled(&ec2, case, &volume, "fully attached again", |reported| {
            reported.fully_attached()
        })
        .await;
        assert_eq!(
            reported.attached_at(),
            vec![(managed.clone(), device.to_string())],
            "case {case:?}: expected the one new attachment at {device}, got {reported:?}"
        );
        assert_eq!(
            reported.state, VOLUME_IN_USE,
            "case {case:?}: a volume that holds an attachment is in use, got {reported:?}"
        );

        // And the detach of THAT attachment still works, which it would not if the record had
        // two rows under one key.
        ok(
            &router,
            "DisassociateVolume",
            attach_body(&instance, &volume, device),
        )
        .await;
        let reported = settled(&ec2, case, &volume, "detached", |reported| {
            reported.attachments.is_empty()
        })
        .await;
        assert_eq!(
            reported.state, VOLUME_AVAILABLE,
            "case {case:?}: expected the volume available again, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn an_attachment_taken_down_through_ec2_is_one_this_service_holds_no_more() {
    let (router, _engine) = harness();
    let ec2 = ec2_client(&router);

    // Rows differing in the device the attachment was made at, so the outcome cannot turn on
    // one device path.
    for device in ["/dev/sdf", "xvdh"] {
        let case = format!("an attachment at {device} taken down through EC2");
        let case = case.as_str();
        let instance =
            create_instance(&router, DEFAULT_TEST_REGION, case, Some(ACCOUNT_ZONE)).await;
        let managed = managed_instance_id(&router, DEFAULT_TEST_REGION, &instance).await;
        let volume = create_volume(&router, case, fixture_volume(ACCOUNT_ZONE)).await;
        settled(&ec2, case, &volume, "available", |reported| {
            reported.state == VOLUME_AVAILABLE
        })
        .await;

        ok(
            &router,
            "AssociateVolume",
            attach_body(&instance, &volume, device),
        )
        .await;
        settled(&ec2, case, &volume, "fully attached", EbsVolume::fully_attached).await;

        // The attachment is an ordinary instance attachment of the account, so the account
        // can take it down through the API that owns it.
        ec2.detach_volume()
            .volume_id(&volume)
            .instance_id(&managed)
            .device(device)
            .send()
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: ec2:DetachVolume answers: {error:?}"));
        let reported = settled(&ec2, case, &volume, "detached", |reported| {
            reported.attachments.is_empty()
        })
        .await;
        assert_eq!(
            reported.state, VOLUME_AVAILABLE,
            "case {case:?}: expected the volume available again, got {reported:?}"
        );

        // There is ONE attachment and one answer about it: this service holds it no more, so
        // a detach has nothing to remove and the volume is attachable again.
        let (status, payload) = call(
            &router,
            "DisassociateVolume",
            attach_body(&instance, &volume, device),
        )
        .await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected the not-attached conflict, got {status} with {payload}"
        );
        assert_eq!(
            message(&payload),
            Some(wsinst::state::VOLUME_NOT_ATTACHED_MESSAGE),
            "case {case:?}: expected the not-attached wording, got {payload}"
        );

        ok(
            &router,
            "AssociateVolume",
            attach_body(&instance, &volume, device),
        )
        .await;
        let reported = settled(&ec2, case, &volume, "attached again", |reported| {
            !reported.attachments.is_empty()
        })
        .await;
        assert_eq!(
            reported.attached_at(),
            vec![(managed.clone(), device.to_string())],
            "case {case:?}: expected the volume attachable again, got {reported:?}"
        );
    }
}
