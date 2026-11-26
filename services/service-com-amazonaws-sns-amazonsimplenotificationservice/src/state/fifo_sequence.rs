// SPDX-License-Identifier: CC0-1.0

//! The `SequenceNumber` a publish to a FIFO topic is answered with.
//!
//! The model states the form of the member: "The sequence number is a large,
//! non-consecutive number that Amazon SNS assigns to each message. The length of
//! `SequenceNumber` is 128 bits. `SequenceNumber` continues to increase for each
//! `MessageGroupId`" (`PublishResponse$SequenceNumber` doc). Four requirements come out of
//! that sentence, and all of them are properties of the VALUE rather than of its digits:
//!
//! * LARGE and 128-bit: a plain decimal of the scale a 128-bit number is written at, so it
//!   carries no leading zero and is nowhere near the first number a counter would hand out
//!   — [`SEQUENCE_NUMBER_FLOOR`] is where the run starts;
//! * PER MESSAGE: a number is "assign[ed] to each message", so no two messages the service
//!   sequences are given the same one;
//! * NON-CONSECUTIVE: the run does not advance by one — it advances by the service's own
//!   unit, [`SEQUENCE_NUMBER_STEP`], once per message;
//! * INCREASING WITHIN A MESSAGE GROUP: it "continues to increase for each
//!   `MessageGroupId`", which holds of every group at once because the run only ever moves
//!   forward: a publish to one group leaves the number every other group was given exactly
//!   where it was.
//!
//! ONE run per service, then, advanced once per message sequenced — which is what the
//! recorded behaviour shows: the FIRST message a freshly created FIFO topic took was
//! answered the floor advanced by a single unit, so a run is not restarted per topic or
//! per message group and the unit is not the one a counter would use. The number a publish
//! is answered with is therefore decided by the messages the service sequenced before it,
//! which makes it a function of the request sequence: replaying one sequence of publishes
//! answers one sequence of numbers, as against the real service. Nothing about the clock,
//! the process or the request's own values enters it, and nothing a caller sends decides
//! which number it gets — two publishes of one body to one group get two numbers.
//!
//! Reached through
//! [`super::ComAmazonawsSnsAmazonsimplenotificationserviceState::next_sequence_number`],
//! the one place a publishing operation asks for a sequence number, so `Publish` and
//! `PublishBatch` cannot sequence one topic two ways.

/// The number a run of sequence numbers starts above: 10^19, the scale a 20-digit decimal
/// is written at.
///
/// A 128-bit number is written in up to 39 digits, and SNS's own numbers sit far below
/// that ceiling while being far above the numbers a counter would hand out. Starting here
/// leaves every number this emulator mints a decimal of at least 20 significant digits —
/// no leading zero, and not a value a client could mistake for a position in a sequence.
pub const SEQUENCE_NUMBER_FLOOR: u128 = 10_u128.pow(19);

/// The unit the run advances by for each message sequenced.
///
/// The numbers are non-consecutive — they move by a whole unit rather than by one — and
/// the unit is FIXED, which is what makes the Nth number of the run the same number
/// whichever process hands it out. The model leaves the unit open, so it is read off the
/// recorded behaviour: the first message a freshly created FIFO topic was answered for sat
/// this far above [`SEQUENCE_NUMBER_FLOOR`].
pub const SEQUENCE_NUMBER_STEP: u128 = 3_000;

/// The run of `SequenceNumber`s this service hands out, in the order the messages were
/// sequenced.
///
/// One number per message and nothing else held: a message group needs no record of its
/// own, because the run only moves forward and so every group's numbers increase along it.
/// That also leaves nothing here for a caller to grow — the message groups are named by
/// the caller, and none of them costs this store a record.
#[derive(Debug, Default)]
pub struct Sequences {
    /// The last number handed out, or zero while the run has not started.
    last: u128,
}

impl Sequences {
    /// The next number of the run: the last one advanced by [`SEQUENCE_NUMBER_STEP`], as a
    /// plain decimal.
    ///
    /// The first is [`SEQUENCE_NUMBER_FLOOR`] advanced by that same unit, so the Nth
    /// message a service sequences is answered the floor plus N units.
    pub fn next_number(&mut self) -> String {
        self.last = self
            .last
            .max(SEQUENCE_NUMBER_FLOOR)
            .saturating_add(SEQUENCE_NUMBER_STEP);
        self.last.to_string()
    }
}
