// SPDX-License-Identifier: CC0-1.0

//! WHICH EMULATOR an invoke's integration is performed against: the address this process
//! answers ITSELF on, and the handle that reaches nothing.
//!
//! Every modelled operation is run with an [`::smithy_server_runtime::OperationContext`]
//! carrying a handle into the emulator it is running within, and that handle is what an
//! integration's call to another emulated service is performed over
//! ([`super::aws_integration`]) and what the reading of which credentials this emulator accepts
//! asks ([`super::sigv4_authentication`]). One seam of this service is handed NO context: the
//! routing seam that answers the invoke a browser sent from another site
//! ([`super::browser_invoke`]), which the front door refuses after an operation is named for it
//! and before it is run, so this service can answer it only by naming no operation at all.
//!
//! So the handle such an invoke is performed over is the one the DEPLOYMENT its stage serves
//! carries ([`crate::state::DeploymentEntry::reached_emulator`]), recorded by the call that
//! created that deployment — a deployment is what makes a stage invocable, so the address its
//! invokes are performed over belongs with it. It is the same arrangement a custom domain
//! name's own socket is opened under ([`crate::state::custom_domain_endpoint`]): a second
//! ADDRESS of one emulator, not a second emulator.
//!
//! The handle names the EMULATOR, never a caller. A
//! [`::smithy_server_runtime::EmbeddedEmulator`] is built by the front door from the one router
//! it serves (`EmbeddedEmulator::from_router`, over a router clone every request of this
//! process shares) plus the credentials that router's own AUTHENTICATORS vend — the keys this
//! process minted, which every handler of this emulator already signs an in-process hop with —
//! and a recursion depth. No header, body, credential or identity of the request that carried
//! it is in it, and its debug form discloses nothing.

use ::smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

/// The address this emulator answers itself on when a deployment carries one, else
/// [`contained`].
///
/// A deployment carrying none is one created where no such address could be read, which leaves
/// the invoke resolved and answered from the store exactly as before and an integration naming
/// a backend reported as one that could not be performed.
pub(super) fn reached_or_contained(reached: Option<EmbeddedEmulator>) -> EmbeddedEmulator {
    reached.unwrap_or_else(contained)
}

/// A handle onto an emulator with NO service mounted, so a call made over it reaches nothing at
/// all.
///
/// The context every step of a serving reads carries a transport, and the step that would use
/// one is the integration: this is what withholds exactly those from an invoke that may not
/// perform one, while the resolution, the authorization the api's own records decide, a `MOCK`
/// integration and every header mapping are served from the store as they are for any other
/// caller. Built per call rather than remembered, so it holds nothing of anything.
pub(super) fn contained() -> EmbeddedEmulator {
    EmbeddedEmulator::from_router(MultiServiceRouter::new())
}
