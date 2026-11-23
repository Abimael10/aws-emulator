// SPDX-License-Identifier: CC0-1.0

//! The CONNECTION a Classic load balancer makes to the registered instance it selected for a
//! request, and what the server behind that instance answers.
//!
//! A listener states where that connection goes: `Listener$InstancePort` is "the port on which
//! the instance is listening" and `Listener$InstanceProtocol` "the protocol to use for routing
//! traffic to instances", so a request the load balancer selected an instance for is sent to
//! THAT port of THAT instance — and what answers there is the server the instance RUNS, which
//! an instance of this account runs because its own user data installed and started it
//! (`service_com_amazonaws_ec2_amazonec2::state::guest_services`). The client is then answered
//! with the instance's own status, media type and bytes: a load balancer relays what its
//! backend served, a status that backend itself denies the request with included.
//!
//! This is the SAME reading of that server the periodic health check of the registration takes
//! (`crate::state::observe_instances`, which asks for the ping path of the configured
//! `HealthCheck$Target` on the target's own port), so what a request is answered with and what
//! decides whether the load balancer sends it there cannot be read two ways.
//!
//! The read is made with NO guard of this service's store held and changes nothing: asking an
//! instance's server for a document moves no status of the registration, consumes no quota and
//! bumps no counter. A registration nothing of this emulator is listening behind on that port
//! is a connection the load balancer cannot establish, which is the caller's
//! [`super::data_plane::INSTANCE_UNREACHABLE_STATUS`] — and no body is invented for it.

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

/// What the server behind one registered instance answered: the status line, the media type of
/// the document it served where it served one, and that document's bytes.
#[derive(Debug, Clone)]
pub struct BackendAnswer {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// What the instance `instance_id` names answers for a request for `path` with `method` on
/// `instance_port` — `None` when nothing of this emulator is listening there, which is a
/// connection that cannot be made rather than a response.
#[must_use]
pub fn answered<Eng: Send + Sync + 'static>(
    engine: &Eng,
    instance_id: &str,
    instance_port: i32,
    method: &str,
    path: &str,
) -> Option<BackendAnswer> {
    // Nothing of this emulator reports what the account's instances run, so there is no server
    // behind the registration to connect to at all.
    let inventory = ec2::service_state(engine)?;
    let store = inventory.lock();
    let answered = store.serve_instance_guest_http(instance_id, instance_port, method, path)?;
    Some(BackendAnswer {
        status: answered.status,
        content_type: answered.content_type,
        body: answered.body,
    })
}
