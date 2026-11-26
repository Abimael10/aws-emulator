// SPDX-License-Identifier: CC0-1.0

//! The language's CLOCK functions, answered from the calling service's own clock.
//!
//! JSONata defines two functions that read the current time — `$now()`, the instant as
//! an ISO 8601 string, and `$millis()`, that same instant in milliseconds since the Unix
//! epoch — and [`::jsonata_rs`] answers both from the HOST's clock.
//!
//! That is the wrong clock for a service whose own clock is not this host's. An emulated
//! service that SIMULATES the spans its API documents — a wait a caller asked for, the
//! interval between two attempts of a retry — moves a run's clock forward by the span
//! instead of sleeping for it, so the instant that run stands at is ahead of the instant
//! this process is at. An expression such a run evaluates has to read the RUN's clock: a
//! definition that measures elapsed time against the instant its own run started measures
//! the spans that run was charged, and reading the host's clock instead reports every one
//! of them as the microseconds the walk really took.
//!
//! So a caller that HAS such a clock supplies it ([`super::Expression::clock`]) and the
//! two names are bound to it for the one expression being evaluated; a caller that
//! supplies none keeps the engine's own.
//!
//! ## Why they are bound in the LANGUAGE and not on the engine's extension seam
//!
//! The engine binds its own native functions when an evaluation STARTS, after everything
//! a caller registered ([`::jsonata_rs::JsonAta::register_function`]) or assigned
//! ([`::jsonata_rs::JsonAta::assign_var`]) — so a binding of one of its own names made
//! before the evaluation is replaced by it and never reached. The engine offers no seam
//! after that point, and the pinned version answers `$now()` from `Utc::now()` with
//! nothing to shift.
//!
//! What IS left is the language's own binding form: a JSONata BLOCK binds a name in a
//! frame of its own, and a name bound there shadows the binding around it for the rest of
//! the block. So the expression is evaluated inside a block that binds the two names to
//! the instant the caller supplied ([`binding_block`]) — the language's own way to name a
//! value, applied to the caller's whole expression rather than to anything read out of its
//! text. Nothing of that text is inspected, matched or rewritten: it is the body of the
//! block, evaluated exactly as written, and the expression a diagnosis quotes stays the
//! one the caller wrote.

use std::time::{SystemTime, UNIX_EPOCH};

/// `text` evaluated with the language's two clock functions bound to `clock`.
///
/// The body is `text` unchanged, so what the block answers is what the expression
/// answers; the bindings before it are what the two names resolve to inside it. Each is a
/// function of no arguments, as the language defines both, so an expression that CALLS
/// one and an expression that merely names it both read the supplied instant.
pub(super) fn binding_block(text: &str, clock: SystemTime) -> String {
    format!(
        "($now := function(){{\"{}\"}}; $millis := function(){{{}}}; {text})",
        rendered(clock),
        epoch_millis(clock)
    )
}

/// `time` in whole milliseconds since the Unix epoch, saturating at the ends of the
/// range the figure carries rather than panicking: the instant is a caller's, and one
/// outside that range must not be able to end the process.
fn epoch_millis(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_millis()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_millis())
            .map_or(i64::MIN, i64::saturating_neg),
    }
}

/// `time` as the ISO 8601 instant the language words a timestamp as: the UTC calendar
/// instant with a three-digit fraction and the UTC designator, which is the form
/// `$now()` answers in and the form `$toMillis` reads back.
fn rendered(time: SystemTime) -> String {
    let millis = epoch_millis(time);
    ::chrono::DateTime::<::chrono::Utc>::from_timestamp_millis(millis)
        .map(|instant| instant.format(INSTANT_FORMAT).to_string())
        // An instant outside the calendar the maintained implementation covers is
        // reported as the epoch milliseconds it was taken at rather than as nothing.
        .unwrap_or_else(|| millis.to_string())
}

/// The rendering [`rendered`] writes (`%.3f` writes the millisecond fraction
/// zero-padded).
const INSTANT_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";
