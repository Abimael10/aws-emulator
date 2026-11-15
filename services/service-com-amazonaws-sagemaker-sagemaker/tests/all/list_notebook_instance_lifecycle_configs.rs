// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for SageMaker's `ListNotebookInstanceLifecycleConfigs`.
//!
//! Every case goes over the wire (`POST /` with
//! `X-Amz-Target: SageMaker.ListNotebookInstanceLifecycleConfigs`, `awsJson1_1`) through
//! the same `MultiServiceRouter` the production router uses, so what these tests exercise
//! is what a real SDK client gets. The harness (`Emulator`, `error_code`, `str_field`) is
//! the crate's one wire harness, shared from `inference_deployment_lifecycle`.
//!
//! The tests assert RULES over a TABLE of inputs rather than one recorded call: every
//! member of the modelled `NotebookInstanceLifecycleConfigSortKey` catalog crossed with
//! every member of `NotebookInstanceLifecycleConfigSortOrder`, each filter member and
//! each of its bounds, every page size from one entry to past the published `MaxResults`
//! maximum, and the summary each entry carries compared against what
//! `DescribeNotebookInstanceLifecycleConfig` replays.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::ops::types::{
    NOTEBOOK_INSTANCE_LIFECYCLE_CONFIG_SORT_KEY_VALUES,
    NOTEBOOK_INSTANCE_LIFECYCLE_CONFIG_SORT_ORDER_VALUES,
};
use service_com_amazonaws_sagemaker_sagemaker::state::{self, MAX_PAGE_SIZE};

use super::inference_deployment_lifecycle::{error_code, str_field, Emulator, ACCOUNT};

/// The ARN resource type SageMaker gives a notebook instance lifecycle configuration
/// (AWS ARN reference: `notebook-instance-lifecycle-config/<name>`).
const ARN_KIND: &str = "notebook-instance-lifecycle-config";

/// The service's own server-side page ceiling, read from the service rather than
/// restated, so changing it does not fail these tests for an unrelated reason.
const PAGE_CEILING: usize = MAX_PAGE_SIZE as usize;

/// The configurations the sort and filter tables range over: the name, the offset (in
/// whole seconds from the suite's base instant) of its `CreationTime`, and the offset of
/// its `LastModifiedTime`.
///
/// The three orders the rows produce are deliberately all different — names are not in
/// creation order, and last-modified order is the reverse of creation order — so a sort
/// on one key cannot accidentally agree with a sort on another.
const SEEDED: [(&str, u64, u64); 4] = [
    ("alpha-one", 0, 40),
    ("beta-two", 10, 30),
    ("demo-three", 20, 20),
    ("demo-four", 30, 10),
];

/// One seeded configuration as the store holds it: its name and the two instants a
/// `DescribeNotebookInstanceLifecycleConfig` reports, in epoch seconds off the wire.
struct Seeded {
    name: String,
    created_at: f64,
    modified_at: f64,
}

/// An instant whose sub-second part is zero, so every timestamp these tests compare
/// against — stored, listed and sent as a filter bound — is exactly representable both as
/// the modelled `Timestamp` and as the JSON number the wire carries.
fn whole_second_base() -> SystemTime {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("the host clock is at or after the epoch")
        .as_secs();
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

/// Create [`SEEDED`] over the wire, then set each configuration's stored instants to its
/// row's offsets, and answer what `DescribeNotebookInstanceLifecycleConfig` replays for
/// each one, in creation order.
///
/// The instants are written straight into the ONE shared store the handlers reach through
/// `state::service_state`, so the time ordering is exact and the suite never waits on a
/// clock to separate two creates.
async fn seed_configs(emulator: &Emulator) -> Vec<Seeded> {
    let base = whole_second_base();
    for (name, _, _) in SEEDED {
        emulator
            .ok(
                "CreateNotebookInstanceLifecycleConfig",
                &json!({
                    "NotebookInstanceLifecycleConfigName": name,
                    "OnStart": [{ "Content": "ZWNobyBzdGFydA==" }],
                }),
            )
            .await;
    }

    {
        let store = state::service_state(&*emulator.engine)
            .expect("the service is mounted on the shared in-memory engine");
        let mut guard = store.lock();
        for (name, created, modified) in SEEDED {
            let config = guard
                .notebook_instance_lifecycle_configs
                .get_mut(name)
                .unwrap_or_else(|| panic!("the create stored a configuration named {name}"));
            config.creation_time = base + Duration::from_secs(created);
            config.last_modified_time = base + Duration::from_secs(modified);
        }
    }

    let mut seeded = Vec::new();
    for (name, _, _) in SEEDED {
        let described = emulator
            .ok(
                "DescribeNotebookInstanceLifecycleConfig",
                &json!({ "NotebookInstanceLifecycleConfigName": name }),
            )
            .await;
        let number = |member: &str| {
            described
                .get(member)
                .and_then(Value::as_f64)
                .unwrap_or_else(|| panic!("expected a numeric {member} in {described}"))
        };
        seeded.push(Seeded {
            name: name.to_string(),
            created_at: number("CreationTime"),
            modified_at: number("LastModifiedTime"),
        });
    }
    seeded
}

/// The `NotebookInstanceLifecycleConfigName`s a response lists, in response order.
fn listed_names(body: &Value) -> Vec<String> {
    body.get("NotebookInstanceLifecycleConfigs")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("expected a NotebookInstanceLifecycleConfigs array in {body}")
        })
        .iter()
        .map(|summary| str_field(summary, "NotebookInstanceLifecycleConfigName"))
        .collect()
}

/// RULE: the listing reports the stored lifecycle configurations ordered by
/// `SortBy`/`SortOrder` — defaulting to `CreationTime` as the member doc states, and to
/// the service's `Descending` order, which this listing's `SortOrder` doc leaves
/// unstated — with the configuration name as the tie-break.
#[tokio::test]
async fn honours_every_documented_sort() {
    let emulator = Emulator::new();
    seed_configs(&emulator).await;

    let by_name_ascending = vec!["alpha-one", "beta-two", "demo-four", "demo-three"];
    let by_name_descending = vec!["demo-three", "demo-four", "beta-two", "alpha-one"];
    let by_created_ascending = vec!["alpha-one", "beta-two", "demo-three", "demo-four"];
    let by_created_descending = vec!["demo-four", "demo-three", "beta-two", "alpha-one"];
    // Last-modified order is the reverse of creation order for these rows.
    let by_modified_ascending = by_created_descending.clone();
    let by_modified_descending = by_created_ascending.clone();

    // Rows: the documented defaults, then EVERY member of the modelled
    // `NotebookInstanceLifecycleConfigSortKey` catalog crossed with every member of
    // `NotebookInstanceLifecycleConfigSortOrder` (both read from the generated value
    // slices, not hand-listed here).
    let mut cases: Vec<(String, Value, Vec<&str>)> = vec![(
        "documented-defaults".to_string(),
        json!({}),
        by_created_descending.clone(),
    )];
    for sort_by in NOTEBOOK_INSTANCE_LIFECYCLE_CONFIG_SORT_KEY_VALUES {
        for sort_order in NOTEBOOK_INSTANCE_LIFECYCLE_CONFIG_SORT_ORDER_VALUES {
            let expected = match (*sort_by, *sort_order) {
                ("Name", "Ascending") => by_name_ascending.clone(),
                ("Name", _) => by_name_descending.clone(),
                ("LastModifiedTime", "Ascending") => by_modified_ascending.clone(),
                ("LastModifiedTime", _) => by_modified_descending.clone(),
                (_, "Ascending") => by_created_ascending.clone(),
                (_, _) => by_created_descending.clone(),
            };
            cases.push((
                format!("{sort_by}-{sort_order}"),
                json!({ "SortBy": sort_by, "SortOrder": sort_order }),
                expected,
            ));
        }
    }
    // A sort order on its own takes the documented default sort field, and vice versa.
    cases.push((
        "sort-order-only".to_string(),
        json!({ "SortOrder": "Ascending" }),
        by_created_ascending.clone(),
    ));
    cases.push((
        "sort-by-only".to_string(),
        json!({ "SortBy": "Name" }),
        by_name_descending.clone(),
    ));

    for (case, body, expected) in cases {
        let listed = emulator
            .ok("ListNotebookInstanceLifecycleConfigs", &body)
            .await;
        assert_eq!(
            listed_names(&listed),
            expected,
            "case {case}: listing order, got {listed:?}"
        );
        assert!(
            listed.get("NextToken").is_none(),
            "case {case}: a complete page must carry no NextToken, got {listed:?}"
        );
    }
}

/// RULE: `SortOrder` reverses the sort key only — the configuration name stays the
/// ascending tie-break — so configurations sharing a sort key keep one order whichever way
/// the page is sorted.
#[tokio::test]
async fn equal_sort_keys_keep_one_order_in_both_directions() {
    let emulator = Emulator::new();
    // Rows differ only in name; every configuration shares ONE creation and
    // last-modified instant, so the sort key never decides the order.
    let names = ["c-third", "a-first", "b-second"];
    for name in names {
        emulator
            .ok(
                "CreateNotebookInstanceLifecycleConfig",
                &json!({ "NotebookInstanceLifecycleConfigName": name }),
            )
            .await;
    }
    let shared = whole_second_base();
    {
        let store = state::service_state(&*emulator.engine)
            .expect("the service is mounted on the shared in-memory engine");
        let mut guard = store.lock();
        for name in names {
            let config = guard
                .notebook_instance_lifecycle_configs
                .get_mut(name)
                .unwrap_or_else(|| panic!("the create stored a configuration named {name}"));
            config.creation_time = shared;
            config.last_modified_time = shared;
        }
    }

    for sort_by in ["CreationTime", "LastModifiedTime"] {
        for sort_order in NOTEBOOK_INSTANCE_LIFECYCLE_CONFIG_SORT_ORDER_VALUES {
            let case = format!("{sort_by}-{sort_order}");
            let listed = emulator
                .ok(
                    "ListNotebookInstanceLifecycleConfigs",
                    &json!({ "SortBy": sort_by, "SortOrder": sort_order }),
                )
                .await;
            assert_eq!(
                listed_names(&listed),
                vec!["a-first", "b-second", "c-third"],
                "case {case}: equal sort keys must keep the name tie-break ascending, \
                 got {listed:?}"
            );
        }
    }
}

/// RULE: `NameContains` selects exactly the configurations whose name contains the
/// substring, and nothing else about the listing changes.
#[tokio::test]
async fn filters_on_name_contains() {
    let emulator = Emulator::new();
    seed_configs(&emulator).await;

    // Rows differ along the filter's selectivity and the shape of the substring: a
    // prefix, an infix, a whole name, a hyphen, one nothing matches and one differing
    // only in case. The modelled `NotebookInstanceLifecycleConfigNameContains` is
    // len 0..63, pattern ^[a-zA-Z0-9-]+$ (smithy_query
    // operation=ListNotebookInstanceLifecycleConfigs).
    let cases: Vec<(&str, &str, Vec<&str>)> = vec![
        ("prefix-two-matches", "demo", vec!["demo-four", "demo-three"]),
        ("prefix-one-match", "alpha", vec!["alpha-one"]),
        ("infix", "ta-tw", vec!["beta-two"]),
        ("whole-name", "demo-three", vec!["demo-three"]),
        (
            "hyphen-matches-every-name",
            "-",
            vec!["alpha-one", "beta-two", "demo-four", "demo-three"],
        ),
        ("no-match", "nothing-here", vec![]),
        ("case-sensitive-no-match", "DEMO", vec![]),
    ];

    for (case, needle, expected) in cases {
        let listed = emulator
            .ok(
                "ListNotebookInstanceLifecycleConfigs",
                &json!({
                    "NameContains": needle,
                    "SortBy": "Name",
                    "SortOrder": "Ascending",
                }),
            )
            .await;
        assert_eq!(
            listed_names(&listed),
            expected,
            "case {case}: NameContains={needle} selection, got {listed:?}"
        );
    }
}

/// RULE: each of the four time bounds selects the configurations on its side of the
/// bound, the bound itself included, so a bound landing exactly on a stored instant still
/// selects the configuration carrying it; the bounds combine with each other and with
/// `NameContains`.
#[tokio::test]
async fn filters_on_the_time_bounds() {
    let emulator = Emulator::new();
    let seeded = seed_configs(&emulator).await;
    // A bound strictly between the second and third configuration, so the split is
    // unambiguous, plus bounds one second to either side of a stored instant and bounds
    // exactly on one.
    let split = (seeded[1].created_at + seeded[2].created_at) / 2.0;
    let second_created = seeded[1].created_at;
    let second_modified = seeded[1].modified_at;
    let one_second = 1.0;

    let by_created = |extra: Value| {
        let mut body = json!({ "SortBy": "CreationTime", "SortOrder": "Ascending" });
        for (key, value) in extra.as_object().expect("a JSON object of extra members") {
            body[key] = value.clone();
        }
        body
    };

    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "creation-time-before-the-split",
            by_created(json!({ "CreationTimeBefore": split })),
            vec!["alpha-one", "beta-two"],
        ),
        (
            "creation-time-after-the-split",
            by_created(json!({ "CreationTimeAfter": split })),
            vec!["demo-three", "demo-four"],
        ),
        (
            "creation-time-after-exactly-a-stored-instant",
            by_created(json!({ "CreationTimeAfter": second_created })),
            vec!["beta-two", "demo-three", "demo-four"],
        ),
        (
            "creation-time-before-exactly-a-stored-instant",
            by_created(json!({ "CreationTimeBefore": second_created })),
            vec!["alpha-one", "beta-two"],
        ),
        (
            "creation-time-after-one-second-past-a-stored-instant",
            by_created(json!({ "CreationTimeAfter": second_created + one_second })),
            vec!["demo-three", "demo-four"],
        ),
        (
            "creation-time-before-one-second-short-of-a-stored-instant",
            by_created(json!({ "CreationTimeBefore": second_created - one_second })),
            vec!["alpha-one"],
        ),
        (
            "last-modified-time-before-exactly-a-stored-instant",
            by_created(json!({ "LastModifiedTimeBefore": second_modified })),
            vec!["beta-two", "demo-three", "demo-four"],
        ),
        (
            "last-modified-time-after-exactly-a-stored-instant",
            by_created(json!({ "LastModifiedTimeAfter": second_modified })),
            vec!["alpha-one", "beta-two"],
        ),
        (
            "both-creation-bounds-around-one-configuration",
            by_created(json!({
                "CreationTimeAfter": split,
                "CreationTimeBefore": (seeded[2].created_at + seeded[3].created_at) / 2.0,
            })),
            vec!["demo-three"],
        ),
        (
            "a-creation-bound-and-a-last-modified-bound",
            by_created(json!({
                "CreationTimeAfter": split,
                "LastModifiedTimeBefore": (seeded[2].modified_at + seeded[3].modified_at) / 2.0,
            })),
            vec!["demo-four"],
        ),
        (
            "bounds-that-exclude-everything",
            by_created(json!({ "CreationTimeAfter": seeded[3].created_at + 3600.0 })),
            vec![],
        ),
        (
            "a-bound-combined-with-a-name-filter",
            json!({
                "CreationTimeAfter": split,
                "NameContains": "demo",
                "SortBy": "Name",
                "SortOrder": "Ascending",
            }),
            vec!["demo-four", "demo-three"],
        ),
    ];

    for (case, body, expected) in cases {
        let listed = emulator
            .ok("ListNotebookInstanceLifecycleConfigs", &body)
            .await;
        assert_eq!(
            listed_names(&listed),
            expected,
            "case {case}: selection, got {listed:?}"
        );
    }
}

/// RULE: every listed summary carries exactly what the store holds for that configuration
/// — the name, the minted ARN and both stored instants — the same values
/// `DescribeNotebookInstanceLifecycleConfig` replays, and listing changes nothing a later
/// caller sees.
#[tokio::test]
async fn every_summary_replays_the_stored_configuration() {
    let emulator = Emulator::new();
    let seeded = seed_configs(&emulator).await;

    let listed = emulator
        .ok(
            "ListNotebookInstanceLifecycleConfigs",
            &json!({ "SortBy": "CreationTime", "SortOrder": "Ascending" }),
        )
        .await;
    let summaries = listed
        .get("NotebookInstanceLifecycleConfigs")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected a NotebookInstanceLifecycleConfigs array in {listed}"));
    assert_eq!(
        summaries.len(),
        SEEDED.len(),
        "every stored configuration must be listed, got {listed:?}"
    );

    for (index, config) in seeded.iter().enumerate() {
        let summary = &summaries[index];
        let name = &config.name;
        assert_eq!(
            str_field(summary, "NotebookInstanceLifecycleConfigName"),
            name.as_str(),
            "case {name}: NotebookInstanceLifecycleConfigName, got {summary:?}"
        );
        assert_eq!(
            str_field(summary, "NotebookInstanceLifecycleConfigArn"),
            format!("arn:aws:sagemaker:us-east-1:{ACCOUNT}:{ARN_KIND}/{name}"),
            "case {name}: NotebookInstanceLifecycleConfigArn, got {summary:?}"
        );
        assert_eq!(
            summary.get("CreationTime").and_then(Value::as_f64),
            Some(config.created_at),
            "case {name}: CreationTime in the summary must be the stored one, got {summary:?}"
        );
        assert_eq!(
            summary.get("LastModifiedTime").and_then(Value::as_f64),
            Some(config.modified_at),
            "case {name}: LastModifiedTime in the summary must be the stored one, got {summary:?}"
        );
    }

    // A listing is a read: it changes nothing a later caller sees.
    let again = emulator
        .ok(
            "ListNotebookInstanceLifecycleConfigs",
            &json!({ "SortBy": "CreationTime", "SortOrder": "Ascending" }),
        )
        .await;
    assert_eq!(
        again, listed,
        "listing twice must report the same configurations, got {again:?}"
    );
    for config in &seeded {
        let name = &config.name;
        let described = emulator
            .ok(
                "DescribeNotebookInstanceLifecycleConfig",
                &json!({ "NotebookInstanceLifecycleConfigName": name }),
            )
            .await;
        assert_eq!(
            described.get("CreationTime").and_then(Value::as_f64),
            Some(config.created_at),
            "case {name}: a listing must not change the stored CreationTime, got {described:?}"
        );
        assert_eq!(
            described.get("LastModifiedTime").and_then(Value::as_f64),
            Some(config.modified_at),
            "case {name}: a listing must not change the stored LastModifiedTime, got {described:?}"
        );
        assert_eq!(
            str_field(&described, "NotebookInstanceLifecycleConfigArn"),
            format!("arn:aws:sagemaker:us-east-1:{ACCOUNT}:{ARN_KIND}/{name}"),
            "case {name}: the described ARN must be the listed one, got {described:?}"
        );
    }
}

/// RULE: the listing reflects the store as it is at the call, so a configuration appears
/// once it is created, reports the instant an update recorded, and is gone after a delete.
#[tokio::test]
async fn the_listing_reflects_every_create_update_and_delete() {
    let emulator = Emulator::new();
    let empty = emulator
        .ok("ListNotebookInstanceLifecycleConfigs", &json!({}))
        .await;
    assert_eq!(
        listed_names(&empty),
        Vec::<String>::new(),
        "case empty-store: an empty store lists nothing, got {empty:?}"
    );
    assert!(
        empty.get("NextToken").is_none(),
        "case empty-store: no NextToken on an empty result, got {empty:?}"
    );

    // Rows differ in count: the expected listing grows by one with every create.
    let mut expected: Vec<String> = Vec::new();
    for (name, _, _) in SEEDED {
        emulator
            .ok(
                "CreateNotebookInstanceLifecycleConfig",
                &json!({ "NotebookInstanceLifecycleConfigName": name }),
            )
            .await;
        expected.push(name.to_string());
        expected.sort();
        let listed = emulator
            .ok(
                "ListNotebookInstanceLifecycleConfigs",
                &json!({ "SortBy": "Name", "SortOrder": "Ascending" }),
            )
            .await;
        assert_eq!(
            listed_names(&listed),
            expected,
            "case after-creating-{name}: the listing must reflect the create, got {listed:?}"
        );
    }

    // An update rewrites the scripts and moves `LastModifiedTime`; the listing reports
    // the instant the store now holds for that configuration.
    let updated = "beta-two";
    emulator
        .ok(
            "UpdateNotebookInstanceLifecycleConfig",
            &json!({
                "NotebookInstanceLifecycleConfigName": updated,
                "OnStart": [{ "Content": "ZWNobyB1cGRhdGVk" }],
            }),
        )
        .await;
    let described = emulator
        .ok(
            "DescribeNotebookInstanceLifecycleConfig",
            &json!({ "NotebookInstanceLifecycleConfigName": updated }),
        )
        .await;
    let listed = emulator
        .ok(
            "ListNotebookInstanceLifecycleConfigs",
            &json!({ "NameContains": updated }),
        )
        .await;
    let summary = listed
        .get("NotebookInstanceLifecycleConfigs")
        .and_then(Value::as_array)
        .and_then(|summaries| summaries.first())
        .unwrap_or_else(|| panic!("case after-update: expected one summary in {listed}"))
        .clone();
    assert_eq!(
        summary.get("LastModifiedTime"),
        described.get("LastModifiedTime"),
        "case after-update: the listed LastModifiedTime must be the stored one, \
         got {summary:?} against {described:?}"
    );
    assert_eq!(
        summary.get("CreationTime"),
        described.get("CreationTime"),
        "case after-update: the listed CreationTime must be the stored one, \
         got {summary:?} against {described:?}"
    );

    // Rows differ in count again: the expected listing shrinks by one with every delete.
    for (name, _, _) in SEEDED {
        emulator
            .ok(
                "DeleteNotebookInstanceLifecycleConfig",
                &json!({ "NotebookInstanceLifecycleConfigName": name }),
            )
            .await;
        expected.retain(|stored| stored.as_str() != name);
        let listed = emulator
            .ok(
                "ListNotebookInstanceLifecycleConfigs",
                &json!({ "SortBy": "Name", "SortOrder": "Ascending" }),
            )
            .await;
        assert_eq!(
            listed_names(&listed),
            expected,
            "case after-deleting-{name}: the listing must reflect the delete, got {listed:?}"
        );
    }
}

/// RULE: paging with `MaxResults` walks the whole ordered result exactly once — each page
/// holds at most `MaxResults` entries, a `NextToken` is present only while more remain (so
/// a page that exactly exhausts the result carries none), and feeding the token back
/// resumes immediately after the previous page.
#[tokio::test]
async fn paging_with_max_results_walks_the_whole_result_exactly_once() {
    let emulator = Emulator::new();
    seed_configs(&emulator).await;
    let ordered = vec!["alpha-one", "beta-two", "demo-four", "demo-three"];

    // Rows: every page size from one entry up to past the end of the result set,
    // including the page size that exactly fits it (4) and the published maximum of
    // `MaxResults` (range 1..100 — smithy_query
    // operation=ListNotebookInstanceLifecycleConfigs).
    for page_size in [1_i64, 2, 3, 4, 5, 100] {
        let case = format!("MaxResults={page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;

        loop {
            let mut body = json!({
                "SortBy": "Name",
                "SortOrder": "Ascending",
                "MaxResults": page_size,
            });
            if let Some(ref value) = token {
                body["NextToken"] = Value::String(value.clone());
            }
            let listed = emulator
                .ok("ListNotebookInstanceLifecycleConfigs", &body)
                .await;
            let names = listed_names(&listed);
            assert!(
                names.len() as i64 <= page_size,
                "case {case}: a page must hold at most {page_size} entries, got {names:?}"
            );
            seen.extend(names);
            pages += 1;
            assert!(
                pages <= ordered.len() + 1,
                "case {case}: paging did not terminate, saw {seen:?}"
            );
            match listed.get("NextToken").and_then(Value::as_str) {
                Some(next) => {
                    assert!(
                        seen.len() < ordered.len(),
                        "case {case}: a page that exhausted the result must carry no \
                         NextToken, saw {seen:?}"
                    );
                    token = Some(next.to_string());
                }
                None => break,
            }
        }

        assert_eq!(
            seen, ordered,
            "case {case}: paging must yield the whole ordered result exactly once"
        );
        assert_eq!(
            pages,
            ordered.len().div_ceil(page_size as usize),
            "case {case}: page count, saw {seen:?}"
        );
    }

    // The same walk under a time sort, so the resume boundary is not tied to the name
    // sort it happens to share a tie-break with.
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut body = json!({
            "SortBy": "LastModifiedTime",
            "SortOrder": "Descending",
            "MaxResults": 2,
        });
        if let Some(ref value) = token {
            body["NextToken"] = Value::String(value.clone());
        }
        let listed = emulator
            .ok("ListNotebookInstanceLifecycleConfigs", &body)
            .await;
        seen.extend(listed_names(&listed));
        assert!(
            seen.len() <= ordered.len(),
            "case time-sorted-walk: paging did not terminate, saw {seen:?}"
        );
        match listed.get("NextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(
        seen,
        vec!["alpha-one", "beta-two", "demo-three", "demo-four"],
        "case time-sorted-walk: the pages must cover the whole ordered result once"
    );
}

/// RULE: the page is capped server-side, so no single call serialises the whole store
/// however large `MaxResults` is or whether the caller sends it at all — and the resume
/// token still walks the full set.
#[tokio::test]
async fn the_page_is_capped_server_side_even_when_the_caller_asks_for_more() {
    // The store is seeded past the service's own ceiling, so a capped page and an
    // uncapped one are distinguishable.
    let stored = PAGE_CEILING + 7;

    let emulator = Emulator::new();
    for index in 0..stored {
        emulator
            .ok(
                "CreateNotebookInstanceLifecycleConfig",
                &json!({ "NotebookInstanceLifecycleConfigName": format!("c{index:03}") }),
            )
            .await;
    }

    // Rows: no `MaxResults` at all, the modelled maximum (above the ceiling), and a value
    // below the ceiling, which still wins because it is the smaller cap.
    let cases: Vec<(&str, Value, usize)> = vec![
        ("max-results-absent", json!({}), PAGE_CEILING),
        (
            "max-results-at-the-modelled-maximum",
            json!({ "MaxResults": 100 }),
            PAGE_CEILING,
        ),
        (
            "max-results-below-the-ceiling",
            json!({ "MaxResults": 10 }),
            10,
        ),
    ];

    for (case, extra, expected_page) in cases {
        let mut body = json!({ "SortBy": "Name", "SortOrder": "Ascending" });
        for (key, value) in extra.as_object().expect("a JSON object of extra members") {
            body[key] = value.clone();
        }
        let listed = emulator
            .ok("ListNotebookInstanceLifecycleConfigs", &body)
            .await;
        assert_eq!(
            listed_names(&listed).len(),
            expected_page,
            "case {case}: page size, got {listed:?}"
        );
        assert!(
            listed.get("NextToken").is_some(),
            "case {case}: more configurations remain, so a NextToken is required, \
             got {listed:?}"
        );
    }

    // And the capped pages still walk the whole store exactly once.
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut body = json!({ "SortBy": "Name", "SortOrder": "Ascending" });
        if let Some(ref value) = token {
            body["NextToken"] = Value::String(value.clone());
        }
        let listed = emulator
            .ok("ListNotebookInstanceLifecycleConfigs", &body)
            .await;
        let names = listed_names(&listed);
        assert!(
            names.len() <= PAGE_CEILING,
            "case capped-walk: a page must never exceed the ceiling, got {} entries",
            names.len()
        );
        seen.extend(names);
        assert!(
            seen.len() <= stored,
            "case capped-walk: paging did not terminate, saw {} entries",
            seen.len()
        );
        match listed.get("NextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    let expected: Vec<String> = (0..stored).map(|index| format!("c{index:03}")).collect();
    assert_eq!(
        seen, expected,
        "case capped-walk: the capped pages must cover the whole store exactly once"
    );
}

/// RULE: a request this operation cannot serve is refused with HTTP 400
/// `ValidationException` — a `SortBy` or `SortOrder` outside the modelled enum, a
/// `NextToken` that names no boundary of the result set, and anything past a member's
/// modelled constraint — never a 200 and never a 501.
#[tokio::test]
async fn requests_this_listing_cannot_serve_are_refused() {
    let emulator = Emulator::new();
    seed_configs(&emulator).await;

    // Rows: a value outside each modelled enum (including one that is a valid member of
    // another listing's enum, and ones differing only in case), a token the service did
    // not mint, and a token that is well-formed hex but names no listed configuration.
    let hex_of_absent_name = "61626f7665"; // "above", a name nothing created
    let refused_by_the_handler: Vec<(&str, Value)> = vec![
        ("sort-by-not-in-the-enum", json!({ "SortBy": "Status" })),
        ("sort-by-wrong-case", json!({ "SortBy": "name" })),
        ("sort-by-empty", json!({ "SortBy": "" })),
        (
            "sort-order-not-in-the-enum",
            json!({ "SortOrder": "Sideways" }),
        ),
        ("sort-order-wrong-case", json!({ "SortOrder": "ascending" })),
        (
            "next-token-not-minted-here",
            json!({ "NextToken": "not-a-token" }),
        ),
        (
            "next-token-naming-no-listed-configuration",
            json!({ "NextToken": hex_of_absent_name }),
        ),
    ];

    for (case, body) in refused_by_the_handler {
        let (status, parsed) = emulator
            .call("ListNotebookInstanceLifecycleConfigs", &body)
            .await;
        assert_eq!(
            status, 400,
            "case {case}: the request must be refused, got {status} {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case}: error code, got {parsed:?}"
        );
    }

    // One past each published bound of `MaxResults` (range 1..100) and `NameContains`
    // (len 0..63, pattern ^[a-zA-Z0-9-]+$) is refused by the modelled constraint, before
    // the handler runs.
    let too_long_needle = "a".repeat(64);
    let outside_the_modelled_constraints: Vec<(&str, Value)> = vec![
        ("max-results-below-the-minimum", json!({ "MaxResults": 0 })),
        (
            "max-results-one-past-the-maximum",
            json!({ "MaxResults": 101 }),
        ),
        (
            "name-contains-one-past-the-63-character-maximum",
            json!({ "NameContains": too_long_needle }),
        ),
        (
            "name-contains-outside-the-modelled-pattern",
            json!({ "NameContains": "under_score" }),
        ),
    ];

    for (case, body) in outside_the_modelled_constraints {
        let (status, parsed) = emulator
            .call("ListNotebookInstanceLifecycleConfigs", &body)
            .await;
        assert_eq!(
            status, 400,
            "case {case}: the request must be refused, got {status} {parsed}"
        );
    }

    // Every refusal above left the store intact.
    let listed = emulator
        .ok(
            "ListNotebookInstanceLifecycleConfigs",
            &json!({ "SortBy": "Name", "SortOrder": "Ascending" }),
        )
        .await;
    assert_eq!(
        listed_names(&listed),
        vec!["alpha-one", "beta-two", "demo-four", "demo-three"],
        "a refused listing must leave the store intact, got {listed:?}"
    );
}
