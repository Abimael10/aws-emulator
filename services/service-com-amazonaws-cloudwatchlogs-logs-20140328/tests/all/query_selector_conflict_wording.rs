// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the sentence `StartQuery` refuses a request that names the log groups
//! to query in TWO places with.
//!
//! The rule under test: `StartQuery` admits exactly one place for the log groups — "either
//! exactly one of the following parameters: logGroupName, logGroupNames, or
//! logGroupIdentifiers. Or the queryString must include a SOURCE command" — and for the SQL
//! query language the statement's own source clause IS that place ("you specify the log group
//! names inside the querystring instead of here", `StartQuery$logGroupName`). A request that
//! names them in two places is refused with `InvalidParameterException` and one fixed sentence
//! quoting the three members, with NO SDK error envelope after it: the fault is in the SHAPE of
//! the request, not in anything a query backend answered.
//!
//! The table spans what the rule generalises over: which of the two by-NAME members the request
//! carries, how the statement writes its own source clause (one name, the selector call), and
//! the member the clause OVERRIDES rather than conflicts with (`logGroupIdentifiers`), which
//! leaves the request admitted.

use serde_json::{json, Value};

use crate::harness::{now_ms, Logs};

/// The operations under test.
const START: &str = "StartQuery";
const DESCRIBE: &str = "DescribeQueries";

/// The modelled code such a refusal is answered under.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The sentence CloudWatch Logs states for the class, written out here rather than read from
/// the crate so a change of wording fails this suite.
const ONLY_ONE: &str = "Only one of the parameters 'logGroupName', 'logGroupNames' or \
                        'logGroupIdentifiers' may be specified in a request";

/// The log groups the statements below read.
const GROUP: &str = "/selector-conflict/one";
const OTHER_GROUP: &str = "/selector-conflict/two";

/// Both groups, and the query window in the epoch SECONDS `StartQuery`'s range is stated in.
async fn groups_with_window(logs: &Logs) -> (i64, i64) {
    for group in [GROUP, OTHER_GROUP] {
        logs.ok("CreateLogGroup", &json!({"logGroupName": group}))
            .await;
    }
    let now_secs = now_ms() / 1_000;
    (now_secs - 3_600, now_secs + 60)
}

/// A SQL statement naming its source in a `FROM` clause.
fn statement(source: &str) -> String {
    format!("SELECT svc, COUNT(*) AS n FROM {source} GROUP BY svc")
}

/// RULE: a SQL statement that names its source beside a member naming log groups by NAME is
/// refused with the one fixed sentence, and nothing is started.
#[tokio::test]
async fn a_second_place_naming_the_log_groups_is_refused_with_one_sentence() {
    let logs = Logs::new();
    let (start_time, end_time) = groups_with_window(&logs).await;

    // (case, the statement's source clause, the selector member the request also carries)
    let cases: Vec<(&str, String, Value)> = vec![
        (
            "logGroupName beside a quoted source name",
            statement(&format!("`{GROUP}`")),
            json!({"logGroupName": GROUP}),
        ),
        (
            "logGroupNames beside a quoted source name",
            statement(&format!("`{GROUP}`")),
            json!({"logGroupNames": [GROUP]}),
        ),
        (
            "logGroupNames listing two groups",
            statement(&format!("`{GROUP}`")),
            json!({"logGroupNames": [GROUP, OTHER_GROUP]}),
        ),
        (
            // The member names a group the statement does not: the fault is that the request
            // names them twice, whichever groups each place names.
            "logGroupName naming another group than the statement",
            statement(&format!("`{GROUP}`")),
            json!({"logGroupName": OTHER_GROUP}),
        ),
        (
            "logGroupName beside the selector call the source clause is written with",
            statement(&format!(
                "logGroups(logGroupIdentifier: ['{GROUP}', '{OTHER_GROUP}'])"
            )),
            json!({"logGroupName": GROUP}),
        ),
    ];

    for (case, query_string, selector) in &cases {
        let mut request = json!({
            "queryLanguage": "SQL",
            "queryString": query_string,
            "startTime": start_time,
            "endTime": end_time,
        });
        for (member, value) in selector.as_object().expect("a selector is an object") {
            request[member] = value.clone();
        }
        let (status, body) = logs.call(START, &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: a request naming the log groups twice is refused, got {status} \
             {body}"
        );
        let code = crate::harness::error_code(&body);
        assert_eq!(
            code, INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {code} with {body}"
        );
        let said = body["message"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a refusal carries a message, got {body}"));
        assert_eq!(
            said, ONLY_ONE,
            "case {case:?}: expected the selector-conflict sentence alone, got {said:?}"
        );
    }

    // Nothing was started: a refused request leaves no query behind.
    for group in [GROUP, OTHER_GROUP] {
        let described = logs.ok(DESCRIBE, &json!({"logGroupName": group})).await;
        assert!(
            described["queries"]
                .as_array()
                .is_none_or(|queries| queries.is_empty()),
            "case the refused requests: no query is recorded for {group}, got {described}"
        );
    }
}

/// RULE: the sentence is the whole message — a request-shape refusal carries none of the SDK
/// error envelope this service appends to a refusal relayed from the query backend.
#[tokio::test]
async fn the_selector_conflict_sentence_carries_no_backend_envelope() {
    let logs = Logs::new();
    let (start_time, end_time) = groups_with_window(&logs).await;
    let case = "a SQL statement beside logGroupName";
    let (status, body) = logs
        .call(
            START,
            &json!({
                "queryLanguage": "SQL",
                "queryString": statement(&format!("`{GROUP}`")),
                "logGroupName": GROUP,
                "startTime": start_time,
                "endTime": end_time,
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the request is refused, got {status} {body}"
    );
    let said = body["message"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a refusal carries a message, got {body}"));
    for absent in ["(Service:", "Status Code:", "Request ID:", "Proxy: null"] {
        assert!(
            !said.contains(absent),
            "case {case:?}: the sentence carries no {absent:?} clause, got {said:?}"
        );
    }
}

/// RULE: `logGroupIdentifiers` is the member a SQL statement's source clause OVERRIDES rather
/// than conflicts with, so a request carrying it beside the statement is admitted.
#[tokio::test]
async fn the_member_the_source_clause_overrides_is_admitted() {
    let logs = Logs::new();
    let (start_time, end_time) = groups_with_window(&logs).await;
    let case = "logGroupIdentifiers beside a SQL statement naming its source";
    let answer = logs
        .ok(
            START,
            &json!({
                "queryLanguage": "SQL",
                "queryString": statement(&format!("`{GROUP}`")),
                "logGroupIdentifiers": [OTHER_GROUP],
                "startTime": start_time,
                "endTime": end_time,
            }),
        )
        .await;
    assert!(
        answer["queryId"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "case {case:?}: an admitted request is minted a query id, got {answer}"
    );
}
