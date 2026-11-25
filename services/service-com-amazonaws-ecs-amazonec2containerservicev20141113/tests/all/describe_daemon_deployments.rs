// SPDX-License-Identifier: CC0-1.0

//! `DescribeDaemonDeployments` as the BATCH read of stored daemon deployments it is.
//!
//! The rules under test, each asserted over a table that spans its class rather than over the
//! one request that exposed it:
//!
//! * `DescribeDaemonDeployments` "Describes one or more of your daemon deployments": the read
//!   answers one `DaemonDeployment` per DISTINCT rollout its `daemonDeploymentArns` resolve, in
//!   the order the request named them — whatever number of entries the member carries, with a
//!   repeated ARN answered once because it names one rollout — beside one `MISSING` failure per
//!   entry that resolves to no stored rollout, so a batch is never an all-or-nothing answer.
//!   [`crate::daemon_read_arn_rules`] holds that rule over which PART of the identifier an
//!   entry resolves nothing for, against a store holding ONE such resource; the table here is
//!   the complement, over a batch that resolves SEVERAL of them;
//! * every member of a reported `DaemonDeployment` is computed from the rollout the store
//!   holds: the cluster that hosts the daemon, the status the clock left the rollout in, the
//!   revision it is deploying with the per-capacity-provider instance counts its tasks add up
//!   to, the deployment configuration the call that started it stated, and the state of the
//!   failure detectors that configuration turned on — so the same rollout read through
//!   `ListDaemonDeployments` agrees with it member for member;
//! * and because this read only reports, repeating it changes nothing a later caller sees.
//!
//! Every case drives the emulator through the shared wire harness in [`crate::ecs_wire`], the
//! same front door a client uses, so nothing reaches into the state struct and a response that
//! is not computed from persisted state cannot pass.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::daemon_scheduling::{
    create_managed_provider, list_deployments_page, register_daemon_definition, IMAGE,
};
use crate::ecs_wire::{
    count, create_cluster, list, ok, poll_until, register_instance, service, string, ACCOUNT,
    SIGNED_REGION,
};

/// `Failure$reason` for an entry that is shaped like a daemon-deployment ARN and resolves to
/// no rollout this store holds.
const MISSING: &str = "MISSING";

/// Create one daemon over `provider` running `definition`, stating `configuration` as its
/// deployment configuration when it carries one, and answer its ARN with the ARN of the
/// rollout the create started.
///
/// The shared [`crate::daemon_scheduling::create_daemon`] states no deployment configuration,
/// and the configuration is what decides the failure detectors a rollout reports, so the
/// table below states it per row.
async fn create_configured_daemon(
    router: &MultiServiceRouter,
    cluster: &str,
    name: &str,
    provider: &str,
    definition: &str,
    configuration: &Value,
    case: &str,
) -> (String, String) {
    let mut request = json!({
        "clusterArn": cluster,
        "daemonName": name,
        "daemonTaskDefinitionArn": definition,
        "capacityProviderArns": [provider],
    });
    if !configuration.is_null() {
        request["deploymentConfiguration"] = configuration.clone();
    }
    let body = ok(router, "CreateDaemon", request, case).await;
    (
        string(&body, "daemonArn", case),
        string(&body, "deploymentArn", case),
    )
}

/// `DescribeDaemonDeployments` over whatever `arns` the row carries.
async fn describe_deployments(
    router: &MultiServiceRouter,
    arns: &[String],
    case: &str,
) -> Value {
    ok(
        router,
        "DescribeDaemonDeployments",
        json!({ "daemonDeploymentArns": arns }),
        case,
    )
    .await
}

/// The ONE rollout a single-entry read answers, and `Value::Null` while it answers none — the
/// reading a poll looks at again until the clock has left the rollout where the case is about.
///
/// `crate::daemon_scheduling`'s own single-rollout read PANICS when the answer carries none,
/// which a poll cannot do: it has to be able to look again.
async fn describe_one(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let body = describe_deployments(router, &[arn.to_string()], case).await;
    list(&body, "daemonDeployments", case)
        .into_iter()
        .next()
        .unwrap_or(Value::Null)
}

/// The `member` of every entry of `entries`, in the order the answer carried them.
fn reported(entries: &[Value], member: &str, case: &str) -> Vec<String> {
    entries
        .iter()
        .map(|entry| string(entry, member, case))
        .collect()
}

/// A daemon-deployment ARN of the form this service MINTS — `daemon-deployment/<cluster>/
/// <daemon>/<id>`, with `id` the UUID (five hyphen-separated groups of 8, 4, 4, 4 and 12
/// hexadecimal digits) its own daemon-scoped ARNs carry — that this store holds nothing under.
/// Such an entry names a rollout of this account, so it is reported on the `failures` channel
/// rather than refused with the request.
///
/// `ordinal` only tells two of them apart; a caller picks one far beyond the rollouts the case
/// creates, and the guard below checks that what it built resolves to none of them.
fn unheld_deployment_arn(cluster: &str, daemon: &str, ordinal: u64) -> String {
    format!(
        "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:daemon-deployment/{cluster}/{daemon}/\
         {ordinal:08x}-0000-0000-0000-{ordinal:012x}"
    )
}

/// One row of the batch table: what the case is called, the entries the member carries, the
/// rollouts the read must answer, and the entries it must report as failures.
type BatchRow = (&'static str, Vec<String>, Vec<String>, Vec<String>);

#[tokio::test]
async fn a_batch_read_answers_one_entry_per_distinct_rollout_and_reports_the_rest_as_failures() {
    let router = service();
    const CLUSTER: &str = "daemon-deployment-batch";
    const CASE: &str = "setup";
    create_cluster(&router, CLUSTER, CASE).await;
    let provider = create_managed_provider(&router, CLUSTER, "mi-batch", CASE).await;
    let definition =
        register_daemon_definition(&router, "daemondeploymentbatch", IMAGE, CASE).await;

    // Three daemons, so three rollouts this store holds: enough for the table to span a batch
    // of one entry, of two and of all three.
    const DAEMONS: [&str; 3] = ["agents-one", "agents-two", "agents-three"];
    let mut held = Vec::new();
    for name in DAEMONS {
        let (_, deployment) = create_configured_daemon(
            &router,
            CLUSTER,
            name,
            &provider,
            &definition,
            &Value::Null,
            CASE,
        )
        .await;
        held.push(deployment);
    }
    let unheld: Vec<String> = [9_001, 9_002]
        .into_iter()
        .map(|ordinal| unheld_deployment_arn(CLUSTER, DAEMONS[0], ordinal))
        .collect();
    for entry in &unheld {
        assert!(
            !held.contains(entry),
            "case {CASE:?}: an entry meant to resolve to nothing names a stored rollout: {entry}"
        );
    }

    // The rows differ along what the rule generalises over: HOW MANY of the store's rollouts
    // the member names, in which ORDER, how often one is REPEATED, and whether entries that
    // resolve to nothing sit among them. Which PART of an identifier an unresolved entry holds
    // nothing for, and a batch that resolves nothing at all, belong to the rule's other table
    // in [`crate::daemon_read_arn_rules`].
    let rows: [BatchRow; 5] = [
        (
            "one rollout",
            vec![held[0].clone()],
            vec![held[0].clone()],
            Vec::new(),
        ),
        (
            "two rollouts",
            vec![held[0].clone(), held[1].clone()],
            vec![held[0].clone(), held[1].clone()],
            Vec::new(),
        ),
        (
            "every rollout, in the reverse of the order they were created in",
            vec![held[2].clone(), held[1].clone(), held[0].clone()],
            vec![held[2].clone(), held[1].clone(), held[0].clone()],
            Vec::new(),
        ),
        (
            "the same rollout named three times",
            vec![held[0].clone(), held[0].clone(), held[0].clone()],
            vec![held[0].clone()],
            Vec::new(),
        ),
        (
            "rollouts and entries that resolve to nothing, interleaved",
            vec![
                unheld[0].clone(),
                held[0].clone(),
                unheld[1].clone(),
                held[2].clone(),
            ],
            vec![held[0].clone(), held[2].clone()],
            unheld.clone(),
        ),
    ];

    for (case, entries, resolved, unresolved) in rows {
        let body = describe_deployments(&router, &entries, case).await;
        assert_eq!(
            reported(&list(&body, "daemonDeployments", case), "daemonDeploymentArn", case),
            resolved,
            "case {case:?}: expected one entry per distinct rollout the request resolves, in the \
             order it named them, got {body}"
        );
        let failures = list(&body, "failures", case);
        assert_eq!(
            reported(&failures, "arn", case),
            unresolved,
            "case {case:?}: expected the entries that resolve to nothing reported as failures, \
             got {body}"
        );
        for failure in &failures {
            assert_eq!(
                string(failure, "reason", case),
                MISSING,
                "case {case:?}: expected an unresolved entry's own MISSING report, got {failure}"
            );
        }
    }

    // The reads above only report: every daemon still holds exactly the one rollout its create
    // started, read back through the listing that selects a daemon's rollouts.
    for name in DAEMONS {
        let case = format!("the rollouts of {name} after the batch reads");
        let daemon = format!("arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:daemon/{CLUSTER}/{name}");
        let (summaries, token) =
            list_deployments_page(&router, json!({ "daemonArn": daemon }), &case).await;
        assert_eq!(
            summaries.len(),
            1,
            "case {case:?}: expected the reads to leave one rollout per daemon, got {summaries:?}"
        );
        assert!(
            token.is_none(),
            "case {case:?}: expected one page, got the token {token:?}"
        );
    }
}

#[tokio::test]
async fn every_reported_member_is_computed_from_the_rollout_the_store_holds() {
    let router = service();
    const CLUSTER: &str = "daemon-deployment-members";
    const CASE: &str = "setup";
    let cluster_arn = create_cluster(&router, CLUSTER, CASE).await;
    register_instance(&router, CLUSTER, "i-0daemondeploy0001", CASE).await;
    let provider = create_managed_provider(&router, CLUSTER, "mi-members", CASE).await;
    let definition =
        register_daemon_definition(&router, "daemondeploymentmembers", IMAGE, CASE).await;

    // The rows differ along the DEPLOYMENT CONFIGURATION the call that started the rollout
    // stated, which is what decides the failure detectors the rollout then reports: none at
    // all, one that only paces the rollout, one that turns the alarm check on, and one that
    // names alarms and leaves the check off.
    let rows: [(&str, &str, Value); 4] = [
        (
            "a rollout whose create stated no deployment configuration",
            "agents-plain",
            Value::Null,
        ),
        (
            "a rollout paced by a drain percent and a bake time",
            "agents-paced",
            json!({ "drainPercent": 25.0, "bakeTimeInMinutes": 2 }),
        ),
        (
            "a rollout whose configuration turns the alarm check on",
            "agents-alarmed",
            json!({ "alarms": { "enable": true, "alarmNames": ["daemon-errors", "daemon-latency"] } }),
        ),
        (
            "a rollout that names alarms and leaves the check off",
            "agents-unalarmed",
            json!({ "alarms": { "enable": false, "alarmNames": ["daemon-errors"] } }),
        ),
    ];

    for (case, daemon_name, configuration) in rows {
        let (daemon_arn, deployment_arn) = create_configured_daemon(
            &router,
            CLUSTER,
            daemon_name,
            &provider,
            &definition,
            &configuration,
            case,
        )
        .await;

        // Where a rollout is left, and how many instances run its revision's task, is the
        // CLOCK's: the case waits for the CONDITION rather than for a span.
        let reported = poll_until(
            case,
            "the rollout to reach SUCCESSFUL with its revision's task counted",
            || describe_one(&router, &deployment_arn, case),
            |deployment| {
                deployment["status"] == json!("SUCCESSFUL")
                    && deployment["targetDaemonRevision"]["totalRunningInstanceCount"] == json!(1)
            },
        )
        .await;

        assert_eq!(
            string(&reported, "daemonDeploymentArn", case),
            deployment_arn,
            "case {case:?}: expected the rollout that was asked about, got {reported}"
        );
        assert_eq!(
            string(&reported, "clusterArn", case),
            cluster_arn,
            "case {case:?}: expected the cluster that hosts the daemon, got {reported}"
        );

        // The revision it is deploying is a revision of THIS daemon, and the counts beside it
        // are read off the daemon tasks that exist on the capacity providers it deploys onto.
        let target = &reported["targetDaemonRevision"];
        let revision_arn = string(target, "arn", case);
        let revision_prefix = format!(
            "arn:aws:ecs:{SIGNED_REGION}:{ACCOUNT}:daemon-revision/{CLUSTER}/{daemon_name}/"
        );
        assert!(
            revision_arn.starts_with(&revision_prefix),
            "case {case:?}: expected a revision of this daemon, got {revision_arn:?}"
        );
        let providers = list(target, "capacityProviders", case);
        assert_eq!(
            providers.len(),
            1,
            "case {case:?}: the daemon deploys onto one provider, got {target}"
        );
        assert_eq!(
            string(&providers[0], "arn", case),
            provider,
            "case {case:?}: expected the provider the daemon deploys onto, got {target}"
        );
        assert_eq!(
            count(&providers[0], "runningInstanceCount", case),
            1,
            "case {case:?}: expected the provider's one instance counted, got {target}"
        );
        assert_eq!(
            count(target, "totalWithoutDaemonInstanceCount", case),
            0,
            "case {case:?}: every instance runs the daemon task, got {target}"
        );
        assert_eq!(
            count(target, "totalDrainingInstanceCount", case),
            0,
            "case {case:?}: nothing is being drained, got {target}"
        );
        assert!(
            list(&reported, "sourceDaemonRevisions", case).is_empty(),
            "case {case:?}: a daemon's FIRST rollout replaces no revision, got {reported}"
        );

        // The failure detectors, each as this rollout's own configuration turned it on. No
        // daemon deployment configuration can turn the circuit breaker on — the shape carries
        // no circuit-breaker option (`smithy_query(query="shape",
        // shape="DaemonDeploymentConfiguration")`) — so every rollout reports it off.
        let breaker = &reported["circuitBreaker"];
        assert_eq!(
            string(breaker, "status", case),
            "DISABLED",
            "case {case:?}: expected the circuit breaker off, got {breaker}"
        );
        assert_eq!(
            count(breaker, "failureCount", case),
            0,
            "case {case:?}: a breaker that is off counts no failure, got {breaker}"
        );
        assert_eq!(
            count(breaker, "threshold", case),
            0,
            "case {case:?}: a breaker that is off has no threshold, got {breaker}"
        );

        let alarms_enabled = configuration["alarms"]["enable"] == json!(true);
        let alarms = &reported["alarms"];
        assert_eq!(
            string(alarms, "status", case),
            if alarms_enabled {
                "MONITORING_COMPLETE"
            } else {
                "DISABLED"
            },
            "case {case:?}: expected the alarm check the configuration asked for, got {alarms}"
        );
        assert_eq!(
            list(alarms, "alarmNames", case),
            list(&configuration["alarms"], "alarmNames", case),
            "case {case:?}: expected the alarms the configuration named, got {alarms}"
        );
        assert!(
            list(alarms, "triggeredAlarmNames", case).is_empty(),
            "case {case:?}: nothing evaluated an alarm, so none is triggered, got {alarms}"
        );
        assert!(
            reported.get("rollback").is_none_or(Value::is_null),
            "case {case:?}: no failure detector ended this rollout, so nothing rolled it back, \
             got {reported}"
        );

        // The deployment configuration the call that started it stated, as it stated it.
        let stored = &reported["deploymentConfiguration"];
        if configuration.is_null() {
            assert!(
                stored.is_null(),
                "case {case:?}: a rollout that was given no configuration reports none, got \
                 {reported}"
            );
        } else {
            assert_eq!(
                stored["drainPercent"], configuration["drainPercent"],
                "case {case:?}: expected the drain percent the create stated, got {stored}"
            );
            assert_eq!(
                count(stored, "bakeTimeInMinutes", case),
                count(&configuration, "bakeTimeInMinutes", case),
                "case {case:?}: expected the bake time the create stated, got {stored}"
            );
            assert_eq!(
                stored["alarms"]["enable"] == json!(true),
                alarms_enabled,
                "case {case:?}: expected the alarm option the create stated, got {stored}"
            );
        }

        // The LISTING of this daemon's rollouts reports the same rollout out of the same state,
        // so the two reads agree member for member on everything they share.
        let (summaries, _) =
            list_deployments_page(&router, json!({ "daemonArn": daemon_arn }), case).await;
        let summary = summaries
            .iter()
            .find(|summary| summary["daemonDeploymentArn"] == json!(deployment_arn.as_str()))
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the rollout in its daemon's listing, got {summaries:?}")
            });
        for member in [
            "clusterArn",
            "status",
            "statusReason",
            "createdAt",
            "startedAt",
            "stoppedAt",
            "finishedAt",
        ] {
            assert_eq!(
                summary[member], reported[member],
                "case {case:?}: expected the listing and the read to agree on {member}, got \
                 {summary} and {reported}"
            );
        }
        assert_eq!(
            summary["targetDaemonRevisionArn"],
            json!(revision_arn),
            "case {case:?}: expected the listing to name the revision the read reports, got \
             {summary}"
        );

        // And because the read only reports, a repeat of it answers the same rollout.
        let again = describe_one(&router, &deployment_arn, case).await;
        assert_eq!(
            again, reported,
            "case {case:?}: expected a repeat of the read to answer the same rollout, got \
             {again} after {reported}"
        );
    }
}
