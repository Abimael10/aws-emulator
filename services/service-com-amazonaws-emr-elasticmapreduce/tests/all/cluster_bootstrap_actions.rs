// SPDX-License-Identifier: CC0-1.0

//! A cluster's bootstrap actions: the ones a launch was given, and what the cluster's
//! bootstrap PHASE does with them, end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so these are the
//! same bytes an SDK client sends. Nothing reaches into the service's state struct: the
//! actions are only ever read back through `ListBootstrapActions`, and the phase only ever
//! through `DescribeCluster`, which is what makes these tests evidence that the answer is
//! computed from persisted state.
//!
//! THE LISTING's RULE: `ListBootstrapActions` reports the bootstrap actions the LAUNCH
//! persisted on the cluster — each one's name, script path and arguments exactly as
//! `RunJobFlow` submitted them, in launch order — paged on `Marker`, and refuses a
//! `ClusterId` it resolves no cluster for the way every cluster read of this service refuses
//! one: HTTP 400, the modelled `InvalidRequestException`, the cluster surface's discriminator
//! and its one sentence. A `Marker` this listing did not mint for this cluster resumes
//! nothing and is refused too.
//!
//! THE PHASE's RULE: those actions "run before Hadoop starts on the cluster nodes"
//! (`RunJobFlow$BootstrapActions`), each fetched from "[the] location in Amazon S3 of the
//! script to run" (`ScriptBootstrapActionConfig$Path`) — so a cluster does not report itself
//! ready until each has run, and an action that did not succeed takes the cluster to
//! `TERMINATED_WITH_ERRORS` with the modelled `BOOTSTRAP_FAILURE` reason, taking its step
//! queue with it. No object store is mounted beside Amazon EMR in this suite, so every script
//! a case names is one the node cannot fetch, which is that half of the rule; the half where
//! scripts really RUN — against Amazon S3's own store, with their own objects read back
//! through `GetObject` — is `core/router/tests/all/emr_bootstrap_action_scripts.rs`.
//!
//! The rows span what the rules generalise over: how many actions a launch carried (none,
//! one, several, more than one page holds), how many arguments each action takes (none, one,
//! many), the form of the script's path, and whether the cluster was to stay up.

use serde_json::{json, Value};
use service_com_amazonaws_emr_elasticmapreduce::state::LIST_BOOTSTRAP_ACTIONS_PAGE_SIZE;

use crate::cluster_id_refusal_code::MALFORMED_CLUSTER_ID;
use crate::job_flow_lifecycle::{
    assert_refused, at, await_cluster_where, kept_alive_request, minimal_request, step_config, Emr,
};
use crate::refusal_messages::message;
use crate::refusal_wording::UNKNOWN_CLUSTER_IDS;
use crate::resource_tags::error_code;

/// One `BootstrapActionConfig` as `RunJobFlow` takes it: "the name of the bootstrap
/// action", and the script it runs with its arguments
/// (`smithy_query(query="shape", shape="BootstrapActionConfig,ScriptBootstrapActionConfig")`).
fn bootstrap_action(name: &str, path: &str, args: Option<Vec<&str>>) -> Value {
    let mut script = json!({ "Path": path });
    if let Some(args) = args {
        script["Args"] = json!(args);
    }
    json!({ "Name": name, "ScriptBootstrapAction": script })
}

/// A kept-alive launch carrying `actions`, or carrying no `BootstrapActions` member at
/// all when `actions` is empty — a launch that asked for none is not the same request as
/// one that asked for an empty list, and both must read back as no actions.
fn launch_with(name: &str, actions: &[Value]) -> Value {
    let mut request = kept_alive_request(name);
    if !actions.is_empty() {
        request["BootstrapActions"] = json!(actions);
    }
    request
}

/// Every action `ListBootstrapActions` reports for `cluster_id`, walking `Marker` to the
/// end, with the number of pages the walk took.
async fn every_action(emr: &Emr, case: &str, cluster_id: &str) -> (Vec<Value>, usize) {
    let mut reported: Vec<Value> = Vec::new();
    let mut marker: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut request = json!({ "ClusterId": cluster_id });
        if let Some(marker) = marker.as_deref() {
            request["Marker"] = json!(marker);
        }
        let out = emr.ok("ListBootstrapActions", request).await;
        pages += 1;
        let page = at(&out, &["BootstrapActions"])
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: BootstrapActions must be a list, got {out}"))
            .clone();
        assert!(
            page.len() <= LIST_BOOTSTRAP_ACTIONS_PAGE_SIZE,
            "case {case:?}: a page holds at most {LIST_BOOTSTRAP_ACTIONS_PAGE_SIZE} actions, \
             got {}",
            page.len()
        );
        reported.extend(page);
        marker = at(&out, &["Marker"]).as_str().map(str::to_string);
        if marker.is_none() {
            return (reported, pages);
        }
        assert!(
            pages < 100,
            "case {case:?}: the listing never ran out of pages, at {} actions",
            reported.len()
        );
    }
}

/// The `Command` members a submitted action must read back as.
fn assert_reported_as(case: &str, submitted: &Value, reported: &Value) {
    assert_eq!(
        at(reported, &["Name"]).as_str(),
        at(submitted, &["Name"]).as_str(),
        "case {case:?}: the name is the submitted one, got {reported}"
    );
    assert_eq!(
        at(reported, &["ScriptPath"]).as_str(),
        at(submitted, &["ScriptBootstrapAction", "Path"]).as_str(),
        "case {case:?}: the script path is the submitted one, got {reported}"
    );
    let submitted_args = at(submitted, &["ScriptBootstrapAction", "Args"]);
    if submitted_args.is_null() {
        assert!(
            at(reported, &["Args"]).is_null(),
            "case {case:?}: an action submitted with no arguments reports none, got {reported}"
        );
    } else {
        assert_eq!(
            args_of(at(reported, &["Args"])),
            args_of(submitted_args),
            "case {case:?}: the arguments are the submitted ones, got {reported}"
        );
    }
}

/// An argument list as a list, for a member a codec may render as an absent empty list.
fn args_of(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}

/// THE RULE: every action a launch carried is reported back, member for member and in
/// launch order, however many there are and however many arguments each takes.
#[tokio::test]
async fn every_bootstrap_action_a_launch_carried_is_reported_as_submitted() {
    let emr = Emr::new();

    // Rows differ along the count of actions and the shape of each one's arguments. The
    // last row carries more actions than one page holds, so the pagination contract is
    // exercised by the same walk.
    let mut long: Vec<Value> = Vec::new();
    for index in 0..(LIST_BOOTSTRAP_ACTIONS_PAGE_SIZE * 2 + 3) {
        long.push(bootstrap_action(
            &format!("step-{index}"),
            &format!("s3://scripts/{index}.sh"),
            Some(vec!["--index", &format!("{index}")]),
        ));
    }
    let cases: Vec<(&str, Vec<Value>)> = vec![
        ("a launch that asked for none", Vec::new()),
        (
            "one action, no arguments",
            vec![bootstrap_action("install", "s3://scripts/install.sh", None)],
        ),
        (
            "one action, an empty argument list",
            vec![bootstrap_action(
                "install",
                "s3://scripts/install.sh",
                Some(Vec::new()),
            )],
        ),
        (
            "three actions, none / one / many arguments",
            vec![
                bootstrap_action("plain", "s3://scripts/plain.sh", None),
                bootstrap_action("one arg", "s3://scripts/one.sh", Some(vec!["--fast"])),
                bootstrap_action(
                    "many args",
                    "s3://scripts/many.sh",
                    Some(vec!["--fast", "--verbose", "/mnt/data", ""]),
                ),
            ],
        ),
        ("more actions than one page holds", long),
    ];

    for (case, actions) in cases {
        let cluster_id = emr.run_job_flow(launch_with(case, &actions)).await;
        let (reported, pages) = every_action(&emr, case, &cluster_id).await;
        assert_eq!(
            reported.len(),
            actions.len(),
            "case {case:?}: expected {} actions, got {reported:?}",
            actions.len()
        );
        let expected_pages = actions.len().div_ceil(LIST_BOOTSTRAP_ACTIONS_PAGE_SIZE).max(1);
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: expected {expected_pages} page(s) for {} actions, got {pages}",
            actions.len()
        );
        for (position, submitted) in actions.iter().enumerate() {
            let case = format!("{case} / action {position}");
            assert_reported_as(&case, submitted, &reported[position]);
        }
    }
}

/// THE RULE's refusal half: a `ClusterId` the service resolves no cluster for is refused
/// exactly as its sibling cluster reads refuse it — the modelled client error, the cluster
/// surface's discriminator and its one sentence — rather than answered with an empty list,
/// which would say the cluster exists and carries no actions.
///
/// Rows span every form of unresolvable id the siblings are asserted over, and a live
/// cluster carrying actions exists throughout, so each refusal is a lookup rather than a
/// blanket refusal of the operation.
#[tokio::test]
async fn an_unresolvable_cluster_id_is_refused_like_every_other_cluster_read() {
    let emr = Emr::new();
    let live = emr
        .run_job_flow(launch_with(
            "live",
            &[bootstrap_action("install", "s3://scripts/install.sh", None)],
        ))
        .await;

    for unknown in UNKNOWN_CLUSTER_IDS {
        let case = format!("ListBootstrapActions with cluster id {unknown:?}");
        let (status, payload) = emr
            .call("ListBootstrapActions", json!({ "ClusterId": unknown }))
            .await;
        assert_refused(&case, status, &payload, "InvalidRequestException");
        assert_eq!(
            error_code(&payload).as_deref(),
            Some(MALFORMED_CLUSTER_ID),
            "case {case:?}: expected ErrorCode {MALFORMED_CLUSTER_ID}, got {payload}"
        );
        assert_eq!(
            message(&case, &payload),
            format!("Cluster id '{unknown}' is not valid."),
            "case {case:?}: the cluster surface's own sentence, got {payload}"
        );
    }

    // The live cluster still reads back, so every refusal above was a lookup of persisted
    // state.
    let (reported, _) = every_action(&emr, "the live cluster", &live).await;
    assert_eq!(
        reported.len(),
        1,
        "the live cluster still reports its one action, got {reported:?}"
    );
}

/// THE RULE's pagination half: a `Marker` this listing did not mint FOR THIS CLUSTER
/// resumes nothing, so it is refused with the marker refusal every paged listing of this
/// service answers — while the marker the listing did mint resumes it.
///
/// Rows span the ways a token can fail to be a resume point: a value that is not a token
/// at all, one that is base64 of something this listing never minted, and a real token of
/// one cluster's listing sent to another cluster.
#[tokio::test]
async fn a_marker_this_listing_did_not_mint_is_refused() {
    let emr = Emr::new();
    let actions: Vec<Value> = (0..(LIST_BOOTSTRAP_ACTIONS_PAGE_SIZE + 2))
        .map(|index| {
            bootstrap_action(
                &format!("step-{index}"),
                &format!("s3://scripts/{index}.sh"),
                None,
            )
        })
        .collect();
    let paged = emr.run_job_flow(launch_with("paged", &actions)).await;
    let other = emr
        .run_job_flow(launch_with(
            "other",
            &[bootstrap_action("other", "s3://scripts/other.sh", None)],
        ))
        .await;

    // The marker this listing minted for the paged cluster, which is what the refusals
    // below are measured against.
    let first = emr
        .ok("ListBootstrapActions", json!({ "ClusterId": &paged }))
        .await;
    let minted = at(&first, &["Marker"])
        .as_str()
        .unwrap_or_else(|| panic!("a cluster with {} actions pages, got {first}", actions.len()))
        .to_string();

    let resumed = emr
        .ok(
            "ListBootstrapActions",
            json!({ "ClusterId": &paged, "Marker": &minted }),
        )
        .await;
    assert_eq!(
        at(&resumed, &["BootstrapActions"]).as_array().map(Vec::len),
        Some(actions.len() - LIST_BOOTSTRAP_ACTIONS_PAGE_SIZE),
        "the minted marker resumes where the first page stopped, got {resumed}"
    );

    // Rows: the cluster the marker is sent to, and the marker. The last row sends a REAL
    // marker of the paged cluster's listing to ANOTHER cluster, which it names no
    // position of.
    let cases: Vec<(&str, &str, String)> = vec![
        ("not a token at all", paged.as_str(), "nonsense".to_string()),
        (
            // base64 of `marker`, a value this listing never encodes.
            "base64 of something this listing never minted",
            paged.as_str(),
            "bWFya2Vy".to_string(),
        ),
        (
            "another cluster's own marker",
            other.as_str(),
            minted.clone(),
        ),
    ];
    for (label, cluster_id, marker) in cases {
        let case = format!("ListBootstrapActions with {label}");
        let (status, payload) = emr
            .call(
                "ListBootstrapActions",
                json!({ "ClusterId": cluster_id, "Marker": &marker }),
            )
            .await;
        assert_refused(&case, status, &payload, "InvalidRequestException");
        assert_eq!(
            error_code(&payload).as_deref(),
            Some("MalformedMarker"),
            "case {case:?}: expected the marker discriminator, got {payload}"
        );
        assert_eq!(
            message(&case, &payload),
            format!("Marker '{marker}' is not valid."),
            "case {case:?}: the marker sentence names the value sent, got {payload}"
        );
    }
}

/// The modelled `ClusterStateChangeReasonCode` a bootstrap failure carries
/// (`smithy_query(query="shape", shape="ClusterStateChangeReasonCode")`), spelled out here
/// rather than imported from the service crate, so a wrong constant in the implementation
/// cannot make the cases below pass.
const BOOTSTRAP_FAILURE: &str = "BOOTSTRAP_FAILURE";

/// The states a cluster never leaves (`com.amazonaws.emr#ClusterState`).
const TERMINAL_STATES: [&str; 2] = ["TERMINATED", "TERMINATED_WITH_ERRORS"];

/// The states a cluster reports once it is up.
const READY_STATES: [&str; 2] = ["RUNNING", "WAITING"];

/// One row of the phase table: the script paths a launch's actions name, and whether the
/// cluster was to stay up once it had no steps to run.
struct Phase {
    case: &'static str,
    paths: Vec<&'static str>,
    kept_alive: bool,
}

/// THE PHASE's RULE: an action whose script did not run takes the cluster DOWN — it reaches
/// `TERMINATED_WITH_ERRORS` carrying the modelled `BOOTSTRAP_FAILURE` reason and the sentence
/// Amazon EMR words a bootstrap download failure with, naming the action by its position in
/// the launch's list — rather than reporting itself ready for a script nothing performed or
/// staying short of ready for ever. Its step queue goes with it.
///
/// No object store is mounted beside Amazon EMR in this suite, so every path a row names is a
/// script the node cannot fetch, whatever form that path takes. The rows span the forms: an
/// address of an object no store here holds, several such actions, an address that names no one
/// object, and a path that is no Amazon S3 address at all — and both answers to whether the
/// cluster was to stay up, since a failing bootstrap phase ends either the same way.
#[tokio::test]
async fn a_bootstrap_action_whose_script_does_not_run_takes_its_cluster_down() {
    let emr = Emr::new();
    let rows = vec![
        Phase {
            case: "one action naming an object no store here holds",
            paths: vec!["s3://scripts/install.sh"],
            kept_alive: true,
        },
        Phase {
            case: "three such actions, of which the first is the one that runs",
            paths: vec![
                "s3://scripts/one.sh",
                "s3://scripts/two.sh",
                "s3://scripts/three.sh",
            ],
            kept_alive: true,
        },
        Phase {
            case: "an address that names no one object",
            paths: vec!["s3://scripts"],
            kept_alive: true,
        },
        Phase {
            case: "a path that is no Amazon S3 address at all",
            paths: vec!["/mnt/var/lib/install.sh"],
            kept_alive: true,
        },
        Phase {
            case: "one action on a cluster that was not to stay up",
            paths: vec!["s3://scripts/transient.sh"],
            kept_alive: false,
        },
    ];

    // Every row is launched before any is polled, so the clusters' phases run together. Each
    // launch carries a step as well: a cluster whose bootstrap phase failed never runs it.
    let mut launched: Vec<String> = Vec::new();
    for row in &rows {
        let mut request = if row.kept_alive {
            kept_alive_request(row.case)
        } else {
            minimal_request(row.case)
        };
        request["BootstrapActions"] = json!(row
            .paths
            .iter()
            .enumerate()
            .map(|(index, path)| bootstrap_action(&format!("action-{index}"), path, None))
            .collect::<Vec<Value>>());
        request["Steps"] = json!([step_config(
            "queued",
            "command-runner.jar",
            json!(["echo", "queued"]),
            Value::Null,
        )]);
        launched.push(emr.run_job_flow(request).await);
    }

    for (row, cluster_id) in rows.iter().zip(&launched) {
        let case = row.case;
        let cluster = await_cluster_where(&emr, cluster_id, case, "a terminal state", |cluster| {
            at(cluster, &["Status", "State"])
                .as_str()
                .is_some_and(|state| TERMINAL_STATES.contains(&state))
        })
        .await;
        assert_eq!(
            at(&cluster, &["Status", "State"]).as_str(),
            Some("TERMINATED_WITH_ERRORS"),
            "case {case:?}: a cluster whose bootstrap action did not succeed ends in the \
             modelled failure state, got {cluster}"
        );
        assert_eq!(
            at(&cluster, &["Status", "StateChangeReason", "Code"]).as_str(),
            Some(BOOTSTRAP_FAILURE),
            "case {case:?}: the shutdown names the modelled reason, got {cluster}"
        );
        // The first action of the launch is the one whose turn came, so it is the one the
        // sentence names. No instance of this cluster exists here — nothing is mounted beside
        // Amazon EMR to launch one — so the sentence names no node.
        assert_eq!(
            at(&cluster, &["Status", "StateChangeReason", "Message"]).as_str(),
            Some("Failed attempting to download bootstrap action 1 file from S3"),
            "case {case:?}: the reason names the action by its position, got {cluster}"
        );
        assert!(
            at(&cluster, &["Status", "Timeline", "ReadyDateTime"]).is_null(),
            "case {case:?}: a cluster that never came up reports no ready instant, got {cluster}"
        );
        // The actions the launch carried are still what the listing reports: the phase answers
        // for them, it does not take them off the cluster.
        let (reported, _) = every_action(&emr, case, cluster_id).await;
        assert_eq!(
            reported.len(),
            row.paths.len(),
            "case {case:?}: the launch's actions are still reported, got {reported:?}"
        );
        // The queue went with the cluster: a step of a cluster whose nodes never came up never
        // ran.
        let listed = emr.list_steps(json!({ "ClusterId": cluster_id })).await;
        let steps = at(&listed, &["Steps"]).as_array().cloned().unwrap_or_default();
        for step in &steps {
            assert_eq!(
                at(step, &["Status", "State"]).as_str(),
                Some("CANCELLED"),
                "case {case:?}: a step of such a cluster never ran, got {step}"
            );
            assert!(
                at(step, &["Status", "Timeline", "StartDateTime"]).is_null(),
                "case {case:?}: a step that never ran reports no start, got {step}"
            );
        }
        assert_eq!(
            steps.len(),
            1,
            "case {case:?}: the launched step is still reported, got {listed}"
        );
    }
}

/// The other half of THE PHASE's RULE: a launch that carried NO bootstrap action is owed none,
/// so its cluster comes up exactly as it did before — the phase gates a cluster on the actions
/// its launch really carried and on nothing else.
///
/// The rows are the two forms a launch takes here: one that asked for no `BootstrapActions`
/// member at all, and one that asked for an empty list.
#[tokio::test]
async fn a_launch_that_carried_no_bootstrap_action_comes_up_ready() {
    let emr = Emr::new();
    let cases = ["no BootstrapActions member", "an empty BootstrapActions list"];
    let mut launched: Vec<String> = Vec::new();
    for case in cases {
        let mut request = kept_alive_request(case);
        if case.contains("empty") {
            request["BootstrapActions"] = json!([]);
        }
        launched.push(emr.run_job_flow(request).await);
    }

    for (case, cluster_id) in cases.iter().zip(&launched) {
        let cluster = await_cluster_where(&emr, cluster_id, case, "a ready state", |cluster| {
            at(cluster, &["Status", "State"])
                .as_str()
                .is_some_and(|state| READY_STATES.contains(&state))
        })
        .await;
        assert_eq!(
            at(&cluster, &["Status", "State"]).as_str(),
            Some("WAITING"),
            "case {case:?}: a cluster kept alive with no steps and no actions comes up waiting, \
             got {cluster}"
        );
        assert!(
            at(&cluster, &["Status", "Timeline", "ReadyDateTime"])
                .as_f64()
                .is_some(),
            "case {case:?}: a cluster that is up reports when it became ready, got {cluster}"
        );
    }
}
