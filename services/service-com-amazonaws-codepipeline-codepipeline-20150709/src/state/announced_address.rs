// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS this service announces a URL on: the one endpoint a client of this
//! emulator can resolve and dial.
//!
//! # The rule this module implements
//!
//! A URL this service hands a caller to FOLLOW — the trigger URL a webhook publishes for a
//! provider to POST to ([`crate::service::webhook_trigger`]), and the deep link an action's
//! state carries for the resource the action acted on
//! ([`crate::state::action_urls`]) — is published on the
//! `host[:port]` the request that minted it reached this emulator at. That address is the
//! only one a client on the machine this process runs on resolves, so a URL announced
//! anywhere else is one no caller of this emulator can reach.
//!
//! It is not the reading for the text of a modelled member this service PUBLISHES about
//! another system: an action type's `entityUrlTemplate` is "[t]he URL returned to the
//! CodePipeline console that provides a deep link to the resources of the external system",
//! so the catalog reports a template as that system publishes it
//! ([`crate::state::built_in_catalog`]) and only the link RESOLVED from it, which a caller
//! follows, is announced here.
//!
//! The reading is `service_common::data_plane_addressing`'s, in the order every data plane
//! served at the front door announces an address in, so this service cannot drift from the
//! others on which address that is:
//!
//! * the EMULATOR'S OWN endpoint the minting request addressed
//!   (`reached_emulator_endpoint`) comes from a client that resolves nothing else;
//! * a URL minted through any other request on an emulator that IS served on a port
//!   (`served_endpoint_authority`) is published on that port, the one address this process
//!   answers on, so a URL minted by an in-process call of another service names an address
//!   that service's own caller can reach too;
//! * an emulator served on no port at all has one base to name, the one
//!   `OperationContext::endpoint` reports for it — a host nothing resolves, which is the
//!   truth about an emulator serving no port, rather than a name of AWS's.
//!
//! Nothing here opens a connection or resolves a name: both functions read request parts
//! and return a string.

use ::service_common::data_plane_addressing::{
    reached_emulator_endpoint, served_endpoint_authority, url_authority,
};
use ::smithy_server_runtime::OperationContext;

/// The `host[:port]` this emulator is reached at, for the request `ctx` is answering.
///
/// See the module note for the order the three readings are taken in. It is an authority
/// rather than a URL, because what a publisher appends to it is the whole of its own URL
/// (the scheme included: every listener of this process presents the core's certificate,
/// so an address it publishes is an HTTPS one whatever the endpoint base is written as).
#[must_use]
pub fn announced_authority(ctx: &OperationContext<'_>) -> String {
    let base = ctx.endpoint();
    reached_emulator_endpoint(
        &ctx.parts().uri.to_string(),
        ctx.parts()
            .headers
            .get(::http::header::HOST)
            .and_then(|host| host.to_str().ok()),
    )
    .or_else(|| served_endpoint_authority(base.as_str()))
    .unwrap_or_else(|| url_authority(base.as_str()).trim_end_matches('/').to_string())
}
