// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Route 53 ARC cluster (data-plane) routing-control
//! toggle loop: `ListRoutingControls`, `GetRoutingControlState`,
//! `UpdateRoutingControlState` and `UpdateRoutingControlStates`.
//!
//! Every call goes over the wire the real SDK uses — `POST /` with
//! `X-Amz-Target: ToggleCustomerAPI.<Op>` and an awsJson1_0 body — through the
//! same `MultiServiceRouter` the emulator boots, so what is asserted is the
//! service's HTTP behaviour and not an internal helper. The fleet every case runs
//! against is created through the CONTROL plane (`super::common::fleet`), which is
//! the only way a routing control exists at all.
//!
//! The rules under test are asserted over a TABLE of inputs (every routing control of
//! the fleet, every modelled state value, every page size, every control panel and
//! every refusal class), not over one recorded request.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_route53recoverycluster_togglecustomerapi::ops::types::ROUTING_CONTROL_STATE_VALUES;
use service_com_amazonaws_route53recoverycluster_togglecustomerapi::state::MAX_UPDATE_ROUTING_CONTROL_STATE_ENTRIES;

use super::common::{
    call, delete_routing_control, emulator, error_code, fleet, get_state, list_all_pages,
    listed_arns, unknown_arn_like, RESOURCE_NOT_FOUND_STATUS, Fleet,
};
use smithy_server_runtime::MultiServiceRouter;

/// A cursor in the encoding `ListRoutingControls` issues (an unfiltered listing
/// resuming at `arn`), so a token naming a routing control that is not in the set can be
/// handed back.
fn cursor_for(arn: &str) -> String {
    hex::encode(format!("v1||{arn}"))
}


/// RULE: a routing control's reported state is the state last written to it, for
/// every control and every modelled state value — never an echo of the request
/// and never a canned value.
///
/// The table spans every routing control of the fleet (5, across 3 control panels and 2
/// clusters) crossed with every value of the modelled `RoutingControlState` enum,
/// including setting a control to the state it is already in.
#[tokio::test]
async fn update_then_get_reports_the_state_that_was_written() {
    let router = emulator();
    let fleet = fleet(&router).await;

    // What every control's state must be, as the table is walked. A newly created
    // routing control starts Off.
    let mut expected: std::collections::BTreeMap<String, &str> =
        fleet.arns().into_iter().map(|arn| (arn, "Off")).collect();

    for (arn, state) in &expected {
        let before = get_state(&router, arn).await;
        assert_eq!(
            before.get("RoutingControlState").and_then(Value::as_str),
            Some(*state),
            "case {arn}: a newly created control starts {state}, got {before}"
        );
    }

    for control in fleet.controls() {
        let arn = &control.arn;
        // The value set comes from the model's own `RoutingControlState` enum, and
        // is walked twice so each control is also set to the state it is already in.
        for state in ROUTING_CONTROL_STATE_VALUES
            .iter()
            .chain(ROUTING_CONTROL_STATE_VALUES.iter())
        {
            let (status, body) = call(
                &router,
                "UpdateRoutingControlState",
                &json!({ "RoutingControlArn": arn, "RoutingControlState": state }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {arn} -> {state}: expected 200, got {status} with {body}"
            );
            assert_eq!(
                body,
                json!({}),
                "case {arn} -> {state}: UpdateRoutingControlState has an empty output shape, got {body}"
            );
            expected.insert(arn.clone(), *state);

            // EVERY control is re-read: the one written reports the new state, and a
            // toggle of one control moves no other.
            for (other, other_state) in &expected {
                let read_back = get_state(&router, other).await;
                assert_eq!(
                    read_back.get("RoutingControlState").and_then(Value::as_str),
                    Some(*other_state),
                    "case {arn} -> {state}: {other} must report {other_state}, got {read_back}"
                );
                assert_eq!(
                    read_back.get("RoutingControlArn").and_then(Value::as_str),
                    Some(other.as_str()),
                    "case {other}: the response echoes the looked-up ARN, got {read_back}"
                );
            }
        }

        // The name comes from the control plane's registry, not from the ARN.
        let read_back = get_state(&router, arn).await;
        assert_eq!(
            read_back.get("RoutingControlName").and_then(Value::as_str),
            Some(control.name.as_str()),
            "case {arn}: the control's name must come from the registry, got {read_back}"
        );
    }
}

/// RULE: a state value outside the modelled `RoutingControlState` value set is
/// refused with HTTP 400 `ValidationException`, and writes nothing.
///
/// The rows differ in form (wrong case, a prefix/suffix of a legal value, empty,
/// an unrelated word), and each is sent to BOTH update operations.
#[tokio::test]
async fn a_state_outside_the_modelled_value_set_is_refused() {
    let router = emulator();
    let fleet = fleet(&router).await;
    let arn = fleet.arns().first().expect("the fleet has controls").clone();

    for bogus in ["ON", "on", "OFF", "off", "Offline", "On ", "", "Unknown"] {
        for (operation, body) in [
            (
                "UpdateRoutingControlState",
                json!({ "RoutingControlArn": arn, "RoutingControlState": bogus }),
            ),
            (
                "UpdateRoutingControlStates",
                json!({ "UpdateRoutingControlStateEntries": [
                    { "RoutingControlArn": arn, "RoutingControlState": bogus }
                ]}),
            ),
        ] {
            let (status, response) = call(&router, operation, &body).await;
            assert_eq!(
                status, 400,
                "case {operation}({bogus:?}): expected 400, got {status} with {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {operation}({bogus:?}): expected ValidationException, got {response}"
            );
        }
    }

    // Nothing was written by any refused request.
    let state = get_state(&router, &arn).await;
    assert_eq!(
        state.get("RoutingControlState").and_then(Value::as_str),
        Some("Off"),
        "a refused update must write nothing, got {state}"
    );
}

/// RULE: an ARN no control-plane routing control was minted for is
/// `ResourceNotFoundException`, and a read never registers it.
///
/// The rows are well-formed routing control ARNs that name nothing, of several shapes: an
/// unknown control panel, an unknown control in a known panel, and an id of another form.
/// A value that is not the ARN of a routing control at all is a different refusal
/// (`crate::malformed_arn_refusal`), so every row here is a routing control's ARN.
#[tokio::test]
async fn an_unknown_routing_control_is_not_found_and_is_not_created() {
    let router = emulator();
    let fleet = fleet(&router).await;
    let known = fleet.controls().first().expect("the fleet has controls").clone();
    let unknown = [
        format!(
            "{}/routingcontrol/{}",
            unknown_arn_like(&known.panel_arn),
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        ),
        unknown_arn_like(&known.arn),
        format!("{}/routingcontrol/{}", known.panel_arn, "0"),
    ];

    for arn in &unknown {
        let (status, body) = call(
            &router,
            "GetRoutingControlState",
            &json!({ "RoutingControlArn": arn }),
        )
        .await;
        assert_eq!(
            status, RESOURCE_NOT_FOUND_STATUS,
            "case {arn}: expected {RESOURCE_NOT_FOUND_STATUS}, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {arn}: expected ResourceNotFoundException, got {body}"
        );

        let (update_status, update_body) = call(
            &router,
            "UpdateRoutingControlState",
            &json!({ "RoutingControlArn": arn, "RoutingControlState": "On" }),
        )
        .await;
        assert_eq!(
            update_status, RESOURCE_NOT_FOUND_STATUS,
            "case update {arn}: expected {RESOURCE_NOT_FOUND_STATUS}, got {update_status} with \
             {update_body}"
        );
        assert_eq!(
            error_code(&update_body),
            "ResourceNotFoundException",
            "case update {arn}: expected ResourceNotFoundException, got {update_body}"
        );
    }

    // No read or refused write registered anything: the fleet is unchanged.
    let listed: BTreeSet<String> = listed_arns(&list_all_pages(&router, None, None, 1).await)
        .into_iter()
        .collect();
    assert_eq!(
        listed,
        fleet.arns().into_iter().collect::<BTreeSet<_>>(),
        "a reference to an unknown ARN must not add anything to the registry"
    );
}

/// RULE: `ListRoutingControls` reports exactly the routing controls the control plane
/// holds, in a stable order, and its pages walk the whole set without repeats or
/// omissions for ANY page size.
///
/// The rows are every page size from 1 to one past the size of the set, plus
/// "unspecified" (the modelled default of 500).
#[tokio::test]
async fn list_pages_walk_the_whole_set_for_every_page_size() {
    let router = emulator();
    let fleet = fleet(&router).await;
    let expected = fleet.arns();

    let mut page_sizes: Vec<Option<u32>> = (1..=(expected.len() as u32 + 1)).map(Some).collect();
    page_sizes.push(None);

    for max_results in page_sizes {
        let reported = listed_arns(&list_all_pages(&router, None, max_results, expected.len()).await);
        assert_eq!(
            reported, expected,
            "case MaxResults={max_results:?}: the pages must walk the whole set in order, got {reported:?}"
        );
    }
}

/// RULE: each reported routing control carries the control panel that hosts it,
/// and a `ControlPanelArn` filter reports exactly that panel's controls; a control
/// panel no control-plane resource was minted for is `ResourceNotFoundException`.
///
/// The rows are every control panel of the fleet (3, one of them in a different
/// cluster) plus an unknown panel.
#[tokio::test]
async fn listing_filters_by_control_panel() {
    let router = emulator();
    let fleet = fleet(&router).await;

    for panel in &fleet.panels {
        let expected: Vec<String> = {
            let mut arns: Vec<String> = panel
                .controls
                .iter()
                .map(|control| control.arn.clone())
                .collect();
            arns.sort();
            arns
        };

        // Page size 1 as well as unpaged, so the filter is proven to survive the cursor.
        for max_results in [None, Some(1)] {
            let page = list_all_pages(&router, Some(&panel.arn), max_results, expected.len()).await;
            let reported = listed_arns(&page);
            assert_eq!(
                reported, expected,
                "case panel {} (MaxResults={max_results:?}): expected only that panel's controls, got {reported:?}",
                panel.name
            );
            for control in &page {
                assert_eq!(
                    control.get("ControlPanelArn").and_then(Value::as_str),
                    Some(panel.arn.as_str()),
                    "case panel {}: every reported control names its panel, got {control}",
                    panel.name
                );
                assert_eq!(
                    control.get("ControlPanelName").and_then(Value::as_str),
                    Some(panel.name.as_str()),
                    "case panel {}: every reported control names its panel, got {control}",
                    panel.name
                );
                assert_eq!(
                    control.get("Owner").and_then(Value::as_str),
                    Some(engine::emulator_account_id()),
                    "case panel {}: the owner is the emulator account, got {control}",
                    panel.name
                );
            }
        }
    }

    let (status, body) = call(
        &router,
        "ListRoutingControls",
        &json!({ "ControlPanelArn": unknown_arn_like(&fleet.panels[0].arn) }),
    )
    .await;
    assert_eq!(
        status, RESOURCE_NOT_FOUND_STATUS,
        "case unknown control panel: expected {RESOURCE_NOT_FOUND_STATUS}, got {status} with \
         {body}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceNotFoundException",
        "case unknown control panel: expected ResourceNotFoundException, got {body}"
    );
}

/// RULE: `MaxResults` is modelled with `@range(min: 1)`, so a value below that
/// bound is refused with HTTP 400 `ValidationException` rather than silently read
/// as "unspecified"; a value at or above the bound is served.
///
/// The rows are the published bound (1), one past it (0), one further past it
/// (-1, which an SDK can send for an `integer` member), and a value far above the
/// size of the set.
#[tokio::test]
async fn max_results_outside_the_modelled_range_is_refused() {
    let router = emulator();
    let fleet = fleet(&router).await;
    let total = fleet.arns().len();

    for refused in [0, -1, i32::MIN] {
        let (status, body) = call(
            &router,
            "ListRoutingControls",
            &json!({ "MaxResults": refused }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case MaxResults={refused}: expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case MaxResults={refused}: expected ValidationException, got {body}"
        );
    }

    for accepted in [1, total as i32, 1_000, i32::MAX] {
        let (status, body) = call(
            &router,
            "ListRoutingControls",
            &json!({ "MaxResults": accepted }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case MaxResults={accepted}: expected 200, got {status} with {body}"
        );
        let page = body
            .get("RoutingControls")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case MaxResults={accepted}: RoutingControls is required, got {body}"));
        let expected = (accepted as usize).min(total);
        assert_eq!(
            page.len(),
            expected,
            "case MaxResults={accepted}: expected {expected} control(s) on the page, got {}",
            page.len()
        );
    }
}

/// RULE: a `NextToken` is only valid for the call that issued it — it carries the
/// filter it was issued for and the routing control it resumes at, and a token this
/// service did not issue (one it cannot decode, or one bound to another
/// `ControlPanelArn`) is refused with HTTP 400 `ValidationException`.
///
/// A token this service CAN decode names a position in the listing instead, and is
/// served from it whether or not the registry still holds that routing control —
/// `a_page_resumes_at_the_position_a_decodable_token_names` and
/// `a_deleted_resume_control_does_not_end_the_pagination_walk` are the rows for that.
#[tokio::test]
async fn a_next_token_is_only_valid_for_the_call_that_issued_it() {
    let router = emulator();
    let fleet = fleet(&router).await;

    let (status, first_page) = call(&router, "ListRoutingControls", &json!({ "MaxResults": 1 })).await;
    assert_eq!(status, 200, "the first page must succeed, got {first_page}");
    let token = first_page
        .get("NextToken")
        .and_then(Value::as_str)
        .expect("a set larger than one page issues a NextToken")
        .to_string();

    // Rows: the unfiltered token replayed under a filter, under another filter,
    // and a token this service never issued at all.
    let cases: Vec<(String, Value)> = vec![
        (
            "a token issued unfiltered, replayed with a filter".to_string(),
            json!({ "MaxResults": 1, "NextToken": token,
                    "ControlPanelArn": fleet.panels[0].arn }),
        ),
        (
            "a token issued unfiltered, replayed with another filter".to_string(),
            json!({ "MaxResults": 1, "NextToken": token,
                    "ControlPanelArn": fleet.panels[2].arn }),
        ),
        (
            "not a token at all".to_string(),
            json!({ "NextToken": "not-a-token" }),
        ),
        (
            "a token in a version this build does not know".to_string(),
            json!({ "NextToken": hex::encode(format!("v0||{}", fleet.arns()[0])) }),
        ),
        (
            "a token carrying more fields than the encoding has".to_string(),
            json!({ "NextToken": hex::encode(format!("v1||{}|extra", fleet.arns()[0])) }),
        ),
    ];

    for (case, request) in cases {
        let (status, body) = call(&router, "ListRoutingControls", &request).await;
        assert_eq!(
            status, 400,
            "case {case}: expected 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case}: expected ValidationException, got {body}"
        );
    }
}

/// Follow the rest of a `ListRoutingControls` walk from `token`, under the same filter
/// and page size, and answer the `RoutingControlArn` of every control the remaining
/// pages report.
///
/// `bound` is one more page than the remaining set can need, so a cursor that never
/// terminates fails the test instead of hanging.
async fn follow_from_token(
    router: &MultiServiceRouter,
    control_panel_arn: Option<&str>,
    max_results: Option<u32>,
    token: &str,
    bound: usize,
    case: &str,
) -> Vec<String> {
    let mut collected: Vec<String> = Vec::new();
    let mut next_token = token.to_string();
    for _ in 0..(bound + 2) {
        let mut request = serde_json::Map::new();
        if let Some(panel) = control_panel_arn {
            request.insert("ControlPanelArn".to_string(), json!(panel));
        }
        if let Some(size) = max_results {
            request.insert("MaxResults".to_string(), json!(size));
        }
        request.insert("NextToken".to_string(), json!(next_token));
        let (status, body) = call(router, "ListRoutingControls", &Value::Object(request)).await;
        assert_eq!(
            status, 200,
            "case {case}: a token this call issued must be served, got {status} with {body}"
        );
        let page = body
            .get("RoutingControls")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case}: RoutingControls is required, got {body}"))
            .clone();
        if let Some(size) = max_results {
            assert!(
                page.len() <= size as usize,
                "case {case}: a page must not exceed MaxResults, got {} entries",
                page.len()
            );
        }
        collected.extend(listed_arns(&page));
        match body.get("NextToken").and_then(Value::as_str) {
            None => return collected,
            Some(issued) => next_token = issued.to_string(),
        }
    }
    panic!("case {case}: ListRoutingControls kept issuing a NextToken past the end of the set");
}

/// RULE: a `NextToken` names a POSITION in the listing's order, so the next page resumes
/// at the first routing control whose ARN is not below the one the token names — a
/// routing control the control plane deletes between two pages is left out of the
/// listing, it does not end the walk, which still reports every control that is still
/// registered and was not reported before.
///
/// The rows differ along the page size, how many controls are deleted at the resume
/// point (one, two, every one the walk had left), and whether the listing carries a
/// `ControlPanelArn` filter.
#[tokio::test]
async fn a_deleted_resume_control_does_not_end_the_pagination_walk() {
    // (case, page size, the control panel of the fleet the listing is filtered by, how
    // many controls to delete from the resume point on)
    let rows: Vec<(&str, u32, Option<usize>, usize)> = vec![
        (
            "page size 1, the control the next page resumes at deleted",
            1,
            None,
            1,
        ),
        (
            "page size 1, the resume control and the one after it deleted",
            1,
            None,
            2,
        ),
        (
            "page size 2, the control the next page resumes at deleted",
            2,
            None,
            1,
        ),
        (
            "page size 1, every control the walk had left deleted",
            1,
            None,
            4,
        ),
        (
            "page size 1 filtered by a control panel, the resume control deleted",
            1,
            Some(0),
            1,
        ),
        (
            "page size 2 filtered by a control panel, its last control deleted",
            2,
            Some(0),
            1,
        ),
    ];

    for (case, max_results, panel, deletions) in rows {
        let router = emulator();
        let fleet = fleet(&router).await;
        let filter = panel.map(|index| fleet.panels[index].arn.clone());
        let scope: Vec<String> = match &filter {
            None => fleet.arns(),
            Some(panel_arn) => fleet
                .controls()
                .into_iter()
                .filter(|control| &control.panel_arn == panel_arn)
                .map(|control| control.arn)
                .collect(),
        };
        let first = max_results as usize;
        assert!(
            scope.len() > first,
            "case {case}: the row needs a set larger than one page, got {} control(s)",
            scope.len()
        );

        // The first page, and the token it issues for the controls past it.
        let mut request = serde_json::Map::new();
        if let Some(panel_arn) = &filter {
            request.insert("ControlPanelArn".to_string(), json!(panel_arn));
        }
        request.insert("MaxResults".to_string(), json!(max_results));
        let (status, first_page) =
            call(&router, "ListRoutingControls", &Value::Object(request)).await;
        assert_eq!(
            status, 200,
            "case {case}: the first page must succeed, got {status} with {first_page}"
        );
        let reported_first = listed_arns(
            first_page
                .get("RoutingControls")
                .and_then(Value::as_array)
                .unwrap_or_else(|| {
                    panic!("case {case}: RoutingControls is required, got {first_page}")
                }),
        );
        assert_eq!(
            reported_first,
            scope[..first].to_vec(),
            "case {case}: the first page must report the start of the set, got {reported_first:?}"
        );
        let token = first_page
            .get("NextToken")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case}: a set larger than one page issues a NextToken, got {first_page}")
            })
            .to_string();

        // Delete the control the next page is to resume at, and the ones after it this
        // row names, through the control plane that owns them.
        let deleted: Vec<String> = scope[first..(first + deletions).min(scope.len())].to_vec();
        for arn in &deleted {
            delete_routing_control(&router, arn).await;
        }

        let rest = follow_from_token(
            &router,
            filter.as_deref(),
            Some(max_results),
            &token,
            scope.len(),
            case,
        )
        .await;
        let expected: Vec<String> = scope[first..]
            .iter()
            .filter(|arn| !deleted.contains(arn))
            .cloned()
            .collect();
        assert_eq!(
            rest, expected,
            "case {case}: the walk must continue over the controls still registered, got {rest:?}"
        );

        // And the walk as a whole reported every still-registered control exactly once.
        let walked: Vec<String> = reported_first.into_iter().chain(rest).collect();
        let still_registered: Vec<String> = scope
            .iter()
            .filter(|arn| !deleted.contains(arn))
            .cloned()
            .collect();
        assert_eq!(
            walked, still_registered,
            "case {case}: the pages must walk the whole remaining set without repeats or \
             omissions, got {walked:?}"
        );
    }
}

/// RULE: a token this call can decode names a position in the listing's ARN order and is
/// served from it — the page starts at the first routing control whose ARN is not below
/// that position, and a position past every control is an empty final page carrying no
/// `NextToken`.
///
/// The rows are a position before every control, the position of a control that IS
/// registered, a position between two controls that no control is registered at, and a
/// position past every control; each is run unpaged and at two page sizes. Every cursor
/// is built out of the fleet's own ARNs, so it stays inside the modelled `Arn` pattern
/// `^[A-Za-z0-9:.\/_-]*$` (`smithy_query(query="shape",shape="Arn")`): `arn` is a prefix
/// of every one of them, and an ARN with one more character sorts straight after the ARN
/// it extends, which — ARNs of one resource type having equal length — is before the
/// next one.
#[tokio::test]
async fn a_page_resumes_at_the_position_a_decodable_token_names() {
    let router = emulator();
    let fleet = fleet(&router).await;
    let arns = fleet.arns();
    let last = arns.last().expect("the fleet has routing controls").clone();

    // (case, the position the cursor names, the controls the walk from it must report)
    let rows: Vec<(&str, String, Vec<String>)> = vec![
        (
            "a position before every routing control",
            "arn".to_string(),
            arns.clone(),
        ),
        (
            "the position of a routing control that is registered",
            arns[1].clone(),
            arns[1..].to_vec(),
        ),
        (
            "a position between two routing controls that none is registered at",
            format!("{}0", arns[0]),
            arns[1..].to_vec(),
        ),
        (
            "a position past every routing control",
            format!("{last}0"),
            Vec::new(),
        ),
    ];

    for (case, position, expected) in rows {
        for max_results in [None, Some(1), Some(2)] {
            let case = format!("{case} (MaxResults={max_results:?})");
            let reported = follow_from_token(
                &router,
                None,
                max_results,
                &cursor_for(&position),
                arns.len(),
                &case,
            )
            .await;
            assert_eq!(
                reported, expected,
                "case {case}: the page must resume at the named position, got {reported:?}"
            );
        }
    }
}

/// RULE: `UpdateRoutingControlStates` applies every entry or none — it validates
/// the whole batch first and commits under one lock, so a refused batch leaves the
/// registry exactly as it was.
///
/// The accepted rows differ in size and in the panels they span (one entry, two
/// entries in one panel, two entries across two panels of ONE cluster, the whole
/// cluster at once, and mixed target states). The refused rows are one per refusal
/// class, each checked to have written nothing.
#[tokio::test]
async fn a_batch_update_applies_every_entry_or_none() {
    // Accepted batches: (case, how the entries are chosen out of the fleet).
    type Entries = fn(&Fleet) -> Vec<(String, &'static str)>;
    let accepted: Vec<(&str, Entries)> = vec![
        ("one entry", |fleet| {
            vec![(fleet.panels[0].controls[0].arn.clone(), "On")]
        }),
        ("two entries in one control panel", |fleet| {
            vec![
                (fleet.panels[0].controls[0].arn.clone(), "Off"),
                (fleet.panels[0].controls[1].arn.clone(), "On"),
            ]
        }),
        (
            "two entries across two control panels of one cluster",
            |fleet| {
                vec![
                    (fleet.panels[0].controls[0].arn.clone(), "On"),
                    (fleet.panels[1].controls[0].arn.clone(), "On"),
                ]
            },
        ),
        ("every control of one cluster, mixed states", |fleet| {
            fleet
                .controls_in_cluster(&fleet.panels[0].cluster_arn)
                .iter()
                .enumerate()
                .map(|(index, control)| {
                    (
                        control.arn.clone(),
                        if index % 2 == 0 { "Off" } else { "On" },
                    )
                })
                .collect()
        }),
        ("an empty batch", |_| vec![]),
    ];

    for (case, choose) in accepted {
        let router = emulator();
        let fleet = fleet(&router).await;
        let entries = choose(&fleet);
        let request = json!({
            "UpdateRoutingControlStateEntries": entries
                .iter()
                .map(|(arn, state)| json!({ "RoutingControlArn": arn, "RoutingControlState": state }))
                .collect::<Vec<Value>>(),
        });
        let (status, body) = call(&router, "UpdateRoutingControlStates", &request).await;
        assert_eq!(
            status, 200,
            "case {case}: expected 200, got {status} with {body}"
        );
        assert_eq!(
            body,
            json!({}),
            "case {case}: UpdateRoutingControlStates has an empty output shape, got {body}"
        );

        for (arn, state) in &entries {
            let read_back = get_state(&router, arn).await;
            assert_eq!(
                read_back.get("RoutingControlState").and_then(Value::as_str),
                Some(*state),
                "case {case}: every entry of the batch must be applied, got {read_back} for {arn}"
            );
        }
        // Controls the batch never named keep their state.
        for arn in fleet
            .arns()
            .into_iter()
            .filter(|arn| !entries.iter().any(|(named, _)| named == arn))
        {
            let read_back = get_state(&router, &arn).await;
            assert_eq!(
                read_back.get("RoutingControlState").and_then(Value::as_str),
                Some("Off"),
                "case {case}: a control the batch did not name must keep its state, got {read_back} for {arn}"
            );
        }
    }

    // Refused batches: (case, entries, expected status, expected error code).
    type Refused = fn(&Fleet) -> Vec<Value>;
    let refused: Vec<(&str, Refused, u16, &str)> = vec![
        (
            "one entry past the batch limit",
            |fleet| {
                let arn = fleet.arns()[0].clone();
                (0..=MAX_UPDATE_ROUTING_CONTROL_STATE_ENTRIES)
                    .map(|index| {
                        json!({
                            "RoutingControlArn": format!("{arn}{index}"),
                            "RoutingControlState": "On",
                        })
                    })
                    .collect()
            },
            402,
            "ServiceLimitExceededException",
        ),
        (
            "the same routing control twice",
            |fleet| {
                vec![
                    json!({ "RoutingControlArn": fleet.arns()[0], "RoutingControlState": "On" }),
                    json!({ "RoutingControlArn": fleet.arns()[0], "RoutingControlState": "Off" }),
                ]
            },
            400,
            "ValidationException",
        ),
        (
            "entries spanning two clusters",
            |fleet| {
                vec![
                    json!({
                        "RoutingControlArn": fleet.panels[0].controls[0].arn,
                        "RoutingControlState": "On",
                    }),
                    json!({
                        "RoutingControlArn": fleet.panels[2].controls[0].arn,
                        "RoutingControlState": "On",
                    }),
                ]
            },
            400,
            "ValidationException",
        ),
        (
            "an unknown routing control after a known one",
            |fleet| {
                vec![
                    json!({ "RoutingControlArn": fleet.arns()[0], "RoutingControlState": "On" }),
                    json!({
                        "RoutingControlArn": unknown_arn_like(&fleet.arns()[0]),
                        "RoutingControlState": "On",
                    }),
                ]
            },
            RESOURCE_NOT_FOUND_STATUS,
            "ResourceNotFoundException",
        ),
    ];

    for (case, choose, expected_status, expected_code) in refused {
        let router = emulator();
        let fleet = fleet(&router).await;
        let (status, body) = call(
            &router,
            "UpdateRoutingControlStates",
            &json!({ "UpdateRoutingControlStateEntries": choose(&fleet) }),
        )
        .await;
        assert_eq!(
            status, expected_status,
            "case {case}: expected {expected_status}, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case}: expected {expected_code}, got {body}"
        );

        // ATOMICITY: not one entry of a refused batch was committed.
        for arn in fleet.arns() {
            let read_back = get_state(&router, &arn).await;
            assert_eq!(
                read_back.get("RoutingControlState").and_then(Value::as_str),
                Some("Off"),
                "case {case}: a refused batch must leave no partial mutation, got {read_back} for {arn}"
            );
        }
    }
}

/// The states this service stores are exactly the values the model's
/// `RoutingControlState` enum declares (`ops::types::ROUTING_CONTROL_STATE_VALUES`),
/// and each round-trips through the stored form.
#[test]
fn every_modelled_state_value_round_trips_through_the_stored_form() {
    use service_com_amazonaws_route53recoverycluster_togglecustomerapi::state::RoutingControlState;

    for value in ROUTING_CONTROL_STATE_VALUES {
        let parsed = RoutingControlState::from_wire(value)
            .unwrap_or_else(|| panic!("case {value}: the modelled value must parse"));
        assert_eq!(
            parsed.as_wire(),
            *value,
            "case {value}: the stored state must round-trip to its wire value, got {}",
            parsed.as_wire()
        );
    }
    assert_eq!(
        ROUTING_CONTROL_STATE_VALUES.len(),
        2,
        "the model declares exactly On and Off, got {ROUTING_CONTROL_STATE_VALUES:?}"
    );
}
