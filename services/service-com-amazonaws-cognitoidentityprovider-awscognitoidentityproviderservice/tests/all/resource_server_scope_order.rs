// SPDX-License-Identifier: CC0-1.0

//! The ORDER a resource server serves its custom scopes in, over the wire, driven exactly
//! as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test: a resource server's custom scopes are a collection KEYED by the
//! scope name — "a list of custom scopes[.] Each scope is a key-value map with the keys
//! `ScopeName` and `ScopeDescription`", and one scope is addressed as
//! `resourceServerIdentifier/ScopeName`
//! (`smithy_query(query="doc",shape="CreateResourceServerRequest",member="Scopes")` and
//! `smithy_query(query="doc",shape="ResourceServerScopeType",member="ScopeName")`) — so the
//! resource server holds them in a table keyed on that name and serves them in the order
//! that table is walked, not in the sequence a declaration wrote: ascending bucket of the
//! name's own string hash at the table's width, two names of one bucket in name order. The
//! declaration decides WHICH scopes are held and what each describes; the order follows the
//! names and their count alone, so every rendering of one resource server — the create's
//! response, the update's, a describe and a listing — states the one order, and declaring a
//! set in another sequence moves nothing.
//!
//! No expectation here is computed by the code under test. Each row carries the held order
//! as a LITERAL, with the bucket of every scope name written beside it:
//!
//! * the hash is the 31-polynomial over the name's UTF-16 code units, with its high half
//!   folded into its low half, reduced to the table's width — so a ONE-character name sits
//!   in the bucket its own code point's low bits name, which is what the single-character
//!   rows below are for;
//! * the table starts at 16 buckets and doubles once more than three quarters of them are
//!   filled, so a row of at most 12 scopes is walked at 16 buckets and a row of 13 at 32 —
//!   the width is observable, and the 13-scope row below pins it;
//! * the three names the recordings carry — `read`, `write`, `admin` — fall in buckets 3,
//!   13 and 9 of a 16-bucket table, so the recorded service serves them `read`, `admin`,
//!   `write` however they were declared.
//!
//! The harness (the engine, the router, the awsJson1_1 call) is the one
//! `log_delivery_configuration.rs` builds for this service, reached through the test crate
//! root rather than copied.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::log_delivery_configuration::{ok, pool, router};

/// `scopes` — `(ScopeName, ScopeDescription)` pairs — as the wire carries a resource
/// server's scope list, in the order they are given here.
fn scope_list_json(scopes: &[(String, String)]) -> Value {
    Value::Array(
        scopes
            .iter()
            .map(|(scope_name, scope_description)| {
                json!({
                    "ScopeName": scope_name,
                    "ScopeDescription": scope_description,
                })
            })
            .collect(),
    )
}

/// A scope list from names alone: each scope's description is its name's own sentence, so a
/// row's descriptions travel with their names and a swap of two scopes is visible in both
/// members.
fn scopes(names: &[&str]) -> Vec<(String, String)> {
    names
        .iter()
        .map(|name| ((*name).to_string(), format!("The {name} scope")))
        .collect()
}

/// The sequences one row declares its scope set in: the set as the row writes it, that
/// sequence reversed, and that sequence rotated by one — three bounded sequences per row,
/// which is enough to show the served order follows no declaration.
fn declared_sequences(held: &[(String, String)]) -> Vec<Vec<(String, String)>> {
    let mut reversed = held.to_vec();
    reversed.reverse();
    let mut rotated = held.to_vec();
    if !rotated.is_empty() {
        let head = rotated.remove(0);
        rotated.push(head);
    }
    let mut sequences: Vec<Vec<(String, String)>> = Vec::new();
    for sequence in [held.to_vec(), reversed, rotated] {
        // A set of fewer than two scopes has one sequence, so the row drives it once.
        if !sequences.contains(&sequence) {
            sequences.push(sequence);
        }
    }
    sequences
}

/// One row: a scope set, and the order the resource server holds it in.
struct ScopeOrderCase {
    case: &'static str,
    /// The scope names in the order the resource server HOLDS them, written out here.
    held: Vec<&'static str>,
}

/// The rows, each with the bucket its names fall in written out beside it.
fn scope_order_cases() -> Vec<ScopeOrderCase> {
    vec![
        ScopeOrderCase {
            case: "no scopes at all",
            held: Vec::new(),
        },
        ScopeOrderCase {
            case: "one scope, which is its own order",
            held: vec!["forecast.read"],
        },
        ScopeOrderCase {
            // `read` 3, `admin` 9, `write` 13 — the three names the recordings carry.
            case: "the three names the recordings carry",
            held: vec!["read", "admin", "write"],
        },
        ScopeOrderCase {
            // One-character names, each in the bucket its code point's low four bits name:
            // `p` 112 -> 0, `a` 97 -> 1, `b` 98 -> 2, `c` 99 -> 3.
            case: "one-character names in four different buckets",
            held: vec!["p", "a", "b", "c"],
        },
        ScopeOrderCase {
            // Three names of ONE bucket — `A` 65, `a` 97 and `q` 113 all fall in bucket 1
            // of a 16-bucket table — so the row pins the tie: a bucket's names are served
            // in name order, `A` before `a` before `q`.
            case: "three names colliding in one bucket, served in name order",
            held: vec!["A", "a", "q"],
        },
        ScopeOrderCase {
            // 13 scopes fill more than three quarters of a 16-bucket table, so the table
            // this row is walked at is 32 buckets wide: `a`..`l` fall in buckets 1..12 at
            // either width, while `q` 113 falls in bucket 1 of a 16-bucket table and in
            // bucket 17 of a 32-bucket one — so `q` is served LAST, after `l`, rather than
            // beside `a`.
            case: "13 scopes, past three quarters of the starting table",
            held: vec![
                "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "q",
            ],
        },
    ]
}

/// The resource server `identifier` names, as `DescribeResourceServer` renders it.
async fn described(router: &MultiServiceRouter, user_pool_id: &str, identifier: &str) -> Value {
    ok(
        router,
        "DescribeResourceServer",
        &json!({ "UserPoolId": user_pool_id, "Identifier": identifier }),
    )
    .await["ResourceServer"]
        .clone()
}

/// The resource server `identifier` names, as `ListResourceServers` serves it.
async fn listed(router: &MultiServiceRouter, user_pool_id: &str, identifier: &str) -> Value {
    let listed = ok(
        router,
        "ListResourceServers",
        // The published `MaxResults` maximum is 50 and no test here stores more than a
        // handful of resource servers, so one page serves the whole listing.
        &json!({ "UserPoolId": user_pool_id, "MaxResults": 50 }),
    )
    .await;
    listed["ResourceServers"]
        .as_array()
        .unwrap_or_else(|| panic!("ResourceServers must be a present list, got {listed}"))
        .iter()
        .find(|entry| entry["Identifier"] == json!(identifier))
        .unwrap_or_else(|| panic!("the listing must serve {identifier:?}, got {listed}"))
        .clone()
}

#[tokio::test]
async fn a_resource_server_serves_its_scopes_in_the_order_it_holds_them_however_they_were_declared()
{
    let router = router();
    let user_pool_id = pool(&router, "resource server scope order", None).await;

    for (row, case) in scope_order_cases().into_iter().enumerate() {
        let label = case.case;
        // The expectation: the row's own held order, written out above.
        let held = scope_list_json(&scopes(&case.held));

        for (at, declared) in declared_sequences(&scopes(&case.held))
            .into_iter()
            .enumerate()
        {
            let names: Vec<&str> = declared.iter().map(|(name, _)| name.as_str()).collect();
            let label = format!("{label}, declared as {names:?}");
            // One resource server per declared sequence, so each sequence's reads are its
            // own.
            let identifier = format!("https://scope-order-{row}-{at}.example.test");
            let created = ok(
                &router,
                "CreateResourceServer",
                &json!({
                    "UserPoolId": user_pool_id,
                    "Identifier": identifier,
                    "Name": "scope order",
                    "Scopes": scope_list_json(&declared),
                }),
            )
            .await["ResourceServer"]
                .clone();
            assert_eq!(
                created["Scopes"], held,
                "case {label:?}: the create must serve the held order {held}, got {created}"
            );

            // Every rendering of the stored resource server states it, and a read changes
            // nothing for the next one.
            let read = described(&router, &user_pool_id, &identifier).await;
            assert_eq!(
                read["Scopes"], held,
                "case {label:?}: the read must state the held order {held}, got {read}"
            );
            let again = described(&router, &user_pool_id, &identifier).await;
            assert_eq!(
                again, read,
                "case {label:?}: a read must not change what the next read sees, got {again}"
            );
            assert_eq!(
                listed(&router, &user_pool_id, &identifier).await,
                read,
                "case {label:?}: the listing must serve what the read renders"
            );

            // And an update RE-DECLARING the same set in the reverse sequence holds it the
            // same way: the order is the names', not the declaration's.
            let mut redeclared = declared.clone();
            redeclared.reverse();
            let updated = ok(
                &router,
                "UpdateResourceServer",
                &json!({
                    "UserPoolId": user_pool_id,
                    "Identifier": identifier,
                    "Name": "scope order, updated",
                    "Scopes": scope_list_json(&redeclared),
                }),
            )
            .await["ResourceServer"]
                .clone();
            assert_eq!(
                updated["Scopes"], held,
                "case {label:?}: the update must serve the held order {held}, got {updated}"
            );
            assert_eq!(
                described(&router, &user_pool_id, &identifier).await,
                updated,
                "case {label:?}: the read after the update must render what it stored"
            );
            assert_eq!(
                listed(&router, &user_pool_id, &identifier).await,
                updated,
                "case {label:?}: the listing after the update must serve what it stored"
            );
        }
    }
}

/// `(ScopeName, ScopeDescription)` pairs as a scope list.
fn pairs(scopes: &[(&str, &str)]) -> Vec<(String, String)> {
    scopes
        .iter()
        .map(|(scope_name, scope_description)| {
            ((*scope_name).to_string(), (*scope_description).to_string())
        })
        .collect()
}

/// One row of the declaration table: the case, the scopes the declaration carries, and the
/// scope list the resource server is left holding, in the order it holds them.
type DeclarationRow = (&'static str, Vec<(String, String)>, Vec<(String, String)>);

#[tokio::test]
async fn a_declaration_decides_which_scopes_are_held_and_what_each_one_describes() {
    let router = router();
    let user_pool_id = pool(&router, "resource server scope content", None).await;
    let identifier = "https://orders.example.test";

    // The held order is the names'; the DESCRIPTIONS are the declaration's, and a name the
    // declaration leaves out is no longer held — so each row states the whole scope list
    // the row's declaration leaves, in held order.
    //
    // Buckets of a 16-bucket table: `read` 3, `admin` 9, `write` 13, `p` 0, `a` 1.
    let rows: [DeclarationRow; 5] = [
        (
            "three scopes declared write, read, admin",
            pairs(&[
                ("write", "Write access"),
                ("read", "Read access"),
                ("admin", "Admin access"),
            ]),
            pairs(&[
                ("read", "Read access"),
                ("admin", "Admin access"),
                ("write", "Write access"),
            ]),
        ),
        (
            "the same names with new descriptions",
            pairs(&[
                ("write", "Write, revised"),
                ("read", "Read, revised"),
                ("admin", "Administer, revised"),
            ]),
            pairs(&[
                ("read", "Read, revised"),
                ("admin", "Administer, revised"),
                ("write", "Write, revised"),
            ]),
        ),
        (
            "a declaration that drops two of the three",
            pairs(&[("write", "Write, alone")]),
            pairs(&[("write", "Write, alone")]),
        ),
        (
            "a declaration of a set that shares no name with the held one",
            pairs(&[("a", "The a scope"), ("p", "The p scope")]),
            pairs(&[("p", "The p scope"), ("a", "The a scope")]),
        ),
        (
            "an empty declaration, the modelled length minimum 0",
            Vec::new(),
            Vec::new(),
        ),
    ];

    ok(
        &router,
        "CreateResourceServer",
        &json!({
            "UserPoolId": user_pool_id,
            "Identifier": identifier,
            "Name": "orders",
            "Scopes": [],
        }),
    )
    .await;

    for (case, declared, expected) in rows {
        let expected = scope_list_json(&expected);
        let updated = ok(
            &router,
            "UpdateResourceServer",
            &json!({
                "UserPoolId": user_pool_id,
                "Identifier": identifier,
                "Name": "orders",
                "Scopes": scope_list_json(&declared),
            }),
        )
        .await["ResourceServer"]
            .clone();
        assert_eq!(
            updated["Scopes"], expected,
            "case {case:?}: expected the scopes {expected}, got {updated}"
        );
        let read = described(&router, &user_pool_id, identifier).await;
        assert_eq!(
            read["Scopes"], expected,
            "case {case:?}: the read must render what the update stored, got {read}"
        );
    }
}
