// SPDX-License-Identifier: CC0-1.0

//! The REQUEST series a load balancer publishes for the traffic it answered, in the namespace
//! its own family owns.
//!
//! An answered request is not only an answer to its client. Elastic Load Balancing meters the
//! traffic it carries, and the counts are what a client WATCHES a load balancer by: how many
//! requests it processed, how many of them ended in which class of status, how long the target
//! took, how many bytes went through. A load balancer that answers requests and publishes
//! nothing leaves every alarm, dashboard and rate expression over those series without data,
//! so nothing a client builds on them can be observed at all.
//!
//! # What one request is counted into
//!
//! Every count here is read off ONE answered request — what the load balancer did with it and
//! what it was answered with ([`super::data_plane::Observed`]) — so the series and the
//! load balancer's own access log cannot disagree about a request:
//!
//! * the request itself is one request PROCESSED ([`REQUEST_COUNT`]);
//! * the status it ended in is counted into the class series of whichever end PRODUCED it: a
//!   status the TARGET answered goes to that end's class count ([`TARGET_CLASS_COUNTS`]) and
//!   to none of the load balancer's, and a status the LOAD BALANCER generated itself goes to
//!   the load balancer's class count ([`ELB_CLASS_COUNTS`]) and the series of that one status
//!   ([`ELB_STATUS_COUNTS`]) and to none of the target's. That split is the whole point of
//!   having two families of series: a 500 a workload returned is the workload's, and a 502 the
//!   load balancer generated because the workload gave it nothing usable is the load
//!   balancer's;
//! * the time the target took is the target's response time ([`TARGET_RESPONSE_TIME`]), for a
//!   request a target was called for at all;
//! * the terminating ACTION that answered is counted where it has a series of its own: a
//!   `fixed-response` ([`FIXED_RESPONSE_COUNT`]) and a `redirect` ([`REDIRECT_COUNT`]);
//! * a `forward` whose chosen target answered NOTHING is a connection that was not established
//!   ([`TARGET_CONNECTION_ERROR_COUNT`]);
//! * and a `forward` to a `lambda` target group carries that target type's own error series:
//!   where the load balancer answered the request itself, whether the failure was the
//!   FUNCTION's or the request's to it ([`LAMBDA_USER_ERROR`] — the invoke its resource policy
//!   admitted nobody to make, a run that raised, a returned object the response contract
//!   refuses, a response or a request body past the size one invocation carries) or neither
//!   ([`LAMBDA_INTERNAL_ERROR`]).
//!
//! What is NOT published here is the BYTE counting of that family's reference
//! (`ProcessedBytes` and its Lambda-target counterpart): those count "HTTP header and HTTP
//! payload" bytes of the connection, and a count of the bodies alone would report a magnitude
//! that is not the one the series means. A series whose value this plane cannot measure is left
//! unpublished rather than published wrong.
//!
//! A series is published only where the request gave it a NON-ZERO value, which is the
//! reporting criteria these series carry: a count of nothing is not a datapoint of zero, and a
//! reader that windows a series over a period the load balancer answered no such request in
//! folds no datapoint rather than a zero. That is what makes the ELB/target split readable at
//! all — a load balancer whose targets answered every request publishes no
//! load-balancer-generated class count, and a `Sum` over it is the sum of nothing.
//!
//! # Where the datapoints live, and which load balancers publish them
//!
//! In the CloudWatch emulator's own metric store, appended through the ONE write this service
//! makes into it ([`crate::state::host_counts::append_datapoints`]) — the same store
//! `PutMetricData` writes and `GetMetricStatistics` / `GetMetricData` / `ListMetrics` read, and
//! the same one the host counts of this load balancer's target groups go into — so one metric
//! has one home and an alarm over any of them is made due by the write.
//!
//! Each series is keyed by the resource the count is ABOUT. A count of what the load balancer
//! did carries its own dimension alone; a count of what one target group's targets did carries
//! the `TargetGroup` + `LoadBalancer` pair as well, so a reader can narrow a class count to the
//! group that answered rather than reading the load balancer's total.
//!
//! Only the APPLICATION family publishes these: they are the series of an HTTP REQUEST — a
//! status class, a terminating action, a target's response time — and only that family's
//! listeners answer one. A Network or Gateway Load Balancer's own metering is of flows and
//! bytes, which is not what is read here, and inventing an HTTP request series for it would
//! report traffic of a kind it never carried.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use crate::state::host_counts::{
    append_datapoints, epoch_secs, load_balancer_dimension_value, namespace,
    target_group_dimension_value, COUNT_UNIT, LOAD_BALANCER_DIMENSION, STORAGE_RESOLUTION_SECS,
    TARGET_GROUP_DIMENSION,
};
use crate::state::{
    ComAmazonawsElasticloadbalancingv2ElasticloadbalancingV10State as ElbState, LoadBalancerFamily,
};

/// "The number of requests processed over IPv4 and IPv6": one per request the load balancer
/// answered, and one for the target group it reached where it reached one.
pub const REQUEST_COUNT: &str = "RequestCount";

/// "The time elapsed, in seconds, after the request leaves the load balancer until a response
/// from the target is received" — which is exactly the span the plane measured around the call
/// it made to the target.
pub const TARGET_RESPONSE_TIME: &str = "TargetResponseTime";

/// "The number of fixed-response actions that were successful."
pub const FIXED_RESPONSE_COUNT: &str = "HTTP_Fixed_Response_Count";

/// "The number of redirect actions that were successful."
pub const REDIRECT_COUNT: &str = "HTTP_Redirect_Count";

/// "The number of connections that were not successfully established between the load balancer
/// and target."
pub const TARGET_CONNECTION_ERROR_COUNT: &str = "TargetConnectionErrorCount";

/// "The number of requests to a Lambda function that failed because of an issue with the
/// Lambda function" — the class of failure the function's own run, its response or the
/// permission on it produced ([`FUNCTION_FAILURE_REASONS`]).
pub const LAMBDA_USER_ERROR: &str = "LambdaUserError";

/// "The number of requests to a Lambda function that failed because of an issue internal to
/// the load balancer or AWS Lambda": a request that reached a `lambda` target group and was
/// answered by the load balancer itself for no failure OF the function.
pub const LAMBDA_INTERNAL_ERROR: &str = "LambdaInternalError";

/// The `StandardUnit` a response time carries: it is measured in seconds.
const SECONDS_UNIT: &str = "Seconds";

/// The series one status CLASS is counted into for a status the LOAD BALANCER generated
/// itself, by the hundreds digit of that status.
///
/// There is no `2XX` series among them: a load balancer generates a success for no request of
/// its own, and a `2XX` the client received came from the target.
const ELB_CLASS_COUNTS: [(u16, &str); 3] = [
    (3, "HTTPCode_ELB_3XX_Count"),
    (4, "HTTPCode_ELB_4XX_Count"),
    (5, "HTTPCode_ELB_5XX_Count"),
];

/// The series ONE load-balancer-generated status has of its own, beside the class count above:
/// the four statuses Elastic Load Balancing meters individually.
const ELB_STATUS_COUNTS: [(u16, &str); 4] = [
    (500, "HTTPCode_ELB_500_Count"),
    (502, "HTTPCode_ELB_502_Count"),
    (503, "HTTPCode_ELB_503_Count"),
    (504, "HTTPCode_ELB_504_Count"),
];

/// The series one status CLASS is counted into for a status the TARGET answered, by the
/// hundreds digit of that status. "This does not include any response codes generated by the
/// load balancer", which is what the split with [`ELB_CLASS_COUNTS`] carries.
const TARGET_CLASS_COUNTS: [(u16, &str); 4] = [
    (2, "HTTPCode_Target_2XX_Count"),
    (3, "HTTPCode_Target_3XX_Count"),
    (4, "HTTPCode_Target_4XX_Count"),
    (5, "HTTPCode_Target_5XX_Count"),
];

/// Every failure class an answer the load balancer generated for a `lambda` target names that
/// is an issue WITH the function or with the request made to it, rather than one internal to
/// the load balancer or to Lambda.
///
/// The one catalog of those classes is the access log's own
/// ([`super::access_logs::reason`]), so the record a request leaves and the series it is
/// counted into name the same failure.
const FUNCTION_FAILURE_REASONS: [&str; 5] = [
    super::access_logs::reason::LAMBDA_ACCESS_DENIED,
    super::access_logs::reason::LAMBDA_UNHANDLED,
    super::access_logs::reason::LAMBDA_INVALID_RESPONSE,
    super::access_logs::reason::LAMBDA_RESPONSE_TOO_LARGE,
    super::access_logs::reason::LAMBDA_REQUEST_TOO_LARGE,
];

/// One answered request, as the series it is counted into see it.
///
/// Everything here was observed while the request was being answered: nothing is read back off
/// a store that may have moved on, and nothing is invented for a request this plane did not
/// observe.
pub(super) struct Answered<'a> {
    /// The load balancer that answered the request.
    pub(super) load_balancer_arn: &'a str,
    /// The status the CLIENT was answered with.
    pub(super) elb_status: u16,
    /// The status the TARGET answered, where a target answered one — which no answer the load
    /// balancer generated itself carries.
    pub(super) target_status: Option<u16>,
    /// The target group the request was forwarded to, where it was forwarded at all.
    pub(super) target_group_arn: Option<&'a str>,
    /// The failure class of an answer the load balancer generated itself
    /// ([`super::access_logs::reason`]).
    pub(super) error_reason: Option<&'a str>,
    /// How long the target took, where one was called.
    pub(super) target_elapsed: Option<Duration>,
    /// The terminating actions the listener performed.
    pub(super) actions: &'a [String],
}

/// Count one answered request into the series it belongs to.
///
/// The store is read under ONE lock that is released before the metric store is touched, so no
/// two services' stores are held at once. A load balancer this store no longer holds — deleted
/// while its last request was in flight — is counted into nothing, and so is one of a family
/// that carries no request series of this kind.
pub(super) fn counted<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<ElbState>,
    answered: &Answered<'_>,
    now: SystemTime,
) {
    let Some(subject) = subject(state, answered) else {
        return;
    };
    append_datapoints(engine, datapoints(&subject, answered, now), now);
}

/// What the series of one answered request are KEYED by: the namespace the load balancer's own
/// family publishes into, how a dimension names that load balancer, how one names the target
/// group that answered where a group did, and whether that group's targets are functions.
struct Subject {
    namespace: &'static str,
    load_balancer: String,
    target_group: Option<String>,
    /// Whether the target group the request reached registers a Lambda function, which is what
    /// gives it the `Lambda*` series of its own.
    lambda_targets: bool,
}

/// Read that subject out of the store, or `None` for a load balancer whose family publishes no
/// request series.
fn subject(
    state: &::engine::ServiceState<ElbState>,
    answered: &Answered<'_>,
) -> Option<Subject> {
    let store = state.lock();
    let record = store.load_balancers.get(answered.load_balancer_arn)?;
    if record.family() != LoadBalancerFamily::Application {
        return None;
    }
    Some(Subject {
        namespace: namespace(&record.lb_type),
        load_balancer: load_balancer_dimension_value(record),
        target_group: answered
            .target_group_arn
            .and_then(target_group_dimension_value),
        lambda_targets: answered
            .target_group_arn
            .and_then(|arn| store.target_groups.get(arn))
            .is_some_and(crate::state::TargetGroupRecord::is_lambda),
    })
}

/// Every datapoint one answered request is published as.
fn datapoints(
    subject: &Subject,
    answered: &Answered<'_>,
    now: SystemTime,
) -> Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> {
    let mut counted = Counted {
        subject,
        stamp: epoch_secs(now),
        published: Vec::new(),
    };
    // The request itself.
    counted.load_balancer(REQUEST_COUNT, 1.0, COUNT_UNIT);
    counted.target_group(REQUEST_COUNT, 1.0, COUNT_UNIT);
    // The terminating action that answered, where it has a series of its own — read against
    // the very `Action$Type` values the plane performs them under, so a renamed action cannot
    // leave a series uncounted.
    let performed = |action_type: &str| {
        answered
            .actions
            .iter()
            .any(|performed| performed == action_type)
    };
    if performed(super::data_plane::FIXED_RESPONSE_ACTION) {
        counted.load_balancer(FIXED_RESPONSE_COUNT, 1.0, COUNT_UNIT);
    }
    if performed(super::data_plane::REDIRECT_ACTION) {
        counted.load_balancer(REDIRECT_COUNT, 1.0, COUNT_UNIT);
    }
    // The status, counted into the class series of whichever end PRODUCED it.
    match answered.target_status {
        Some(status) => {
            for (class, metric) in TARGET_CLASS_COUNTS {
                if status / 100 == class {
                    counted.both(metric, 1.0, COUNT_UNIT);
                }
            }
        }
        None => {
            for (class, metric) in ELB_CLASS_COUNTS {
                if answered.elb_status / 100 == class {
                    counted.load_balancer(metric, 1.0, COUNT_UNIT);
                }
            }
            for (status, metric) in ELB_STATUS_COUNTS {
                if answered.elb_status == status {
                    counted.load_balancer(metric, 1.0, COUNT_UNIT);
                }
            }
        }
    }
    // How long the target took, for a request a target was called for at all.
    if let Some(elapsed) = answered.target_elapsed {
        counted.both(TARGET_RESPONSE_TIME, elapsed.as_secs_f64(), SECONDS_UNIT);
    }
    // What the target TYPE of the group that answered carries of its own.
    if subject.lambda_targets {
        if answered
            .error_reason
            .is_some_and(|reason| FUNCTION_FAILURE_REASONS.contains(&reason))
        {
            counted.both(LAMBDA_USER_ERROR, 1.0, COUNT_UNIT);
        } else if answered.target_status.is_none() {
            counted.both(LAMBDA_INTERNAL_ERROR, 1.0, COUNT_UNIT);
        }
    } else if subject.target_group.is_some() && answered.target_status.is_none() {
        // A `forward` whose chosen target answered nothing at all: the load balancer could
        // not establish the connection it needed to pass the request on.
        counted.both(TARGET_CONNECTION_ERROR_COUNT, 1.0, COUNT_UNIT);
    }
    counted.published
}

/// The datapoints of one request as they are collected, keyed by the resource each count is
/// ABOUT.
struct Counted<'a> {
    subject: &'a Subject,
    /// The epoch seconds every datapoint of this request carries: the instant the request was
    /// answered, so one request lands in one period of every series it is counted into.
    stamp: f64,
    published: Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)>,
}

impl Counted<'_> {
    /// Count `value` into the series of `metric` the LOAD BALANCER publishes: its own
    /// dimension alone.
    fn load_balancer(&mut self, metric: &str, value: f64, unit: &str) {
        let dimensions = [(
            LOAD_BALANCER_DIMENSION.to_string(),
            self.subject.load_balancer.clone(),
        )]
        .into_iter()
        .collect();
        self.push(metric, dimensions, value, unit);
    }

    /// Count `value` into the series of `metric` the TARGET GROUP that answered publishes: the
    /// `TargetGroup` + `LoadBalancer` pair. A request no target group answered is counted into
    /// nothing here.
    fn target_group(&mut self, metric: &str, value: f64, unit: &str) {
        let Some(target_group) = self.subject.target_group.clone() else {
            return;
        };
        let dimensions = [
            (TARGET_GROUP_DIMENSION.to_string(), target_group),
            (
                LOAD_BALANCER_DIMENSION.to_string(),
                self.subject.load_balancer.clone(),
            ),
        ]
        .into_iter()
        .collect();
        self.push(metric, dimensions, value, unit);
    }

    /// Count `value` into BOTH series of `metric`: the load balancer's own total and the
    /// target group's narrowing of it, which is how a reader windows one group's share of what
    /// the load balancer carried.
    fn both(&mut self, metric: &str, value: f64, unit: &str) {
        self.load_balancer(metric, value, unit);
        self.target_group(metric, value, unit);
    }

    /// One datapoint of one series: a single observation of `value`.
    ///
    /// A value of nothing is NOT published: these series report a datapoint only where there is
    /// a non-zero value, so a period the load balancer answered no such request in holds no
    /// datapoint at all rather than a zero.
    fn push(
        &mut self,
        metric: &str,
        dimensions: BTreeMap<String, String>,
        value: f64,
        unit: &str,
    ) {
        if !value.is_finite() || value <= 0.0 {
            return;
        }
        self.published.push((
            cloudwatch::MetricKey {
                namespace: self.subject.namespace.to_string(),
                metric_name: metric.to_string(),
                dimensions,
            },
            cloudwatch::StoredDatum {
                timestamp_secs: self.stamp,
                // One observation: this request's own.
                sample_count: 1.0,
                sum: value,
                minimum: value,
                maximum: value,
                unit: Some(unit.to_string()),
                storage_resolution: STORAGE_RESOLUTION_SECS,
            },
        ));
    }
}
