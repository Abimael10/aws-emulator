// SPDX-License-Identifier: CC0-1.0

//! The EventBridge events this service publishes as a BUILD changes state.
//!
//! # The rule this module implements
//!
//! CodeBuild does not only record a build's progress for a reader to poll: every change of
//! a build's `buildStatus` is PUBLISHED onto the account's default event bus
//! ([`DEFAULT_EVENT_BUS`]) under the `source` this service owns ([`EVENT_SOURCE`]) and the
//! `detail-type` of a build's state change ([`BUILD_STATE_CHANGE_DETAIL_TYPE`]), carrying
//! the build's ARN in the envelope's `resources` and, in the event's `detail`, which project
//! the build belongs to, the build's own id, the status it moved into and the phase it is in
//! ([`build_state_change_detail`]). A rule on that bus whose `EventPattern` matches then
//! delivers it to that rule's targets, so a client that gates a pipeline or a notification on
//! build state observes the build without calling CodeBuild again — and a rule whose pattern
//! constrains `detail.project-name` to one project receives that project's build events and
//! not a sibling project's.
//!
//! The `detail` members are the ones the AWS-managed schema of this event declares
//! (`CodeBuildBuildStateChange`: `build-id`, `build-status`, `project-name` and `version`
//! required, `current-phase` and `current-phase-context` beside them), each read off the
//! stored build, so the event reports the build a `BatchGetBuilds` of the same instant
//! reports and never a second, invented copy of it.
//!
//! # How a transition becomes an event
//!
//! Nothing here decides WHEN a build moves: `StartBuild` and `RetryBuild` put it in progress,
//! the run the clock owes it ([`super::build_run`]) gives it its outcome and `StopBuild` ends
//! it. This module is a DIFF rather than a second state machine:
//! [`ComAmazonawsCodebuildCodebuild20161006State::record_build_state_changes`] compares every
//! stored build's status against the status last published for it
//! (`published_build_statuses`, held in the store beside the builds) and queues one change per
//! status that differs — whichever call or clock-owed run materialised it, and without any
//! transition site having to remember to announce itself. A status that has not changed owes
//! nothing however many times this runs.
//!
//! A queued change is published by [`publish_build_state_changes`], which runs with no store
//! guard held: EventBridge is reached the way every emulated service reaches another, through
//! the framework's in-process callback, so no socket is opened and nothing outside this
//! process is reachable from it. Publication is best effort — an emulator with no EventBridge
//! mounted has nowhere to publish — so a failure is traced and the operation's own answer
//! stands.
//!
//! The queue is BOUNDED ([`MAX_PENDING_BUILD_EVENTS`], oldest dropped first) because every
//! transition appends to it, and the snapshot map is pruned to the builds the store still
//! holds, so neither grows with the number of calls served.

use ::std::collections::VecDeque;

use ::smithy_server_runtime::EmbeddedEmulator;

use super::{build_arn, project_name_from_resource_id, ComAmazonawsCodebuildCodebuild20161006State,
    StoredBuild};
use crate::ops::types::Build;

/// The `source` of every event CodeBuild publishes.
///
/// EventBridge names the publishing service in an event's `source` as `aws.<service>`, and
/// this service's namespace is the one its own ARNs carry ([`super::ARN_NAMESPACE`]).
pub const EVENT_SOURCE: &str = "aws.codebuild";

/// The `detail-type` of the event published when a BUILD changes state.
pub const BUILD_STATE_CHANGE_DETAIL_TYPE: &str = "CodeBuild Build State Change";

/// The event bus this service publishes onto.
///
/// The events an AWS service generates go to the account's default event bus, which is the
/// bus a rule carrying no `EventBusName` is created on.
pub const DEFAULT_EVENT_BUS: &str = "default";

/// The `detail.version` this event carries: the version of the event's own detail schema,
/// which `CodeBuildBuildStateChange` declares as a string and CodeBuild publishes as `1`.
pub const BUILD_STATE_CHANGE_DETAIL_VERSION: &str = "1";

/// The `detail.current-phase-context` of a build whose current phase carries no context at
/// all: the empty list, which is what the member's bracketed-list form spells.
pub const EMPTY_PHASE_CONTEXT: &str = "[]";

/// How many build state changes the queue holds at once.
///
/// This module's OWN ceiling, not an AWS quota: the queue lives in memory and every
/// transition appends to it, so without a bound a client starting builds without end could
/// grow it without end. The oldest change is dropped first, so the transitions the builds
/// that just moved made are the ones that survive. It is far above what a realistic burst
/// produces — a build contributes one change per status it moves into.
pub const MAX_PENDING_BUILD_EVENTS: usize = 2_000;

/// How many entries one `PutEvents` request carries.
///
/// The bound the service this publishes to declares: its `PutEventsRequestEntryList` is
/// `@length(min: 1, max: 10)`, so a drain holding more changes than that is published in
/// several requests rather than one the service would refuse.
const MAX_PUT_EVENTS_ENTRIES: usize = 10;

/// How many changes ONE drain publishes.
///
/// The queue itself is bounded ([`MAX_PENDING_BUILD_EVENTS`]), but one call draining all of
/// it would fan out to a rule's targets that many times: this caps one drain, and
/// [`publish_build_state_changes`] repeats a drain until the queue is empty, so nothing
/// waits for a later call.
const MAX_PUBLISHED_PER_DRAIN: usize = 50;

/// One build state change waiting to be published.
///
/// The `detail` is rendered when the change is RECORDED, so the event reports the build as it
/// stood at that instant rather than as a later drain finds it.
#[derive(Debug, Clone)]
pub struct BuildStateChange {
    /// The build's ARN, which the event envelope carries in `resources`: the event names the
    /// resource it is about, which is what a rule filtering on `resources` reads.
    pub build_arn: String,
    /// WHEN the build moved, in epoch milliseconds — the instant the build's own record
    /// stamps on the transition, not the instant a later call published it.
    pub at_millis: i64,
    /// The rendered `detail` of the event.
    pub detail: ::serde_json::Value,
}

impl ComAmazonawsCodebuildCodebuild20161006State {
    /// Queue one event for every stored build whose `buildStatus` differs from the status
    /// last published for it, and take that status to be the published one.
    ///
    /// This is the whole of the "which transitions owe an event" rule: the status the store
    /// holds is compared against the last one published, so every move — a build starting, a
    /// run the clock owed giving it its outcome, a `StopBuild` ending it — owes exactly one
    /// event, and a status that has not changed owes none however many times this runs.
    ///
    /// `region` is the Region the event is published in, which is the Region whose ARN a
    /// build that states none is named by.
    pub fn record_build_state_changes(&mut self, region: &str) {
        // The builds, the snapshot and the queue are DISJOINT fields of the store, so a
        // build is read while the snapshot and the queue are written.
        let Self {
            builds,
            published_build_statuses: published,
            pending_build_events: pending,
            ..
        } = self;
        for (id, stored) in builds.iter() {
            let status = stored.build.build_status.clone().unwrap_or_default();
            if published.get(id).map(String::as_str) == Some(status.as_str()) {
                continue;
            }
            published.insert(id.clone(), status.clone());
            // A record that states no status at all moved into nothing a `build-status` could
            // report, so nothing is published for it rather than an empty status.
            if status.is_empty() {
                continue;
            }
            let arn = stored
                .build
                .arn
                .clone()
                .unwrap_or_else(|| build_arn(region, id));
            enqueue(
                pending,
                BuildStateChange {
                    at_millis: state_change_millis(stored),
                    detail: build_state_change_detail(&stored.build, &arn, id),
                    build_arn: arn,
                },
            );
        }
        // A snapshot belongs to a build: one the store no longer holds (`BatchDeleteBuilds`
        // removed it, or the batch build it belonged to was deleted) can never differ again.
        published.retain(|id, _| builds.contains_key(id));
    }

    /// Take at most [`MAX_PUBLISHED_PER_DRAIN`] queued changes out of the store, oldest
    /// first — one batch of the drain [`publish_build_state_changes`] repeats until the queue
    /// is empty.
    ///
    /// Taking the changes out under the guard is what stops two concurrent calls from
    /// publishing one change twice.
    fn take_pending_build_events(&mut self) -> Vec<BuildStateChange> {
        let taking = MAX_PUBLISHED_PER_DRAIN.min(self.pending_build_events.len());
        self.pending_build_events.drain(..taking).collect()
    }

    /// How many changes are queued for publication — reporting only, for a reader that has to
    /// tell "nothing moved" from "nothing was published".
    #[must_use]
    pub fn pending_build_event_count(&self) -> usize {
        self.pending_build_events.len()
    }
}

/// Append `change` to the queue, dropping the oldest once the queue is full.
fn enqueue(pending: &mut VecDeque<BuildStateChange>, change: BuildStateChange) {
    while pending.len() >= MAX_PENDING_BUILD_EVENTS {
        pending.pop_front();
        ::tracing::warn!(
            "the queue of CodeBuild build state changes waiting to be published to EventBridge \
             is full at {MAX_PENDING_BUILD_EVENTS}; the oldest change was dropped"
        );
    }
    pending.push_back(change);
}

/// The instant the build's current status was reached at: the instant it ENDED for a build
/// that has, and the instant it STARTED for one still in progress — the stamps the record
/// itself carries, so the event's `time` is when the build moved.
fn state_change_millis(stored: &StoredBuild) -> i64 {
    stored
        .build
        .end_time
        .as_ref()
        .and_then(|at| at.to_millis().ok())
        .unwrap_or(stored.start_millis)
}

/// The `detail` of a build state change: which project the build belongs to, the build's own
/// id, the status it moved into, the phase it is in and the context that phase carries.
///
/// The members are the AWS-managed `CodeBuildBuildStateChange` schema's, each read off the
/// stored build. `build-id` is the build's ARN, which is also what the envelope's `resources`
/// names. A build whose record states no project is named by the project its id carries — a
/// build is keyed by `<projectName>:<uuid>` — so the member a rule filters on is always the
/// project the build really belongs to.
fn build_state_change_detail(
    build: &Build,
    build_arn: &str,
    build_id: &str,
) -> ::serde_json::Value {
    let mut detail = ::serde_json::Map::new();
    detail.insert(
        "build-status".to_string(),
        ::serde_json::Value::String(build.build_status.clone().unwrap_or_default()),
    );
    detail.insert(
        "project-name".to_string(),
        ::serde_json::Value::String(
            build
                .project_name
                .clone()
                .or_else(|| project_name_from_resource_id(build_id).map(str::to_string))
                .unwrap_or_default(),
        ),
    );
    detail.insert(
        "build-id".to_string(),
        ::serde_json::Value::String(build_arn.to_string()),
    );
    // The phase the build is in. A record that states none states no member either, rather
    // than a phase it never reached.
    if let Some(phase) = build.current_phase.as_ref() {
        detail.insert(
            "current-phase".to_string(),
            ::serde_json::Value::String(phase.clone()),
        );
    }
    detail.insert(
        "current-phase-context".to_string(),
        ::serde_json::Value::String(current_phase_context(build)),
    );
    detail.insert(
        "version".to_string(),
        ::serde_json::Value::String(BUILD_STATE_CHANGE_DETAIL_VERSION.to_string()),
    );
    ::serde_json::Value::Object(detail)
}

/// The `current-phase-context` of `build`: the contexts the phase the build is in carries,
/// as the bracketed, comma-separated list the member is worded as — [`EMPTY_PHASE_CONTEXT`]
/// for a phase that carries none, which is every phase of a build that has failed at nothing.
///
/// One context is its `statusCode` and its `message` (`PhaseContext`), in the order the phase
/// records them; a context that states neither says nothing, so it contributes no entry
/// rather than an empty one.
fn current_phase_context(build: &Build) -> String {
    let Some(current) = build.current_phase.as_deref() else {
        return EMPTY_PHASE_CONTEXT.to_string();
    };
    let entries: Vec<String> = build
        .phases
        .iter()
        .flatten()
        .filter(|phase| phase.phase_type.as_deref() == Some(current))
        .flat_map(|phase| phase.contexts.iter().flatten())
        .filter_map(|context| {
            let status_code = context.status_code.clone().unwrap_or_default();
            let message = context.message.clone().unwrap_or_default();
            match (status_code.is_empty(), message.is_empty()) {
                (true, true) => None,
                (true, false) => Some(message),
                (false, true) => Some(status_code),
                (false, false) => Some(format!("{status_code}: {message}")),
            }
        })
        .collect();
    format!("[{}]", entries.join(", "))
}

/// Record every build state change this store now holds and publish each one onto the
/// account's default EventBridge bus, leaving the queue empty.
///
/// Called with NO guard of this store held: it takes the guard itself to compare the builds
/// against what has been published and to take each batch of changes out — so nothing is
/// published twice — and the in-process `PutEvents` runs outside it. The queue is drained
/// until it is EMPTY: a batch is capped ([`MAX_PUBLISHED_PER_DRAIN`]) so no single request is
/// unbounded, and the loop is what guarantees the tail of a burst is published by the call
/// that produced it rather than waiting for the next one.
///
/// Nothing about the answer the operation gives changes either way, for the reason the module
/// docs give.
pub async fn publish_build_state_changes(
    callback: &EmbeddedEmulator,
    state: &::engine::ServiceState<ComAmazonawsCodebuildCodebuild20161006State>,
    region: &str,
) {
    // The guard is taken and dropped inside this statement.
    let mut changes = {
        let mut store = state.lock();
        store.record_build_state_changes(region);
        store.take_pending_build_events()
    };
    if changes.is_empty() {
        return;
    }
    let client = ::service_common::eventbridge_client::eventbridge_client(callback, region);
    while !changes.is_empty() {
        publish_batch(&client, &changes).await;
        // Whatever a concurrent call left behind: the loop ends on the empty queue, which
        // every batch brings closer because a batch is only ever removed from it.
        changes = state.lock().take_pending_build_events();
    }
}

/// Publish one batch of changes, in `PutEvents` requests of at most
/// [`MAX_PUT_EVENTS_ENTRIES`] entries.
async fn publish_batch(client: &::aws_sdk_eventbridge::Client, changes: &[BuildStateChange]) {
    for window in changes.chunks(MAX_PUT_EVENTS_ENTRIES) {
        let mut request = client.put_events();
        for change in window {
            let entry = ::aws_sdk_eventbridge::types::PutEventsRequestEntry::builder()
                .event_bus_name(DEFAULT_EVENT_BUS)
                .source(EVENT_SOURCE)
                .detail_type(BUILD_STATE_CHANGE_DETAIL_TYPE)
                // The event's `time` is WHEN the build moved, not when the queue was
                // drained: `PutEventsRequestEntry$Time` is "the time stamp of the event, per
                // RFC3339. If no time stamp is provided, the time stamp of the PutEvents
                // call is used."
                .time(::aws_sdk_eventbridge::primitives::DateTime::from_millis(
                    change.at_millis,
                ))
                .resources(&change.build_arn)
                .detail(change.detail.to_string())
                .build();
            request = request.entries(entry);
        }
        if let Err(error) = request.send().await {
            ::tracing::warn!(
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "a CodeBuild build state change could not be published to EventBridge"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::{BuildPhase, PhaseContext};

    /// A build of `project` in `status`, reporting `phase` as the phase it is in, whose
    /// phases carry `contexts` on that phase.
    fn build(
        project: &str,
        status: &str,
        phase: Option<&str>,
        contexts: Vec<(&str, &str)>,
    ) -> Build {
        Build {
            project_name: Some(project.to_string()),
            build_status: Some(status.to_string()),
            current_phase: phase.map(str::to_string),
            phases: phase.map(|phase| {
                vec![BuildPhase {
                    phase_type: Some(phase.to_string()),
                    contexts: Some(
                        contexts
                            .iter()
                            .map(|(status_code, message)| PhaseContext {
                                status_code: Some((*status_code).to_string()),
                                message: Some((*message).to_string()),
                            })
                            .collect(),
                    ),
                    ..BuildPhase::default()
                }]
            }),
            ..Build::default()
        }
    }

    /// One row of the phase-context table below: the case, the contexts the phase carries as
    /// `(statusCode, message)` pairs, and the rendering they produce.
    type ContextRow = (
        &'static str,
        Vec<(&'static str, &'static str)>,
        &'static str,
    );

    /// A build id of the form the store keys a build by, `<projectName>:<uuid>`: the uuid
    /// half has to BE one ([`project_name_from_resource_id`]), so the project half of a real
    /// id is what is read back off it.
    fn build_id(project: &str, nibble: char) -> String {
        let group = |count: usize| -> String { ::std::iter::repeat_n(nibble, count).collect() };
        format!(
            "{project}:{}-{}-{}-{}-{}",
            group(8),
            group(4),
            group(4),
            group(4),
            group(12)
        )
    }

    /// RULE: the `detail` of a build state change names the project the build belongs to, the
    /// build's ARN, the status it moved into and the phase it is in — whatever that status
    /// is, because the event reports the transition rather than one outcome.
    ///
    /// The rows span the `StatusType` values a build's `buildStatus` takes
    /// (`smithy_query(query="shape",shape="StatusType")`: SUCCEEDED, FAILED, FAULT,
    /// TIMED_OUT, IN_PROGRESS, STOPPED).
    #[test]
    fn a_state_change_details_the_build_that_moved() {
        for status in [
            "IN_PROGRESS",
            "SUCCEEDED",
            "FAILED",
            "FAULT",
            "TIMED_OUT",
            "STOPPED",
        ] {
            let id = build_id("a-project", '1');
            let arn = format!("arn:aws:codebuild:us-west-2:000000000000:build/{id}");
            let detail = build_state_change_detail(
                &build("a-project", status, Some("QUEUED"), Vec::new()),
                &arn,
                &id,
            );
            for (member, expected) in [
                ("build-status", status),
                ("project-name", "a-project"),
                ("build-id", arn.as_str()),
                ("current-phase", "QUEUED"),
                ("current-phase-context", EMPTY_PHASE_CONTEXT),
                ("version", BUILD_STATE_CHANGE_DETAIL_VERSION),
            ] {
                assert_eq!(
                    detail[member],
                    ::serde_json::Value::String(expected.to_string()),
                    "case {status:?}: expected detail.{member} {expected:?}, got {detail}"
                );
            }
        }
    }

    /// RULE: the `project-name` a rule filters on is the project the build really belongs to,
    /// so a record that states none is named by the project its own id carries.
    #[test]
    fn a_record_stating_no_project_is_named_by_its_id() {
        let mut record = build("ignored", "IN_PROGRESS", None, Vec::new());
        record.project_name = None;
        let id = build_id("named-by-the-id", '2');
        let detail = build_state_change_detail(&record, "arn", &id);
        assert_eq!(
            detail["project-name"],
            ::serde_json::json!("named-by-the-id"),
            "expected the project the id carries, got {detail}"
        );
    }

    /// RULE: `current-phase-context` is the contexts the phase the build is in carries, as a
    /// bracketed list — empty for a phase that carries none.
    ///
    /// The rows differ along what the rule generalises over: how many contexts the phase
    /// carries, and which halves of one are stated.
    #[test]
    fn the_phase_context_lists_what_the_current_phase_carries() {
        // (case, the phase's contexts, the rendering)
        let rows: Vec<ContextRow> = vec![
            ("a phase that carries none", Vec::new(), "[]"),
            (
                "one context",
                vec![("CLIENT_ERROR", "the build failed")],
                "[CLIENT_ERROR: the build failed]",
            ),
            (
                "two contexts",
                vec![("CLIENT_ERROR", "first"), ("CLIENT_ERROR", "second")],
                "[CLIENT_ERROR: first, CLIENT_ERROR: second]",
            ),
            (
                "a context that states only a code",
                vec![("CLIENT_ERROR", "")],
                "[CLIENT_ERROR]",
            ),
            (
                "a context that states only a message",
                vec![("", "the build failed")],
                "[the build failed]",
            ),
            ("a context that states neither", vec![("", "")], "[]"),
        ];
        for (case, contexts, expected) in rows {
            let actual =
                current_phase_context(&build("a-project", "FAILED", Some("BUILD"), contexts));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a status that has not changed owes no event, and every status a build moves into
    /// owes exactly one — however many times the diff runs.
    #[test]
    fn one_event_is_queued_per_status_a_build_moves_into() {
        let mut store = ComAmazonawsCodebuildCodebuild20161006State::default();
        let id = build_id("a-project", '3');
        store.insert_build(
            id.clone(),
            StoredBuild {
                build: build("a-project", "IN_PROGRESS", Some("QUEUED"), Vec::new()),
                start_millis: 1_700_000_000_000,
                settle_due_millis: 1_700_000_000_750,
                artifacts: None,
                secondary_artifacts: Vec::new(),
                run_claimed: false,
            },
        );
        store.record_build_state_changes("us-west-2");
        store.record_build_state_changes("us-west-2");
        assert_eq!(
            store.pending_build_event_count(),
            1,
            "a build that started owes one event, however many times the diff runs"
        );
        if let Some(stored) = store.builds.get_mut(&id) {
            stored.build.build_status = Some("SUCCEEDED".to_string());
        }
        store.record_build_state_changes("us-west-2");
        assert_eq!(
            store.pending_build_event_count(),
            2,
            "the status the build moved into owes a second event"
        );
        let queued: Vec<String> = store
            .pending_build_events
            .iter()
            .map(|change| change.detail["build-status"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            queued,
            vec!["IN_PROGRESS".to_string(), "SUCCEEDED".to_string()],
            "expected one event per status, in the order the build moved, got {queued:?}"
        );
    }
}
