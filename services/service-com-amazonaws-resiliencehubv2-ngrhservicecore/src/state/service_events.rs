// SPDX-License-Identifier: CC0-1.0

//! The event log of one service: what `ListServiceEvents` reports.
//!
//! A `ServiceEvent` "represents an event in the service event log", so the log is a
//! RECORD of what has been done to a service rather than anything a caller writes: every
//! operation of this crate that CHANGES a service, or one of the resources that live
//! inside it (its service functions, their associated resources, its assertions, its input
//! sources, the system and policy it references), appends exactly one entry here, typed by
//! what it changed. Nothing else writes to it — a read of the log, or of the service,
//! never does.
//!
//! The entries are held inside the parent [`ServiceRecord`] ([`ServiceRecord::events`]),
//! the way its service functions and assertions are: the log is the log OF that service,
//! so deleting the service takes it with the rest of the record and no read can reach the
//! log of a service that is gone. That is also why no entry is appended for the delete
//! itself: the record the entry would live in is the one being removed.
//!
//! Every `ServiceEvent` body this crate returns is projected from a record here through
//! [`ServiceEventRecord::to_service_event`], so what an appender stored and what the
//! listing reports cannot drift.

use std::time::SystemTime;

use crate::ops::types::{
    AssertionCreatedMetadata, AssertionDeletedMetadata, AssertionUpdatedMetadata, EventActor,
    ServiceCreatedMetadata, ServiceEvent, ServiceEventDetails, ServiceEventMetadata,
    ServiceFunctionCreatedMetadata, ServiceFunctionDeletedMetadata,
    ServiceFunctionResourcesAddedMetadata, ServiceFunctionUpdatedMetadata,
    ServiceInputSourcesUpdatedMetadata, ServicePolicyAssociatedMetadata,
    ServicePolicyDisassociatedMetadata, ServiceSystemAssociatedMetadata,
    ServiceSystemDisassociatedMetadata,
};

use super::{
    to_date_time, ComAmazonawsResiliencehubv2NgrhservicecoreState, ServiceRecord,
    POLICY_VALUE_SOURCE_CROSS_ACCOUNT, POLICY_VALUE_SOURCE_SELF,
};

/// The ceiling on how many events one service's log keeps, so a caller repeating a
/// mutation cannot grow the process's memory without limit.
///
/// CHOSEN, not documented: the model publishes no retention for the log, so this mirrors
/// the ceiling the service records themselves are bounded by ([`super::MAX_SERVICES`]).
/// Past it the log keeps its most recent entries and drops the oldest, because a log is a
/// history whose newest end is the part a reader is looking for — an append always
/// succeeds, since refusing a mutation because its own log is full would refuse a call the
/// model has no error for.
pub const MAX_EVENTS_PER_SERVICE: usize = super::MAX_SERVICES;

/// `ActorType`'s value for an event a CALLER caused — "the type of actor, either USER or
/// SYSTEM". Every event this crate records is caused by a request, so every one of them
/// carries this value.
pub const ACTOR_TYPE_USER: &str = "USER";

/// The `ServiceEventType` values this crate records, one per kind of change its
/// implemented mutations can make. Each is a value the modelled `ServiceEventType` enum
/// publishes, so a caller can filter on it with `ListServiceEvents`' `eventTypes`.
pub const SERVICE_EVENT_CREATED: &str = "SERVICE_CREATED";
/// See [`SERVICE_EVENT_CREATED`].
pub const SERVICE_EVENT_SYSTEM_ASSOCIATED: &str = "SERVICE_SYSTEM_ASSOCIATED";
/// See [`SERVICE_EVENT_CREATED`].
pub const SERVICE_EVENT_SYSTEM_DISASSOCIATED: &str = "SERVICE_SYSTEM_DISASSOCIATED";
/// See [`SERVICE_EVENT_CREATED`].
pub const SERVICE_EVENT_INPUT_SOURCES_UPDATED: &str = "SERVICE_INPUT_SOURCES_UPDATED";
/// See [`SERVICE_EVENT_CREATED`].
pub const SERVICE_EVENT_POLICY_ASSOCIATED: &str = "SERVICE_POLICY_ASSOCIATED";
/// See [`SERVICE_EVENT_CREATED`].
pub const SERVICE_EVENT_POLICY_DISASSOCIATED: &str = "SERVICE_POLICY_DISASSOCIATED";
/// See [`SERVICE_EVENT_CREATED`].
pub const SERVICE_EVENT_FUNCTION_CREATED: &str = "SERVICE_FUNCTION_CREATED";
/// See [`SERVICE_EVENT_CREATED`].
pub const SERVICE_EVENT_FUNCTION_UPDATED: &str = "SERVICE_FUNCTION_UPDATED";
/// See [`SERVICE_EVENT_CREATED`].
pub const SERVICE_EVENT_FUNCTION_DELETED: &str = "SERVICE_FUNCTION_DELETED";
/// See [`SERVICE_EVENT_CREATED`].
pub const SERVICE_EVENT_FUNCTION_RESOURCES_ADDED: &str = "SERVICE_FUNCTION_RESOURCES_ADDED";
/// See [`SERVICE_EVENT_CREATED`].
pub const SERVICE_EVENT_ASSERTION_CREATED: &str = "ASSERTION_CREATED";
/// See [`SERVICE_EVENT_CREATED`].
pub const SERVICE_EVENT_ASSERTION_UPDATED: &str = "ASSERTION_UPDATED";
/// See [`SERVICE_EVENT_CREATED`].
pub const SERVICE_EVENT_ASSERTION_DELETED: &str = "ASSERTION_DELETED";

/// One entry of a service's event log, and the sole source of every `ServiceEvent` body
/// this crate returns.
#[derive(Debug, Clone)]
pub struct ServiceEventRecord {
    /// The minted `eventId`; unique for the life of the process.
    pub event_id: String,
    /// When the change this event records was made.
    pub timestamp: SystemTime,
    /// One of the `ServiceEventType` values above.
    pub event_type: String,
    /// Who caused it — see [`caller_actor`].
    pub actor: EventActor,
    /// The event's title, description and type-specific metadata.
    pub details: ServiceEventDetails,
    /// Position in the store's monotonic counter, so the log has a total order even for
    /// two events recorded in the same millisecond, and a `nextToken` can name a position
    /// in it the way every other listing in this crate does.
    pub sequence: u64,
}

impl ServiceEventRecord {
    /// This record as the `ServiceEvent` shape `ListServiceEvents` reports.
    ///
    /// `serviceArn` is the ARN of the service whose log this entry sits in — the log is
    /// nested in that service's record, so it is passed in rather than duplicated in every
    /// entry.
    pub fn to_service_event(&self, service_arn: &str) -> ServiceEvent {
        ServiceEvent {
            event_id: self.event_id.clone(),
            timestamp: to_date_time(self.timestamp),
            event_type: self.event_type.clone(),
            service_arn: service_arn.to_string(),
            actor: self.actor.clone(),
            event_details: self.details.clone(),
        }
    }
}

/// What a mutation DID to a service: one variant per `ServiceEventType` the implemented
/// mutations of this crate can cause.
///
/// An appender states the change in these terms and nothing more; which event type it is,
/// how it is worded and which metadata it carries are decided HERE, so the operations that
/// cause the same kind of change cannot describe it differently. Everything a variant does
/// not carry is resolved from the store at the moment the event is recorded (a policy's
/// name and owner, a system's name and id), so no appender has to look it up.
#[derive(Debug, Clone)]
pub enum ServiceChange {
    /// The service was created, under `name`.
    Created { name: String },
    /// A system was associated with the service.
    SystemAssociated { system_arn: String },
    /// A system the service referenced is no longer referenced.
    SystemDisassociated { system_arn: String },
    /// A resilience policy was attached to the service.
    PolicyAssociated { policy_arn: String },
    /// A resilience policy the service referenced is no longer attached.
    PolicyDisassociated { policy_arn: String },
    /// The service's input sources changed — one was created or deleted.
    InputSourcesUpdated,
    /// A service function was created within the service.
    FunctionCreated {
        service_function_id: String,
        name: String,
    },
    /// A service function of the service was updated.
    FunctionUpdated {
        service_function_id: String,
        name: String,
    },
    /// A service function of the service was deleted.
    FunctionDeleted {
        service_function_id: String,
        name: String,
    },
    /// Resources were associated with a service function of the service.
    FunctionResourcesAdded {
        service_function_id: String,
        name: String,
        /// The identifiers this call associated.
        resources: Vec<String>,
    },
    /// An assertion was created on the service.
    AssertionCreated { assertion_id: String },
    /// An assertion of the service was updated.
    AssertionUpdated { assertion_id: String },
    /// An assertion of the service was deleted.
    AssertionDeleted { assertion_id: String },
}

impl ServiceChange {
    /// The `ServiceEventType` this change is recorded under.
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::Created { .. } => SERVICE_EVENT_CREATED,
            Self::SystemAssociated { .. } => SERVICE_EVENT_SYSTEM_ASSOCIATED,
            Self::SystemDisassociated { .. } => SERVICE_EVENT_SYSTEM_DISASSOCIATED,
            Self::PolicyAssociated { .. } => SERVICE_EVENT_POLICY_ASSOCIATED,
            Self::PolicyDisassociated { .. } => SERVICE_EVENT_POLICY_DISASSOCIATED,
            Self::InputSourcesUpdated => SERVICE_EVENT_INPUT_SOURCES_UPDATED,
            Self::FunctionCreated { .. } => SERVICE_EVENT_FUNCTION_CREATED,
            Self::FunctionUpdated { .. } => SERVICE_EVENT_FUNCTION_UPDATED,
            Self::FunctionDeleted { .. } => SERVICE_EVENT_FUNCTION_DELETED,
            Self::FunctionResourcesAdded { .. } => SERVICE_EVENT_FUNCTION_RESOURCES_ADDED,
            Self::AssertionCreated { .. } => SERVICE_EVENT_ASSERTION_CREATED,
            Self::AssertionUpdated { .. } => SERVICE_EVENT_ASSERTION_UPDATED,
            Self::AssertionDeleted { .. } => SERVICE_EVENT_ASSERTION_DELETED,
        }
    }
}

/// The actor an event of this emulator carries.
///
/// The emulator speaks for exactly ONE IAM principal (`service_common::caller_principal`),
/// which is the identity every request here is made by and the one STS reports as the
/// caller, so that user is who the event names: a USER actor, its `principalId` the user's
/// own unique id, its `userName` that user's name, and `accountId` the account the service
/// belongs to.
pub fn caller_actor(account_id: &str) -> EventActor {
    EventActor {
        r#type: ACTOR_TYPE_USER.to_string(),
        principal_id: ::service_common::caller_principal::ambient_user_id(),
        account_id: Some(account_id.to_string()),
        user_name: Some(::service_common::caller_principal::AMBIENT_USER_NAME.to_string()),
    }
}

/// A fresh `eventId`.
///
/// `ServiceEvent$eventId` is typed `Uuid` (`len 36..36`, with the version and variant
/// nibbles its pattern pins), so it comes from this crate's one UUID minting path; the ARN
/// of the service the event belongs to is passed as that path's salt, and none of the id's
/// bits comes from the request.
fn mint_event_id(service_arn: &str) -> String {
    super::mint_uuid(service_arn)
}

impl ServiceRecord {
    /// This service's event log, in the order the events were recorded.
    ///
    /// Appends go on the end and nothing reorders them, so the slice is already in
    /// recording order — which is also ascending `sequence` order, the order every listing
    /// in this crate scans in and the one its page tokens name a position in.
    pub fn events_in_log_order(&self) -> &[ServiceEventRecord] {
        &self.events
    }
}

impl ComAmazonawsResiliencehubv2NgrhservicecoreState {
    /// Record `change` against the service stored under `service_arn`, as of `now`.
    ///
    /// Called by every mutation AFTER it has committed its change, so the log holds an
    /// entry only for a change the store really took. An ARN naming no stored service
    /// records nothing: there is no log to append to, which is the case a mutation that
    /// REMOVED the service itself is in.
    pub fn record_service_event(
        &mut self,
        service_arn: &str,
        change: &ServiceChange,
        now: SystemTime,
    ) {
        let Some(record) = self.services.get(service_arn) else {
            return;
        };
        let account_id = record.account_id.clone();
        let details = self.event_details(change, &account_id);
        let sequence = self.next_sequence();
        let Some(record) = self.services.get_mut(service_arn) else {
            return;
        };
        record.events.push(ServiceEventRecord {
            event_id: mint_event_id(service_arn),
            timestamp: now,
            event_type: change.event_type().to_string(),
            actor: caller_actor(&account_id),
            details,
            sequence,
        });
        // The log is bounded (see [`MAX_EVENTS_PER_SERVICE`]): past the ceiling the oldest
        // entries go, so the log keeps the newest ones.
        if record.events.len() > MAX_EVENTS_PER_SERVICE {
            let beyond_the_bound = record.events.len() - MAX_EVENTS_PER_SERVICE;
            record.events.drain(..beyond_the_bound);
        }
    }

    /// The `eventDetails` of `change`: its title, its description, and the one
    /// `ServiceEventMetadata` member the modelled union holds for its event type.
    ///
    /// The title and the description are the service's own prose for that kind of change;
    /// every value inside them, and every member of the metadata, is read from the request
    /// that caused the change or from the store, never invented. A member the store has
    /// nothing to fill (an assertion has no name, a resource identifier that is not an ARN
    /// has no representation in a member typed `Arn`) is left absent rather than filled
    /// with something nothing backs.
    fn event_details(&self, change: &ServiceChange, account_id: &str) -> ServiceEventDetails {
        match change {
            ServiceChange::Created { name } => details(
                "Service created",
                format!("Service {name} was created."),
                ServiceEventMetadata::serviceCreated(ServiceCreatedMetadata {}),
            ),
            ServiceChange::SystemAssociated { system_arn } => {
                let system = self.systems.get(system_arn);
                details(
                    "System associated",
                    format!("System {} was associated with the service.", self
                        .system_label(system_arn)),
                    ServiceEventMetadata::serviceSystemAssociated(
                        ServiceSystemAssociatedMetadata {
                            system_name: system.map(|record| record.name.clone()),
                            system_arn: Some(system_arn.clone()),
                        },
                    ),
                )
            }
            ServiceChange::SystemDisassociated { system_arn } => {
                let system = self.systems.get(system_arn);
                details(
                    "System disassociated",
                    format!(
                        "System {} was disassociated from the service.",
                        self.system_label(system_arn)
                    ),
                    ServiceEventMetadata::serviceSystemDisassociated(
                        ServiceSystemDisassociatedMetadata {
                            system_id: system.map(|record| record.system_id.clone()),
                            system_name: system.map(|record| record.name.clone()),
                            system_arn: Some(system_arn.clone()),
                        },
                    ),
                )
            }
            ServiceChange::PolicyAssociated { policy_arn } => details(
                "Policy associated",
                format!(
                    "Resilience policy {} was associated with the service.",
                    self.policy_label(policy_arn)
                ),
                ServiceEventMetadata::servicePolicyAssociated(ServicePolicyAssociatedMetadata {
                    policy_name: self.policies.get(policy_arn).map(|p| p.name.clone()),
                    policy_arn: Some(policy_arn.clone()),
                    policy_owner_account_id: policy_owner_account(policy_arn),
                    policy_source: policy_source(policy_arn, account_id),
                }),
            ),
            ServiceChange::PolicyDisassociated { policy_arn } => details(
                "Policy disassociated",
                format!(
                    "Resilience policy {} was disassociated from the service.",
                    self.policy_label(policy_arn)
                ),
                ServiceEventMetadata::servicePolicyDisassociated(
                    ServicePolicyDisassociatedMetadata {
                        policy_name: self.policies.get(policy_arn).map(|p| p.name.clone()),
                        policy_arn: Some(policy_arn.clone()),
                        policy_owner_account_id: policy_owner_account(policy_arn),
                        policy_source: policy_source(policy_arn, account_id),
                        // `reason` is a modelled enum of the reasons the SERVICE
                        // disassociates a policy on its own; this one was disassociated
                        // because the caller attached another, so there is no reason value
                        // to report.
                        reason: None,
                    },
                ),
            ),
            ServiceChange::InputSourcesUpdated => details(
                "Input sources updated",
                "The input sources of the service were updated.".to_string(),
                ServiceEventMetadata::serviceInputSourcesUpdated(
                    ServiceInputSourcesUpdatedMetadata {},
                ),
            ),
            ServiceChange::FunctionCreated {
                service_function_id,
                name,
            } => details(
                "Service function created",
                format!("Service function {name} was created."),
                ServiceEventMetadata::serviceFunctionCreated(ServiceFunctionCreatedMetadata {
                    service_function_id: Some(service_function_id.clone()),
                    service_function_name: Some(name.clone()),
                }),
            ),
            ServiceChange::FunctionUpdated {
                service_function_id,
                name,
            } => details(
                "Service function updated",
                format!("Service function {name} was updated."),
                ServiceEventMetadata::serviceFunctionUpdated(ServiceFunctionUpdatedMetadata {
                    service_function_id: Some(service_function_id.clone()),
                    service_function_name: Some(name.clone()),
                    // This operation changes a function's own attributes; the resources
                    // associated with it are changed by the operations that record
                    // [`ServiceChange::FunctionResourcesAdded`], so neither list is
                    // reported here.
                    resources_added: None,
                    resources_removed: None,
                }),
            ),
            ServiceChange::FunctionDeleted {
                service_function_id,
                name,
            } => details(
                "Service function deleted",
                format!("Service function {name} was deleted."),
                ServiceEventMetadata::serviceFunctionDeleted(ServiceFunctionDeletedMetadata {
                    service_function_id: Some(service_function_id.clone()),
                    service_function_name: Some(name.clone()),
                }),
            ),
            ServiceChange::FunctionResourcesAdded {
                service_function_id,
                name,
                resources,
            } => details(
                "Service function resources added",
                format!("Resources were added to service function {name}."),
                ServiceEventMetadata::serviceFunctionResourcesAdded(
                    ServiceFunctionResourcesAddedMetadata {
                        service_function_id: Some(service_function_id.clone()),
                        service_function_name: Some(name.clone()),
                        resources_added: Some(arns_among(resources)),
                    },
                ),
            ),
            ServiceChange::AssertionCreated { assertion_id } => details(
                "Assertion created",
                format!("Assertion {assertion_id} was created."),
                ServiceEventMetadata::assertionCreated(AssertionCreatedMetadata {
                    assertion_id: Some(assertion_id.clone()),
                    // An assertion carries text and no name, so there is no name to report.
                    assertion_name: None,
                }),
            ),
            ServiceChange::AssertionUpdated { assertion_id } => details(
                "Assertion updated",
                format!("Assertion {assertion_id} was updated."),
                ServiceEventMetadata::assertionUpdated(AssertionUpdatedMetadata {
                    assertion_id: Some(assertion_id.clone()),
                    assertion_name: None,
                }),
            ),
            ServiceChange::AssertionDeleted { assertion_id } => details(
                "Assertion deleted",
                format!("Assertion {assertion_id} was deleted."),
                ServiceEventMetadata::assertionDeleted(AssertionDeletedMetadata {
                    assertion_id: Some(assertion_id.clone()),
                    assertion_name: None,
                }),
            ),
        }
    }

    /// How a description names the system `system_arn`: by the name the store holds for it,
    /// falling back to the ARN itself when no system is stored under it.
    fn system_label(&self, system_arn: &str) -> String {
        self.systems
            .get(system_arn)
            .map_or_else(|| system_arn.to_string(), |record| record.name.clone())
    }

    /// How a description names the policy `policy_arn` — see [`Self::system_label`].
    fn policy_label(&self, policy_arn: &str) -> String {
        self.policies
            .get(policy_arn)
            .map_or_else(|| policy_arn.to_string(), |record| record.name.clone())
    }
}

/// Which account's policy `policy_arn` is, from the point of view of a service whose own
/// account is `account_id`: `SELF` for one that account owns, `CROSS_ACCOUNT` for another
/// account's. The same reading `effectivePolicyValues` takes of a policy reference.
fn policy_source(policy_arn: &str, account_id: &str) -> Option<String> {
    let owner = policy_owner_account(policy_arn)?;
    let source = if owner == account_id {
        POLICY_VALUE_SOURCE_SELF
    } else {
        POLICY_VALUE_SOURCE_CROSS_ACCOUNT
    };
    Some(source.to_string())
}

/// The account that owns `policy_arn`: the account field of the ARN, which is where a
/// resource's owning account lives.
fn policy_owner_account(policy_arn: &str) -> Option<String> {
    super::policies::arn_account(policy_arn).map(str::to_string)
}

/// The identifiers of `resources` that are ARNs.
///
/// `ServiceFunctionResourcesAddedMetadata$resourcesAdded` targets `ArnList`, so only an
/// identifier that IS an ARN has a representation in it; a service's resources are
/// discovered through its input sources and an identifier in another form (a physical
/// resource id, say) is left out rather than reported as an ARN it is not.
fn arns_among(resources: &[String]) -> Vec<String> {
    resources
        .iter()
        .filter(|identifier| ::service_common::arn::parse(identifier).is_some())
        .cloned()
        .collect()
}

/// A `ServiceEventDetails` from its three parts, so every event is assembled one way.
fn details(
    title: &str,
    description: String,
    metadata: ServiceEventMetadata,
) -> ServiceEventDetails {
    ServiceEventDetails {
        title: title.to_string(),
        description,
        event_metadata: Some(metadata),
    }
}
