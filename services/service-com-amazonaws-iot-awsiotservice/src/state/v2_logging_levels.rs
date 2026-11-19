// SPDX-License-Identifier: CC0-1.0

//! The account's V2 LOGGING LEVELS: the per-target records `SetV2LoggingLevel` writes
//! and `ListV2LoggingLevels` reports.
//!
//! `SetV2LoggingLevel` "sets the logging level" for the `LogTarget` it names and
//! `ListV2LoggingLevels` "lists logging levels"
//! (`smithy_query(query="operation",operation="SetV2LoggingLevel,ListV2LoggingLevels")`),
//! so unlike the one set of account-wide options in [`super::v2_logging`] these are a
//! MAP: one record per log target, each holding the level set for it.
//!
//! The records are keyed by the whole log target and not by its name, because a
//! `LogTarget` is the PAIR `targetType` + `targetName` ("The target type", `@required`,
//! and "The target name") — the same name under two target types is two targets — and a
//! second set for one target carries the level that target now has rather than a second
//! record for it. `BTreeMap` (not `HashMap`) because `ListV2LoggingLevels` is
//! `@paginated` and a page boundary is only meaningful over a stable order.
//!
//! A level is an OVERRIDE of the account's V2 logging options, so a handler that sets
//! or reports one refuses an account that has no options with the modelled
//! `NotConfiguredException` ("The resource is not configured", HTTP 404) — which is why
//! both operations model that error and `DeleteV2LoggingLevel`, which only takes a
//! record away, does not.

use std::collections::BTreeMap;

use crate::ops::types::{LogTarget, LogTargetConfiguration};

/// How many per-target logging levels this emulator keeps for the account.
///
/// The store is one a CALLER grows — one record per log target it sets a level for — so
/// it is bounded, for the same reason [`super::MAX_ROLE_ALIASES`] is: past the ceiling
/// nothing is recorded and the set is refused with the `LimitExceededException`
/// `SetV2LoggingLevel` models ("A limit has been exceeded", HTTP 410) rather than
/// growing without bound.
pub const MAX_V2_LOGGING_LEVELS: usize = 100;

/// The `LogTargetType` value that names the ACCOUNT's own default logging rather than
/// one of its resources (`smithy_query(query="shape",shape="LogTargetType")`): the one
/// target type that carries no target name.
pub const LOG_TARGET_TYPE_DEFAULT: &str = "DEFAULT";

/// A LOG TARGET, as this service keys the levels it holds by: everything the modelled
/// `LogTarget` carries, which is the pair that identifies it.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct LoggedTarget {
    /// "The target type" — one of the values `LogTargetType` declares; the boundary
    /// validator refuses anything else before a handler sees it.
    pub target_type: String,
    /// "The target name": the resource the level applies to, which only the account's
    /// own [`LOG_TARGET_TYPE_DEFAULT`] target does without.
    pub target_name: Option<String>,
}

impl LoggedTarget {
    /// The target a request named.
    ///
    /// The account's own [`LOG_TARGET_TYPE_DEFAULT`] target names no resource, so its
    /// identity is that type ALONE: a name sent beside it is not part of the target, and
    /// dropping it here is what makes the writer, the reader and the remover of that one
    /// target agree on which record it is.
    pub fn of(target: LogTarget) -> Self {
        let target_name = if target.target_type == LOG_TARGET_TYPE_DEFAULT {
            None
        } else {
            target.target_name
        };
        Self {
            target_type: target.target_type,
            target_name,
        }
    }

    /// This key as the modelled `LogTarget` a reader reports it as, so what a reader
    /// hands back is the target the writer recorded and the two cannot drift.
    pub fn log_target(&self) -> LogTarget {
        LogTarget {
            target_type: self.target_type.clone(),
            target_name: self.target_name.clone(),
        }
    }

    /// This target's POSITION in a listing's total order.
    ///
    /// A page marker names a position rather than a record ([`super::page_of`]), and a
    /// position is one string, so the pair is folded into one: the target type, then the
    /// target name hex-encoded. Hex is what makes the fold injective — a name is an
    /// arbitrary `LogTargetName` string, and a hex-encoded byte string keeps the order
    /// of the bytes it encodes — and a target with no name at all is its type alone,
    /// which sorts before every named target of that type.
    pub fn page_position(&self) -> String {
        match self.target_name.as_deref() {
            Some(name) => format!("{}/{}", self.target_type, ::hex::encode(name)),
            None => self.target_type.clone(),
        }
    }
}

/// The configurations an account holds for `target_type` — every one it holds when no
/// type is named — each beside the position it occupies in the listing's total order,
/// ascending.
///
/// `targetType` is "The type of resource for which you are configuring logging"
/// (`smithy_query(query="operation",operation="ListV2LoggingLevels")`), so a listing
/// that names one reports the targets OF that type and a listing that names none
/// reports them all.
pub fn logged_target_rows(
    levels: &BTreeMap<LoggedTarget, String>,
    target_type: Option<&str>,
) -> Vec<(String, LogTargetConfiguration)> {
    let mut rows: Vec<(String, LogTargetConfiguration)> = levels
        .iter()
        .filter(|(target, _)| target_type.is_none_or(|named| target.target_type == named))
        .map(|(target, log_level)| {
            (
                target.page_position(),
                LogTargetConfiguration {
                    log_target: Some(target.log_target()),
                    log_level: Some(log_level.clone()),
                },
            )
        })
        .collect();
    rows.sort_by(|(left, _), (right, _)| left.cmp(right));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a target's listing position is injective over the pair that identifies it,
    /// and ordering positions orders the targets.
    #[test]
    fn a_position_identifies_one_target_and_keeps_the_pairs_order() {
        // The rows span what the pair generalises over: a target with no name, one whose
        // name is empty, two names under one type, and one name under two types.
        let targets = [
            LoggedTarget {
                target_type: LOG_TARGET_TYPE_DEFAULT.to_string(),
                target_name: None,
            },
            LoggedTarget {
                target_type: LOG_TARGET_TYPE_DEFAULT.to_string(),
                target_name: Some(String::new()),
            },
            LoggedTarget {
                target_type: "THING_GROUP".to_string(),
                target_name: Some("fleet".to_string()),
            },
            LoggedTarget {
                target_type: "THING_GROUP".to_string(),
                target_name: Some("fleet/sensors".to_string()),
            },
            LoggedTarget {
                target_type: "CLIENT_ID".to_string(),
                target_name: Some("fleet".to_string()),
            },
        ];

        let positions: Vec<String> = targets.iter().map(LoggedTarget::page_position).collect();
        for (left, left_position) in targets.iter().zip(&positions) {
            for (right, right_position) in targets.iter().zip(&positions) {
                assert_eq!(
                    left_position == right_position,
                    left == right,
                    "case {left:?} vs {right:?}: expected equal positions only for one \
                     target, got {left_position:?} and {right_position:?}"
                );
                assert_eq!(
                    left_position.cmp(right_position),
                    left.cmp(right),
                    "case {left:?} vs {right:?}: expected the pair's order, got \
                     {left_position:?} against {right_position:?}"
                );
            }
        }
    }

    /// RULE: a listing reports the targets of the type it names, and every target it
    /// holds when it names none.
    #[test]
    fn rows_report_the_named_type_and_otherwise_every_target() {
        let mut levels = BTreeMap::new();
        for (target_type, target_name, level) in [
            (LOG_TARGET_TYPE_DEFAULT, None, "WARN"),
            ("THING_GROUP", Some("fleet"), "DEBUG"),
            ("THING_GROUP", Some("alpha"), "INFO"),
            ("CLIENT_ID", Some("sensor-7"), "ERROR"),
        ] {
            levels.insert(
                LoggedTarget {
                    target_type: target_type.to_string(),
                    target_name: target_name.map(str::to_string),
                },
                level.to_string(),
            );
        }

        for (case, filter, expected) in [
            (
                "no type named",
                None,
                vec!["CLIENT_ID", "DEFAULT", "THING_GROUP", "THING_GROUP"],
            ),
            ("one named type", Some("THING_GROUP"), vec!["THING_GROUP"; 2]),
            ("a type with one target", Some("CLIENT_ID"), vec!["CLIENT_ID"]),
            ("a type with no target", Some("SOURCE_IP"), Vec::new()),
        ] {
            let rows = logged_target_rows(&levels, filter);
            let reported: Vec<String> = rows
                .iter()
                .map(|(_, configuration)| {
                    configuration
                        .log_target
                        .as_ref()
                        .expect("a reported configuration names its target")
                        .target_type
                        .clone()
                })
                .collect();
            assert_eq!(
                reported, expected,
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );

            let positions: Vec<&String> = rows.iter().map(|(position, _)| position).collect();
            let mut ascending = positions.clone();
            ascending.sort();
            assert_eq!(
                positions, ascending,
                "case {case:?}: expected the rows in listing order, got {positions:?}"
            );
        }
    }
}
