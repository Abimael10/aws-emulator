// SPDX-License-Identifier: CC0-1.0

//! `DeleteFramework` on an account that holds MORE THAN ONE audit framework.
//!
//! The rule under test: a framework-scoped delete removes exactly the framework
//! whose name the request carries and nothing else the account holds — every
//! other framework stays readable member for member, keeps the ARN its own
//! create minted and keeps its place in the listing — so an account's
//! frameworks are emptied one delete at a time, and the name the delete took is
//! the only one the account stops holding.
//!
//! The sibling suites pin the single-framework cases (the refusal while the
//! deployment is in flight, the empty 200 once it is terminal, the refusal of a
//! name the account holds nothing under, and the freed name). What is pinned
//! here is the SCOPE of the removal, which only an account holding several
//! frameworks can show.
//!
//! The rows of each table differ along what the rule generalises over: how many
//! frameworks the account holds, where in name order the deleted one sits
//! (first, middle, last), how many controls each framework carries and which
//! ones, whether a framework has a description, and the length of each name —
//! including the ends of the length the model admits for `FrameworkName`.
//!
//! Every case drives real HTTP through the router. The request harness is the
//! shared one in `vault_and_backup_job.rs`, the framework helpers are the ones
//! in `framework.rs`, the delete is `framework_refusals.rs`'s and the wait for a
//! settled deployment is `framework_deletion.rs`'s poll, so there is ONE wire
//! path for this service's suites and nothing here waits for a fixed time.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_backup_cryocontrollerusermanager::state::SUPPORTED_FRAMEWORK_CONTROL_NAMES;

use crate::framework::{
    create_framework, describe_framework, framework_name_of_length, text,
    MAX_FRAMEWORK_NAME_LENGTH,
};
use crate::framework_deletion::settled_framework;
use crate::framework_refusals::delete_framework;
use crate::vault_and_backup_job::{call, error_code, router, ACCOUNT};

/// One framework a row seeds: the name it is created under, where in the
/// published control catalog its controls start, how many controls it carries
/// and its description when it has one.
struct Seed {
    name: String,
    control_offset: usize,
    controls: usize,
    description: Option<&'static str>,
}

/// How AWS words "the framework asked about is not one this account holds": the
/// fixed text around the name the call addressed and the account it was made in.
fn not_found_message(name: &str) -> String {
    format!("Framework {name} not found in account {ACCOUNT}.")
}

/// The create request a [`Seed`] stands for.
///
/// The control names are taken from the service's published catalog
/// (`SUPPORTED_FRAMEWORK_CONTROL_NAMES`, the Backup Audit Manager control
/// identifiers), starting at the seed's offset, so the frameworks of one row
/// carry DIFFERENT control sets and no framework repeats a control name.
fn create_request(seed: &Seed) -> Value {
    let controls: Vec<Value> = (0..seed.controls)
        .map(|index| {
            let control =
                SUPPORTED_FRAMEWORK_CONTROL_NAMES[(seed.control_offset + index)
                    % SUPPORTED_FRAMEWORK_CONTROL_NAMES.len()];
            json!({"ControlName": control})
        })
        .collect();
    let mut request = json!({"FrameworkName": seed.name, "FrameworkControls": controls});
    if let Some(description) = seed.description {
        request["FrameworkDescription"] = json!(description);
    }
    request
}

fn seed(
    name: impl Into<String>,
    control_offset: usize,
    controls: usize,
    description: Option<&'static str>,
) -> Seed {
    Seed {
        name: name.into(),
        control_offset,
        controls,
        description,
    }
}

/// The `FrameworkName` of every entry of a listing, in the order it lists them.
fn listed_names(listing: &Value) -> Vec<String> {
    listing
        .get("Frameworks")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| text(entry, "FrameworkName"))
                .collect()
        })
        .unwrap_or_default()
}

/// `GET /audit/frameworks` — `ListFrameworks`'s HTTP binding, with no paging
/// query, so the answer is the whole set the account holds.
async fn list_all(router: &MultiServiceRouter) -> (u16, Vec<String>) {
    let (status, listing) = call(router, "GET", "/audit/frameworks", None).await;
    (status, listed_names(&listing))
}

/// Create every framework a row asks for, then wait for each deployment to be
/// reported terminal, and return each framework's settled read keyed by name in
/// `FrameworkName` order — the order a listing reports.
async fn seeded_and_settled(
    router: &MultiServiceRouter,
    case: &str,
    seeds: &[Seed],
) -> Vec<(String, Value)> {
    for seed in seeds {
        let request = create_request(seed);
        let (status, created) = create_framework(router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateFramework 200 for {request:?}, got {status} {created:?}"
        );
    }
    let mut settled: Vec<(String, Value)> = Vec::new();
    for seed in seeds {
        let read = settled_framework(router, &seed.name).await;
        settled.push((seed.name.clone(), read));
    }
    settled.sort_by(|left, right| left.0.cmp(&right.0));
    settled
}

/// The rows both tables walk: the account's frameworks and, for the scope table,
/// which of them the delete names.
fn deletion_scope_cases() -> Vec<(&'static str, Vec<Seed>, String)> {
    let shortest = framework_name_of_length(1);
    let longest = framework_name_of_length(MAX_FRAMEWORK_NAME_LENGTH);
    let mid_length = framework_name_of_length(64);
    vec![
        (
            "two frameworks, the one that sorts first deleted",
            vec![
                seed("AlphaFramework", 0, 1, None),
                seed("ZuluFramework", 3, 2, Some("kept beside the deleted one")),
            ],
            "AlphaFramework".to_string(),
        ),
        (
            "three frameworks, the one that sorts in the middle deleted",
            vec![
                seed("AuditEarly", 1, 2, None),
                seed("AuditMiddle", 4, 3, Some("the one this row removes")),
                seed("AuditLate", 8, 1, None),
            ],
            "AuditMiddle".to_string(),
        ),
        (
            "three frameworks created in the reverse of name order, the one that sorts last deleted",
            vec![
                seed("OrderedThird", 2, 1, Some("created first, sorts last")),
                seed("OrderedSecond", 5, 2, None),
                seed("OrderedFirst", 7, 3, None),
            ],
            "OrderedThird".to_string(),
        ),
        (
            "frameworks at both ends of the admitted name length, the longest name deleted",
            vec![
                seed(shortest, 6, 1, None),
                seed(mid_length, 9, 2, Some("a mid-length name")),
                seed(longest.clone(), 0, 4, None),
            ],
            longest,
        ),
    ]
}

/// A delete removes exactly the framework it names: the named one stops being
/// held, every other framework the account holds reads back exactly as it did
/// before the delete, and the listing reports precisely the survivors in name
/// order.
#[tokio::test]
async fn a_delete_removes_only_the_framework_it_names() {
    for (case, seeds, deleted) in deletion_scope_cases() {
        // One router per row: each row's account holds only its own frameworks.
        let router = router();
        let before = seeded_and_settled(&router, case, &seeds).await;
        let survivors: Vec<String> = before
            .iter()
            .map(|(name, _)| name.clone())
            .filter(|name| name != &deleted)
            .collect();
        let (status, listed) = list_all(&router).await;
        assert_eq!(
            (status, listed.len()),
            (200, seeds.len()),
            "case {case:?}: expected every seeded framework to be listed, got {status} {listed:?}"
        );

        let (status, errortype, body) = delete_framework(&router, &deleted).await;
        assert_eq!(
            (status, errortype.as_str()),
            (200, "<unset>"),
            "case {case:?}: expected DeleteFramework 200 for {deleted:?}, \
             got {status} {errortype} {body:?}"
        );
        assert!(
            body.is_null() || body.as_object().is_some_and(serde_json::Map::is_empty),
            "case {case:?}: DeleteFramework answers an empty body, got {body:?}"
        );

        // The name the delete carried is one the account holds nothing under.
        let (status, missing) = describe_framework(&router, &deleted).await;
        assert_eq!(
            (
                status,
                error_code(&missing).as_str(),
                text(&missing, "Message").as_str()
            ),
            (
                400,
                "ResourceNotFoundException",
                not_found_message(&deleted).as_str()
            ),
            "case {case:?}: the deleted framework must be unreadable, got {status} {missing:?}"
        );

        // Every OTHER framework is untouched, member for member.
        for (name, settled) in &before {
            if name == &deleted {
                continue;
            }
            let (status, read) = describe_framework(&router, name).await;
            assert_eq!(
                (status, &read),
                (200, settled),
                "case {case:?}: deleting {deleted:?} must leave {name:?} exactly as it was, \
                 got {status} {read:?}"
            );
        }

        let (status, listed) = list_all(&router).await;
        assert_eq!(
            (status, &listed),
            (200, &survivors),
            "case {case:?}: after deleting {deleted:?} the listing must report the survivors \
             in name order, got {status} {listed:?}"
        );
    }
}

/// The account is emptied one delete at a time: each delete takes exactly one
/// name out of the listing, the frameworks not yet named still read back, and
/// once the last one is deleted the account holds none.
#[tokio::test]
async fn the_account_is_emptied_one_delete_at_a_time() {
    for (case, seeds, _) in deletion_scope_cases() {
        let router = router();
        let before = seeded_and_settled(&router, case, &seeds).await;

        // Deleted in the order the row CREATED them, which for one row is the
        // reverse of name order, so the listing's order is not the delete order.
        let mut remaining: Vec<String> = before.iter().map(|(name, _)| name.clone()).collect();
        for seed in &seeds {
            let (status, errortype, body) = delete_framework(&router, &seed.name).await;
            assert_eq!(
                (status, errortype.as_str()),
                (200, "<unset>"),
                "case {case:?}: expected DeleteFramework 200 for {:?}, \
                 got {status} {errortype} {body:?}",
                seed.name
            );
            remaining.retain(|name| name != &seed.name);

            let (status, listed) = list_all(&router).await;
            assert_eq!(
                (status, &listed),
                (200, &remaining),
                "case {case:?}: after deleting {:?} the listing must report only {remaining:?}, \
                 got {status} {listed:?}",
                seed.name
            );
            for (name, settled) in &before {
                if !remaining.contains(name) {
                    continue;
                }
                let (status, read) = describe_framework(&router, name).await;
                assert_eq!(
                    (status, &read),
                    (200, settled),
                    "case {case:?}: {name:?} must survive the delete of {:?}, \
                     got {status} {read:?}",
                    seed.name
                );
            }
        }

        let (status, listed) = list_all(&router).await;
        assert!(
            status == 200 && listed.is_empty(),
            "case {case:?}: every framework was deleted, so the account holds none, \
             got {status} {listed:?}"
        );
    }
}
