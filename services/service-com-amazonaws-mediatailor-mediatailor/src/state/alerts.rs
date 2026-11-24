// SPDX-License-Identifier: CC0-1.0

//! The alerts this service holds AGAINST a resource, and how a read of them
//! reports one.
//!
//! An alert is not a resource a caller creates: it is a condition MediaTailor
//! raises against a resource it already holds and withdraws when the condition
//! clears, which is why the API surface only READS them
//! (`smithy_query(query="operation",operation="ListAlerts",model_path="services/service-com-amazonaws-mediatailor-mediatailor/smithy-model/com-amazonaws-mediatailor-mediatailor.json")`
//! is `@readonly`, and the model declares no alert write). So the alerts of a
//! resource live HERE, in the one store every read of them reports, keyed by the
//! ARN of the resource they are held against and then by the alert's own
//! `AlertCode` — the code that identifies the condition, so one resource holds
//! at most one alert per condition and an alert is replaced rather than
//! duplicated when the condition is raised again.
//!
//! No request grows this store: every resource this emulator holds was minted
//! from a request it had already validated, and nothing it runs afterwards fails
//! against one, so a resource it holds carries no alert and its read reports the
//! empty set. The store is what makes that answer the RESOURCE'S own rather than
//! a constant: a resource whose entry holds alerts reports exactly those, in the
//! pages the read serves, and a resource's alerts are released with the
//! resource, by the delete that removes it.

use super::*;

/// One alert held against a resource: the condition's code and its explanation,
/// when it last changed, the ARNs it relates to, and the category MediaTailor
/// files it under.
///
/// The members are `Alert`'s own, so a read projects the record member for
/// member ([`Self::alert`]); `Category` is the one optional member, absent when
/// the alert is filed under none of `AlertCategory`'s values.
#[derive(Debug, Clone)]
pub struct AlertRecord {
    pub alert_code: String,
    pub alert_message: String,
    pub last_modified_time: ::aws_smithy_types::DateTime,
    pub related_resource_arns: Vec<String>,
    pub resource_arn: String,
    pub category: Option<String>,
}

impl AlertRecord {
    /// This alert as the wire shape reports it.
    pub fn alert(&self) -> Alert {
        Alert {
            alert_code: self.alert_code.clone(),
            alert_message: self.alert_message.clone(),
            last_modified_time: self.last_modified_time,
            related_resource_arns: self.related_resource_arns.clone(),
            resource_arn: self.resource_arn.clone(),
            category: self.category.clone(),
        }
    }
}

impl ComAmazonawsMediatailorMediatailorState {    /// The alerts held against `arn`, in the order a read reports them: by
    /// `AlertCode`, the identifier an alert of one resource is held under, so
    /// two reads of one resource walk its alerts in the same order and a
    /// continuation token can name the page boundary.
    ///
    /// A resource with no alert against it answers the EMPTY list — the caller
    /// asked what the resource's alerts are, and the answer is "none" — which is
    /// a different answer from the refusal an ARN holding no resource gets (see
    /// [`Self::resource_exists`]).
    pub fn alerts_of(&self, arn: &str) -> Vec<Alert> {
        self.alerts
            .get(arn)
            .map(|held| held.values().map(AlertRecord::alert).collect())
            .unwrap_or_default()
    }
}

/// How the alerts read states that `arn` addresses no resource this account
/// holds.
///
/// An ARN names a resource KIND as well as a name, and the resource the read
/// addressed is the one that was not found, so the refusal is worded the way
/// that kind's OWN reads word a miss: the channel sentence for a channel ARN,
/// the playback-configuration sentence — which carries the configuration's name
/// — for a playback-configuration ARN. An ARN naming no kind this service holds
/// (a kind it does not persist, another service's ARN, a string that is no ARN)
/// addresses no such resource to name, so it gets the sentence the routes that
/// address a resource by ARN answer a miss with.
pub fn alerts_resource_not_found_message(arn: &str) -> String {
    match resource_from_arn(arn) {
        Some(("channel", _)) => CHANNEL_NOT_FOUND_MESSAGE.to_string(),
        Some(("playbackConfiguration", name)) => playback_configuration_not_found_message(name),
        _ => RESOURCE_NOT_FOUND_MESSAGE.to_string(),
    }
}
