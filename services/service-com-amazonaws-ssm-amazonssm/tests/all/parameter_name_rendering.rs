// SPDX-License-Identifier: CC0-1.0

//! How Parameter Store RENDERS the name of a parameter it holds, end to end.
//!
//! The rule under test: the hierarchy separator leads a name that names a hierarchy — "The
//! fully qualified name includes the complete hierarchy of the parameter path and name. For
//! parameters in a hierarchy, you must include a leading forward slash character (/) when you
//! create or reference a parameter"
//! (`smithy_query(query="doc", shape="PutParameterRequest", member="Name")`) — so a name of a
//! SINGLE level names no hierarchy and is held, and reported, without a leading separator,
//! whichever of its two spellings a put wrote; a name of two levels or more keeps its whole
//! path, separator and all. Every projection of a stored parameter reports that one
//! rendering, so the `Name` of a record never disagrees with the `ARN` beside it, and a read
//! of ONE NAMED parameter echoes the spelling its own request asked with
//! (`GetParameterRequest$Name` is "the name or Amazon Resource Name (ARN) of the parameter
//! that you want to query").
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct: every name
//! asserted here was persisted by a `PutParameter` of the same test.
//!
//! The rows span what the rule generalises over: how many levels the name has (one, two,
//! five), which of the two spellings of a one-level name the put used, which symbols the name
//! carries, the parameter's type, and how many versions it holds.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call, router, ACCOUNT, REGION};

/// One case: its label, the name the put wrote, and the name the store must hold it under —
/// which is what every projection of the parameter reports.
type Case<'a> = (&'a str, &'a str, &'a str);

/// The table every suite below walks.
///
/// The one-level names appear in both spellings, because the rule is about the spelling the
/// put used; the deeper names differ in depth and in the symbols a name may carry
/// ("Parameter names can include only the following symbols and letters: a-zA-Z0-9_.-" plus
/// the `/` separator, `PutParameterRequest$Name`).
const CASES: &[Case<'static>] = &[
    ("a one-level name written bare", "Rendered", "Rendered"),
    (
        "a one-level name written with the separator",
        "/RenderedRooted",
        "RenderedRooted",
    ),
    (
        "a one-level name of the symbols the name rule admits",
        "/Rendered_one-level.0",
        "Rendered_one-level.0",
    ),
    (
        "a two-level name",
        "/Rendered/two-levels",
        "/Rendered/two-levels",
    ),
    (
        "a five-level name",
        "/Rendered/a/b/c/five",
        "/Rendered/a/b/c/five",
    ),
];

/// The ARN this service mints for a parameter of `name`: the name without its leading
/// separator, which is the parameter's store key.
fn parameter_arn(name: &str) -> String {
    format!(
        "arn:aws:ssm:{REGION}:{ACCOUNT}:parameter/{}",
        name.strip_prefix('/').unwrap_or(name)
    )
}

/// Write `value` as a version of `name`, overwriting an existing parameter, and return the
/// version the put reports.
async fn put_version(router: &MultiServiceRouter, case: &str, name: &str, value: &str) -> i64 {
    let (status, response) = call(
        router,
        "PutParameter",
        &json!({ "Name": name, "Type": "String", "Value": value, "Overwrite": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: writing {name} must succeed, got {status} {response}"
    );
    response["Version"]
        .as_i64()
        .unwrap_or_else(|| panic!("case {case:?}: a put reports the version it wrote"))
}

/// Call `operation`, asserting it was served.
async fn served(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, response) = call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} {body} must be served, got {status} {response}"
    );
    response
}

/// The records of a response's `Parameters` list.
fn records(case: &str, operation: &str, response: &Value) -> Vec<Value> {
    response["Parameters"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: {operation} carries a Parameters list, got {response}")
        })
        .clone()
}

/// Every projection of the stored parameter reports it under the name the store holds: the
/// history of its versions, the metadata listing that names it, and the path read of the
/// level it sits in.
#[tokio::test]
async fn every_projection_reports_the_name_the_store_holds() {
    let router = router();

    for (case, written, held) in CASES {
        // Two versions, so the assertion is made on every entry of a history rather than on
        // one, and three for the deeper names is not needed: what differs per row is the name.
        put_version(&router, case, written, "v1").await;
        let newest = put_version(&router, case, written, "v2").await;
        assert_eq!(
            newest, 2,
            "case {case:?}: the second put appends to the one parameter, got version {newest}"
        );

        let history = served(
            &router,
            case,
            "GetParameterHistory",
            &json!({ "Name": written }),
        )
        .await;
        let entries = records(case, "GetParameterHistory", &history);
        assert_eq!(
            entries.len(),
            2,
            "case {case:?}: the history reports both versions, got {history}"
        );
        for entry in &entries {
            assert_eq!(
                entry["Name"].as_str(),
                Some(*held),
                "case {case:?}: every history entry reports the held name, got {entry}"
            );
        }

        // The metadata listing names no parameter of its own, so it reports the held name
        // too — and the ARN in that same record carries it as the store key.
        let listed = served(
            &router,
            case,
            "DescribeParameters",
            &json!({ "ParameterFilters": [{ "Key": "Name", "Values": [written] }] }),
        )
        .await;
        let metadata = records(case, "DescribeParameters", &listed);
        assert_eq!(
            metadata.len(),
            1,
            "case {case:?}: the filter names exactly this parameter, got {listed}"
        );
        assert_eq!(
            (metadata[0]["Name"].as_str(), metadata[0]["ARN"].as_str()),
            (Some(*held), Some(parameter_arn(written).as_str())),
            "case {case:?}: the listing reports the held name beside the store key the ARN \
             carries, got {listed}"
        );

        // The level the parameter sits in: the root for a one-level name, its own path
        // otherwise. A path read names no parameter either, so the held name is what it
        // reports.
        let path = match held.rsplit_once('/') {
            Some((head, _)) if !head.is_empty() => head.to_string(),
            _ => "/".to_string(),
        };
        let under = served(
            &router,
            case,
            "GetParametersByPath",
            &json!({ "Path": path }),
        )
        .await;
        let reported: Vec<String> = records(case, "GetParametersByPath", &under)
            .iter()
            .filter_map(|record| record["Name"].as_str().map(str::to_string))
            .collect();
        assert!(
            reported.iter().any(|name| name == held),
            "case {case:?}: the path read of {path} reports the held name {held:?}, got \
             {reported:?}"
        );
    }
}

/// A read of ONE NAMED parameter echoes the name its own request asked with, and both
/// spellings of a one-level name address the one parameter: the name a projection reported
/// resolves to the same record, and a write through it appends a version rather than creating
/// a second parameter.
#[tokio::test]
async fn a_named_read_echoes_its_request_and_either_spelling_resolves() {
    let router = router();

    for (case, written, held) in CASES {
        put_version(&router, case, written, "v1").await;

        // Asked with the put's spelling and with the held one: each is answered under the
        // spelling it asked with, and both report the same stored value and version.
        for asked in [*written, *held] {
            let single = served(&router, case, "GetParameter", &json!({ "Name": asked })).await;
            assert_eq!(
                (
                    single["Parameter"]["Name"].as_str(),
                    single["Parameter"]["Value"].as_str(),
                    single["Parameter"]["ARN"].as_str(),
                ),
                (Some(asked), Some("v1"), Some(parameter_arn(held).as_str())),
                "case {case:?}: GetParameter of {asked:?} echoes that name and serves the one \
                 parameter, got {single}"
            );

            let batch = served(&router, case, "GetParameters", &json!({ "Names": [asked] })).await;
            let found = records(case, "GetParameters", &batch);
            assert_eq!(
                found.len(),
                1,
                "case {case:?}: GetParameters of {asked:?} resolves it, got {batch}"
            );
            assert_eq!(
                found[0]["Name"].as_str(),
                Some(asked),
                "case {case:?}: GetParameters echoes the name it was asked with, got {batch}"
            );
        }

        // The round trip: a write through the name a projection reported reaches the record
        // that projection came from.
        let appended = put_version(&router, case, held, "v2").await;
        assert_eq!(
            appended, 2,
            "case {case:?}: writing through the held name {held:?} appends to the one \
             parameter, got version {appended}"
        );
        let history = served(&router, case, "GetParameterHistory", &json!({ "Name": held })).await;
        assert_eq!(
            records(case, "GetParameterHistory", &history).len(),
            2,
            "case {case:?}: the history of the held name holds both versions, got {history}"
        );
    }
}
