// SPDX-License-Identifier: CC0-1.0

//! BASIC INGEST: the reserved topic space a publication reaches a named topic rule through
//! WITHOUT going through the broker's publish/subscribe side at all.
//!
//! AWS IoT documents it as the way to send a message straight to the rules engine
//! ("Reducing messaging costs with Basic Ingest"): a client publishes to
//! `$aws/rules/<ruleName>/<topic>` and the message is handed to the rule that name
//! identifies, so the `<topic>` the remaining levels spell is what the rule's statement
//! reads as its topic and NOTHING is delivered to the subscribers of any topic — which is
//! exactly what makes the path cheaper than an ordinary publication.
//!
//! Two facts follow from one reading of the topic, and both belong to whoever serves a
//! publication rather than to one wire: a publication in this space is routed to ONE rule by
//! name instead of to the rules whose statements cover the topic, and it is not published to
//! subscribers (nor retained, since a retained message is a message for a later subscriber).
//! The reading itself is here, so the two wires of the broker read the space the same way.

/// The reserved topic space Basic Ingest lives in: the levels before the rule name.
///
/// `$aws/rules` — AWS IoT's own reserved prefix for it, the one a publication names a rule
/// through.
pub const BASIC_INGEST_PREFIX: &str = "$aws/rules/";

/// The level separator of a topic, which the rule name is one level of.
const TOPIC_LEVEL_SEPARATOR: char = '/';

/// What a publication to the Basic Ingest space asks for: the rule it names, and the topic
/// the levels after that name spell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicIngest {
    /// The rule the publication is addressed to, which is the only rule it runs.
    pub rule_name: String,
    /// The topic the rule's statement reads — the levels after the rule name, which is what
    /// AWS IoT's `topic()` reports for a message ingested this way, and empty for a
    /// publication that named none.
    pub topic: String,
}

/// What `topic` asks for when it is a Basic Ingest topic, and `None` for every other topic —
/// including the reserved prefix carrying no rule name at all, which names no rule to run.
///
/// The rule name is ONE level: the levels after it are the topic, separators and all, so a
/// publication to `$aws/rules/<ruleName>/a/b` names the rule `<ruleName>` and the topic
/// `a/b`.
#[must_use]
pub fn ingested(topic: &str) -> Option<BasicIngest> {
    let addressed = topic.strip_prefix(BASIC_INGEST_PREFIX)?;
    let (rule_name, remaining) = match addressed.split_once(TOPIC_LEVEL_SEPARATOR) {
        Some((rule_name, remaining)) => (rule_name, remaining),
        None => (addressed, ""),
    };
    if rule_name.is_empty() {
        return None;
    }
    Some(BasicIngest {
        rule_name: rule_name.to_string(),
        topic: remaining.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a publication whose topic opens with the Basic Ingest prefix names the rule in
    /// the level after it and carries the remaining levels as its topic; every other topic is
    /// an ordinary publication.
    ///
    /// The rows differ along what the reading generalises over: how many levels follow the
    /// rule name, a publication naming no topic at all (with and without the trailing
    /// separator), the prefix carrying no rule name, a topic in another reserved space, a
    /// plain application topic, and a topic that merely CONTAINS the prefix rather than
    /// opening with it.
    #[test]
    fn a_basic_ingest_topic_names_its_rule_and_carries_the_rest_as_the_topic() {
        let rows: &[(&str, Option<(&str, &str)>)] = &[
            (
                "$aws/rules/to_the_queue/devices/sensor-1/telemetry",
                Some(("to_the_queue", "devices/sensor-1/telemetry")),
            ),
            (
                "$aws/rules/to_the_queue/telemetry",
                Some(("to_the_queue", "telemetry")),
            ),
            ("$aws/rules/to_the_queue/", Some(("to_the_queue", ""))),
            ("$aws/rules/to_the_queue", Some(("to_the_queue", ""))),
            ("$aws/rules/", None),
            ("$aws/rules", None),
            ("$aws/things/sensor-1/shadow/update", None),
            ("devices/sensor-1/telemetry", None),
            ("devices/$aws/rules/to_the_queue/telemetry", None),
            ("", None),
        ];
        for (topic, expected) in rows {
            let actual = ingested(topic);
            let expected = expected.map(|(rule_name, ingested_topic)| BasicIngest {
                rule_name: rule_name.to_string(),
                topic: ingested_topic.to_string(),
            });
            assert_eq!(
                actual, expected,
                "case {topic:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
