// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for RUNNING a prepared statement: `StartQueryExecution` submitted as
//! `EXECUTE <statement_name> [USING <argument>, …]` runs the query statement the named
//! workgroup holds under that name, with the arguments bound to its placeholders, and a
//! submission whose statement that workgroup does not hold is refused by the submission
//! itself.
//!
//! Driven over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the
//! same router the emulator boots, so every assertion is about what an SDK client
//! observes.
//!
//! Each test states a rule and asserts it over a TABLE of inputs rather than one request:
//! the rows differ along what the rule generalises over — how many arguments are bound and
//! of which type, the form of the statement they are bound into, the case the keywords are
//! written in, and which workgroup the submission names.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    PREPARED_STATEMENT_NOT_FOUND_IN_WORK_GROUP, QUERY_STATE_SUCCEEDED,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_work_group, error_code, history, message, router, settled, start,
};

/// Where the queries in these tests write their results: any location satisfies the
/// documented rule that one must be given, and nothing is written to it.
const OUTPUT_LOCATION: &str = "s3://athena-results/prepared/";

/// The wire error code this service refuses a bad request with.
const INVALID_REQUEST: &str = "InvalidRequestException";

/// Register the prepared statement `name` in `work_group`, standing for `query_statement`.
async fn create_statement(
    router: &MultiServiceRouter,
    work_group: &str,
    name: &str,
    query_statement: &str,
) {
    let request = json!({
        "WorkGroup": work_group,
        "StatementName": name,
        "QueryStatement": query_statement,
    });
    let (status, created) = call(router, "CreatePreparedStatement", &request).await;
    assert_eq!(
        status, 200,
        "CreatePreparedStatement({request}) must be accepted, got {status} with {created:?}"
    );
}

/// A `StartQueryExecution` body submitting `sql` in `work_group`, in the engine database
/// `database` so the statements of one test see each other's writes and another test's do
/// not.
fn submission(sql: &str, work_group: &str, database: &str) -> Value {
    json!({
        "QueryString": sql,
        "WorkGroup": work_group,
        "QueryExecutionContext": { "Database": database },
        "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION },
    })
}

/// The rows of `id`'s result set, each as the values its `Datum`s carry.
async fn rows_of(router: &MultiServiceRouter, id: &str) -> Vec<Vec<String>> {
    let (status, body) = call(router, "GetQueryResults", &json!({ "QueryExecutionId": id })).await;
    assert_eq!(
        status, 200,
        "GetQueryResults({id}) must answer 200, got {status} with {body:?}"
    );
    body["ResultSet"]["Rows"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|row| {
            row["Data"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|datum| {
                    datum["VarCharValue"]
                        .as_str()
                        .unwrap_or("<no value>")
                        .to_string()
                })
                .collect()
        })
        .collect()
}

/// One row of the EXECUTE table.
struct ExecuteCase {
    /// What this row is about.
    case: &'static str,
    /// The `QueryStatement` the prepared statement stands for, with one `?` per argument it
    /// expects.
    query_statement: &'static str,
    /// The submitted text, with `{name}` standing for the statement's own name — so the
    /// keywords, the argument list and its spelling are this row's.
    submitted: &'static str,
    /// The column labels the result set must open with.
    header: Vec<&'static str>,
    /// The one data row it must carry.
    data: Vec<&'static str>,
}

/// `EXECUTE <statement_name> [USING <argument>, …]` runs the query statement the submission's
/// workgroup holds under that name, with the `USING` arguments bound to that statement's
/// placeholders in the order they were written — so the result set carries the statement's
/// own column labels and the values the arguments bound.
#[tokio::test]
async fn an_execute_runs_the_workgroups_statement_with_its_arguments_bound() {
    let router = router();
    let work_group = "prepared-execution";
    create_work_group(&router, work_group).await;
    let database = "prepared_execution";

    // The rows differ along how many arguments are bound (none, one, two), the type of each
    // (an integer, a negative integer, an approximate number, a quoted string, one with
    // blanks in it), whether the statement computes with the bound value, and the case the
    // keywords are written in.
    let cases: Vec<ExecuteCase> = vec![
        ExecuteCase {
            case: "one integer argument",
            query_statement: "SELECT ? AS val",
            submitted: "EXECUTE {name} USING 777",
            header: vec!["val"],
            data: vec!["777"],
        },
        ExecuteCase {
            case: "two arguments of different types, in order",
            query_statement: "SELECT ? AS n, ? AS s",
            submitted: "EXECUTE {name} USING 42, 'hi'",
            header: vec!["n", "s"],
            data: vec!["42", "hi"],
        },
        ExecuteCase {
            case: "a statement that takes no argument at all",
            query_statement: "SELECT 'fixed' AS label",
            submitted: "EXECUTE {name}",
            header: vec!["label"],
            data: vec!["fixed"],
        },
        ExecuteCase {
            case: "a negative integer argument",
            query_statement: "SELECT ? AS delta",
            submitted: "EXECUTE {name} USING -5",
            header: vec!["delta"],
            data: vec!["-5"],
        },
        ExecuteCase {
            case: "an approximate-numeric argument",
            query_statement: "SELECT ? AS ratio",
            submitted: "EXECUTE {name} USING 2.5",
            header: vec!["ratio"],
            data: vec!["2.5"],
        },
        ExecuteCase {
            case: "a quoted string argument carrying blanks",
            query_statement: "SELECT ? AS label",
            submitted: "EXECUTE {name} USING 'two words'",
            header: vec!["label"],
            data: vec!["two words"],
        },
        ExecuteCase {
            case: "an argument the statement computes with",
            query_statement: "SELECT ? + 1 AS incremented",
            submitted: "EXECUTE {name} USING 41",
            header: vec!["incremented"],
            data: vec!["42"],
        },
        ExecuteCase {
            case: "the keywords written in lower case",
            query_statement: "SELECT ? AS val",
            submitted: "execute {name} using 9",
            header: vec!["val"],
            data: vec!["9"],
        },
        ExecuteCase {
            case: "three arguments, one of them used twice",
            query_statement: "SELECT ? AS a, ? AS b, ? AS c",
            submitted: "EXECUTE {name} USING 'x', 'x', 3",
            header: vec!["a", "b", "c"],
            data: vec!["x", "x", "3"],
        },
    ];

    // Every statement is registered, then every submission is made, and only then is each
    // one read back: the completion is owed a span after the submit, so the submissions
    // wait that span once between them rather than once each.
    let mut started: Vec<(usize, String, String)> = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let name = format!("executed_{index}");
        create_statement(&router, work_group, &name, case.query_statement).await;
        let submitted = case.submitted.replace("{name}", &name);
        let id = start(&router, &submission(&submitted, work_group, database)).await;
        started.push((index, id, submitted));
    }

    for (index, id, submitted) in &started {
        let ExecuteCase {
            case,
            query_statement: _,
            submitted: _,
            header,
            data,
        } = &cases[*index];
        let found = settled(&router, id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {case:?}: expected {QUERY_STATE_SUCCEEDED}, got {found:?}"
        );
        // The KIND of statement reported is the kind submitted: an EXECUTE, of the class
        // that queries or changes data.
        assert_eq!(
            (
                found["StatementType"].as_str(),
                found["SubstatementType"].as_str()
            ),
            (Some("DML"), Some("EXECUTE")),
            "case {case:?}: an EXECUTE is reported as one, got {found:?}"
        );
        assert_eq!(
            found["Query"].as_str(),
            Some(submitted.as_str()),
            "case {case:?}: the execution reports the text the client submitted, \
             {submitted:?}, got {found:?}"
        );
        let rows = rows_of(&router, id).await;
        let expected: Vec<Vec<String>> = vec![
            header.iter().map(|label| (*label).to_string()).collect(),
            data.iter().map(|value| (*value).to_string()).collect(),
        ];
        assert_eq!(
            rows, expected,
            "case {case:?}: expected the rows {expected:?}, got {rows:?}"
        );
    }
}

/// An `EXECUTE` binds its argument wherever the statement's placeholder stands, including a
/// predicate over a stored table — so the submission selects exactly the rows that argument
/// matches and reports the bytes its read scanned.
#[tokio::test]
async fn an_execute_binds_its_argument_into_a_predicate_over_a_table() {
    let router = router();
    let work_group = "prepared-predicate";
    create_work_group(&router, work_group).await;
    let database = "prepared_predicate";

    // The table the statement reads, written first so there is stored data to read: the
    // statement RAN in the submission that started it, so the table is there for the next
    // one whatever state the clock has the execution in.
    for sql in [
        "CREATE TABLE stocked (id integer, item varchar)",
        "INSERT INTO stocked VALUES (1, 'desk'), (2, 'lamp'), (3, 'chair')",
    ] {
        start(&router, &submission(sql, work_group, database)).await;
    }
    create_statement(
        &router,
        work_group,
        "one_item",
        "SELECT item FROM stocked WHERE id = ?",
    )
    .await;

    // (case, the argument bound, the items it must select). The rows differ along which
    // stored row the argument matches, and one matches none.
    let cases: Vec<(&str, &str, Vec<&str>)> = vec![
        ("the first row", "1", vec!["desk"]),
        ("a row in the middle", "2", vec!["lamp"]),
        ("the last row", "3", vec!["chair"]),
        ("a value no row carries", "4", Vec::new()),
    ];

    let mut started: Vec<(usize, String)> = Vec::new();
    for (index, (_, argument, _)) in cases.iter().enumerate() {
        let submitted = format!("EXECUTE one_item USING {argument}");
        started.push((
            index,
            start(&router, &submission(&submitted, work_group, database)).await,
        ));
    }

    for (index, id) in &started {
        let (case, argument, items) = &cases[*index];
        let found = settled(&router, id).await;
        assert_eq!(
            found["Status"]["State"].as_str(),
            Some(QUERY_STATE_SUCCEEDED),
            "case {case:?}: expected {QUERY_STATE_SUCCEEDED}, got {found:?}"
        );
        let rows = rows_of(&router, id).await;
        let expected: Vec<Vec<String>> = std::iter::once(vec!["item".to_string()])
            .chain(items.iter().map(|item| vec![(*item).to_string()]))
            .collect();
        assert_eq!(
            rows, expected,
            "case {case:?}: binding {argument} must select {expected:?}, got {rows:?}"
        );
        // A statement that READ rows of the stored table scanned the data it read; one
        // whose predicate matched none read nothing to scan.
        let scanned = found["Statistics"]["DataScannedInBytes"].as_i64();
        if items.is_empty() {
            assert_eq!(
                scanned,
                Some(0),
                "case {case:?}: a read that matched no row scanned nothing, got {found:?}"
            );
        } else {
            assert!(
                scanned.is_some_and(|scanned| scanned > 0),
                "case {case:?}: a read of a stored table must report the bytes it read, \
                 got {found:?}"
            );
        }
    }
}

/// A prepared statement is addressed by its name TOGETHER WITH a workgroup, so an `EXECUTE`
/// resolves in the workgroup the submission names: one that holds no statement under that
/// name refuses the submission itself — `InvalidRequestException`, no execution minted —
/// while the same text in the owning workgroup runs.
#[tokio::test]
async fn an_execute_of_a_statement_the_workgroup_does_not_hold_is_refused() {
    let router = router();
    let owner = "prepared-owner";
    let other = "prepared-other";
    for work_group in [owner, other] {
        create_work_group(&router, work_group).await;
    }
    create_statement(&router, owner, "owned", "SELECT ? AS val").await;
    create_statement(&router, other, "elsewhere", "SELECT ? AS val").await;

    // (case, the workgroup the submission names, the submitted text, whether it must be
    // accepted). The rows differ along which workgroup holds the statement the text names:
    // the owning one, one that holds another statement under another name, and names
    // nothing holds at all.
    let cases: Vec<(&str, &str, &str, bool)> = vec![
        (
            "the workgroup that holds it",
            owner,
            "EXECUTE owned USING 2",
            true,
        ),
        (
            "a workgroup that does not hold it",
            other,
            "EXECUTE owned USING 2",
            false,
        ),
        (
            "the other workgroup's own statement, in the other workgroup",
            other,
            "EXECUTE elsewhere USING 2",
            true,
        ),
        (
            "the other workgroup's statement, in the owning workgroup",
            owner,
            "EXECUTE elsewhere USING 2",
            false,
        ),
        (
            "a name no workgroup holds",
            owner,
            "EXECUTE never_created USING 2",
            false,
        ),
        (
            "a name no workgroup holds, with no arguments",
            other,
            "EXECUTE never_created",
            false,
        ),
    ];

    for (case, work_group, submitted, accepted) in &cases {
        let before = history(&router, work_group).await;
        let (status, body) = call(
            &router,
            "StartQueryExecution",
            &submission(submitted, work_group, "prepared_scope"),
        )
        .await;
        let after = history(&router, work_group).await;
        if *accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: the owning workgroup must run it, got {status} with {body:?}"
            );
            assert_eq!(
                after.len(),
                before.len() + 1,
                "case {case:?}: an accepted submission mints one execution, got {after:?}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_REQUEST,
            "case {case:?}: expected {INVALID_REQUEST}, got {body:?}"
        );
        assert_eq!(
            message(&body),
            PREPARED_STATEMENT_NOT_FOUND_IN_WORK_GROUP,
            "case {case:?}: unexpected wording in {body:?}"
        );
        assert_eq!(
            after, before,
            "case {case:?}: a refused submission must mint no execution, got {after:?}"
        );
    }

    // A statement that is deleted stops being one its workgroup holds, so the same
    // submission is refused afterwards.
    let (status, deleted) = call(
        &router,
        "DeletePreparedStatement",
        &json!({ "WorkGroup": owner, "StatementName": "owned" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the statement must be deleted, got {status} with {deleted:?}"
    );
    let (status, body) = call(
        &router,
        "StartQueryExecution",
        &submission("EXECUTE owned USING 2", owner, "prepared_scope"),
    )
    .await;
    assert_eq!(
        status, 400,
        "a deleted statement cannot be executed, got {status} with {body:?}"
    );
    assert_eq!(
        message(&body),
        PREPARED_STATEMENT_NOT_FOUND_IN_WORK_GROUP,
        "unexpected wording in {body:?}"
    );
}

/// `ListQueryExecutions` reports the history it found, so the ids member is ALWAYS
/// serialized: an empty list for a workgroup in which nothing ran, and the ids once queries
/// exist.
#[tokio::test]
async fn a_listing_reports_an_empty_history_as_an_empty_list() {
    let router = router();
    let empty = "listing-empty";
    let busy = "listing-busy";
    for work_group in [empty, busy] {
        create_work_group(&router, work_group).await;
    }
    let ran = start(&router, &submission("SELECT 1 AS one", busy, "listing")).await;

    // (case, the request, the ids it must report). The rows differ along whether the
    // workgroup has a history at all, and along a page that asks for no ids.
    let cases: Vec<(&str, Value, Vec<String>)> = vec![
        (
            "a workgroup in which nothing ran",
            json!({ "WorkGroup": empty }),
            Vec::new(),
        ),
        (
            "a workgroup in which one query ran",
            json!({ "WorkGroup": busy }),
            vec![ran.clone()],
        ),
        (
            "a page that asks for no ids at all",
            json!({ "WorkGroup": busy, "MaxResults": 0 }),
            Vec::new(),
        ),
    ];

    for (case, request, expected) in &cases {
        let (status, body) = call(&router, "ListQueryExecutions", request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body:?}"
        );
        let reported = body["QueryExecutionIds"].as_array().cloned();
        let listed: Vec<String> = reported
            .clone()
            .unwrap_or_default()
            .iter()
            .filter_map(|id| id.as_str().map(str::to_string))
            .collect();
        assert!(
            reported.is_some(),
            "case {case:?}: QueryExecutionIds must be serialized, got {body:?}"
        );
        assert_eq!(
            &listed, expected,
            "case {case:?}: expected {expected:?}, got {body:?}"
        );
    }
}

/// `ListPreparedStatements` reports the name and last modified time of each statement the
/// named workgroup holds, in name order, a page at a time — out of the same registry the
/// create wrote — and a workgroup this account does not hold has none to list.
#[tokio::test]
async fn a_listing_reports_the_statements_its_workgroup_holds() {
    let router = router();
    let holder = "listing-statements";
    let empty = "listing-no-statements";
    for work_group in [holder, empty] {
        create_work_group(&router, work_group).await;
    }
    // Created out of name order, so the listing's order is the registry's and not the
    // order of the writes.
    for name in ["gamma", "alpha", "beta"] {
        create_statement(&router, holder, name, "SELECT ? AS val").await;
    }

    // (case, the request, the names it must report, whether a token must follow it). The
    // rows differ along which workgroup is listed and how large a page was asked for.
    let cases: Vec<(&str, Value, Vec<&str>, bool)> = vec![
        (
            "a workgroup holding statements",
            json!({ "WorkGroup": holder }),
            vec!["alpha", "beta", "gamma"],
            false,
        ),
        (
            "a workgroup holding none",
            json!({ "WorkGroup": empty }),
            Vec::new(),
            false,
        ),
        (
            "a page smaller than the registry",
            json!({ "WorkGroup": holder, "MaxResults": 2 }),
            vec!["alpha", "beta"],
            true,
        ),
        (
            "a page of one",
            json!({ "WorkGroup": holder, "MaxResults": 1 }),
            vec!["alpha"],
            true,
        ),
    ];

    for (case, request, expected, more) in &cases {
        let (status, body) = call(&router, "ListPreparedStatements", request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body:?}"
        );
        let listed = body["PreparedStatements"].as_array().cloned();
        assert!(
            listed.is_some(),
            "case {case:?}: PreparedStatements must be serialized, got {body:?}"
        );
        let names: Vec<String> = listed
            .clone()
            .unwrap_or_default()
            .iter()
            .filter_map(|summary| summary["StatementName"].as_str().map(str::to_string))
            .collect();
        assert_eq!(
            &names, expected,
            "case {case:?}: expected {expected:?}, got {body:?}"
        );
        for summary in listed.unwrap_or_default() {
            assert!(
                summary["LastModifiedTime"].is_number(),
                "case {case:?}: each summary reports when it was last written, got \
                 {summary:?}"
            );
        }
        assert_eq!(
            body["NextToken"].as_str().is_some(),
            *more,
            "case {case:?}: expected a token to follow: {more}, got {body:?}"
        );
    }

    // The pages of a token walk concatenate to the whole registry, in that same order.
    let mut walked: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = json!({ "WorkGroup": holder, "MaxResults": 2 });
        if let Some(token) = &token {
            request["NextToken"] = json!(token);
        }
        let (status, body) = call(&router, "ListPreparedStatements", &request).await;
        assert_eq!(status, 200, "a page must answer 200, got {status}: {body:?}");
        for summary in body["PreparedStatements"].as_array().unwrap_or(&Vec::new()) {
            walked.push(
                summary["StatementName"]
                    .as_str()
                    .unwrap_or("<no StatementName>")
                    .to_string(),
            );
        }
        token = body["NextToken"].as_str().map(str::to_string);
        if token.is_none() || walked.len() > 8 {
            break;
        }
    }
    assert_eq!(
        walked,
        vec!["alpha", "beta", "gamma"],
        "the pages must concatenate to the whole registry, got {walked:?}"
    );

    // (case, the request) — a workgroup this account does not hold, and a token this
    // service did not mint for THIS workgroup's listing.
    let (_, first) = call(
        &router,
        "ListPreparedStatements",
        &json!({ "WorkGroup": holder, "MaxResults": 1 }),
    )
    .await;
    let minted = first["NextToken"]
        .as_str()
        .expect("a truncated listing hands back a token")
        .to_string();
    let refusals: Vec<(&str, Value)> = vec![
        (
            "a workgroup nothing created",
            json!({ "WorkGroup": "no-such-work-group" }),
        ),
        (
            "a token minted for another workgroup's listing",
            json!({ "WorkGroup": empty, "NextToken": minted }),
        ),
        (
            "a token nothing minted",
            json!({ "WorkGroup": holder, "NextToken": "made-up-token" }),
        ),
    ];
    for (case, request) in &refusals {
        let (status, body) = call(&router, "ListPreparedStatements", request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_REQUEST,
            "case {case:?}: expected {INVALID_REQUEST}, got {body:?}"
        );
    }
}

/// A `StatementName` outside the pattern the model declares for it is refused with the
/// operation's own MODELLED client error and AWS's validation wording — never with a code
/// the operation does not model.
#[tokio::test]
async fn a_statement_name_outside_its_pattern_is_refused_as_a_bad_request() {
    let router = router();
    let work_group = "prepared-names";
    create_work_group(&router, work_group).await;

    // The pattern the model declares on `StatementName`
    // (`smithy_query(query="operation",operation="GetPreparedStatement")`:
    // `^[a-zA-Z_][a-zA-Z0-9_@:]{1,256}$`), as the clause states it with its anchors
    // dropped.
    let pattern = "[a-zA-Z_][a-zA-Z0-9_@:]{1,256}";
    // (case, the name) — each one breaks that pattern in its own way: a character outside
    // the set, a first character outside the leading set, and a name of one character,
    // which the pattern's own minimum excludes.
    let cases: Vec<(&str, &str)> = vec![
        ("a name carrying hyphens", "sim2real-missing-stmt"),
        ("a name opening with a digit", "1statement"),
        ("a name carrying a blank", "two words"),
        ("a name of one character", "s"),
    ];

    for (case, name) in &cases {
        let (status, body) = call(
            &router,
            "GetPreparedStatement",
            &json!({ "WorkGroup": work_group, "StatementName": name }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body:?}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_REQUEST,
            "case {case:?}: expected {INVALID_REQUEST}, got {body:?}"
        );
        assert_eq!(
            message(&body),
            format!(
                "1 validation error detected: Value at 'statementName' failed to satisfy \
                 constraint: Member must satisfy regular expression pattern: {pattern}"
            ),
            "case {case:?}: unexpected wording in {body:?}"
        );
    }
}
