// SPDX-License-Identifier: CC0-1.0

//! What the SERVER behind one REGISTERED TARGET answers, and whether its answer is one this
//! emulator can judge the target by.
//!
//! `TargetGroup$TargetType` says what a registration names — "instance - Targets are specified
//! by instance ID", "ip - Targets are specified by IP address" — and `TargetDescription$Port`
//! where the workload behind it is reached: "the port on which the target is listening". What
//! answers there is the server that workload RUNS:
//!
//! * a workload another emulated service's SCHEDULER started records its own server under the
//!   address and port it listens at (`::service_common::served_http_addresses`) — a container
//!   an Amazon ECS task runs, whose own service cannot be asked directly because it already
//!   reads this store to register its targets;
//! * an instance of this account runs what its own user data installed and started, which its
//!   guest serves out of its own storage
//!   (`service_com_amazonaws_ec2_amazonec2::state::guest_services`).
//!
//! This is the ONE reading of that server this crate has, and both halves of the load balancer
//! take it: the `forward` of a client's request to such a target is answered by it
//! ([`crate::service::instance_target`]), and so is each periodic HEALTH CHECK of the
//! registration ([`super::host_counts`]) — so what one target answers cannot be read two ways.
//!
//! Every read here is made with NO guard of this store held and changes nothing: asking a
//! server for a document moves no status of the target, consumes no quota and bumps no counter.
//!
//! # What the answer is worth
//!
//! An answer also carries whether the server computed it from documents this emulator HOLDS
//! ([`TargetAnswer::holds_what_it_serves`]). A server over a directory holding none of them
//! would serve what the image or package that shipped it put there, which the service running
//! the workload models nothing of — so such an answer is no statement about the workload, and a
//! reader that judges the TARGET by the status it was answered with (a health check against its
//! group's own success criterion) reads no verdict into it.

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;
use ::service_common::served_http_addresses::{AddressAnswer, ServedAddresses};

/// The `TargetType` whose registrations name an INSTANCE of the account, and the one whose
/// registrations name an ADDRESS (`crate::ops::types::TARGET_TYPE_ENUM_VALUES` carries the whole
/// catalog).
pub const INSTANCE_TARGET_TYPE: &str = "instance";
pub const IP_TARGET_TYPE: &str = "ip";

/// What the server behind one registration answered.
#[derive(Debug, Clone)]
pub struct TargetAnswer {
    /// The status, media type and bytes that server answered with.
    pub answer: AddressAnswer,
    /// Whether it computed them from documents this emulator holds — see this module's header.
    pub holds_what_it_serves: bool,
}

/// What the workload the registration `target_id` of a `target_type` group names answers for a
/// request for `path` with `method` on `port` — `None` when nothing of this emulator is
/// listening there, which is a connection that cannot be made rather than a response.
///
/// The published record is read first and the account's own instances after it: a workload
/// another service started at that address is what answers there, and an instance holding the
/// address answers for one nothing published a server for.
#[must_use]
pub fn answered<Eng: Send + Sync + 'static>(
    engine: &Eng,
    target_type: &str,
    target_id: &str,
    port: i32,
    method: &str,
    path: &str,
) -> Option<TargetAnswer> {
    published_workload(engine, target_type, target_id, port, method, path)
        .or_else(|| instance_guest(engine, target_type, target_id, port, method, path))
}

/// What a workload ANOTHER service started a server for answers — the server recorded at the
/// address the registration names, on the port it receives traffic on
/// ([`::service_common::served_http_addresses`]).
///
/// Only an `ip` registration names such an address: every other target type names a resource by
/// its own id, which is no address a workload listens at. `None` when nothing is recorded there,
/// which leaves the instance of this account holding the address to answer instead.
fn published_workload<Eng: Send + Sync + 'static>(
    engine: &Eng,
    target_type: &str,
    target_id: &str,
    port: i32,
    method: &str,
    path: &str,
) -> Option<TargetAnswer> {
    if target_type != IP_TARGET_TYPE {
        return None;
    }
    let (answer, holds_what_it_serves) =
        ServedAddresses::of(engine)?.served(target_id, port, method, path)?;
    Some(TargetAnswer {
        answer,
        holds_what_it_serves,
    })
}

/// What the GUEST of the instance the registration names answers, read out of Amazon EC2's own
/// store under its own guard.
///
/// The address of an `ip` registration is resolved to the instance whose interface holds it, and
/// an `instance` registration names that instance outright. `None` for a registration no
/// instance of this account stands behind, one whose guest is not up, and one whose guest is
/// listening on no such port.
///
/// Such a guest HOLDS what it serves: what an instance of this account runs is what its own user
/// data installed and started, and the documents under the root it serves are the ones that same
/// program wrote — the whole account of the workload, which this emulator models.
fn instance_guest<Eng: Send + Sync + 'static>(
    engine: &Eng,
    target_type: &str,
    target_id: &str,
    port: i32,
    method: &str,
    path: &str,
) -> Option<TargetAnswer> {
    // Nothing of this emulator reports what the account's instances run, so there is no server
    // behind the registration to connect to.
    let state = ec2::service_state(engine)?;
    let store = state.lock();
    let instance_id = match target_type {
        // "if the target type is instance, specify an instance ID" (`TargetDescription$Id`).
        INSTANCE_TARGET_TYPE => Some(target_id.to_string()),
        // "if the target type is ip, specify an IP address": the workload at that address is the
        // one the interface holding it belongs to.
        IP_TARGET_TYPE => store.instance_id_at_address(target_id),
        // Every other target type is one this reading does not name a server for: a `lambda`
        // target is invoked ([`crate::service::lambda_target`]) and an `alb` one is passed on
        // ([`crate::service::data_plane`]).
        _ => None,
    }?;
    let answered = store.serve_instance_guest_http(&instance_id, port, method, path)?;
    Some(TargetAnswer {
        answer: AddressAnswer {
            status: answered.status,
            content_type: answered.content_type,
            body: answered.body,
        },
        holds_what_it_serves: true,
    })
}
