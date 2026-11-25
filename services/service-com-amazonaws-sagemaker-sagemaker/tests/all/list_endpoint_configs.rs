// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for SageMaker's `ListEndpointConfigs`.
//!
//! Every case goes over the wire (`POST /` with
//! `X-Amz-Target: SageMaker.ListEndpointConfigs`, `awsJson1_1`) through the same
//! `MultiServiceRouter` the production router uses, so what these tests exercise is what a
//! real SDK client gets. The harness (`Emulator`, `error_code`, `str_field`) and the
//! model/configuration body helpers are the crate's own, shared from
//! `inference_deployment_lifecycle`.
//!
//! The tests assert RULES over a TABLE of inputs rather than one recorded call: every
//! member of the modelled `EndpointConfigSortKey` catalog crossed with every member of
//! `OrderKey`, the name filter at several selectivities (including a needle equal to a
//! whole name), each creation-time bound and each side of it, every page size from one
//! entry to past the published `MaxResults` maximum, and the summary each entry carries
//! compared against what `DescribeEndpointConfig` replays.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::ops::types::{
    ENDPOINT_CONFIG_SORT_KEY_VALUES, ORDER_KEY_VALUES,
};
use service_com_amazonaws_sagemaker_sagemaker::state::{self, MAX_PAGE_SIZE};

use crate::inference_deployment_lifecycle::{
    error_code, one_variant_config, primary_container, str_field, Emulator, ACCOUNT,
};

/// The ARN resource type SageMaker gives an endpoint configuration — the prefix the
/// modelled `EndpointConfigArn` pattern states
/// (`^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:endpoint-config/`).
const ARN_KIND: &str = "endpoint-config";

/// The model every configuration below hosts: `CreateEndpointConfig` refuses a variant
/// naming a model that does not exist, so it is created first.
const MODEL: &str = "list-endpoint-configs-model";

/// The service's own server-side page ceiling, read from the service rather than
/// restated, so changing it does not fail these tests for an unrelated reason.
const PAGE_CEILING: usize = MAX_PAGE_SIZE as usize;

/// The configurations the sort and filter tables range over: the name, and the offset (in
/// whole seconds from the suite's base instant) of its `CreationTime`.
///
/// The names are deliberately NOT in creation order, so a `Name` sort and a
/// `CreationTime` sort cannot accidentally agree.
const SEEDED: [(&str, u64); 4] = [
    ("alpha-one", 0),
    ("beta-two", 10),
    ("demo-three", 20),
    ("demo-four", 30),
];

/// One seeded configuration as the store holds it: its name and the instant
/// `DescribeEndpointConfig` reports for it, in the epoch seconds the wire carries.
struct Seeded {
    name: String,
    created_at: f64,
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

/// Create the shared model over the wire.
async fn seed_model(emulator: &Emulator) {
    emulator
        .ok(
            "CreateModel",
            &json!({ "ModelName": MODEL, "PrimaryContainer": primary_container("demo:1") }),
        )
        .await;
}

/// Create [`SEEDED`] over the wire, then set each configuration's stored instant to its
/// row's offset, and answer what `DescribeEndpointConfig` replays for each one, in
/// creation order.
///
/// The instants are written straight into the ONE shared store the handlers reach through
/// `state::service_state`, so the time ordering is exact and the suite never waits on a
/// clock to separate two creates.
async fn seed_configs(emulator: &Emulator) -> Vec<Seeded> {
    seed_model(emulator).await;
    for (name, _) in SEEDED {
        emulator
            .ok("CreateEndpointConfig", &one_variant_config(name, MODEL))
            .await;
    }

    let base = whole_second_base();
    {
        let store = state::service_state(&*emulator.engine)
            .expect("the service is mounted on the shared in-memory engine");
        let mut guard = store.lock();
        for (name, created) in SEEDED {
            let config = guard
                .endpoint_configs
                .get_mut(name)
                .unwrap_or_else(|| panic!("the create stored a configuration named {name}"));
            config.creation_time = base + Duration::from_secs(created);
        }
    }

    let mut seeded = Vec::new();
    for (name, _) in SEEDED {
        let described = emulator
            .ok(
                "DescribeEndpointConfig",
                &json!({ "EndpointConfigName": name }),
            )
            .await;
        let created_at = described
            .get("CreationTime")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("expected a numeric CreationTime in {described}"));
        seeded.push(Seeded {
            name: name.to_string(),
            created_at,
        });
    }
    seeded
}

/// The `EndpointConfigName`s a response lists, in response order.
fn listed_names(body: &Value) -> Vec<String> {
    body.get("EndpointConfigs")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected an EndpointConfigs array in {body}"))
        .iter()
        .map(|summary| str_field(summary, "EndpointConfigName"))
        .collect()
}

/// A listing body with `extra`'s members folded in, so a case states only the members it
/// varies.
fn body_with(base: Value, extra: Value) -> Value {
    let mut body = base;
    for (key, value) in extra.as_object().expect("a JSON object of extra members") {
        body[key] = value.clone();
    }
    body
}

/// RULE: the listing reports the stored endpoint configurations ordered by
/// `SortBy`/`SortOrder` — defaulting to `CreationTime` and `Descending` as both members'
/// model docs state — with the configuration name as the tie-break.
#[tokio::test]
async fn list_endpoint_configs_honours_every_documented_sort() {
    let emulator = Emulator::new();
    seed_configs(&emulator).await;

    let by_name_ascending = vec!["alpha-one", "beta-two", "demo-four", "demo-three"];
    let by_name_descending = vec!["demo-three", "demo-four", "beta-two", "alpha-one"];
    let by_created_ascending = vec!["alpha-one", "beta-two", "demo-three", "demo-four"];
    let by_created_descending = vec!["demo-four", "demo-three", "beta-two", "alpha-one"];

    // Rows: the documented defaults, then EVERY member of the modelled
    // `EndpointConfigSortKey` catalog crossed with every member of `OrderKey` (both read
    // from the generated value slices, not hand-listed here), then each member sent on
    // its own so the other one's default applies.
    let mut cases: Vec<(String, Value, Vec<&str>)> = vec![(
        "documented-defaults".to_string(),
        json!({}),
        by_created_descending.clone(),
    )];
    for sort_by in ENDPOINT_CONFIG_SORT_KEY_VALUES {
        for sort_order in ORDER_KEY_VALUES {
            let expected = match (*sort_by, *sort_order) {
                ("Name", "Ascending") => by_name_ascending.clone(),
                ("Name", _) => by_name_descending.clone(),
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
        let listed = emulator.ok("ListEndpointConfigs", &body).await;
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
async fn equal_creation_instants_keep_one_order_in_both_directions() {
    let emulator = Emulator::new();
    seed_model(&emulator).await;
    // Rows differ only in name; every configuration shares ONE creation instant, so the
    // sort key never decides the order.
    let names = ["c-third", "a-first", "b-second"];
    for name in names {
        emulator
            .ok("CreateEndpointConfig", &one_variant_config(name, MODEL))
            .await;
    }
    let shared = whole_second_base();
    {
        let store = state::service_state(&*emulator.engine)
            .expect("the service is mounted on the shared in-memory engine");
        let mut guard = store.lock();
        for name in names {
            let config = guard
                .endpoint_configs
                .get_mut(name)
                .unwrap_or_else(|| panic!("the create stored a configuration named {name}"));
            config.creation_time = shared;
        }
    }

    for sort_order in ORDER_KEY_VALUES {
        let case = format!("CreationTime-{sort_order}");
        let listed = emulator
            .ok(
                "ListEndpointConfigs",
                &json!({ "SortBy": "CreationTime", "SortOrder": sort_order }),
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

/// RULE: `NameContains` selects exactly the configurations whose name contains that
/// string, so a needle equal to a whole name selects that configuration alone, and
/// nothing else about the listing changes.
#[tokio::test]
async fn list_endpoint_configs_filters_on_name_contains() {
    let emulator = Emulator::new();
    seed_configs(&emulator).await;

    // Rows differ along the filter's selectivity and the shape of the substring: a
    // prefix, an infix, a suffix, a whole name, a hyphen, one nothing matches and one
    // differing only in case. The modelled `EndpointConfigNameContains` is len 0..63,
    // pattern ^[a-zA-Z0-9-]+$ (smithy_query operation=ListEndpointConfigs).
    let cases: Vec<(&str, &str, Vec<&str>)> = vec![
        (
            "prefix-two-matches",
            "demo",
            vec!["demo-four", "demo-three"],
        ),
        ("prefix-one-match", "alpha", vec!["alpha-one"]),
        ("infix", "ta-tw", vec!["beta-two"]),
        ("suffix", "three", vec!["demo-three"]),
        ("whole-name", "demo-four", vec!["demo-four"]),
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
                "ListEndpointConfigs",
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

/// RULE: each creation-time bound selects the configurations on its side of the bound, the
/// bound itself included — `CreationTimeAfter` selects a creation time "greater than or
/// equal to the specified time" — and the bounds combine with each other and with
/// `NameContains`.
#[tokio::test]
async fn list_endpoint_configs_filters_on_the_creation_time_bounds() {
    let emulator = Emulator::new();
    let seeded = seed_configs(&emulator).await;
    // A bound strictly between the second and third configuration, so the split is
    // unambiguous, plus bounds one second to either side of a stored instant and bounds
    // exactly on one.
    let split = (seeded[1].created_at + seeded[2].created_at) / 2.0;
    let second = seeded[1].created_at;
    let one_second = 1.0;
    let by_created = |extra: Value| {
        body_with(
            json!({ "SortBy": "CreationTime", "SortOrder": "Ascending" }),
            extra,
        )
    };

    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "before-the-split",
            by_created(json!({ "CreationTimeBefore": split })),
            vec!["alpha-one", "beta-two"],
        ),
        (
            "after-the-split",
            by_created(json!({ "CreationTimeAfter": split })),
            vec!["demo-three", "demo-four"],
        ),
        (
            "after-exactly-a-stored-instant",
            by_created(json!({ "CreationTimeAfter": second })),
            vec!["beta-two", "demo-three", "demo-four"],
        ),
        (
            "before-exactly-a-stored-instant",
            by_created(json!({ "CreationTimeBefore": second })),
            vec!["alpha-one", "beta-two"],
        ),
        (
            "after-one-second-past-a-stored-instant",
            by_created(json!({ "CreationTimeAfter": second + one_second })),
            vec!["demo-three", "demo-four"],
        ),
        (
            "before-one-second-short-of-a-stored-instant",
            by_created(json!({ "CreationTimeBefore": second - one_second })),
            vec!["alpha-one"],
        ),
        (
            "both-bounds-around-one-configuration",
            by_created(json!({
                "CreationTimeAfter": split,
                "CreationTimeBefore": (seeded[2].created_at + seeded[3].created_at) / 2.0,
            })),
            vec!["demo-three"],
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
        let listed = emulator.ok("ListEndpointConfigs", &body).await;
        assert_eq!(
            listed_names(&listed),
            expected,
            "case {case}: selection, got {listed:?}"
        );
    }
}

/// RULE: every listed summary carries exactly what the store holds for that configuration
/// — the name, the minted ARN and the stored creation instant — the same values
/// `DescribeEndpointConfig` replays, and listing changes nothing a later caller sees.
#[tokio::test]
async fn every_endpoint_config_summary_replays_the_stored_configuration() {
    let emulator = Emulator::new();
    let seeded = seed_configs(&emulator).await;

    let ascending = json!({ "SortBy": "CreationTime", "SortOrder": "Ascending" });
    let listed = emulator.ok("ListEndpointConfigs", &ascending).await;
    let summaries = listed
        .get("EndpointConfigs")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected an EndpointConfigs array in {listed}"));
    assert_eq!(
        summaries.len(),
        SEEDED.len(),
        "every stored configuration must be listed, got {listed:?}"
    );

    for (index, config) in seeded.iter().enumerate() {
        let summary = &summaries[index];
        let name = &config.name;
        assert_eq!(
            str_field(summary, "EndpointConfigName"),
            name.as_str(),
            "case {name}: EndpointConfigName, got {summary:?}"
        );
        assert_eq!(
            str_field(summary, "EndpointConfigArn"),
            format!("arn:aws:sagemaker:us-east-1:{ACCOUNT}:{ARN_KIND}/{name}"),
            "case {name}: EndpointConfigArn, got {summary:?}"
        );
        assert_eq!(
            summary.get("CreationTime").and_then(Value::as_f64),
            Some(config.created_at),
            "case {name}: CreationTime in the summary must be the stored one, got {summary:?}"
        );
    }

    // A listing is a read: it changes nothing a later caller sees.
    let again = emulator.ok("ListEndpointConfigs", &ascending).await;
    assert_eq!(
        again, listed,
        "listing twice must report the same configurations, got {again:?}"
    );
    for config in &seeded {
        let name = &config.name;
        let described = emulator
            .ok(
                "DescribeEndpointConfig",
                &json!({ "EndpointConfigName": name }),
            )
            .await;
        assert_eq!(
            described.get("CreationTime").and_then(Value::as_f64),
            Some(config.created_at),
            "case {name}: a listing must not change the stored CreationTime, got {described:?}"
        );
        assert_eq!(
            str_field(&described, "EndpointConfigArn"),
            format!("arn:aws:sagemaker:us-east-1:{ACCOUNT}:{ARN_KIND}/{name}"),
            "case {name}: the described ARN must be the listed one, got {described:?}"
        );
    }
}

/// RULE: the listing reflects the store as it is at the call, so a configuration appears
/// once it is created and is gone after a delete.
#[tokio::test]
async fn the_endpoint_config_listing_reflects_every_create_and_delete() {
    let emulator = Emulator::new();
    seed_model(&emulator).await;

    let empty = emulator.ok("ListEndpointConfigs", &json!({})).await;
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
    let by_name = json!({ "SortBy": "Name", "SortOrder": "Ascending" });
    let mut expected: Vec<String> = Vec::new();
    for (name, _) in SEEDED {
        emulator
            .ok("CreateEndpointConfig", &one_variant_config(name, MODEL))
            .await;
        expected.push(name.to_string());
        expected.sort();
        let listed = emulator.ok("ListEndpointConfigs", &by_name).await;
        assert_eq!(
            listed_names(&listed),
            expected,
            "case after-creating-{name}: the listing must reflect the create, got {listed:?}"
        );
    }

    // And shrinks by one with every delete.
    for (name, _) in SEEDED {
        emulator
            .ok(
                "DeleteEndpointConfig",
                &json!({ "EndpointConfigName": name }),
            )
            .await;
        expected.retain(|stored| stored.as_str() != name);
        let listed = emulator.ok("ListEndpointConfigs", &by_name).await;
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
async fn endpoint_config_paging_walks_the_whole_result_exactly_once() {
    let emulator = Emulator::new();
    seed_configs(&emulator).await;
    let ordered = vec!["alpha-one", "beta-two", "demo-four", "demo-three"];

    // Rows: every page size from one entry up to past the end of the result set,
    // including the size that exactly fits it (4) and the published maximum of
    // `MaxResults` (range 1..100 — smithy_query operation=ListEndpointConfigs).
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
            let listed = emulator.ok("ListEndpointConfigs", &body).await;
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

    // The same walk under the time sort, so the resume boundary is not tied to the name
    // sort it happens to share a tie-break with.
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut body = json!({
            "SortBy": "CreationTime",
            "SortOrder": "Descending",
            "MaxResults": 2,
        });
        if let Some(ref value) = token {
            body["NextToken"] = Value::String(value.clone());
        }
        let listed = emulator.ok("ListEndpointConfigs", &body).await;
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
        vec!["demo-four", "demo-three", "beta-two", "alpha-one"],
        "case time-sorted-walk: the pages must cover the whole ordered result once"
    );
}

/// RULE: the page is capped server-side, so no single call serialises the whole store
/// however large `MaxResults` is or whether the caller sends it at all — and the resume
/// token still walks the full set.
#[tokio::test]
async fn the_endpoint_config_page_is_capped_server_side_even_when_the_caller_asks_for_more() {
    // The store is seeded past the service's own ceiling, so a capped page and an
    // uncapped one are distinguishable.
    let stored = PAGE_CEILING + 7;

    let emulator = Emulator::new();
    seed_model(&emulator).await;
    for index in 0..stored {
        emulator
            .ok(
                "CreateEndpointConfig",
                &one_variant_config(&format!("c{index:03}"), MODEL),
            )
            .await;
    }

    // Rows: no `MaxResults` at all, the modelled maximum (above the ceiling), and a value
    // below the ceiling, which still wins because it is the smaller cap.
    let by_name = json!({ "SortBy": "Name", "SortOrder": "Ascending" });
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
        let listed = emulator
            .ok("ListEndpointConfigs", &body_with(by_name.clone(), extra))
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
        let mut body = by_name.clone();
        if let Some(ref value) = token {
            body["NextToken"] = Value::String(value.clone());
        }
        let listed = emulator.ok("ListEndpointConfigs", &body).await;
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
async fn requests_the_endpoint_config_listing_cannot_serve_are_refused() {
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
        let (status, parsed) = emulator.call("ListEndpointConfigs", &body).await;
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
        let (status, parsed) = emulator.call("ListEndpointConfigs", &body).await;
        assert_eq!(
            status, 400,
            "case {case}: the request must be refused, got {status} {parsed}"
        );
    }

    // Every refusal above left the store intact.
    let listed = emulator
        .ok(
            "ListEndpointConfigs",
            &json!({ "SortBy": "Name", "SortOrder": "Ascending" }),
        )
        .await;
    assert_eq!(
        listed_names(&listed),
        vec!["alpha-one", "beta-two", "demo-four", "demo-three"],
        "a refused listing must leave the store intact, got {listed:?}"
    );
}
