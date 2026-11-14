// SPDX-License-Identifier: CC0-1.0

//! `CreateMLTransform`: the WIDTH of the schema a find-matches transform is defined over,
//! and when that width is judged.
//!
//! RULE: a machine learning transform runs against "the columns and data types that this
//! transform can run against" (`MLTransform$Schema`), which the create resolves out of the
//! catalog tables its `InputRecordTables` address — and the find-matches algorithm tells
//! rows apart by "a column that uniquely identifies rows in the source table"
//! (`FindMatchesParameters$PrimaryKeyColumnName`) and then compares the columns BESIDE that
//! key, so it cannot be defined over fewer than a key column and one column to compare. A
//! create whose input record tables resolve to a narrower schema is refused with the
//! operation's modelled `InvalidInputException` (HTTP 400) and the service's fixed sentence
//! for that check, persisting nothing — and that judgement is made BEFORE the identity the
//! transform's task runs would be performed as, so a request that gets both the schema and
//! the role wrong is told about the data.
//!
//! A width this store cannot measure is not refused: an input record table is an ADDRESS
//! and not an existence claim, so a request naming no table the catalog holds describes a
//! schema the store knows nothing about and is admitted, which is the reading the rest of
//! this crate's transform suites register transforms under.
//!
//! The rows differ along what the rule generalises over: how many columns the schema
//! resolves to (the published minimum, one step under it, one step over it, and none at
//! all), how many tables contribute them, whether two tables name the same column, which
//! catalog the table is addressed in, whether the catalog holds the table at all, and — for
//! the ordering — what the account's own IAM says about the role the same request names.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler. The create body, the
//! role and the table helper are the ones [`crate::ml_transforms`] already defines, and the
//! IAM-backed emulator is the one [`crate::crawler_role_trust`] stands up.

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::MIN_FIND_MATCHES_SCHEMA_COLUMNS;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state::{role_arn, DEFAULT_PATH};
use smithy_server_runtime::MultiServiceRouter;

use crate::crawler_role_trust::{emulator_with_iam, glue_trust, seed_role, trust};
use crate::ml_transforms::{create_body, store_table};
use crate::wire::{call, emulator, error_code, error_message, ok, ACCOUNT_ID};

/// The whole `Message` a create over a too-narrow schema is refused with — the service's
/// fixed sentence for this check, written out here rather than read from the crate's own
/// constant so this suite pins the wording.
const SCHEMA_TOO_NARROW: &str = "Number of columns in schema less than the minimum allowed number";

/// The `Message` a create naming a role this service cannot assume is refused with: the
/// fixed text plus the ARN that addresses the role. Pinned here for the same reason.
fn role_not_assumable(role_arn: &str) -> String {
    format!("Unable to assume role for arn: {role_arn}")
}

/// One catalog table a row stages: its database, its name and the columns (name and type)
/// its storage descriptor lists.
type StagedTable<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

/// One row of the width table.
struct WidthRow<'a> {
    /// The case name every assertion of the row reports.
    case: &'a str,
    /// The tables the catalog holds before the create.
    staged: &'a [StagedTable<'a>],
    /// The `InputRecordTables` the create sends.
    input: Value,
    /// The columns the created transform must then report, or `None` when the create must
    /// be refused for a schema the algorithm cannot run against.
    created: Option<&'a [(&'a str, &'a str)]>,
}

/// The `Schema` of one transform projection, as pairs of name and data type.
fn schema_of(case: &str, transform: &Value) -> Vec<(String, String)> {
    transform["Schema"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: Schema is a list, got {transform}"))
        .iter()
        .map(|column| {
            (
                column["Name"].as_str().unwrap_or_default().to_string(),
                column["DataType"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Whether this account holds a transform named `name`.
async fn is_stored(router: &MultiServiceRouter, name: &str) -> bool {
    let listed = ok(router, "GetMLTransforms", &json!({})).await;
    listed
        .get("Transforms")
        .and_then(Value::as_array)
        .is_some_and(|transforms| {
            transforms
                .iter()
                .any(|transform| transform.get("Name") == Some(&json!(name)))
        })
}

#[tokio::test]
async fn a_transform_is_created_only_over_a_schema_the_algorithm_can_run_against() {
    // The rows below span the published minimum, which the crate states once: a key column
    // plus one column to compare it across.
    assert_eq!(
        MIN_FIND_MATCHES_SCHEMA_COLUMNS, 2,
        "the rows span a minimum of two columns, but the crate states {MIN_FIND_MATCHES_SCHEMA_COLUMNS}"
    );

    let rows: &[WidthRow<'_>] = &[
        WidthRow {
            case: "one column, a step under the minimum",
            staged: &[("narrow", "contacts", &[("email", "string")])],
            input: json!([{ "DatabaseName": "narrow", "TableName": "contacts" }]),
            created: None,
        },
        WidthRow {
            case: "a table whose storage descriptor lists no column at all",
            staged: &[("bare", "contacts", &[])],
            input: json!([{ "DatabaseName": "bare", "TableName": "contacts" }]),
            created: None,
        },
        WidthRow {
            case: "two tables naming one and the same column",
            staged: &[
                ("dup", "left", &[("email", "string")]),
                ("dup", "right", &[("email", "string")]),
            ],
            input: json!([
                { "DatabaseName": "dup", "TableName": "left" },
                { "DatabaseName": "dup", "TableName": "right" },
            ]),
            created: None,
        },
        WidthRow {
            case: "a one-column table addressed through the account's own catalog",
            staged: &[("narrow", "contacts", &[("email", "string")])],
            input: json!([{
                "DatabaseName": "narrow",
                "TableName": "contacts",
                "CatalogId": ACCOUNT_ID,
            }]),
            created: None,
        },
        WidthRow {
            case: "a one-column table beside a table the catalog does not hold",
            staged: &[("narrow", "contacts", &[("email", "string")])],
            input: json!([
                { "DatabaseName": "narrow", "TableName": "contacts" },
                { "DatabaseName": "narrow", "TableName": "nowhere" },
            ]),
            created: None,
        },
        WidthRow {
            case: "exactly the minimum of two columns",
            staged: &[(
                "wide",
                "contacts",
                &[("email", "string"), ("zip", "int")],
            )],
            input: json!([{ "DatabaseName": "wide", "TableName": "contacts" }]),
            created: Some(&[("email", "string"), ("zip", "int")]),
        },
        WidthRow {
            case: "one column past the minimum",
            staged: &[(
                "wide",
                "people",
                &[("email", "string"), ("zip", "int"), ("name", "string")],
            )],
            input: json!([{ "DatabaseName": "wide", "TableName": "people" }]),
            created: Some(&[
                ("email", "string"),
                ("zip", "int"),
                ("name", "string"),
            ]),
        },
        WidthRow {
            case: "two one-column tables naming different columns",
            staged: &[
                ("split", "left", &[("email", "string")]),
                ("split", "right", &[("zip", "int")]),
            ],
            input: json!([
                { "DatabaseName": "split", "TableName": "left" },
                { "DatabaseName": "split", "TableName": "right" },
            ]),
            created: Some(&[("email", "string"), ("zip", "int")]),
        },
        WidthRow {
            case: "no input record tables at all",
            staged: &[],
            input: json!([]),
            created: Some(&[]),
        },
        WidthRow {
            case: "only a table the catalog does not hold",
            staged: &[("wide", "contacts", &[("email", "string"), ("zip", "int")])],
            input: json!([{ "DatabaseName": "wide", "TableName": "nowhere" }]),
            created: Some(&[]),
        },
    ];

    for row in rows {
        let case = row.case;
        let router = emulator();
        for (database, table, columns) in row.staged {
            store_table(&router, database, table, columns).await;
        }

        let name = format!("over-{}", case.replace(' ', "-"));
        let body = create_body(&name, json!({ "InputRecordTables": row.input.clone() }));
        let (status, answer) = call(&router, "CreateMLTransform", &body).await;

        let Some(expected) = row.created else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the modelled InvalidInputException's 400, got {status} \
                 with {answer}"
            );
            assert!(
                error_code(&answer).ends_with("InvalidInputException"),
                "case {case:?}: expected InvalidInputException, got {}",
                error_code(&answer)
            );
            let message = error_message(&answer);
            assert_eq!(
                message, SCHEMA_TOO_NARROW,
                "case {case:?}: expected Message {SCHEMA_TOO_NARROW:?}, got {message:?}"
            );
            assert!(
                !is_stored(&router, &name).await,
                "case {case:?}: a refused create must store no transform, but {name} is listed"
            );
            continue;
        };

        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be admitted, got {status} with {answer}"
        );
        let id = answer["TransformId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a TransformId, got {answer}"))
            .to_string();
        // The create is proven by a READ of the record it stored, under the id it minted.
        let read = ok(&router, "GetMLTransform", &json!({ "TransformId": id })).await;
        let schema = schema_of(case, &read);
        let expected: Vec<(String, String)> = expected
            .iter()
            .map(|(name, data_type)| ((*name).to_string(), (*data_type).to_string()))
            .collect();
        assert_eq!(
            schema, expected,
            "case {case:?}: expected the catalog columns {expected:?}, got {read}"
        );
    }
}

/// One row of the ordering table: what the account's IAM holds for the role the create
/// names, how wide the schema it resolves is, and the refusal (or admission) it earns.
struct OrderRow<'a> {
    /// The case name every assertion of the row reports.
    case: &'a str,
    /// The columns the one staged table lists.
    columns: &'a [(&'a str, &'a str)],
    /// The trust document the account holds for the row's role, or `None` for a row whose
    /// role the account does not hold at all.
    held: Option<Option<String>>,
    /// Whether the refusal must be the schema sentence (`true`), the role sentence
    /// (`false`), or neither because the create is admitted (`None`).
    schema_refused: Option<bool>,
}

#[tokio::test]
async fn the_schema_is_judged_before_the_role_the_task_runs_would_be_performed_as() {
    let narrow: &[(&str, &str)] = &[("email", "string")];
    let wide: &[(&str, &str)] = &[("email", "string"), ("zip", "int")];

    let rows: &[OrderRow<'_>] = &[
        OrderRow {
            case: "a too-narrow schema and a role the account does not hold",
            columns: narrow,
            held: None,
            schema_refused: Some(true),
        },
        OrderRow {
            case: "a too-narrow schema and a role whose trust document denies Glue",
            columns: narrow,
            held: Some(Some(trust(
                "Allow",
                "lambda.amazonaws.com",
                "sts:AssumeRole",
            ))),
            schema_refused: Some(true),
        },
        OrderRow {
            case: "a too-narrow schema and a role Glue can assume",
            columns: narrow,
            held: Some(Some(glue_trust())),
            schema_refused: Some(true),
        },
        OrderRow {
            case: "a wide enough schema and a role the account does not hold",
            columns: wide,
            held: None,
            schema_refused: Some(false),
        },
        OrderRow {
            case: "a wide enough schema and a role carrying no trust document",
            columns: wide,
            held: Some(None),
            schema_refused: Some(false),
        },
        OrderRow {
            case: "a wide enough schema and a role Glue can assume",
            columns: wide,
            held: Some(Some(glue_trust())),
            schema_refused: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let (router, engine) = emulator_with_iam();
        let database = format!("ordering_{index}");
        store_table(&router, &database, "contacts", row.columns).await;

        let role_name = format!("ordering-role-{index}");
        let role = match &row.held {
            Some(document) => seed_role(&engine, &role_name, document.clone()),
            // Not seeded, so IAM answers that this account holds no such role. The ARN is
            // the one IAM would hold it under.
            None => role_arn(DEFAULT_PATH, &role_name),
        };

        let name = format!("ordering-{index}");
        let body = create_body(
            &name,
            json!({
                "InputRecordTables": [{ "DatabaseName": database, "TableName": "contacts" }],
                "Role": role,
            }),
        );
        let (status, answer) = call(&router, "CreateMLTransform", &body).await;

        let Some(schema_refused) = row.schema_refused else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to be admitted, got {status} with {answer}"
            );
            continue;
        };

        assert_eq!(
            status, 400,
            "case {case:?}: expected the modelled InvalidInputException's 400, got {status} with \
             {answer}"
        );
        assert!(
            error_code(&answer).ends_with("InvalidInputException"),
            "case {case:?}: expected InvalidInputException, got {}",
            error_code(&answer)
        );
        let message = error_message(&answer);
        let expected = if schema_refused {
            SCHEMA_TOO_NARROW.to_string()
        } else {
            role_not_assumable(&role)
        };
        assert_eq!(
            message, expected,
            "case {case:?}: expected Message {expected:?}, got {message:?}"
        );
        assert!(
            !is_stored(&router, &name).await,
            "case {case:?}: a refused create must store no transform, but {name} is listed"
        );
    }
}
