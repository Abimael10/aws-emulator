// SPDX-License-Identifier: CC0-1.0

//! ONE DIRECTORY'S AMAZON SNS EVENT-TOPIC ASSOCIATIONS: what `RegisterEventTopic`
//! persisted, what `DeregisterEventTopic` removes, and what `DescribeEventTopics` reports
//! about them.
//!
//! The association itself is this service's record — the directory it hangs off, the topic
//! NAME the register bound and the instant it was bound — but the topic it points AT is a
//! resource of Amazon SNS and not of this one. So the `Status` an association reports is
//! not a field a register writes once: it is resolved, at every read, in the store of the
//! service that OWNS topics ([`SnsTopics`]), because a topic can be deleted — or never have
//! existed — after the binding was made. A directory's event topic is its ONLY notification
//! channel, and the modelled `TopicStatus` enum carries
//! [`TOPIC_STATUS_TOPIC_NOT_FOUND`] for exactly that case, so a binding whose topic SNS
//! does not hold is reported in that status rather than as one that holds.
//!
//! The register itself still records the binding whichever way that resolution goes: the
//! status is the one thing it does not decide, and the enum member for a topic that is not
//! there is reachable only because the association exists to report it.
//!
//! LOCK ORDER: a reader holds this service's own store first and the store of the service
//! that owns topics second, which is the order every cross-service reader in this tree
//! keeps.

use super::*;

use ::service_com_amazonaws_sns_amazonsimplenotificationservice::state as sns;

/// The `TopicStatus` an association whose topic Amazon SNS HOLDS reports: the `Registered`
/// value of the modelled `TopicStatus` enum
/// (`smithy_query(query="shape",shape="TopicStatus")` answers
/// `["Registered", "Topic not found", "Failed", "Deleted"]`), and the one value of it that
/// means the directory's notification channel is wired up.
pub const TOPIC_STATUS_REGISTERED: &str = "Registered";

/// The `TopicStatus` an association whose topic Amazon SNS does NOT hold reports: the
/// `Topic not found` value of the same modelled enum, which is the member that exists for
/// a binding pointing at a topic that was deleted or never created.
pub const TOPIC_STATUS_TOPIC_NOT_FOUND: &str = "Topic not found";

/// Which topics Amazon SNS holds, as the ONE read of that service's store a projection of
/// this service's associations needs.
///
/// Taken once per reporting call rather than once per association, so a directory with
/// many bindings costs one look at the owner of topics. An engine carrying no topic store
/// holds no topic at all, which is the same position as a topic that was deleted: there is
/// nothing to publish to.
#[derive(Debug, Default)]
pub struct SnsTopics {
    /// The ARNs the service that owns topics currently holds one under.
    held: BTreeSet<String>,
}

impl SnsTopics {
    /// Read what that service holds. Called with a handler's `&*self.engine`.
    ///
    /// BOUNDED by that service's own ceiling on its topic store rather than by anything
    /// this request carries: no member of a request reaches this read, and the set it
    /// builds is as large as the account's topics, which the owner of topics caps.
    #[must_use]
    pub fn read<Eng: Send + Sync + 'static>(engine: &Eng) -> Self {
        let held = sns::service_state(engine)
            .map(|state| state.lock().topics.keys().cloned().collect())
            .unwrap_or_default();
        Self { held }
    }

    /// The `TopicStatus` an association pointing at `topic_arn` reports: whether the
    /// service that owns topics holds that topic, and nothing about this service's own
    /// record.
    #[must_use]
    pub fn status_of(&self, topic_arn: &str) -> &'static str {
        if self.held.contains(topic_arn) {
            TOPIC_STATUS_REGISTERED
        } else {
            TOPIC_STATUS_TOPIC_NOT_FOUND
        }
    }
}

/// One stored association between a directory and an Amazon SNS topic: what
/// `RegisterEventTopic` persisted and `DescribeEventTopics` reports.
///
/// It holds no status: see the module docs — the status is the owner of topics' answer, not
/// a value of this record.
#[derive(Debug, Clone)]
pub struct EventTopicRegistration {
    pub topic_name: String,
    /// The topic's ARN, built once at registration through [`sns_topic_arn`] so every
    /// later read reports the same one.
    pub topic_arn: String,
    /// When the association was made — the `CreatedDateTime` member's "the date and
    /// time of when you associated your directory with the Amazon SNS topic".
    pub created_at: SystemTime,
}

impl EventTopicRegistration {
    /// This association as the modelled `EventTopic`, reported for `directory_id` with the
    /// status `topics` resolves its topic to.
    ///
    /// The one place a stored registration is rendered, so `DescribeEventTopics`
    /// reports exactly what was persisted and exactly the status the owner of topics
    /// answers for.
    pub fn to_event_topic(&self, directory_id: &str, topics: &SnsTopics) -> EventTopic {
        EventTopic {
            directory_id: Some(directory_id.to_string()),
            topic_name: Some(self.topic_name.clone()),
            topic_arn: Some(self.topic_arn.clone()),
            created_date_time: Some(as_timestamp(self.created_at)),
            status: Some(topics.status_of(&self.topic_arn).to_string()),
        }
    }
}

impl super::Directory {
    /// This directory's associations as the modelled `EventTopics` list, in topic-name
    /// order, narrowed to `wanted` when the request asked for particular topic names, each
    /// reported in the status `topics` resolves its topic to.
    pub fn event_topic_list(
        &self,
        wanted: Option<&BTreeSet<&String>>,
        topics: &SnsTopics,
    ) -> Vec<EventTopic> {
        self.event_topics
            .values()
            .filter(|registration| {
                wanted.is_none_or(|names| names.contains(&registration.topic_name))
            })
            .map(|registration| registration.to_event_topic(&self.directory_id, topics))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the status an association reports follows whether the owner of topics holds
    /// the topic it points at — `Registered` when it does, the modelled `Topic not found`
    /// when it does not — and both are values the model declares.
    #[test]
    fn the_status_follows_whether_the_topic_is_held() {
        let region = "us-east-1";
        let held_names = ["ds-status", "Topic_1", &"x".repeat(256)];
        let topics = SnsTopics {
            held: held_names
                .iter()
                .map(|name| sns_topic_arn(region, name))
                .collect(),
        };
        // (case, the topic a binding points at, the status it must report)
        let rows: Vec<(&str, String, &str)> = vec![
            (
                "a simple name the owner holds",
                "ds-status".to_string(),
                TOPIC_STATUS_REGISTERED,
            ),
            (
                "a mixed-form name the owner holds",
                "Topic_1".to_string(),
                TOPIC_STATUS_REGISTERED,
            ),
            (
                "the longest name the model admits, held",
                "x".repeat(256),
                TOPIC_STATUS_REGISTERED,
            ),
            (
                "a name the owner does not hold",
                "never-created".to_string(),
                TOPIC_STATUS_TOPIC_NOT_FOUND,
            ),
            (
                "a name differing from a held one in case alone",
                "DS-STATUS".to_string(),
                TOPIC_STATUS_TOPIC_NOT_FOUND,
            ),
            (
                "the shortest name the model admits, not held",
                "a".to_string(),
                TOPIC_STATUS_TOPIC_NOT_FOUND,
            ),
        ];
        for (case, topic_name, expected) in rows {
            let registration = EventTopicRegistration {
                topic_arn: sns_topic_arn(region, &topic_name),
                topic_name: topic_name.clone(),
                created_at: SystemTime::UNIX_EPOCH,
            };
            let reported = registration.to_event_topic("d-0123456789", &topics);
            assert_eq!(
                reported.status.as_deref(),
                Some(expected),
                "case {case:?}: expected the status {expected:?}, got {:?}",
                reported.status
            );
            assert!(
                crate::ops::types::TOPIC_STATUS_VALUES.contains(&expected),
                "case {case:?}: expected a modelled TopicStatus one of {:?}",
                crate::ops::types::TOPIC_STATUS_VALUES
            );
        }
    }

    /// RULE: an engine whose owner of topics holds nothing at all resolves every binding to
    /// `Topic not found` — the same position as a topic that was deleted.
    #[test]
    fn nothing_held_reports_the_topic_as_not_found() {
        let topics = SnsTopics::default();
        let registration = EventTopicRegistration {
            topic_arn: sns_topic_arn("us-east-1", "ds-status"),
            topic_name: "ds-status".to_string(),
            created_at: SystemTime::UNIX_EPOCH,
        };
        let reported = registration.to_event_topic("d-0123456789", &topics);
        assert_eq!(
            reported.status.as_deref(),
            Some(TOPIC_STATUS_TOPIC_NOT_FOUND),
            "case \"no topic held\": expected {TOPIC_STATUS_TOPIC_NOT_FOUND:?}, got {:?}",
            reported.status
        );
    }
}
