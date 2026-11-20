// SPDX-License-Identifier: CC0-1.0

//! The VALUES a topic's configuration entries may carry, and how a broker refuses one
//! that is outside them.
//!
//! A topic-level configuration entry is not free text: each key Apache Kafka defines
//! declares a TYPE, and some declare the exact set of strings they admit. A broker
//! therefore validates every entry of a `CreateTopics` request before it creates the
//! topic, and refuses that topic — and only that topic — with `INVALID_CONFIG` when an
//! entry's value is not one the key admits, rather than storing a setting no broker
//! could act on. The sentence it refuses with is Apache Kafka's own
//! `ConfigException` rendering, `Invalid value <value> for configuration <name>:
//! <why>`, which is what a client surfaces to its user.
//!
//! Its own file because this is DATA: the keys, their types and their admitted values
//! are Apache Kafka's published topic-level configuration ("Topic-Level Configs",
//! Apache Kafka documentation), and nothing in this emulator's state implies them. A
//! key that is not named here is one this catalog says nothing about, so its value is
//! taken as it stands.

/// How one topic configuration key reads its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    /// One of a fixed set of strings, in the order the broker lists them.
    OneOf(&'static [&'static str]),
    /// A comma-separated list, each element one of a fixed set of strings.
    ListOf(&'static [&'static str]),
    /// A 64-bit integer.
    Long,
    /// A 32-bit integer.
    Int,
    /// A floating-point number.
    Double,
    /// `true` or `false`.
    Boolean,
}

/// The topic-level configuration keys whose value this broker reads, with how each one
/// reads it ("Topic-Level Configs", Apache Kafka documentation; the type names are the
/// ones `ConfigDef` reports a value in).
const TOPIC_CONFIG_VALUES: &[(&str, ValueKind)] = &[
    (
        "cleanup.policy",
        ValueKind::ListOf(&["compact", "delete"]),
    ),
    (
        "compression.type",
        ValueKind::OneOf(&["uncompressed", "zstd", "lz4", "snappy", "gzip", "producer"]),
    ),
    ("delete.retention.ms", ValueKind::Long),
    ("file.delete.delay.ms", ValueKind::Long),
    ("flush.messages", ValueKind::Long),
    ("flush.ms", ValueKind::Long),
    ("index.interval.bytes", ValueKind::Int),
    ("local.retention.bytes", ValueKind::Long),
    ("local.retention.ms", ValueKind::Long),
    ("max.compaction.lag.ms", ValueKind::Long),
    ("max.message.bytes", ValueKind::Int),
    ("message.downconversion.enable", ValueKind::Boolean),
    ("message.timestamp.after.max.ms", ValueKind::Long),
    ("message.timestamp.before.max.ms", ValueKind::Long),
    ("message.timestamp.difference.max.ms", ValueKind::Long),
    (
        "message.timestamp.type",
        ValueKind::OneOf(&["CreateTime", "LogAppendTime"]),
    ),
    ("min.cleanable.dirty.ratio", ValueKind::Double),
    ("min.compaction.lag.ms", ValueKind::Long),
    ("min.insync.replicas", ValueKind::Int),
    ("preallocate", ValueKind::Boolean),
    ("remote.storage.enable", ValueKind::Boolean),
    ("retention.bytes", ValueKind::Long),
    ("retention.ms", ValueKind::Long),
    ("segment.bytes", ValueKind::Int),
    ("segment.index.bytes", ValueKind::Int),
    ("segment.jitter.ms", ValueKind::Long),
    ("segment.ms", ValueKind::Long),
    ("unclean.leader.election.enable", ValueKind::Boolean),
];

/// Why one configuration entry is not one the broker can act on, worded as Apache
/// Kafka's `ConfigException` words it — or `None` when the entry is one it admits (and
/// for a key this catalog does not name, whose value it says nothing about).
///
/// The fixed text is the error class's; the value, the key and, for a key that admits a
/// fixed set of strings, that set are what vary — the set being the key's own, not the
/// request's.
#[must_use]
pub fn value_violation(name: &str, value: &str) -> Option<String> {
    let kind = TOPIC_CONFIG_VALUES
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, kind)| *kind)?;
    let trimmed = value.trim();
    let why = match kind {
        ValueKind::OneOf(admitted) => {
            (!admitted.contains(&trimmed)).then(|| one_of(admitted))
        }
        ValueKind::ListOf(admitted) => trimmed
            .split(',')
            .map(str::trim)
            .filter(|element| !element.is_empty())
            .any(|element| !admitted.contains(&element))
            .then(|| one_of(admitted)),
        ValueKind::Long => trimmed
            .parse::<i64>()
            .is_err()
            .then(|| not_a_number("LONG")),
        ValueKind::Int => trimmed
            .parse::<i32>()
            .is_err()
            .then(|| not_a_number("INT")),
        ValueKind::Double => trimmed
            .parse::<f64>()
            .is_err()
            .then(|| not_a_number("DOUBLE")),
        ValueKind::Boolean => (!matches!(trimmed, "true" | "false"))
            .then(|| "Expected value to be either true or false".to_string()),
    }?;
    Some(format!(
        "Invalid value {value} for configuration {name}: {why}"
    ))
}

/// How that sentence states the values a key admits.
fn one_of(admitted: &[&str]) -> String {
    format!("String must be one of: {}", admitted.join(", "))
}

/// How it states a value that is no number of the key's own type.
fn not_a_number(kind: &str) -> String {
    format!("Not a number of type {kind}")
}
