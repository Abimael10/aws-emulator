// SPDX-License-Identifier: CC0-1.0

//! Which attributes `SearchUsers` and `SearchGroups` will search by, end to end over HTTP.
//!
//! The rule under test: a directory matches a search against an attribute it INDEXES for
//! search, and the service's attribute list says which of a user's or a group's attributes
//! those are. An attribute of the object that the list does NOT mark searchable is not a
//! search that matches nothing — it is a request the directory refuses, with HTTP 400,
//! `ValidationException`, the modelled `Reason` `INVALID_ATTRIBUTE_FOR_SEARCH`
//! (`smithy_query(query="shape", shape="ValidationExceptionReason")`) and a sentence naming
//! the attribute the request asked for.
//!
//! The rows span both search operations, every case the name can be spelled in, where the
//! offender sits among searchable names, whether the directory holds a matching object at
//! all, and the searchable attributes that keep answering — and a refused search leaves the
//! directory exactly as it stood, because it is refused before any record is read.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::validation_envelope::{
    call, emulator, Answer, DIRECTORY, UNUSED_DIRECTORY, VALIDATION_EXCEPTION,
};

/// A `DirectoryId` this account holds and nothing is ever written to, for the cases about a
/// refusal that precedes reading any record.
const EMPTY_DIRECTORY: &str = UNUSED_DIRECTORY;

/// The `ValidationExceptionReason` a search names an unsearchable attribute with
/// (`smithy_query(query="shape", shape="ValidationExceptionReason")` holds it), spelled out
/// here rather than imported so a wrong value in the implementation fails.
const INVALID_ATTRIBUTE_FOR_SEARCH: &str = "INVALID_ATTRIBUTE_FOR_SEARCH";

/// The attribute of a user and of a group that the service's attribute list holds and marks
/// `Searchable: No`: the object's common name, which carries the account name over again.
const UNSEARCHABLE: &str = "cn";

/// The account name of the user and of the group the cases below search for.
const USER: &str = "jdoe";
const GROUP: &str = "engineering";

/// The sentence a refused search states: fixed text around the attribute the request named,
/// quoted as the request spelled it.
fn cannot_be_searched(attribute: &str) -> String {
    format!("Attribute '{attribute}' cannot be searched.")
}

/// One search request.
fn search(string: &str, attributes: &[&str]) -> Value {
    json!({"SearchString": string, "SearchAttributes": attributes})
}

/// The two search routes, each with the route that lists the objects it searches and the
/// member the answer carries them under — so every rule below is asserted on BOTH.
fn search_routes() -> [(&'static str, &'static str, &'static str, &'static str); 2] {
    [
        ("/Users/SearchUsers", "/Users/ListUsers", "Users", USER),
        (
            "/Groups/SearchGroups",
            "/Groups/ListGroups",
            "Groups",
            GROUP,
        ),
    ]
}

/// One user and one group in `DIRECTORY`, so a search that is accepted has something to
/// match and a search that is refused has something it could have matched.
async fn populate(router: &MultiServiceRouter) {
    for (route, body) in [
        ("/Users/CreateUser", json!({"SAMAccountName": USER})),
        ("/Groups/CreateGroup", json!({"SAMAccountName": GROUP})),
    ] {
        let created = call(router, route, DIRECTORY, &body).await;
        assert_eq!(
            created.status, 200,
            "case {route:?}: the create must be accepted, got {} with {:?}",
            created.status, created.body
        );
    }
}

/// The objects a listing reports for `directory`, in the order it reports them.
async fn inventory(
    router: &MultiServiceRouter,
    route: &str,
    member: &str,
    directory: &str,
) -> Vec<Value> {
    let listed = call(router, route, directory, &json!({"MaxResults": 250})).await;
    assert_eq!(
        listed.status, 200,
        "case {route:?}: the listing must be accepted, got {} with {:?}",
        listed.status, listed.body
    );
    listed.body[member].as_array().cloned().unwrap_or_default()
}

/// Every part of the refusal, asserted once for whichever case produced it.
fn assert_refused(case: &str, refused: &Answer, attribute: &str) {
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected HTTP 400, got {} with {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code(),
        VALIDATION_EXCEPTION,
        "case {case:?}: expected the modelled ValidationException, got {:?}",
        refused.body
    );
    assert_eq!(
        refused.error_type, VALIDATION_EXCEPTION,
        "case {case:?}: the header must carry the same error type, got {:?}",
        refused.error_type
    );
    assert_eq!(
        refused.body.get("Reason").and_then(Value::as_str),
        Some(INVALID_ATTRIBUTE_FOR_SEARCH),
        "case {case:?}: expected Reason {INVALID_ATTRIBUTE_FOR_SEARCH}, got {:?}",
        refused.body
    );
    assert_eq!(
        refused.message(),
        Some(cannot_be_searched(attribute).as_str()),
        "case {case:?}: expected the sentence naming {attribute:?}, got {:?}",
        refused.message()
    );
    assert!(
        refused.body.get("Users").is_none() && refused.body.get("Groups").is_none(),
        "case {case:?}: a refused search reports no object, got {:?}",
        refused.body
    );
}

/// RULE: naming an attribute the directory does not index for search refuses the search,
/// whatever case the name is written in and whichever searchable names it sits among.
///
/// The rows differ along the case of the attribute name, how many names the request carries
/// and where the offender sits among them, and whether the directory holds an object the
/// search would otherwise have matched — and every row is sent to both search operations,
/// which share the rule.
#[tokio::test]
async fn an_unsearchable_attribute_refuses_the_search() {
    let router = emulator();
    populate(&router).await;

    // (case, the attributes the request names, the directory it searches, the name the
    // refusal must quote)
    let rows: [(&str, &[&str], &str, &str); 7] = [
        (
            "the name as the attribute list spells it",
            &["cn"],
            DIRECTORY,
            "cn",
        ),
        ("the same name capitalised", &["Cn"], DIRECTORY, "Cn"),
        ("the same name in capitals", &["CN"], DIRECTORY, "CN"),
        ("the same name, mixed case", &["cN"], DIRECTORY, "cN"),
        (
            "the offender after a searchable name",
            &["SAMAccountName", "Cn"],
            DIRECTORY,
            "Cn",
        ),
        (
            "the offender before several searchable names",
            &["Cn", "SAMAccountName", "Description"],
            DIRECTORY,
            "Cn",
        ),
        (
            "a directory holding no object the search could match",
            &["Cn"],
            EMPTY_DIRECTORY,
            "Cn",
        ),
    ];

    for (route, _, _, name) in search_routes() {
        for (what, attributes, directory, quoted) in rows {
            let case = format!("{route} naming {attributes:?} in {directory}: {what}");
            let refused = call(&router, route, directory, &search(name, attributes)).await;
            assert_refused(&case, &refused, quoted);
        }
    }
}

/// RULE, the other side: an attribute the directory does search is answered, so the refusal
/// above is about the attribute and not about the search path.
///
/// The rows are the searchable attributes of each object: the account name under the model's
/// member name and under the directory's own spelling, and — for a user — the principal name
/// the directory derives, which is a name no create ever sent.
#[tokio::test]
async fn a_searchable_attribute_is_still_answered() {
    let router = emulator();
    populate(&router).await;

    // (route, the member the answer carries, the attributes to search, the string to search)
    let rows: [(&str, &str, &[&str], &str); 5] = [
        ("/Users/SearchUsers", "Users", &["SAMAccountName"], USER),
        ("/Users/SearchUsers", "Users", &["samaccountname"], USER),
        (
            "/Users/SearchUsers",
            "Users",
            &["SAMAccountName", "UserPrincipalName"],
            USER,
        ),
        ("/Groups/SearchGroups", "Groups", &["SAMAccountName"], GROUP),
        ("/Groups/SearchGroups", "Groups", &["GroupType"], "Security"),
    ];

    for (route, member, attributes, string) in rows {
        let case = format!("{route} searching {string:?} over {attributes:?}");
        let answer = call(&router, route, DIRECTORY, &search(string, attributes)).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: a searchable attribute must be answered, got {} with {:?}",
            answer.status, answer.body
        );
        let matched = answer.body[member].as_array().cloned().unwrap_or_default();
        assert!(
            !matched.is_empty(),
            "case {case:?}: the search must report the object holding that value, got {:?}",
            answer.body
        );
        assert!(
            answer.body.get("Reason").is_none(),
            "case {case:?}: an answered search states no Reason, got {:?}",
            answer.body
        );
    }
}

/// RULE: the object's common name no longer stands in for its account name — a search over
/// the unsearchable attribute is refused rather than quietly matching the object, which is
/// the difference a caller probing the attribute would otherwise never see.
#[tokio::test]
async fn the_unsearchable_name_no_longer_matches_the_account_name() {
    let router = emulator();
    populate(&router).await;

    for (route, _, member, name) in search_routes() {
        let refused = call(&router, route, DIRECTORY, &search(name, &[UNSEARCHABLE])).await;
        let case = format!("{route} searching an account name over {UNSEARCHABLE:?}");
        assert_refused(&case, &refused, UNSEARCHABLE);
        assert!(
            refused.body.get(member).is_none(),
            "case {case:?}: the object must not be reported, got {:?}",
            refused.body
        );
    }
}

/// RULE: a refused search is refused before any record is read, so it reports nothing and
/// changes nothing — the inventory it would have searched is exactly what it was.
#[tokio::test]
async fn a_refused_search_leaves_the_directory_as_it_stood() {
    let router = emulator();
    populate(&router).await;

    for (route, listing, member, name) in search_routes() {
        let before = inventory(&router, listing, member, DIRECTORY).await;
        assert!(
            !before.is_empty(),
            "case {listing:?}: the inventory must hold the created object, got {before:?}"
        );

        for attributes in [vec![UNSEARCHABLE], vec!["SAMAccountName", "Cn"]] {
            let refused = call(&router, route, DIRECTORY, &search(name, &attributes)).await;
            assert_eq!(
                refused.status, 400,
                "case {route:?} over {attributes:?}: expected the refusal, got {} with {:?}",
                refused.status, refused.body
            );
        }

        let after = inventory(&router, listing, member, DIRECTORY).await;
        assert_eq!(
            after, before,
            "case {listing:?}: a refused search must leave the inventory as it stood, got \
             {after:?}"
        );
    }
}
