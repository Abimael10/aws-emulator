// SPDX-License-Identifier: CC0-1.0

//! Which ACCOUNT's metrics a `GetMetricData` query reads.
//!
//! The rule under test: **a query's `AccountId` is part of the identity of the series it reads —
//! the caller's own account, named or left out, resolves the series this endpoint holds, and an
//! account the caller's is not linked to is answered on the RESULT, with `StatusCode`
//! `Forbidden`, no values at all and the message AWS states that refusal in**, while the other
//! queries of the same request are still answered.
//!
//! "The ID of the account where the metrics are located. If you are performing a
//! `GetMetricData` operation in a monitoring account, use this to specify which account to
//! retrieve this metric from."
//! (`smithy_query(query="doc",shape="MetricDataQuery",member="AccountId")`) — a monitoring
//! account reads a source account's metrics only where the two are LINKED;
//! `MetricDataResult$StatusCode` is the `StatusCode` enum, whose values are `Complete`,
//! `InternalError`, `PartialData` and `Forbidden`
//! (`smithy_query(query="shape",shape="StatusCode")`), and `MetricDataResult$Messages` is "a list
//! of messages with additional information about the data returned". This endpoint serves one
//! account — every datapoint `PutMetricData` writes belongs to it — and it is linked to no other.
//!
//! The rows differ along what the rule generalises over: whether the query names an account at
//! all, whether the account it names is the caller's own, which foreign account it names, and
//! where in the request's array that query sits.

use serde_json::{json, Value};

use crate::harness::{build_router, now_secs, ok};

/// The namespace this suite publishes and reads under.
const NAMESPACE: &str = "Custom/AccountScope";

/// The metric every row reads.
const METRIC: &str = "Load";

/// The value the one published datapoint carries, which is what a row answered from the caller's
/// own account reports.
const VALUE: f64 = 37.0;

/// What AWS reports on a result whose query named an account the caller's is not linked to.
const FORBIDDEN_MESSAGE: &str = "Access denied when getting data - please check that this \
     account and the requested metric account are linked";

/// One `MetricStat` query over the published series, carrying `account` when a row names one.
fn query(id: &str, account: Option<&str>) -> Value {
    let mut query = json!({
        "Id": id,
        "MetricStat": {
            "Metric": {
                "Namespace": NAMESPACE,
                "MetricName": METRIC,
                "Dimensions": [{"Name": "A", "Value": "1"}, {"Name": "B", "Value": "2"}],
            },
            "Period": 60,
            "Stat": "Sum",
        },
    });
    if let (Some(account), Value::Object(members)) = (account, &mut query) {
        members.insert("AccountId".to_string(), json!(account));
    }
    query
}

/// What one result reports: its `StatusCode`, its values, and the `(Code, Value)` of each of its
/// messages. Named so the reader below stays simple enough to read.
type Reported = (String, Vec<f64>, Vec<(String, String)>);

/// What every result of one answer reports, in the order it listed them.
fn answered(body: &Value) -> Vec<Reported> {
    body.get("MetricDataResults")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("GetMetricData returns MetricDataResults, got {body}"))
        .iter()
        .map(|result| {
            (
                result
                    .get("StatusCode")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                result
                    .get("Values")
                    .and_then(Value::as_array)
                    .map(|listed| listed.iter().filter_map(Value::as_f64).collect())
                    .unwrap_or_default(),
                result
                    .get("Messages")
                    .and_then(Value::as_array)
                    .map(|listed| {
                        listed
                            .iter()
                            .map(|message| {
                                (
                                    message
                                        .get("Code")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                    message
                                        .get("Value")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_query_naming_an_unlinked_account_is_forbidden_on_its_own_result() {
    let router = build_router();
    let window = (now_secs() / 60.0).floor() * 60.0 - 5.0 * 60.0;
    ok(
        &router,
        "PutMetricData",
        &json!({
            "Namespace": NAMESPACE,
            "MetricData": [{
                "MetricName": METRIC,
                "Dimensions": [{"Name": "A", "Value": "1"}, {"Name": "B", "Value": "2"}],
                "Value": VALUE,
                "Timestamp": window + 30.0,
            }],
        }),
    )
    .await;
    let own = ::engine::emulator_account_id().to_string();

    // (case, the account the query names, whether the read answers from this account's data).
    // The rows differ along whether an account is named, whether it is the caller's own, and
    // which other account it is — a different prefix, a different digit, and a value that is no
    // account id at all, since none of them is this endpoint's.
    let rows: Vec<(&str, Option<String>, bool)> = vec![
        ("no account at all", None, true),
        ("the caller's own account", Some(own.clone()), true),
        ("another account", Some("111122223333".to_string()), false),
        ("one digit away from the caller's own", Some(shifted(&own)), false),
        ("an account id of another shape", Some("1".to_string()), false),
    ];
    for (case, account, answered_locally) in rows {
        let body = read(&router, window, json!([query("m1", account.as_deref())])).await;
        let reported = answered(&body);
        let expected = if answered_locally {
            vec![("Complete".to_string(), vec![VALUE], Vec::new())]
        } else {
            vec![(
                "Forbidden".to_string(),
                Vec::new(),
                vec![("Forbidden".to_string(), FORBIDDEN_MESSAGE.to_string())],
            )]
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {expected:?}, got {body}"
        );
        // A read reports only: the same query answers the same the second time.
        let again =
            answered(&read(&router, window, json!([query("m1", account.as_deref())])).await);
        assert_eq!(
            again, reported,
            "case {case:?}: a read answers the same twice, got {again:?}"
        );
    }

    // The refusal is the RESULT's, not the request's: the queries beside it are answered, and
    // which position carries the foreign account decides which result reports it.
    for (case, foreign_at) in [("first", 0usize), ("second", 1usize)] {
        let queries: Vec<Value> = (0..2)
            .map(|index| {
                let account = (index == foreign_at).then_some("111122223333");
                query(&format!("m{index}"), account)
            })
            .collect();
        let body = read(&router, window, Value::Array(queries)).await;
        let reported = answered(&body);
        let statuses: Vec<&str> = reported
            .iter()
            .map(|(status, _, _)| status.as_str())
            .collect();
        let expected: Vec<&str> = (0..2)
            .map(|index| {
                if index == foreign_at {
                    "Forbidden"
                } else {
                    "Complete"
                }
            })
            .collect();
        assert_eq!(
            statuses, expected,
            "case the foreign account on the {case} query: expected {expected:?}, got {body}"
        );
        let values: Vec<&Vec<f64>> = reported.iter().map(|(_, values, _)| values).collect();
        assert!(
            values[foreign_at].is_empty(),
            "case the foreign account on the {case} query: a forbidden result carries no value \
             of this account's series, got {body}"
        );
    }
}

/// Read `queries` over the one-minute window at `window`.
async fn read(
    router: &::smithy_server_runtime::MultiServiceRouter,
    window: f64,
    queries: Value,
) -> Value {
    ok(
        router,
        "GetMetricData",
        &json!({
            "MetricDataQueries": queries,
            "StartTime": window,
            "EndTime": window + 60.0,
        }),
    )
    .await
}

/// `account` with its last digit moved on by one: an account id that is not this endpoint's,
/// built from it rather than written out, so the row cannot accidentally name the caller's own.
fn shifted(account: &str) -> String {
    let mut digits: Vec<char> = account.chars().collect();
    if let Some(last) = digits.last_mut() {
        let next = char::from_digit((last.to_digit(10).unwrap_or(0) + 1) % 10, 10);
        *last = next.unwrap_or('1');
    }
    digits.into_iter().collect()
}
