// SPDX-License-Identifier: CC0-1.0

//! Which attributes a search of a REAL directory may name, across the two services that
//! share that directory.
//!
//! `ds:CreateMicrosoftAD` builds the directory and `ds:EnableDirectoryDataAccess` opens its
//! data plane; the Directory Service DATA plane (`ds-data`) then creates the users and
//! groups inside it and searches them. So the path a client takes to a search — create the
//! directory, wait for it to come into service, turn the feature on, create an object,
//! search for it — crosses both services, and only this configuration (both mounted on ONE
//! engine, which is what `core/router` ships) can walk it.
//!
//! The rule these rows pin: a directory matches a search against an attribute it INDEXES
//! for search, so `SearchUsers`/`SearchGroups` naming an attribute of the object that the
//! service's attribute list does NOT mark searchable is refused — HTTP 400,
//! `ValidationException`, the modelled `Reason` `INVALID_ATTRIBUTE_FOR_SEARCH`
//! (`smithy_query(query="shape", shape="ValidationExceptionReason")`) and a sentence naming
//! the attribute the request asked for — while a searchable attribute of the same object is
//! answered. The crate suite `search_attribute_validation` owns the rule over the data
//! plane alone; these rows walk it over the directory the control plane really built.

use std::time::Duration;

use serde_json::{json, Value};

use crate::directory_data_plane_authorization::{create_microsoft_ad, Answer, Emulator};

/// The domain the directory under test is created with, and the account names of the two
/// objects created inside it.
const DOMAIN: &str = "sim2real-search.example.com";
const GROUP: &str = "s2rGrpSearch";
const USER: &str = "s2rUsrSearch";

/// The `DirectoryStage` a directory whose build has finished reports: the one state the DS
/// Data switch is admitted from.
const ACTIVE: &str = "Active";

/// The error a refused search carries, and the modelled `ValidationExceptionReason` that
/// names an attribute the directory does not index for search — spelled out here rather
/// than imported, so a wrong value in the implementation fails.
const VALIDATION_EXCEPTION: &str = "ValidationException";
const INVALID_ATTRIBUTE_FOR_SEARCH: &str = "INVALID_ATTRIBUTE_FOR_SEARCH";

/// The attribute of a user and of a group that the service's attribute list holds and marks
/// unsearchable: the object's common name, which carries the account name over again.
const UNSEARCHABLE: &str = "Cn";

/// How long these rows give the control plane's build and switch chains before calling them
/// wedged, and how often they look again: a generous bound on a state that never settles
/// rather than a wait for a duration, so nothing here depends on how fast the host runs.
const ATTEMPTS: usize = 240;
const LOOK_AGAIN: Duration = Duration::from_millis(250);

/// The sentence a refused search states: fixed text around the attribute the request named,
/// quoted as the request spelled it.
fn cannot_be_searched(attribute: &str) -> String {
    format!("Attribute '{attribute}' cannot be searched.")
}

/// The `DirectoryStage` the control plane reports for `directory`.
async fn stage(emulator: &Emulator, directory: &str) -> String {
    let described = emulator
        .control_plane("DescribeDirectories", json!({"DirectoryIds": [directory]}))
        .await;
    described.body["DirectoryDescriptions"][0]["Stage"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A Managed Microsoft AD directory of domain [`DOMAIN`], in service, with its DS Data
/// feature on and answering: the control-plane preamble every row below searches through.
async fn directory_serving_its_data_plane(emulator: &Emulator) -> String {
    let created = emulator
        .control_plane("CreateMicrosoftAD", create_microsoft_ad(DOMAIN))
        .await;
    assert_eq!(
        created.status, 200,
        "CreateMicrosoftAD must be accepted, got {} with {:?}",
        created.status, created.body
    );
    let directory_id = created.text("DirectoryId");

    // The switch is admitted only once the build the create owes has fallen due, so the
    // stage is looked for in a loop rather than waited out.
    let mut reached = String::new();
    for _ in 0..ATTEMPTS {
        reached = stage(emulator, &directory_id).await;
        if reached == ACTIVE {
            break;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    assert_eq!(
        reached, ACTIVE,
        "the directory never reached Stage {ACTIVE}, it stayed in {reached:?}"
    );

    let enabled = emulator
        .control_plane(
            "EnableDirectoryDataAccess",
            json!({"DirectoryId": &directory_id}),
        )
        .await;
    assert_eq!(
        enabled.status, 200,
        "EnableDirectoryDataAccess must be accepted, got {} with {:?}",
        enabled.status, enabled.body
    );

    // The data plane answers for the directory once that switch is in service; look for the
    // condition — a listing it serves — rather than for a duration.
    let mut listed = emulator
        .data_plane("/Groups/ListGroups", &directory_id, json!({}))
        .await;
    for _ in 0..ATTEMPTS {
        if listed.status == 200 {
            break;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
        listed = emulator
            .data_plane("/Groups/ListGroups", &directory_id, json!({}))
            .await;
    }
    assert_eq!(
        listed.status, 200,
        "the data plane never served the directory, it answered {} with {:?}",
        listed.status, listed.body
    );
    directory_id
}

/// One search request body.
fn search(string: &str, attributes: &[&str]) -> Value {
    json!({"SearchString": string, "SearchAttributes": attributes})
}

/// Every part of the refusal an unsearchable attribute draws.
fn assert_refused(case: &str, refused: &Answer, attribute: &str) {
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected HTTP 400, got {} with {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code(),
        VALIDATION_EXCEPTION,
        "case {case:?}: expected {VALIDATION_EXCEPTION}, got {:?}",
        refused.body
    );
    assert_eq!(
        refused.text("Reason"),
        INVALID_ATTRIBUTE_FOR_SEARCH,
        "case {case:?}: expected Reason {INVALID_ATTRIBUTE_FOR_SEARCH}, got {:?}",
        refused.body
    );
    assert_eq!(
        refused.text("Message"),
        cannot_be_searched(attribute),
        "case {case:?}: expected the sentence naming {attribute:?}, got {:?}",
        refused.body
    );
    assert!(
        refused.body.get("Users").is_none() && refused.body.get("Groups").is_none(),
        "case {case:?}: a refused search reports no object, got {:?}",
        refused.body
    );
}

/// RULE: over the directory the control plane really built, a search naming an attribute the
/// directory does not index for search is refused, and a searchable attribute of the same
/// object is answered — before and after the refusal, so the refusal leaves nothing behind.
///
/// Both search operations are walked, and the rows differ along the case the unsearchable
/// name is spelled in and where it sits among the searchable names a request also carries.
#[tokio::test]
async fn an_unsearchable_attribute_is_refused_over_a_directory_the_control_plane_built() {
    let emulator = Emulator::new();
    let directory_id = directory_serving_its_data_plane(&emulator).await;

    // (the search route, the member its answer carries, the create route, the account name)
    let objects: [(&str, &str, &str, &str); 2] = [
        ("/Groups/SearchGroups", "Groups", "/Groups/CreateGroup", GROUP),
        ("/Users/SearchUsers", "Users", "/Users/CreateUser", USER),
    ];

    for (route, member, create_route, name) in objects {
        let created = emulator
            .data_plane(create_route, &directory_id, json!({"SAMAccountName": name}))
            .await;
        assert_eq!(
            created.status, 200,
            "case {create_route:?}: the create must be served, got {} with {:?}",
            created.status, created.body
        );

        // The account name is a searchable attribute, so the object the create stored is
        // what the search reports.
        let served = emulator
            .data_plane(route, &directory_id, search(name, &["SAMAccountName"]))
            .await;
        assert_eq!(
            served.status, 200,
            "case {route:?} over SAMAccountName: expected the search to be served, got {} \
             with {:?}",
            served.status, served.body
        );
        assert_eq!(
            served.body[member][0]["SAMAccountName"],
            json!(name),
            "case {route:?} over SAMAccountName: expected the created object, got {:?}",
            served.body
        );
        assert_eq!(
            served.text("Realm"),
            DOMAIN,
            "case {route:?}: the search answers in the directory's own realm, got {:?}",
            served.body
        );

        // The same string over the attribute the list does not index for search is refused,
        // whatever case it is spelled in and whichever searchable names it sits among.
        //
        // (the attributes the request names, the name the refusal must quote)
        let refusals: [(&[&str], &str); 5] = [
            (&[UNSEARCHABLE], UNSEARCHABLE),
            (&["cn"], "cn"),
            (&["CN"], "CN"),
            (&["SAMAccountName", "cN"], "cN"),
            (&["Cn", "SAMAccountName", "Description"], "Cn"),
        ];
        for (attributes, quoted) in refusals {
            let case = format!("{route} naming {attributes:?}");
            let refused = emulator
                .data_plane(route, &directory_id, search(name, attributes))
                .await;
            assert_refused(&case, &refused, quoted);
        }

        // And the searchable search still answers the same object: the refusals above read
        // no record and changed none.
        let again = emulator
            .data_plane(route, &directory_id, search(name, &["SAMAccountName"]))
            .await;
        assert_eq!(
            again.body[member], served.body[member],
            "case {route:?}: a refused search must leave the search path as it stood, got {:?}",
            again.body
        );
    }
}
