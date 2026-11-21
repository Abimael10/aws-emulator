// SPDX-License-Identifier: CC0-1.0

//! The LOG-DELIVERY configurations of a cache, and the state machine that carries each of
//! them from the status the call that asked for it answered to the one it settles at.
//!
//! A `LogDeliveryConfigurationRequest` is accepted up front, whatever destination it names:
//! `LogDeliveryConfiguration$Status` is what reports how that delivery ended up
//! (`LogDeliveryConfigurationStatus`: `active | enabling | modifying | disabling | error`),
//! and `Message` is documented as "an error message for the log delivery configuration". So
//! a newly asked-for delivery answers [`LOG_DELIVERY_STATUS_ENABLING`], a change to one
//! already configured answers [`LOG_DELIVERY_STATUS_MODIFYING`], and the clock then owes the
//! configuration a SETTLED status, which is reached by RESOLVING the destination it names:
//!
//! * a destination that is there takes the delivery, so the configuration settles
//!   [`LOG_DELIVERY_STATUS_ACTIVE`] and reports no message;
//! * a destination that is NOT there can never take one, so the configuration settles
//!   [`LOG_DELIVERY_STATUS_ERROR`] and reports which destination was missing
//!   ([`missing_log_group_message`]) — and nothing here creates it, because the destination
//!   is a resource of the ACCOUNT, held by the service that owns it.
//!
//! `active` is the one signal a client has that its logs are flowing, so a destination that
//! does not exist must not be reported as a healthy delivery: an operator or a readiness
//! check that waits for `active` would otherwise conclude delivery is healthy while no
//! record can ever arrive.
//!
//! WHERE THE ANSWER COMES FROM. Whether a CloudWatch Logs log group exists is CloudWatch
//! Logs' own answer, so it is read out of that service's store on the same shared engine —
//! the store `logs:DescribeLogGroups` answers from, and the one a delivered record is written
//! into ([`super::cache_data_plane::slow_log`]) — rather than from a copy of it kept here.
//! A destination this service has no view of (a Kinesis Data Firehose delivery stream, or a
//! configuration that names no group at all) has nothing to resolve against, and such a
//! configuration settles as it did before, `active`.
//!
//! LOCK ORDER. [`settle_log_delivery`] takes this store's lock, releases it, takes the
//! CloudWatch Logs store's, releases it, and only then takes this one again to write the
//! settled statuses: no two store locks are ever held at once, and CloudWatch Logs never
//! reaches this service, so no cycle exists.

use ::std::collections::BTreeSet;
use ::std::time::{Duration, SystemTime};

use ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state as logs;

use super::*;

/// The `LogDeliveryConfigurationStatus` of a delivery that is carrying logs to the
/// destination it names.
pub const LOG_DELIVERY_STATUS_ACTIVE: &str = "active";

/// The `LogDeliveryConfigurationStatus` of a delivery that has just been asked for and has
/// not been carried through yet.
pub const LOG_DELIVERY_STATUS_ENABLING: &str = "enabling";

/// The `LogDeliveryConfigurationStatus` of a delivery that was already configured for its
/// log type and is being changed.
pub const LOG_DELIVERY_STATUS_MODIFYING: &str = "modifying";

/// The `LogDeliveryConfigurationStatus` of a delivery that could not be set up, which is
/// what [`LogDeliveryRecord::message`] then says the reason for.
pub const LOG_DELIVERY_STATUS_ERROR: &str = "error";

/// How long a log-delivery configuration stays in the status the call that asked for it
/// answered before the clock owes it a settled one.
///
/// The same reasoning as [`CLUSTER_TRANSITION`], whose span this is: ElastiCache takes its
/// own time to set a delivery up, the emulator chooses a span, and it is deliberately NOT
/// zero — so the call that asks for the delivery answers with the transient status a client
/// polls out of, and no status is settled before the destination has been resolved.
pub const LOG_DELIVERY_TRANSITION: Duration = CLUSTER_TRANSITION;

/// How many log-delivery configurations ONE cache holds at most: one per log type the model
/// declares, which is what the stored list is keyed by — a request for a log type already
/// configured REPLACES that configuration rather than adding one.
///
/// A bound on the STORE, so what one cache accumulates follows the model rather than how many
/// requests were sent. It is read off the GENERATED catalog of log types
/// (`crate::ops::types::LOG_TYPE_VALUES`, from the modelled enum `LogType`), so it cannot
/// disagree with the set a request gets past the boundary validator with — and because a
/// token outside that set is refused before any handler runs, the cap changes no request this
/// service serves. What it does is keep a RUN of requests from growing one cache's
/// configuration list, and so the work every later read of that cache does, without end.
pub const MAX_LOG_DELIVERY_CONFIGURATIONS: usize = crate::ops::types::LOG_TYPE_VALUES.len();

/// The `Message` a configuration whose CloudWatch Logs destination does not exist settles
/// with: the fixed sentence, naming the log group the request asked for.
#[must_use]
pub fn missing_log_group_message(log_group: &str) -> String {
    format!("Destination log group {log_group} does not exist.")
}

/// One log-delivery configuration of a cache, as this emulator persists it.
///
/// `LogDeliveryConfigurationRequest` carries an `Enabled` flag; a disabled
/// request removes the configuration rather than storing one, so a stored record
/// is always an enabled one and needs no flag of its own.
#[derive(Debug, Clone)]
pub struct LogDeliveryRecord {
    pub log_type: Option<String>,
    pub destination_type: Option<String>,
    pub destination_details: Option<DestinationDetails>,
    pub log_format: Option<String>,
    /// This configuration's `LogDeliveryConfiguration$Status`.
    pub status: String,
    /// Its `LogDeliveryConfiguration$Message` — the reason a configuration is in
    /// [`LOG_DELIVERY_STATUS_ERROR`], and nothing for one that is not.
    pub message: Option<String>,
    /// The instant the clock owes this configuration a settled status, while it is still
    /// waiting for one; `None` once it has settled.
    pub due: Option<SystemTime>,
}

impl LogDeliveryRecord {
    /// This record as the modelled `LogDeliveryConfiguration`.
    pub fn to_modeled(&self) -> LogDeliveryConfiguration {
        LogDeliveryConfiguration {
            log_type: self.log_type.clone(),
            destination_type: self.destination_type.clone(),
            destination_details: self.destination_details.clone(),
            log_format: self.log_format.clone(),
            status: Some(self.status.clone()),
            message: self.message.clone(),
        }
    }

    /// The CloudWatch Logs log group this configuration delivers to, when it names one:
    /// `DestinationType` `cloudwatch-logs` with a `CloudWatchLogsDestinationDetails$LogGroup`.
    ///
    /// `None` for every other destination, which is one this service holds no view of and
    /// resolves nothing against.
    #[must_use]
    pub fn cloud_watch_log_group(&self) -> Option<&str> {
        if self.destination_type.as_deref() != Some(super::slow_log::CLOUDWATCH_LOGS_DESTINATION)
        {
            return None;
        }
        self.destination_details
            .as_ref()?
            .cloud_watch_logs_details
            .as_ref()?
            .log_group
            .as_deref()
            .filter(|group| !group.is_empty())
    }

    /// Settle this configuration against `groups_present`, the destination log groups
    /// CloudWatch Logs was found to hold, if `now` is past its due instant.
    ///
    /// `resolved` names the groups that WERE looked up, so a configuration stored after the
    /// lookup is left for the next pass rather than settled against an answer that was never
    /// asked about it. This is the one write a reporting operation may make: the settlement
    /// was owed at this instant whether or not anybody called.
    fn settle(
        &mut self,
        now: SystemTime,
        resolved: &BTreeSet<String>,
        groups_present: &BTreeSet<String>,
    ) {
        if self.due.is_none_or(|due| now < due) {
            return;
        }
        let Some(log_group) = self.cloud_watch_log_group().map(str::to_string) else {
            // Nothing to resolve: a destination this service holds no view of takes the
            // delivery as far as this emulator can tell.
            self.due = None;
            self.status = LOG_DELIVERY_STATUS_ACTIVE.to_string();
            self.message = None;
            return;
        };
        if !resolved.contains(&log_group) {
            return;
        }
        self.due = None;
        if groups_present.contains(&log_group) {
            self.status = LOG_DELIVERY_STATUS_ACTIVE.to_string();
            self.message = None;
        } else {
            self.status = LOG_DELIVERY_STATUS_ERROR.to_string();
            self.message = Some(missing_log_group_message(&log_group));
        }
    }
}

/// Apply `requests` to the log-delivery configurations `stored`: a request
/// replaces the configuration for its log type, and `Enabled: false` removes it.
///
/// A stored configuration is NOT settled: the one a request asks for answers
/// [`LOG_DELIVERY_STATUS_ENABLING`], one that replaces a configuration already held for that
/// log type answers [`LOG_DELIVERY_STATUS_MODIFYING`], and the clock owes both a settled
/// status [`LOG_DELIVERY_TRANSITION`] after `now` — reached by resolving the destination
/// ([`settle_log_delivery`]).
///
/// What is stored is bounded at [`MAX_LOG_DELIVERY_CONFIGURATIONS`] however many requests
/// arrive, so neither one call nor a run of them can grow this list without end.
pub fn apply_log_delivery(
    stored: &mut Vec<LogDeliveryRecord>,
    requests: Vec<LogDeliveryConfigurationRequest>,
    now: SystemTime,
) {
    for request in requests {
        let replaced = stored
            .iter()
            .any(|existing| existing.log_type == request.log_type);
        stored.retain(|existing| existing.log_type != request.log_type);
        if request.enabled == Some(false) {
            continue;
        }
        // The bound, taken before the record is allocated: a cache holds one configuration
        // per modelled log type, and a request for a type already held was just removed
        // above, so this is unreachable for any request the boundary validator admits — it is
        // what keeps the list bounded whatever arrives.
        if stored.len() >= MAX_LOG_DELIVERY_CONFIGURATIONS {
            continue;
        }
        stored.push(LogDeliveryRecord {
            log_type: request.log_type,
            destination_type: request.destination_type,
            destination_details: request.destination_details,
            log_format: request.log_format,
            status: if replaced {
                LOG_DELIVERY_STATUS_MODIFYING.to_string()
            } else {
                LOG_DELIVERY_STATUS_ENABLING.to_string()
            },
            message: None,
            due: Some(now + LOG_DELIVERY_TRANSITION),
        });
    }
}

/// Settle every log-delivery configuration of this store whose status the clock has already
/// made due, resolving each destination where the service that owns it answers.
///
/// Called with NO guard of this service's store held, by the reporting operations that
/// publish a `LogDeliveryConfiguration` — the settlement is owed at the instant they answer
/// whether or not anybody called, exactly as a cluster's own status change is.
///
/// Nothing is settled while the destination answers cannot be read at all (an engine holding
/// no CloudWatch Logs store), so a configuration keeps the transient status it was asked for
/// rather than being reported as a delivery that was never resolved.
pub fn settle_log_delivery<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let now = SystemTime::now();
    // Which destination groups the configurations that are DUE name, collected under this
    // store's lock and answered under CloudWatch Logs' own, so no two locks are held at once.
    let wanted = {
        let store = state.lock();
        destinations_due(&store, now)
    };
    let Some(logs_state) = logs::service_state(engine) else {
        return;
    };
    let groups_present: BTreeSet<String> = {
        let logs_store = logs_state.lock();
        wanted
            .iter()
            .filter(|log_group| logs_store.log_groups.contains_key(*log_group))
            .cloned()
            .collect()
    };
    let mut store = state.lock();
    for configurations in every_configuration_list(&mut store) {
        for record in configurations {
            record.settle(now, &wanted, &groups_present);
        }
    }
}

/// The CloudWatch Logs log groups named by the configurations this store holds that are due a
/// settled status at `now`.
///
/// A pass over the caches this store holds, which is bounded by the service's own cluster and
/// group quotas times [`MAX_LOG_DELIVERY_CONFIGURATIONS`], so what one call costs follows what
/// the store HOLDS rather than anything in a request.
fn destinations_due(
    store: &ComAmazonawsElasticacheAmazonelasticachev9State,
    now: SystemTime,
) -> BTreeSet<String> {
    let mut wanted = BTreeSet::new();
    let clusters = store
        .cache_clusters
        .values()
        .map(|cluster| &cluster.log_delivery_configurations);
    let groups = store.replication_groups.values().flat_map(|group| {
        [
            &group.log_delivery_configurations,
            &group.pending_modified_values.log_delivery_configurations,
        ]
    });
    for configurations in clusters.chain(groups) {
        for record in configurations {
            if record.due.is_some_and(|due| now >= due) {
                if let Some(log_group) = record.cloud_watch_log_group() {
                    wanted.insert(log_group.to_string());
                }
            }
        }
    }
    wanted
}

/// Every list of log-delivery configurations this store holds, so one settle pass reaches
/// each of them: the configurations of a cache cluster, those of a replication group, and the
/// ones a deferred change to a group is holding.
fn every_configuration_list(
    store: &mut ComAmazonawsElasticacheAmazonelasticachev9State,
) -> impl Iterator<Item = &mut Vec<LogDeliveryRecord>> {
    let clusters = store
        .cache_clusters
        .values_mut()
        .map(|cluster| &mut cluster.log_delivery_configurations);
    let groups = store.replication_groups.values_mut().flat_map(|group| {
        [
            &mut group.log_delivery_configurations,
            &mut group.pending_modified_values.log_delivery_configurations,
        ]
    });
    clusters.chain(groups)
}
