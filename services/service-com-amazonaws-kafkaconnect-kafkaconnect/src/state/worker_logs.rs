// SPDX-License-Identifier: CC0-1.0

//! What a connector's WORKERS write about the connector they are running, and
//! where those records go.
//!
//! A connector's `logDelivery` is not a value a client reads back: it is an
//! instruction. `CloudWatchLogsLogDelivery` carries "whether log delivery to
//! Amazon CloudWatch Logs is enabled" and "the name of the CloudWatch log group
//! that is the destination for log delivery", so a connector carrying one
//! enabled DELIVERS the records its workers write into that group as it walks its
//! lifecycle. The rule this module implements: every time the clock brings a
//! connector to rest — the bring-up a create started, and the return from a
//! restart or an update — its workers write that outcome as one log event, and
//! the event is delivered to the destination the connector's own `logDelivery`
//! names. A connector whose `logDelivery` names no enabled CloudWatch Logs group
//! has nowhere to write and writes nothing, rather than having its records
//! diverted to a destination it did not name.
//!
//! The group is the ACCOUNT'S, named by a caller that created it and pointed the
//! connector at it, so the write goes into the group CloudWatch Logs already
//! holds and creates none
//! ([`::service_common::cloudwatch_logs::delivered_into_held_group`], the one
//! vended-logs write in this tree): a destination nothing created loses the
//! records, which is what makes a misconfigured destination observable.
//!
//! The clock, not a call, is what moves a connector, so a record is WRITTEN when
//! the state it reports is reached ([`ComAmazonawsKafkaconnectKafkaconnectState::settle_due`])
//! and held until a handler can deliver it: delivery awaits, and `settle_due`
//! runs under this service's state guard. Every handler that drives a connector
//! hands the held records over once it has released that guard
//! ([`deliver_worker_logs`]), so a record is delivered at the first call after
//! the worker wrote it and never twice.

use super::*;

/// One log event a connector's worker wrote, with the destination the
/// connector's `logDelivery` named for it.
///
/// The destination is kept WITH the record rather than looked up at delivery
/// time, because the record belongs to the connector as it was when the worker
/// wrote it: a connector deleted before its last records are delivered still
/// wrote them.
#[derive(Debug, Clone)]
pub struct WorkerLogRecord {
    /// The Region the destination log group lives in: the connector's own.
    pub region: String,
    /// The CloudWatch Logs log group `logDelivery` named.
    pub log_group: String,
    /// The log stream inside it this connector's worker records go into.
    pub log_stream: String,
    /// The record itself.
    pub message: String,
}

impl ConnectorRecord {
    /// The log group this connector's worker records are delivered to, or `None`
    /// when its `logDelivery` names no enabled CloudWatch Logs destination.
    ///
    /// `enabled` is what decides it — "whether log delivery to Amazon CloudWatch
    /// Logs is enabled" — and a destination that is enabled without naming a
    /// group names no group to write to, so neither one delivers.
    fn worker_log_group(&self) -> Option<&str> {
        let logs = self
            .log_delivery
            .as_ref()?
            .worker_log_delivery
            .cloud_watch_logs
            .as_ref()?;
        logs.enabled
            .then_some(logs.log_group.as_deref())
            .flatten()
            .filter(|group| !group.is_empty())
    }

    /// The log stream this connector's worker records go into: the connector's
    /// own identity, so one group holds the records of every connector pointed
    /// at it without two connectors' records sharing a stream.
    ///
    /// The identity is the connector's name and the id its ARN carries (the last
    /// segment of the ARN's resource), which together name exactly one connector
    /// even after a connector of that name has been deleted and created again.
    fn worker_log_stream(&self) -> String {
        let id = self.arn.rsplit('/').next().unwrap_or_default();
        format!("{}-{id}", self.name)
    }

    /// The record this connector's worker writes about having come to rest in the
    /// state it is now in, for the destination its `logDelivery` names — or
    /// nothing at all when it names none.
    ///
    /// One record per resting state reached, naming the connector and what
    /// happened to it: a connector that is running reports its workers and tasks
    /// as running, and one that failed reports what its `stateDescription` says
    /// went wrong, which is the whole reason a caller turns worker logs on.
    pub fn worker_log_records(&self) -> Vec<WorkerLogRecord> {
        let Some(log_group) = self.worker_log_group() else {
            return Vec::new();
        };
        let message = if self.state == CONNECTOR_FAILED {
            let reason = self
                .state_description
                .as_ref()
                .and_then(|description| description.message.clone())
                .unwrap_or_else(|| "the connector could not be started".to_string());
            format!(
                "ERROR Kafka Connect worker for connector {} could not start: {reason}",
                self.name
            )
        } else {
            format!(
                "INFO Kafka Connect worker for connector {} started; the connector and its tasks \
                 are {}",
                self.name, self.state
            )
        };
        vec![WorkerLogRecord {
            region: self.region.clone(),
            log_group: log_group.to_string(),
            log_stream: self.worker_log_stream(),
            message,
        }]
    }
}

/// Deliver every worker log record this service is holding, in the order the
/// workers wrote them.
///
/// Called at the END of a handler that drives a connector, with NO state guard
/// held: this awaits, and the records are taken out of the store before the
/// first hand-over so a concurrent call delivers none of them twice.
///
/// Delivery is BEST EFFORT, as it is for every vended-logs destination: a group
/// the account has since deleted, or a write CloudWatch Logs refuses, loses the
/// record and is reported in tracing rather than failing the call the client has
/// already been answered.
pub async fn deliver_worker_logs<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
) {
    let Some(store) = service_state(engine) else {
        return;
    };
    let held: Vec<WorkerLogRecord> = {
        let mut state = store.lock();
        ::std::mem::take(&mut state.worker_log_records)
    };
    // One hand-over per destination, in the order the records were written: the
    // records of one stream travel together, which is what keeps them in that
    // order inside the stream.
    let mut delivered = 0;
    while delivered < held.len() {
        let destination = &held[delivered];
        let batch: Vec<String> = held[delivered..]
            .iter()
            .take_while(|record| {
                record.region == destination.region
                    && record.log_group == destination.log_group
                    && record.log_stream == destination.log_stream
            })
            .map(|record| record.message.clone())
            .collect();
        delivered += batch.len();
        ::service_common::cloudwatch_logs::delivered_into_held_group(
            ctx,
            &destination.region,
            &destination.log_group,
            &destination.log_stream,
            &batch,
        )
        .await;
    }
}
