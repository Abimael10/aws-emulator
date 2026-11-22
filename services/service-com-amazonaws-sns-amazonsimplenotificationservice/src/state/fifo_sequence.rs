// SPDX-License-Identifier: CC0-1.0

//! The `SequenceNumber` a publish to a FIFO topic is answered with.
//!
//! The model states the whole form of the member: "The sequence number is a large,
//! non-consecutive number that Amazon SNS assigns to each message. The length of
//! `SequenceNumber` is 128 bits. `SequenceNumber` continues to increase for each
//! `MessageGroupId`" (`PublishResponse$SequenceNumber` doc). Three requirements come out
//! of that sentence, and all three are properties of the VALUE rather than of its digits:
//!
//! * LARGE and 128-bit: a plain decimal of the scale a 128-bit number is written at, so it
//!   carries no leading zero and is nowhere near the first number a counter would hand out
//!   — [`SEQUENCE_NUMBER_FLOOR`] is where the numbers start;
//! * NON-CONSECUTIVE: the number a publish is answered with does not let its caller work
//!   out the number the next publish will get, so each one advances its group by an
//!   unpredictable step inside [`MIN_SEQUENCE_NUMBER_STEP`] ..=
//!   [`MAX_SEQUENCE_NUMBER_STEP`] rather than by one;
//! * PER MESSAGE GROUP: it "continues to increase for each `MessageGroupId`", so each
//!   message group of a topic advances on its OWN run of numbers — a publish to one group
//!   moves that group's number and no other group's.
//!
//! A number is therefore minted from the group's own last number, which is what
//! [`Sequences`] holds, keyed by the topic and the message group the publish named. It is
//! NOT derived from the message: two publishes of one body to one group get two numbers,
//! and nothing a caller sends decides which.
//!
//! Reached through
//! [`super::ComAmazonawsSnsAmazonsimplenotificationserviceState::next_sequence_number`],
//! the one place a publishing operation asks for a sequence number, so `Publish` and
//! `PublishBatch` cannot sequence one topic two ways.

use std::collections::HashMap;

/// The number a message group's run of sequence numbers starts above: 10^19, the scale a
/// 20-digit decimal is written at.
///
/// A 128-bit number is written in up to 39 digits, and SNS's own numbers sit far below
/// that ceiling while being far above the numbers a counter would hand out. Starting here
/// leaves every number this emulator mints a decimal of at least 20 significant digits —
/// no leading zero, and not a value a client could mistake for a position in a sequence.
pub const SEQUENCE_NUMBER_FLOOR: u128 = 10_u128.pow(19);

/// The smallest and the largest step ONE publish advances its message group by.
///
/// The step is drawn inside this window, so a client holding one group's number can bound
/// where the next one lies but cannot name it: the numbers are non-consecutive, which is
/// what the member's documented form asks for. The window is this emulator's choice — the
/// model states only that the numbers are not consecutive — and it is wide enough that the
/// next number is not guessable and narrow enough that a group stays at the scale
/// [`SEQUENCE_NUMBER_FLOOR`] sets it at.
pub const MIN_SEQUENCE_NUMBER_STEP: u128 = 1_000;
/// The upper end of that window — see [`MIN_SEQUENCE_NUMBER_STEP`].
pub const MAX_SEQUENCE_NUMBER_STEP: u128 = 1_000_000;

/// How many message groups this store remembers a sequence number for.
///
/// A caller names the message groups, so the map is caller-grown and needs a ceiling like
/// every other store here ([`super::MAX_RECORDS_PER_STORE`] bounds those). Crossing it
/// costs no refusal: the group furthest behind is forgotten, and a later publish to it
/// starts from the high-water mark — above every number that group was ever given, so its
/// own numbers still only increase.
const MAX_SEQUENCED_GROUPS: usize = 10_000;

/// The `SequenceNumber` run of each message group that has been published to, keyed by the
/// topic and the group.
#[derive(Debug, Default)]
pub struct Sequences {
    /// The last number handed to each message group ([`group_key`]).
    groups: HashMap<String, u128>,
    /// The highest number handed out so far, which is where a group with no number of its
    /// own starts.
    high_water: u128,
    /// The monotone part of the step entropy, so two steps drawn inside one clock tick
    /// still differ.
    minted: u64,
}

impl Sequences {
    /// The next sequence number for the message group `message_group_id` of `topic_arn`:
    /// that group's last number advanced by one unpredictable step, as a plain decimal.
    ///
    /// A group that has none yet starts above the highest number this store has handed out,
    /// which is at least [`SEQUENCE_NUMBER_FLOOR`].
    pub fn next(&mut self, topic_arn: &str, message_group_id: Option<&str>) -> String {
        let key = group_key(topic_arn, message_group_id);
        let step = self.step();
        let from = match self.groups.get(&key) {
            Some(last) => *last,
            None => {
                self.forget_a_group_if_full();
                self.high_water.max(SEQUENCE_NUMBER_FLOOR)
            }
        };
        let minted = from.saturating_add(step);
        self.high_water = self.high_water.max(minted);
        self.groups.insert(key, minted);
        minted.to_string()
    }

    /// One step: a value inside [`MIN_SEQUENCE_NUMBER_STEP`] ..=
    /// [`MAX_SEQUENCE_NUMBER_STEP`] that nothing in the request decides.
    ///
    /// Drawn from the material every other value this service mints is drawn from
    /// ([`super::minted_digest`]: this store's own monotone counter together with the
    /// clock and the process's randomly seeded hasher), so successive publishes advance
    /// by different amounts rather than by one fixed amount.
    fn step(&mut self) -> u128 {
        self.minted = self.minted.saturating_add(1);
        let digest = super::minted_digest(self.minted);
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&digest[..16]);
        let span = MAX_SEQUENCE_NUMBER_STEP - MIN_SEQUENCE_NUMBER_STEP + 1;
        MIN_SEQUENCE_NUMBER_STEP + u128::from_be_bytes(bytes) % span
    }

    /// Make room for a group this store holds no number for, by forgetting the group
    /// furthest behind — the one whose number is lowest, and so the one a restart from the
    /// high-water mark costs the least.
    fn forget_a_group_if_full(&mut self) {
        while self.groups.len() >= MAX_SEQUENCED_GROUPS {
            let Some(furthest_behind) = self
                .groups
                .iter()
                .min_by_key(|(_, last)| **last)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.groups.remove(&furthest_behind);
        }
    }
}

/// The key one message group's run of numbers is held under: the topic, and the group the
/// publish named.
///
/// The separator is a character neither an ARN nor a `MessageGroupId` may carry (a group id
/// is alphanumerics and punctuation), so two different pairs cannot spell one key — the
/// same separation [`super::deduplication_key`] uses.
fn group_key(topic_arn: &str, message_group_id: Option<&str>) -> String {
    format!("{topic_arn}\n{}", message_group_id.unwrap_or_default())
}
