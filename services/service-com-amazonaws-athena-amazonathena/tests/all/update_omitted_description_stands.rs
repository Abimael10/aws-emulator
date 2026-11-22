// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what an UPDATE of a described resource writes: the members the
//! request carries, and nothing else. Driven over the wire (awsJson1_1, `x-amz-target:
//! AmazonAthena.<Operation>`) through the same router the emulator boots, so every
//! assertion is about what an SDK client observes.
//!
//! THE RULE UNDER TEST: an update writes the members its request carries, so an optional
//! member the request leaves out keeps the value the resource already holds — there is no
//! member a write clears by omitting it — and every read path of that resource reports the
//! value that stands.
//!
//! The rule holds for every described resource of this service whose update carries an
//! optional `Description`, so each test asserts it over a TABLE whose rows differ along
//! what the update carries (a new description, none at all, where the resource held one and
//! where it held none), along the SIZE of the description that has to stand (one character
//! up to the modelled maximum of 1024), along how many updates in a row omit it, and along
//! which workgroup holds the resource. The prepared statement is read back through BOTH
//! read paths that report a `PreparedStatement` — `GetPreparedStatement` and
//! `BatchGetPreparedStatement` — because the member has to stand on each of them.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, create_work_group, router};

/// The workgroup every account has without creating one, which holds prepared statements
/// and saved queries like any other.
const DEFAULT_WORK_GROUP: &str = "primary";

/// One row of the update table.
struct Row {
    case: &'static str,
    /// The workgroup the resource is created in.
    work_group: &'static str,
    /// The description the create carries, or `None` for a create that carries none.
    created: Option<String>,
    /// The description each update carries, in order — `None` for an update that carries
    /// none. More than one row sends several updates, so a description has to stand across
    /// a sequence of writes rather than just one.
    updates: Vec<Option<String>>,
    /// The description every read must report once those updates are applied.
    expected: Option<String>,
}

/// The rows every test of this suite drives, over `work_group` for the ones a test creates
/// itself.
fn rows(work_group: &'static str) -> Vec<Row> {
    vec![
        Row {
            case: "an update carrying no description, over one the create stored",
            work_group,
            created: Some("the created description".to_string()),
            updates: vec![None],
            expected: Some("the created description".to_string()),
        },
        Row {
            case: "an update carrying another description",
            work_group,
            created: Some("the created description".to_string()),
            updates: vec![Some("the updated description".to_string())],
            expected: Some("the updated description".to_string()),
        },
        Row {
            case: "an update carrying a description where the create stored none",
            work_group,
            created: None,
            updates: vec![Some("described by the update".to_string())],
            expected: Some("described by the update".to_string()),
        },
        Row {
            case: "an update carrying no description where the create stored none either",
            work_group,
            created: None,
            updates: vec![None],
            expected: None,
        },
        Row {
            case: "three updates in a row, none of them carrying a description",
            work_group,
            created: Some("still the created description".to_string()),
            updates: vec![None, None, None],
            expected: Some("still the created description".to_string()),
        },
        Row {
            case: "an update that omits a description of the shortest modelled length",
            work_group,
            // `DescriptionString` is modelled with @length min 1 max 1024, so one
            // character is the shortest description a write may carry at all.
            created: Some("d".to_string()),
            updates: vec![None],
            expected: Some("d".to_string()),
        },
        Row {
            case: "an update that omits a description of the longest modelled length",
            work_group,
            // The other published bound of `DescriptionString`: 1024 characters.
            created: Some("d".repeat(1024)),
            updates: vec![None],
            expected: Some("d".repeat(1024)),
        },
        Row {
            case: "an update that replaces a description with one of the longest length",
            work_group,
            created: Some("short".to_string()),
            updates: vec![Some("x".repeat(1024))],
            expected: Some("x".repeat(1024)),
        },
        Row {
            case: "a resource in the workgroup every account has without creating one",
            work_group: DEFAULT_WORK_GROUP,
            created: Some("held by the default workgroup".to_string()),
            updates: vec![None],
            expected: Some("held by the default workgroup".to_string()),
        },
    ]
}

/// The query text a write carries at `position`, so each write of a row carries text of its
/// own and a read proves the update did write what it carried.
fn query_text(position: usize) -> String {
    format!("SELECT ? AS col_{position}")
}

/// `GetPreparedStatement`'s view of one statement: `(status, PreparedStatement)`.
async fn get_statement(router: &MultiServiceRouter, work_group: &str, name: &str) -> (u16, Value) {
    let (status, body) = call(
        router,
        "GetPreparedStatement",
        &json!({ "WorkGroup": work_group, "StatementName": name }),
    )
    .await;
    (status, body["PreparedStatement"].clone())
}

/// `BatchGetPreparedStatement`'s view of the same statement: `(status, PreparedStatements[0])`.
async fn batch_get_statement(
    router: &MultiServiceRouter,
    work_group: &str,
    name: &str,
) -> (u16, Value) {
    let (status, body) = call(
        router,
        "BatchGetPreparedStatement",
        &json!({ "WorkGroup": work_group, "PreparedStatementNames": [name] }),
    )
    .await;
    (status, body["PreparedStatements"][0].clone())
}

/// The description a read reported, as an `Option<&str>` — `None` when the member is absent
/// from the body, which is what a resource holding no description reports.
fn described(found: &Value) -> Option<&str> {
    found["Description"].as_str()
}

/// A prepared statement keeps the description that stands through every update that carries
/// none, and both read paths that report a `PreparedStatement` report it.
#[tokio::test]
async fn a_prepared_statement_keeps_a_description_an_update_does_not_carry() {
    let router = router();
    const WORK_GROUP: &str = "omitted-description-prepared-statements";
    create_work_group(&router, WORK_GROUP).await;

    for (index, row) in rows(WORK_GROUP).into_iter().enumerate() {
        let case = row.case;
        let name = format!("omitted_description_{index}");

        let mut create = json!({
            "WorkGroup": row.work_group,
            "StatementName": name,
            "QueryStatement": query_text(0),
        });
        if let Some(description) = &row.created {
            create["Description"] = json!(description);
        }
        let (status, created) = call(&router, "CreatePreparedStatement", &create).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the create must be accepted, got {status} with {created:?}"
        );

        let mut last_query = query_text(0);
        for (step, description) in row.updates.iter().enumerate() {
            last_query = query_text(step + 1);
            let mut update = json!({
                "WorkGroup": row.work_group,
                "StatementName": name,
                "QueryStatement": last_query,
            });
            if let Some(description) = description {
                update["Description"] = json!(description);
            }
            let (status, updated) = call(&router, "UpdatePreparedStatement", &update).await;
            assert_eq!(
                status, 200,
                "case {case:?}: update {step} must be accepted, got {status} with {updated:?}"
            );
        }

        let (status, read) = get_statement(&router, row.work_group, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the statement must be readable, got {status} with {read:?}"
        );
        assert_eq!(
            described(&read),
            row.expected.as_deref(),
            "case {case:?}: GetPreparedStatement expected the description {:?}, got {read:?}",
            row.expected
        );
        // The update DID write what it carried: the member the request named is the one it
        // changed, which is the same rule the standing description follows.
        assert_eq!(
            read["QueryStatement"].as_str(),
            Some(last_query.as_str()),
            "case {case:?}: expected the query statement the last write carried, got {read:?}"
        );

        let (status, batched) = batch_get_statement(&router, row.work_group, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the batch read must answer 200, got {status} with {batched:?}"
        );
        assert_eq!(
            described(&batched),
            row.expected.as_deref(),
            "case {case:?}: BatchGetPreparedStatement expected the description {:?}, got \
             {batched:?}",
            row.expected
        );
        assert_eq!(
            batched["StatementName"].as_str(),
            Some(name.as_str()),
            "case {case:?}: the batch read must report the statement asked for, got {batched:?}"
        );
    }
}

/// One batch read of SEVERAL statements reports each one's standing description, so the
/// member does not depend on how many statements a read hydrates at once.
#[tokio::test]
async fn a_batch_read_reports_every_statements_standing_description() {
    let router = router();
    const WORK_GROUP: &str = "omitted-description-batch";
    create_work_group(&router, WORK_GROUP).await;

    // (the statement name, the description the create carried, whether an update that
    //  carries no description follows)
    let statements: [(&str, Option<&str>, bool); 4] = [
        ("described_then_updated", Some("the first description"), true),
        ("described_only", Some("the second description"), false),
        ("undescribed_then_updated", None, true),
        ("undescribed_only", None, false),
    ];

    for (name, description, updated) in statements {
        let mut create = json!({
            "WorkGroup": WORK_GROUP,
            "StatementName": name,
            "QueryStatement": query_text(0),
        });
        if let Some(description) = description {
            create["Description"] = json!(description);
        }
        let (status, created) = call(&router, "CreatePreparedStatement", &create).await;
        assert_eq!(
            status, 200,
            "the create of {name} must be accepted, got {status} with {created:?}"
        );
        if updated {
            let (status, written) = call(
                &router,
                "UpdatePreparedStatement",
                &json!({
                    "WorkGroup": WORK_GROUP,
                    "StatementName": name,
                    "QueryStatement": query_text(1),
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "the update of {name} must be accepted, got {status} with {written:?}"
            );
        }
    }

    let names: Vec<&str> = statements.iter().map(|(name, _, _)| *name).collect();
    let (status, body) = call(
        &router,
        "BatchGetPreparedStatement",
        &json!({ "WorkGroup": WORK_GROUP, "PreparedStatementNames": names }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the batch read must answer 200, got {status} with {body:?}"
    );
    let found = body["PreparedStatements"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        found.len(),
        statements.len(),
        "expected one element per name asked for, got {body:?}"
    );

    for (name, description, _) in statements {
        let element = found
            .iter()
            .find(|element| element["StatementName"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("case {name:?}: the batch read must report it, got {body:?}"));
        assert_eq!(
            described(element),
            description,
            "case {name:?}: expected the description {description:?}, got {element:?}"
        );
    }
}

/// The same rule on the other described resource of this service: a saved query keeps the
/// description that stands through an update that carries none.
#[tokio::test]
async fn a_named_query_keeps_a_description_an_update_does_not_carry() {
    let router = router();
    const WORK_GROUP: &str = "omitted-description-named-queries";
    create_work_group(&router, WORK_GROUP).await;

    for (index, row) in rows(WORK_GROUP).into_iter().enumerate() {
        let case = row.case;
        let name = format!("omitted-description-{index}");

        let mut create = json!({
            "Name": name,
            "Database": "logs",
            "WorkGroup": row.work_group,
            "QueryString": query_text(0),
        });
        if let Some(description) = &row.created {
            create["Description"] = json!(description);
        }
        let (status, created) = call(&router, "CreateNamedQuery", &create).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the create must be accepted, got {status} with {created:?}"
        );
        let id = created["NamedQueryId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected an id, got {created:?}"))
            .to_string();

        let mut last_query = query_text(0);
        for (step, description) in row.updates.iter().enumerate() {
            last_query = query_text(step + 1);
            let mut update = json!({
                "NamedQueryId": id,
                "Name": name,
                "QueryString": last_query,
            });
            if let Some(description) = description {
                update["Description"] = json!(description);
            }
            let (status, updated) = call(&router, "UpdateNamedQuery", &update).await;
            assert_eq!(
                status, 200,
                "case {case:?}: update {step} must be accepted, got {status} with {updated:?}"
            );
        }

        let (status, body) = call(&router, "GetNamedQuery", &json!({ "NamedQueryId": id })).await;
        let read = body["NamedQuery"].clone();
        assert_eq!(
            status, 200,
            "case {case:?}: the saved query must be readable, got {status} with {read:?}"
        );
        assert_eq!(
            described(&read),
            row.expected.as_deref(),
            "case {case:?}: GetNamedQuery expected the description {:?}, got {read:?}",
            row.expected
        );
        assert_eq!(
            read["QueryString"].as_str(),
            Some(last_query.as_str()),
            "case {case:?}: expected the query text the last write carried, got {read:?}"
        );
    }
}
