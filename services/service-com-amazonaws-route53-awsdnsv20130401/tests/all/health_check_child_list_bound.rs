// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of what the `ChildHealthChecks` list may hold: its published
//! length, and that it names each child at most once.
//!
//! The rules under test:
//!
//! * a `CALCULATED` health check may name at most the published maximum of child
//!   health checks (`HealthCheckConfig$ChildHealthChecks` targets
//!   `ChildHealthCheckList`, `len 0..256` —
//!   `smithy_query(query="shape",shape="HealthCheckConfig")`), and a request naming
//!   more is refused with `InvalidInput` and persists nothing. The list is read for
//!   every assessment of the check
//!   ([`service_com_amazonaws_route53_awsdnsv20130401::state::health_check_status`]),
//!   so its length is a bound this service enforces rather than one it assumes.
//! * the list is a SET of health check ids: a configuration that names one child
//!   more than once is refused `InvalidInput` (HTTP 400) with the one fixed sentence
//!   the service states for that fault, on every operation that persists a
//!   configuration — the create that stores a child list and the update that
//!   replaces one — and persists nothing. A repeated child would be counted twice
//!   against `HealthThreshold` ("Route 53 adds up the number of health checks that
//!   Route 53 health checkers consider to be healthy and compares that number with
//!   the value of HealthThreshold", `HealthCheckConfig$Type`), so one endpoint's
//!   health would carry a threshold meant for several.
//!
//! The rows span what each rule generalises over: both published bounds of the list
//! and one past the maximum, and — for the uniqueness rule — where the repeat sits
//! in the list, how many times it is repeated, how many distinct children sit beside
//! it and the length of the list it is in, at both operations that take a child
//! list. Everything is driven through the real router with the same harness the other
//! health-check suites use.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_route53_awsdnsv20130401::state::MAX_CHILD_HEALTH_CHECKS;

use crate::health_checks::{create_health_check, get_health_check};
use crate::health_check_updates::update_health_check;
use crate::hosted_zone_records::router;
use crate::refusal_message_rules::message_of;

/// The one input error this service's model declares (`InvalidInput`, HTTP 400).
const INVALID_INPUT: &str = "InvalidInput";

/// The ONE sentence a configuration naming a child health check twice is refused with,
/// pinned HERE rather than read from the service's own constant: the text is the thing
/// under test, so an edit to that constant has to fail these tests.
const DUPLICATE_CHILDREN_MESSAGE: &str =
    "Calculated health checks must not have duplicate child health check IDs.";

/// A `CALCULATED` configuration naming `children`, with a threshold of one.
fn calculated(children: &[String]) -> String {
    format!(
        "<Type>CALCULATED</Type><HealthThreshold>1</HealthThreshold>{}",
        child_list(children)
    )
}

/// The `ChildHealthChecks` element naming `children`, as a request writes it.
fn child_list(children: &[String]) -> String {
    let entries: String = children
        .iter()
        .map(|child| format!("<ChildHealthCheck>{child}</ChildHealthCheck>"))
        .collect();
    format!("<ChildHealthChecks>{entries}</ChildHealthChecks>")
}

/// Create `count` TCP health checks, each a health check of the account, and answer
/// their ids. They exist only to be named as children of a `CALCULATED` one.
async fn children(router: &MultiServiceRouter, tag: &str, count: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for index in 0..count {
        let created = create_health_check(
            router,
            &format!("{tag}-child-{index}"),
            &format!(
                "<Type>TCP</Type><IPAddress>192.0.2.{}</IPAddress><Port>443</Port>",
                index + 1
            ),
        )
        .await;
        ids.push(crate::health_checks::health_check_id(&created, "a child"));
    }
    ids
}

#[tokio::test]
async fn a_calculated_check_names_at_most_the_published_number_of_children() {
    let router = router();

    // Two children to build the lists out of, each a health check of the account.
    let children = children(&router, "bound", 2).await;

    // (case, how the list is built, whether the service takes it)
    let rows: Vec<(&str, Vec<String>, bool)> = vec![
        ("no child at all, the published minimum", Vec::new(), true),
        ("one child", vec![children[0].clone()], true),
        ("two distinct children", children.clone(), true),
        (
            // One past the maximum, as repeats of one child: the length is judged
            // before the list's contents are, so this row is refused for its
            // length. (A list of repeats INSIDE the bound is refused too, by the
            // uniqueness rule the test below asserts.)
            "one past the published maximum",
            vec![children[0].clone(); MAX_CHILD_HEALTH_CHECKS + 1],
            false,
        ),
    ];

    for (index, (case, list, accepted)) in rows.into_iter().enumerate() {
        let before = crate::health_checks::health_check_count(&router, case).await;
        let created =
            create_health_check(&router, &format!("parent-{index}"), &calculated(&list)).await;
        if accepted {
            assert_eq!(
                created.status, 201,
                "case {case:?}: expected 201 from CreateHealthCheck, got {} with body {}",
                created.status, created.body
            );
            let id = crate::health_checks::health_check_id(&created, case);
            // Read back through the reader, so the list the service stored is the
            // one it reports, and the check is addressable afterwards.
            let read = get_health_check(&router, &id).await;
            let stored = read.all("HealthCheckConfig/ChildHealthChecks/ChildHealthCheck");
            assert_eq!(
                stored.len(),
                list.len(),
                "case {case:?}: expected the {} entries the request named, got {stored:?}",
                list.len()
            );
        } else {
            assert_eq!(
                created.status, 400,
                "case {case:?}: expected 400, got {} with body {}",
                created.status, created.body
            );
            assert_eq!(
                created.error_code(),
                INVALID_INPUT,
                "case {case:?}: expected {INVALID_INPUT}, got {:?}; body {}",
                created.error_code(),
                created.body
            );
            let count = crate::health_checks::health_check_count(&router, case).await;
            assert_eq!(
                count, before,
                "case {case:?}: a refused create must persist nothing, got \
                 HealthCheckCount {count} against {before}"
            );
        }
    }
}

/// One child list that names a child twice: how it is built out of the ids of the
/// children the account holds.
struct RepeatRow {
    case: &'static str,
    /// The list the request names, as indices into the children created for the
    /// test — so a row says WHERE the repeat sits and how often it occurs.
    indices: &'static [usize],
}

/// The lists that name a child more than once. The rows differ along where the
/// repeat sits (the whole list, its start, its middle, its end), how often one id
/// is repeated (twice, three times, many times), how many distinct children sit
/// beside it, and the length of the list the repeat is in.
const REPEAT_ROWS: &[RepeatRow] = &[
    RepeatRow {
        case: "one child named twice and nothing else",
        indices: &[0, 0],
    },
    RepeatRow {
        case: "one child named three times",
        indices: &[1, 1, 1],
    },
    RepeatRow {
        case: "a repeat at the start of a longer list",
        indices: &[2, 2, 0, 1, 3],
    },
    RepeatRow {
        case: "a repeat in the middle of a longer list",
        indices: &[0, 1, 2, 1, 3],
    },
    RepeatRow {
        case: "a repeat at the end of a longer list",
        indices: &[0, 1, 2, 3, 0],
    },
    RepeatRow {
        case: "two different children each named twice",
        indices: &[0, 1, 0, 1],
    },
    RepeatRow {
        case: "the same child named at both ends of the longest distinct list",
        indices: &[3, 0, 1, 2, 3],
    },
];

/// Assert that `reply` is the duplicate-child refusal: HTTP 400, `InvalidInput` and
/// the one fixed sentence, carrying nothing the request named.
fn assert_duplicate_children_refused(
    reply: &crate::hosted_zone_records::Reply,
    case: &str,
    named: &[String],
) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected HTTP 400, got {} with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_code(),
        INVALID_INPUT,
        "case {case:?}: expected {INVALID_INPUT}, got {:?}; body {}",
        reply.error_code(),
        reply.body
    );
    let got = message_of(reply, case);
    assert_eq!(
        got, DUPLICATE_CHILDREN_MESSAGE,
        "case {case:?}: expected message {DUPLICATE_CHILDREN_MESSAGE:?}, got {got:?}"
    );
    // The sentence is entirely fixed text: no id the request named reaches it.
    for id in named {
        assert!(
            !got.contains(id.as_str()),
            "case {case:?}: the sentence must carry nothing the request named, \
             got {got:?} naming {id}"
        );
    }
}

#[tokio::test]
async fn a_create_naming_one_child_twice_is_refused_and_persists_nothing() {
    let router = router();
    // Four children, so a row can name up to four distinct ids beside its repeat.
    let held = children(&router, "repeat-create", 4).await;

    for (index, row) in REPEAT_ROWS.iter().enumerate() {
        let list: Vec<String> = row.indices.iter().map(|at| held[*at].clone()).collect();
        let before = crate::health_checks::health_check_count(&router, row.case).await;
        let refused = create_health_check(
            &router,
            &format!("repeat-create-parent-{index}"),
            &calculated(&list),
        )
        .await;
        assert_duplicate_children_refused(&refused, row.case, &list);
        let count = crate::health_checks::health_check_count(&router, row.case).await;
        assert_eq!(
            count, before,
            "case {:?}: a refused create must persist nothing, got HealthCheckCount \
             {count} against {before}",
            row.case
        );
    }

    // The control: the same rows with the repeat taken out are created, so what the
    // refusals above are about is the repeat and nothing else about the list.
    for (index, row) in REPEAT_ROWS.iter().enumerate() {
        let mut distinct: Vec<usize> = Vec::new();
        for at in row.indices {
            if !distinct.contains(at) {
                distinct.push(*at);
            }
        }
        let list: Vec<String> = distinct.iter().map(|at| held[*at].clone()).collect();
        let created = create_health_check(
            &router,
            &format!("distinct-create-parent-{index}"),
            &calculated(&list),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {:?} with the repeat removed: expected 201, got {} with body {}",
            row.case, created.status, created.body
        );
        let id = crate::health_checks::health_check_id(&created, row.case);
        let read = get_health_check(&router, &id).await;
        let stored = read.all("HealthCheckConfig/ChildHealthChecks/ChildHealthCheck");
        assert_eq!(
            stored.len(),
            list.len(),
            "case {:?} with the repeat removed: expected the {} entries the request named, \
             got {stored:?}",
            row.case,
            list.len()
        );
    }
}

#[tokio::test]
async fn an_update_naming_one_child_twice_is_refused_and_changes_nothing() {
    let router = router();
    let held = children(&router, "repeat-update", 4).await;

    // The health check the updates are asked of: a CALCULATED one holding a single
    // child, so every refusal below has a stored list to leave alone.
    let created = create_health_check(
        &router,
        "repeat-update-parent",
        &calculated(&held[0..1]),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "the parent must be created, got {} with body {}",
        created.status, created.body
    );
    let parent = crate::health_checks::health_check_id(&created, "the parent");
    let version_before = read_version(&router, &parent).await;

    for row in REPEAT_ROWS {
        let list: Vec<String> = row.indices.iter().map(|at| held[*at].clone()).collect();
        let refused = update_health_check(&router, &parent, &child_list(&list)).await;
        assert_duplicate_children_refused(&refused, row.case, &list);

        // The stored configuration is untouched, and so is the counter an update
        // that WAS applied would have moved.
        let read = get_health_check(&router, &parent).await;
        let stored = read.all("HealthCheckConfig/ChildHealthChecks/ChildHealthCheck");
        assert_eq!(
            stored,
            vec![held[0].as_str()],
            "case {:?}: a refused update must leave the stored child list alone, got {stored:?}",
            row.case
        );
        let version = read_version(&router, &parent).await;
        assert_eq!(
            version, version_before,
            "case {:?}: a refused update must not move HealthCheckVersion, got {version} \
             against {version_before}",
            row.case
        );
    }

    // The control: an update naming the same children WITHOUT a repeat is applied,
    // so the refusals above are about the repeat alone.
    let distinct: Vec<String> = held.clone();
    let applied = update_health_check(&router, &parent, &child_list(&distinct)).await;
    assert_eq!(
        applied.status, 200,
        "an update naming distinct children must be applied, got {} with body {}",
        applied.status, applied.body
    );
    let read = get_health_check(&router, &parent).await;
    let stored = read.all("HealthCheckConfig/ChildHealthChecks/ChildHealthCheck");
    assert_eq!(
        stored.len(),
        distinct.len(),
        "the applied update must store the {} children it named, got {stored:?}",
        distinct.len()
    );
}

/// The `HealthCheckVersion` the reader reports for one health check.
async fn read_version(router: &MultiServiceRouter, id: &str) -> String {
    let read = get_health_check(router, id).await;
    read.first("HealthCheck/HealthCheckVersion")
        .unwrap_or_else(|| {
            panic!(
                "the read must report HealthCheckVersion; body {}",
                read.body
            )
        })
        .to_string()
}
