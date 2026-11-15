// SPDX-License-Identifier: CC0-1.0

//! Who is naming the tag keys one request carries.
//!
//! The namespace [`super::RESERVED_TAG_PREFIX`] names belongs to AWS: a caller may neither
//! create, modify nor remove a tag whose key sits in it, and this service words that refusal
//! on each of its tag-writing calls ([`super::system_tag_message`]). But the tags in that
//! namespace EXIST — AWS's own services write them, through no public API at all, which is
//! how a secret a CloudFormation stack provisioned carries the three records that service
//! keeps about every resource it provisions. So the reservation is a rule about WHO states
//! the key, not about the key alone, and the one thing a tag write needs to know beside the
//! tags themselves is which of the two it is.
//!
//! In this emulator one service reaches another ONLY through the in-process callback
//! ([`::smithy_server_runtime::OperationContext::callback`]), so that callback is the inside
//! path, and the signal is the [`::smithy_server_runtime::CallbackRecursionDepth`] request
//! EXTENSION the framework's callback transport stamps on every in-process call. An extension
//! never travels over the wire: nothing a client can send — on any socket, signed with any
//! credential — sets it, so a client's reserved key is refused exactly as before. It is the
//! same framework signal other service crates read to tell an in-process call from a client's
//! request, so nothing about the transport is reconstructed here.
//!
//! The allowance is on the PATH and not on which key is named: this service cannot tell which
//! service is calling, so a service that takes tags from its own caller and forwards them has
//! to drop the reserved ones itself. It is a different question from
//! [`super::service_owned`], which asks whose secret the call is MAKING and whose it may
//! change: a request may be another service's own provisioning call without that service
//! owning — or claiming to own — the secret it tags.

/// Which path a tag write arrived on, which is what decides whether a key in the reserved
/// `aws:` namespace may be stored ([`reserved_key_refused`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKeyOrigin {
    /// A request a CLIENT made, arriving from a socket. A key in the reserved namespace is
    /// AWS's own, so such a request may not write one.
    Caller,
    /// A request another emulated service made in-process, while provisioning or managing a
    /// resource of its own — the path the tags AWS sets from inside itself arrive on, which no
    /// client's request can be. A key in the reserved namespace travels on it.
    Service,
}

/// Where the tag keys this request names came from: see this module's own documentation for
/// why the extension is the signal and why a client cannot forge it.
#[must_use]
pub fn tag_key_origin(ctx: &::smithy_server_runtime::OperationContext<'_>) -> TagKeyOrigin {
    match ctx
        .parts()
        .extensions
        .get::<::smithy_server_runtime::CallbackRecursionDepth>()
    {
        Some(_) => TagKeyOrigin::Service,
        None => TagKeyOrigin::Caller,
    }
}

/// The first key in `keys` this request may not name, for a request from `origin` — `None`
/// when every key is one the request may write or remove.
///
/// The one reading of the reserved-namespace rule for every tag path of this service, so the
/// create, the attach and the detach cannot come to read it differently.
pub fn reserved_key_refused<'a, I>(keys: I, origin: TagKeyOrigin) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    if origin == TagKeyOrigin::Service {
        return None;
    }
    keys.into_iter()
        .find(|key| key.starts_with(super::RESERVED_TAG_PREFIX))
}
