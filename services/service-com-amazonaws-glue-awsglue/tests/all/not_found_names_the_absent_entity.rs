// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH entity a Glue refusal names when a request addresses one that
//! is not there, across the two surfaces that resolve an entity through an address of
//! several parts: the tagging surface, which resolves a `ResourceArn`, and the
//! column-statistics schedule pair, which resolves a table's settings record.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rule under test: a refusal reports the entity the operation itself looks up, in the
//! class and the sentence that entity's own lookup answers with. For the tagging surface
//! that means a `ResourceArn` that IS a Glue resource ARN is an ENTITY LOOKUP, so a value it
//! cannot place is an absence (`EntityNotFoundException`) and never a bad parameter — worded
//! by the surface that owns the resource TYPE the ARN names where one owns it, and by the
//! sentence recorded for an ARN no surface owns where none does — while
//! `InvalidInputException` is reserved for a value that is no Glue resource ARN at all. For
//! the schedule pair it means the entity is the `ColumnStatisticsTaskSettings` record the
//! schedule hangs on, named by its own account-qualified key, whatever the catalog holds
//! under the database and table names on the way to it.
//!
//! The recorded cases — a tag read of a Glue ARN this service places no entity under, a tag
//! read of another Region's job ARN, and a schedule start and stop for a pair carrying no
//! settings — are each ONE row of the tables below. The rows differ along what the rule
//! generalises over: whether the ARN is one this service places an entity under, one it does
//! not, or no Glue resource ARN at all; which Region and account it names; which part of the
//! schedule's address is absent; whether the request states a catalog id; and which operation
//! of each surface is asked. The ARN forms are tabled per form by the suites that own them
//! ([`crate::tagging_resource_kinds`], [`crate::catalog_input_refusals`]).

use serde_json::{json, Value};

use crate::column_statistics_task_settings::{create_body, store_table};
use crate::resource_tagging::{
    glue_arn, resource_arn, TAGGING_ENTITY_NOT_FOUND, UNPLACEABLE_RESOURCE_ARN_NOT_FOUND,
};
use crate::tagging_resource_kinds::read_miss_message;
use crate::wire::{
    call, call_in_region, emulator, error_code, error_message, ok, ACCOUNT_ID, REGION,
};

/// The error class a refusal of a value that is no Glue resource ARN carries.
const INVALID_INPUT: &str = "InvalidInputException";

/// The error class every absence below carries ("A specified entity does not exist" —
/// `smithy_query operation=GetTags,StartColumnStatisticsTaskRunSchedule,
/// StopColumnStatisticsTaskRunSchedule`).
const NOT_FOUND: &str = "EntityNotFoundException";

/// What [`error_message`] answers for a refusal carrying no `Message` member at all.
const NO_PROSE: &str = "<no Message>";

/// The IAM role the job and crawler rows are created with.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/sim2real-glue-miss-role";

/// The two operations that decide a column statistics task run schedule, so every row of
/// the schedule table is asked of both and the two must answer the same sentence.
const SCHEDULE_OPERATIONS: [&str; 2] = [
    "StartColumnStatisticsTaskRunSchedule",
    "StopColumnStatisticsTaskRunSchedule",
];

/// Assert one refusal: HTTP 400, the error class `code`, and `expected` as the whole
/// `Message`.
fn assert_refusal(case: &str, status: u16, body: &Value, code: &str, expected: &str) {
    assert_eq!(
        status, 400,
        "case {case}: expected HTTP 400, got {status} with {body}"
    );
    assert!(
        error_code(body).ends_with(code),
        "case {case}: expected {code}, got {} with {body}",
        error_code(body)
    );
    assert_eq!(
        error_message(body),
        expected,
        "case {case}: expected the refusal {expected:?}, got {}",
        error_message(body)
    );
}

/// The three operations of the tagging surface, so every ARN form below is asked of all of
/// them: the class line is the surface's, not one operation's.
const TAGGING_OPERATIONS: [&str; 3] = ["GetTags", "TagResource", "UntagResource"];

/// The request body `operation` takes for `arn`, with the members its write legs require.
fn tagging_body(operation: &str, arn: &str) -> Value {
    match operation {
        "TagResource" => json!({ "ResourceArn": arn, "TagsToAdd": { "written": "no" } }),
        "UntagResource" => json!({ "ResourceArn": arn, "TagsToRemove": ["owner"] }),
        _ => json!({ "ResourceArn": arn }),
    }
}

/// A `ResourceArn` that IS a Glue resource ARN is an entity LOOKUP, so a value it fails to
/// place is an absence on all three tagging operations; only a value that is no Glue resource
/// ARN is the member's own refusal, and that one carries no prose. The two sides of that one
/// line are asserted here side by side.
///
/// The ARN forms that place no entity are tabled per form by
/// [`crate::tagging_resource_kinds`] and the values that are no Glue resource ARN by
/// [`crate::catalog_input_refusals`]; what this test adds is the CONTRAST — a form of each
/// kind, on each of the three operations, beside an ARN whose kind the surface does place.
#[tokio::test]
async fn a_glue_arn_that_places_no_entity_is_an_absence_not_a_bad_parameter() {
    let router = emulator();
    ok(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": "present_db" }, "Tags": { "owner": "data" } }),
    )
    .await;

    // (case, the resource portion of a legal Glue ARN). This service places no entity under
    // either — a kind that carries no tag set, and a resource portion naming no one resource
    // of the kind it spells — and no owning surface words that miss.
    let places_no_entity: Vec<(&str, &str)> = vec![
        (
            "a catalog table, a kind that carries no tag set",
            "table/present_db/orders",
        ),
        (
            "a one-segment kind carrying a second segment",
            "database/present_db/extra",
        ),
    ];

    for (case, resource) in &places_no_entity {
        let arn = glue_arn(REGION, ACCOUNT_ID, resource);
        for operation in TAGGING_OPERATIONS {
            let (status, body) = call(&router, operation, &tagging_body(operation, &arn)).await;
            let case = format!("{case} / {operation}");
            assert_refusal(
                &case,
                status,
                &body,
                NOT_FOUND,
                UNPLACEABLE_RESOURCE_ARN_NOT_FOUND,
            );
        }
    }

    // (case, the value sent). Neither is a Glue resource ARN, so there is no address to look
    // anything up by: the member itself is refused, and that refusal carries no prose at all.
    let not_an_arn: Vec<(&str, String)> = vec![
        ("a bare resource name", "present_db".to_string()),
        (
            "an ARN of a partition the pattern does not admit",
            format!("arn:aws-moon:glue:{REGION}:{ACCOUNT_ID}:database/present_db"),
        ),
    ];

    for (case, value) in &not_an_arn {
        for operation in TAGGING_OPERATIONS {
            let (status, body) = call(&router, operation, &tagging_body(operation, value)).await;
            let case = format!("{case} / {operation}");
            assert_refusal(&case, status, &body, INVALID_INPUT, NO_PROSE);
        }
    }

    // And an ARN whose KIND this surface does place, naming no resource: the same absence,
    // worded by the surface that owns the kind on the read and by the writes' own fixed
    // sentence on the two writes.
    for (segment, name) in [("database", "absent_db"), ("job", "absent-job")] {
        let arn = resource_arn(segment, name);
        for operation in TAGGING_OPERATIONS {
            let (status, body) = call(&router, operation, &tagging_body(operation, &arn)).await;
            let case = format!("a {segment} name nothing created / {operation}");
            let expected = if operation == "GetTags" {
                read_miss_message(segment, name)
            } else {
                TAGGING_ENTITY_NOT_FOUND.to_string()
            };
            assert_refusal(&case, status, &body, NOT_FOUND, &expected);
        }
    }

    // The positive control: the database the create tagged still serves its tag set, so no
    // row above turned a served read into a refusal.
    let served = ok(
        &router,
        "GetTags",
        &json!({ "ResourceArn": resource_arn("database", "present_db") }),
    )
    .await;
    assert_eq!(
        served["Tags"],
        json!({ "owner": "data" }),
        "the ARN of the tagged database serves its tags, got {served}"
    );
}

/// A tag read of the ARN of a resource that EXISTS in another Region's estate reports the
/// entity type it looked up as absent: this endpoint serves one Region, so the resource is
/// not in the estate the ARN addresses.
///
/// The rows differ along which side of the pair is asked — the other Region's ARN through
/// this endpoint, and this Region's ARN through that endpoint — along the account the ARN
/// names, and along the kind of resource, so the per-type sentence is pinned for two kinds
/// whose own lookup states one.
#[tokio::test]
async fn a_tag_read_across_regions_names_the_type_it_looked_up() {
    let router = emulator();
    let other_region = "us-west-2";
    ok(
        &router,
        "CreateJob",
        &json!({
            "Name": "regional-job",
            "Role": ROLE_ARN,
            "Command": { "Name": "glueetl", "ScriptLocation": "s3://etl-scripts/etl.py" },
            "Tags": { "env": "test" },
        }),
    )
    .await;
    ok(
        &router,
        "CreateCrawler",
        &json!({
            "Name": "regional-crawler",
            "Role": ROLE_ARN,
            "Targets": { "S3Targets": [{ "Path": "s3://crawler-bucket/data/" }] },
            "Tags": { "env": "test" },
        }),
    )
    .await;

    // (case, the Region the request is served in, the ARN it names, the expected Message).
    let rows: Vec<(String, &str, String, String)> = vec![
        (
            "the job's own ARN in another Region, through this endpoint".to_string(),
            REGION,
            glue_arn(other_region, ACCOUNT_ID, "job/regional-job"),
            read_miss_message("job", "regional-job"),
        ),
        (
            "the job's ARN of this Region, through another endpoint".to_string(),
            other_region,
            resource_arn("job", "regional-job"),
            read_miss_message("job", "regional-job"),
        ),
        (
            "the crawler's own ARN in another Region, through this endpoint".to_string(),
            REGION,
            glue_arn(other_region, ACCOUNT_ID, "crawler/regional-crawler"),
            read_miss_message("crawler", "regional-crawler"),
        ),
        (
            "a job ARN of another account".to_string(),
            REGION,
            glue_arn(REGION, "111122223333", "job/regional-job"),
            read_miss_message("job", "regional-job"),
        ),
    ];

    for (case, region, arn, expected) in &rows {
        let (status, body) = call_in_region(
            &router,
            region,
            "GetTags",
            &json!({ "ResourceArn": arn }),
        )
        .await;
        assert_refusal(case, status, &body, NOT_FOUND, expected);
    }

    // The job is still served through the endpoint of its own Region, tags and all.
    let served = ok(
        &router,
        "GetTags",
        &json!({ "ResourceArn": resource_arn("job", "regional-job") }),
    )
    .await;
    assert_eq!(
        served["Tags"],
        json!({ "env": "test" }),
        "the job's own ARN serves its tags through its own Region, got {served}"
    );
}

/// The absence of the `ColumnStatisticsTaskSettings` record a schedule start or stop
/// addresses is reported as THAT record, by its account-qualified key, whichever part of
/// the address the catalog does not hold — and the two operations answer the same sentence.
///
/// The rows differ along which part of the address is absent (the settings of a stored
/// table, the table, the database), and along whether the request states the catalog id.
#[tokio::test]
async fn a_schedule_miss_names_the_settings_record_it_looked_up() {
    let router = emulator();
    store_table(&router, "schedule_db", "stored_no_settings", &["id"]).await;

    // (case, the DatabaseName sent, the TableName sent, the extra members).
    let rows: Vec<(&str, &str, &str, Value)> = vec![
        (
            "a stored table no create has written settings for",
            "schedule_db",
            "stored_no_settings",
            json!({}),
        ),
        (
            "a table the stored database does not hold",
            "schedule_db",
            "no_such_table",
            json!({}),
        ),
        (
            "a database the catalog does not hold",
            "no_such_db",
            "any_table",
            json!({}),
        ),
        (
            "a stored table with no settings, with the catalog stated",
            "schedule_db",
            "stored_no_settings",
            json!({ "CatalogID": ACCOUNT_ID }),
        ),
    ];

    for (case, database, table, extra) in &rows {
        // The record's key: the catalog the request addressed, the account whose estate it
        // is in, then the database and table the settings would belong to.
        let expected = format!(
            "ColumnStatisticsTaskSettings {ACCOUNT_ID}:{ACCOUNT_ID}:{database}-{table} not found."
        );
        let mut body = json!({ "DatabaseName": database, "TableName": table });
        if let Some(members) = extra.as_object() {
            for (member, value) in members {
                body[member] = value.clone();
            }
        }
        let mut answered: Vec<String> = Vec::new();
        for operation in SCHEDULE_OPERATIONS {
            let (status, parsed) = call(&router, operation, &body).await;
            let case = format!("{case} / {operation}");
            assert_refusal(&case, status, &parsed, NOT_FOUND, &expected);
            answered.push(error_message(&parsed));
        }
        assert_eq!(
            answered[0], answered[1],
            "case {case}: the start and the stop must word one absence identically, got \
             {answered:?}"
        );
    }

    // The positive control: once the settings exist and carry a schedule, both operations
    // are served for the same address every row above was refused for.
    ok(
        &router,
        "CreateColumnStatisticsTaskSettings",
        &create_body(
            "schedule_db",
            "stored_no_settings",
            json!({ "Schedule": "cron(15 12 * * ? *)" }),
        ),
    )
    .await;
    for operation in SCHEDULE_OPERATIONS {
        let answer = ok(
            &router,
            operation,
            &json!({ "DatabaseName": "schedule_db", "TableName": "stored_no_settings" }),
        )
        .await;
        assert!(
            answer.as_object().is_some_and(|members| members.is_empty()),
            "case {operation} on a table whose settings carry a schedule: expected an empty \
             document, got {answer}"
        );
    }
}
