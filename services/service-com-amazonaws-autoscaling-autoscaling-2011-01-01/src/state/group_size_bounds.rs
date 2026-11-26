// SPDX-License-Identifier: CC0-1.0

//! The refusal a request that MOVES named instances of a group states when the group's own
//! sizes do not admit the move.
//!
//! # The rule this module implements
//!
//! An Auto Scaling group's desired capacity stays inside the two sizes it is configured with
//! — `MinSize` and `MaxSize`, "the minimum size of the group" and "the maximum size of the
//! group" (`smithy_query(query="shape",shape="AutoScalingGroup")`) — so a request that names
//! instances to bring INTO a group, or to park out of service while the capacity follows
//! them down, is refused whole when the capacity it asks for falls outside those sizes.
//!
//! The refusal is about the group's CONFIGURATION rather than about any one instance, so it
//! states the three sizes the group holds now and asks the caller to change them, naming the
//! move it was asked for and how many instances it named. The clause naming the move
//! ([`GroupSizeMove`]) is the only thing that differs between the operations that answer it,
//! which is why the sentence is written once here: `AttachInstances`, `EnterStandby` and the
//! release `DetachInstances` performs state one template, so none of them can drift into a
//! wording of its own.
//!
//! `TerminateInstanceInAutoScalingGroup` crosses the same floor but states a sentence of its
//! own class ([`super::min_size_breach_message`]), about the decrement rather than about the
//! group's configuration, so it is not worded here.

/// Which move a request asked for, which is the clause the refusal names it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupSizeMove {
    /// `AttachInstances`: bring the account's own instances into the group.
    Attach,
    /// `EnterStandby`: park instances of the group out of service.
    Standby,
    /// `DetachInstances`: release instances of the group while the capacity follows them
    /// down, which is the move the request's own decrement asked for.
    Detach,
}

impl GroupSizeMove {
    /// The clause the refusal states this move in, which the number of instances the request
    /// named follows.
    #[must_use]
    fn clause(self) -> &'static str {
        match self {
            Self::Attach => "To attach",
            Self::Standby => "To place into standby",
            Self::Detach => "To detach",
        }
    }
}

/// The three sizes a group holds at the moment it refused the move: what the caller has to
/// change for the same request to be admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupSizes {
    /// The group's own `MinSize`.
    pub min_size: i32,
    /// The group's own `MaxSize`.
    pub max_size: i32,
    /// The desired capacity it holds now.
    pub desired_capacity: i32,
}

/// The sentence a move the group's sizes do not admit is refused with.
///
/// THE ONE COPY of that sentence. Fixed text, with four variable parts read off the request
/// and the group as it stands: the group the request named, the three sizes it is configured
/// with now, and how many instances the request asked to move. The noun after that count is
/// part of the fixed text, as is the remedy the sentence closes with — the caller is asked to
/// update the group's sizes, because that is what admits the move.
#[must_use]
pub fn group_sizes_refuse_move_message(
    group_name: &str,
    sizes: GroupSizes,
    move_kind: GroupSizeMove,
    instances: usize,
) -> String {
    let GroupSizes {
        min_size,
        max_size,
        desired_capacity,
    } = sizes;
    format!(
        "AutoScalingGroup {group_name} has min-size={min_size}, max-size={max_size}, and \
         desired-size={desired_capacity}. {} {instances} instance, please update the \
         AutoScalingGroup sizes appropriately.",
        move_kind.clause()
    )
}

#[cfg(test)]
mod tests {
    use super::{group_sizes_refuse_move_message, GroupSizeMove, GroupSizes};

    /// One sentence, one template: the moves differ in their own clause alone, and every
    /// other part — the group, the three sizes, the count and the remedy — is placed the same
    /// way for all of them.
    #[test]
    fn every_move_states_one_template() {
        let sizes = GroupSizes {
            min_size: 1,
            max_size: 2,
            desired_capacity: 2,
        };
        for (case, move_kind, expected) in [
            (
                "a detach",
                GroupSizeMove::Detach,
                "AutoScalingGroup asg has min-size=1, max-size=2, and desired-size=2. To detach \
                 3 instance, please update the AutoScalingGroup sizes appropriately.",
            ),
            (
                "an attach",
                GroupSizeMove::Attach,
                "AutoScalingGroup asg has min-size=1, max-size=2, and desired-size=2. To attach \
                 3 instance, please update the AutoScalingGroup sizes appropriately.",
            ),
            (
                "a park",
                GroupSizeMove::Standby,
                "AutoScalingGroup asg has min-size=1, max-size=2, and desired-size=2. To place \
                 into standby 3 instance, please update the AutoScalingGroup sizes \
                 appropriately.",
            ),
        ] {
            let stated = group_sizes_refuse_move_message("asg", sizes, move_kind, 3);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }
}
