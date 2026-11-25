// SPDX-License-Identifier: CC0-1.0

//! `DisassociateVolume` takes the volume's Amazon EBS attachment down whichever
//! `DisassociateMode` it is asked with.
//!
//! RULE under test: the attachment `AssociateVolume` accepts IS the Amazon EBS attachment of
//! that volume to the managed Amazon EC2 instance the WorkSpace Instance is, so an accepted
//! `DisassociateVolume` takes THAT attachment down — after it, `ec2:DescribeVolumes` resolves
//! the volume with no attachments and the volume goes on existing, `available` rather than
//! gone, and attachable again. `DisassociateMode` chooses only how the guest is asked to
//! release the device before the volume is detached, so the volume ends up detached either
//! way and no mode is given an effect of its own; `Device` names which attachment is removed
//! when it is sent and the volume's one attachment is meant when it is not.
//!
//! Nothing here reaches into either store: the attachment is believed to be gone only because
//! `ec2:DescribeVolumes` stopped reporting it, and the two views of the one attachment are
//! asserted to agree by this service refusing a second detach of a volume it holds no
//! attachment for.
//!
//! The rows span what the rule generalises over: every value the modelled
//! `DisassociateModeEnum` publishes and its absence, whether `Device` is sent, and whether
//! the detach is made while the attachment is still coming up or after the owning service's
//! clock brought it up.
//!
//! The EBS view comes from the `ebs_attachment_propagation` suite, the EC2 client from
//! `ec2_managed_instance_propagation`, the `AssociateVolume` body and envelope reader from
//! `volume_attachment`, the instance create from `volume_zone_affinity` and the volume create
//! from `volume_lifecycle`.

use serde_json::{json, Value};
use service_com_amazonaws_workspacesinstances_eucmifrontendapiservice as wsinst;

use crate::ebs_attachment_propagation::{
    ebs_view, settled, EbsVolume, VOLUME_AVAILABLE, VOLUME_IN_USE,
};
use crate::ec2_managed_instance_propagation::ec2_client;
use crate::volume_attachment::{attach_body, managed_instance_id, message};
use crate::volume_lifecycle::{create_volume, simple_create};
use crate::volume_zone_affinity::create_instance;
use crate::workspace_instance_lifecycle::{
    call, error_code, harness, ok, ACCOUNT_ZONE, DEFAULT_TEST_REGION,
};

/// Whether the detach waits for the owning service's clock to bring the attachment up first.
#[derive(Clone, Copy, Debug)]
enum DetachWhen {
    /// Straight after the attach, while the attachment is still coming up.
    StillComingUp,
    /// Once `ec2:DescribeVolumes` reports every attachment of the volume `attached`.
    FullyAttached,
}

/// One detach case: the mode it is asked with, whether it names the device, and when it is
/// made.
struct Row {
    case: &'static str,
    /// The `DisassociateMode` the request carries, or `None` when the member is left out.
    mode: Option<&'static str>,
    /// Whether the request names the `Device` the attachment is at.
    names_device: bool,
    when: DetachWhen,
}

/// Every value the modelled `DisassociateModeEnum` publishes
/// (`smithy_query(query="shape",shape="DisassociateModeEnum")`), each crossed with one of the
/// other two dimensions so no row's outcome can come from a combination the suite never
/// drives.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "FORCE, device named, attachment up",
            mode: Some("FORCE"),
            names_device: true,
            when: DetachWhen::FullyAttached,
        },
        Row {
            case: "FORCE, device left out, attachment still coming up",
            mode: Some("FORCE"),
            names_device: false,
            when: DetachWhen::StillComingUp,
        },
        Row {
            case: "NO_FORCE, device named, attachment still coming up",
            mode: Some("NO_FORCE"),
            names_device: true,
            when: DetachWhen::StillComingUp,
        },
        Row {
            case: "NO_FORCE, device left out, attachment up",
            mode: Some("NO_FORCE"),
            names_device: false,
            when: DetachWhen::FullyAttached,
        },
        Row {
            case: "mode left out, device named, attachment up",
            mode: None,
            names_device: true,
            when: DetachWhen::FullyAttached,
        },
        Row {
            case: "mode left out, device left out, attachment still coming up",
            mode: None,
            names_device: false,
            when: DetachWhen::StillComingUp,
        },
    ]
}

/// The device path the rows attach at. Inside the modelled `DeviceName` (`len 0..32`) and one
/// of the forms AWS publishes for a Linux block-device attachment point.
const DEVICE: &str = "/dev/sdh";

/// A device path no row attaches at, which a detach naming it therefore names no attachment
/// of. Another of the forms AWS publishes for a Linux block-device attachment point.
const OTHER_DEVICE: &str = "/dev/sdz";

/// A `DisassociateVolume` body: `Device` and `DisassociateMode` are both optional, so each is
/// carried only when the caller names one.
fn detach_body(instance: &str, volume: &str, device: Option<&str>, mode: Option<&str>) -> Value {
    let mut body = json!({ "WorkspaceInstanceId": instance, "VolumeId": volume });
    if let Some(device) = device {
        body["Device"] = json!(device);
    }
    if let Some(mode) = mode {
        body["DisassociateMode"] = json!(mode);
    }
    body
}

#[tokio::test]
async fn a_detach_takes_the_ebs_attachment_down_whichever_mode_it_is_asked_with() {
    let (router, _engine) = harness();
    let ec2 = ec2_client(&router);

    for row in rows() {
        let case = row.case;
        let instance =
            create_instance(&router, DEFAULT_TEST_REGION, case, Some(ACCOUNT_ZONE)).await;
        // The managed Amazon EC2 instance the attachment is against, read back through this
        // service's own report of the instance rather than assumed.
        let managed = managed_instance_id(&router, DEFAULT_TEST_REGION, &instance).await;
        let volume = create_volume(&router, simple_create(ACCOUNT_ZONE)).await;
        settled(&ec2, case, &volume, "available", |reported| {
            reported.state == VOLUME_AVAILABLE
        })
        .await;

        ok(&router, "AssociateVolume", attach_body(&instance, &volume, DEVICE)).await;
        let reported = settled(&ec2, case, &volume, "attached", |reported| {
            !reported.attachments.is_empty()
        })
        .await;
        assert_eq!(
            reported.attached_at(),
            vec![(managed.clone(), DEVICE.to_string())],
            "case {case:?}: expected the one attachment the attach made, to {managed:?} at \
             {DEVICE:?}, got {reported:?}"
        );
        assert_eq!(
            reported.state, VOLUME_IN_USE,
            "case {case:?}: a volume that holds an attachment is in use, got {reported:?}"
        );
        if matches!(row.when, DetachWhen::FullyAttached) {
            settled(&ec2, case, &volume, "fully attached", EbsVolume::fully_attached).await;
        }

        let body = detach_body(
            &instance,
            &volume,
            row.names_device.then_some(DEVICE),
            row.mode,
        );
        ok(&router, "DisassociateVolume", body.clone()).await;

        // The one observable effect of the call, whichever mode asked for it: EBS reports the
        // volume with no attachment left, and reports the volume — it was detached, not
        // deleted, so it exists and is available for another attach.
        let reported = settled(&ec2, case, &volume, "detached", |reported| {
            reported.attachments.is_empty()
        })
        .await;
        assert_eq!(
            reported.state, VOLUME_AVAILABLE,
            "case {case:?}: a detached volume goes on existing and is available again, got \
             {reported:?}"
        );

        // And this service's own view of the one attachment agrees: there is nothing left for
        // a second detach to remove.
        let (status, payload) = call(&router, "DisassociateVolume", body).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected a second detach to be refused with HTTP 409, got {status} \
             with {payload}"
        );
        assert_eq!(
            error_code(&payload),
            "ConflictException",
            "case {case:?}: expected ConflictException from the second detach, got {payload}"
        );
        assert_eq!(
            payload["ResourceId"].as_str(),
            Some(volume.as_str()),
            "case {case:?}: expected the conflict to name the volume, got {payload}"
        );
        assert_eq!(
            payload["ResourceType"].as_str(),
            Some(wsinst::state::VOLUME_RESOURCE_TYPE),
            "case {case:?}: expected the conflict to name the volume's resource type, got \
             {payload}"
        );
        assert_eq!(
            message(&payload),
            Some(wsinst::state::VOLUME_NOT_ATTACHED_MESSAGE),
            "case {case:?}: expected the service's own wording for a volume holding no \
             attachment, got {payload}"
        );

        // The detach freed the volume where the volume IS, so it takes another attachment —
        // at the device the old one held, which the detach freed too.
        ok(&router, "AssociateVolume", attach_body(&instance, &volume, DEVICE)).await;
        let reported = settled(&ec2, case, &volume, "attached again", |reported| {
            !reported.attachments.is_empty()
        })
        .await;
        assert_eq!(
            reported.attached_at(),
            vec![(managed.clone(), DEVICE.to_string())],
            "case {case:?}: expected the one new attachment, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_refused_detach_leaves_the_ebs_attachment_standing() {
    let (router, _engine) = harness();
    let ec2 = ec2_client(&router);

    let case = "refused detach";
    let instance = create_instance(&router, DEFAULT_TEST_REGION, case, Some(ACCOUNT_ZONE)).await;
    let managed = managed_instance_id(&router, DEFAULT_TEST_REGION, &instance).await;
    let volume = create_volume(&router, simple_create(ACCOUNT_ZONE)).await;
    settled(&ec2, case, &volume, "available", |reported| {
        reported.state == VOLUME_AVAILABLE
    })
    .await;
    ok(&router, "AssociateVolume", attach_body(&instance, &volume, DEVICE)).await;
    settled(&ec2, case, &volume, "fully attached", EbsVolume::fully_attached).await;

    // A detach that names a device the volume is not attached at names no attachment of this
    // volume, whichever mode asks for it — so it is refused and nothing is taken down.
    for mode in [Some("FORCE"), Some("NO_FORCE"), None] {
        let body = detach_body(&instance, &volume, Some(OTHER_DEVICE), mode);
        let (status, payload) = call(&router, "DisassociateVolume", body).await;
        assert_eq!(
            status, 409,
            "case {mode:?}: expected a detach at a device the volume is not attached at to be \
             refused with HTTP 409, got {status} with {payload}"
        );
        assert_eq!(
            error_code(&payload),
            "ConflictException",
            "case {mode:?}: expected ConflictException from the refused detach, got {payload}"
        );
        assert_eq!(
            message(&payload),
            Some(wsinst::state::VOLUME_DEVICE_MISMATCH_MESSAGE),
            "case {mode:?}: expected the service's own wording for a device the volume is not \
             attached at, got {payload}"
        );
        let reported = ebs_view(&ec2, &volume)
            .await
            .unwrap_or_else(|| panic!("case {mode:?}: EC2 holds no volume {volume}"));
        assert_eq!(
            reported.attached_at(),
            vec![(managed.clone(), DEVICE.to_string())],
            "case {mode:?}: expected the refused detach to leave the attachment standing, got \
             {reported:?}"
        );
        assert_eq!(
            reported.state, VOLUME_IN_USE,
            "case {mode:?}: expected the volume still in use, got {reported:?}"
        );
    }
}
