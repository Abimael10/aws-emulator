// SPDX-License-Identifier: CC0-1.0

//! How a read of ONE bot names the error shape of a bot it cannot report: it does not.
//!
//! A `GetBot` whose `botId` names no bot of the `networkId` it was asked under is refused with
//! the status the model declares for `ResourceNotFoundError` and this API's fixed gone-bot
//! sentence ([`crate::state::BOT_GONE_MESSAGE`]) — and with NO error-shape discriminator,
//! neither the `x-amzn-errortype` header `restJson1` resolves an error shape from nor a
//! `__type` / `code` / `Code` member in the body, so a client classifies it by status.
//!
//! This module states that ONE fact of this API's wire: the status that refusal travels at.
//! The wire form itself — dropping the shape from both places, and nothing else — is
//! [`crate::service::untyped_refusal`], and which operation it fronts is stated in
//! [`crate::service`]. `GetBot`'s handler answers this status for exactly one reason: the bot
//! the request named is not there ([`crate::ops::get_bot`]).

/// The status the gone-bot refusal carries: the one the model declares for
/// `ResourceNotFoundError` (`@httpError(404)`), which is the only reason a bot read answers
/// it.
pub const GONE_BOT_STATUS: u16 = 404;
