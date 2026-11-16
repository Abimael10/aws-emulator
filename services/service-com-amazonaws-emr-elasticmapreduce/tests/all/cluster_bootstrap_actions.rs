// SPDX-License-Identifier: CC0-1.0

//! The bootstrap actions a cluster was LAUNCHED with, end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so these are the
//! same bytes an SDK client sends. Nothing reaches into the service's state struct: the
//! actions are only ever read back through `ListBootstrapActions`, which is what makes
//! these tests evidence that the answer is computed from persisted state.
//!
//! THE RULE: `ListBootstrapActions` reports the bootstrap actions the LAUNCH persisted on
//! the cluster — each one's name, script path and arguments exactly as `RunJobFlow`
//! submitted them, in launch order — paged on `Marker`, and refuses a `ClusterId` it
//! resolves no cluster for the way every cluster read of this service refuses one: HTTP
//! 400, the modelled `InvalidRequestException`, the cluster surface's discriminator and
//! its one sentence. A `Marker` this listing did not mint for this cluster resumes
//! nothing and is refused too.
//!
//! The rows span what the rule generalises over: how many actions a launch carried (none,
//! one, several, more than one page holds) and how many arguments each action takes (none,
//! one, many).

use serde_json::{json, Value};
use service_com_amazonaws_emr_elasticmapreduce::state::LIST_BOOTSTRAP_ACTIONS_PAGE_SIZE;

use crate::cluster_id_refusal_code::MALFORMED_CLUSTER_ID;
use crate::job_flow_lifecycle::{assert_refused, at, kept_alive_request, Emr};
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
