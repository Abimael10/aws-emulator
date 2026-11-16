// SPDX-License-Identifier: CC0-1.0

//! WHICH of the two target refusals a request gets, driven over the wire against
//! the Data API mounted beside the two Redshift control planes that own the
//! warehouses.
//!
//! ONE rule, asserted over a TABLE of inputs rather than one recorded request:
//! the Data API resolves WHAT a request named against the control planes that
//! own it before it judges HOW MANY targets it named. So
//!
//! 1. a request that names no target selector at all has nothing to resolve and
//!    is refused with the sentence enumerating THAT operation's selectors;
//! 2. a request that names more than one is refused with that same sentence as
//!    long as one of the warehouses it named exists in the Region it was signed
//!    for — which endpoint it meant is then undecided — but is refused as naming
//!    no endpoint when every one of them is absent, because then which of them it
//!    meant decides nothing; and
//! 3. a request that names exactly one target is refused as naming no endpoint
//!    when that target is absent, and served when it is there.
//!
//! The rows differ along what the rule generalises over: the OPERATION that
//! resolves a target (both submits and all three catalogue readers), how many
//! selectors the request names, which selector it names and in which form (a
//! name, an ARN), whether each warehouse named exists, and the credentials the
//! request carries beside the selection.

use serde_json::{json, Value};

use super::emulator::{error_code, list, Warehouses};

/// The fixed sentence the service refuses a selected target that resolves to no
/// endpoint in the Region with. It names neither the cluster nor the workgroup
/// that could not be resolved, and no Region.
const NO_ENDPOINT: &str = "Redshift endpoint doesn't exist in this region.";

/// The sentence a SUBMIT operation refuses a selection that is not exactly one
/// target with: beside the two warehouse selectors its input models `SessionId`,
/// and a submission may address a Glue-managed target, so its refusal enumerates
/// all four
/// (`smithy_query(query="operation",operation="ExecuteStatement,BatchExecuteStatement")`).
const SUBMIT_SELECTORS: &str = "There should be only one of the following: ClusterIdentifier, \
                                WorkgroupName, Glue resource, SessionId";

/// The sentence a CATALOGUE reader refuses such a selection with: `ListDatabases`,
/// `ListTables` and `DescribeTable` model the two warehouse selectors and no
/// other
/// (`smithy_query(query="operation",operation="ListDatabases,ListTables,DescribeTable")`).
const READER_SELECTORS: &str = "There should be only one of the following: ClusterIdentifier, \
                                WorkgroupName";

/// A `SessionId` in the form the modelled `UUID` pattern accepts. Whether a
/// session with it exists is never reached by the rows that use it: a selection
/// naming two targets is refused before any session is looked up.
const SESSION_ID: &str = "3dc6a4b5-5e2b-4b5a-9f1d-6b6d2b6c7a8e";

/// The database both warehouses below are CREATED with, so every request in this
/// suite connects through a database its warehouse really holds and what decides
/// each answer is the TARGET.
const PROVISIONED_DATABASE: &str = "dev";

/// The table the two present warehouses are given, so the row that describes a
/// table on a target that exists is about the target and not about a missing
/// table.
const TABLE: &str = "orders";

/// The `Message` an error body carries, or `None` when it carries none.
fn message(response: &Value) -> Option<&str> {
    response["Message"].as_str()
}

/// One operation that resolves a compute target: its name, the members it needs
/// beside the selection (`Database` is `@required` on `ListTables` and
/// `DescribeTable`, and a submit carries its statements), and the sentence it
/// states for a selection that is not exactly one target.
struct Resolver {
    operation: &'static str,
    base: Value,
    selectors: &'static str,
}

/// Every operation of this service that resolves a compute target.
fn resolvers() -> Vec<Resolver> {
    vec![
        Resolver {
            operation: "ExecuteStatement",
            base: json!({"Database": PROVISIONED_DATABASE, "Sql": "SELECT 1"}),
            selectors: SUBMIT_SELECTORS,
        },
        Resolver {
            operation: "BatchExecuteStatement",
            base: json!({"Database": PROVISIONED_DATABASE, "Sqls": ["SELECT 1"]}),
            selectors: SUBMIT_SELECTORS,
        },
        Resolver {
            operation: "ListDatabases",
            base: json!({"Database": PROVISIONED_DATABASE}),
            selectors: READER_SELECTORS,
        },
        Resolver {
            operation: "ListTables",
            base: json!({"Database": PROVISIONED_DATABASE}),
            selectors: READER_SELECTORS,
        },
        Resolver {
            operation: "DescribeTable",
            base: json!({"Database": PROVISIONED_DATABASE, "Table": TABLE}),
            selectors: READER_SELECTORS,
        },
    ]
}

/// `base` with every member of `overrides` set on it.
fn with(base: &Value, overrides: &Value) -> Value {
    let mut request = base.clone();
    for (member, value) in overrides.as_object().expect("an object of members") {
        request[member] = value.clone();
    }
    request
}

/// The two warehouses this suite resolves against — a provisioned cluster and a
/// serverless workgroup, both created with [`PROVISIONED_DATABASE`] and both
/// holding [`TABLE`] — and the workgroup's own ARN.
async fn warehouses() -> (Warehouses, String) {
    let warehouses = Warehouses::new();
    warehouses
        .create_cluster_with_database("present-cluster", Some(PROVISIONED_DATABASE))
        .await;
    let arn = warehouses
        .create_workgroup_with_database("present-workgroup", Some(PROVISIONED_DATABASE))
        .await;
    (warehouses, arn)
}

/// Run `sql` to completion on `selection`'s warehouse, so what the catalogue
/// readers are asked about really exists there.
async fn run(warehouses: &Warehouses, selection: &Value, sql: &str) {
    let request = with(
        &json!({
            "Database": PROVISIONED_DATABASE,
            "DbUser": "awsuser",
            "Sql": sql,
            "WaitTimeSeconds": 30,
        }),
        selection,
    );
    let submitted = warehouses.data_ok("ExecuteStatement", &request).await;
    assert_eq!(
        submitted["Status"], "FINISHED",
        "{sql} on {selection} must run to completion, got {submitted}"
    );
}

/// Rule 1 and 2: a selection that is not exactly one target is refused on the
/// SELECTION, in the words of the operation that was called — as long as there is
/// an endpoint to be ambiguous about, and whatever else the request carries.
#[tokio::test]
async fn a_selection_that_is_not_exactly_one_target_is_refused_on_its_selection() {
    let (warehouses, present_arn) = warehouses().await;

    // Each row is a selection that does not name exactly one target: none at
    // all, or more than one of which at least one really exists. The rows
    // naming two differ in which pair they name, in the form each is named in,
    // and in which of them exists.
    let selections: [(&str, Value); 6] = [
        // Nothing was named, so there is nothing to resolve and nothing to
        // report but the selectors the caller must name one of.
        ("no target at all", json!({})),
        (
            "two warehouses the Region holds",
            json!({"ClusterIdentifier": "present-cluster", "WorkgroupName": "present-workgroup"}),
        ),
        (
            "a present cluster beside an absent workgroup",
            json!({"ClusterIdentifier": "present-cluster", "WorkgroupName": "absent-workgroup"}),
        ),
        (
            "a present workgroup named by ARN beside an absent cluster",
            json!({"ClusterIdentifier": "absent-cluster", "WorkgroupName": present_arn}),
        ),
        (
            "a present cluster beside a session",
            json!({"ClusterIdentifier": "present-cluster", "SessionId": SESSION_ID}),
        ),
        (
            "a present workgroup beside a session",
            json!({"WorkgroupName": "present-workgroup", "SessionId": SESSION_ID}),
        ),
    ];

    // Each row is what the request carries BESIDE its selection: the
    // authorization method, or none at all. The selection is what is wrong, so
    // none of them changes the answer.
    let contexts: [(&str, Value); 3] = [
        ("temporary credentials", json!({"DbUser": "awsuser"})),
        (
            "Secrets Manager",
            json!({"SecretArn": "arn:aws:secretsmanager:us-east-1:123456789012:secret:db-AbCdEf"}),
        ),
        ("no authorization member", json!({})),
    ];

    for (selection_case, selection) in &selections {
        for (context_case, context) in &contexts {
            for resolver in resolvers() {
                // A catalogue reader models no `SessionId`, so a selection
                // naming one cannot be sent to it at all.
                if selection.get("SessionId").is_some() && resolver.selectors == READER_SELECTORS {
                    continue;
                }
                let request = with(&with(&resolver.base, selection), context);
                let case = format!(
                    "{selection_case:?} with {context_case:?} on {}",
                    resolver.operation
                );
                let (status, response) = warehouses.data(resolver.operation, &request).await;
                assert_eq!(
                    status, 400,
                    "case {case}: expected 400, got {status} {response}"
                );
                assert_eq!(
                    error_code(&response),
                    "ValidationException",
                    "case {case}: expected ValidationException, got {response}"
                );
                assert_eq!(
                    message(&response),
                    Some(resolver.selectors),
                    "case {case}: expected {:?}, got {response}",
                    resolver.selectors
                );
                // The refusal is about the SELECTION, so it never reports the
                // endpoint instead — and it never names a selector the operation
                // does not model.
                let stated = message(&response).unwrap_or_default();
                assert!(
                    !stated.contains("endpoint"),
                    "case {case}: a selection fault must not be reported as an endpoint fault, \
                     got {stated:?}"
                );
                if resolver.selectors == READER_SELECTORS {
                    for absent in ["Glue", "SessionId"] {
                        assert!(
                            !stated.contains(absent),
                            "case {case}: {absent:?} is not a selector this operation models, got \
                             {stated:?}"
                        );
                    }
                }
            }
        }
    }

    // Nothing above was admitted.
    let (status, page) = warehouses.data("ListStatements", &json!({})).await;
    assert_eq!(status, 200, "ListStatements: expected 200, got {page}");
    let stored = list(&page, "Statements").len();
    assert_eq!(
        stored, 0,
        "a request refused on its selection stores no statement, got {stored} in {page}"
    );
}

/// Rule 2, the other half: a selection naming more than one target whose every
/// named warehouse is absent from the Region names no endpoint at all — which of
/// them it meant decides nothing — so it is refused as naming none, in the same
/// fixed words a lone absent target gets.
#[tokio::test]
async fn a_selection_whose_every_named_target_is_absent_names_no_endpoint() {
    let (warehouses, present_arn) = warehouses().await;
    let absent_arn = present_arn.replace(
        present_arn.rsplit('/').next().unwrap_or_default(),
        "00000000-0000-4000-8000-000000000000",
    );

    // Each row names more than one target and not one of them is there. The rows
    // differ in which pair they name, in the FORM each is named in (a name, an
    // ARN), in how many they name, and in how each fails to resolve (never
    // created, a typo, a session this service holds none of).
    let selections: [(&str, Value); 6] = [
        (
            "two warehouses the Region does not hold",
            json!({"ClusterIdentifier": "absent-cluster", "WorkgroupName": "absent-workgroup"}),
        ),
        (
            "two warehouse names with a typo each",
            json!({"ClusterIdentifier": "presnt-cluster", "WorkgroupName": "presnt-workgroup"}),
        ),
        (
            "an absent cluster beside the ARN of an absent workgroup",
            json!({"ClusterIdentifier": "absent-cluster", "WorkgroupName": absent_arn}),
        ),
        (
            "an absent cluster beside a session that is not live",
            json!({"ClusterIdentifier": "absent-cluster", "SessionId": SESSION_ID}),
        ),
        (
            "an absent workgroup beside a session that is not live",
            json!({"WorkgroupName": "absent-workgroup", "SessionId": SESSION_ID}),
        ),
        (
            "both absent warehouses beside a session that is not live",
            json!({
                "ClusterIdentifier": "absent-cluster",
                "WorkgroupName": "absent-workgroup",
                "SessionId": SESSION_ID,
            }),
        ),
    ];

    for (case, selection) in &selections {
        for resolver in resolvers() {
            // A catalogue reader models no `SessionId`, so a selection naming
            // one cannot be sent to it at all.
            if selection.get("SessionId").is_some() && resolver.selectors == READER_SELECTORS {
                continue;
            }
            let request = with(
                &with(&resolver.base, selection),
                &json!({"DbUser": "awsuser"}),
            );
            let case = format!("{case:?} on {}", resolver.operation);
            let (status, response) = warehouses.data(resolver.operation, &request).await;
            assert_eq!(
                status, 400,
                "case {case}: expected 400, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {case}: expected ValidationException, got {response}"
            );
            assert_eq!(
                message(&response),
                Some(NO_ENDPOINT),
                "case {case}: expected {NO_ENDPOINT:?}, got {response}"
            );
        }
    }

    // And the contrast, on one request each way: the SAME selection with one of
    // the warehouses it names present is refused on its selection instead, so
    // what decides which refusal a request gets is whether anything it named is
    // there.
    for (case, selection, expected) in [
        (
            "an absent cluster beside a present workgroup",
            json!({"ClusterIdentifier": "absent-cluster", "WorkgroupName": "present-workgroup"}),
            SUBMIT_SELECTORS,
        ),
        (
            "an absent workgroup beside a present cluster",
            json!({"ClusterIdentifier": "present-cluster", "WorkgroupName": "absent-workgroup"}),
            SUBMIT_SELECTORS,
        ),
    ] {
        let request = with(
            &json!({
                "Database": PROVISIONED_DATABASE,
                "DbUser": "awsuser",
                "Sql": "SELECT 1",
            }),
            &selection,
        );
        let (status, response) = warehouses.data("ExecuteStatement", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} {response}"
        );
        assert_eq!(
            message(&response),
            Some(expected),
            "case {case:?}: expected {expected:?}, got {response}"
        );
    }

    // Nothing above was admitted.
    let page = warehouses.data_ok("ListStatements", &json!({})).await;
    let stored = list(&page, "Statements").len();
    assert_eq!(
        stored, 0,
        "a request that names no endpoint stores no statement, got {stored} in {page}"
    );
}

/// Rule 3: a selection that names exactly ONE target is resolved against the
/// control plane that owns it — refused with the fixed absent-endpoint sentence
/// when the Region holds no such warehouse, and served when it does.
#[tokio::test]
async fn a_lone_target_is_refused_only_when_it_resolves_to_no_endpoint() {
    let (warehouses, present_arn) = warehouses().await;
    // The same ARN form for a workgroup that was never created: only the
    // workgroup id differs, so what decides the answer is the resource and not
    // the shape of the reference.
    let absent_arn = present_arn.replace(
        present_arn.rsplit('/').next().unwrap_or_default(),
        "00000000-0000-4000-8000-000000000000",
    );

    // The tables the catalogue readers are asked about exist on both warehouses
    // that do, so a row about a target that exists is never answered by a
    // missing table.
    for present in [
        json!({"ClusterIdentifier": "present-cluster"}),
        json!({"WorkgroupName": "present-workgroup"}),
    ] {
        run(
            &warehouses,
            &present,
            &format!("CREATE TABLE {TABLE} (id INTEGER)"),
        )
        .await;
    }

    // Each row is a selection naming exactly one target, and whether the Region
    // holds it. Both halves of the rule are rows of one table, so an emulator
    // that refused every target (or none) fails it. The rows differ in the KIND
    // of warehouse, the FORM it is named in, and how it fails to resolve (never
    // created, a typo).
    let targets: [(&str, Value, bool); 7] = [
        (
            "a cluster the Region holds",
            json!({"ClusterIdentifier": "present-cluster"}),
            true,
        ),
        (
            "a cluster the Region does not hold",
            json!({"ClusterIdentifier": "absent-cluster"}),
            false,
        ),
        (
            "a cluster identifier with a typo",
            json!({"ClusterIdentifier": "presnt-cluster"}),
            false,
        ),
        (
            "a workgroup the Region holds",
            json!({"WorkgroupName": "present-workgroup"}),
            true,
        ),
        (
            "a workgroup the Region does not hold",
            json!({"WorkgroupName": "absent-workgroup"}),
            false,
        ),
        (
            "the ARN of a workgroup the Region holds",
            json!({"WorkgroupName": present_arn}),
            true,
        ),
        (
            "the ARN of a workgroup the Region does not hold",
            json!({"WorkgroupName": absent_arn}),
            false,
        ),
    ];

    let mut admitted_statements = 0;
    for (case, selection, exists) in &targets {
        for resolver in resolvers() {
            let request = with(
                &with(&resolver.base, selection),
                &json!({"DbUser": "awsuser"}),
            );
            let case = format!("{case:?} on {}", resolver.operation);
            let (status, response) = warehouses.data(resolver.operation, &request).await;
            if *exists {
                assert_eq!(
                    status, 200,
                    "case {case}: a target the Region holds must be served, got {status} \
                     {response}"
                );
                if resolver.selectors == SUBMIT_SELECTORS {
                    assert!(
                        response["Id"].is_string(),
                        "case {case}: an admitted submission carries an Id, got {response}"
                    );
                    admitted_statements += 1;
                }
                continue;
            }
            assert_eq!(
                status, 400,
                "case {case}: expected 400, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {case}: expected ValidationException, got {response}"
            );
            assert_eq!(
                message(&response),
                Some(NO_ENDPOINT),
                "case {case}: expected {NO_ENDPOINT:?}, got {response}"
            );
            // Fixed text: neither the target the request named nor the Region it
            // was signed for reaches the message.
            for value in selection.as_object().expect("an object").values() {
                let named = value.as_str().unwrap_or_default();
                assert!(
                    !message(&response).unwrap_or_default().contains(named),
                    "case {case}: the message must carry no part of the target, got {response}"
                );
            }
        }
    }

    // Only the submissions made against a target that exists were admitted: the
    // history holds those and the two statements that built the tables, and
    // nothing for a target that resolved to no endpoint.
    let page = warehouses.data_ok("ListStatements", &json!({})).await;
    let stored = list(&page, "Statements").len();
    assert_eq!(
        stored,
        admitted_statements + 2,
        "only statements admitted against a target that exists are stored, got {stored} in {page}"
    );
}
