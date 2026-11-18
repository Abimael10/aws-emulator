// SPDX-License-Identifier: CC0-1.0

//! A `forward` to an `instance` or an `ip` TARGET GROUP: how a request a client sent to a
//! load balancer is served by the workload the REGISTERED TARGET runs.
//!
//! `TargetGroup$TargetType` gives what such a registration names — "instance - Targets are
//! specified by instance ID" and "ip - Targets are specified by IP address" — and
//! `TargetDescription$Port` gives where the workload behind it is reached: "the port on which
//! the target is listening". So a `forward` to one of those groups names a resource of this
//! emulator's own account, on a port of it, and what answers the request is the server that
//! resource RUNS there: an instance of this account runs what its own user data installed and
//! started, which its guest serves out of its own storage
//! (`service_com_amazonaws_ec2_amazonec2::state::guest_services`). The client is then answered
//! with the TARGET's own status line, media type and bytes — nothing of the answer is the load
//! balancer's.
//!
//! The hand-off is made with no lock of this service's store held, exactly as a `lambda`
//! target's invocation is ([`super::lambda_target`]), and it changes nothing: serving a
//! request moves no status of the target, consumes no quota and bumps no counter — the
//! registration's health is what its own checks decide (`crate::state::host_counts`).
//!
//! # What the load balancer answers instead
//!
//! A registration this emulator has no server behind on that port is a target the load
//! balancer cannot establish a connection to, which is
//! [`super::data_plane::TARGET_UNREACHABLE_STATUS`] — a target that is not an instance of this
//! account, an instance whose guest is not up, and one running nothing on the port the
//! registration receives traffic on. No body is invented for it: the load balancer's own error
//! page is not a document this emulator holds, exactly as the sibling statuses this plane
//! answers carry none ([`super::data_plane`]).

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

use super::data_plane::{DataPlaneRequest, Served, TARGET_UNREACHABLE_STATUS};

/// The `TargetType` whose registrations name an INSTANCE of the account, and the one whose
/// registrations name an ADDRESS (`crate::ops::types::TARGET_TYPE_ENUM_VALUES` carries the
/// whole catalog).
const INSTANCE_TARGET_TYPE: &str = "instance";
const IP_TARGET_TYPE: &str = "ip";

/// The response header a load balancer's own cookie travels in — the target-group stickiness
/// pin ([`super::target_group_selection`]) — beside whatever the target's own response set.
const SET_COOKIE_HEADER: &str = "set-cookie";

/// Which target a `forward` reaches and where — read out of the store under its own guard, so
/// the request is served with no lock of this service's held.
#[derive(Debug)]
pub(super) struct Forwarded {
    /// The target group the chosen registration belongs to, which the load balancer's own
    /// access log records as the group the request was forwarded to.
    pub(super) target_group_arn: String,
    /// That group's `TargetType`, which says what the registration's id names.
    pub(super) target_type: String,
    /// The registration's id: an instance id, or an address.
    pub(super) target_id: String,
    /// The port the registration receives traffic on — "the port on which the target is
    /// listening" — which is the target's own port when it carries one and the target group's
    /// otherwise.
    pub(super) port: Option<i32>,
    /// The `Set-Cookie` field the answer carries, where the forward action pins the client to
    /// the target group this request was distributed to
    /// ([`super::target_group_selection`]). The load balancer's own, so it accompanies the
    /// response the TARGET produced and no status the load balancer answered instead.
    pub(super) set_cookie: Option<String>,
}

/// Serve one request with the workload the target `forwarded` names runs, and answer what that
/// server answered — or the load balancer's own [`TARGET_UNREACHABLE_STATUS`] when this
/// emulator has no server behind the registration on that port.
///
/// Every answer records what the load balancer OBSERVED while producing it, which is what its
/// own access log carries for the request ([`super::access_logs`]): the `forward` it performed,
/// the target group it went to, the status the TARGET answered where it answered one, and how
/// long the target took.
pub(super) fn served<Eng: Send + Sync + 'static>(
    engine: &Eng,
    forwarded: &Forwarded,
    request: &DataPlaneRequest,
) -> Served {
    let mut served = proxied(engine, forwarded, request);
    served
        .observed
        .actions
        .push(crate::state::FORWARD_ACTION_TYPE.to_string());
    served.observed.target_group_arn = Some(forwarded.target_group_arn.clone());
    served
}

/// The hop itself: the target's own answer, as the server it runs produced it.
fn proxied<Eng: Send + Sync + 'static>(
    engine: &Eng,
    forwarded: &Forwarded,
    request: &DataPlaneRequest,
) -> Served {
    let Some(port) = forwarded.port else {
        // A registration with no port at all names nothing to connect to: neither the target
        // nor its group says which port the workload is listening on.
        ::tracing::warn!(
            target = %forwarded.target_id,
            "the registration names no port the target receives traffic on, so the connection \
             to it cannot be made"
        );
        return Served::status(TARGET_UNREACHABLE_STATUS);
    };
    let Some(state) = ec2::service_state(engine) else {
        // Nothing of this emulator reports what the account's instances run, so there is no
        // server behind the registration to connect to.
        return Served::status(TARGET_UNREACHABLE_STATUS);
    };
    // How long the TARGET took, which the access-log record reports for a connection that was
    // made at all.
    let started = ::std::time::Instant::now();
    let answered = {
        let store = state.lock();
        let instance_id = match forwarded.target_type.as_str() {
            // "if the target type is instance, specify an instance ID"
            // (`TargetDescription$Id`).
            INSTANCE_TARGET_TYPE => Some(forwarded.target_id.clone()),
            // "if the target type is ip, specify an IP address": the workload at that address
            // is the one the interface holding it belongs to.
            IP_TARGET_TYPE => store.instance_id_at_address(&forwarded.target_id),
            // Every other target type is one this hand-off does not name a server for: a
            // `lambda` target is invoked ([`super::lambda_target`]) and an `alb` one is passed
            // on ([`super::data_plane`]), both before this.
            _ => None,
        };
        instance_id.and_then(|instance_id| {
            // The request's own path, which is what a static server resolves under the
            // directory it serves.
            store.serve_instance_guest_http(&instance_id, port, &request.method, &request.path)
        })
    };
    let Some(answered) = answered else {
        ::tracing::warn!(
            target = %forwarded.target_id,
            port,
            "nothing of this emulator is listening for the target of the forward on the port \
             its registration receives traffic on, so the connection to it cannot be made"
        );
        return Served::status(TARGET_UNREACHABLE_STATUS);
    };
    let mut served = Served {
        status: answered.status,
        content_type: answered.content_type,
        location: None,
        headers: Vec::new(),
        body: answered.body,
        ..Served::default()
    };
    // The target answered, so the pin the forward action mints goes back with its response —
    // the one answer "requests from a client should be routed to the same target group" can be
    // made true of.
    if let Some(field) = forwarded.set_cookie.clone() {
        served.headers.push((SET_COOKIE_HEADER.to_string(), field));
    }
    // The status the client is answered with is the TARGET's own, which is what the record
    // reports as the target's.
    served.observed.target_status = Some(served.status);
    served.observed.target_elapsed = Some(started.elapsed());
    served
}
