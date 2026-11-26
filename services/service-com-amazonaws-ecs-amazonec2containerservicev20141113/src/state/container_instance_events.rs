// SPDX-License-Identifier: CC0-1.0

//! The EventBridge events this service publishes as a CONTAINER INSTANCE moves.
//!
//! # The rule this module implements
//!
//! Amazon ECS is an event SOURCE for the capacity a cluster holds, not only for the tasks
//! that run on it ([`super::task_events`]): every change a container instance EXPERIENCES is
//! published onto the account's default event bus ([`DEFAULT_EVENT_BUS`]) under the `source`
//! this service owns ([`EVENT_SOURCE`]) and the `detail-type` of a container instance's state
//! change ([`CONTAINER_INSTANCE_STATE_CHANGE_DETAIL_TYPE`]), carrying the instance Amazon ECS
//! holds at that instant in the event's `detail` — the same projection
//! `DescribeContainerInstances` answers with, so the event and the API agree about the
//! instance — and the instance's own ARN in the envelope's `resources`. A rule on that bus
//! whose `EventPattern` matches then delivers it to the rule's targets, so an operator that
//! reacts to capacity joining, draining or going away observes it without calling Amazon ECS
//! again.
//!
//! WHAT counts as a change: what the instance REPORTS of itself — its `status`, whether its
//! agent is connected, the version counter Amazon ECS bumps for every change it experiences
//! ("every time a container instance experiences a change ... the version counter is
//! incremented") and the tasks it is carrying. An instance is published the first time it is
//! seen, which is its registration, and again every time any of those changes: the agent
//! registering it, a caller draining it, a deregistration taking it out of service, the agent's
//! connection timing out behind a machine that stopped, an attribute written on it, or a task
//! being placed on or stopped from it. A pass that finds nothing changed publishes nothing, so
//! two calls at one instant publish the same single event.
//!
//! # How a change becomes an event
//!
//! Nothing here decides WHEN an instance changes: the writers and the stored due instants do.
//! This module only COMPARES what the store now holds against what was last published
//! ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::record_container_instance_state_changes`]),
//! so a change is published exactly once however it was made and no writer has to remember to
//! announce itself — the registration the agent makes on its own
//! ([`super::ec2_capacity`]) included. The queue it feeds is bounded
//! ([`MAX_PENDING_CONTAINER_INSTANCE_EVENTS`], oldest dropped first) because every change
//! appends to it.
//!
//! The queue is drained with no lock held, by the same clock pump that publishes a task's
//! moves ([`super::task_events::drive_task_clock`]) and through the same in-process hop, so
//! this crate names no other service's crate and a container instance's change reaches a
//! rule's targets with no further Amazon ECS call. Publication is best effort — an emulator
//! with no EventBridge mounted has nowhere to publish — so a failure is logged and the
//! operation's own answer stands.

use super::*;

/// The `detail-type` of the event published when a CONTAINER INSTANCE changes state.
pub const CONTAINER_INSTANCE_STATE_CHANGE_DETAIL_TYPE: &str =
    "ECS Container Instance State Change";

/// How many container-instance changes the queue holds at once.
///
/// This module's OWN ceiling, not an AWS quota, for the reason the task queue has one
/// ([`super::task_events::MAX_PENDING_EVENTS`]): the queue lives in memory and every change
/// appends to it. The oldest change is dropped first, so the changes the live instances just
/// made are the ones that survive. It is far above what a realistic burst produces — the
/// instances themselves are capped ([`MAX_CONTAINER_INSTANCES`]) and one contributes a handful
/// of changes.
pub const MAX_PENDING_CONTAINER_INSTANCE_EVENTS: usize = 2_000;

/// What a container instance REPORTS of itself, as the comparison reads it: the status it is
/// in, whether its agent is connected, the version counter Amazon ECS bumps for every change
/// it experiences, and the tasks it is carrying. A change to any of them is a change an event
/// reports.
pub type ContainerInstanceReportedState = (String, bool, i64, i32, i32);

/// One container-instance change waiting to be published, rendered against the instance as it
/// stood when the change was observed.
#[derive(Debug, Clone)]
pub struct ContainerInstanceStateChange {
    /// The instance that changed, named by the ARN the envelope's `resources` carries.
    pub container_instance_arn: String,
    /// WHEN it changed: the instant the registration happened at for the first event about an
    /// instance, and the instant the change was observed at for every later one.
    pub at: SystemTime,
    /// The rendered `detail`: the instance, as `DescribeContainerInstances` reports it.
    pub detail: ::serde_json::Value,
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Queue a state-change event for every container instance whose reported state differs
    /// from the one last published for it, as of `now`.
    ///
    /// This is the ONE place a container instance's change becomes an event, so no writer can
    /// forget to announce one and none can be published twice: the comparison is against what
    /// was published, not against who wrote it. An instance this store no longer holds cannot
    /// change again, so its entry goes with it.
    ///
    /// The changes are queued oldest change first, so events about several instances leave in
    /// the order the instances changed rather than in the order the map happened to be walked.
    pub fn record_container_instance_state_changes(&mut self, now: SystemTime) {
        let instances = &self.container_instances;
        self.published_container_instance_state
            .retain(|arn, _| instances.contains_key(arn));
        let arns: Vec<String> = self.container_instances.keys().cloned().collect();
        let mut observed: Vec<(
            String,
            ContainerInstanceReportedState,
            ContainerInstanceStateChange,
        )> = Vec::new();
        for arn in arns {
            // The projection every container-instance operation answers with, so the event
            // reports the instance a `DescribeContainerInstances` of the same instant reports
            // and never a second, invented copy of it. The tags are left out: they are what an
            // `include` asks a describe for, and the event body carries the instance itself.
            let Some(view) =
                self.container_instance_view(&arn, ContainerInstanceFields::default())
            else {
                continue;
            };
            let reported: ContainerInstanceReportedState = (
                view.status.clone().unwrap_or_default(),
                view.agent_connected,
                view.version,
                view.running_tasks_count,
                view.pending_tasks_count,
            );
            let published = self.published_container_instance_state.get(&arn);
            if published == Some(&reported) {
                continue;
            }
            // The FIRST event about an instance is its registration, which happened when the
            // agent registered it rather than when this comparison ran; every later one
            // reports the instant the change was observed at, there being no stamp on the
            // record for a change other than the registration.
            let at = match published {
                None => view
                    .registered_at
                    .and_then(|at| SystemTime::try_from(at).ok())
                    .unwrap_or(now),
                Some(_) => now,
            };
            let cluster_arn = self
                .container_instances
                .get(&arn)
                // The instance's own ARN carries the Region it was registered in, which is
                // the Region its cluster lives in ([`super::cluster_region_of_arn`]).
                .and_then(|instance| {
                    self.cluster(
                        super::cluster_region_of_arn(&instance.arn),
                        &instance.cluster_name,
                    )
                })
                .map(|cluster| cluster.arn.clone());
            let detail = container_instance_state_change_detail(&view, cluster_arn, at);
            observed.push((
                arn.clone(),
                reported,
                ContainerInstanceStateChange {
                    container_instance_arn: arn,
                    at,
                    detail,
                },
            ));
        }
        observed.sort_by_key(|(_, _, change)| change.at);
        for (arn, reported, change) in observed {
            self.published_container_instance_state.insert(arn, reported);
            while self.pending_container_instance_events.len()
                >= MAX_PENDING_CONTAINER_INSTANCE_EVENTS
            {
                self.pending_container_instance_events.pop_front();
                ::tracing::warn!(
                    "the queue of ECS container instance state changes waiting to be published \
                     to EventBridge is full at {MAX_PENDING_CONTAINER_INSTANCE_EVENTS}; the \
                     oldest change was dropped"
                );
            }
            self.pending_container_instance_events.push_back(change);
        }
    }

    /// Take at most `limit` of the changes not yet published, oldest first.
    ///
    /// Taking them out under the lock is what stops two concurrent calls from publishing one
    /// change twice.
    pub(super) fn take_container_instance_state_changes(
        &mut self,
        limit: usize,
    ) -> Vec<ContainerInstanceStateChange> {
        let taking = limit.min(self.pending_container_instance_events.len());
        self.pending_container_instance_events
            .drain(..taking)
            .collect()
    }

    /// Whether any container-instance change is still waiting to be published — what keeps the
    /// clock pump awake until the bus has them.
    pub(super) fn has_pending_container_instance_state_changes(&self) -> bool {
        !self.pending_container_instance_events.is_empty()
    }

    /// How many container-instance changes are queued for publication — reporting only, for a
    /// reader that has to tell "nothing changed" from "nothing was published".
    pub fn pending_container_instance_event_count(&self) -> usize {
        self.pending_container_instance_events.len()
    }
}

/// The `detail` of an ECS container instance state change: the CONTAINER INSTANCE, as the
/// store's own projection reports it.
///
/// Every member is read off that projection, so the event reports the instance a
/// `DescribeContainerInstances` of the same instant reports. `clusterArn` names the cluster
/// the capacity belongs to, and `updatedAt` is the instant of THIS change, which is what tells
/// two events about one instance apart.
fn container_instance_state_change_detail(
    view: &ContainerInstance,
    cluster_arn: Option<String>,
    at: SystemTime,
) -> ::serde_json::Value {
    let mut detail = ::serde_json::Map::new();
    super::task_events::insert_text(
        &mut detail,
        "containerInstanceArn",
        view.container_instance_arn.clone(),
    );
    super::task_events::insert_text(&mut detail, "clusterArn", cluster_arn);
    super::task_events::insert_text(&mut detail, "ec2InstanceId", view.ec2_instance_id.clone());
    super::task_events::insert_text(
        &mut detail,
        "capacityProviderName",
        view.capacity_provider_name.clone(),
    );
    super::task_events::insert_text(&mut detail, "status", view.status.clone());
    super::task_events::insert_text(&mut detail, "statusReason", view.status_reason.clone());
    super::task_events::insert_text(
        &mut detail,
        "agentUpdateStatus",
        view.agent_update_status.clone(),
    );
    detail.insert(
        "agentConnected".to_string(),
        ::serde_json::Value::Bool(view.agent_connected),
    );
    detail.insert(
        "runningTasksCount".to_string(),
        ::serde_json::Value::from(view.running_tasks_count),
    );
    detail.insert(
        "pendingTasksCount".to_string(),
        ::serde_json::Value::from(view.pending_tasks_count),
    );
    detail.insert(
        "version".to_string(),
        ::serde_json::Value::from(view.version),
    );
    super::task_events::insert_instant(&mut detail, "registeredAt", view.registered_at);
    super::task_events::insert_instant(&mut detail, "updatedAt", Some(reported_instant(at)));
    if let Some(info) = view.version_info.as_ref() {
        detail.insert("versionInfo".to_string(), version_info_detail(info));
    }
    detail.insert(
        "registeredResources".to_string(),
        resources_detail(view.registered_resources.as_deref().unwrap_or_default()),
    );
    detail.insert(
        "remainingResources".to_string(),
        resources_detail(view.remaining_resources.as_deref().unwrap_or_default()),
    );
    detail.insert(
        "attributes".to_string(),
        ::serde_json::Value::Array(
            view.attributes
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(attribute_detail)
                .collect(),
        ),
    );
    detail.insert(
        "attachments".to_string(),
        ::serde_json::Value::Array(Vec::new()),
    );
    ::serde_json::Value::Object(detail)
}

/// The agent and runtime versions the instance reports, as the `detail` carries them.
fn version_info_detail(info: &VersionInfo) -> ::serde_json::Value {
    let mut rendered = ::serde_json::Map::new();
    super::task_events::insert_text(&mut rendered, "agentVersion", info.agent_version.clone());
    super::task_events::insert_text(&mut rendered, "agentHash", info.agent_hash.clone());
    super::task_events::insert_text(
        &mut rendered,
        "dockerVersion",
        info.docker_version.clone(),
    );
    ::serde_json::Value::Object(rendered)
}

/// A list of the instance's resources, as the `detail` carries it.
fn resources_detail(resources: &[Resource]) -> ::serde_json::Value {
    ::serde_json::Value::Array(resources.iter().map(resource_detail).collect())
}

/// ONE resource of the instance: its name, the kind of value it is measured as
/// ("Valid values: INTEGER, DOUBLE, LONG, or STRINGSET") and the value itself.
fn resource_detail(resource: &Resource) -> ::serde_json::Value {
    let mut rendered = ::serde_json::Map::new();
    super::task_events::insert_text(&mut rendered, "name", resource.name.clone());
    super::task_events::insert_text(&mut rendered, "type", resource.r#type.clone());
    rendered.insert(
        "doubleValue".to_string(),
        ::serde_json::Value::from(resource.double_value),
    );
    rendered.insert(
        "longValue".to_string(),
        ::serde_json::Value::from(resource.long_value),
    );
    rendered.insert(
        "integerValue".to_string(),
        ::serde_json::Value::from(resource.integer_value),
    );
    rendered.insert(
        "stringSetValue".to_string(),
        ::serde_json::Value::Array(
            resource
                .string_set_value
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|value| ::serde_json::Value::String(value.clone()))
                .collect(),
        ),
    );
    ::serde_json::Value::Object(rendered)
}

/// ONE attribute of the instance, as the `detail` carries it — the same four members the
/// `Attribute` projection reports.
fn attribute_detail(attribute: &Attribute) -> ::serde_json::Value {
    let mut rendered = ::serde_json::Map::new();
    super::task_events::insert_text(&mut rendered, "name", Some(attribute.name.clone()));
    super::task_events::insert_text(&mut rendered, "value", attribute.value.clone());
    super::task_events::insert_text(&mut rendered, "targetType", attribute.target_type.clone());
    super::task_events::insert_text(&mut rendered, "targetId", attribute.target_id.clone());
    ::serde_json::Value::Object(rendered)
}

/// Publish one drain of container-instance changes onto the bus.
///
/// The hop itself is the one a task's own moves travel
/// ([`super::task_events::publish_events`]): one `PutEvents` per window of entries, best
/// effort, with nothing about an operation's own answer depending on it.
pub(super) async fn publish_container_instance_state_changes(
    client: &::aws_sdk_eventbridge::Client,
    changes: &[ContainerInstanceStateChange],
) {
    let entries: Vec<(&str, SystemTime, &::serde_json::Value)> = changes
        .iter()
        .map(|change| {
            (
                change.container_instance_arn.as_str(),
                change.at,
                &change.detail,
            )
        })
        .collect();
    super::task_events::publish_events(
        client,
        CONTAINER_INSTANCE_STATE_CHANGE_DETAIL_TYPE,
        &entries,
    )
    .await;
}
