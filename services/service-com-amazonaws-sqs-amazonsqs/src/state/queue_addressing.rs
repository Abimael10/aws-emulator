// SPDX-License-Identifier: CC0-1.0

//! How a queue is ADDRESSED: the url this service reports for one, and what a value
//! supplied where a `QueueUrl` is expected addresses.
//!
//! The two halves are one subject read in opposite directions, so they live together.
//!
//! # The url a call reports
//!
//! A `QueueUrl` is the only handle every queue-addressed operation takes, and a client is
//! expected to send the string it was handed straight back — so whatever endpoint that
//! string names has to be an endpoint this emulator ANSWERS on. The authority therefore
//! comes from where the call reached this process ([`reported_endpoint_authority`]) and
//! only the `/<accountId>/<QueueName>` path comes from the store, which is the same
//! reading every other address this emulator announces for a client to reach is chosen by.
//!
//! # The queue a value addresses
//!
//! Read the other way round, the `/<accountId>/<QueueName>` path is what names the queue:
//! a value supplied for a `QueueUrl` is resolved by that pair and not by its authority
//! ([`queue_address`]), so a client reaches its queue through whichever endpoint of this
//! emulator it holds a url for. A value that carries no such pair is no address of this
//! endpoint at all, which is a different fault from an address whose queue is absent, and
//! the two are reported as the two different modelled errors every one of those operations
//! declares ([`unresolved_queue_address`]).

use crate::ops::types::{InvalidAddress, QueueDoesNotExist};

/// The `host[:port]` a queue url this call reports must name — the endpoint the caller is
/// already talking to, so the url it is handed leads back here.
///
/// Chosen the way every other endpoint this emulator announces for a client to reach is:
///
/// * the authority the request REACHED this emulator at, for a client that resolves
///   nothing else — which is what a client pointed at this emulator's own endpoint is;
/// * else the authority this process is SERVED at, so a url minted on an in-process call
///   of another service still names an address that service's own caller can reach;
/// * else Amazon SQS's own endpoint host for the region the call was made in, the form AWS
///   publishes its queue urls at, for a caller that resolves AWS's own names.
///
/// Never a host this emulator does not answer on: a client that stored such a url and sent
/// it back would be addressing production AWS with the credentials it is holding, and would
/// never reach the queue it was told about.
pub fn reported_endpoint_authority(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> String {
    ::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts(ctx.parts())
        .or_else(|| {
            ::service_common::data_plane_addressing::served_endpoint_authority(
                ctx.endpoint().as_str(),
            )
        })
        .unwrap_or_else(|| {
            ::service_common::data_plane_addressing::service_endpoint_host(
                super::ARN_SERVICE,
                region,
            )
        })
}

/// The url THIS call reports for the queue called `name`: the endpoint it reached
/// ([`reported_endpoint_authority`]) followed by the queue's own
/// `/<accountId>/<QueueName>` path.
///
/// The ONE builder every operation that hands a caller a queue url goes through
/// (`CreateQueue`, `GetQueueUrl`, `ListQueues`, `ListDeadLetterSourceQueues`), so no two of
/// them can report a queue at different addresses.
pub fn reported_queue_url(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> String {
    queue_url_at(
        &reported_endpoint_authority(ctx, region),
        ::engine::emulator_account_id(),
        name,
    )
}

/// A queue address written out: `https://<authority>/<accountId>/<QueueName>`.
///
/// The FORM of every queue url, wherever the authority came from — so the path a reader
/// resolves a queue by ([`queue_address`]) is composed in one place.
pub fn queue_url_at(authority: &str, account: &str, name: &str) -> String {
    format!("https://{authority}/{account}/{name}")
}

/// What a value supplied where a `QueueUrl` is expected addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueAddress<'a> {
    /// The queue called this, of the account this endpoint answers for — in whichever
    /// region the call was made in ([`super::ComAmazonawsSqsAmazonsqsState::queue_by_url`]).
    OfThisAccount(&'a str),
    /// A queue address of ANOTHER account: a well-formed address, naming a queue that is
    /// not this endpoint's to hold.
    OfAnotherAccount,
    /// No queue address for this endpoint at all: the value carries no
    /// `<accountId>/<QueueName>` pair and is not a bare queue name either, so there is no
    /// queue for it to name.
    NotForThisEndpoint,
}

/// The queue `value` addresses, read off the `(account, QueueName)` pair it carries.
///
/// A queue address names its queue by that pair: the name is the last path segment, and the
/// account is the segment BEFORE it. Reading the pair rather than the whole string is what
/// lets a client reach its queue through any endpoint this emulator answers on — the url it
/// was handed, or one built for another host name — and is why the authority decides
/// nothing here.
///
/// A value carrying no path at all is a bare `QueueName`, which addresses the caller's own
/// queue of that name: RECORDED — a `GetQueueAttributes` whose `QueueUrl` is the queue's
/// name alone is served that queue's attributes, not refused.
///
/// Everything else carries no pair, so it addresses no queue of any account and is not an
/// address this endpoint serves: a url whose path is empty (an endpoint root), a url whose
/// path names only one segment (the segment before the name is then not an account id), and
/// an empty value. A trailing `/` is not part of the name.
pub fn queue_address(value: &str) -> QueueAddress<'_> {
    let trimmed = value.trim_end_matches('/');
    let Some((prefix, name)) = trimmed.rsplit_once('/') else {
        // No separator at all: a bare `QueueName`, or nothing.
        return match trimmed.is_empty() {
            true => QueueAddress::NotForThisEndpoint,
            false => QueueAddress::OfThisAccount(trimmed),
        };
    };
    // `<...>/<accountId>/<QueueName>`, or `<accountId>/<QueueName>` with no endpoint
    // before it: the segment ahead of the name has to be an account id either way.
    let account = prefix.rsplit_once('/').map_or(prefix, |(_, account)| account);
    if name.is_empty() || !addresses_an_account(account) {
        return QueueAddress::NotForThisEndpoint;
    }
    match account == ::engine::emulator_account_id() {
        true => QueueAddress::OfThisAccount(name),
        false => QueueAddress::OfAnotherAccount,
    }
}

/// Whether `segment` is shaped like the account id a queue address carries: an Amazon Web
/// Services account number ([`super::ACCOUNT_NUMBER_DIGITS`] digits), which is the only
/// thing the segment ahead of the queue name can be.
fn addresses_an_account(segment: &str) -> bool {
    segment.len() == super::ACCOUNT_NUMBER_DIGITS
        && segment.chars().all(|character| character.is_ascii_digit())
}

/// Which modelled refusal a `QueueUrl` that resolved no queue earns.
///
/// The two faults are not the same, and a client recovers from them differently: an address
/// this endpoint does not serve is the caller's url to fix or re-address, while an address
/// whose queue is absent is a queue to create or stop addressing. Keying retries or
/// create-on-absence logic on one when the service means the other is what makes a bad url
/// look like a missing queue forever.
#[derive(Debug)]
pub enum UnresolvedQueueAddress {
    /// The value is no queue address of this endpoint ([`QueueAddress::NotForThisEndpoint`]).
    NotForThisEndpoint(InvalidAddress),
    /// A well-formed address whose queue this region does not hold.
    NoSuchQueue(QueueDoesNotExist),
}

impl UnresolvedQueueAddress {
    /// This refusal as the operation's own error, named by the two variants every
    /// queue-addressed operation's model declares for these faults.
    pub fn into_error<E>(
        self,
        not_valid_for_this_endpoint: fn(InvalidAddress) -> E,
        no_such_queue: fn(QueueDoesNotExist) -> E,
    ) -> E {
        match self {
            Self::NotForThisEndpoint(error) => not_valid_for_this_endpoint(error),
            Self::NoSuchQueue(error) => no_such_queue(error),
        }
    }
}

/// How a `QueueUrl` that resolved no queue is refused: `InvalidAddress` for a value that is
/// no address of this endpoint, and `QueueDoesNotExist` for an address whose queue is
/// absent.
///
/// The ONE classification every operation that resolves a `QueueUrl` refuses through, so
/// the fault a caller is told about is the one its value has.
pub fn unresolved_queue_address(
    value: &str,
    parts: &::http::request::Parts,
) -> UnresolvedQueueAddress {
    match queue_address(value) {
        QueueAddress::NotForThisEndpoint => {
            UnresolvedQueueAddress::NotForThisEndpoint(invalid_queue_address(value))
        }
        QueueAddress::OfThisAccount(_) | QueueAddress::OfAnotherAccount => {
            UnresolvedQueueAddress::NoSuchQueue(super::queue_does_not_exist(parts))
        }
    }
}

/// The modelled refusal of a value that is no queue address of this endpoint, worded as
/// Amazon SQS words it ([`super::invalid_address_for_endpoint_message`]).
///
/// The one variable part is the ADDRESS that value names — [`addressed_endpoint`], the
/// endpoint it points at rather than the whole string: what the caller has to fix or
/// re-address is where it is pointing, and the path it carried is precisely the part that
/// named no queue.
pub fn invalid_queue_address(value: &str) -> InvalidAddress {
    InvalidAddress {
        message: Some(super::invalid_address_for_endpoint_message(
            &addressed_endpoint(value),
        )),
    }
}

/// The address a value supplied for a `QueueUrl` NAMES: the endpoint it points at — its
/// scheme and the authority the shared reading of a url reports for it
/// ([`::service_common::data_plane_addressing::url_authority`]), followed by the `/` that
/// begins the path — or the value itself when it names no endpoint at all (a bare name, or
/// a path with nothing before it).
///
/// RECORDED: a `GetQueueAttributes` whose `QueueUrl` carries a path that names no queue —
/// two segments whose first is not an account id — is refused naming the url's ENDPOINT,
/// with the path it carried dropped. So what a refusal names is WHERE the value points,
/// which is what the caller has to fix or re-address, and not the string it supplied: the
/// path is the part that named nothing.
pub fn addressed_endpoint(value: &str) -> String {
    let Some((scheme, _)) = value.split_once("://") else {
        return value.to_string();
    };
    // The shared reading drops the scheme; what it leaves is the authority followed by the
    // path, and the path is what this drops.
    let after_scheme = ::service_common::data_plane_addressing::url_authority(value);
    let authority = after_scheme
        .split('/')
        .next()
        .unwrap_or(after_scheme);
    format!("{scheme}://{authority}/")
}
