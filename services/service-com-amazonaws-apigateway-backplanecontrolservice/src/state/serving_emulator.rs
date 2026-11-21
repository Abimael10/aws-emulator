// SPDX-License-Identifier: CC0-1.0

//! WHAT THIS EMULATOR ACCEPTS, read off a handle onto it: whether it accepts ACCESS KEYS at
//! all, which is what decides whether a request's SigV4 signature is verified before anything
//! is performed for it.
//!
//! A handle onto the emulator a request is served through
//! ([`::smithy_server_runtime::EmbeddedEmulator`]) carries the credentials the router it was
//! built over VENDS, which the router fills from the authenticators registered on it and leaves
//! empty when none is. So the handle answers one question no store can: does this emulator
//! verify the signature a request presents, or does it accept whatever credential arrives?
//!
//! ONE reading of it, because two planes ask it and must not differ: the data plane's own SigV4
//! authentication of an `AWS_IAM` method's caller ([`crate::service::invoke::sigv4_authentication`]),
//! which asks it of the handle the request's own context carries, and the admission of a
//! cross-origin browser request ([`crate::service::invoke::browser_invoke`]), which has to know
//! BEFORE anything is served whether the serving will verify a signature at all — a request
//! nothing would authenticate is not one this emulator can tell from a page.

use ::smithy_server_runtime::EmbeddedEmulator;

/// Whether the emulator `served_through` reaches accepts ACCESS KEYS: one of the credentials it
/// vends is an AWS access key, which is what a SigV4 signature is verified against.
///
/// `false` for an emulator that vends none — one serving with request authentication off, where
/// every credential is accepted as presented and no signature is checked.
#[must_use]
pub fn accepts_access_keys(served_through: &EmbeddedEmulator) -> bool {
    served_through.credentials().iter().any(|credential| {
        matches!(
            credential,
            ::emulator_server::VendedCredentials::AwsAccessKey { .. }
        )
    })
}
