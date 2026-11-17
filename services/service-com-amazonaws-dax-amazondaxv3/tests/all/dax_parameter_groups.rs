// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DAX parameter-group slice: `CreateParameterGroup`,
//! `DescribeParameterGroups`, `UpdateParameterGroup` and `DeleteParameterGroup` over the
//! wire.
//!
//! Every case drives `POST /` with `X-Amz-Target: AmazonDAXV3.<Operation>` through the
//! same `MultiServiceRouter` the production router builds, so what is asserted is the
//! HTTP behaviour a DAX SDK client sees — not an internal function call.
//!
//! The tables below span the CLASS each rule covers: the name and the description the
//! issue happened to cite are one row each, and the rules are asserted over names of
//! different lengths, shapes and orderings, over present/absent/empty descriptions, and
//! over every published `MaxResults` bound plus one past each.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_dax_amazondaxv3::{state, EmulatedService};
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// A role ARN to pass as `CreateCluster`'s required `IamRoleArn`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/DAXRole";

/// A node type to pass as `CreateCluster`'s required `NodeType`.
const NODE_TYPE: &str = "dax.r4.large";

/// Wait until every clock-owed cluster status move started before the call is due.
async fn settle() {
    tokio::time::sleep(state::STATUS_SETTLE_DELAY + Duration::from_millis(400)).await;
}

/// A router carrying only this service, over its own engine, so each test starts from a
/// fresh store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Call one operation and return its HTTP status and parsed JSON body.
async fn call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonDAXV3.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/dax/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The wire error code of a failed call: the `__type` member, without any namespace.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `ParameterGroupName`s a `DescribeParameterGroups` body reports, in order.
fn reported_names(body: &Value) -> Vec<String> {
    body["ParameterGroups"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .map(|group| {
                    group["ParameterGroupName"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One created group, as a name and the `Description` the create carried (`None` for a
/// request that names none).
type GroupCase = (&'static str, Option<&'static str>);

/// Names and descriptions spanning the class a create accepts: the name a group is
/// created under is read as a parameter-group IDENTIFIER (see
/// `creating_or_changing_a_parameter_group_named_by_a_non_identifier_is_refused` for the
/// class that is refused), and the model constrains the description to no pattern or
/// length (`smithy_query(query="shape", shape="CreateParameterGroupRequest")` — both
/// members target the unconstrained `String`), so a one-character name, a short one, a
/// hyphenated one, mixed case with a digit, a long one and an absent, present or empty
/// description are all one class.
const GROUP_CASES: [GroupCase; 6] = [
    ("p", Some("one character name")),
    ("pg", Some("two character name")),
    ("my-dax-parameters", Some("hyphenated name")),
    ("Group9", Some("mixed case and a digit")),
    ("no-description", None),
    (
        "a-rather-long-parameter-group-name-0123456789",
        Some(""),
    ),
];

// ---------------------------------------------------------------------------
// Create -> describe round trip.
// ---------------------------------------------------------------------------

/// Every created group is read back out of the store exactly as it was stored: the
/// create's own response, a describe naming it, and the unfiltered describe all report
/// the same name and description.
#[tokio::test]
async fn created_parameter_groups_round_trip_through_describe() {
    let router = emulator();

    for (name, description) in GROUP_CASES {
        let mut body = json!({ "ParameterGroupName": name });
        if let Some(description) = description {
            body["Description"] = json!(description);
        }
        let (status, created) = call(&router, "CreateParameterGroup", body).await;
        assert_eq!(status, 200, "case {name:?}: expected 200, got {created:?}");
        assert_eq!(
            created["ParameterGroup"]["ParameterGroupName"].as_str(),
            Some(name),
            "case {name:?}: create must echo the stored name, got {created:?}"
        );
        assert_eq!(
            created["ParameterGroup"]["Description"].as_str(),
            description,
            "case {name:?}: create must report the stored description, got {created:?}"
        );

        let (status, read) = call(
            &router,
            "DescribeParameterGroups",
            json!({ "ParameterGroupNames": [name] }),
        )
        .await;
        assert_eq!(status, 200, "case {name:?}: describe got {read:?}");
        assert_eq!(
            reported_names(&read),
            vec![name.to_string()],
            "case {name:?}: a describe naming one group reports only it, got {read:?}"
        );
        assert_eq!(
            read["ParameterGroups"][0]["Description"].as_str(),
            description,
            "case {name:?}: describe must report the stored description, got {read:?}"
        );
    }

    // The unfiltered describe reports every stored group, plus the default group DAX
    // provides, in one stable (sorted) order.
    let (status, all) = call(&router, "DescribeParameterGroups", json!({})).await;
    assert_eq!(status, 200, "describe all: got {all:?}");
    let mut expected: Vec<String> = GROUP_CASES
        .iter()
        .map(|(name, _)| (*name).to_string())
        .chain([state::DEFAULT_PARAMETER_GROUP.to_string()])
        .collect();
    expected.sort();
    assert_eq!(
        reported_names(&all),
        expected,
        "describe all: expected every stored group sorted, got {all:?}"
    );
    assert!(
        all["NextToken"].is_null(),
        "describe all: a single page must carry no NextToken, got {all:?}"
    );
}

/// A fresh account already has the parameter group AWS provides, and it is reported both
/// unfiltered and by name.
#[tokio::test]
async fn the_default_parameter_group_is_reported_before_anything_is_created() {
    let router = emulator();

    let (status, all) = call(&router, "DescribeParameterGroups", json!({})).await;
    assert_eq!(status, 200, "describe all: got {all:?}");
    assert_eq!(
        reported_names(&all),
        vec![state::DEFAULT_PARAMETER_GROUP.to_string()],
        "a fresh store reports exactly the default parameter group, got {all:?}"
    );

    let (status, named) = call(
        &router,
        "DescribeParameterGroups",
        json!({ "ParameterGroupNames": [state::DEFAULT_PARAMETER_GROUP] }),
    )
    .await;
    assert_eq!(status, 200, "describe default by name: got {named:?}");
    assert_eq!(
        named["ParameterGroups"][0]["Description"].as_str(),
        Some(state::DEFAULT_PARAMETER_GROUP_DESCRIPTION),
        "the default group carries its description, got {named:?}"
    );

    // A read leaves the store as it found it: a second describe answers the same thing.
    let (status, again) = call(&router, "DescribeParameterGroups", json!({})).await;
    assert_eq!(status, 200, "describe all again: got {again:?}");
    assert_eq!(
        reported_names(&again),
        reported_names(&all),
        "a describe must not change what a later caller sees, got {again:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule: the parameter group AWS provides is identified by the parameter group FAMILY
// whose defaults it carries — `default.<family>`, described as "Default parameter group
// for <family>" — and that name is the identifier a client passes back wherever a
// `ParameterGroupName` is taken, so no other spelling of it resolves.
// Source: `state::DEFAULT_PARAMETER_GROUP_FAMILY` (the family the catalog's defaults
// belong to) and the members that take the name back on the wire —
// `DescribeParameterGroups$ParameterGroupNames` and `CreateCluster$ParameterGroupName`
// (`smithy_query(query="operation",operation="CreateCluster,DescribeParameterGroups")`).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_default_parameter_group_is_identified_by_its_family() {
    // The identity is the family's, not an unrelated spelling: both members are the
    // family-qualified forms.
    assert_eq!(
        state::DEFAULT_PARAMETER_GROUP,
        format!("default.{}", state::DEFAULT_PARAMETER_GROUP_FAMILY),
        "case name: expected default.<family>, got {:?}",
        state::DEFAULT_PARAMETER_GROUP
    );
    assert_eq!(
        state::DEFAULT_PARAMETER_GROUP_DESCRIPTION,
        format!(
            "Default parameter group for {}",
            state::DEFAULT_PARAMETER_GROUP_FAMILY
        ),
        "case description: expected it to name the family, got {:?}",
        state::DEFAULT_PARAMETER_GROUP_DESCRIPTION
    );

    let router = emulator();
    let (status, all) = call(&router, "DescribeParameterGroups", json!({})).await;
    assert_eq!(status, 200, "describe all: got {all:?}");
    assert_eq!(
        all["ParameterGroups"][0]["ParameterGroupName"].as_str(),
        Some(state::DEFAULT_PARAMETER_GROUP),
        "case unfiltered: expected the family-qualified name, got {all:?}"
    );
    assert_eq!(
        all["ParameterGroups"][0]["Description"].as_str(),
        Some(state::DEFAULT_PARAMETER_GROUP_DESCRIPTION),
        "case unfiltered: expected the family's description, got {all:?}"
    );

    // Rows: spellings that drop part of the identifier — the family, its version, the
    // separator — plus the family on its own. None of them names a group, so a client
    // that guessed one is refused rather than served the default group by accident.
    for absent in [
        "default.dax",
        "default",
        "default.dax1",
        "default-dax1.0",
        state::DEFAULT_PARAMETER_GROUP_FAMILY,
    ] {
        let (status, body) = call(
            &router,
            "DescribeParameterGroups",
            json!({ "ParameterGroupNames": [absent] }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ParameterGroupNotFoundFault"),
            "case {absent:?}: only the family-qualified name resolves, got {status} {body:?}"
        );
    }

    // The name reads back as an identifier: a describe naming it resolves, and a cluster
    // created with it reports it unchanged.
    let (status, named) = call(
        &router,
        "DescribeParameterGroups",
        json!({ "ParameterGroupNames": [state::DEFAULT_PARAMETER_GROUP] }),
    )
    .await;
    assert_eq!(status, 200, "case describe by name: got {named:?}");
    assert_eq!(
        reported_names(&named),
        vec![state::DEFAULT_PARAMETER_GROUP.to_string()],
        "case describe by name: expected only the default group, got {named:?}"
    );

    let (status, created) = call(
        &router,
        "CreateCluster",
        json!({
            "ClusterName": "default-params",
            "NodeType": NODE_TYPE,
            "ReplicationFactor": 1,
            "IamRoleArn": ROLE_ARN,
            "ParameterGroupName": state::DEFAULT_PARAMETER_GROUP,
        }),
    )
    .await;
    assert_eq!(status, 200, "case CreateCluster: got {created:?}");
    assert_eq!(
        created["Cluster"]["ParameterGroup"]["ParameterGroupName"].as_str(),
        Some(state::DEFAULT_PARAMETER_GROUP),
        "case CreateCluster: expected the group it was created with, got {created:?}"
    );
}

// ---------------------------------------------------------------------------
// Refusals: an unknown name, a name already taken.
// ---------------------------------------------------------------------------

/// A name that names no stored group is answered with `ParameterGroupNotFoundFault`
/// by both the describe and the delete — including a name that only differs
/// from a stored one in case, and a name whose group has just been deleted.
#[tokio::test]
async fn an_unknown_parameter_group_name_is_refused_with_not_found() {
    let router = emulator();
    let (status, body) = call(
        &router,
        "CreateParameterGroup",
        json!({ "ParameterGroupName": "stored-group" }),
    )
    .await;
    assert_eq!(status, 200, "create stored-group: got {body:?}");

    // Absent names spanning the class: never created, differing only in case from a
    // stored one, the empty name, and one that differs by a single character.
    for absent in ["never-created", "STORED-GROUP", "", "stored-grou"] {
        let (status, body) = call(
            &router,
            "DescribeParameterGroups",
            json!({ "ParameterGroupNames": [absent] }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ParameterGroupNotFoundFault"),
            "case {absent:?}: describe of an absent group must be 400 \
             ParameterGroupNotFoundFault, got {status} {body:?}"
        );
        assert_eq!(
            body["message"].as_str().unwrap_or_default(),
            state::parameter_group_not_found_message(),
            "case {absent:?}: a read of a group it addresses answers that path's own \
             template, got {body:?}"
        );
    }

    // The same absent names on the path that REMOVES a group. The empty name is not one of
    // its rows: a name a change is addressed by is read as an identifier before the store
    // is consulted, so it is refused on that rule instead — see
    // `creating_or_changing_a_parameter_group_named_by_a_non_identifier_is_refused`.
    for absent in ["never-created", "STORED-GROUP", "stored-grou"] {
        let (status, body) = call(
            &router,
            "DeleteParameterGroup",
            json!({ "ParameterGroupName": absent }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ParameterGroupNotFoundFault"),
            "case {absent:?}: delete of an absent group must be 400 \
             ParameterGroupNotFoundFault, got {status} {body:?}"
        );
    }

    // One absent name among present ones is still a refusal, and the present ones are
    // not reported in its place.
    let (status, body) = call(
        &router,
        "DescribeParameterGroups",
        json!({ "ParameterGroupNames": ["stored-group", "never-created"] }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ParameterGroupNotFoundFault"),
        "a mixed list must be refused on the absent name, got {status} {body:?}"
    );
}

/// A name may hold one parameter group: a second create under a taken name is refused and
/// writes nothing. The provided group's name is not among the rows: it is not an
/// identifier, so a create naming it is refused on that rule before the store is consulted
/// (`creating_or_changing_a_parameter_group_named_by_a_non_identifier_is_refused`).
#[tokio::test]
async fn creating_a_parameter_group_under_a_taken_name_is_refused() {
    let router = emulator();

    for (name, description) in [("taken", Some("the first create")), ("also-taken", None)] {
        let mut body = json!({ "ParameterGroupName": name });
        if let Some(description) = description {
            body["Description"] = json!(description);
        }
        let (status, created) = call(&router, "CreateParameterGroup", body).await;
        assert_eq!(status, 200, "case {name:?}: first create got {created:?}");
    }

    for (name, taken_description) in [("taken", Some("the first create")), ("also-taken", None)] {
        let (status, body) = call(
            &router,
            "CreateParameterGroup",
            json!({ "ParameterGroupName": name, "Description": "a second create" }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ParameterGroupAlreadyExistsFault"),
            "case {name:?}: expected 400 ParameterGroupAlreadyExistsFault, got {status} {body:?}"
        );
        // Like this service's other parameter-group fault, the template is one fixed
        // sentence that echoes nothing of the request.
        assert_eq!(
            body["message"].as_str(),
            state::parameter_group_already_exists().message.as_deref(),
            "case {name:?}: expected the fault's own template, got {body:?}"
        );
        assert!(
            !body["message"].as_str().unwrap_or_default().contains(name),
            "case {name:?}: the refusal must not echo the requested name, got {body:?}"
        );

        // Nothing was written: the stored description is still the first one.
        let (status, read) = call(
            &router,
            "DescribeParameterGroups",
            json!({ "ParameterGroupNames": [name] }),
        )
        .await;
        assert_eq!(status, 200, "case {name:?}: describe got {read:?}");
        assert_eq!(
            read["ParameterGroups"][0]["Description"].as_str(),
            taken_description,
            "case {name:?}: a refused create must not overwrite the stored group, got {read:?}"
        );
    }
}

/// `ParameterGroupName` is `@required`, which the empty string satisfies; it names
/// nothing a later describe or delete could resolve, so the create is refused.
#[tokio::test]
async fn creating_a_parameter_group_with_an_empty_name_is_refused() {
    let router = emulator();
    let (status, body) = call(
        &router,
        "CreateParameterGroup",
        json!({ "ParameterGroupName": "" }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "InvalidParameterValueException"),
        "an empty ParameterGroupName must be 400 InvalidParameterValueException, \
         got {status} {body:?}"
    );

    let (status, all) = call(&router, "DescribeParameterGroups", json!({})).await;
    assert_eq!(status, 200, "describe all: got {all:?}");
    assert_eq!(
        reported_names(&all),
        vec![state::DEFAULT_PARAMETER_GROUP.to_string()],
        "a refused create must persist nothing, got {all:?}"
    );
}

// ---------------------------------------------------------------------------
// Paging: the documented MaxResults window, and the continuation token.
// ---------------------------------------------------------------------------

/// "The value for MaxResults must be between 20 and 100" (`MaxResults`): each published
/// bound is accepted, one step past either is refused with
/// `InvalidParameterValueException`, and the refusal carries no state change.
#[tokio::test]
async fn max_results_is_accepted_only_inside_the_documented_window() {
    let router = emulator();

    // Each published bound, one inside it, and one step past each bound, plus the
    // degenerate values a client can still send.
    let accepted = [
        state::MIN_MAX_RESULTS,
        state::MIN_MAX_RESULTS + 1,
        state::MAX_MAX_RESULTS - 1,
        state::MAX_MAX_RESULTS,
    ];
    for max_results in accepted {
        let (status, body) = call(
            &router,
            "DescribeParameterGroups",
            json!({ "MaxResults": max_results }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case MaxResults={max_results}: expected 200, got {body:?}"
        );
    }

    let refused = [
        state::MIN_MAX_RESULTS - 1,
        state::MAX_MAX_RESULTS + 1,
        1,
        0,
        -1,
        i32::MAX,
    ];
    for max_results in refused {
        let (status, body) = call(
            &router,
            "DescribeParameterGroups",
            json!({ "MaxResults": max_results }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterValueException"),
            "case MaxResults={max_results}: expected 400 InvalidParameterValueException, \
             got {status} {body:?}"
        );
    }
}

/// A page is at most `MaxResults` long, its `NextToken` continues exactly where it left
/// off, and following the chain reports every stored group once, in sorted order. A token
/// this service never issued is refused rather than answering an arbitrary page.
#[tokio::test]
async fn describe_pages_through_every_stored_parameter_group() {
    let router = emulator();

    // More groups than one minimum-sized page holds, so the chain has several pages.
    let total = 2 * state::MIN_MAX_RESULTS + 5;
    let mut expected: Vec<String> = vec![state::DEFAULT_PARAMETER_GROUP.to_string()];
    for index in 0..total {
        let name = format!("group-{index:03}");
        let (status, body) = call(
            &router,
            "CreateParameterGroup",
            json!({ "ParameterGroupName": name }),
        )
        .await;
        assert_eq!(status, 200, "case {name:?}: create got {body:?}");
        expected.push(name);
    }
    expected.sort();

    for page_size in [state::MIN_MAX_RESULTS, state::MAX_MAX_RESULTS] {
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut body = json!({ "MaxResults": page_size });
            if let Some(token) = token.as_deref() {
                body["NextToken"] = json!(token);
            }
            let (status, page) = call(&router, "DescribeParameterGroups", body).await;
            assert_eq!(
                status, 200,
                "case page_size={page_size} page {pages}: got {page:?}"
            );
            let names = reported_names(&page);
            assert!(
                names.len() <= usize::try_from(page_size).expect("a positive page size"),
                "case page_size={page_size} page {pages}: a page may hold at most \
                 {page_size} groups, got {} ({page:?})",
                names.len()
            );
            seen.extend(names);
            pages += 1;
            assert!(
                pages <= expected.len(),
                "case page_size={page_size}: paging did not terminate after {pages} pages"
            );
            match page["NextToken"].as_str() {
                Some(next) => token = Some(next.to_string()),
                None => break,
            }
        }
        assert_eq!(
            seen, expected,
            "case page_size={page_size}: the chain must report every stored group once, \
             in sorted order"
        );
    }

    for bogus in ["not-a-token", "!!!!", "Zm9v"] {
        let (status, body) = call(
            &router,
            "DescribeParameterGroups",
            json!({ "NextToken": bogus }),
        )
        .await;
        // "Zm9v" decodes to "foo", which no group is named: a token is only honoured
        // when it continues this service's own order, so a page is never fabricated.
        assert!(
            status == 400 || reported_names(&body).iter().all(|name| expected.contains(name)),
            "case {bogus:?}: a token this service never issued must be refused or \
             continue the real order, got {status} {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Delete.
// ---------------------------------------------------------------------------

/// A created group is gone after its delete — and gone from the unfiltered describe too.
#[tokio::test]
async fn deleting_a_parameter_group_removes_it_from_the_store() {
    let router = emulator();

    for (name, description) in GROUP_CASES {
        let mut body = json!({ "ParameterGroupName": name });
        if let Some(description) = description {
            body["Description"] = json!(description);
        }
        let (status, created) = call(&router, "CreateParameterGroup", body).await;
        assert_eq!(status, 200, "case {name:?}: create got {created:?}");
    }

    for (name, _) in GROUP_CASES {
        let (status, deleted) = call(
            &router,
            "DeleteParameterGroup",
            json!({ "ParameterGroupName": name }),
        )
        .await;
        assert_eq!(status, 200, "case {name:?}: delete got {deleted:?}");
        assert!(
            deleted["DeletionMessage"].as_str().is_some(),
            "case {name:?}: delete reports a DeletionMessage, got {deleted:?}"
        );

        let (status, read) = call(
            &router,
            "DescribeParameterGroups",
            json!({ "ParameterGroupNames": [name] }),
        )
        .await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (400, "ParameterGroupNotFoundFault"),
            "case {name:?}: a deleted group is gone, got {status} {read:?}"
        );
    }

    let (status, all) = call(&router, "DescribeParameterGroups", json!({})).await;
    assert_eq!(status, 200, "describe all: got {all:?}");
    assert_eq!(
        reported_names(&all),
        vec![state::DEFAULT_PARAMETER_GROUP.to_string()],
        "only the default group survives every delete, got {all:?}"
    );
}

/// The parameter group AWS provides is not the caller's to remove: the delete is refused
/// and the group is still reported afterwards.
#[tokio::test]
async fn deleting_the_default_parameter_group_is_refused() {
    let router = emulator();

    let (status, body) = call(
        &router,
        "DeleteParameterGroup",
        json!({ "ParameterGroupName": state::DEFAULT_PARAMETER_GROUP }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "InvalidParameterValueException"),
        "deleting the default group must be 400 InvalidParameterValueException, \
         got {status} {body:?}"
    );

    let (status, all) = call(&router, "DescribeParameterGroups", json!({})).await;
    assert_eq!(status, 200, "describe all: got {all:?}");
    assert_eq!(
        reported_names(&all),
        vec![state::DEFAULT_PARAMETER_GROUP.to_string()],
        "a refused delete leaves the default group in place, got {all:?}"
    );
}

// ---------------------------------------------------------------------------
// The name a parameter group is CREATED or CHANGED by, read as an identifier.
//
// Rule: the name a request creates a parameter group under, or addresses the group it
// changes by, is read as a parameter-group IDENTIFIER — it must begin with a letter, carry
// only ASCII letters, digits and hyphens, not end with a hyphen and not contain two
// consecutive hyphens — before the rest of the request and before the store is consulted
// for it. A name that is not one is refused 400 `InvalidParameterValueException` carrying
// the identifier rule itself, in text that is ENTIRELY fixed and interpolates no part of
// the request, and nothing is stored or changed. The group this service provides is named
// `default.` followed by its family, so the dot in its name is why no caller can change it.
//
// Source: the identifier rule the refusal states, and the paths that create or change a
// group by name — `CreateParameterGroup$ParameterGroupName`,
// `DeleteParameterGroup$ParameterGroupName` and `UpdateParameterGroup$ParameterGroupName`
// (`smithy_query(query="operation",operation="CreateParameterGroup,DeleteParameterGroup,
// UpdateParameterGroup")`, which models `InvalidParameterValueException` on all three). A
// path that READS a group is not one of them: the provided group is described and its
// parameters listed under its own `default.<family>` name, as the suites for those
// operations assert.
// ---------------------------------------------------------------------------

/// AWS's wording for a parameter-group name that is not a valid identifier, written out
/// here so what the wire carries is pinned to that text rather than to whatever the
/// service happens to build.
const NAME_NOT_AN_IDENTIFIER: &str = "The parameter CacheParameterGroupName is not a valid \
     identifier. Identifiers must begin with a letter; must contain only ASCII letters, \
     digits, and hyphens; and must not end with a hyphen or contain two consecutive \
     hyphens.";

/// Names that are NOT parameter-group identifiers, one row per way the rule can break: a
/// dotted name (the shape the provided group's own name has), an underscore, a space, a
/// leading digit, a leading hyphen, a trailing hyphen, two consecutive hyphens, a
/// non-ASCII letter, and the empty name the model's `@required` admits.
const NON_IDENTIFIER_NAMES: [&str; 9] = [
    "my.group",
    "has_underscore",
    "has space",
    "1leading-digit",
    "-leading-hyphen",
    "trailing-",
    "double--hyphen",
    "grüppe",
    "",
];

/// The group this service provides, resolved the way a client resolves it: the name
/// `DescribeParameterGroups` reports that begins `default.`.
async fn provided_group(router: &MultiServiceRouter) -> String {
    let (status, all) = call(router, "DescribeParameterGroups", json!({})).await;
    assert_eq!(status, 200, "describe all: got {status} {all:?}");
    reported_names(&all)
        .into_iter()
        .find(|name| name.starts_with("default."))
        .unwrap_or_else(|| panic!("the account has a provided parameter group, got {all:?}"))
}

/// Every path that CREATES a parameter group under `group` or CHANGES the one it names,
/// over bodies carrying different amounts of other content — including content that would
/// be refused on its own, which the NAME being read first takes precedence over.
fn create_or_change_requests(group: &str) -> Vec<(&'static str, Value)> {
    let published = state::DEFAULT_PARAMETERS[0].name.to_string();
    let every_published: Vec<(String, String)> = state::DEFAULT_PARAMETERS
        .iter()
        .map(|definition| (definition.name.to_string(), "1000".to_string()))
        .collect();
    vec![
        (
            "CreateParameterGroup",
            json!({ "ParameterGroupName": group }),
        ),
        (
            "CreateParameterGroup",
            json!({ "ParameterGroupName": group, "Description": "a description" }),
        ),
        (
            "DeleteParameterGroup",
            json!({ "ParameterGroupName": group }),
        ),
        (
            "UpdateParameterGroup",
            json!({
                "ParameterGroupName": group,
                "ParameterNameValues": name_values(&[(published, "1000".to_string())]),
            }),
        ),
        (
            "UpdateParameterGroup",
            json!({
                "ParameterGroupName": group,
                "ParameterNameValues": name_values(&every_published),
            }),
        ),
        // A pair naming a parameter the catalog does not publish, and a list naming no
        // parameter at all, are each refused on their own; the NAME is read first, so both
        // of these requests are refused on the name.
        (
            "UpdateParameterGroup",
            json!({
                "ParameterGroupName": group,
                "ParameterNameValues": name_values(&[(
                    "no-such-parameter".to_string(),
                    "1000".to_string(),
                )]),
            }),
        ),
        (
            "UpdateParameterGroup",
            json!({ "ParameterGroupName": group, "ParameterNameValues": [] }),
        ),
    ]
}

/// Every request that creates or changes a parameter group under a name that is not an
/// identifier is refused with the identifier rule, in fixed text that echoes nothing of the
/// request, and the store is left exactly as it was — whether that name is the provided
/// group's own (so nothing about it can be changed) or one no group is held under (so the
/// name is refused before the store is consulted, instead of answering not-found).
#[tokio::test]
async fn creating_or_changing_a_parameter_group_named_by_a_non_identifier_is_refused() {
    let router = emulator();
    let provided = provided_group(&router).await;
    // WHY the provided group's name is one of this class: a dot is not one of the
    // characters an identifier may carry.
    assert!(
        provided.contains('.'),
        "case {provided:?}: the provided group's name is not a valid identifier"
    );
    // A group held under a valid identifier, so the store is not empty while the class is
    // driven: no refusal below may touch it either.
    let held = "held-group";
    let (status, body) = call(
        &router,
        "CreateParameterGroup",
        json!({ "ParameterGroupName": held }),
    )
    .await;
    assert_eq!(status, 200, "case {held:?}: create got {status} {body:?}");
    let stored_names = vec![provided.clone(), held.to_string()];

    for name in NON_IDENTIFIER_NAMES
        .iter()
        .copied()
        .chain([provided.as_str()])
    {
        for (operation, body) in create_or_change_requests(name) {
            let case = format!("{operation} {body}");
            let (status, answered) = call(&router, operation, body).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "InvalidParameterValueException"),
                "case {case}: expected 400 InvalidParameterValueException, got {status} \
                 {answered:?}"
            );
            let message = answered["message"].as_str().unwrap_or_default();
            assert_eq!(
                message, NAME_NOT_AN_IDENTIFIER,
                "case {case}: expected the identifier rule, got {message:?}"
            );
            // The text is entirely fixed: no part of the request appears in it.
            if !name.is_empty() {
                assert!(
                    !message.contains(name),
                    "case {case}: the refusal interpolates the name it was given, got \
                     {message:?}"
                );
            }
        }

        // Nothing was stored under that name, and nothing the store held was removed.
        let (status, all) = call(&router, "DescribeParameterGroups", json!({})).await;
        assert_eq!(status, 200, "case {name:?}: describe all got {status} {all:?}");
        assert_eq!(
            reported_names(&all),
            stored_names,
            "case {name:?}: a refused create or change leaves the store alone, got {all:?}"
        );
    }

    // Every value of the provided group and of the held one is still the catalog's.
    for group in [provided.as_str(), held] {
        let stored = reported_values(&router, group).await;
        assert_eq!(
            stored,
            catalog_defaults(),
            "case {group:?}: a refused change leaves every value alone, got {stored:?}"
        );
    }
}

/// The identifier rule is not the answer to every refusal: a name that IS an identifier is
/// resolved against the store, so it either names a group (and the create, change or
/// removal is carried out) or names none (and the refusal is that path's own not-found).
#[tokio::test]
async fn a_name_that_is_an_identifier_is_resolved_instead_of_refused_on_the_rule() {
    let router = emulator();

    // Rows: a one-letter name, a hyphenated one, one carrying digits, and a mixed-case one
    // — each a valid identifier that names no group of this account.
    let pairs = vec![(
        state::DEFAULT_PARAMETERS[0].name.to_string(),
        "1000".to_string(),
    )];
    for absent in ["a", "my-group", "group9", "Params"] {
        let (status, answered) = call(
            &router,
            "DeleteParameterGroup",
            json!({ "ParameterGroupName": absent }),
        )
        .await;
        assert_eq!(
            error_code(&answered).as_str(),
            "ParameterGroupNotFoundFault",
            "case {absent:?}: expected the not-found refusal, got {status} {answered:?}"
        );
        let message = answered["message"].as_str().unwrap_or_default();
        assert_eq!(
            message,
            state::parameter_group_not_found_message(),
            "case {absent:?}: expected the not-found template, got {message:?}"
        );

        // The path that changes a group resolves such a name too, and names it in its own
        // not-found template.
        let (status, answered) = call(
            &router,
            "UpdateParameterGroup",
            json!({
                "ParameterGroupName": absent,
                "ParameterNameValues": name_values(&pairs),
            }),
        )
        .await;
        assert_eq!(
            error_code(&answered).as_str(),
            "ParameterGroupNotFoundFault",
            "case {absent:?}: expected the not-found refusal, got {status} {answered:?}"
        );
        assert_eq!(
            answered["message"].as_str().unwrap_or_default(),
            state::parameter_group_not_found_named_message(absent),
            "case {absent:?}: expected the update path's template, got {answered:?}"
        );
    }

    // And a group the caller creates under a valid identifier is the caller's to change
    // and to remove: the rule refuses names, not these operations.
    for created in ["b", "Removable-group9", "a-rather-long-but-valid-name-0123"] {
        let (status, body) = call(
            &router,
            "CreateParameterGroup",
            json!({ "ParameterGroupName": created }),
        )
        .await;
        assert_eq!(status, 200, "case {created:?}: create got {status} {body:?}");
        let (status, body) = call(
            &router,
            "UpdateParameterGroup",
            json!({
                "ParameterGroupName": created,
                "ParameterNameValues": name_values(&pairs),
            }),
        )
        .await;
        assert_eq!(status, 200, "case {created:?}: update got {status} {body:?}");
        let (status, body) = call(
            &router,
            "DeleteParameterGroup",
            json!({ "ParameterGroupName": created }),
        )
        .await;
        assert_eq!(status, 200, "case {created:?}: delete got {status} {body:?}");
    }
}

/// "You cannot delete a parameter group if it is associated with any DAX clusters"
/// (`DeleteParameterGroup`): while a cluster references the group the delete is refused,
/// and once no cluster does it succeeds.
#[tokio::test]
async fn deleting_a_parameter_group_a_cluster_uses_is_refused() {
    let router = emulator();
    let group = "cluster-params";
    let (status, body) = call(
        &router,
        "CreateParameterGroup",
        json!({ "ParameterGroupName": group }),
    )
    .await;
    assert_eq!(status, 200, "create {group}: got {body:?}");

    // Two clusters reference it, so the refusal is not keyed to a single holder.
    for cluster in ["pgcluster-one", "pgcluster-two"] {
        let (status, body) = call(
            &router,
            "CreateCluster",
            json!({
                "ClusterName": cluster,
                "NodeType": NODE_TYPE,
                "ReplicationFactor": 1,
                "IamRoleArn": ROLE_ARN,
                "ParameterGroupName": group,
            }),
        )
        .await;
        assert_eq!(status, 200, "CreateCluster {cluster}: got {body:?}");
    }
    settle().await;

    let (status, body) = call(
        &router,
        "DeleteParameterGroup",
        json!({ "ParameterGroupName": group }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "InvalidParameterGroupStateFault"),
        "deleting a group a cluster uses must be 400 InvalidParameterGroupStateFault, \
         got {status} {body:?}"
    );

    // Still there: the refusal wrote nothing.
    let (status, read) = call(
        &router,
        "DescribeParameterGroups",
        json!({ "ParameterGroupNames": [group] }),
    )
    .await;
    assert_eq!(status, 200, "describe {group}: got {read:?}");

    // One holder gone is not enough; the group frees only when the last one is.
    let (status, body) = call(
        &router,
        "DeleteCluster",
        json!({ "ClusterName": "pgcluster-one" }),
    )
    .await;
    assert_eq!(status, 200, "DeleteCluster pgcluster-one: got {body:?}");
    settle().await;
    let (status, body) = call(
        &router,
        "DeleteParameterGroup",
        json!({ "ParameterGroupName": group }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "InvalidParameterGroupStateFault"),
        "one remaining holder still blocks the delete, got {status} {body:?}"
    );

    let (status, body) = call(
        &router,
        "DeleteCluster",
        json!({ "ClusterName": "pgcluster-two" }),
    )
    .await;
    assert_eq!(status, 200, "DeleteCluster pgcluster-two: got {body:?}");
    settle().await;

    let (status, deleted) = call(
        &router,
        "DeleteParameterGroup",
        json!({ "ParameterGroupName": group }),
    )
    .await;
    assert_eq!(
        status, 200,
        "with no cluster left the delete succeeds, got {deleted:?}"
    );
    let (status, read) = call(
        &router,
        "DescribeParameterGroups",
        json!({ "ParameterGroupNames": [group] }),
    )
    .await;
    assert_eq!(
        (status, error_code(&read).as_str()),
        (400, "ParameterGroupNotFoundFault"),
        "the group is gone after the delete, got {status} {read:?}"
    );
}

// ---------------------------------------------------------------------------
// The bound on how far one account's parameter-group store grows.
// ---------------------------------------------------------------------------

/// The store holds at most [`state::MAX_PARAMETER_GROUPS`] parameter groups, counting the
/// default group it starts with: every create up to the bound is accepted, the first one
/// past it is refused with `ParameterGroupQuotaExceededFault` ("You have attempted to
/// exceed the maximum number of parameter groups"), and that refusal stores nothing — so
/// no sequence of creates grows the store without limit. The names below span the bound
/// rather than one cited value: the last accepted create and the first refused one are
/// both asserted.
#[tokio::test]
async fn a_create_past_the_parameter_group_bound_is_refused() {
    let router = emulator();

    // The default group already occupies one of the slots.
    for index in 0..(state::MAX_PARAMETER_GROUPS - 1) {
        let name = format!("quota-{index:03}");
        let (status, body) = call(
            &router,
            "CreateParameterGroup",
            json!({ "ParameterGroupName": name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: a create inside the bound is accepted, got {status} {body:?}"
        );
    }

    let (status, body) = call(
        &router,
        "CreateParameterGroup",
        json!({ "ParameterGroupName": "one-group-too-many" }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ParameterGroupQuotaExceededFault"),
        "case one past the bound: expected 400 ParameterGroupQuotaExceededFault, \
         got {status} {body:?}"
    );

    // The refusal wrote nothing: the store still holds exactly the bound, and the
    // refused name resolves to no group.
    let (status, page) = call(
        &router,
        "DescribeParameterGroups",
        json!({ "MaxResults": state::MAX_MAX_RESULTS }),
    )
    .await;
    assert_eq!(status, 200, "DescribeParameterGroups: got {page:?}");
    assert_eq!(
        reported_names(&page).len(),
        state::MAX_PARAMETER_GROUPS,
        "case one past the bound: the store must still hold exactly {} groups, got {page:?}",
        state::MAX_PARAMETER_GROUPS
    );
    let (status, read) = call(
        &router,
        "DescribeParameterGroups",
        json!({ "ParameterGroupNames": ["one-group-too-many"] }),
    )
    .await;
    assert_eq!(
        (status, error_code(&read).as_str()),
        (400, "ParameterGroupNotFoundFault"),
        "case one past the bound: the refused name must name no group, got {status} {read:?}"
    );
}

// ---------------------------------------------------------------------------
// Update: the values a group holds for the parameters the caching software defines.
//
// Rule: `UpdateParameterGroup` stores, for the group it names, the value each
// name-value pair in the request carries for a parameter the caching software defines,
// and nothing else about that group or any other changes; the whole request is applied
// or none of it is.
// Source: `UpdateParameterGroup` ("Modifies the parameters of a parameter group. You can
// modify up to 20 parameters in a single request by submitting a list parameter name and
// value pairs") and `ParameterNameValues` ("record-ttl-millis and query-ttl-millis are
// the only supported parameter names"), with each parameter's own `DataType` and
// `AllowedValues` from the catalog this service publishes
// (`smithy_query(query="operation",operation="UpdateParameterGroup")`,
// `smithy_query(query="shape",shape="Parameter")`).
// ---------------------------------------------------------------------------

/// The value a group reports for each of its parameters, read back through
/// `DescribeParameters` — the operation a client reads a group's parameters with.
async fn reported_values(router: &MultiServiceRouter, group: &str) -> BTreeMap<String, String> {
    let (status, body) = call(
        router,
        "DescribeParameters",
        json!({ "ParameterGroupName": group }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {group:?}: DescribeParameters got {status} {body:?}"
    );
    body["Parameters"]
        .as_array()
        .map(|parameters| {
            parameters
                .iter()
                .map(|parameter| {
                    (
                        parameter["ParameterName"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        parameter["ParameterValue"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The values a group that has never been updated holds: the system-defined value of
/// every parameter the catalog publishes.
fn catalog_defaults() -> BTreeMap<String, String> {
    state::DEFAULT_PARAMETERS
        .iter()
        .map(|definition| {
            (
                definition.name.to_string(),
                definition.default_value.to_string(),
            )
        })
        .collect()
}

/// The `ParameterNameValues` list for `pairs`.
fn name_values(pairs: &[(String, String)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(name, value)| json!({ "ParameterName": name, "ParameterValue": value }))
            .collect(),
    )
}

/// The inclusive bounds the catalog publishes for one parameter's values.
fn published_range(definition: &state::ParameterDefinition) -> (i64, Option<i64>) {
    definition.allowed_range().unwrap_or_else(|| {
        panic!(
            "case {}: the catalog must publish a range, got {:?}",
            definition.name, definition.allowed_values
        )
    })
}

/// Create one group to update, and return its name.
async fn group_to_update(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(
        router,
        "CreateParameterGroup",
        json!({ "ParameterGroupName": name, "Description": "a group to update" }),
    )
    .await;
    assert_eq!(status, 200, "create {name}: got {status} {body:?}");
    name.to_string()
}

/// Every accepted update is read back out of the store: the update's own response reports
/// the stored group, `DescribeParameters` reports the values it persisted, every
/// parameter the request did not name keeps the value it had, and repeating the same
/// update reports the same thing.
///
/// The rows are built from the catalog rather than from one cited pair: each published
/// parameter on its own at the bottom of its published range, one step inside it, well
/// inside it and back at its own default, then every parameter in one request, and the
/// same request with the pairs in the other order.
#[tokio::test]
async fn an_accepted_update_round_trips_through_describe_parameters() {
    let router = emulator();

    let mut cases: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for definition in state::DEFAULT_PARAMETERS {
        let (min, max) = published_range(definition);
        let mut values = vec![min, min + 1, min + 60_000];
        if let Some(max) = max {
            values.push(max);
        }
        for value in values {
            cases.push((
                format!("{} = {value}", definition.name),
                vec![(definition.name.to_string(), value.to_string())],
            ));
        }
        cases.push((
            format!("{} back to its own default", definition.name),
            vec![(
                definition.name.to_string(),
                definition.default_value.to_string(),
            )],
        ));
    }
    cases.push((
        "every published parameter in one request".to_string(),
        state::DEFAULT_PARAMETERS
            .iter()
            .map(|definition| (definition.name.to_string(), "1234".to_string()))
            .collect(),
    ));
    cases.push((
        "every published parameter, pairs reversed".to_string(),
        state::DEFAULT_PARAMETERS
            .iter()
            .rev()
            .map(|definition| (definition.name.to_string(), "4321".to_string()))
            .collect(),
    ));

    for (index, (label, pairs)) in cases.iter().enumerate() {
        // A fresh group per row, so each row starts from the values a group that has
        // never been updated holds.
        let group = group_to_update(&router, &format!("updated-{index:03}")).await;

        let (status, updated) = call(
            &router,
            "UpdateParameterGroup",
            json!({
                "ParameterGroupName": group,
                "ParameterNameValues": name_values(pairs),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected 200, got {status} {updated:?}"
        );
        assert_eq!(
            updated["ParameterGroup"]["ParameterGroupName"].as_str(),
            Some(group.as_str()),
            "case {label:?}: the update reports the group it modified, got {updated:?}"
        );
        assert_eq!(
            updated["ParameterGroup"]["Description"].as_str(),
            Some("a group to update"),
            "case {label:?}: an update leaves the stored description alone, got {updated:?}"
        );

        // Exactly the named parameters carry their new value; every other one still
        // carries the value it had.
        let mut expected = catalog_defaults();
        for (name, value) in pairs {
            expected.insert(name.clone(), value.clone());
        }
        let stored = reported_values(&router, &group).await;
        assert_eq!(
            stored, expected,
            "case {label:?}: expected {expected:?}, got {stored:?}"
        );

        // The same update again is accepted and reports the same values.
        let (status, again) = call(
            &router,
            "UpdateParameterGroup",
            json!({
                "ParameterGroupName": group,
                "ParameterNameValues": name_values(pairs),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label:?}: a repeated update got {status} {again:?}"
        );
        let stored_again = reported_values(&router, &group).await;
        assert_eq!(
            stored_again, expected,
            "case {label:?}: a repeated update must report the same values, got {stored_again:?}"
        );

        // The group is still the one group of that name, and still carries its
        // description, when read through the describe a client lists groups with.
        let (status, read) = call(
            &router,
            "DescribeParameterGroups",
            json!({ "ParameterGroupNames": [&group] }),
        )
        .await;
        assert_eq!(status, 200, "case {label:?}: describe got {status} {read:?}");
        assert_eq!(
            reported_names(&read),
            vec![group.clone()],
            "case {label:?}: the updated group is reported once, got {read:?}"
        );

        // The modification is recorded against the group, beside its creation.
        let (status, events) = call(
            &router,
            "DescribeEvents",
            json!({ "SourceName": &group, "SourceType": state::SOURCE_TYPE_PARAMETER_GROUP }),
        )
        .await;
        assert_eq!(status, 200, "case {label:?}: DescribeEvents got {events:?}");
        let messages: Vec<String> = events["Events"]
            .as_array()
            .expect("Events is a list")
            .iter()
            .map(|event| event["Message"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            messages,
            vec![
                state::EVENT_PARAMETER_GROUP_CREATED.to_string(),
                state::EVENT_PARAMETER_GROUP_MODIFIED.to_string(),
                state::EVENT_PARAMETER_GROUP_MODIFIED.to_string(),
            ],
            "case {label:?}: each accepted update is recorded against the group, \
             got {events:?}"
        );
    }
}

/// An update changes the group it names and no other: the groups beside it, and the
/// parameter group AWS provides, still report the values they held.
#[tokio::test]
async fn an_update_changes_only_the_group_it_names() {
    let router = emulator();
    for name in ["left", "right"] {
        group_to_update(&router, name).await;
    }

    let changed: Vec<(String, String)> = state::DEFAULT_PARAMETERS
        .iter()
        .map(|definition| (definition.name.to_string(), "7777".to_string()))
        .collect();
    let (status, body) = call(
        &router,
        "UpdateParameterGroup",
        json!({
            "ParameterGroupName": "left",
            "ParameterNameValues": name_values(&changed),
        }),
    )
    .await;
    assert_eq!(status, 200, "update left: got {status} {body:?}");

    let mut expected = catalog_defaults();
    for (name, value) in &changed {
        expected.insert(name.clone(), value.clone());
    }
    let stored = reported_values(&router, "left").await;
    assert_eq!(
        stored, expected,
        "case left: expected {expected:?}, got {stored:?}"
    );

    for untouched in ["right", state::DEFAULT_PARAMETER_GROUP] {
        let stored = reported_values(&router, untouched).await;
        assert_eq!(
            stored,
            catalog_defaults(),
            "case {untouched:?}: a group nothing updated still reports the published \
             defaults, got {stored:?}"
        );
    }
}

/// A name that names no stored group is answered with `ParameterGroupNotFoundFault`
/// naming it — the template DAX answers on a path that modifies THROUGH a group name —
/// and nothing is created in its place.
#[tokio::test]
async fn updating_a_parameter_group_that_does_not_exist_is_refused() {
    let router = emulator();
    group_to_update(&router, "stored-group").await;
    group_to_update(&router, "deleted-group").await;
    let (status, body) = call(
        &router,
        "DeleteParameterGroup",
        json!({ "ParameterGroupName": "deleted-group" }),
    )
    .await;
    assert_eq!(status, 200, "delete deleted-group: got {status} {body:?}");

    let pairs = vec![(
        state::DEFAULT_PARAMETERS[0].name.to_string(),
        "1000".to_string(),
    )];
    // Absent names spanning the class: never created, one that differs only in case from
    // a stored one, one a single character off, and one whose group was just deleted. The
    // empty name is not among them: it is not an identifier, so an update naming it is
    // refused on that rule before the store is consulted
    // (`creating_or_changing_a_parameter_group_named_by_a_non_identifier_is_refused`).
    for absent in [
        "never-created",
        "STORED-GROUP",
        "stored-grou",
        "deleted-group",
    ] {
        let (status, body) = call(
            &router,
            "UpdateParameterGroup",
            json!({
                "ParameterGroupName": absent,
                "ParameterNameValues": name_values(&pairs),
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ParameterGroupNotFoundFault"),
            "case {absent:?}: expected 400 ParameterGroupNotFoundFault, got {status} {body:?}"
        );
        assert_eq!(
            body["message"].as_str().unwrap_or_default(),
            state::parameter_group_not_found_named_message(absent),
            "case {absent:?}: an update names the group it could not resolve, got {body:?}"
        );

        let (status, read) = call(
            &router,
            "DescribeParameterGroups",
            json!({ "ParameterGroupNames": [absent] }),
        )
        .await;
        assert_eq!(
            (status, error_code(&read).as_str()),
            (400, "ParameterGroupNotFoundFault"),
            "case {absent:?}: a refused update must create nothing, got {status} {read:?}"
        );
    }

    // The group that does exist was not touched by any of those refusals.
    let stored = reported_values(&router, "stored-group").await;
    assert_eq!(
        stored,
        catalog_defaults(),
        "case stored-group: a refusal elsewhere changes nothing, got {stored:?}"
    );
}

/// The parameter group AWS provides carries the family's own defaults, so it is not the
/// caller's to change: the update is refused and the group still reports those defaults.
#[tokio::test]
async fn updating_the_default_parameter_group_is_refused() {
    let router = emulator();

    for definition in state::DEFAULT_PARAMETERS {
        let pairs = vec![(definition.name.to_string(), "9999".to_string())];
        let (status, body) = call(
            &router,
            "UpdateParameterGroup",
            json!({
                "ParameterGroupName": state::DEFAULT_PARAMETER_GROUP,
                "ParameterNameValues": name_values(&pairs),
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterValueException"),
            "case {}: expected 400 InvalidParameterValueException, got {status} {body:?}",
            definition.name
        );

        let stored = reported_values(&router, state::DEFAULT_PARAMETER_GROUP).await;
        assert_eq!(
            stored,
            catalog_defaults(),
            "case {}: a refused update leaves the default group's values alone, got {stored:?}",
            definition.name
        );
    }
}

/// A group holds a value only for a parameter the caching software defines, so a name
/// outside the published catalog is refused with `InvalidParameterValueException` and the
/// whole request — including the pairs that were acceptable — changes nothing.
#[tokio::test]
async fn updating_a_parameter_the_catalog_does_not_publish_is_refused() {
    let router = emulator();
    let group = group_to_update(&router, "unknown-names").await;
    let published = state::DEFAULT_PARAMETERS[0].name;

    // Rows: a name unrelated to any published one, a published name in another case,
    // one with a trailing character, a truncated one, and one carrying whitespace.
    for unknown in [
        "ttl".to_string(),
        published.to_uppercase(),
        format!("{published}x"),
        published[..published.len() - 1].to_string(),
        format!(" {published}"),
    ] {
        for pairs in [
            vec![(unknown.clone(), "1000".to_string())],
            // Mixed with a pair that WOULD be accepted on its own: the request is
            // applied whole or not at all.
            vec![
                (published.to_string(), "1000".to_string()),
                (unknown.clone(), "1000".to_string()),
            ],
        ] {
            let (status, body) = call(
                &router,
                "UpdateParameterGroup",
                json!({
                    "ParameterGroupName": &group,
                    "ParameterNameValues": name_values(&pairs),
                }),
            )
            .await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "InvalidParameterValueException"),
                "case {unknown:?} ({} pairs): expected 400 InvalidParameterValueException, \
                 got {status} {body:?}",
                pairs.len()
            );

            let stored = reported_values(&router, &group).await;
            assert_eq!(
                stored,
                catalog_defaults(),
                "case {unknown:?} ({} pairs): a refused update must write nothing, \
                 got {stored:?}",
                pairs.len()
            );
        }
    }
}

/// A value is accepted only when it is one the catalog publishes for that parameter: a
/// whole number inside its `AllowedValues` range. Each published bound is accepted and
/// one step past it is refused, as is anything that is not a whole number; a refused
/// value changes nothing.
#[tokio::test]
async fn updating_a_parameter_outside_its_published_values_is_refused() {
    let router = emulator();
    let group = group_to_update(&router, "bad-values").await;

    for definition in state::DEFAULT_PARAMETERS {
        let (min, max) = published_range(definition);

        let mut accepted = vec![min.to_string(), (min + 1).to_string()];
        if let Some(max) = max {
            accepted.push(max.to_string());
        }
        for value in accepted {
            let pairs = vec![(definition.name.to_string(), value.clone())];
            let (status, body) = call(
                &router,
                "UpdateParameterGroup",
                json!({
                    "ParameterGroupName": &group,
                    "ParameterNameValues": name_values(&pairs),
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {} = {value:?}: a published value is accepted, got {status} {body:?}",
                definition.name
            );
            let stored = reported_values(&router, &group).await;
            assert_eq!(
                stored.get(definition.name).map(String::as_str),
                Some(value.as_str()),
                "case {} = {value:?}: expected the stored value to be it, got {stored:?}",
                definition.name
            );
        }

        // Back to the published default, so each refusal below is measured against it.
        let reset = vec![(
            definition.name.to_string(),
            definition.default_value.to_string(),
        )];
        let (status, body) = call(
            &router,
            "UpdateParameterGroup",
            json!({
                "ParameterGroupName": &group,
                "ParameterNameValues": name_values(&reset),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {} reset: got {status} {body:?}",
            definition.name
        );

        let mut refused = vec![
            (min - 1).to_string(),
            "not-a-number".to_string(),
            String::new(),
            "1.5".to_string(),
            format!(" {min}"),
            "0x10".to_string(),
        ];
        if let Some(max) = max {
            refused.push((max + 1).to_string());
        }
        for value in refused {
            let pairs = vec![(definition.name.to_string(), value.clone())];
            let (status, body) = call(
                &router,
                "UpdateParameterGroup",
                json!({
                    "ParameterGroupName": &group,
                    "ParameterNameValues": name_values(&pairs),
                }),
            )
            .await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, "InvalidParameterValueException"),
                "case {} = {value:?}: expected 400 InvalidParameterValueException, \
                 got {status} {body:?}",
                definition.name
            );
            let stored = reported_values(&router, &group).await;
            assert_eq!(
                stored.get(definition.name).map(String::as_str),
                Some(definition.default_value),
                "case {} = {value:?}: a refused value must not be stored, got {stored:?}",
                definition.name
            );
        }
    }
}

/// A request that names no parameter to change, or names one with nothing to set it to,
/// is refused: `ParameterNameValues` is `@required` and both members of a pair are
/// optional, so the empty list and a half-filled pair are what a client can still send.
#[tokio::test]
async fn an_update_that_names_nothing_to_change_is_refused() {
    let router = emulator();
    let group = group_to_update(&router, "names-nothing").await;
    let published = state::DEFAULT_PARAMETERS[0].name;

    // Rows: no pair at all, a pair with no name, a pair with an empty name, and a pair
    // with a name but no value.
    let cases: [(&str, Value); 4] = [
        ("no pair at all", json!([])),
        ("a pair with no name", json!([{ "ParameterValue": "1000" }])),
        (
            "a pair with an empty name",
            json!([{ "ParameterName": "", "ParameterValue": "1000" }]),
        ),
        (
            "a pair with no value",
            json!([{ "ParameterName": published }]),
        ),
    ];

    for (label, pairs) in cases {
        let (status, body) = call(
            &router,
            "UpdateParameterGroup",
            json!({
                "ParameterGroupName": &group,
                "ParameterNameValues": pairs,
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterValueException"),
            "case {label:?}: expected 400 InvalidParameterValueException, got {status} {body:?}"
        );
        let stored = reported_values(&router, &group).await;
        assert_eq!(
            stored,
            catalog_defaults(),
            "case {label:?}: a refused update must write nothing, got {stored:?}"
        );
    }
}

/// One request carries at most one value per parameter, and at most
/// [`state::MAX_PARAMETERS_PER_UPDATE`] pairs ("You can modify up to 20 parameters in a
/// single request"): either is refused with `InvalidParameterCombinationException` and
/// writes nothing.
#[tokio::test]
async fn an_update_that_names_a_parameter_twice_or_too_many_is_refused() {
    let router = emulator();
    let group = group_to_update(&router, "combinations").await;
    let published = state::DEFAULT_PARAMETERS[0].name;

    // Rows: the same name twice with different values, the same name twice with the same
    // value, and a pair repeated past the bound on how many one request may carry.
    let duplicated = [
        vec![
            (published.to_string(), "1000".to_string()),
            (published.to_string(), "2000".to_string()),
        ],
        vec![
            (published.to_string(), "1000".to_string()),
            (published.to_string(), "1000".to_string()),
        ],
    ];
    for pairs in &duplicated {
        let (status, body) = call(
            &router,
            "UpdateParameterGroup",
            json!({
                "ParameterGroupName": &group,
                "ParameterNameValues": name_values(pairs),
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "InvalidParameterCombinationException"),
            "case {pairs:?}: expected 400 InvalidParameterCombinationException, \
             got {status} {body:?}"
        );
        let stored = reported_values(&router, &group).await;
        assert_eq!(
            stored,
            catalog_defaults(),
            "case {pairs:?}: a refused update must write nothing, got {stored:?}"
        );
    }

    // One pair past the documented bound: refused on the bound itself, which the
    // refusal names.
    let too_many: Vec<(String, String)> = (0..=state::MAX_PARAMETERS_PER_UPDATE)
        .map(|index| (published.to_string(), index.to_string()))
        .collect();
    let (status, body) = call(
        &router,
        "UpdateParameterGroup",
        json!({
            "ParameterGroupName": &group,
            "ParameterNameValues": name_values(&too_many),
        }),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "InvalidParameterCombinationException"),
        "case one pair past the bound: expected 400 InvalidParameterCombinationException, \
         got {status} {body:?}"
    );
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains(&state::MAX_PARAMETERS_PER_UPDATE.to_string()),
        "case one pair past the bound: the refusal names the bound, got {body:?}"
    );
    let stored = reported_values(&router, &group).await;
    assert_eq!(
        stored,
        catalog_defaults(),
        "case one pair past the bound: a refused update must write nothing, got {stored:?}"
    );
}
