// SPDX-License-Identifier: CC0-1.0

//! The LOG FILES a cluster's NODES leave in Amazon S3.
//!
//! "LogUri — the location in Amazon S3 to write the log files of the job flow"
//! (`RunJobFlow$LogUri` doc). A step's own directory is one part of what is written there
//! ([`super::step_logs`]); this is the other: Amazon EMR's log push agent runs ON THE NODES, so
//! the files a node produces exist under that location too, keyed by the node they are of —
//! `<LogUri>/<cluster-id>/node/<instance-id>/` ([`NODE_SEGMENT`]), the recorded layout, which
//! this service's Smithy model states nothing about. A client that waits for a cluster to come
//! up and then lists its log location therefore finds the files its nodes wrote, and not only
//! the ones its steps did.
//!
//! # What is written, and from what
//!
//! Per node, the files whose text this service really holds:
//!
//! * one directory per BOOTSTRAP ACTION the node ran, `bootstrap-actions/<position>/`
//!   ([`BOOTSTRAP_ACTIONS_SEGMENT`]), holding that run's two console streams and the
//!   controller's own account of it ([`bootstrap_controller_log_text`]). The streams are the
//!   script's own: what the session that performed it wrote while it ran
//!   ([`super::bootstrap_run`]), so a script that printed a line leaves that line in its
//!   `stdout.gz` and a script that printed nothing leaves that file empty — which is what a
//!   real action that printed nothing leaves too. A run this emulator performed NOTHING of
//!   claims no exit status and no stream: the controller file states what the run came to, and
//!   no file says a script ran that none did;
//! * the node's own INSTANCE STATE file, `instance-state/instance-state.log-<stamp>.gz`
//!   ([`INSTANCE_STATE_SEGMENT`]) — what this service recorded about the instance the node is:
//!   the cluster it belongs to, the instance Amazon EC2 minted for it, its type and purchasing
//!   option, the collection it was provisioned by, its private address and hostname, and the
//!   instants of its own timeline ([`instance_state_log_text`]). Every value is read off the
//!   stored record, so the file cannot disagree with what `ListInstances` reports, and a fact
//!   the record does not carry is written by nothing.
//!
//! Each file is gzipped, as the agent pushes them ([`gzipped`]).
//!
//! # When they are written
//!
//! By the CLOCK, from the instant the node has something to push: a bootstrap action's
//! directory from the instant its run was ANSWERED for, and the node's instance-state file from
//! the instant the node came UP (its `ReadyDateTime`). Both belong to the node rather than to
//! any call, so whichever later operation reports the cluster drains what is owed
//! ([`deliver_due_node_logs`]) with this service's store already released. Nothing sleeps for
//! it, nothing is written for a node that is still provisioning or an action nothing has
//! answered for, and one file is written once however many calls arrive afterwards
//! ([`BootstrapActionRun::log_delivered`], [`ClusterInstanceRecord::node_log_delivered`]). The
//! files outlive the cluster, exactly as the real ones do: nothing deletes them when its nodes
//! go away.
//!
//! Delivery is BEST EFFORT and is the NODE's push, decided by the permissions in force on the
//! role those nodes run as — both exactly as a step's directory is, and for the same reasons,
//! which [`super::step_logs`] states in full.

use super::*;

/// The key segment under one cluster's log prefix that keys the directories by NODE.
pub const NODE_SEGMENT: &str = "node";

/// The segment under one node's prefix that keys the directories by bootstrap action.
pub const BOOTSTRAP_ACTIONS_SEGMENT: &str = "bootstrap-actions";

/// The segment under one node's prefix holding the node's own instance-state file.
pub const INSTANCE_STATE_SEGMENT: &str = "instance-state";

/// How the node's instance-state file is named, before the stamp that keys one push of it.
pub const INSTANCE_STATE_LOG_OBJECT: &str = "instance-state.log";

/// The form the stamp in that name writes an instant in: the date and time, in UTC, to the
/// minute — the resolution the file name carries one at.
const INSTANCE_STATE_STAMP_FORMAT: &str = "%Y-%m-%d-%H-%M";

/// The form a log line writes an instant in, the same form the step controller's own lines use
/// ([`super::step_logs`]).
const NODE_LOG_TIME_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";

/// How many of a cluster's nodes and runs ONE call delivers files for, leaving the rest to the
/// next call that reports the cluster. CHOSEN: the ceiling on what one REQUEST spends writing
/// them, well above the nodes and actions a realistic cluster carries.
pub const MAX_NODE_LOG_DELIVERIES_PER_CALL: usize = 64;

/// The key one NODE's files live under, relative to the bucket:
/// `<prefix>/<cluster-id>/node/<node>`.
#[must_use]
pub fn node_log_prefix(destination: &LogDestination, cluster_id: &str, node: &str) -> String {
    destination.key(&format!("{cluster_id}/{NODE_SEGMENT}/{node}"))
}

/// The key the files of ONE bootstrap action's run on one node live under:
/// `<node prefix>/bootstrap-actions/<position>`.
#[must_use]
pub fn bootstrap_action_log_prefix(
    destination: &LogDestination,
    cluster_id: &str,
    node: &str,
    position: usize,
) -> String {
    format!(
        "{}/{BOOTSTRAP_ACTIONS_SEGMENT}/{position}",
        node_log_prefix(destination, cluster_id, node)
    )
}

/// The key one node's instance-state file is written at, stamped with the instant it reports.
#[must_use]
pub fn instance_state_log_key(
    destination: &LogDestination,
    cluster_id: &str,
    node: &str,
    at: SystemTime,
) -> String {
    let stamp = ::chrono::DateTime::<::chrono::Utc>::from(at)
        .format(INSTANCE_STATE_STAMP_FORMAT)
        .to_string();
    format!(
        "{}/{INSTANCE_STATE_SEGMENT}/{INSTANCE_STATE_LOG_OBJECT}-{stamp}.gz",
        node_log_prefix(destination, cluster_id, node)
    )
}

/// One instant as a node's log lines write it.
fn at(instant: SystemTime) -> String {
    ::chrono::DateTime::<::chrono::Utc>::from(instant)
        .format(NODE_LOG_TIME_FORMAT)
        .to_string()
}

/// The CONTROLLER log of one bootstrap action's run, as text.
///
/// Built from the run's own persisted record, so it cannot disagree with what this service
/// reports about the cluster: which action of the launch it is, the script it fetched, the node
/// it ran on, WHEN it was answered for and what it came to — the status the script exited with
/// where one ran it, the object store's own answer where the script could not be fetched, and
/// otherwise the plain statement that nothing performed it.
#[must_use]
pub fn bootstrap_controller_log_text(run: &BootstrapActionRun) -> String {
    let stamp = at(run.at);
    let mut lines = vec![
        format!(
            "{stamp} INFO Fetching bootstrap action {} from {}",
            run.position, run.path
        ),
        format!(
            "{stamp} INFO Running bootstrap action {} on instance {}",
            run.position, run.node
        ),
    ];
    lines.push(match &run.answer {
        BootstrapActionAnswer::Exited(status) => format!(
            "{stamp} INFO Bootstrap action {} ended with exit code {status}",
            run.position
        ),
        BootstrapActionAnswer::Unfetched(detail) => format!(
            "{stamp} WARN Bootstrap action {} file could not be downloaded: {detail}",
            run.position
        ),
        BootstrapActionAnswer::NotPerformed => format!(
            "{stamp} WARN Bootstrap action {} was not performed, so it exited with no status",
            run.position
        ),
    });
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// The INSTANCE STATE log of one node, as text.
///
/// Every line is a fact of the stored instance record ([`ClusterInstanceRecord`]) — the same
/// facts `ListInstances` reports — and a member the record carries nothing for is left out
/// rather than written as a value.
#[must_use]
pub fn instance_state_log_text(instance: &ClusterInstanceRecord, cluster_id: &str) -> String {
    let reported = instance
        .ready_date_time
        .unwrap_or(instance.creation_date_time);
    let stamp = at(reported);
    let mut lines = vec![
        format!(
            "{stamp} INFO Instance {} of job flow {cluster_id} is {}",
            instance.ec2_instance_id, instance.state
        ),
        format!(
            "{stamp} INFO Amazon EMR instance id {}, instance type {}, market {}",
            instance.id, instance.instance_type, instance.market
        ),
    ];
    if let Some(group) = &instance.instance_group_id {
        lines.push(format!("{stamp} INFO instance group {group}"));
    }
    if let Some(fleet) = &instance.instance_fleet_id {
        lines.push(format!("{stamp} INFO instance fleet {fleet}"));
    }
    if let Some(address) = &instance.private_ip_address {
        lines.push(format!("{stamp} INFO private address {address}"));
    }
    if let Some(hostname) = &instance.private_dns_name {
        lines.push(format!("{stamp} INFO private DNS name {hostname}"));
    }
    lines.push(format!(
        "{stamp} INFO created at {}",
        at(instance.creation_date_time)
    ));
    if let Some(ready) = instance.ready_date_time {
        lines.push(format!("{stamp} INFO ready at {}", at(ready)));
    }
    if let Some(ended) = instance.end_date_time {
        lines.push(format!("{stamp} INFO ended at {}", at(ended)));
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// The files one NODE of a cluster owes its cluster's log destination.
#[derive(Debug, Clone)]
pub struct PendingNodeLogs {
    /// The node they belong to, for the tracing of a failed write and for giving the record of
    /// delivery back.
    pub node: String,
    /// The bootstrap action whose run left them, for the files of ONE action, and `None` for
    /// the node's own instance-state file.
    pub position: Option<usize>,
    /// WHO the push is made by, as the launch named it: the role the cluster's instances assume
    /// through their instance profile, whose permissions decide whether a file lands
    /// ([`super::step_logs`]); `None` for a cluster that named none.
    pub pushed_by: Option<String>,
    /// The files themselves.
    pub objects: Vec<PendingLogObject>,
}

impl ComAmazonawsEmrElasticmapreduceState {
    /// The log files the NODES of the cluster `cluster_id` are owed, recorded as delivered.
    ///
    /// Called with the store held, right after the cluster's transitions were materialised, and
    /// answers nothing for a cluster this account holds none of or one whose launch named no
    /// `LogUri`: with no destination configured there is nowhere a log file could exist, which
    /// is what the real service does with such a cluster too.
    pub fn take_due_node_logs(&mut self, cluster_id: &str) -> Vec<PendingNodeLogs> {
        let Some(cluster) = self.clusters.get_mut(cluster_id) else {
            return Vec::new();
        };
        let Some(destination) = cluster.log_uri.as_deref().and_then(LogDestination::parse) else {
            return Vec::new();
        };
        // WHO the cluster's nodes push with, read before the records are walked.
        let pushed_by = cluster.job_flow_role.clone();
        let id = cluster.id.clone();
        let mut owed: Vec<PendingNodeLogs> = Vec::new();
        // The files each bootstrap-action RUN left on its node.
        for run in &mut cluster.bootstrap_runs {
            if owed.len() >= MAX_NODE_LOG_DELIVERIES_PER_CALL {
                return owed;
            }
            if run.log_delivered {
                continue;
            }
            let prefix =
                bootstrap_action_log_prefix(&destination, &id, &run.node, run.position);
            let log = run.log.as_ref();
            let texts = [
                (CONTROLLER_LOG_OBJECT, bootstrap_controller_log_text(run)),
                (STDERR_LOG_OBJECT, stderr_log_text(log)),
                (STDOUT_LOG_OBJECT, stdout_log_text(log)),
            ];
            let objects: Vec<PendingLogObject> = texts
                .iter()
                .filter_map(|(name, text)| {
                    Some(PendingLogObject {
                        bucket: destination.bucket.clone(),
                        key: format!("{prefix}/{name}"),
                        body: gzipped(text)?,
                    })
                })
                .collect();
            if objects.len() != STEP_LOG_OBJECTS.len() {
                continue;
            }
            run.log_delivered = true;
            owed.push(PendingNodeLogs {
                node: run.node.clone(),
                position: Some(run.position),
                pushed_by: pushed_by.clone(),
                objects,
            });
        }
        // The node's own instance-state file, owed once the node has come UP.
        for instance in &mut cluster.ec2_instances {
            if owed.len() >= MAX_NODE_LOG_DELIVERIES_PER_CALL {
                return owed;
            }
            if instance.node_log_delivered || instance.ready_date_time.is_none() {
                continue;
            }
            let reported = instance
                .ready_date_time
                .unwrap_or(instance.creation_date_time);
            let key = instance_state_log_key(
                &destination,
                &id,
                &instance.ec2_instance_id,
                reported,
            );
            let Some(body) = gzipped(&instance_state_log_text(instance, &id)) else {
                continue;
            };
            instance.node_log_delivered = true;
            owed.push(PendingNodeLogs {
                node: instance.ec2_instance_id.clone(),
                position: None,
                pushed_by: pushed_by.clone(),
                objects: vec![PendingLogObject {
                    bucket: destination.bucket.clone(),
                    key,
                    body,
                }],
            });
        }
        owed
    }

    /// Give back the record of delivery for the node files `undelivered` names: they were
    /// recorded as delivered and the write did not go through, so the next call that reports
    /// the cluster writes them rather than nothing ever writing them at all.
    ///
    /// Each entry is the node and, for the files of one bootstrap action, that action's
    /// position ([`PendingNodeLogs`]).
    pub fn forget_delivered_node_logs(
        &mut self,
        cluster_id: &str,
        undelivered: &[(String, Option<usize>)],
    ) {
        let Some(cluster) = self.clusters.get_mut(cluster_id) else {
            return;
        };
        for (node, position) in undelivered {
            match position {
                Some(position) => {
                    for run in &mut cluster.bootstrap_runs {
                        if &run.node == node && run.position == *position {
                            run.log_delivered = false;
                        }
                    }
                }
                None => {
                    for instance in &mut cluster.ec2_instances {
                        if &instance.ec2_instance_id == node {
                            instance.node_log_delivered = false;
                        }
                    }
                }
            }
        }
    }
}

/// Write the log files every node of the cluster `cluster_id` is owed.
///
/// Called by the operations that report a cluster or its steps, the way a step's own directory
/// is written ([`deliver_due_step_logs`]): the delivery was owed from the instant the node had
/// something to push, whoever asks about it next, and the write may not happen under this
/// service's lock. The store is taken only to materialise the cluster's transitions and to take
/// what is owed.
///
/// Each file is written only where the permissions in force on the role the push is made by do
/// not deny the object write, so a node whose files IAM refuses leaves its prefix empty rather
/// than this reporting a file the object store does not hold.
pub async fn deliver_due_node_logs<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    cluster_id: &str,
    now: SystemTime,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let owed = {
        let mut store = state.lock();
        // The transitions the clock owes are applied first, so a node whose span has passed is
        // UP and its file is among the owed ones.
        if store.cluster_mut(cluster_id, now).is_none() {
            return;
        }
        store.take_due_node_logs(cluster_id)
    };
    if owed.is_empty() {
        return;
    }
    let client = ::service_common::s3_client::s3_client(ctx.callback(), region);
    let mut undelivered: Vec<(String, Option<usize>)> = Vec::new();
    for logs in owed {
        // The ROLE the node's push is authorized against: the role the cluster's instances
        // assume through their instance profile. Nothing resolved is nothing known about the
        // permissions in force, which withholds no write.
        let pushed_by = match logs.pushed_by.as_deref() {
            Some(named) => super::work_role_arn(ctx, region, named).await,
            None => None,
        };
        for object in &logs.objects {
            if let Some(role_arn) = pushed_by.as_deref() {
                let resource = object_arn(&object.bucket, &object.key);
                if ::service_common::iam_role::role_denies_action(
                    ctx,
                    region,
                    role_arn,
                    PUT_OBJECT_ACTION,
                    &resource,
                )
                .await
                {
                    ::tracing::warn!(
                        cluster_id = %cluster_id,
                        node = %logs.node,
                        role_arn = %role_arn,
                        resource = %resource,
                        "an Amazon EMR node log file was not written: the permissions in force \
                         on the role the cluster's nodes run as deny the object write"
                    );
                    undelivered.push((logs.node.clone(), logs.position));
                    break;
                }
            }
            if let Err(error) = client
                .put_object()
                .bucket(&object.bucket)
                .key(&object.key)
                .content_type(LOG_OBJECT_MEDIA_TYPE)
                .body(::aws_sdk_s3::primitives::ByteStream::from(
                    object.body.clone(),
                ))
                .send()
                .await
            {
                ::tracing::warn!(
                    cluster_id = %cluster_id,
                    node = %logs.node,
                    bucket = %object.bucket,
                    key = %object.key,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "an Amazon EMR node log file could not be delivered to Amazon S3"
                );
                undelivered.push((logs.node.clone(), logs.position));
                break;
            }
        }
    }
    // A node whose files did not land is owed them again: nothing ever reports a file the
    // object store does not hold.
    if !undelivered.is_empty() {
        let mut store = state.lock();
        store.forget_delivered_node_logs(cluster_id, &undelivered);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a node's files live under `<LogUri>/<cluster-id>/node/<instance-id>/`, a bootstrap
    /// action's run under that node's `bootstrap-actions/<position>/`, and the node's own
    /// instance-state file under its `instance-state/`.
    #[test]
    fn a_nodes_files_are_keyed_by_its_cluster_and_its_instance() {
        let rows: [(&str, &str, &str); 3] = [
            ("a prefix", "s3://bucket/logs/", "logs/j-1/node/i-9"),
            ("no prefix", "s3://bucket", "j-1/node/i-9"),
            ("a nested prefix", "s3://bucket/a/b/", "a/b/j-1/node/i-9"),
        ];
        for (case, log_uri, expected) in rows {
            let destination =
                LogDestination::parse(log_uri).expect("the row's URI names a destination");
            let actual = node_log_prefix(&destination, "j-1", "i-9");
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
            let action = bootstrap_action_log_prefix(&destination, "j-1", "i-9", 2);
            let expected_action = format!("{expected}/{BOOTSTRAP_ACTIONS_SEGMENT}/2");
            assert_eq!(
                action, expected_action,
                "case {case:?}: expected {expected_action:?}, got {action:?}"
            );
            let state_log =
                instance_state_log_key(&destination, "j-1", "i-9", SystemTime::UNIX_EPOCH);
            let expected_state = format!(
                "{expected}/{INSTANCE_STATE_SEGMENT}/{INSTANCE_STATE_LOG_OBJECT}-\
                 1970-01-01-00-00.gz"
            );
            assert_eq!(
                state_log, expected_state,
                "case {case:?}: expected {expected_state:?}, got {state_log:?}"
            );
        }
    }

    /// RULE: the controller file states what the run came to, and claims an exit status only
    /// for a run something performed.
    #[test]
    fn the_controller_file_states_what_the_run_came_to() {
        let rows: [(&str, BootstrapActionAnswer, &str); 3] = [
            (
                "a script that exited zero",
                BootstrapActionAnswer::Exited(0),
                "ended with exit code 0",
            ),
            (
                "a script whose object the node could not fetch",
                BootstrapActionAnswer::Unfetched("no such key".to_string()),
                "could not be downloaded: no such key",
            ),
            (
                "a script nothing performed",
                BootstrapActionAnswer::NotPerformed,
                "was not performed",
            ),
        ];
        for (case, answer, fragment) in rows {
            let run = BootstrapActionRun {
                position: 1,
                path: "s3://scripts/install.sh".to_string(),
                node: "i-9".to_string(),
                answer,
                at: SystemTime::UNIX_EPOCH,
                log: None,
                log_delivered: false,
            };
            let text = bootstrap_controller_log_text(&run);
            assert!(
                text.contains(fragment),
                "case {case:?}: expected {fragment:?} in the controller file, got {text:?}"
            );
            assert!(
                text.contains("s3://scripts/install.sh") && text.contains("i-9"),
                "case {case:?}: the file names the script and the node, got {text:?}"
            );
        }
    }
}
