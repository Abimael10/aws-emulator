// SPDX-License-Identifier: CC0-1.0

//! The Macie findings-filter plane — defining a filter, reading its criteria
//! and settings back, and deleting it — end to end over the wire.
//!
//! Each case drives the production `MultiServiceRouter` through the harness
//! `macie_session_and_custom_data_identifiers` builds, so what is pinned is the
//! HTTP behaviour and the response BODIES rather than a handler called
//! directly.
//!
//! The rules under test, each asserted over a SET of inputs rather than one
//! request:
//!
//! * `CreateFindingsFilter` PERSISTS every member of the request and mints an
//!   id and an ARN for the filter, so `GetFindingsFilter` reports the stored
//!   settings — the action, the criteria with every operator the request used,
//!   the name, the description, the position and the tags — and reports nothing
//!   for an optional member the create named;
//! * a create outside a documented bound is refused with 400
//!   `ValidationException` and stores nothing;
//! * a client token replays the filter it minted for as long as that filter
//!   exists, and nothing once it is deleted;
//! * `DeleteFindingsFilter` deletes the filter, so every later call addressing
//!   that id answers 404 `ResourceNotFoundException` with the refusal Macie
//!   words for a resource it does not hold, while the filters it did not name
//!   are untouched;
//! * a filter belongs to the region its create addressed, and all three
//!   operations need a Macie session in the region the request names;
//! * `UpdateFindingsFilter` edits exactly the settings the request names and
//!   leaves every other one as it was;
//! * a retried create carrying the same `clientToken` replays the one filter
//!   that token minted.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::macie_session_and_custom_data_identifiers::{
    assert_id_grammar, call_in, emulator, emulator_with_engine, enable_in, error_code,
    error_message,
};

/// The region most cases sign for; it must reach every minted ARN.
const REGION: &str = "us-west-2";

/// A second region, for the cases that pin the per-region scope of a filter.
const OTHER_REGION: &str = "eu-west-1";

/// The findings-filter collection's URI, from `CreateFindingsFilter`'s `@http`
/// binding.
const FILTERS_URI: &str = "/findingsfilters";

/// Create a findings filter in `region` with the body a caller sends.
async fn create(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in(router, region, "POST", FILTERS_URI, &body.to_string()).await
}

/// Read the findings filter `id` in `region`.
async fn read(router: &MultiServiceRouter, region: &str, id: &str) -> (u16, Value) {
    call_in(router, region, "GET", &format!("{FILTERS_URI}/{id}"), "").await
}

/// Delete the findings filter `id` in `region`.
async fn delete(router: &MultiServiceRouter, region: &str, id: &str) -> (u16, Value) {
    call_in(router, region, "DELETE", &format!("{FILTERS_URI}/{id}"), "").await
}

/// Edit the findings filter `id` in `region` with the body a caller sends.
async fn update(
    router: &MultiServiceRouter,
    region: &str,
    id: &str,
    body: &Value,
) -> (u16, Value) {
    call_in(
        router,
        region,
        "PATCH",
        &format!("{FILTERS_URI}/{id}"),
        &body.to_string(),
    )
    .await
}

/// Create one filter, requiring the documented 200, and hand back the id and
/// the ARN it reported.
async fn create_ok(
    router: &MultiServiceRouter,
    region: &str,
    body: &Value,
    case: &str,
) -> (String, String) {
    let (status, reply) = create(router, region, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateFindingsFilter must answer 200, got {status} with {reply:?}"
    );
    let id = reply["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create must report an id, got {reply:?}"))
        .to_string();
    let arn = reply["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create must report an arn, got {reply:?}"))
        .to_string();
    (id, arn)
}

/// The ARN a findings filter of `region` carries, built the way every Macie ARN
/// is: the service, the region the call addressed, the emulator account and the
/// resource.
fn expected_arn(region: &str, id: &str) -> String {
    format!(
        "arn:aws:macie2:{region}:{}:findings-filter/{id}",
        ::engine::emulator_account_id()
    )
}

/// How the service refuses a call addressing a filter the region holds none of.
/// The whole sentence is fixed text — it names no id — so it is spelled out here
/// rather than built from the request.
const NOT_FOUND_MESSAGE: &str = "Resource not found. Verify any provided IDs are correct.";

/// One create body and the settings a read of the filter it mints must report.
///
/// The rows differ along what the rule generalises over: the action, the
/// condition operators the criteria use, how many properties they test, and
/// which optional members the request names at all.
struct Row {
    case: &'static str,
    body: Value,
}

/// The create bodies this suite spans, each exercising a different part of the
/// request. Every member that appears here must come back out of a read
/// unchanged, and every one that does not must be absent from it.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an archiving filter with one equality condition and no optional member",
            body: json!({
                "action": "ARCHIVE",
                "name": "archive-low-severity",
                "findingCriteria": {
                    "criterion": {
                        "severity.description": { "eq": ["Low"] }
                    }
                }
            }),
        },
        Row {
            case: "a NOOP filter with a description, a position and tags",
            body: json!({
                "action": "NOOP",
                "name": "watch-public-buckets",
                "description": "Flags findings for buckets that allow public access.",
                "position": 3,
                "findingCriteria": {
                    "criterion": {
                        "resourcesAffected.s3Bucket.publicAccess.effectivePermission": {
                            "eqExactMatch": ["PUBLIC"]
                        }
                    }
                },
                "tags": { "owner": "security", "tier": "1" }
            }),
        },
        Row {
            case: "numeric conditions over several properties at once",
            body: json!({
                "action": "ARCHIVE",
                "name": "archive-small-and-recent",
                "position": 0,
                "findingCriteria": {
                    "criterion": {
                        "resourcesAffected.s3Object.size": { "lt": 1024, "gte": 1 },
                        "count": { "gt": 2, "lte": 10 },
                        "type": { "neq": ["Policy:IAMUser/S3BucketPublic"] }
                    }
                }
            }),
        },
        Row {
            case: "a name at its documented minimum and criteria that test nothing",
            body: json!({
                "action": "NOOP",
                "name": "abc",
                "findingCriteria": { "criterion": {} }
            }),
        },
        Row {
            case: "a name at its documented maximum and a negative position",
            body: json!({
                "action": "ARCHIVE",
                "name": "f".repeat(64),
                "position": -1,
                "findingCriteria": {
                    "criterion": { "category": { "eq": ["CLASSIFICATION", "POLICY"] } }
                }
            }),
        },
    ]
}

/// Every member a create stored is reported by a read of the filter, every
/// member it did not name is absent from that read, and the id and ARN the
/// create minted address it.
#[tokio::test]
async fn a_created_filter_reads_back_exactly_as_it_was_stored() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    let mut minted: Vec<String> = Vec::new();
    for Row { case, body } in rows() {
        let (id, arn) = create_ok(&router, REGION, &body, case).await;
        assert_id_grammar(&id, &format!("{case}: the minted findings filter id"));
        assert_eq!(
            arn,
            expected_arn(REGION, &id),
            "case {case:?}: the minted ARN must name this region, account and filter, got {arn:?}"
        );
        assert!(
            !minted.contains(&id),
            "case {case:?}: every create mints its own id, got {id:?} twice"
        );
        minted.push(id.clone());

        let (status, filter) = read(&router, REGION, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetFindingsFilter must answer 200, got {status} with {filter:?}"
        );
        assert_eq!(
            filter["id"].as_str(),
            Some(id.as_str()),
            "case {case:?}: the read must report the id it was addressed by, got {filter:?}"
        );
        assert_eq!(
            filter["arn"].as_str(),
            Some(arn.as_str()),
            "case {case:?}: the read must report the ARN the create minted, got {filter:?}"
        );

        // Each member the create named must come back unchanged, and each one
        // it did not name must be absent: a read reports the stored filter, not
        // a filled-in copy of it.
        for member in ["action", "name", "description", "position", "tags"] {
            assert_eq!(
                filter[member],
                body.get(member).cloned().unwrap_or(Value::Null),
                "case {case:?}: {member} must be reported as stored, got {:?}",
                filter[member]
            );
        }
        // The criteria are what the filter IS, so they are always reported,
        // with every operator and property the create used.
        assert_eq!(
            filter["findingCriteria"],
            body["findingCriteria"],
            "case {case:?}: findingCriteria must be reported as stored, got {:?}",
            filter["findingCriteria"]
        );
    }

    // Each filter is still readable after the ones created later, so a create
    // adds to the registry rather than replacing what is in it.
    for (at, id) in minted.iter().enumerate() {
        let (status, _) = read(&router, REGION, id).await;
        assert_eq!(
            status, 200,
            "case {at}: every filter created stays readable, got {status} for {id:?}"
        );
    }
}

/// A create that breaks a documented bound is refused with the modelled
/// `ValidationException`, and leaves the registry exactly as it was.
#[tokio::test]
async fn a_create_outside_a_documented_bound_is_refused_and_stores_nothing() {
    let (router, engine) = emulator_with_engine();
    enable_in(&router, REGION, "{}").await;

    let criteria = json!({ "criterion": { "severity.description": { "eq": ["Low"] } } });
    // One tag map of `count` entries whose keys and values are as wide as a row
    // asks for.
    let tag_map = |count: usize, key_chars: usize, value_chars: usize| -> Value {
        let mut tags = BTreeMap::new();
        for at in 0..count {
            let key = format!("{at}");
            let padding = key_chars.saturating_sub(key.chars().count());
            tags.insert(format!("{key}{}", "k".repeat(padding)), "v".repeat(value_chars));
        }
        json!(tags)
    };
    // One `findingCriteria` of `conditions` conditions, the first of which
    // lists `values` values.
    let criteria_of = |conditions: usize, values: usize| -> Value {
        let mut criterion = BTreeMap::new();
        for at in 0..conditions {
            let listed = if at == 0 { values } else { 1 };
            criterion.insert(
                format!("property{at}"),
                json!({ "eq": (0..listed).map(|v| format!("value{v}")).collect::<Vec<_>>() }),
            );
        }
        json!({ "criterion": criterion })
    };

    // The bounds come from the request members' own documentation — a name of 3
    // to 64 characters, a description of as many as 512, and as many as 50 tags
    // with a key of at most 128 characters and a value of at most 256 — and from
    // the emulator's own bound on how much one accepted request stores: as many
    // as 100 conditions, each listing as many as 100 values. Each row breaks
    // exactly one of them.
    let refused: Vec<(&str, Value)> = vec![
        (
            "an empty name",
            json!({ "action": "ARCHIVE", "name": "", "findingCriteria": criteria }),
        ),
        (
            "a name one character short of its minimum",
            json!({ "action": "ARCHIVE", "name": "ab", "findingCriteria": criteria }),
        ),
        (
            "a name one character past its maximum",
            json!({ "action": "NOOP", "name": "n".repeat(65), "findingCriteria": criteria }),
        ),
        (
            "a description one character past its maximum",
            json!({
                "action": "ARCHIVE",
                "name": "too-long-a-description",
                "description": "d".repeat(513),
                "findingCriteria": criteria
            }),
        ),
        (
            "one tag too many",
            json!({
                "action": "ARCHIVE",
                "name": "too-many-tags",
                "findingCriteria": criteria,
                "tags": tag_map(51, 3, 3)
            }),
        ),
        (
            "a tag key one character too long",
            json!({
                "action": "ARCHIVE",
                "name": "too-long-a-tag-key",
                "findingCriteria": criteria,
                "tags": tag_map(1, 129, 3)
            }),
        ),
        (
            "a tag value one character too long",
            json!({
                "action": "ARCHIVE",
                "name": "too-long-a-tag-value",
                "findingCriteria": criteria,
                "tags": tag_map(1, 3, 257)
            }),
        ),
        (
            "one condition too many",
            json!({
                "action": "ARCHIVE",
                "name": "too-many-conditions",
                "findingCriteria": criteria_of(101, 1)
            }),
        ),
        (
            "one value too many in a condition",
            json!({
                "action": "ARCHIVE",
                "name": "too-many-values",
                "findingCriteria": criteria_of(1, 101)
            }),
        ),
    ];

    for (case, body) in refused {
        let (status, reply) = create(&router, REGION, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a create outside a documented bound must answer 400, \
             got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {reply:?}"
        );
        assert!(
            !error_message(&reply).is_empty(),
            "case {case:?}: a refusal must say what it refused, got {reply:?}"
        );
        assert_eq!(
            engine
                .state::<service_com_amazonaws_macie2_macie2::ComAmazonawsMacie2Macie2State>()
                .lock()
                .findings_filters
                .len(),
            0,
            "case {case:?}: a refused create must store nothing"
        );
    }

    // An accepted create at every limit at once is what makes the rows above a
    // bound rather than a blanket refusal.
    let accepted = json!({
        "action": "ARCHIVE",
        "name": "n".repeat(64),
        "description": "d".repeat(512),
        "findingCriteria": criteria_of(100, 100),
        "tags": tag_map(50, 128, 256)
    });
    let (id, _) = create_ok(&router, REGION, &accepted, "every bound at its limit").await;
    let (status, stored) = read(&router, REGION, &id).await;
    assert_eq!(
        status, 200,
        "case \"every bound at its limit\": the filter must read back, got {status}"
    );
    assert_eq!(
        stored["findingCriteria"], accepted["findingCriteria"],
        "case \"every bound at its limit\": the criteria at the limit must be stored whole"
    );
}

/// A delete removes exactly the filter it names: every later call addressing
/// that id finds nothing, and the other filters are untouched.
#[tokio::test]
async fn deleting_a_filter_makes_every_later_call_not_find_it() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    let mut ids: Vec<String> = Vec::new();
    for Row { case, body } in rows() {
        let (id, _) = create_ok(&router, REGION, &body, case).await;
        ids.push(id);
    }

    // Each filter is deleted in turn, so the rule is asserted at every registry
    // size rather than on one filter.
    let mut live = ids.clone();
    while let Some(gone) = live.pop() {
        let (status, reply) = delete(&router, REGION, &gone).await;
        assert_eq!(
            status, 200,
            "case {gone:?}: DeleteFindingsFilter must answer 200, got {status} with {reply:?}"
        );

        // A read, an update and a second delete of the filter that went: all
        // three find nothing.
        for (call, (status, reply)) in [
            ("GetFindingsFilter", read(&router, REGION, &gone).await),
            (
                "UpdateFindingsFilter",
                update(&router, REGION, &gone, &json!({ "name": "renamed-filter" })).await,
            ),
            ("DeleteFindingsFilter", delete(&router, REGION, &gone).await),
        ] {
            assert_eq!(
                status, 404,
                "case {gone:?}: {call} after the delete must answer 404, \
                 got {status} with {reply:?}"
            );
            assert_eq!(
                error_code(&reply),
                "ResourceNotFoundException",
                "case {gone:?}: {call} must answer a ResourceNotFoundException, got {reply:?}"
            );
            assert_eq!(
                error_message(&reply),
                NOT_FOUND_MESSAGE,
                "case {gone:?}: {call} must answer the refusal Macie words for a resource \
                 it does not hold"
            );
        }

        for still_there in &live {
            let (status, _) = read(&router, REGION, still_there).await;
            assert_eq!(
                status, 200,
                "case {gone:?}: deleting one filter must leave {still_there:?} readable, \
                 got {status}"
            );
        }
    }
}

/// An id no create of the addressed region minted is not found, whatever it
/// looks like.
#[tokio::test]
async fn an_unknown_filter_is_not_found() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    // The ids vary along what an id can be: an id another region's create
    // minted, an id of the right form that nothing minted, and ids of forms
    // this service never issues.
    let (elsewhere, _) = {
        enable_in(&router, OTHER_REGION, "{}").await;
        create_ok(
            &router,
            OTHER_REGION,
            &rows()[0].body,
            "a filter of another region",
        )
        .await
    };
    let unknown = [
        ("another region's filter", elsewhere.as_str()),
        ("an unminted UUID", "6f1a9c1e-1f2b-4a3c-8d4e-5f6a7b8c9d0e"),
        ("a bare word", "nosuchfilter"),
        ("22 base-36 digits", "abcdefghij0123456789kl"),
    ];

    for (case, id) in unknown {
        for (call, (status, reply)) in [
            ("GetFindingsFilter", read(&router, REGION, id).await),
            (
                "UpdateFindingsFilter",
                update(&router, REGION, id, &json!({ "name": "renamed-filter" })).await,
            ),
            ("DeleteFindingsFilter", delete(&router, REGION, id).await),
        ] {
            assert_eq!(
                status, 404,
                "case {case:?}: {call} must answer 404, got {status} with {reply:?}"
            );
            assert_eq!(
                error_code(&reply),
                "ResourceNotFoundException",
                "case {case:?}: {call} must answer a ResourceNotFoundException, got {reply:?}"
            );
            assert_eq!(
                error_message(&reply),
                NOT_FOUND_MESSAGE,
                "case {case:?}: {call} must answer the refusal Macie words for a resource \
                 it does not hold"
            );
        }
    }

    // The filter of the other region is still there: a read of it from its own
    // region finds it.
    let (status, _) = read(&router, OTHER_REGION, &elsewhere).await;
    assert_eq!(
        status, 200,
        "a filter must stay readable in the region its create addressed, got {status}"
    );
}

/// A filter belongs to the region its create addressed: another region neither
/// reads it nor deletes it, and the two regions hold separate registries.
#[tokio::test]
async fn a_filter_belongs_to_the_region_its_create_addressed() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    enable_in(&router, OTHER_REGION, "{}").await;

    for Row { case, body } in rows() {
        let (here, arn) = create_ok(&router, REGION, &body, case).await;
        assert_eq!(
            arn,
            expected_arn(REGION, &here),
            "case {case:?}: the ARN must name the region the create addressed, got {arn:?}"
        );

        let (status, reply) = read(&router, OTHER_REGION, &here).await;
        assert_eq!(
            status, 404,
            "case {case:?}: another region must not read this filter, got {status} with {reply:?}"
        );
        let (status, reply) = delete(&router, OTHER_REGION, &here).await;
        assert_eq!(
            status, 404,
            "case {case:?}: another region must not delete this filter, got {status} with {reply:?}"
        );
        let (status, reply) =
            update(&router, OTHER_REGION, &here, &json!({ "name": "renamed-filter" })).await;
        assert_eq!(
            status, 404,
            "case {case:?}: another region must not edit this filter, got {status} with {reply:?}"
        );

        let (status, _) = read(&router, REGION, &here).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the filter's own region must still read it, got {status}"
        );
    }
}

/// Every findings-filter operation is refused until Macie is enabled in the
/// region the request addresses.
#[tokio::test]
async fn every_operation_is_refused_until_macie_is_enabled() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;
    let (id, _) = create_ok(&router, REGION, &rows()[0].body, "a filter of the enabled region")
        .await;

    // Macie is enabled in REGION only, so OTHER_REGION has no session: each
    // call there is refused, whether it names an existing filter or not.
    let calls = [
        ("CreateFindingsFilter", create(&router, OTHER_REGION, &rows()[0].body).await),
        ("GetFindingsFilter", read(&router, OTHER_REGION, &id).await),
        (
            "UpdateFindingsFilter",
            update(&router, OTHER_REGION, &id, &json!({ "name": "renamed-filter" })).await,
        ),
        ("DeleteFindingsFilter", delete(&router, OTHER_REGION, &id).await),
    ];
    for (call, (status, reply)) in calls {
        assert_eq!(
            status, 403,
            "case {call:?}: a call to a region with no Macie session must answer 403, \
             got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "AccessDeniedException",
            "case {call:?}: expected an AccessDeniedException, got {reply:?}"
        );
        assert_eq!(
            error_message(&reply),
            "Macie is not enabled.",
            "case {call:?}: expected the refusal Macie words for an account with no session"
        );
    }
}

/// An update edits exactly the settings the request names, and leaves every
/// other setting of the filter as it was.
#[tokio::test]
async fn an_update_edits_only_the_settings_it_names() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    // Each row names a different subset of the editable members, so what is
    // pinned is both that a named setting changes and that an unnamed one does
    // not. Every member of `UpdateFindingsFilterRequest` but the id appears in
    // at least one row.
    let edits: Vec<(&str, Value)> = vec![
        ("the action alone", json!({ "action": "ARCHIVE" })),
        ("the name alone", json!({ "name": "renamed-filter" })),
        (
            "the description alone",
            json!({ "description": "A revised description of the filter." }),
        ),
        ("the position alone", json!({ "position": 7 })),
        (
            "the criteria alone",
            json!({
                "findingCriteria": {
                    "criterion": { "severity.description": { "eq": ["High", "Medium"] } }
                }
            }),
        ),
        ("no setting at all", json!({})),
        (
            "every editable setting at once",
            json!({
                "action": "ARCHIVE",
                "name": "wholly-rewritten",
                "description": "Rewritten.",
                "position": 11,
                "findingCriteria": { "criterion": { "count": { "gt": 5 } } }
            }),
        ),
        (
            "a client token beside one setting",
            json!({ "clientToken": "retry-0001", "position": 2 }),
        ),
    ];

    for (case, edit) in edits {
        // The filter each row edits names every optional member, so an unnamed
        // setting has something to be left alone.
        let (id, arn) = create_ok(&router, REGION, &rows()[1].body, case).await;
        let (_, before) = read(&router, REGION, &id).await;

        let (status, reply) = update(&router, REGION, &id, &edit).await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateFindingsFilter must answer 200, got {status} with {reply:?}"
        );
        assert_eq!(
            (reply["id"].as_str(), reply["arn"].as_str()),
            (Some(id.as_str()), Some(arn.as_str())),
            "case {case:?}: an update must report the filter it edited, got {reply:?}"
        );

        let (status, after) = read(&router, REGION, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the edited filter must still be readable, got {status}"
        );
        for member in [
            "action",
            "name",
            "description",
            "position",
            "findingCriteria",
            // `tags` is not a member of an update at all, so an edit never
            // touches the filter's tags.
            "tags",
        ] {
            let expected = edit
                .get(member)
                .cloned()
                .unwrap_or_else(|| before[member].clone());
            assert_eq!(
                after[member], expected,
                "case {case:?}: {member} must be {expected:?} after the edit, got {:?}",
                after[member]
            );
        }
    }
}

/// An update outside a documented bound is refused with 400
/// `ValidationException` and leaves the stored filter exactly as it was.
#[tokio::test]
async fn an_update_outside_a_documented_bound_is_refused_and_changes_nothing() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    // The same bounds the create is held to, on the members an update carries:
    // a name of 3 to 64 characters and a description of as many as 512.
    let refused: Vec<(&str, Value)> = vec![
        ("an empty name", json!({ "name": "" })),
        (
            "a name one character short of its minimum",
            json!({ "name": "ab" }),
        ),
        (
            "a name one character past its maximum",
            json!({ "name": "n".repeat(65) }),
        ),
        (
            "a description one character past its maximum",
            json!({ "description": "d".repeat(513) }),
        ),
    ];

    for (case, edit) in refused {
        let (id, _) = create_ok(&router, REGION, &rows()[1].body, case).await;
        let (_, before) = read(&router, REGION, &id).await;

        let (status, reply) = update(&router, REGION, &id, &edit).await;
        assert_eq!(
            status, 400,
            "case {case:?}: an update outside a documented bound must answer 400, \
             got {status} with {reply:?}"
        );
        assert_eq!(
            error_code(&reply),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {reply:?}"
        );

        let (_, after) = read(&router, REGION, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused update must leave the filter as it was"
        );
    }

    // An accepted edit at both limits is what makes the rows above a bound
    // rather than a blanket refusal.
    let (id, _) = create_ok(&router, REGION, &rows()[1].body, "every bound at its limit").await;
    let (status, reply) = update(
        &router,
        REGION,
        &id,
        &json!({ "name": "n".repeat(64), "description": "d".repeat(512) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"every bound at its limit\": the edit must be accepted, got {status} with {reply:?}"
    );
}

/// A retried create carrying the same `clientToken` replays the one filter that
/// token minted; a different token mints another.
#[tokio::test]
async fn a_retried_create_replays_the_filter_its_client_token_minted() {
    let router = emulator();
    enable_in(&router, REGION, "{}").await;

    for Row { case, body } in rows() {
        let token = format!("token-for-{case}");
        let mut with_token = body.clone();
        with_token["clientToken"] = json!(token);

        let (first, first_arn) = create_ok(&router, REGION, &with_token, case).await;
        let (replayed, replayed_arn) = create_ok(&router, REGION, &with_token, case).await;
        assert_eq!(
            (replayed.as_str(), replayed_arn.as_str()),
            (first.as_str(), first_arn.as_str()),
            "case {case:?}: a retry carrying the same clientToken must replay the filter \
             that token minted"
        );

        let mut other_token = body.clone();
        other_token["clientToken"] = json!(format!("{token}-again"));
        let (minted, _) = create_ok(&router, REGION, &other_token, case).await;
        assert_ne!(
            minted, first,
            "case {case:?}: a create carrying another clientToken must mint its own filter"
        );

        // Both filters are in the registry, so the replay did not stand in for
        // a create that never happened.
        for id in [&first, &minted] {
            let (status, _) = read(&router, REGION, id).await;
            assert_eq!(
                status, 200,
                "case {case:?}: {id:?} must be readable after the retried create, got {status}"
            );
        }
    }
}

/// A token replays the filter it minted, so once that filter is deleted the
/// token replays nothing: the next create carrying it mints a new filter, and
/// the record of the token goes with the filter it minted.
#[tokio::test]
async fn a_client_token_replays_nothing_once_its_filter_is_deleted() {
    let (router, engine) = emulator_with_engine();
    enable_in(&router, REGION, "{}").await;

    // The rows vary along the body the token is sent with, so what is pinned is
    // the token's lifetime rather than one request's shape.
    for Row { case, body } in rows() {
        let mut with_token = body.clone();
        with_token["clientToken"] = json!(format!("token-for-{case}"));

        let (first, _) = create_ok(&router, REGION, &with_token, case).await;
        let (status, reply) = delete(&router, REGION, &first).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the delete must answer 200, got {status} with {reply:?}"
        );

        let (second, _) = create_ok(&router, REGION, &with_token, case).await;
        assert_ne!(
            second, first,
            "case {case:?}: a token whose filter was deleted must replay nothing"
        );
        let (status, _) = read(&router, REGION, &second).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the filter the token minted again must be readable, got {status}"
        );

        delete(&router, REGION, &second).await;
        // Deleting a filter drops what was remembered about the tokens that
        // minted it, so a caller creating and deleting forever cannot grow the
        // store past the registry's own bound.
        let state = engine.state::<service_com_amazonaws_macie2_macie2::ComAmazonawsMacie2Macie2State>();
        let held = state.lock();
        assert!(
            held.findings_filters.is_empty(),
            "case {case:?}: both filters must be gone, {} are held",
            held.findings_filters.len()
        );
        assert!(
            held.idempotency.is_empty(),
            "case {case:?}: no client token of a deleted filter may be remembered, {} are",
            held.idempotency.len()
        );
    }
}
