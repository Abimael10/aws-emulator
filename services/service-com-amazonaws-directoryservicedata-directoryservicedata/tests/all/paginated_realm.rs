// SPDX-License-Identifier: CC0-1.0

//! WHICH AD DOMAIN each PAGE of a Directory Service Data listing is about.
//!
//! The rule under test: every page of a paginated projection — the first page and every page
//! reached through the `NextToken` the page before it issued — reports, in each realm-valued
//! member its output declares, the Active Directory domain name of the directory the request
//! names (the `Name` the control plane that created that directory was given, which this
//! plane reads from the shared directory inventory), beside the `DirectoryId` that request
//! named. A realm is a property of the directory being read, so it is answered identically on
//! page one and on the last page of the walk.
//!
//! Why pages rather than one answer: all six listings of this API are paginated
//! (`smithy_query(query="operation", operation="ListUsers,ListGroups,ListGroupMembers,ListGroupsForMember,SearchUsers,SearchGroups")`
//! — each `@paginated` with `inputToken`/`outputToken` `NextToken` and `pageSize`
//! `MaxResults`, and each output declaring `Realm`, the two membership listings also
//! `MemberRealm`), so a domain the FIRST page happens to carry says nothing about the pages
//! behind it.
//!
//! The table spans what the rule generalises over: every paginated projection of this API,
//! crossed with three directories each created in its own domain (label counts, a hyphenated
//! label, upper case — the forms `Realm`'s
//! `^([a-zA-Z0-9]+[\\.-])+([a-zA-Z0-9])+[.]?$` admits), crossed with page sizes small enough
//! to force each walk across several page boundaries. Each paged walk is compared with the
//! same listing read in one page, so what a page reports is pinned beside the items it
//! carries. Every assertion goes through the router: routing, deserialization, the handler,
//! the persisted state and serialization.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_directoryservicedata_directoryservicedata::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use super::validation_envelope::{call, hold_directory_in_domain, Answer};

/// The users every directory below is seeded with: names inside `UserName`'s
/// `^[\w\-.]+$`, deliberately out of creation order.
const USERS: [&str; 4] = ["pg-dana", "pg-ann", "pg-cleo", "pg-bo"];

/// The surname every one of them carries, so one search matches them all: `sn` is an
/// attribute the directory indexes for search.
const SURNAME: &str = "Paged";

/// The groups every directory is seeded with, beside the groups an AD domain comes with.
const GROUPS: [&str; 3] = ["pg-eng", "pg-ops", "pg-sec"];

/// The group whose members the membership listings are read for.
const GROUP_WITH_MEMBERS: &str = "pg-eng";

/// The `description` every seeded group carries — a data attribute of the directory's own
/// schema, so one search matches exactly those groups.
const GROUP_DESCRIPTION: &str = "paged-group";

/// One row of the directory table: a directory of this account and the domain name its
/// create was given.
struct Directory {
    case: &'static str,
    id: &'static str,
    domain: &'static str,
}

/// The directories: one per form a domain name takes.
const DIRECTORIES: [Directory; 3] = [
    Directory {
        case: "a three-label dotted domain",
        id: "d-00000000a1",
        domain: "corp.example.com",
    },
    Directory {
        case: "a hyphenated label and four of them",
        id: "d-00000000b2",
        domain: "ad.eng-west.example.net",
    },
    Directory {
        case: "an upper-case two-label domain",
        id: "d-00000000c3",
        domain: "ADCORP.EXAMPLE",
    },
];

/// One paginated projection: the route, the output member holding its items, and the
/// realm-valued members its output declares.
struct Projection {
    case: &'static str,
    route: &'static str,
    items: &'static str,
    realm_members: &'static [&'static str],
}

impl Projection {
    /// The request body this projection is walked with, less `MaxResults` and `NextToken`.
    fn body(&self) -> Value {
        match self.route {
            "/GroupMemberships/ListGroupMembers" => json!({"SAMAccountName": GROUP_WITH_MEMBERS}),
            "/GroupMemberships/ListGroupsForMember" => json!({"SAMAccountName": USERS[0]}),
            "/Users/SearchUsers" => {
                json!({"SearchString": SURNAME, "SearchAttributes": ["sn"]})
            }
            "/Groups/SearchGroups" => {
                json!({
                    "SearchString": GROUP_DESCRIPTION,
                    "SearchAttributes": ["description"],
                })
            }
            _ => json!({}),
        }
    }
}

/// Every paginated projection this API serves.
const PROJECTIONS: [Projection; 6] = [
    Projection {
        case: "ListUsers",
        route: "/Users/ListUsers",
        items: "Users",
        realm_members: &["Realm"],
    },
    Projection {
        case: "ListGroups",
        route: "/Groups/ListGroups",
        items: "Groups",
        realm_members: &["Realm"],
    },
    Projection {
        case: "ListGroupMembers",
        route: "/GroupMemberships/ListGroupMembers",
        items: "Members",
        realm_members: &["Realm", "MemberRealm"],
    },
    Projection {
        case: "ListGroupsForMember",
        route: "/GroupMemberships/ListGroupsForMember",
        items: "Groups",
        realm_members: &["Realm", "MemberRealm"],
    },
    Projection {
        case: "SearchUsers",
        route: "/Users/SearchUsers",
        items: "Users",
        realm_members: &["Realm"],
    },
    Projection {
        case: "SearchGroups",
        route: "/Groups/SearchGroups",
        items: "Groups",
        realm_members: &["Realm"],
    },
];

/// The page sizes each walk is driven with: `MaxResults` 1 puts every item on a page of its
/// own, and 3 puts a part page at the end of most walks.
const PAGE_SIZES: [i64; 2] = [1, 3];

/// One emulator holding every directory of the table, each under its OWN domain name —
/// exactly what a control-plane create plus an `EnableDirectoryDataAccess` leaves in the
/// shared inventory.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    for directory in &DIRECTORIES {
        hold_directory_in_domain(&engine, directory.id, directory.domain);
    }
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Create the users, the groups and the memberships every walk below reads, in `directory`.
async fn seed(router: &MultiServiceRouter, directory: &str) {
    for name in USERS {
        let created = call(
            router,
            "/Users/CreateUser",
            directory,
            &json!({"SAMAccountName": name, "Surname": SURNAME}),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "seeding {directory}: CreateUser {name:?} must be served, got {} with {:?}",
            created.status, created.body
        );
    }
    for name in GROUPS {
        let created = call(
            router,
            "/Groups/CreateGroup",
            directory,
            &json!({
                "SAMAccountName": name,
                "OtherAttributes": {"description": {"S": GROUP_DESCRIPTION}},
            }),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "seeding {directory}: CreateGroup {name:?} must be served, got {} with {:?}",
            created.status, created.body
        );
    }
    for name in USERS {
        let added = call(
            router,
            "/GroupMemberships/AddGroupMember",
            directory,
            &json!({"GroupName": GROUP_WITH_MEMBERS, "MemberName": name}),
        )
        .await;
        assert_eq!(
            added.status, 200,
            "seeding {directory}: AddGroupMember {name:?} must be served, got {} with {:?}",
            added.status, added.body
        );
    }
}

/// The account names one page reported, in the order it reported them.
fn page_names(answer: &Answer, items: &str) -> Vec<String> {
    answer.body[items]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    entry["SAMAccountName"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Walk `projection` for `directory` with `page_size` (`None` for a request naming no
/// `MaxResults`), asserting on EVERY page that it reports the directory's own domain in
/// every realm-valued member, names the directory it was asked about, and holds no more
/// items than were asked for. Answers with the names the whole walk reported, in order.
async fn walk(
    router: &MultiServiceRouter,
    projection: &Projection,
    directory: &Directory,
    page_size: Option<i64>,
) -> Vec<String> {
    let sizing = page_size.map_or_else(
        || "no MaxResults".to_string(),
        |size| format!("MaxResults={size}"),
    );
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0_usize;
    loop {
        let mut body = projection.body();
        if let Some(size) = page_size {
            body["MaxResults"] = json!(size);
        }
        if let Some(token) = token.as_ref() {
            body["NextToken"] = json!(token);
        }
        let answer = call(router, projection.route, directory.id, &body).await;
        pages += 1;
        let case = format!(
            "{} in {} ({}, {sizing}) page {pages}",
            projection.case, directory.id, directory.case
        );
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the page must be served, got {} with {:?}",
            answer.status, answer.body
        );
        for member in projection.realm_members {
            assert_eq!(
                answer.body[*member].as_str(),
                Some(directory.domain),
                "case {case:?}: expected {member} {:?}, got {:?}",
                directory.domain,
                answer.body
            );
        }
        assert_eq!(
            answer.body["DirectoryId"].as_str(),
            Some(directory.id),
            "case {case:?}: expected the page to name the directory it read, got {:?}",
            answer.body
        );
        let names = page_names(&answer, projection.items);
        if let Some(size) = page_size {
            assert!(
                i64::try_from(names.len()).expect("a page fits in i64") <= size,
                "case {case:?}: a page must hold at most {size} items, got {names:?}"
            );
        }
        walked.extend(names);
        token = answer.body["NextToken"].as_str().map(str::to_string);
        if token.is_none() {
            break;
        }
        // Each page carries at least one item, so a walk cannot outlast the inventory it
        // reads; the guard is generous and only catches a walk that fails to terminate.
        assert!(
            pages <= walked.len() + 1,
            "case {case:?}: paging must terminate, walked {walked:?}"
        );
    }
    walked
}

/// RULE: every page of every paginated projection reports the domain name of the directory
/// the request names, and the paged walk reports exactly what the unpaged read reports.
#[tokio::test]
async fn every_page_of_every_paginated_projection_reports_the_directorys_own_domain() {
    let router = emulator();
    for directory in &DIRECTORIES {
        seed(&router, directory.id).await;
    }

    for directory in &DIRECTORIES {
        for projection in &PROJECTIONS {
            let whole = walk(&router, projection, directory, None).await;
            let case = format!("{} in {}", projection.case, directory.id);
            // The seed puts several objects in each projection, so every paged walk below
            // crosses at least one page boundary and the pages behind the first are read.
            assert!(
                whole.len() > 1,
                "case {case:?}: this projection must report more than one object for the \
                 paged walks to reach a second page, got {whole:?}"
            );

            for size in PAGE_SIZES {
                let paged = walk(&router, projection, directory, Some(size)).await;
                assert_eq!(
                    paged, whole,
                    "case {case:?} with MaxResults={size}: walking the pages must report what \
                     one page reports, got {paged:?}"
                );
            }
        }
    }

    // No two directories answer one domain: the realm each page carries is the directory's
    // own and not a value this emulator holds for all of them.
    let mut reported: Vec<String> = Vec::new();
    for directory in &DIRECTORIES {
        let page = call(
            &router,
            "/Users/ListUsers",
            directory.id,
            &json!({"MaxResults": 1}),
        )
        .await;
        reported.push(page.body["Realm"].as_str().unwrap_or_default().to_string());
    }
    let mut distinct = reported.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        DIRECTORIES.len(),
        "expected one domain per directory on the first page of its listing, got {reported:?}"
    );
}
