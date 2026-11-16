// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `ListThingGroups`: the account's listing of its thing
//! groups, and the filters that narrow it.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what an
//! AWS SDK client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * the listing reports the thing groups the account holds, each with the ARN its own
//!   create minted, at every depth of the hierarchy and in `thingGroupName` order;
//! * `parentGroup` narrows it to that group's children, and `recursive` ("If true,
//!   return child groups as well") widens those children to every group below it; with no
//!   `parentGroup` there are no children to widen, so `recursive` selects nothing of its
//!   own;
//! * `namePrefixFilter` keeps only the groups whose name starts with it, and composes
//!   with the hierarchy filter;
//! * a `parentGroup` the account does not hold is `ResourceNotFoundException` (404)
//!   rather than an empty page;
//! * the listing is `@paginated`, so `maxResults` bounds a page, its own `nextToken`
//!   resumes exactly where that page stopped, and a token that belongs to another
//!   listing is refused.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for; the ARN assertions read it back.
const REGION: &str = "ap-south-1";

/// The account id this emulator serves, which the registry families qualify a missing
/// name with (`engine::emulator_account_id`).
const ACCOUNT_ID: &str = "000000000000";

/// One emulator: a router with this service mounted on its own engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the thing group request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    /// Create one static thing group, optionally under a parent, answering its ARN.
    async fn create_group(&self, group_name: &str, parent: Option<&str>) -> String {
        let body = match parent {
            Some(parent) => json!({ "parentGroupName": parent }),
            None => json!({}),
        };
        let reply = self
            .dispatch("POST", &format!("/thing-groups/{group_name}"), Some(&body))
            .await;
        assert_eq!(
            reply.status,
            200,
            "case create {group_name}: expected CreateThingGroup to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["thingGroupArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case create {group_name}: no thingGroupArn in {}", reply.text()))
            .to_string()
    }

    /// `ListThingGroups` with the query string a case asks for (`""` for none).
    async fn list_groups(&self, query: &str) -> Reply {
        let path = if query.is_empty() {
            "/thing-groups".to_string()
        } else {
            format!("/thing-groups?{query}")
        };
        self.dispatch("GET", &path, None).await
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: response.into_body(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.text()))
    }

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    fn message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }

    /// The group names a `ListThingGroups` page carries.
    fn group_names(&self) -> Vec<String> {
        self.json()["thingGroups"]
            .as_array()
            .unwrap_or_else(|| panic!("no thingGroups in {}", self.text()))
            .iter()
            .map(|group| group["groupName"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// The group ARNs it carries, beside those names.
    fn group_arns(&self) -> Vec<String> {
        self.json()["thingGroups"]
            .as_array()
            .unwrap_or_else(|| panic!("no thingGroups in {}", self.text()))
            .iter()
            .map(|group| group["groupArn"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    fn next_token(&self) -> Option<String> {
        self.json()["nextToken"].as_str().map(str::to_string)
    }
}

/// The hierarchy every case in this suite reads: a chain three levels deep beside a
/// second root, so the rows differ in DEPTH and in how many children a group has.
/// `(group, parent)`, created in this order.
const HIERARCHY: [(&str, Option<&str>); 5] = [
    ("fleet", None),
    ("fleet-east", Some("fleet")),
    ("fleet-west", Some("fleet")),
    ("fleet-east-rack1", Some("fleet-east")),
    ("warehouse", None),
];

/// Every group of [`HIERARCHY`] in `thingGroupName` order, which is the listing's order.
const EVERY_GROUP: [&str; 5] = [
    "fleet",
    "fleet-east",
    "fleet-east-rack1",
    "fleet-west",
    "warehouse",
];

/// Build [`HIERARCHY`] on a fresh emulator, answering each group's minted ARN.
async fn with_hierarchy() -> (Emulator, Vec<(String, String)>) {
    let emulator = Emulator::new();
    let mut arns: Vec<(String, String)> = Vec::new();
    for (group_name, parent) in HIERARCHY {
        let arn = emulator.create_group(group_name, parent).await;
        arns.push(((group_name).to_string(), arn));
    }
    (emulator, arns)
}

fn names(of: &[&str]) -> Vec<String> {
    of.iter().map(|name| (*name).to_string()).collect()
}

/// RULE: the account's listing reports every thing group it holds, at every depth, in
/// name order, each with the ARN its own create minted.
#[tokio::test]
async fn the_account_listing_reports_every_group_with_the_arn_its_create_minted() {
    let (emulator, arns) = with_hierarchy().await;

    let listed = emulator.list_groups("").await;
    assert_eq!(
        listed.status,
        200,
        "case the whole account: expected 200, got {} body {}",
        listed.status,
        listed.text()
    );
    assert_eq!(
        listed.group_names(),
        names(&EVERY_GROUP),
        "case the whole account: expected every group once in name order, got {:?}",
        listed.group_names()
    );
    // The ARN beside each name is the group's own, so this listing and the create
    // cannot disagree about a group's identity.
    let expected_arns: Vec<String> = EVERY_GROUP
        .iter()
        .map(|group_name| {
            arns.iter()
                .find(|(held, _arn)| held == group_name)
                .map(|(_held, arn)| arn.clone())
                .unwrap_or_else(|| panic!("no ARN recorded for {group_name}"))
        })
        .collect();
    assert_eq!(
        listed.group_arns(),
        expected_arns,
        "case the whole account: expected the minted ARNs, got {:?}",
        listed.group_arns()
    );

    // An account holding no group at all answers an EMPTY list, not an absent member.
    let empty = Emulator::new().list_groups("").await;
    assert_eq!(
        empty.status,
        200,
        "case an account with no group: expected 200, got {} body {}",
        empty.status,
        empty.text()
    );
    assert!(
        empty.group_names().is_empty(),
        "case an account with no group: expected an empty listing, got {:?}",
        empty.group_names()
    );
}

/// RULE: `parentGroup` narrows the listing to that group's children, and `recursive`
/// widens those children to every group below it in the hierarchy.
#[tokio::test]
async fn the_hierarchy_filter_reports_children_and_recursive_widens_it_to_descendants() {
    let (emulator, _arns) = with_hierarchy().await;

    // The rows span what the rule generalises over: the depth of the parent, how many
    // children it has, whether `recursive` is absent, false or true, and a leaf and a
    // childless root, which have no group below them at all.
    struct Case {
        label: &'static str,
        query: &'static str,
        expected: &'static [&'static str],
    }
    let cases = [
        Case {
            label: "recursive with no parent group names the whole account",
            query: "recursive=true",
            expected: &EVERY_GROUP,
        },
        Case {
            label: "the root's children, recursive absent",
            query: "parentGroup=fleet",
            expected: &["fleet-east", "fleet-west"],
        },
        Case {
            label: "the root's children, recursive false",
            query: "parentGroup=fleet&recursive=false",
            expected: &["fleet-east", "fleet-west"],
        },
        Case {
            label: "the root's whole subtree",
            query: "parentGroup=fleet&recursive=true",
            expected: &["fleet-east", "fleet-east-rack1", "fleet-west"],
        },
        Case {
            label: "an intermediate group's one child",
            query: "parentGroup=fleet-east&recursive=false",
            expected: &["fleet-east-rack1"],
        },
        Case {
            label: "an intermediate group's subtree, which is that one child",
            query: "parentGroup=fleet-east&recursive=true",
            expected: &["fleet-east-rack1"],
        },
        Case {
            label: "a leaf has no child",
            query: "parentGroup=fleet-east-rack1",
            expected: &[],
        },
        Case {
            label: "a leaf has no subtree either",
            query: "parentGroup=fleet-east-rack1&recursive=true",
            expected: &[],
        },
        Case {
            label: "a childless root",
            query: "parentGroup=warehouse&recursive=true",
            expected: &[],
        },
    ];

    for case in &cases {
        let listed = emulator.list_groups(case.query).await;
        assert_eq!(
            listed.status,
            200,
            "case {}: expected 200, got {} body {}",
            case.label,
            listed.status,
            listed.text()
        );
        assert_eq!(
            listed.group_names(),
            names(case.expected),
            "case {}: expected {:?}, got {:?}",
            case.label,
            case.expected,
            listed.group_names()
        );
    }
}

/// RULE: `namePrefixFilter` keeps only the groups whose own name starts with it, and it
/// composes with the hierarchy filter.
#[tokio::test]
async fn the_name_prefix_filter_keeps_the_groups_whose_name_starts_with_it() {
    let (emulator, _arns) = with_hierarchy().await;

    // The rows span what the rule generalises over: the length of the prefix (one
    // character up to a whole name), a prefix that matches several groups, one that
    // matches exactly one, one that matches none, and a prefix beside a hierarchy
    // filter.
    struct Case {
        label: &'static str,
        query: &'static str,
        expected: &'static [&'static str],
    }
    let cases = [
        Case {
            label: "a one-character prefix",
            query: "namePrefixFilter=f",
            expected: &["fleet", "fleet-east", "fleet-east-rack1", "fleet-west"],
        },
        Case {
            label: "a prefix that is itself a group name",
            query: "namePrefixFilter=fleet-east",
            expected: &["fleet-east", "fleet-east-rack1"],
        },
        Case {
            label: "a prefix matching a single group",
            query: "namePrefixFilter=w",
            expected: &["warehouse"],
        },
        Case {
            label: "a whole name as the prefix",
            query: "namePrefixFilter=warehouse",
            expected: &["warehouse"],
        },
        Case {
            label: "a prefix no group starts with",
            query: "namePrefixFilter=zzz",
            expected: &[],
        },
        Case {
            label: "a prefix that matches only on a deeper level",
            query: "namePrefixFilter=fleet-east-rack",
            expected: &["fleet-east-rack1"],
        },
        Case {
            label: "a prefix beside the subtree filter",
            query: "parentGroup=fleet&recursive=true&namePrefixFilter=fleet-east",
            expected: &["fleet-east", "fleet-east-rack1"],
        },
        Case {
            label: "a prefix beside the children filter",
            query: "parentGroup=fleet&namePrefixFilter=fleet-east",
            expected: &["fleet-east"],
        },
        Case {
            label: "a prefix that excludes every child of the named parent",
            query: "parentGroup=fleet&namePrefixFilter=warehouse",
            expected: &[],
        },
    ];

    for case in &cases {
        let listed = emulator.list_groups(case.query).await;
        assert_eq!(
            listed.status,
            200,
            "case {}: expected 200, got {} body {}",
            case.label,
            listed.status,
            listed.text()
        );
        assert_eq!(
            listed.group_names(),
            names(case.expected),
            "case {}: expected {:?}, got {:?}",
            case.label,
            case.expected,
            listed.group_names()
        );
    }
}

/// RULE: a `parentGroup` the account does not hold names a hierarchy that does not
/// exist, which is the registry family's 404 — not an empty page.
#[tokio::test]
async fn a_parent_group_the_account_does_not_hold_is_refused_not_found() {
    let (emulator, _arns) = with_hierarchy().await;

    // The rows span the ways a request can name a group the account does not hold: a
    // name nothing resembles, a name that differs only in case from one it holds, and a
    // name at each modelled length bound (`len 1..128`).
    let long_name = "n".repeat(128);
    let cases: [(&str, String); 4] = [
        ("an unrelated name", "no-such-group".to_string()),
        ("a name differing only in case", "Fleet".to_string()),
        ("the shortest modelled name", "x".to_string()),
        ("the longest modelled name", long_name.clone()),
    ];

    for (label, parent_group) in &cases {
        let refused = emulator
            .list_groups(&format!("parentGroup={parent_group}"))
            .await;
        assert_eq!(
            refused.status,
            404,
            "case {label}: expected 404, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "ResourceNotFoundException",
            "case {label}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.message(),
            format!("ThingGroup {ACCOUNT_ID}:{parent_group} not found."),
            "case {label}: got {}",
            refused.text()
        );
    }
}

/// RULE: the listing is paginated, so `maxResults` bounds a page and the token it
/// answers with resumes exactly where that page stopped — over the filtered listing as
/// over the whole one.
#[tokio::test]
async fn the_listing_pages_over_the_groups_it_reports() {
    let (emulator, _arns) = with_hierarchy().await;

    // The rows span what the rule generalises over: the page size (one row at a time up
    // to the modelled maximum of 250, `RegistryMaxResults` range 1..250) and the filter
    // the page is taken over.
    struct Case {
        label: &'static str,
        filter: &'static str,
        expected: &'static [&'static str],
    }
    let cases = [
        Case {
            label: "the whole account",
            filter: "",
            expected: &EVERY_GROUP,
        },
        Case {
            label: "a subtree",
            filter: "parentGroup=fleet&recursive=true",
            expected: &["fleet-east", "fleet-east-rack1", "fleet-west"],
        },
        Case {
            label: "a name prefix",
            filter: "namePrefixFilter=fleet",
            expected: &["fleet", "fleet-east", "fleet-east-rack1", "fleet-west"],
        },
    ];

    for case in &cases {
        for page_size in [1usize, 2, 4, 250] {
            let mut seen: Vec<String> = Vec::new();
            let mut query = if case.filter.is_empty() {
                format!("maxResults={page_size}")
            } else {
                format!("{}&maxResults={page_size}", case.filter)
            };
            loop {
                let page = emulator.list_groups(&query).await;
                assert_eq!(
                    page.status,
                    200,
                    "case {} at maxResults={page_size}: expected 200, got {} body {}",
                    case.label,
                    page.status,
                    page.text()
                );
                let page_names = page.group_names();
                assert!(
                    page_names.len() <= page_size,
                    "case {} at maxResults={page_size}: a page holds at most {page_size}, \
                     got {page_names:?}",
                    case.label
                );
                seen.extend(page_names);
                match page.next_token() {
                    Some(token) => {
                        query = if case.filter.is_empty() {
                            format!("maxResults={page_size}&nextToken={token}")
                        } else {
                            format!("{}&maxResults={page_size}&nextToken={token}", case.filter)
                        };
                    }
                    None => break,
                }
            }
            assert_eq!(
                seen,
                names(case.expected),
                "case {} at maxResults={page_size}: expected every group once in order, \
                 got {seen:?}",
                case.label
            );
        }
    }

    // A token this listing did not issue is refused rather than silently restarting it,
    // and so is one issued for a DIFFERENT filter set: the filters decide which groups
    // the listing walks, so a token cannot cross between them.
    let first = emulator.list_groups("maxResults=1").await;
    let token = first
        .next_token()
        .unwrap_or_else(|| panic!("the first page of five groups must be truncated: {}", first.text()));
    let refusals = [
        ("a token this listing never issued", "nextToken=not-a-token".to_string()),
        (
            "a token issued for the unfiltered listing, used with a prefix filter",
            format!("namePrefixFilter=fleet&nextToken={token}"),
        ),
        (
            "a token issued for the unfiltered listing, used with a hierarchy filter",
            format!("parentGroup=fleet&nextToken={token}"),
        ),
    ];
    for (label, query) in &refusals {
        let refused = emulator.list_groups(query).await;
        assert_eq!(
            refused.status,
            400,
            "case {label}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {label}: got {}",
            refused.text()
        );
    }

    // `recursive` narrows nothing without a `parentGroup`, so a request carrying it walks
    // the same groups as one that does not and the token of either resumes the other.
    let resumed = emulator
        .list_groups(&format!("maxResults=1&recursive=true&nextToken={token}"))
        .await;
    assert_eq!(
        resumed.status,
        200,
        "case a token of the unfiltered listing, used with the inert recursive: expected 200, \
         got {} body {}",
        resumed.status,
        resumed.text()
    );
    assert_eq!(
        resumed.group_names(),
        names(&EVERY_GROUP[1..2]),
        "case a token of the unfiltered listing, used with the inert recursive: expected the \
         page after the first group, got {:?}",
        resumed.group_names()
    );
}
