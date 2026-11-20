// SPDX-License-Identifier: CC0-1.0

//! What `StartQueryExecution` answers for text that is NOT a statement of Athena's SQL.
//!
//! THE RULE UNDER TEST: Athena parses a submitted statement in the submit call itself, so
//! text its grammar cannot read is refused SYNCHRONOUSLY — HTTP 400
//! `InvalidRequestException` of the malformed-query class, carrying the parser's own
//! diagnostic (where the grammar stopped, the token it stopped on, and the tokens it accepts
//! there where its accepted set at that position is known) — and NO query execution is
//! minted for it, so the workgroup's history stays as it was. Text the grammar does read is
//! accepted and listed, whatever the submitted statement then does.
//!
//! The rule is asserted over a TABLE rather than one request: the rows differ along what the
//! text is, which word the grammar stopped on, the case that word was written in, WHERE in
//! the submitted text the stop is (first line, second line, after a statement separator) and
//! whether the stopping position has an accepted set the grammar states.
//!
//! Driven end to end over the same router the emulator boots (awsJson1_1,
//! `x-amz-target: AmazonAthena.StartQueryExecution`).

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::catalog_sql::grammar::ERROR_CODE_MALFORMED_QUERY;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{assert_refusal, call, history, router};

/// The Spark-less Athena SQL workgroup these submissions run in, which keeps its own results
/// location so a submission carries nothing but the statement.
const WORK_GROUP: &str = "malformed-sql";

/// Where that workgroup's queries would write their results. Nothing is written to it: every
/// submission below is refused before anything runs.
const OUTPUT_LOCATION: &str = "s3://athena-results/malformed/";

/// The accepted set Athena's parser states where a STATEMENT begins: every word a statement
/// of its SQL may begin with, quoted and in the grammar's own order, and the production for a
/// query last.
///
/// Source: Athena's SQL grammar — its statement production — as the parser lists it in this
/// diagnostic. Written out here so the suite pins the sentence a client reads rather than
/// deriving it from the code under test.
const AT_STATEMENT_START: &str = "Expecting: 'ALTER', 'ANALYZE', 'CALL', 'COMMENT', \
     'COMMIT', 'CREATE', 'DEALLOCATE', 'DELETE', 'DENY', 'DESC', 'DESCRIBE', 'DROP', \
     'EXECUTE', 'EXPLAIN', 'GRANT', 'INSERT', 'MERGE', 'PREPARE', 'REFRESH', 'RESET', \
     'REVOKE', 'ROLLBACK', 'SET', 'SHOW', 'START', 'TRUNCATE', 'UNLOAD', 'UPDATE', 'USE', \
     <query>";

/// The accepted set the grammar states after `CREATE`: the objects its definitions name.
///
/// Source: Athena's SQL grammar — the `CREATE` statements it has a production for.
const AFTER_CREATE: &str = "Expecting: 'MATERIALIZED', 'MULTI', 'OR', 'PROTECTED', 'ROLE', \
     'SCHEMA', 'TABLE', 'VIEW'";

/// Create the workgroup these submissions run in, with a results location of its own.
async fn given_work_group(router: &MultiServiceRouter) {
    let (status, created) = call(
        router,
        "CreateWorkGroup",
        &json!({
            "Name": WORK_GROUP,
            "Configuration": {
                "ResultConfiguration": { "OutputLocation": OUTPUT_LOCATION }
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the workgroup {WORK_GROUP} must be created, got {status} with {created:?}"
    );
}

/// One submission, as a client sends it: the statement and the workgroup, and nothing else.
async fn submit(router: &MultiServiceRouter, sql: &str) -> (u16, Value) {
    call(
        router,
        "StartQueryExecution",
        &json!({ "QueryString": sql, "WorkGroup": WORK_GROUP }),
    )
    .await
}

/// Text the grammar cannot read is refused by the submit call itself, with the parser's
/// diagnostic, and mints no execution; a statement it does read is accepted and listed.
#[tokio::test]
async fn text_that_is_no_statement_is_refused_at_submit_and_mints_nothing() {
    let router = router();
    given_work_group(&router).await;

    // (case, the submitted text, the diagnostic the refusal states)
    let cases: Vec<(&str, &str, String)> = vec![
        (
            "a misspelled query word, with the rest of the text no statement either",
            "SELEC 1 FRM dual !!!",
            format!("line 1:1: mismatched input 'SELEC'. {AT_STATEMENT_START}"),
        ),
        (
            "the same kind of word written in lower case",
            "selekt 1",
            format!("line 1:1: mismatched input 'selekt'. {AT_STATEMENT_START}"),
        ),
        (
            "a statement that begins on the second line of the submitted text",
            "\n  SELEKT 1",
            format!("line 2:3: mismatched input 'SELEKT'. {AT_STATEMENT_START}"),
        ),
        (
            "a word that begins a statement of another engine's dialect",
            "VACUUM INTO 'copy.db'",
            format!("line 1:1: mismatched input 'VACUUM'. {AT_STATEMENT_START}"),
        ),
        (
            "a second statement, after a separator the grammar read",
            "SELECT 1;\n  SELEKT 2",
            format!("line 2:3: mismatched input 'SELEKT'. {AT_STATEMENT_START}"),
        ),
        (
            "a definition naming an object the grammar has no statement for",
            "CREATE DATABASE bad-name",
            format!("line 1:8: mismatched input 'DATABASE'. {AFTER_CREATE}"),
        ),
        (
            "a stop the parser named no position for, reported at the start of the text",
            "SELECT FROM",
            "line 1:1: mismatched input 'SELECT'".to_string(),
        ),
    ];

    for (case, sql, expected) in &cases {
        let (status, refused) = submit(&router, sql).await;
        assert_refusal(case, status, &refused, ERROR_CODE_MALFORMED_QUERY, expected);
        assert_eq!(
            refused["QueryExecutionId"].as_str(),
            None,
            "case {case:?}: a refused submission mints no execution id, got {refused:?}"
        );
    }

    // Nothing was minted for any of them: the workgroup's history is still empty.
    let listed = history(&router, WORK_GROUP).await;
    assert!(
        listed.is_empty(),
        "expected no execution for {} refused submissions, got {listed:?}",
        cases.len()
    );

    // The positive control: a statement the grammar reads is accepted, and it is the ONE
    // execution the workgroup holds.
    let (status, accepted) = submit(&router, "SELECT 1").await;
    assert_eq!(
        status, 200,
        "a statement the grammar reads must be accepted, got {status} with {accepted:?}"
    );
    let minted = accepted["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("an accepted submission answers with an id, got {accepted:?}"))
        .to_string();
    let listed = history(&router, WORK_GROUP).await;
    assert_eq!(
        listed,
        vec![minted],
        "the workgroup's history holds exactly the accepted submission, got {listed:?}"
    );
}
