// SPDX-License-Identifier: CC0-1.0

//! The Amazon SNS notification configurations of an Auto Scaling group: which topic the
//! group notifies, and about which events.
//!
//! A notification configuration is a CHILD of one Auto Scaling group keyed by the topic it
//! names, so it is stored on [`StoredGroup`] itself
//! ([`StoredGroup::notification_configurations`]): one collection that
//! `PutNotificationConfiguration` upserts into, `DescribeNotificationConfigurations`
//! projects from, and the group's own delete takes with the group. Nothing in this crate
//! holds a second copy.
//!
//! # The upsert
//!
//! `PutNotificationConfiguration` "configures an Auto Scaling group to send notifications
//! when specified events take place […] This configuration overwrites any existing
//! configuration"
//! (`smithy_query(query="operation",operation="PutNotificationConfiguration")`), and the
//! configuration a call carries is one TOPIC and the event types it is to be told about
//! (`TopicARN`, `NotificationTypes`). So the collection is keyed by topic and a second call
//! naming the same topic REPLACES that topic's event types rather than adding to them,
//! while a call naming another topic leaves the first one alone — which is also what makes
//! the documented ceiling ("your maximum limit of SNS topics, which is 10 per Auto Scaling
//! group") a count of topics.
//!
//! # What a `TopicARN` has to name
//!
//! Both writers of the collection take the topic as a `TopicARN` the model documents as
//! "The Amazon Resource Name (ARN) of the Amazon SNS topic"
//! (`smithy_query(query="operation",operation="PutNotificationConfiguration")`). The member
//! is modelled as free text bounded by length alone, so nothing at the request boundary can
//! hold it to that form and this module does
//! ([`names_an_sns_topic`]): the value has to be a well-formed resource ARN of the Amazon
//! SNS namespace naming a topic in a Region, because a topic is a REGIONAL resource and the
//! Region of its ARN is the one the notification is published in
//! ([`super::sns_notifications`]). Anything else names no topic this service could ever
//! notify, so it is refused on the input
//! ([`NotificationConfigurationRefusal::MalformedTopicArn`]) rather than stored as a
//! destination that can receive nothing.
//!
//! # The delete
//!
//! `DeleteNotificationConfiguration` "deletes the specified notification"
//! (`smithy_query(query="operation",operation="DeleteNotificationConfiguration")`), and the
//! notification it specifies is the one entry the request's (`AutoScalingGroupName`,
//! `TopicARN`) pair keys — so it takes that topic's whole entry, every event type included,
//! and leaves the group's other topics alone. The configuration has to BE there: a pair
//! this collection holds no entry for names no notification to delete, so the request is
//! refused rather than answered as a no-op
//! ([`NotificationConfigurationRefusal::AbsentConfiguration`]).
//!
//! # What the group is told about
//!
//! [`StoredGroup::notification_topics`] is the one reading of "which topics does this group
//! publish THIS event to", so the publisher of an instance launch and the publisher of an
//! instance termination select their topics by the same rule
//! ([`super::sns_notifications`]).

use std::collections::{BTreeMap, BTreeSet};

use super::sns_notifications::SNS_ARN_VENDOR;
use super::StoredGroup;

/// The most Amazon SNS topics one Auto Scaling group may be configured to notify.
///
/// From the model's own documentation for `PutNotificationConfiguration`: "If you exceed
/// your maximum limit of SNS topics, which is 10 per Auto Scaling group, the call fails."
/// The member carries no modelled bound, so the handler checks it and answers the
/// operation's modelled `LimitExceededFault` past it.
pub const MAX_NOTIFICATION_TOPICS_PER_GROUP: usize = 10;

/// The notification configurations of ONE group: the event types it notifies about, keyed
/// by the `TopicARN` they are notified to.
///
/// A set rather than a list, because `DescribeNotificationConfigurations` reports one
/// configuration per (group, topic, event type) and a request that names the same type
/// twice configures one.
pub type NotificationConfigurations = BTreeMap<String, BTreeSet<String>>;

/// Why a notification configuration could not be stored, for the operation to answer the
/// error class each situation belongs to.
#[derive(Debug, PartialEq, Eq)]
pub enum NotificationConfigurationRefusal {
    /// The request's `TopicARN` names no Amazon SNS topic ([`names_an_sns_topic`]); carries
    /// the value it carried.
    MalformedTopicArn(String),
    /// The request names an event type this service does not notify about; carries what it
    /// named.
    UnknownNotificationType(String),
    /// The group is already configured with [`MAX_NOTIFICATION_TOPICS_PER_GROUP`] topics
    /// and this request names one more.
    LimitExceeded,
    /// The group holds no configuration for the topic the request names, so there is no
    /// notification to delete; carries the topic it named.
    AbsentConfiguration(String),
}

impl NotificationConfigurationRefusal {
    /// What the caller is told. ONE spelling per situation, so no two writers of a
    /// configuration refuse the same request differently.
    pub fn message(&self) -> String {
        match self {
            Self::MalformedTopicArn(topic_arn) => invalid_topic_arn_message(topic_arn),
            Self::UnknownNotificationType(notification_type) => {
                unknown_notification_type_message(notification_type)
            }
            Self::LimitExceeded => too_many_topics_message(),
            Self::AbsentConfiguration(topic_arn) => {
                absent_notification_configuration_message(topic_arn)
            }
        }
    }
}

impl StoredGroup {
    /// Create or replace the configuration naming `topic_arn`, per the upsert rule in this
    /// module's own documentation.
    ///
    /// Every rule is checked BEFORE the first write, so a refused request leaves the
    /// group's configurations exactly as they were.
    pub fn put_notification_configuration(
        &mut self,
        topic_arn: &str,
        notification_types: &[String],
    ) -> Result<(), NotificationConfigurationRefusal> {
        if !names_an_sns_topic(topic_arn) {
            return Err(NotificationConfigurationRefusal::MalformedTopicArn(
                topic_arn.to_string(),
            ));
        }
        if let Some(unknown) = notification_types
            .iter()
            .find(|candidate| !super::AUTO_SCALING_NOTIFICATION_TYPES.contains(&candidate.as_str()))
        {
            return Err(NotificationConfigurationRefusal::UnknownNotificationType(
                unknown.clone(),
            ));
        }
        let configures_a_new_topic = !self.notification_configurations.contains_key(topic_arn);
        if configures_a_new_topic
            && self.notification_configurations.len() >= MAX_NOTIFICATION_TOPICS_PER_GROUP
        {
            return Err(NotificationConfigurationRefusal::LimitExceeded);
        }
        self.notification_configurations.insert(
            topic_arn.to_string(),
            notification_types.iter().cloned().collect(),
        );
        Ok(())
    }

    /// Take the configuration naming `topic_arn` off this group, per the delete rule in
    /// this module's own documentation.
    ///
    /// The whole entry goes, so the group stops notifying that topic about every event it
    /// was configured for; a topic this group holds no entry for is refused and nothing is
    /// written.
    pub fn delete_notification_configuration(
        &mut self,
        topic_arn: &str,
    ) -> Result<(), NotificationConfigurationRefusal> {
        if !names_an_sns_topic(topic_arn) {
            return Err(NotificationConfigurationRefusal::MalformedTopicArn(
                topic_arn.to_string(),
            ));
        }
        if self.notification_configurations.remove(topic_arn).is_none() {
            return Err(NotificationConfigurationRefusal::AbsentConfiguration(
                topic_arn.to_string(),
            ));
        }
        Ok(())
    }

    /// The topics this group notifies about `notification_type`, in the order the
    /// collection holds them.
    ///
    /// A topic configured for other event types is not told about this one, which is what
    /// makes a configuration a selection rather than a subscription to everything.
    #[must_use]
    pub fn notification_topics(&self, notification_type: &str) -> Vec<String> {
        self.notification_configurations
            .iter()
            .filter(|(_, types)| types.contains(notification_type))
            .map(|(topic_arn, _)| topic_arn.clone())
            .collect()
    }
}

/// Whether `value` names an Amazon SNS topic this service could publish to, per the rule in
/// this module's own documentation.
///
/// ONE reading of the value, through the workspace's own caller-supplied-ARN parse
/// (`service_common::arn::region_of`): a well-formed resource ARN of the Amazon SNS
/// namespace whose Region field is a Region LABEL. Both halves come out of that one reader
/// because they are one question — a topic is a REGIONAL resource, so an ARN naming no
/// Region names no topic, and the label is read under the grammar the publisher addresses
/// the topic in ([`super::sns_notifications`]). Nothing more is required of the resource
/// field: an SNS topic ARN's resource field is the topic name itself
/// (`arn:aws:sns:<region>:<account>:<topic>`), so there is no resource type to match.
///
/// THE ONE predicate for the whole crate: the writers hold a caller's `TopicARN` to it and
/// the publisher of a confirmation and of an instance lifecycle event asks it before
/// addressing a stored topic ([`super::sns_notifications`]), so what a group may be
/// configured with and what the publisher can reach cannot drift apart.
#[must_use]
pub fn names_an_sns_topic(value: &str) -> bool {
    ::service_common::arn::region_of(value, SNS_ARN_VENDOR).is_some()
}

/// The message a request whose `TopicARN` names no Amazon SNS topic is refused with: the
/// member as the model names it, and the value the request carried, which is the only
/// variable part.
pub fn invalid_topic_arn_message(topic_arn: &str) -> String {
    format!("Invalid TopicARN: {topic_arn}")
}

/// The message a request naming an event type outside this service's own catalog is
/// refused with, in the shape this service refuses every value outside one of its
/// catalogs: the member as the model names it, the value from the request, and the catalog
/// the caller has to choose from.
pub fn unknown_notification_type_message(notification_type: &str) -> String {
    format!(
        "The NotificationType value {notification_type} is not valid: it must be one of {}",
        super::stated_value_set(super::AUTO_SCALING_NOTIFICATION_TYPES.iter().copied())
    )
}

/// The message a request that would configure a topic past
/// [`MAX_NOTIFICATION_TOPICS_PER_GROUP`] is refused with, which quotes the limit the
/// caller has to come back under — the shape this service reports every per-group ceiling
/// in.
pub fn too_many_topics_message() -> String {
    format!(
        "The notification configuration exceeds the limit of \
         {MAX_NOTIFICATION_TOPICS_PER_GROUP} SNS topics per Auto Scaling group"
    )
}

/// The message a request naming a notification this group was never configured with is
/// refused with: Amazon EC2 Auto Scaling reports the absence as the TOPIC not existing for
/// the account, since a configuration is what makes a topic one of the account's
/// notification topics at all.
///
/// Two variable parts, both quoted: the topic the request named, and the account the
/// request was made for — this emulator's own account, which is the account every resource
/// it holds belongs to. The rest, punctuation included, is fixed text.
pub fn absent_notification_configuration_message(topic_arn: &str) -> String {
    format!(
        "Notification Topic '{topic_arn}' doesn't exist for '{}'",
        ::engine::emulator_account_id()
    )
}
