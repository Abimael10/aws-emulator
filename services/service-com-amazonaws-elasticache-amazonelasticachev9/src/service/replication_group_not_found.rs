// SPDX-License-Identifier: CC0-1.0

//! The templates this service answers a `ReplicationGroupNotFoundFault` with BESIDE the
//! service-wide one, and which operations take them.
//!
//! ElastiCache words that fault PER OPERATION, and the wordings are observable apart on the
//! live service, so one service-wide template cannot serve them all:
//!
//! ```text
//! ReplicationGroup <id> not found.           <- the shape's spelling, a trailing stop
//! Replication group '<id>' not found         <- prose, the id quoted, no trailing stop
//! Replication group <id> does not exist.     <- prose, unquoted, a trailing stop
//! Specified replication group does not exist <- the delete's own, naming no id at all
//! ```
//!
//! The first is [`crate::state::replication_group_not_found_message`], which the describe and
//! the migration answer. [`quoted_message`] is the second, which the settings change
//! (`ModifyReplicationGroup`), the shard reconfiguration
//! (`ModifyReplicationGroupShardConfiguration`) and the Global datastore create
//! (`CreateGlobalReplicationGroup`) answer for the group each resolves.
//! [`does_not_exist_message`] is the third, which the failover test answers, and
//! [`DELETE_MESSAGE`] the fourth, which `DeleteReplicationGroup` answers.
//!
//! Stated here once so the handlers answering one cannot drift apart about its text: the
//! message is the only place a client reads WHY a call was refused, and for three of the four
//! WHICH group could not be resolved.

/// The template `Replication group '<id>' not found` — the id single-quoted, and no
/// trailing stop.
///
/// The id is the one variable part and comes from the request, as the request spelled it,
/// because the message is built from the request and from nothing else.
pub fn quoted_message(replication_group_id: &str) -> String {
    format!("Replication group '{replication_group_id}' not found")
}

/// How `TestFailover` words a replication group this account does not hold:
/// `Replication group <id> does not exist.`
///
/// Prose, the id unquoted, and a trailing stop. The id is the one variable part and comes
/// from the request, as the request spelled it.
pub fn does_not_exist_message(replication_group_id: &str) -> String {
    format!("Replication group {replication_group_id} does not exist.")
}

/// How `DeleteReplicationGroup` words a replication group this account does not hold:
/// `Specified replication group does not exist`.
///
/// WHOLLY FIXED — the sentence has no variable part at all. The delete states the class of
/// resource that could not be resolved and nothing else, as
/// [`crate::state::REPLICATION_GROUP_ALREADY_EXISTS_MESSAGE`] does for the name a create
/// collides on, so interpolating the requested id would change the string for every caller.
pub const DELETE_MESSAGE: &str = "Specified replication group does not exist";
