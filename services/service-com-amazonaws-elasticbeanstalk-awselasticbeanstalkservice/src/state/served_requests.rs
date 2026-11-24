// SPDX-License-Identifier: CC0-1.0

//! The REQUESTS an environment's application has answered, and what the
//! enhanced-health reports derive from them.
//!
//! Enhanced health reports what the health agent on the instance observes of the
//! application: `ApplicationMetrics` is "application request metrics for an
//! environment" over "the most recent time slice of 10 seconds" with
//! `StatusCodes` — "the percentage of requests over the last 10 seconds that
//! resulted in each type of status code" — and the per-instance status the agent
//! derives from them is documented by what it is reporting: `Warning` is "the
//! health agent is reporting a moderate number of request failures or other
//! issues", `Degraded` "a high number", and `Ok` "an instance is passing health
//! checks and the health agent is not reporting any problems".
//!
//! So those reports are not about the environment RECORD: they are about the
//! traffic the application really answered. This module is that observation —
//! every answer the environment's address served, with the status the
//! application set and the instant it answered ([`Environment::record_served_answer`],
//! called by the data plane once the application has answered) — and what the
//! reports read off it: the metrics over the slice they cover
//! ([`Environment::application_metrics`]) and the health the failures in them
//! amount to ([`Environment::reported_health`]).
//!
//! An environment nothing has dialled has answered no request, so its metrics are
//! empty and its health is the one the record carries: the agent reports no
//! problem it has not observed.

use super::*;

/// One answer the environment's application served: the status the application
/// set, and when it answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServedAnswer {
    /// When the application answered.
    pub at: Instant,
    /// The status it answered with.
    pub status: u16,
}

/// How many answers one environment keeps.
///
/// An EMULATOR bound on what the record holds: the answers inside the windows
/// below are what the reports read, and anything older is dropped as it is
/// recorded, so this is only the ceiling on a burst inside one window.
pub const MAX_SERVED_ANSWERS: usize = 256;

/// The span the health agent JUDGES an application's failures over.
///
/// CHOSEN: AWS states no number for how long a failure the agent observed goes on
/// counting against an instance's health, only that the status is what the agent
/// is reporting ("a moderate number of request failures", "a high number"). It is
/// wider than the slice the metrics cover, because an instance that failed a
/// request a moment ago is one the agent is still reporting a failure for — an
/// environment is reported healthy again once it has gone this long without
/// failing a request.
pub const HEALTH_JUDGEMENT_SPAN: Duration = Duration::from_secs(60);

/// The share of answers that have to be server failures for the agent to report a
/// MODERATE number of request failures (`Warning`, Yellow), a HIGH number
/// (`Degraded`, Red) and a VERY HIGH number (`Severe`, Red).
///
/// CHOSEN: AWS publishes the health colours and statuses by what the agent is
/// reporting rather than by a percentage, so these are this emulator's reading of
/// "moderate", "high" and "very high" — a few failures in the window are a
/// warning, a fifth of the traffic failing is a degraded instance, and an
/// application failing more requests than it answers is a severe one.
pub const MODERATE_FAILURE_SHARE: f64 = 0.05;
/// See [`MODERATE_FAILURE_SHARE`].
pub const HIGH_FAILURE_SHARE: f64 = 0.20;
/// See [`MODERATE_FAILURE_SHARE`].
pub const VERY_HIGH_FAILURE_SHARE: f64 = 0.50;

/// The lowest status that is a SERVER failure, which is what the agent counts
/// against an instance's health: a 4xx answer is the client's request being
/// refused, which the metrics count separately and the health does not.
const SERVER_FAILURE_STATUS: u16 = 500;

impl Environment {
    /// Record that this environment's application answered one request with
    /// `status`.
    ///
    /// Called by the data plane once the application has answered
    /// ([`crate::service::environment_plane`]): what the agent reports is what the
    /// application really answered. Answers older than the span the agent judges
    /// are dropped here, so the record holds the observation the reports read and
    /// not the whole history of the address.
    pub fn record_served_answer(&mut self, status: u16) {
        let at = Instant::now();
        self.served_answers
            .retain(|answer| at.duration_since(answer.at) < HEALTH_JUDGEMENT_SPAN);
        let excess = (self.served_answers.len() + 1).saturating_sub(MAX_SERVED_ANSWERS);
        if excess > 0 {
            self.served_answers.drain(..excess);
        }
        self.served_answers.push(ServedAnswer { at, status });
    }

    /// The answers this environment's application served inside `span`.
    fn answers_within(&self, span: Duration) -> Vec<u16> {
        let now = Instant::now();
        self.served_answers
            .iter()
            .filter(|answer| now.duration_since(answer.at) < span)
            .map(|answer| answer.status)
            .collect()
    }

    /// "Application request metrics for an environment" over "the most recent time
    /// slice of 10 seconds" ([`METRICS_DURATION_SECONDS`]): how many requests the
    /// application answered in it, and how many of them it answered with each
    /// class of status.
    ///
    /// Zero requests for an address nothing has dialled in that slice, and then no
    /// latency percentiles either: there is nothing to report percentiles over.
    pub fn application_metrics(&self) -> ApplicationMetrics {
        let statuses = self.answers_within(Duration::from_secs(
            u64::try_from(METRICS_DURATION_SECONDS).unwrap_or_default(),
        ));
        let counted = |class: u16| {
            Some(
                i32::try_from(
                    statuses
                        .iter()
                        .filter(|status| **status >= class && **status < class + 100)
                        .count(),
                )
                .unwrap_or(i32::MAX),
            )
        };
        ApplicationMetrics {
            duration: Some(METRICS_DURATION_SECONDS),
            request_count: i32::try_from(statuses.len()).unwrap_or(i32::MAX),
            status_codes: Some(StatusCodes {
                status2xx: counted(200),
                status3xx: counted(300),
                status4xx: counted(400),
                status5xx: counted(500),
            }),
            // "Represents the average latency for the slowest X percent of
            // requests over the last 10 seconds": the plane measures no latency of
            // its own, so none is reported rather than numbers nothing timed.
            latency: None,
        }
    }

    /// The health COLOUR and health STATUS this environment reports: the record's
    /// own, and the one the agent's observation of the application's failures
    /// amounts to where that is worse.
    ///
    /// The record's own comes first for everything that is not the agent's to
    /// report: an environment mid-operation has its "health checks suspended
    /// during an UpdateEnvironment or RestartEnvironment request" and reports
    /// `Grey`, and one that has never been dialled has no observation behind it.
    /// Over a RUNNING environment the agent's reading decides: a moderate number
    /// of request failures is a `Warning` (Yellow) instance, a high number a
    /// `Degraded` (Red) one and a very high number a `Severe` (Red) one, which is
    /// the whole point of enhanced health.
    pub fn reported_health(&self) -> (String, Option<String>) {
        let recorded = (self.health.clone(), self.health_status.clone());
        if self.health != HEALTH_GREEN {
            return recorded;
        }
        match self.observed_failure_share() {
            Some(share) if share >= VERY_HIGH_FAILURE_SHARE => (
                HEALTH_RED.to_string(),
                Some(INSTANCE_HEALTH_SEVERE.to_string()),
            ),
            Some(share) if share >= HIGH_FAILURE_SHARE => (
                HEALTH_RED.to_string(),
                Some(INSTANCE_HEALTH_DEGRADED.to_string()),
            ),
            Some(share) if share >= MODERATE_FAILURE_SHARE => (
                HEALTH_YELLOW.to_string(),
                Some(INSTANCE_HEALTH_WARNING.to_string()),
            ),
            _ => recorded,
        }
    }

    /// The share of the answers inside [`HEALTH_JUDGEMENT_SPAN`] that were SERVER
    /// failures, or `None` when the application answered no request in it — about
    /// which the agent reports nothing.
    pub fn observed_failure_share(&self) -> Option<f64> {
        let statuses = self.answers_within(HEALTH_JUDGEMENT_SPAN);
        if statuses.is_empty() {
            return None;
        }
        let failed = statuses
            .iter()
            .filter(|status| **status >= SERVER_FAILURE_STATUS)
            .count();
        // Both counts come from the same list, so the share is of exactly the
        // answers judged.
        Some(failed as f64 / statuses.len() as f64)
    }

    /// What the agent is reporting about the application's requests, as one of
    /// "the descriptions of the data that contributed to the environment's current
    /// health status" — and `None` for an environment it is reporting no request
    /// failure for.
    pub fn request_failure_cause(&self) -> Option<String> {
        let share = self.observed_failure_share()?;
        if share < MODERATE_FAILURE_SHARE {
            return None;
        }
        let percentage = (share * 100.0).round() as i64;
        Some(format!(
            "{percentage}% of the requests to the environment are failing with HTTP status codes 5xx."
        ))
    }
}

impl ComAmazonawsElasticbeanstalkAwselasticbeanstalkserviceState {
    /// Record that the application of the environment `environment_id` answered
    /// one request with `status`.
    ///
    /// The one write the data plane performs on this store: what the agent reports
    /// of an application is what that application answered, so the answer is
    /// recorded where every reader of this environment reads it.
    pub fn record_served_answer(&mut self, environment_id: &str, status: u16) {
        if let Some(environment) = self.environments.get_mut(environment_id) {
            environment.record_served_answer(status);
        }
    }
}
