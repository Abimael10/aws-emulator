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
//! An `ip` registration may also name a workload of a service this one cannot ask — a container
//! an Amazon ECS task runs, whose own service registers that task's address in this store and
//! so cannot read this store back. Such a workload's server is recorded by the service that
//! STARTED it, under the address and port it listens at
//! ([`::service_common::served_http_addresses`]), and the answer is that workload's own,
//! computed by its own service from what it holds, exactly as an instance's is computed by
//! Amazon EC2.
//!
//! WHICH server stands behind a registration and what it answers is read through the one
//! reading of it this crate has ([`crate::state::target_servers`]), shared with the periodic
//! HEALTH CHECK of that same registration — so the request a client is answered and the check
//! that decides whether the load balancer sends it there read one target one way.
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
//! [`super::data_plane::TARGET_UNREACHABLE_STATUS`] — a target that is neither an instance of
//! this account nor an address a workload of another service is listening at, an instance whose
//! guest is not up, and one running nothing on the port the registration receives traffic on. No
//! body is invented for it: the load balancer's own error page is not a document this emulator
//! holds, exactly as the sibling statuses this plane answers carry none
//! ([`super::data_plane`]).
//!
//! And so is a registration the security groups governing it admit NO connection to from this
//! load balancer on that port: the connection a `forward` opens is the same connection a health
//! check of the registration opens, so the same firewall decides it
//! ([`crate::state::target_reachability`]) — a request to a target nothing admits reaches no
//! server at all, whatever stands behind the registration, so what the client is answered is
//! the load balancer's own status and never the target's response.

use super::data_plane::{DataPlaneRequest, Served, TARGET_UNREACHABLE_STATUS};

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
    /// The security groups GOVERNING the address this registration names, as the scheduler
    /// running the workload behind it declared them (`crate::state::GuardingGroups`), and
    /// empty for a registration nothing declares them for — whose groups are the ones the
    /// account's own inventory reports for the resource it names.
    pub(super) guarded_by: crate::state::GuardingGroups,
    /// The load balancer the connection comes FROM, as an inbound rule of those groups can
    /// name it ([`crate::state::target_reachability::ConnectionSource`]).
    pub(super) connected_from: crate::state::target_reachability::ConnectionSource,
    /// The `Set-Cookie` field the answer carries, where the forward action pins the client to
    /// the target group this request was distributed to
    /// ([`super::target_group_selection`]). The load balancer's own, so it accompanies the
    /// response the TARGET produced and no status the load balancer answered instead.
    pub(super) set_cookie: Option<String>,
}

/// Serve one request with the workload the target `forwarded` names runs, and answer what that
/// server answered — or the load balancer's own [`TARGET_UNREACHABLE_STATUS`] where the
/// connection cannot be made at all: no security group governing the registration admits it
/// from this load balancer, or this emulator has no server behind the registration on that
/// port.
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
    // Whether this connection can ARRIVE: the security groups governing what the registration
    // names admit it only where an inbound rule of one of them covers this port and names this
    // load balancer ([`crate::state::target_reachability`]). Read before anything behind the
    // target is asked, because a connection nothing admits is answered by nothing — the same
    // reading that fails every health check of such a registration
    // (`crate::state::host_counts`).
    if crate::state::target_reachability::connection_is_blocked(
        engine,
        &forwarded.target_type,
        &forwarded.target_id,
        port,
        &forwarded.guarded_by,
        &forwarded.connected_from,
    ) {
        ::tracing::warn!(
            target = %forwarded.target_id,
            port,
            "no security group governing the target admits a connection from this load \
             balancer on the port its registration receives traffic on, so the connection to \
             it cannot be made"
        );
        return Served::status(TARGET_UNREACHABLE_STATUS);
    }
    // How long the TARGET took, which the access-log record reports for a connection that was
    // made at all.
    let started = ::std::time::Instant::now();
    // The ONE reading of what the server behind a registration answers, shared with the health
    // check of that same registration (`crate::state::target_servers`).
    let answered = crate::state::target_servers::answered(
        engine,
        &forwarded.target_type,
        &forwarded.target_id,
        port,
        &request.method,
        &request.path,
    )
    .map(|answered| answered.answer);
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

