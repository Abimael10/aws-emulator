// SPDX-License-Identifier: CC0-1.0

//! What a client that DISCOVERS a deployment's fleet sees: `ListDeploymentTargets` over an
//! EC2/on-premises (`Server`) deployment, from the instant it is created to after it has
//! reached an outcome.
//!
//! RULE: `ListDeploymentTargets` answers the ids of the targets the deployment resolved —
//! for a `Server` deployment, the running Amazon EC2 instances its deployment group's EC2
//! tag selectors identify — and it answers the same set at every instant of the deployment
//! and after it, narrowed only by the `targetFilters` the request names;
//! `DeploymentNotStartedException` covers ONLY the window before the deployment starts, so a
//! caller that polls through it is answered rather than refused forever, and a
//! `TargetStatus` filter no target satisfies is an empty page rather than a refusal. Because
//! that narrowing reads the same per-target status the deployment's own progress fixes, the
//! three statuses an in-flight in-place target can hold together name the whole fleet at
//! every instant, and at most (fleet size - the resolved configuration's
//! `minimumHealthyHosts`) of them are `InProgress` at one instant.
//!
//! `ListDeploymentTargets` "returns an array of target IDs that are associated a deployment",
//! its `targetIds` output member being "the unique IDs of deployment targets" and its
//! `targetFilters` input member "a key used to filter the returned targets […] TargetStatus -
//! A TargetStatus filter string can be Failed, InProgress, Pending, Ready, Skipped,
//! Succeeded, or Unknown"; `DeploymentNotStartedException` is "the specified deployment has
//! not started"
//! (`smithy_query(query="operation",operation="ListDeploymentTargets")`,
//! `smithy_query(query="doc",shape="ListDeploymentTargetsInput",member="targetFilters")`).
//! `minimumHealthyHosts` is "the minimum number or percentage of instances that should be
//! healthy at any time during a deployment", and the model states the arithmetic itself: "In
//! an example of nine instances, if a HOST_COUNT of six is specified, deploy to up to three
//! instances at a time"
//! (`smithy_query(query="shape",shape="MinimumHealthyHosts")`).
//!
//! This suite lives beside the router because the fleet belongs to ANOTHER service: every
//! host below is launched through Amazon EC2's own `RunInstances` on the SAME engine that
//! serves CodeDeploy, so the ids this listing reports are ids EC2 handed out. It drives that
//! emulator through the harness the sibling suite
//! (`codedeploy_minimum_healthy_hosts_paces_instance_targets.rs`) owns, rather than a second
//! one that would drift from it; what it adds is the READ a client discovers the fleet
//! with — `ListDeploymentTargets`, its pre-start refusal and its `TargetStatus` narrowing —
//! where that suite samples `BatchGetDeploymentTargets` and `deploymentOverview`.
//!
//! The rows differ along what the rule generalises over: how many hosts the selectors
//! identify (one, two, three), and the bound the resolved configuration states — a built-in
//! that keeps no host back, a built-in that keeps all but one back, and client-created
//! `HOST_COUNT` and `FLEET_PERCENT` bounds, over fleets they leave one and two hosts
//! deployable out of.
//!
//! Every wait here polls for the CONDITION it is waiting on; nothing asserts how long
//! anything took on this host.

use std::collections::BTreeSet;
use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::codedeploy_minimum_healthy_hosts_paces_instance_targets::{
    by_tag_filter, codedeploy, codedeploy_ok, deploy, deployment_info, emulator, given_group,
    given_running_fleet, ALL_AT_ONCE, CONVERGENCE_BUDGET, ONE_AT_A_TIME, POLL_INTERVAL,
};

/// The statuses a target of an in-place deployment that is running and succeeding passes
/// through, which together have to name the whole fleet at every instant. All three are
/// members of `TargetStatus` (`smithy_query(query="shape",shape="TargetStatus")`: Pending |
/// InProgress | Succeeded | Failed | Skipped | Unknown | Ready).
const IN_FLIGHT_STATUSES: &[&str] = &["Pending", "InProgress", "Succeeded"];

/// A `TargetStatus` no target of a deployment that succeeds over its whole fleet ever holds,
/// so a filter naming it selects nothing. Also a member of `TargetStatus`.
const UNHELD_STATUS: &[&str] = &["Failed"];

/// One row of the table: a fleet of `hosts`, the bound the deployment runs under, and how
/// many of that fleet the bound leaves deployable at one instant.
struct Listed {
    /// What the row is about.
    case: &'static str,
    /// How many running, tagged hosts the group's selectors identify.
    hosts: usize,
    /// The `minimumHealthyHosts` a configuration created for this row states, or `None` to
    /// name a built-in instead.
    minimum_healthy_hosts: Option<(&'static str, i32)>,
    /// The built-in the group names, for a row that creates no configuration of its own.
    builtin: Option<&'static str>,
    /// How many of the fleet the bound leaves deployable at one instant.
    concurrency: usize,
}

/// The error code an awsJson1_1 refusal carries, as its `__type` names it: the bare code,
/// which a `__type` may qualify with the shape's namespace.
fn error_code(body: &Value) -> &str {
    body["__type"]
        .as_str()
        .unwrap_or("<no __type>")
        .rsplit(['#', '.'])
        .next()
        .unwrap_or("<no __type>")
}

/// Call `ListDeploymentTargets` for `deployment_id`, narrowed by a `TargetStatus` filter over
/// `statuses` when the row names any, answering the HTTP status and the body.
async fn list_targets(
    router: &MultiServiceRouter,
    deployment_id: &str,
    statuses: Option<&[&str]>,
) -> (u16, Value) {
    let mut request = json!({"deploymentId": deployment_id});
    if let Some(statuses) = statuses {
        request["targetFilters"] = json!({"TargetStatus": statuses});
    }
    codedeploy(router, "ListDeploymentTargets", &request).await
}

/// The `targetIds` of one answered listing, as a set.
fn ids_of(case: &str, statuses: Option<&[&str]>, body: &Value) -> BTreeSet<String> {
    let listed = body["targetIds"].as_array().unwrap_or_else(|| {
        panic!("case {case:?}: expected a targetIds list for {statuses:?}, got {body}")
    });
    listed
        .iter()
        .map(|id| {
            id.as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected each target id as a string, got {body}")
                })
                .to_string()
        })
        .collect()
}

/// One listing that must have been ANSWERED, as a set of ids.
async fn listed(
    router: &MultiServiceRouter,
    deployment_id: &str,
    statuses: Option<&[&str]>,
    case: &str,
) -> BTreeSet<String> {
    let (status, body) = list_targets(router, deployment_id, statuses).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the targets of a started deployment to be listable with the \
         filter {statuses:?}, got {status} with {body}"
    );
    ids_of(case, statuses, &body)
}

/// Poll `ListDeploymentTargets` until the deployment has started, answering the FIRST set of
/// ids it reports.
///
/// Every answer before that has to be the one refusal the model gives this operation for a
/// deployment that has not started yet — never an empty page, which would tell a caller
/// waiting for the fleet that there is none — and it has to name the deployment the request
/// asked about. That the refusal ends at all is the condition this wait is on.
async fn targets_once_started(
    router: &MultiServiceRouter,
    deployment_id: &str,
    case: &str,
) -> BTreeSet<String> {
    let deadline = Instant::now() + CONVERGENCE_BUDGET;
    loop {
        let (status, body) = list_targets(router, deployment_id, None).await;
        if status == 200 {
            return ids_of(case, None, &body);
        }
        assert_eq!(
            (status, error_code(&body)),
            (400, "DeploymentNotStartedException"),
            "case {case:?}: expected the only refusal a deployment that has not started \
             answers, got {status} with {body}"
        );
        let message = body["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(deployment_id),
            "case {case:?}: expected the refusal to name {deployment_id:?}, got {message:?}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the deployment to start and report its targets within \
             {CONVERGENCE_BUDGET:?}, last {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: see this module's own documentation — the listing is the resolved fleet at every
/// instant from the deployment's start onwards, the pre-start refusal is transient, and the
/// `TargetStatus` narrowing partitions that fleet under the bound the configuration states.
#[tokio::test]
async fn list_deployment_targets_reports_the_fleet_a_server_deployment_resolved() {
    let cases: &[Listed] = &[
        Listed {
            case: "one host, taken out of service at once",
            hosts: 1,
            minimum_healthy_hosts: None,
            builtin: Some(ALL_AT_ONCE),
            concurrency: 1,
        },
        Listed {
            case: "two hosts, both taken out of service at once",
            hosts: 2,
            minimum_healthy_hosts: None,
            builtin: Some(ALL_AT_ONCE),
            concurrency: 2,
        },
        Listed {
            case: "two hosts under a HOST_COUNT of one, which leaves one deployable",
            hosts: 2,
            minimum_healthy_hosts: Some(("HOST_COUNT", 1)),
            builtin: None,
            concurrency: 1,
        },
        Listed {
            case: "three hosts under a HOST_COUNT of one, which leaves two deployable",
            hosts: 3,
            minimum_healthy_hosts: Some(("HOST_COUNT", 1)),
            builtin: None,
            concurrency: 2,
        },
        Listed {
            case: "three hosts under a FLEET_PERCENT of ten, rounded up to one healthy",
            hosts: 3,
            minimum_healthy_hosts: Some(("FLEET_PERCENT", 10)),
            builtin: None,
            concurrency: 2,
        },
        Listed {
            case: "three hosts, one at a time",
            hosts: 3,
            minimum_healthy_hosts: None,
            builtin: Some(ONE_AT_A_TIME),
            concurrency: 1,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        // One emulator per row, so each row's fleet is the whole account's and the ids the
        // listing reports are exactly the hosts this row launched.
        let router = emulator();
        let fleet: BTreeSet<String> = given_running_fleet(&router, case.hosts)
            .await
            .into_iter()
            .collect();
        assert_eq!(
            fleet.len(),
            case.hosts,
            "case {:?}: expected EC2 to have launched {} distinct host(s), got {fleet:?}",
            case.case,
            case.hosts
        );

        let application = format!("listed-{index:02}");
        let group = format!("{application}-dg");
        let config_name = format!("{application}-config");
        if let Some((kind, value)) = case.minimum_healthy_hosts {
            codedeploy_ok(
                &router,
                "CreateDeploymentConfig",
                &json!({
                    "deploymentConfigName": config_name,
                    "computePlatform": "Server",
                    "minimumHealthyHosts": {"type": kind, "value": value},
                }),
            )
            .await;
        }
        let named = case
            .minimum_healthy_hosts
            .map(|_| config_name.as_str())
            .or(case.builtin);
        given_group(&router, &application, &group, &by_tag_filter(), named).await;
        let deployment_id = deploy(&router, &application, &group).await;

        // The pre-start refusal is a WINDOW, not the answer: the listing becomes the fleet.
        let first = targets_once_started(&router, &deployment_id, case.case).await;
        assert_eq!(
            first, fleet,
            "case {:?}: expected the first listing to report the fleet {fleet:?}, got {first:?}",
            case.case
        );

        // And from there on, at every instant until the deployment reaches an outcome.
        let deadline = Instant::now() + CONVERGENCE_BUDGET;
        let mut most_in_progress = 0_usize;
        let outcome = loop {
            let info = deployment_info(&router, &deployment_id).await;
            let status = info["status"].as_str().unwrap_or_default().to_string();

            let all = listed(&router, &deployment_id, None, case.case).await;
            assert_eq!(
                all, fleet,
                "case {:?}: expected the listing to stay the fleet {fleet:?} while the \
                 deployment is {status:?}, got {all:?}",
                case.case
            );

            // One SNAPSHOT of the narrowing: the statuses an in-flight target can hold name
            // the whole fleet together, so each target is in exactly one of them.
            let partitioned =
                listed(&router, &deployment_id, Some(IN_FLIGHT_STATUSES), case.case).await;
            assert_eq!(
                partitioned, fleet,
                "case {:?}: expected {IN_FLIGHT_STATUSES:?} to name the whole fleet {fleet:?} \
                 while the deployment is {status:?}, got {partitioned:?}",
                case.case
            );

            let in_progress =
                listed(&router, &deployment_id, Some(&["InProgress"]), case.case).await;
            assert!(
                in_progress.len() <= case.concurrency,
                "case {:?}: expected at most {} target(s) out of service at once, got \
                 {in_progress:?}",
                case.case,
                case.concurrency
            );
            assert!(
                in_progress.is_subset(&fleet),
                "case {:?}: expected the out-of-service targets to be hosts of the fleet \
                 {fleet:?}, got {in_progress:?}",
                case.case
            );
            most_in_progress = most_in_progress.max(in_progress.len());

            // A filter naming a status no target holds is an empty PAGE, not a refusal.
            let unheld = listed(&router, &deployment_id, Some(UNHELD_STATUS), case.case).await;
            assert!(
                unheld.is_empty(),
                "case {:?}: expected {UNHELD_STATUS:?} to name no target of a deployment that \
                 is succeeding, got {unheld:?}",
                case.case
            );

            if matches!(status.as_str(), "Succeeded" | "Failed" | "Stopped") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "case {:?}: expected the deployment to reach an outcome within \
                 {CONVERGENCE_BUDGET:?}, last {info}",
                case.case
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert_eq!(
            outcome, "Succeeded",
            "case {:?}: expected the deployment over the fleet it resolved to succeed, got \
             {outcome:?}",
            case.case
        );
        assert_eq!(
            most_in_progress, case.concurrency,
            "case {:?}: expected a wave of {} target(s) to have been out of service at once, \
             the most listed was {most_in_progress}",
            case.case, case.concurrency
        );

        // After the outcome the listing is unchanged, every target of it is Succeeded, and
        // reading it again changes none of that: twice over, so a read that consumed
        // something would show.
        for read in 0..2 {
            let all = listed(&router, &deployment_id, None, case.case).await;
            assert_eq!(
                all, fleet,
                "case {:?}: expected read {read} of the finished deployment to still report \
                 the fleet {fleet:?}, got {all:?}",
                case.case
            );
            let succeeded = listed(&router, &deployment_id, Some(&["Succeeded"]), case.case).await;
            assert_eq!(
                succeeded, fleet,
                "case {:?}: expected read {read} to report every host of {fleet:?} Succeeded, \
                 got {succeeded:?}",
                case.case
            );
            for gone in [["Pending"], ["InProgress"]] {
                let still = listed(&router, &deployment_id, Some(&gone), case.case).await;
                assert!(
                    still.is_empty(),
                    "case {:?}: expected read {read} to report no target {gone:?}, got {still:?}",
                    case.case
                );
            }
        }
    }
}
