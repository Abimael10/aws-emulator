// SPDX-License-Identifier: CC0-1.0

//! The refusals the volume operations surface from the services that BACK them, driven
//! over the wire this service really speaks (awsJson1_0, `POST /`,
//! `X-Amz-Target: EUCMIFrontendAPIService.<Operation>`).
//!
//! The rule under test, stated once and asserted over a SET of inputs rather than one
//! recorded request:
//!
//! > a volume call this service refuses for a condition the BACKING Amazon EBS/EC2
//! > service detects — the attach EC2 performs against the managed instance, the
//! > provisioned-performance window that service's own create enforces, the
//! > `available` state its delete requires — is answered with the modelled error class
//! > that backing refusal maps to (HTTP 400 `ValidationException` for a parameter value
//! > that service will not take, HTTP 409 `ConflictException` for a volume whose state
//! > forbids the call) and carries that service's own refusal: this service's wrapper
//! > for the call that failed, the code the backing service classified the refusal
//! > under and the sentence it worded, with the values filled from the request and from
//! > this service's state.
//!
//! The rows differ along what that rule generalises over: the operation
//! (`AssociateVolume`, `CreateVolume`, `DeleteVolume`), the device path (the published
//! attachment-point forms and both ends of the modelled `DeviceName`), the volume type
//! (every one that publishes a performance window, not only the cited `gp3`), the
//! member the window belongs to (`Iops`, `Throughput`), which side of the window the
//! value falls on, and how far past it the value is.
//!
//! Nothing here can pass against a canned answer: every id comes back out of a create,
//! the EC2 instance id in the expected wording is read back through
//! `GetWorkspaceInstance`, each expected sentence is written out here rather than taken
//! from the code under test, and every refusal is followed by the call that succeeds
//! once the condition is gone.

use serde_json::{json, Value};
use service_com_amazonaws_workspacesinstances_eucmifrontendapiservice as wsinst;

use crate::volume_attachment::{attach_body, managed_instance_id, message};
use crate::volume_lifecycle::create_volume;
use crate::workspace_instance_lifecycle::{
    call, create, error_code, harness, minimal_create, ok, DEFAULT_TEST_REGION,
};

/// The `ValidationExceptionReason` a refusal the BACKING service made is reported with,
/// as the modelled enum value
/// (`smithy_query(query="shape",shape="ValidationExceptionReason")`): the call was
/// refused by the service this one depends on, not by a constraint of this request.
const DEPENDENCY_FAILURE: &str = "DEPENDENCY_FAILURE";

/// Device paths an attach may name.
///
/// The first three are forms AWS publishes for a block-device attachment point (the
/// Amazon EC2 device-naming reference: `/dev/sd[f-p]` on Linux, the `xvd…` paravirtual
/// and Windows names); the last two sit on the bounds of the modelled `DeviceName`
/// (`len 0..32`, `smithy_query(query="shape",shape="DeviceName")`) — its shortest
/// non-empty value and its longest.
fn device_catalog() -> Vec<String> {
    let devices = vec![
        "/dev/sdf".to_string(),
        "/dev/xvdb".to_string(),
        "xvdh".to_string(),
        "a".to_string(),
        format!("/dev/{}", "d".repeat(27)),
    ];
    assert_eq!(
        devices[4].len(),
        32,
        "case device catalog: the longest row must sit on the modelled maximum, got {:?}",
        devices[4]
    );
    devices
}

/// A `CreateVolume` body in `zone` for `spec`'s type: the type's own floor size, and the
/// IOPS a type that requires them takes.
fn accepted_body(spec: &wsinst::state::VolumeTypeSpec, zone: &str) -> Value {
    let mut body = json!({
        "AvailabilityZone": zone,
        "VolumeType": spec.volume_type,
        "SizeInGB": spec.size_range().0,
    });
    if spec.iops_required {
        body["Iops"] = json!(
            spec.iops
                .expect("a type that requires IOPS publishes a window")
                .0
        );
    }
    body
}

/// The sentence this service answers for a create the backing Amazon EBS service refused
/// for a performance value outside the window the volume type publishes.
///
/// Written out here, from the refusal AWS answers for that class, so the assertion is on
/// the wording rather than on whatever the code under test produces: the fixed
/// `Create volume failed (`, the backing service's `InvalidParameterValue` code, the
/// `Volume <member> of <value>` the request sent, and the bound it passed.
fn expected_create_refusal(member: &str, value: i32, above: bool, bound: i32) -> String {
    let clause = if above {
        format!("is too high; maximum is {bound}.")
    } else {
        format!("is too low; minimum is {bound}.")
    };
    format!("Create volume failed (InvalidParameterValue: Volume {member} of {value} {clause})")
}

/// How many volumes this service's own store holds, read the one legal way: the engine
/// the router was built from.
fn stored_volumes(engine: &engine::InMemoryEngine) -> usize {
    wsinst::state::service_state(engine)
        .expect("the service's store is reachable from the engine the router was built from")
        .lock()
        .volumes
        .len()
}

#[tokio::test]
async fn an_attach_at_an_occupied_device_carries_ec2s_refusal_of_the_device() {
    let (router, engine) = harness();
    let zone = format!("{DEFAULT_TEST_REGION}a");
    let instance = create(&router, minimal_create()).await;
    // The managed EC2 instance the attach is performed against, read back through this
    // service's own report of the instance rather than assumed.
    let managed_instance = managed_instance_id(&router, DEFAULT_TEST_REGION, &instance).await;

    for device in device_catalog() {
        let case = format!("the device path {device:?}");
        let held = create_volume(&router, json!({ "AvailabilityZone": &zone, "SizeInGB": 4 })).await;
        let refused =
            create_volume(&router, json!({ "AvailabilityZone": &zone, "SizeInGB": 8 })).await;
        ok(&router, "AssociateVolume", attach_body(&instance, &held, &device)).await;

        let before = stored_volumes(&engine);
        let (status, payload) = call(
            &router,
            "AssociateVolume",
            attach_body(&instance, &refused, &device),
        )
        .await;
        // The attachment point is taken on the instance EC2 holds, so what the caller
        // reads is EC2's refusal of the `Device` value — a bad parameter (HTTP 400), not
        // a state another call clears.
        assert_eq!(
            (status, error_code(&payload).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected the modelled validation refusal, got {status} with \
             {payload}"
        );
        let expected = format!(
            "Ec2 can't associate volume {refused} to instance {managed_instance} \
             (InvalidParameterValue: Invalid value '{device}' for unixDevice. Attachment \
             point {device} is already in use)"
        );
        assert_eq!(
            message(&payload),
            Some(expected.as_str()),
            "case {case:?}: expected EC2's own refusal of the device, got {payload}"
        );
        assert_eq!(
            payload["Reason"].as_str(),
            Some(DEPENDENCY_FAILURE),
            "case {case:?}: expected the dependency-failure reason, got {payload}"
        );
        assert!(
            payload["FieldList"].is_null(),
            "case {case:?}: a dependency's refusal faults no member of this request, got \
             {payload}"
        );
        assert_eq!(
            stored_volumes(&engine),
            before,
            "case {case:?}: a refused attach must persist nothing"
        );

        // The refusal was about THAT device path: the same pair attaches at a device
        // nothing occupies, and the volume that was refused is deletable, so no
        // attachment was written for it. The free path is one no catalog row names.
        let free = "/dev/sdp";
        ok(
            &router,
            "AssociateVolume",
            attach_body(&instance, &refused, free),
        )
        .await;
        ok(
            &router,
            "DisassociateVolume",
            attach_body(&instance, &refused, free),
        )
        .await;
        ok(&router, "DeleteVolume", json!({ "VolumeId": &refused })).await;
        ok(
            &router,
            "DisassociateVolume",
            attach_body(&instance, &held, &device),
        )
        .await;
        ok(&router, "DeleteVolume", json!({ "VolumeId": &held })).await;
    }
}

#[tokio::test]
async fn a_create_outside_a_published_performance_window_carries_ebss_refusal() {
    let (router, engine) = harness();
    let zone = format!("{DEFAULT_TEST_REGION}a");

    // One set of rows per volume type that publishes a performance window, generated
    // from the service's own catalog (`wsinst::state::VOLUME_TYPE_SPECS`, whose doc
    // carries the Amazon EBS sentences the windows come from) — so `gp3`, the type the
    // issue cites, is one row of several, and a bound that changes in the catalog
    // changes the rows with it.
    let mut rows: Vec<(String, &str, Value, String)> = Vec::new();
    for spec in &wsinst::state::VOLUME_TYPE_SPECS {
        let volume_type = spec.volume_type;
        for (member, window) in [
            (wsinst::state::IOPS_MEMBER, spec.iops),
            (wsinst::state::THROUGHPUT_MEMBER, spec.throughput_mib),
        ] {
            let Some((min, max)) = window else {
                continue;
            };
            // The value one past each published bound, a value far past the ceiling (so
            // the wording has to report what was SENT rather than the bound next to it),
            // and `0`, which the modelled `range 0..` admits and every window excludes.
            let far_above = max.saturating_mul(10).max(max + 2);
            for (what, value, above, bound) in [
                (format!("one below the {member} floor"), min - 1, false, min),
                (format!("one above the {member} ceiling"), max + 1, true, max),
                (format!("far above the {member} ceiling"), far_above, true, max),
                (format!("{member} 0, below every floor"), 0, false, min),
            ] {
                let mut body = accepted_body(spec, &zone);
                body[member] = json!(value);
                rows.push((
                    format!("{volume_type} {what}"),
                    member,
                    body,
                    expected_create_refusal(
                        &member.to_ascii_lowercase(),
                        value,
                        above,
                        bound,
                    ),
                ));
            }
        }
    }
    assert!(
        rows.iter().any(|(case, ..)| case.starts_with("gp3 ")),
        "case catalog: the rows must cover the cited type as well as the others, got {:?}",
        rows.iter().map(|(case, ..)| case.clone()).collect::<Vec<_>>()
    );

    for (case, member, body, expected) in rows {
        let before = stored_volumes(&engine);
        let (status, payload) = call(&router, "CreateVolume", body.clone()).await;
        assert_eq!(
            (status, error_code(&payload).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected the modelled validation refusal for {body}, got \
             {status} with {payload}"
        );
        assert_eq!(
            message(&payload),
            Some(expected.as_str()),
            "case {case:?}: expected the backing service's refusal of the {member} sent, \
             got {payload}"
        );
        assert_eq!(
            payload["Reason"].as_str(),
            Some(DEPENDENCY_FAILURE),
            "case {case:?}: expected the dependency-failure reason, got {payload}"
        );
        assert!(
            payload["FieldList"].is_null(),
            "case {case:?}: a dependency's refusal faults no member of this request, got \
             {payload}"
        );
        assert_eq!(
            stored_volumes(&engine),
            before,
            "case {case:?}: a refused create must store nothing"
        );
    }

    // And the windows themselves are untouched by the wording: each published bound is
    // still a value the create provisions.
    for spec in &wsinst::state::VOLUME_TYPE_SPECS {
        for (member, window) in [
            (wsinst::state::IOPS_MEMBER, spec.iops),
            (wsinst::state::THROUGHPUT_MEMBER, spec.throughput_mib),
        ] {
            let Some((min, max)) = window else {
                continue;
            };
            for value in [min, max] {
                let mut body = accepted_body(spec, &zone);
                body[member] = json!(value);
                let case = format!("{} at its {member} bound {value}", spec.volume_type);
                let id = create_volume(&router, body).await;
                ok(&router, "DeleteVolume", json!({ "VolumeId": &id })).await;
                assert_eq!(
                    stored_volumes(&engine),
                    0,
                    "case {case:?}: the accepted create and its delete must leave the \
                     store as it was"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_delete_of_an_attached_volume_carries_ebss_not_available_sentence() {
    let (router, _engine) = harness();
    let zone = format!("{DEFAULT_TEST_REGION}a");
    let instance = create(&router, minimal_create()).await;

    // Several volumes attached at once, each at its own device path: the refusal names
    // the volume it is about, so a caller deleting a set of them can tell which.
    let devices = device_catalog();
    let mut attached = Vec::new();
    for (index, device) in devices.iter().enumerate() {
        let size = 4 + i32::try_from(index).expect("the device catalog is short");
        let volume = create_volume(
            &router,
            json!({ "AvailabilityZone": &zone, "SizeInGB": size }),
        )
        .await;
        ok(&router, "AssociateVolume", attach_body(&instance, &volume, device)).await;
        attached.push((volume, device.clone()));
    }

    for (volume, device) in &attached {
        let case = format!("the volume attached at {device:?}");
        let (status, payload) = call(&router, "DeleteVolume", json!({ "VolumeId": volume })).await;
        // A volume holding an attachment is not in the `available` state the backing
        // Amazon EBS delete requires: a state a detach clears, which is the modelled
        // conflict (HTTP 409).
        assert_eq!(
            (status, error_code(&payload).as_str()),
            (409, "ConflictException"),
            "case {case:?}: expected the modelled conflict, got {status} with {payload}"
        );
        let expected = format!("EBS Volume is not in available state for volume {volume}");
        assert_eq!(
            message(&payload),
            Some(expected.as_str()),
            "case {case:?}: expected the backing service's not-available sentence naming \
             {volume:?}, got {payload}"
        );
        assert_eq!(
            payload["ResourceId"].as_str(),
            Some(volume.as_str()),
            "case {case:?}: expected the conflict to name {volume:?}, got {payload}"
        );
        assert_eq!(
            payload["ResourceType"].as_str(),
            Some(wsinst::state::VOLUME_RESOURCE_TYPE),
            "case {case:?}: expected the volume resource type, got {payload}"
        );
    }

    // Each refusal was about the state of its own volume: once detached, the same delete
    // is served.
    for (volume, device) in &attached {
        ok(
            &router,
            "DisassociateVolume",
            attach_body(&instance, volume, device),
        )
        .await;
        ok(&router, "DeleteVolume", json!({ "VolumeId": volume })).await;
    }
}
