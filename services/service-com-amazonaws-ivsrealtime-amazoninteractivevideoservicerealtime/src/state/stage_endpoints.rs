// SPDX-License-Identifier: CC0-1.0

//! The four ADDRESSES every stage this service reports announces — the event channel a
//! client subscribes to and the three real-time endpoints it publishes on — built here and
//! nowhere else, so every operation that projects a `Stage` announces the same set.
//!
//! All four name THIS emulator, because this emulator is the only thing serving the stage.
//! `StageEndpoints` is "Summary information about various endpoints for a stage. We
//! recommend that you cache these values at stage creation", and each member is an address
//! a client then DIALS — "the endpoint to be used for IVS real-time streaming using the
//! WHIP protocol", the RTMP and RTMPS endpoints beside it, and the events endpoint
//! (`smithy_query(query="shape",shape="StageEndpoints")`). A client does exactly what the
//! member says: it caches the value and connects to it. So the HOST of each is an address
//! of this deployment's that answers the member's own protocol, rather than a name
//! belonging to someone else: the event channel and the WHIP endpoint name the `host[:port]`
//! this deployment answers HTTP on ([`announced_authority`]), the address their client
//! already reached it at.
//!
//! The two CONTRIBUTION endpoints are the exception, and for the same reason: an
//! `rtmp`/`rtmps` value is dialed by an ENCODER speaking RTMP, which the address this
//! emulator answers HTTP on does not serve, so both name the socket this deployment's own
//! contribution plane answers that protocol on ([`super::ingest_plane`], whose
//! `host:port` the core assigns) — the one address a publisher outside this process can
//! complete a handshake against. A deployment that could open no such plane announces
//! neither member: an address nothing answers on is the one thing a member whose whole
//! purpose is to be dialed may not carry.
//!
//! What stays fixed is each member's PROTOCOL: the scheme a client parses out of the value
//! (`wss`, `https`, `rtmp`, `rtmps`) and the path that protocol dials under the host — the
//! event channel and the WHIP endpoint are dialed at the host itself, while an RTMP or
//! RTMPS contribution dials the application path both carry. Nothing of the stage's own
//! identity is in any of them: which stage a connection joins is carried by the
//! participant token the client presents on the WHIP and events endpoints (its audience is
//! the stage's ARN) and by the ingest key an encoder authenticates with on the two
//! contribution endpoints, which is why the model's four endpoints are a summary a client
//! may cache and reuse.
//!
//! All four are therefore composed at PROJECTION time, per request
//! ([`announced_stage_endpoints`]), and none is stored: a stage reached at one address is
//! never answered another address's, and a read composes the values without writing
//! anything.
//!
//! Nothing here is ever CONNECTED to. This module formats strings into a response and holds
//! no outbound client, socket or resolver; every address it composes is one of this
//! process's own inbound addresses — the address the request was answered on, and the
//! socket the core bound for the contribution plane — so no value it reports leads off this
//! machine.

use crate::ops::types::StageEndpoints;

/// The application an RTMP or RTMPS contribution dials under the host, which is the path
/// both of those endpoints carry — the application name an encoder appends its stream key
/// to.
const CONTRIBUTE_APPLICATION_PATH: &str = "/app/";

/// The four endpoints every stage of this deployment announces, as this service reports
/// them to the request in `ctx`, contributed to on the plane open at `contribute`.
///
/// Every operation that answers a `Stage` runs it through this, so the create, the read and
/// the update all announce one set — which is what the model's "cache these values at stage
/// creation" depends on.
///
/// `contribute` is the `host:port` this deployment's contribution plane answers RTMP and
/// RTMPS on ([`super::contribute_authority`]), or `None` for a deployment that could open
/// none — and then the stage announces no contribution address at all, for either member,
/// because an address nothing answers on is the one thing a member whose whole purpose is
/// to be dialed may not carry.
#[must_use]
pub fn announced_stage_endpoints(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    contribute: Option<&str>,
) -> StageEndpoints {
    let authority = announced_authority(ctx);
    StageEndpoints {
        events: Some(format!("wss://{authority}")),
        whip: Some(format!("https://{authority}")),
        rtmp: contribute.map(|plane| format!("rtmp://{plane}{CONTRIBUTE_APPLICATION_PATH}")),
        rtmps: contribute.map(|plane| format!("rtmps://{plane}{CONTRIBUTE_APPLICATION_PATH}")),
    }
}

/// The `host[:port]` every address this service announces is reached at.
///
/// The same reading the sibling IVS plane announces a channel's addresses by, in the same
/// order:
///
/// * a request that addressed the EMULATOR'S OWN endpoint
///   (`service_common::data_plane_addressing::reached_emulator_endpoint_of_parts`) comes
///   from a client that resolves nothing else, so the address is announced on that same
///   host, port included;
/// * a stage read through any other request on an emulator that IS served on a port
///   (`service_common::data_plane_addressing::served_endpoint_authority`) is announced on
///   that port, the one address this process answers on, so a stage reported to an
///   in-process caller of another service names an address its own client can reach;
/// * an emulator served on no port at all has one base and announces it, so the value still
///   names this emulator rather than a host belonging to someone else.
#[must_use]
pub fn announced_authority(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    let base = ctx.endpoint();
    ::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts(ctx.parts())
        .or_else(|| {
            ::service_common::data_plane_addressing::served_endpoint_authority(base.as_str())
        })
        .unwrap_or_else(|| {
            ::service_common::data_plane_addressing::url_authority(base.as_str())
                .trim_end_matches('/')
                .to_string()
        })
}
