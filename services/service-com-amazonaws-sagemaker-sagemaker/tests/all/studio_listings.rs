// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for the two Studio LISTINGS: `ListApps` and `ListUserProfiles`.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, so
//! what these tests exercise is what a real SDK client gets. The harness (`Emulator`,
//! `error_code`, `message`, `str_field`) is the one `inference_deployment_lifecycle`
//! defines for this target, and the domain and profile a Studio app hangs off are seeded
//! by `crate::studio_user_profiles`.
//!
//! The tests assert RULES over a TABLE of inputs rather than one recorded call: each
//! listing is driven for every search member it documents, for both sort orders, and for
//! page sizes from one record to the whole set, and `ListApps` is driven for an app the
//! service has torn down as well as for running ones.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::inference_deployment_lifecycle::{error_code, message, str_field, Emulator};
use crate::studio_user_profiles::{seed_studio_domain, seed_studio_owner};

/// How long a poll below gives the clock to settle a resource: far longer than the spans
/// the state gives an app's launch and its teardown, so a loaded host still settles well
/// inside it. Only the loop's condition decides when it stops.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// How often a poll looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The domain the app rows below live in.
const DOMAIN: &str = "d-listing01";

/// A second domain, so a `DomainIdEquals` search can be told from no search at all.
const OTHER_DOMAIN: &str = "d-listing02";

/// One app row: the members that identify it, as `ListApps` reports them.
struct AppRow {
    domain: &'static str,
    owner_member: &'static str,
    owner_name: &'static str,
    app_type: &'static str,
    app_name: &'static str,
}

impl AppRow {
    /// The request body naming this app — the whole input of `DescribeApp` and `DeleteApp`
    /// and the identifying part of `CreateApp`'s.
    fn body(&self) -> Value {
        json!({
            "DomainId": self.domain,
            self.owner_member: self.owner_name,
            "AppType": self.app_type,
            "AppName": self.app_name,
        })
    }

    /// How this app is identified in an assertion: the four members `AppDetails` carries
    /// for it, in one comparable string.
    fn identity(&self) -> String {
        format!(
            "{}/{}/{}/{}",
            self.domain, self.owner_name, self.app_type, self.app_name
        )
    }
}

/// The rows every `ListApps` case is driven over: both owner kinds, several app types and
/// two domains, so each search member narrows the page differently.
fn app_rows() -> Vec<AppRow> {
    vec![
        AppRow {
            domain: DOMAIN,
            owner_member: "UserProfileName",
            owner_name: "studio-user",
            app_type: "JupyterServer",
            app_name: "default",
        },
        AppRow {
            domain: DOMAIN,
            owner_member: "UserProfileName",
            owner_name: "studio-user",
            app_type: "JupyterLab",
            app_name: "lab",
        },
        AppRow {
            domain: DOMAIN,
            owner_member: "UserProfileName",
            owner_name: "analyst",
            app_type: "Canvas",
            app_name: "canvas",
        },
        AppRow {
            domain: DOMAIN,
            owner_member: "SpaceName",
            owner_name: "team-space",
            app_type: "CodeEditor",
            app_name: "editor",
        },
        AppRow {
            domain: OTHER_DOMAIN,
            owner_member: "UserProfileName",
            owner_name: "studio-user",
            app_type: "TensorBoard",
            app_name: "board",
        },
    ]
}

/// How one listed `AppDetails` is identified: the same four members [`AppRow::identity`]
/// composes, read off the response.
fn listed_app_identity(details: &Value) -> String {
    let owner = details
        .get("UserProfileName")
        .or_else(|| details.get("SpaceName"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected an owner member in {details}"));
    format!(
        "{}/{owner}/{}/{}",
        str_field(details, "DomainId"),
        str_field(details, "AppType"),
        str_field(details, "AppName")
    )
}

/// The `Apps` one `ListApps` response carries.
fn listed_apps(body: &Value) -> Vec<Value> {
    body.get("Apps")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("expected an Apps list in {body}"))
        .clone()
}

/// Walk `ListApps` to the end with `request` as the first page, answering every listed app
/// in response order.
async fn all_listed_apps(emulator: &Emulator, request: &Value) -> Vec<Value> {
    let mut apps: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut page_request = request
            .as_object()
            .expect("the request members form an object")
            .clone();
        if let Some(token) = &token {
            page_request.insert("NextToken".to_string(), json!(token));
        }
        let page = emulator
            .ok("ListApps", &Value::Object(page_request))
            .await;
        apps.extend(listed_apps(&page));
        match page.get("NextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    apps
}

/// Create every row of [`app_rows`], seeding the parents each hangs off first.
async fn create_app_rows(emulator: &Emulator) -> Vec<AppRow> {
    let rows = app_rows();
    for row in &rows {
        if row.owner_member == "UserProfileName" {
            seed_studio_owner(emulator, "us-east-1", row.domain, row.owner_name);
        } else {
            seed_studio_domain(emulator, "us-east-1", row.domain);
        }
        emulator.ok("CreateApp", &row.body()).await;
    }
    rows
}

/// RULE: `ListApps` reports every stored app with the members that identify it, narrowed
/// by whichever of `DomainIdEquals`, `UserProfileNameEquals` and `SpaceNameEquals` the
/// request carries — an absent search member narrows nothing.
#[tokio::test]
async fn list_apps_pages_the_apps_each_search_member_selects() {
    let emulator = Emulator::new();
    let rows = create_app_rows(&emulator).await;

    // Rows: no search at all, then each documented search member, so every one is shown to
    // select exactly the apps whose own member equals it.
    let cases: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "no-search",
            json!({}),
            rows.iter().map(AppRow::identity).collect(),
        ),
        (
            "domain-id-equals",
            json!({ "DomainIdEquals": DOMAIN }),
            rows.iter()
                .filter(|row| row.domain == DOMAIN)
                .map(AppRow::identity)
                .collect(),
        ),
        (
            "domain-id-equals-other-domain",
            json!({ "DomainIdEquals": OTHER_DOMAIN }),
            rows.iter()
                .filter(|row| row.domain == OTHER_DOMAIN)
                .map(AppRow::identity)
                .collect(),
        ),
        (
            "user-profile-name-equals",
            json!({ "UserProfileNameEquals": "studio-user" }),
            rows.iter()
                .filter(|row| row.owner_member == "UserProfileName" && row.owner_name == "studio-user")
                .map(AppRow::identity)
                .collect(),
        ),
        (
            "space-name-equals",
            json!({ "SpaceNameEquals": "team-space" }),
            rows.iter()
                .filter(|row| row.owner_member == "SpaceName" && row.owner_name == "team-space")
                .map(AppRow::identity)
                .collect(),
        ),
        (
            "domain-and-owner",
            json!({ "DomainIdEquals": DOMAIN, "UserProfileNameEquals": "analyst" }),
            rows.iter()
                .filter(|row| row.domain == DOMAIN && row.owner_name == "analyst")
                .map(AppRow::identity)
                .collect(),
        ),
        (
            "search-nothing-matches",
            json!({ "UserProfileNameEquals": "nobody" }),
            Vec::new(),
        ),
    ];

    for (case, request, expected) in cases {
        let listed = all_listed_apps(&emulator, &request).await;
        let got: BTreeSet<String> = listed.iter().map(listed_app_identity).collect();
        let want: BTreeSet<String> = expected.into_iter().collect();
        assert_eq!(
            got, want,
            "case {case}: the page must hold exactly the apps the search selects, got \
             {listed:?}"
        );
        for details in &listed {
            assert!(
                details.get("CreationTime").is_some_and(Value::is_number),
                "case {case}: every listed app reports its stored creation time, got \
                 {details}"
            );
            assert!(
                ["Pending", "InService"].contains(&str_field(details, "Status").as_str()),
                "case {case}: a running app reports a launching status, got {details}"
            );
        }
    }
}

/// RULE: `ListApps` keeps reporting an app the service has torn down — with `Status`
/// `Deleted` once the clock settles the teardown — for as long as its record is retained,
/// so an audit flow still sees the apps deleted recently.
#[tokio::test]
async fn list_apps_reports_an_app_that_was_deleted() {
    let emulator = Emulator::new();
    let rows = create_app_rows(&emulator).await;
    let deleted = &rows[0];

    emulator.ok("DeleteApp", &deleted.body()).await;

    // Wait for the CONDITION: the `Deleted` status is the clock's, and no call has to make
    // it happen.
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let listed = all_listed_apps(&emulator, &json!({})).await;
        let entry = listed
            .iter()
            .find(|details| listed_app_identity(details) == deleted.identity())
            .unwrap_or_else(|| {
                panic!("case deleted-app: a deleted app stays listed, got {listed:?}")
            });
        if str_field(entry, "Status") == "Deleted" {
            break;
        }
        assert_eq!(
            str_field(entry, "Status"),
            "Deleting",
            "case deleted-app: a listed app being torn down reports its teardown, got \
             {entry}"
        );
        assert!(
            Instant::now() < deadline,
            "case deleted-app: the app must reach Deleted on the clock, last read {entry}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // Every other app is listed unchanged, so the teardown narrowed the page by nothing.
    let listed = all_listed_apps(&emulator, &json!({})).await;
    let got: BTreeSet<String> = listed.iter().map(listed_app_identity).collect();
    let want: BTreeSet<String> = rows.iter().map(AppRow::identity).collect();
    assert_eq!(
        got, want,
        "case deleted-app: the retained record keeps the app listed, got {listed:?}"
    );
}

/// RULE: `ListApps` serves at most `MaxResults` apps per page and names the boundary in a
/// `NextToken`, so a client walking the pages sees every app exactly once whatever the page
/// size; `SortOrder` reverses the one order the pages are served in, and the last page
/// carries no token.
#[tokio::test]
async fn list_apps_walks_every_page_size_and_both_sort_orders() {
    let emulator = Emulator::new();
    let rows = create_app_rows(&emulator).await;
    let total = rows.len();

    for page_size in 1..=total + 1 {
        let request = json!({ "MaxResults": page_size });
        let walked = all_listed_apps(&emulator, &request).await;
        let identities: Vec<String> = walked.iter().map(listed_app_identity).collect();
        let unique: BTreeSet<&String> = identities.iter().collect();
        assert_eq!(
            unique.len(),
            total,
            "case page-size-{page_size}: walking the pages must see every app exactly \
             once, got {identities:?}"
        );
        assert_eq!(
            identities.len(),
            total,
            "case page-size-{page_size}: no app may be served twice, got {identities:?}"
        );

        // One page of that size holds no more than it asks for, and only names a token
        // when something is left.
        let page = emulator.ok("ListApps", &request).await;
        let served = listed_apps(&page).len();
        assert!(
            served <= page_size,
            "case page-size-{page_size}: a page is an upper bound, got {served}"
        );
        assert_eq!(
            page.get("NextToken").is_some(),
            served < total,
            "case page-size-{page_size}: a token is served only where a further page \
             exists, got {page}"
        );
    }

    // Both documented sort orders serve the same apps, in opposite order.
    let ascending: Vec<String> = all_listed_apps(&emulator, &json!({ "SortOrder": "Ascending" }))
        .await
        .iter()
        .map(listed_app_identity)
        .collect();
    let descending: Vec<String> = all_listed_apps(&emulator, &json!({ "SortOrder": "Descending" }))
        .await
        .iter()
        .map(listed_app_identity)
        .collect();
    let reversed: Vec<String> = ascending.iter().rev().cloned().collect();
    assert_eq!(
        descending, reversed,
        "case sort-order: Descending must serve the Ascending order reversed, got \
         {descending:?} against {ascending:?}"
    );
    // The documented default is the ascending one, so a request naming no order matches it.
    let default: Vec<String> = all_listed_apps(&emulator, &json!({}))
        .await
        .iter()
        .map(listed_app_identity)
        .collect();
    assert_eq!(
        default, ascending,
        "case sort-order-default: the default order is Ascending, got {default:?}"
    );
}

/// RULE: a `ListApps` naming both owner search members is refused with HTTP 400
/// `ValidationException` — each member's doc says the other cannot be set beside it — and a
/// `NextToken` naming no boundary of the current result set is refused the same way.
#[tokio::test]
async fn list_apps_refuses_a_search_it_cannot_serve() {
    let emulator = Emulator::new();
    create_app_rows(&emulator).await;

    let cases: Vec<(&str, Value)> = vec![
        (
            "both-owner-search-members",
            json!({ "UserProfileNameEquals": "studio-user", "SpaceNameEquals": "team-space" }),
        ),
        (
            "token-naming-no-boundary",
            // The token format is opaque hex of the boundary id, so this names an app the
            // result set does not hold.
            json!({ "NextToken": hex::encode(b"d-listing01/nobody/Canvas/absent") }),
        ),
    ];

    for (case, request) in cases {
        let (status, parsed) = emulator.call("ListApps", &request).await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400 from ListApps, got {status} with {parsed}"
        );
        assert_eq!(
            error_code(&parsed),
            "ValidationException",
            "case {case}: error code, got {parsed}"
        );
        assert!(
            !message(&parsed).is_empty(),
            "case {case}: the refusal must say what was wrong, got {parsed}"
        );
    }
}

/// RULE: `ListUserProfiles` reports every stored profile with the members that identify it,
/// narrowed by `DomainIdEquals` and by `UserProfileNameContains` — which selects the names
/// that CONTAIN the value, whatever its case, since `UserProfileName` is not case
/// sensitive.
#[tokio::test]
async fn list_user_profiles_pages_the_profiles_each_search_member_selects() {
    let emulator = Emulator::new();

    // Rows: two domains, and names that share fragments, so each search member narrows the
    // page differently.
    let seeded: [(&str, &str); 5] = [
        (DOMAIN, "studio-user"),
        (DOMAIN, "AnalystUser"),
        (DOMAIN, "data-scientist"),
        (OTHER_DOMAIN, "studio-user"),
        (OTHER_DOMAIN, "auditor"),
    ];
    for (domain, name) in seeded {
        seed_studio_owner(&emulator, "us-east-1", domain, name);
    }

    let identity = |domain: &str, name: &str| format!("{domain}/{name}");
    let cases: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "no-search",
            json!({}),
            seeded
                .iter()
                .map(|(domain, name)| identity(domain, name))
                .collect(),
        ),
        (
            "domain-id-equals",
            json!({ "DomainIdEquals": DOMAIN }),
            seeded
                .iter()
                .filter(|(domain, _)| *domain == DOMAIN)
                .map(|(domain, name)| identity(domain, name))
                .collect(),
        ),
        (
            "name-contains",
            json!({ "UserProfileNameContains": "user" }),
            seeded
                .iter()
                .filter(|(_, name)| name.to_ascii_lowercase().contains("user"))
                .map(|(domain, name)| identity(domain, name))
                .collect(),
        ),
        (
            "name-contains-other-case",
            json!({ "UserProfileNameContains": "USER" }),
            seeded
                .iter()
                .filter(|(_, name)| name.to_ascii_lowercase().contains("user"))
                .map(|(domain, name)| identity(domain, name))
                .collect(),
        ),
        (
            "domain-and-name-contains",
            json!({ "DomainIdEquals": DOMAIN, "UserProfileNameContains": "data" }),
            vec![identity(DOMAIN, "data-scientist")],
        ),
        (
            "search-nothing-matches",
            json!({ "UserProfileNameContains": "absent" }),
            Vec::new(),
        ),
    ];

    for (case, request, expected) in cases {
        let page = emulator.ok("ListUserProfiles", &request).await;
        let listed = page
            .get("UserProfiles")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case}: expected a UserProfiles list in {page}"))
            .clone();
        let got: BTreeSet<String> = listed
            .iter()
            .map(|details| {
                identity(
                    &str_field(details, "DomainId"),
                    &str_field(details, "UserProfileName"),
                )
            })
            .collect();
        let want: BTreeSet<String> = expected.into_iter().collect();
        assert_eq!(
            got, want,
            "case {case}: the page must hold exactly the profiles the search selects, got \
             {listed:?}"
        );
        for details in &listed {
            assert!(
                details.get("CreationTime").is_some_and(Value::is_number),
                "case {case}: every listed profile reports its stored creation time, got \
                 {details}"
            );
            assert!(
                details
                    .get("LastModifiedTime")
                    .is_some_and(Value::is_number),
                "case {case}: every listed profile reports when it last changed, got \
                 {details}"
            );
        }
    }
}

/// RULE: `ListUserProfiles` serves at most `MaxResults` profiles per page and names the
/// boundary in a `NextToken`, so a client walking the pages sees every profile exactly once
/// whatever the page size and whichever documented `SortBy` and `SortOrder` it asks for.
#[tokio::test]
async fn list_user_profiles_walks_every_page_size_and_every_documented_sort() {
    let emulator = Emulator::new();

    let names = ["one", "two", "three", "four"];
    for name in names {
        seed_studio_owner(&emulator, "us-east-1", DOMAIN, name);
    }
    let total = names.len();

    // `UserProfileSortKey`'s members (smithy_query shape=UserProfileSortKey) crossed with
    // both `SortOrder` members and every page size from one record to one past the set.
    for sort_by in ["CreationTime", "LastModifiedTime"] {
        for sort_order in ["Ascending", "Descending"] {
            for page_size in 1..=total + 1 {
                let case = format!("{sort_by}-{sort_order}-page-{page_size}");
                let mut walked: Vec<String> = Vec::new();
                let mut token: Option<String> = None;
                loop {
                    let mut request = json!({
                        "SortBy": sort_by,
                        "SortOrder": sort_order,
                        "MaxResults": page_size,
                    });
                    if let Some(token) = &token {
                        request
                            .as_object_mut()
                            .expect("the request members form an object")
                            .insert("NextToken".to_string(), json!(token));
                    }
                    let page = emulator.ok("ListUserProfiles", &request).await;
                    let listed = page
                        .get("UserProfiles")
                        .and_then(Value::as_array)
                        .unwrap_or_else(|| panic!("case {case}: expected a list in {page}"))
                        .clone();
                    assert!(
                        listed.len() <= page_size,
                        "case {case}: a page is an upper bound, got {}",
                        listed.len()
                    );
                    walked.extend(
                        listed
                            .iter()
                            .map(|details| str_field(details, "UserProfileName")),
                    );
                    match page.get("NextToken").and_then(Value::as_str) {
                        Some(next) => token = Some(next.to_string()),
                        None => break,
                    }
                }
                let unique: BTreeSet<&String> = walked.iter().collect();
                assert_eq!(
                    (walked.len(), unique.len()),
                    (total, total),
                    "case {case}: walking the pages must see every profile exactly once, \
                     got {walked:?}"
                );
            }
        }
    }
}
