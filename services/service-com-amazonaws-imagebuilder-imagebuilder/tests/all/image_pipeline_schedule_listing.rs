// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for what a pipeline's SCHEDULE leaves in the ACCOUNT'S COLLECTION of
//! image pipelines: `ListImagePipelines` after a create whose `schedule.scheduleExpression`
//! the service could not read, and after one it could.
//!
//! The rule under test: `scheduleExpression` is an expression in a grammar — "You can specify
//! a cron expression, or a rate expression such as `rate(1 day)`"
//! (`smithy_query(query="doc",shape="Schedule",member="scheduleExpression")`) — whose target
//! carries a length bound and no pattern, so the SERVICE reads the syntax and decides it
//! BEFORE anything is written: an expression it cannot read is refused as that parameter's
//! value with HTTP 400 and `InvalidParameterValueException`, and the account's collection of
//! pipelines is left exactly as it was, while one it can read is stored as submitted and
//! reported back by the collection beside the start condition the pipeline runs under.
//!
//! [`super::image_pipeline_schedule`] asserts the refusal and the round trip through the
//! pipeline's OWN read (`GetImagePipeline`); what these cases add is the collection's view —
//! a refused create leaves an account that already holds a pipeline with exactly the entries
//! it had, so "no pipeline is created" holds for the listing and not only for the ARN the
//! create would have minted — over expression forms and refusal details that suite does not
//! carry: an hours field written as a word, a minutes field out of range, a seventh field
//! after the year, a body that stops before the day-of-week field, a rate whose value is not a
//! number, and cron bodies using a range, a step and a numbered month.
//!
//! The harness, the fixture and the refusal wording are that sibling's: they are borrowed from
//! it rather than copied, so what these cases assert about a message cannot drift from what it
//! asserts.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::image_pipeline_schedule::{
    assert_ok, call, create_pipeline, emulator, error_code, error_message, illegal_characters,
    pipeline_arn, refusal_message, text, unexpected_end, Fixture, SCHEDULE_SYNTAX,
};

/// The start condition a schedule naming none reads back carrying: "If no value is set Image
/// Builder defaults to `EXPRESSION_MATCH_AND_DEPENDENCY_UPDATES_AVAILABLE`"
/// (`smithy_query(query="doc",shape="Schedule",member="pipelineExecutionStartCondition")`).
const DEFAULT_START_CONDITION: &str = "EXPRESSION_MATCH_AND_DEPENDENCY_UPDATES_AVAILABLE";

/// The six cron fields as the format states them, which a refusal of a body carrying a field
/// beyond them states back.
const CRON_FIELD_LAYOUT: &str = "[Minutes] [Hours] [Day_of_Month] [Month] [Day_of_Week] [Year]";

/// The parser's statement about a cron body carrying a `token` beyond the six fields the
/// format declares, at `position` — the one detail form the sibling suite's rows do not reach,
/// since a seventh field is read as a token in the year's place only when the sixth is not a
/// year.
fn unexpected_token(position: usize, token: &str) -> String {
    format!(
        "Invalid CRON expression: error at position {position}, details: Unexpected token \
         '{token}' after the Year field, the CRON format consists of 6 fields \
         ({CRON_FIELD_LAYOUT})."
    )
}

/// One expression the service cannot read, and the requirement clause its refusal states.
struct Refused {
    /// What the row is about.
    case: &'static str,
    /// The expression the request carried.
    expression: &'static str,
    /// The requirement clause the refusal states.
    detail: String,
}

/// The expressions these cases submit that the service cannot read: the recorded one, a token
/// no field admits in two more places, a body carrying a field beyond the six the format
/// declares, one that stops before a field the format declares, and values in no schedule
/// grammar at all.
fn refused_expressions() -> Vec<Refused> {
    vec![
        Refused {
            case: "a minutes field that is not a minute",
            expression: "cron(not valid)",
            detail: illegal_characters(0, "NOT"),
        },
        Refused {
            case: "an hours field written as a word",
            expression: "cron(0 ten * * ? *)",
            detail: illegal_characters(2, "TEN"),
        },
        Refused {
            case: "a minutes field out of range",
            expression: "cron(99 10 * * ? *)",
            detail: illegal_characters(0, "99"),
        },
        Refused {
            case: "a seventh field after the year",
            expression: "cron(0 10 * * ? 2030 2031)",
            detail: unexpected_token(16, "2031"),
        },
        Refused {
            case: "a body that stops before the day of week",
            expression: "cron(0 10 * *)",
            detail: unexpected_end(8, "Day_of_Week"),
        },
        Refused {
            case: "a rate whose value is not a number",
            expression: "rate(many days)",
            detail: SCHEDULE_SYNTAX.to_string(),
        },
        Refused {
            case: "a value in no schedule grammar",
            expression: "every night",
            detail: SCHEDULE_SYNTAX.to_string(),
        },
    ]
}

/// The expressions these cases submit that the service reads: the cron operators a field
/// admits (a range, a step, a numbered month) beside the rate form the member's own
/// documentation gives ("a rate expression such as `rate(1 day)`").
const ACCEPTED_EXPRESSIONS: &[(&str, &str)] = &[
    ("a weekly cron", "cron(0 3 ? * SUN *)"),
    ("a cron with a day-of-month range", "cron(0 10 1-5 * ? *)"),
    ("a cron with a step in the hours field", "cron(15 */4 * * ? *)"),
    ("a cron naming a month by number", "cron(0 6 1 7 ? *)"),
    ("a rate of one day", "rate(1 day)"),
    ("a rate of several minutes", "rate(30 minutes)"),
];

/// A `CreateImagePipeline` body for `name` carrying the schedule `expression`.
fn create_body(fixture: &Fixture, name: &str, expression: &str) -> Value {
    fixture.create_body(
        name,
        name,
        json!({"schedule": {"scheduleExpression": expression}}),
    )
}

/// Every pipeline `ListImagePipelines` reports for the account, as the entries the collection
/// carries.
async fn listed_pipelines(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let (status, answer) = call(
        router,
        "POST",
        "/ListImagePipelines",
        Some(&json!({"maxResults": 25})),
    )
    .await;
    let answer = assert_ok(case, status, answer);
    assert!(
        answer.get("nextToken").is_none(),
        "case {case:?}: expected every pipeline on one page, got {answer}"
    );
    answer
        .get("imagePipelineList")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: expected an imagePipelineList in {answer}"))
        .clone()
}

#[tokio::test]
async fn an_unreadable_expression_leaves_the_pipeline_listing_as_it_was() {
    let router = emulator();
    let fixture = Fixture::given(&router).await;

    // One pipeline the account already holds, on an expression the service reads: the
    // collection a refused create has to leave untouched is a NON-empty one, so a listing
    // that answered nothing at all could not pass these rows.
    let standing = "the pipeline the account already holds";
    let body = create_body(&fixture, "listing-standing", "cron(0 3 ? * SUN *)");
    let (status, created) = create_pipeline(&router, &body).await;
    let created = assert_ok(standing, status, created);
    let standing_arn = text(standing, &created, "imagePipelineArn");
    let before = listed_pipelines(&router, standing).await;
    assert_eq!(
        before
            .iter()
            .map(|entry| text(standing, entry, "arn"))
            .collect::<Vec<String>>(),
        vec![standing_arn],
        "case {standing:?}: expected the listing to carry exactly that pipeline, got {before:?}"
    );

    for (index, row) in refused_expressions().iter().enumerate() {
        let case = row.case;
        let name = format!("listing-refused-{index}");
        let body = create_body(&fixture, &name, row.expression);
        let (status, refused) = create_pipeline(&router, &body).await;
        assert_eq!(
            status, 400,
            "case {case:?} ({:?}): expected HTTP 400, got {status} with {refused}",
            row.expression
        );
        let code = error_code(case, &refused);
        assert_eq!(
            code, "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {code:?} with {refused}"
        );
        let expected = refusal_message(&row.detail);
        let message = error_message(case, &refused);
        assert_eq!(
            message, expected,
            "case {case:?} ({:?}): expected {expected:?}, got {message:?}",
            row.expression
        );

        // Nothing was written: the account's collection of pipelines is entry for entry what
        // it was before the refused create, so the refusal neither added the pipeline it
        // named nor touched the one already there.
        let after = listed_pipelines(&router, case).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the refused create to leave the listing at {before:?}, got \
             {after:?}"
        );
        assert!(
            !after
                .iter()
                .any(|entry| entry.get("arn") == Some(&json!(pipeline_arn(&name)))),
            "case {case:?}: expected no pipeline named {name}, got {after:?}"
        );
    }
}

#[tokio::test]
async fn an_expression_the_parser_reads_is_reported_by_the_listing_as_submitted() {
    let router = emulator();
    let fixture = Fixture::given(&router).await;

    let mut expected_arns = Vec::new();
    for (index, (case, expression)) in ACCEPTED_EXPRESSIONS.iter().enumerate() {
        let name = format!("listing-accepted-{index}");
        let body = create_body(&fixture, &name, expression);
        let (status, created) = create_pipeline(&router, &body).await;
        let created = assert_ok(case, status, created);
        let arn = text(case, &created, "imagePipelineArn");
        assert_eq!(
            arn,
            pipeline_arn(&name),
            "case {case:?}: expected the pipeline ARN of {name}, got {arn:?}"
        );
        expected_arns.push(arn.clone());

        // The collection reports the pipeline, carrying the expression as submitted and the
        // start condition it will run under.
        let listed = listed_pipelines(&router, case).await;
        let entry = listed
            .iter()
            .find(|entry| entry.get("arn") == Some(&json!(arn)))
            .unwrap_or_else(|| panic!("case {case:?}: expected {arn} in the listing {listed:?}"));
        let schedule = entry
            .get("schedule")
            .unwrap_or_else(|| panic!("case {case:?}: expected a schedule in {entry}"));
        assert_eq!(
            schedule.get("scheduleExpression"),
            Some(&json!(expression)),
            "case {case:?}: expected the expression {expression:?} to be reported back, got \
             {schedule}"
        );
        assert_eq!(
            schedule.get("pipelineExecutionStartCondition"),
            Some(&json!(DEFAULT_START_CONDITION)),
            "case {case:?}: expected the start condition {DEFAULT_START_CONDITION:?}, got \
             {schedule}"
        );
    }

    // Every accepted create, and only those, is in the collection.
    let listed = listed_pipelines(&router, "every accepted pipeline").await;
    let mut listed_arns: Vec<String> = listed
        .iter()
        .map(|entry| text("every accepted pipeline", entry, "arn"))
        .collect();
    listed_arns.sort();
    expected_arns.sort();
    assert_eq!(
        listed_arns, expected_arns,
        "case \"every accepted pipeline\": expected {expected_arns:?}, got {listed_arns:?}"
    );
}
