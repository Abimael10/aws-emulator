// SPDX-License-Identifier: CC0-1.0

//! The `MaxEntries` cap of a managed prefix list, driven over the wire the service speaks:
//! `ec2Query`, an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: **a `ModifyManagedPrefixList` whose resulting entry set would not fit
//! the maximum number of entries in effect for the list is refused with its OWN error class,
//! `PrefixListMaxEntriesExceeded`, and changes nothing** — not the entry set, not the
//! version, not the maximum — while a modification that fits the maximum exactly is applied.
//! "The maximum number of entries for the prefix list"
//! (`ModifyManagedPrefixListRequest$MaxEntries`,
//! `smithy_query(query="operation",operation="ModifyManagedPrefixList")`) is the most the
//! list may hold, so the cap is reached from either side: by adding entries past it, or by
//! lowering it below the entries the list already holds.
//!
//! The refusal is a class of its own rather than the generic "the parameter cannot take this
//! value": it is the one cause a caller answers by RAISING the maximum and asking again, so a
//! client has to be able to tell it from a malformed CIDR block.
//!
//! Every row is a distinct input along what that rule generalises over: the maximum the list
//! carries, how many entries it holds, how many the request adds, how far past the maximum
//! the result would be, and which side of the comparison the request moves — plus the
//! maximum itself as a bound, exercised AT the cap (which succeeds) and one entry past it.

use std::time::Duration;

use ec2::state::{
    PREFIX_LIST_ADDRESS_FAMILY_IPV4, PREFIX_LIST_MAX_ENTRIES_EXCEEDED,
    PREFIX_LIST_STATE_CREATE_COMPLETE, TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message};
use super::wire::{child_text, document, param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-2";

/// The version a created list stands at, before any revision of its entry set
/// (`ec2::state::FIRST_PREFIX_LIST_VERSION`).
const FIRST_VERSION: i64 = ec2::state::FIRST_PREFIX_LIST_VERSION;

/// The settled state the clock owes a list whose revision has been applied — the
/// `PrefixListState` catalog's `modify-complete`
/// (`smithy_query(query="shape",shape="PrefixListState")`).
const MODIFY_COMPLETE: &str = "modify-complete";

/// One look inside the deadline below, so the deadline costs many looks rather than one.
fn poll_interval() -> Duration {
    Duration::from_millis(25)
}

/// The state `DescribeManagedPrefixLists` reports for `prefix_list_id` once it is `wanted`:
/// the state is looked at AGAIN in a loop until it has moved or a deadline far past
/// [`TRANSITION_DELAY`] passes, so the wait is for the CONDITION rather than for a span of
/// the clock and a loaded host cannot turn "not yet" into a failure.
async fn poll_until_state(
    router: &MultiServiceRouter,
    case: &str,
    prefix_list_id: &str,
    wanted: &str,
) -> String {
    let deadline = std::time::Instant::now() + TRANSITION_DELAY * 40;
    loop {
        let (reported, _, _) = described(router, case, prefix_list_id).await;
        let state = reported.unwrap_or_default();
        if state == wanted || std::time::Instant::now() >= deadline {
            break state;
        }
        tokio::time::sleep(poll_interval()).await;
    }
}

/// Every list of `lists` settled into `wanted` before the rows that read them run: the state
/// is polled for, so the wait is for the CONDITION and not for a span of the clock.
async fn settle_all(
    router: &MultiServiceRouter,
    lists: &[(&'static str, String)],
    wanted: &str,
) {
    for (name, prefix_list_id) in lists {
        let settled = poll_until_state(router, name, prefix_list_id, wanted).await;
        assert_eq!(
            settled, wanted,
            "case {name:?}: the list must have settled into {wanted} before the row runs, got \
             {settled:?}"
        );
    }
}

/// The sentence the cap refusal carries: fixed text, with the number of entries the
/// modification would leave stored and the maximum it would have to fit in.
fn expected_message(entries_after: usize, max_entries: i32) -> String {
    format!(
        "You've reached the maximum number of entries for the prefix list. This modification \
         saves ({entries_after}) entries and the maximum number of entries for this prefix \
         list is ({max_entries})."
    )
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// The `AddEntry.N.Cidr` form fields that name `cidrs` as the entries a modify adds.
fn add_params(cidrs: &[&str]) -> Vec<Param> {
    cidrs
        .iter()
        .enumerate()
        .map(|(index, cidr)| param(&format!("AddEntry.{}.Cidr", index + 1), cidr))
        .collect()
}

/// One prefix list of the account, created with `entries` and the maximum `max_entries`; its
/// id.
async fn create_list(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    max_entries: i32,
    entries: &[&str],
) -> String {
    let mut params = vec![
        param("PrefixListName", name),
        param("AddressFamily", PREFIX_LIST_ADDRESS_FAMILY_IPV4),
        param("MaxEntries", &max_entries.to_string()),
    ];
    for (index, cidr) in entries.iter().enumerate() {
        params.push(param(&format!("Entry.{}.Cidr", index + 1), cidr));
    }
    let (status, body) = call(router, "CreateManagedPrefixList", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create must succeed, got {status} with body {body}"
    );
    element(&body, "prefixListId")
        .unwrap_or_else(|| panic!("case {case:?}: the create reported no prefixListId: {body}"))
}

/// The state, the maximum and the version the list `prefix_list_id` is reported with, read
/// back through `DescribeManagedPrefixLists`.
async fn described(
    router: &MultiServiceRouter,
    case: &str,
    prefix_list_id: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let (status, body) = call(
        router,
        "DescribeManagedPrefixLists",
        &[param("PrefixListId.1", prefix_list_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe must succeed, got {status} with body {body}"
    );
    let parsed = document(case, &body);
    let reported = parsed
        .descendants()
        .find(|node| node.is_element() && child_text(*node, "prefixListId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no reported prefix list in {body}"));
    (
        child_text(reported, "state"),
        child_text(reported, "maxEntries"),
        child_text(reported, "version"),
    )
}

/// The CIDR blocks the list `prefix_list_id` reports, in the order it reported them.
async fn reported_blocks(
    router: &MultiServiceRouter,
    case: &str,
    prefix_list_id: &str,
) -> Vec<String> {
    let (status, body) = call(
        router,
        "GetManagedPrefixListEntries",
        &[param("PrefixListId", prefix_list_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the entry report must succeed, got {status} with body {body}"
    );
    let parsed = document(case, &body);
    super::wire::set_items(parsed.root_element(), "entrySet")
        .into_iter()
        .filter_map(|item| child_text(item, "cidr"))
        .collect()
}

/// One row of the refusal table.
struct CapCase {
    name: &'static str,
    /// The maximum the list is created with.
    max_entries: i32,
    /// The entries it is created with.
    held: &'static [&'static str],
    /// The blocks the refused request adds, if it adds any.
    added: &'static [&'static str],
    /// The maximum the refused request asks for, if it asks for one — the other side of the
    /// same comparison.
    asked_max: Option<i32>,
    /// How many entries the refused modification would have left stored, and the maximum it
    /// would have had to fit in: the two numbers its message names.
    expected_entries_after: usize,
    expected_max: i32,
}

/// The rows: entries added one past the cap, two past it, three past it, a list already at a
/// cap of one, and the same comparison reached from the other side by lowering the maximum
/// below the entries the list holds.
const CAP_CASES: &[CapCase] = &[
    CapCase {
        name: "one entry added to a list at a maximum of one",
        max_entries: 1,
        held: &["10.30.0.0/16"],
        added: &["10.31.0.0/16"],
        asked_max: None,
        expected_entries_after: 2,
        expected_max: 1,
    },
    CapCase {
        name: "one entry added to a list at a maximum of two",
        max_entries: 2,
        held: &["10.32.0.0/16", "10.33.0.0/16"],
        added: &["10.34.0.0/16"],
        asked_max: None,
        expected_entries_after: 3,
        expected_max: 2,
    },
    CapCase {
        name: "two entries added to a list one short of its maximum",
        max_entries: 3,
        held: &["10.35.0.0/16", "10.36.0.0/16"],
        added: &["10.37.0.0/16", "10.38.0.0/16"],
        asked_max: None,
        expected_entries_after: 4,
        expected_max: 3,
    },
    CapCase {
        name: "three entries added to an empty list with a maximum of two",
        max_entries: 2,
        held: &[],
        added: &["10.39.0.0/16", "10.40.0.0/16", "10.41.0.0/16"],
        asked_max: None,
        expected_entries_after: 3,
        expected_max: 2,
    },
    CapCase {
        name: "the maximum lowered below the entries the list holds",
        max_entries: 5,
        held: &["10.42.0.0/16", "10.43.0.0/16", "10.44.0.0/16"],
        added: &[],
        asked_max: Some(2),
        expected_entries_after: 3,
        expected_max: 2,
    },
];

/// A modification whose entry set would not fit the maximum in effect is refused with the
/// cap's own code and sentence, and changes nothing.
#[tokio::test]
async fn a_modification_past_the_maximum_is_refused_with_the_caps_own_code() {
    let (router, _engine) = super::ec2_query::emulator();

    // Every row's list first, then the creations the clock owes, waited for as the CONDITION
    // the describe reports: a list whose creation is still in progress is refused for its
    // state, which is a different rule.
    let mut lists = Vec::new();
    for (index, case) in CAP_CASES.iter().enumerate() {
        let prefix_list_id = create_list(
            &router,
            case.name,
            &format!("cap-case-{index}"),
            case.max_entries,
            case.held,
        )
        .await;
        lists.push((case.name, prefix_list_id));
    }
    settle_all(&router, &lists, PREFIX_LIST_STATE_CREATE_COMPLETE).await;

    for ((name, prefix_list_id), case) in lists.iter().zip(CAP_CASES) {
        let name = *name;
        let mut params = vec![param("PrefixListId", prefix_list_id)];
        params.extend(add_params(case.added));
        if let Some(asked_max) = case.asked_max {
            params.push(param("MaxEntries", &asked_max.to_string()));
        }
        let (status, body) = call(&router, "ModifyManagedPrefixList", &params).await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected the over-cap modification refused with HTTP 400, got \
             {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            PREFIX_LIST_MAX_ENTRIES_EXCEEDED,
            "case {name:?}: expected {PREFIX_LIST_MAX_ENTRIES_EXCEEDED}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_message(case.expected_entries_after, case.expected_max),
            "case {name:?}: expected the sentence naming the entries the modification would \
             have saved and the maximum it had to fit in, got {body}"
        );

        // Nothing moved: the list still holds the entries it was created with, still stands
        // at its first version, and still carries the maximum it was created with.
        let blocks = reported_blocks(&router, name, prefix_list_id).await;
        let mut expected_blocks: Vec<String> =
            case.held.iter().map(|cidr| (*cidr).to_string()).collect();
        expected_blocks.sort();
        let mut reported = blocks.clone();
        reported.sort();
        assert_eq!(
            reported, expected_blocks,
            "case {name:?}: a refused modification must not touch the entries, got {blocks:?}"
        );
        let (_, reported_max, reported_version) = described(&router, name, prefix_list_id).await;
        assert_eq!(
            reported_max.as_deref(),
            Some(case.max_entries.to_string().as_str()),
            "case {name:?}: a refused modification must not resize the list, got \
             {reported_max:?}"
        );
        assert_eq!(
            reported_version.as_deref(),
            Some(FIRST_VERSION.to_string().as_str()),
            "case {name:?}: a refused modification must not move the version, got \
             {reported_version:?}"
        );
    }
}

/// One row of the bound table: the maximum the list is held to, the entries it is created
/// with, the blocks one call adds to fill it to that maximum exactly, and the one block past
/// it.
struct BoundCase {
    name: &'static str,
    max_entries: i32,
    created: &'static [&'static str],
    fill: &'static [&'static str],
    past: &'static str,
}

/// The rows: a list filled to its maximum one entry at a time, one filled by a single call
/// adding two, and one created already at a maximum of one — so the bound asserted is each
/// list's own.
const BOUND_CASES: &[BoundCase] = &[
    BoundCase {
        name: "a maximum of two, filled one entry at a time",
        max_entries: 2,
        created: &["10.50.0.0/16"],
        fill: &["10.51.0.0/16"],
        past: "10.52.0.0/16",
    },
    BoundCase {
        name: "a maximum of three, filled by one call adding two",
        max_entries: 3,
        created: &["10.53.0.0/16"],
        fill: &["10.54.0.0/16", "10.55.0.0/16"],
        past: "10.56.0.0/16",
    },
    BoundCase {
        name: "a maximum of one, created already full",
        max_entries: 1,
        created: &["10.57.0.0/16"],
        fill: &[],
        past: "10.58.0.0/16",
    },
];

/// The maximum is a BOUND: a modification that fills the list to it exactly is applied, and
/// the next entry past it is refused with the cap's own code — the entries it would have
/// saved counted from the entry set the list holds by then.
///
/// The rows differ in the maximum they are held to, so the bound asserted is the list's own
/// rather than one number.
#[tokio::test]
async fn a_list_fills_to_its_maximum_and_refuses_the_entry_past_it() {
    let (router, _engine) = super::ec2_query::emulator();

    let mut lists = Vec::new();
    for (index, case) in BOUND_CASES.iter().enumerate() {
        let prefix_list_id = create_list(
            &router,
            case.name,
            &format!("bound-case-{index}"),
            case.max_entries,
            case.created,
        )
        .await;
        lists.push((case.name, prefix_list_id));
    }
    settle_all(&router, &lists, PREFIX_LIST_STATE_CREATE_COMPLETE).await;

    // The call that fills each list to its maximum exactly: the bound itself, which is
    // applied rather than refused.
    let mut revised: Vec<(&'static str, String)> = Vec::new();
    for ((name, prefix_list_id), case) in lists.iter().zip(BOUND_CASES) {
        let name = *name;
        if case.fill.is_empty() {
            continue;
        }
        let mut params = vec![param("PrefixListId", prefix_list_id)];
        params.extend(add_params(case.fill));
        let (status, body) = call(&router, "ModifyManagedPrefixList", &params).await;
        assert_eq!(
            status, 200,
            "case {name:?}: a modification that fills the list to its maximum of {max} must \
             be applied, got {status} with body {body}",
            max = case.max_entries
        );
        revised.push((name, prefix_list_id.clone()));
    }
    // The revisions the clock owes, waited for as the CONDITION the describe reports, so the
    // call past the bound is not refused for a state the list has already left — and the
    // entry set a revision mints is the entry set of the version it settles into, so the
    // entries are read back once the list has settled.
    settle_all(&router, &revised, MODIFY_COMPLETE).await;

    for ((name, prefix_list_id), case) in lists.iter().zip(BOUND_CASES) {
        let name = *name;
        if case.fill.is_empty() {
            continue;
        }
        let blocks = reported_blocks(&router, name, prefix_list_id).await;
        assert_eq!(
            blocks.len(),
            case.created.len() + case.fill.len(),
            "case {name:?}: expected the list filled to {max} entries, got {blocks:?}",
            max = case.max_entries
        );
    }

    for ((name, prefix_list_id), case) in lists.iter().zip(BOUND_CASES) {
        let name = *name;
        let held = case.created.len() + case.fill.len();
        let (status, body) = call(
            &router,
            "ModifyManagedPrefixList",
            &[
                param("PrefixListId", prefix_list_id),
                param("AddEntry.1.Cidr", case.past),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected the entry past the maximum refused, got {status} with \
             body {body}"
        );
        assert_eq!(
            error_code(&body),
            PREFIX_LIST_MAX_ENTRIES_EXCEEDED,
            "case {name:?}: expected {PREFIX_LIST_MAX_ENTRIES_EXCEEDED}, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_message(held + 1, case.max_entries),
            "case {name:?}: expected the sentence to count the entries the modification \
             would have saved, got {body}"
        );
        let blocks = reported_blocks(&router, name, prefix_list_id).await;
        assert!(
            !blocks.contains(&case.past.to_string()),
            "case {name:?}: the refused entry must not be stored, got {blocks:?}"
        );
        assert_eq!(
            blocks.len(),
            held,
            "case {name:?}: expected the list to still hold {held} entries, got {blocks:?}"
        );
    }
}
