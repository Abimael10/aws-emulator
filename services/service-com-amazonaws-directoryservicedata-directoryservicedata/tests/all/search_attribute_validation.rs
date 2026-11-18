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
    call, emulator, Answer, DIRECTORY, PAGED_DIRECTORY, SEARCH_ATTRIBUTES_CEILING,
    UNUSED_DIRECTORY, VALIDATION_EXCEPTION,
};

/// A `DirectoryId` this account holds and nothing is ever written to, for the cases about a
/// refusal that precedes reading any record.
const EMPTY_DIRECTORY: &str = UNUSED_DIRECTORY;

/// The `ValidationExceptionReason` a search names an unsearchable attribute with
/// (`smithy_query(query="shape", shape="ValidationExceptionReason")` holds it), spelled out
/// here rather than imported so a wrong value in the implementation fails.
const INVALID_ATTRIBUTE_FOR_SEARCH: &str = "INVALID_ATTRIBUTE_FOR_SEARCH";

/// The `ValidationExceptionReason` a paginated read refuses a cursor it did not issue with
/// (`smithy_query(query="shape", shape="ValidationExceptionReason")` holds it too): the one
/// the rows below read once every attribute the request names is searchable, which is how
/// they pin that the attribute set is read FIRST.
const INVALID_NEXT_TOKEN: &str = "INVALID_NEXT_TOKEN";

/// The attribute of a user and of a group that the service's attribute list holds and marks
/// `Searchable: No`: the object's common name, which carries the account name over again.
const UNSEARCHABLE: &str = "cn";

/// The account name of the user and of the group the cases below search for.
const USER: &str = "jdoe";
const GROUP: &str = "engineering";

/// A value several groups hold for one DATA attribute, so a page of one issues a cursor and
/// the rows about what the refusal PRECEDES have a real walk to interrupt.
const SHARED: &str = "shared-across-groups";

/// DATA attribute names of the service's own attribute list — the list
/// `CreateGroupRequest$OtherAttributes` links — as this API spells them, taken from the
/// crate's schema catalogue of that list (`src/state/attribute_schema.rs`). Every one of them
/// is an attribute a caller writes and the directory searches as the data it is, so a request
/// naming them is served; they fill the attribute list up to the size the model admits.
const CATALOGUED: [&str; 24] = [
    "Co",
    "CodePage",
    "Company",
    "CountryCode",
    "Department",
    "Description",
    "DisplayName",
    "Division",
    "EmployeeID",
    "EmployeeNumber",
    "EmployeeType",
    "FacsimileTelephoneNumber",
    "HomeDirectory",
    "HomeDrive",
    "HomePhone",
    "Info",
    "Initials",
    "IpPhone",
    "L",
    "ManagedBy",
    "Manager",
    "MiddleName",
    "Mobile",
    "OtherTelephone",
];

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

/// One row of the ordering table below: what the request carries BESIDES its attributes,
/// the attributes it names, the name its refusal must quote, the attributes the mirrored
/// request names instead, and the `Reason` that mirror draws (`None` when it is served).
type OrderingRow = (
    &'static str,
    Value,
    Vec<&'static str>,
    &'static str,
    Vec<&'static str>,
    Option<&'static str>,
);

/// RULE: the ATTRIBUTE SET alone decides the refusal — it is read before the page cursor,
/// the realm, the page size and the search string's own matching, so none of those can turn
/// a search the directory cannot run into one it answers, and the rule holds at every size
/// `SearchAttributes` admits.
///
/// The rows differ along what else the request carries (a cursor a previous page issued, a
/// cursor no page issued, a realm the directory holds and a realm it does not, the largest
/// page the model admits, a search string nothing holds) and along the SIZE of the attribute
/// list, up to the maximum the model declares with the unsearchable name as its last entry.
/// Each row is mirrored by the same request with a searchable name in the offender's place,
/// which is answered.
#[tokio::test]
async fn the_attribute_set_alone_decides_whether_the_search_is_refused() {
    let router = emulator();

    // Three groups holding one value for one data attribute, so a page of one leaves groups
    // behind it and the search issues a cursor to resume from.
    for name in ["alpha", "beta", "gamma"] {
        let created = call(
            &router,
            "/Groups/CreateGroup",
            PAGED_DIRECTORY,
            &json!({
                "SAMAccountName": name,
                "OtherAttributes": {"Description": {"S": SHARED}},
            }),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {name:?}: the create must be accepted, got {} with {:?}",
            created.status, created.body
        );
    }

    let page = call(
        &router,
        "/Groups/SearchGroups",
        PAGED_DIRECTORY,
        &json!({"SearchString": SHARED, "SearchAttributes": ["Description"], "MaxResults": 1}),
    )
    .await;
    assert_eq!(
        page.status, 200,
        "the first page over a searchable attribute must be served, got {} with {:?}",
        page.status, page.body
    );
    let cursor = page.body["NextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "a page of one with groups behind it issues a cursor, got {:?}",
                page.body
            )
        })
        .to_string();
    // The realm the directory itself answers in, read from that page rather than written
    // here, so the row carries the realm this directory really has.
    let held_realm = page.body["Realm"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "a served page states the realm it searched, got {:?}",
                page.body
            )
        })
        .to_string();

    // The attribute list at the maximum size the model admits, with the unsearchable name as
    // its last entry, and the same list with a searchable name in its place.
    let mut at_the_ceiling: Vec<&str> = CATALOGUED.to_vec();
    at_the_ceiling.push("Cn");
    let mut all_searchable: Vec<&str> = CATALOGUED.to_vec();
    all_searchable.push("SAMAccountName");
    assert_eq!(
        at_the_ceiling.len(),
        SEARCH_ATTRIBUTES_CEILING,
        "the ceiling row must carry the largest list the model admits, got {}",
        at_the_ceiling.len()
    );

    // (case, what the request carries besides the attributes, the attributes, the offender,
    // the attributes the mirrored request names instead, the `Reason` that mirror draws)
    let rows: [OrderingRow; 7] = [
        (
            "the cursor the previous page issued",
            json!({"NextToken": cursor.clone()}),
            vec!["Description", "Cn"],
            "Cn",
            vec!["Description"],
            None,
        ),
        (
            "a cursor no page ever issued",
            json!({"NextToken": "not-a-cursor-this-service-minted"}),
            vec!["Cn"],
            "Cn",
            vec!["SAMAccountName"],
            // With every name searchable, the cursor is what the request fails on — which is
            // the ordering this row pins: the attribute set is read first.
            Some(INVALID_NEXT_TOKEN),
        ),
        (
            "the realm the directory answers in",
            json!({"Realm": held_realm.clone()}),
            vec!["Cn"],
            "Cn",
            vec!["SAMAccountName"],
            None,
        ),
        (
            "a realm no directory of this account holds",
            json!({"Realm": "sim2real-elsewhere.example.org"}),
            vec!["Description", "CN"],
            "CN",
            vec!["Description"],
            None,
        ),
        (
            "the largest page the model admits",
            json!({"MaxResults": 250}),
            vec!["cN"],
            "cN",
            vec!["SAMAccountName"],
            None,
        ),
        (
            "a search string no object holds",
            json!({"SearchString": "sim2real-no-object-holds-this"}),
            vec!["cn"],
            "cn",
            vec!["Description"],
            None,
        ),
        (
            "the offender as the last name the list admits",
            json!({}),
            at_the_ceiling.clone(),
            "Cn",
            all_searchable.clone(),
            None,
        ),
    ];

    for (what, extra, attributes, quoted, mirrored_attributes, mirrored_reason) in rows {
        let mut request = search(SHARED, &attributes);
        for (member, value) in extra.as_object().expect("an object of members") {
            request[member] = value.clone();
        }
        let case = format!("SearchGroups over {attributes:?} with {what}");
        let refused = call(&router, "/Groups/SearchGroups", PAGED_DIRECTORY, &request).await;
        assert_refused(&case, &refused, quoted);

        // The same request with only searchable names is decided by what it carries BESIDES
        // the attributes, so the row is about the attribute and nothing else.
        let mut mirrored = request.clone();
        mirrored["SearchAttributes"] = json!(mirrored_attributes);
        let answered = call(&router, "/Groups/SearchGroups", PAGED_DIRECTORY, &mirrored).await;
        match mirrored_reason {
            None => {
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}: the same request over {mirrored_attributes:?} must be \
                     served, got {} with {:?}",
                    answered.status, answered.body
                );
                assert!(
                    answered.body.get("Reason").is_none(),
                    "case {case:?}: an answered search states no Reason, got {:?}",
                    answered.body
                );
            }
            Some(reason) => {
                assert_eq!(
                    answered.status, 400,
                    "case {case:?}: the same request over {mirrored_attributes:?} must be \
                     refused, got {} with {:?}",
                    answered.status, answered.body
                );
                assert_eq!(
                    answered.body.get("Reason").and_then(Value::as_str),
                    Some(reason),
                    "case {case:?}: expected Reason {reason} once every name is searchable, \
                     got {:?}",
                    answered.body
                );
            }
        }
    }

    // And the cursor the first page issued still resumes that same walk, which is what the
    // refused rows above left untouched.
    let resumed = call(
        &router,
        "/Groups/SearchGroups",
        PAGED_DIRECTORY,
        &json!({
            "SearchString": SHARED,
            "SearchAttributes": ["Description"],
            "MaxResults": 1,
            "NextToken": cursor,
        }),
    )
    .await;
    assert_eq!(
        resumed.status, 200,
        "the cursor must still resume the walk, got {} with {:?}",
        resumed.status, resumed.body
    );
    assert_eq!(
        resumed.body["Groups"].as_array().map(Vec::len),
        Some(1),
        "the resumed page must report the group after the first, got {:?}",
        resumed.body
    );
}
