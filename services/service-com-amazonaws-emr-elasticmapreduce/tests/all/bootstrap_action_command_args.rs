// SPDX-License-Identifier: CC0-1.0

//! THE RULE: `Args` is a member Amazon EMR serializes on EVERY `Command` a bootstrap-action
//! report carries — "arguments for Amazon EMR to pass to the command for execution"
//! (`smithy_query(query="shape", shape="Command")`) — so an action whose launch submitted no
//! argument reports the EMPTY list, not an absent member. The member's presence does not vary
//! from element to element, so a caller that reads `BootstrapActions[i].Args` and iterates it
//! never meets a missing key where a sibling element carries a list.
//!
//! Every case drives the real HTTP path — `POST /` with `X-Amz-Target: ElasticMapReduce.<Op>`
//! and an `awsJson1_1` body — through the router's `dispatch`, using the harness
//! `tests/all/job_flow_lifecycle.rs` owns, and reads the actions back only through
//! `ListBootstrapActions`, so the answer is computed from the state `RunJobFlow` persisted.
//!
//! The rows span what the rule generalises over: how a launch expressed "no arguments" (no
//! `Args` member at all, or an explicit empty list), how many arguments an action that has
//! them takes (one, several, one that is itself the empty string), where the argument-less
//! action sits in the launch's list (first, middle, last, alone), and whether the reported
//! element arrives on the first page or on one resumed by a `Marker`.

use serde_json::{json, Value};
use service_com_amazonaws_emr_elasticmapreduce::state::LIST_BOOTSTRAP_ACTIONS_PAGE_SIZE;

use crate::job_flow_lifecycle::{at, kept_alive_request, Emr};

/// How a row expresses the arguments of one bootstrap action.
#[derive(Clone, Copy, Debug)]
enum Args {
    /// The launch named no `ScriptBootstrapAction.Args` member at all.
    Absent,
    /// The launch named `Args`, with these values (possibly none).
    Named(&'static [&'static str]),
}

impl Args {
    /// The arguments the report must carry for this action: both ways of saying "none" read
    /// back as the empty list.
    fn expected(self) -> Vec<Value> {
        match self {
            Args::Absent => Vec::new(),
            Args::Named(args) => args.iter().map(|arg| json!(arg)).collect(),
        }
    }
}

/// One `BootstrapActionConfig` as `RunJobFlow` takes it
/// (`smithy_query(query="shape", shape="BootstrapActionConfig,ScriptBootstrapActionConfig")`).
fn action(name: &str, path: &str, args: Args) -> Value {
    let mut script = json!({ "Path": path });
    if let Args::Named(args) = args {
        script["Args"] = json!(args);
    }
    json!({ "Name": name, "ScriptBootstrapAction": script })
}

/// Every action `ListBootstrapActions` reports for `cluster_id`, walking `Marker` to the end,
/// so a row longer than one page is read the same way as a short one.
async fn reported_actions(emr: &Emr, case: &str, cluster_id: &str) -> Vec<Value> {
    let mut reported: Vec<Value> = Vec::new();
    let mut marker: Option<String> = None;
    for _ in 0..100 {
        let mut request = json!({ "ClusterId": cluster_id });
        if let Some(marker) = marker.as_deref() {
            request["Marker"] = json!(marker);
        }
        let out = emr.ok("ListBootstrapActions", request).await;
        let page = at(&out, &["BootstrapActions"])
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: BootstrapActions must be a list, got {out}"))
            .clone();
        reported.extend(page);
        marker = at(&out, &["Marker"]).as_str().map(str::to_string);
        if marker.is_none() {
            return reported;
        }
    }
    panic!("case {case:?}: the listing never ran out of pages, at {reported:?}");
}

/// THE RULE, over the table: every reported element carries `Args`, as a list, holding exactly
/// the arguments the launch submitted — the empty list where it submitted none, however the
/// launch said so and wherever that action sits.
#[tokio::test]
async fn every_reported_bootstrap_action_carries_args_as_a_list() {
    let emr = Emr::new();

    // A launch longer than one page, whose LAST action — the one a resumed page reports — is
    // the argument-less one, so the rule is asserted on a page the `Marker` resumed too.
    let mut paged: Vec<(&'static str, Args)> = Vec::new();
    for _ in 0..LIST_BOOTSTRAP_ACTIONS_PAGE_SIZE {
        paged.push(("with args", Args::Named(&["--fast"])));
    }
    paged.push(("resumed, no args", Args::Absent));

    let rows: Vec<(&str, Vec<(&'static str, Args)>)> = vec![
        ("one action, no Args member", vec![("plain", Args::Absent)]),
        (
            "one action, an explicit empty Args list",
            vec![("empty", Args::Named(&[]))],
        ),
        (
            "argument-less action FIRST, one with arguments after it",
            vec![
                ("plain", Args::Absent),
                ("armed", Args::Named(&["--fast", "--verbose"])),
            ],
        ),
        (
            "argument-less action LAST, which is the recorded shape",
            vec![
                ("armed", Args::Named(&["--fast"])),
                ("plain", Args::Absent),
            ],
        ),
        (
            "argument-less action in the MIDDLE, between two armed ones",
            vec![
                ("armed one", Args::Named(&["--one"])),
                ("plain", Args::Named(&[])),
                ("armed many", Args::Named(&["--a", "--b", "--c", "--d"])),
            ],
        ),
        (
            "an argument that is itself the empty string",
            vec![
                ("empty string arg", Args::Named(&[""])),
                ("plain", Args::Absent),
            ],
        ),
        ("more actions than one page holds", paged),
    ];

    for (case, actions) in rows {
        let mut request = kept_alive_request(case);
        request["BootstrapActions"] = json!(actions
            .iter()
            .enumerate()
            .map(|(index, (name, args))| action(
                &format!("{name}-{index}"),
                &format!("s3://scripts/{index}.sh"),
                *args
            ))
            .collect::<Vec<Value>>());
        let cluster_id = emr.run_job_flow(request).await;

        let reported = reported_actions(&emr, case, &cluster_id).await;
        assert_eq!(
            reported.len(),
            actions.len(),
            "case {case:?}: expected {} reported action(s), got {reported:?}",
            actions.len()
        );
        for (position, (name, args)) in actions.iter().enumerate() {
            let element = &reported[position];
            let member = element.get("Args");
            assert!(
                member.is_some(),
                "case {case:?} / action {position} ({name}, {args:?}): Args is serialized on \
                 every element, got {element}"
            );
            assert_eq!(
                member.and_then(Value::as_array),
                Some(&args.expected()),
                "case {case:?} / action {position} ({name}, {args:?}): expected {:?}, got \
                 {element}",
                args.expected()
            );
        }
    }
}

/// The uniformity half of THE RULE, stated as the invariant a client relies on: across ONE
/// response, every element's member set is the same, so `Args` is never present on one element
/// and absent on its neighbour.
#[tokio::test]
async fn args_is_present_on_every_element_of_one_response() {
    let emr = Emr::new();
    // A launch mixing both forms of "no arguments" with armed actions, which is exactly the
    // mixture whose element-to-element difference the rule forbids.
    let actions = [
        ("armed", Args::Named(&["--fast"])),
        ("no member", Args::Absent),
        ("empty list", Args::Named(&[])),
        ("armed many", Args::Named(&["--a", "--b"])),
    ];
    let case = "a launch mixing armed and argument-less actions";
    let mut request = kept_alive_request(case);
    request["BootstrapActions"] = json!(actions
        .iter()
        .enumerate()
        .map(|(index, (name, args))| action(name, &format!("s3://scripts/{index}.sh"), *args))
        .collect::<Vec<Value>>());
    let cluster_id = emr.run_job_flow(request).await;

    let reported = reported_actions(&emr, case, &cluster_id).await;
    let member_sets: Vec<Vec<String>> = reported
        .iter()
        .map(|element| {
            let mut keys: Vec<String> = element
                .as_object()
                .unwrap_or_else(|| panic!("case {case:?}: an element is a structure, got {element}"))
                .keys()
                .cloned()
                .collect();
            keys.sort();
            keys
        })
        .collect();
    let first = member_sets
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: the launch's actions are reported"))
        .clone();
    assert!(
        first.contains(&"Args".to_string()),
        "case {case:?}: Args is one of the members every element carries, got {first:?}"
    );
    for (position, members) in member_sets.iter().enumerate() {
        assert_eq!(
            members, &first,
            "case {case:?}: element {position} carries the same members as the first, got \
             {members:?}"
        );
    }
}
