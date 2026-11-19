// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for HOW the control plane reports `RestApi$policy`, across EVERY operation
//! that sets and reads it.
//!
//! THE RULE UNDER TEST: `RestApi$policy` is "a stringified JSON policy document". What the api
//! STORES is the document the caller wrote with every `Resource` written in API Gateway's own
//! `execute-api:/{stage}/{verb}/{path}` shorthand expanded to the ARN of the method of this api
//! it names; what every operation REPORTS for the member is that stored document's text encoded
//! as a JSON string, so a client decodes the member once and then parses the document. Nothing
//! else of the document is rewritten.
//!
//! What this suite adds to the unit tests of `src/state/stringified_policy.rs`, which own the
//! two functions: that the rule reaches the member through the WIRE on each of the operations
//! that set it (`CreateRestApi` at create time and `UpdateRestApi`'s `/policy` patch afterwards)
//! and each of the ones that report it (`GetRestApi` and the `GetRestApis` collection) — so no
//! operation can report one stored document differently from another; and that the cycle a
//! client performs on the member — decode it once, parse it, write the decoded text back — is a
//! FIXED POINT, so a tool that diffs or edits a resource policy reads back what it wrote however
//! many times it rewrites it.
//!
//! The table spans what the rule generalises over: the characters a JSON string has to encode
//! (the quotes every policy carries, a backslash, a quote inside a value, a newline, none at
//! all) and the shape of a `Resource` the expansion applies to (the shorthand's wildcard, one
//! concrete method, a list of two, and the ARN spelling the shorthand stands for, which is
//! stored unchanged).

use serde_json::{json, Value};

use crate::execute_api_data_plane::{policy_document_of, stored_policy_document};
use crate::rest_api_lifecycle::{page_items, string_member, Gateway};

/// The Region the shared control-plane harness scopes its requests to, and so the Region of
/// the method ARNs a stored document names.
const REGION: &str = "us-east-1";

/// Read `api`'s reported policy through `GetRestApi`.
async fn read_policy(gateway: &Gateway, api: &str, case: &str) -> String {
    let read = gateway.call("GET", &format!("/restapis/{api}"), None).await;
    assert_eq!(
        read.status, 200,
        "case {case}: expected GetRestApi to answer 200, got {} with {:?}",
        read.status, read.body
    );
    string_member(&read.body, "policy", case)
}

/// The same member as the `GetRestApis` collection reports it for `api`.
async fn listed_policy(gateway: &Gateway, api: &str, case: &str) -> String {
    let listed = gateway.call("GET", "/restapis", None).await;
    assert_eq!(
        listed.status, 200,
        "case {case}: expected GetRestApis to answer 200, got {} with {:?}",
        listed.status, listed.body
    );
    let item = page_items(&listed.body, case)
        .into_iter()
        .find(|item| item["id"].as_str() == Some(api))
        .unwrap_or_else(|| panic!("case {case}: expected the collection to carry api {api}"));
    string_member(&item, "policy", case)
}

/// `arn:aws:execute-api:{region}:{account}:{api-id}`, which the shorthand's own leading `/`
/// follows in the stored document.
fn arn_prefix(api: &str) -> String {
    format!(
        "arn:aws:execute-api:{REGION}:{account}:{api}",
        account = ::engine::emulator_account_id()
    )
}

/// The documents every case below is driven with — the CLASS the rule is about, defined once
/// and shared by both tests so neither can span less of it than the other.
///
/// Rows differ along the characters a JSON string has to encode and along the shape of the
/// `Resource` the expansion applies to.
fn documents() -> Vec<(&'static str, Value)> {
    vec![
        (
            "a document carrying only the quotes every policy has",
            json!({ "Version": "2012-10-17", "Statement": [] }),
        ),
        (
            "a value carrying a backslash and a quote",
            json!({ "Version": "2012-10-17", "Statement": [], "Id": "a\\b and \"c\"" }),
        ),
        (
            "a value carrying a newline",
            json!({ "Version": "2012-10-17", "Statement": [], "Id": "a\nb" }),
        ),
        (
            "the shorthand's wildcard",
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "execute-api:Invoke",
                    "Resource": "execute-api:/*",
                }]
            }),
        ),
        (
            "the shorthand naming one method",
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "execute-api:Invoke",
                    "Resource": "execute-api:/test/GET/widgets",
                }]
            }),
        ),
        (
            "a list of two methods in the shorthand",
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Deny",
                    "Principal": "*",
                    "Action": "execute-api:Invoke",
                    "Resource": ["execute-api:/test/GET/a", "execute-api:/test/POST/b"],
                }]
            }),
        ),
        (
            "the ARN spelling the shorthand stands for",
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": "*",
                    "Action": "execute-api:Invoke",
                    "Resource": "arn:aws:execute-api:us-east-1:000000000000:none/*",
                }]
            }),
        ),
    ]
}

/// RULE: every operation that reports the member reports the STORED document's text encoded as
/// a JSON string, and the stored document is what the caller wrote with the shorthand expanded
/// — whichever operation set it.
#[tokio::test]
async fn every_operation_reports_the_stored_policy_the_one_way() {
    let gateway = Gateway::new();

    for (index, (case, document)) in documents().iter().enumerate() {
        let written = document.to_string();
        // SET AT CREATE TIME, where the api id the expansion names is minted by the very call.
        let created = gateway
            .call(
                "POST",
                "/restapis",
                Some(json!({ "name": format!("policy-report-{index}"), "policy": written })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreateRestApi to answer 201, got {} with {:?}",
            created.status, created.body
        );
        let api = string_member(&created.body, "id", case);
        let stored = stored_policy_document(document, &arn_prefix(&api));
        for (reporter, reported) in [
            ("CreateRestApi", string_member(&created.body, "policy", case)),
            ("GetRestApi", read_policy(&gateway, &api, case).await),
            ("GetRestApis", listed_policy(&gateway, &api, case).await),
        ] {
            assert_eq!(
                policy_document_of(&reported, case),
                stored,
                "case {case}: expected {reporter} to report the stored document"
            );
        }

        // SET AFTERWARDS: the patch stores the same document the create did, so one api cannot
        // hold two spellings of one grant depending on which operation wrote it.
        let patched = gateway
            .call(
                "PATCH",
                &format!("/restapis/{api}"),
                Some(json!({
                    "patchOperations": [
                        { "op": "replace", "path": "/policy", "value": written }
                    ]
                })),
            )
            .await;
        assert_eq!(
            patched.status, 200,
            "case {case}: expected UpdateRestApi to answer 200, got {} with {:?}",
            patched.status, patched.body
        );
        assert_eq!(
            policy_document_of(&string_member(&patched.body, "policy", case), case),
            stored,
            "case {case}: expected UpdateRestApi to report the stored document"
        );
        assert_eq!(
            policy_document_of(&read_policy(&gateway, &api, case).await, case),
            stored,
            "case {case}: expected the patched document to be read back unchanged"
        );
    }
}

/// RULE: the member carries the stored document's text encoded as a JSON string, so the value a
/// client DECODES ONCE and then PARSES is the stored document, and writing that decoded text
/// back through the `/policy` patch stores the same document and is reported identically — the
/// read-modify-write cycle every tool that diffs or edits a resource policy performs is a fixed
/// point, and the member is never encoded a second time.
#[tokio::test]
async fn a_policy_read_back_and_written_again_is_stored_unchanged() {
    let gateway = Gateway::new();

    // The shared table, so the cycle is driven over the whole class the rule is about rather
    // than a second selection of it.
    for (index, (case, document)) in documents().iter().enumerate() {
        let created = gateway
            .call(
                "POST",
                "/restapis",
                Some(json!({
                    "name": format!("policy-round-trip-{index}"),
                    "policy": document.to_string(),
                })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreateRestApi to answer 201, got {} with {:?}",
            created.status, created.body
        );
        let api = string_member(&created.body, "id", case);
        let prefix = arn_prefix(&api);
        let stored = stored_policy_document(document, &prefix);
        let mut reported = read_policy(&gateway, &api, case).await;

        // TWO cycles: one is what a drift-detecting tool does, and a second one is what makes an
        // encoding applied when the document is STORED rather than when it is reported visible,
        // because such an encoding compounds on every rewrite.
        for cycle in 1..=2 {
            // The two steps every client takes on this member: decode the JSON string, then
            // parse the document it carries.
            let text = policy_document_of(&reported, case);
            let parsed: Value = serde_json::from_str(&text).unwrap_or_else(|error| {
                panic!("case {case} cycle {cycle}: the decoded {text:?} must parse: {error}")
            });
            assert_eq!(
                text, stored,
                "case {case} cycle {cycle}: expected the stored document from one decode"
            );
            // No value of the stored document is still written in the shorthand: whatever the
            // caller wrote, the rewrite stores the one spelling of each resource.
            for resource in resources_of(&parsed) {
                assert!(
                    !resource.starts_with("execute-api:/"),
                    "case {case} cycle {cycle}: expected the resource expanded under {prefix}, \
                     got {resource:?}"
                );
            }

            // Written back exactly as it was read, decoded once — nothing else of the document
            // edited.
            let patched = gateway
                .call(
                    "PATCH",
                    &format!("/restapis/{api}"),
                    Some(json!({
                        "patchOperations": [
                            { "op": "replace", "path": "/policy", "value": text }
                        ]
                    })),
                )
                .await;
            assert_eq!(
                patched.status, 200,
                "case {case} cycle {cycle}: expected UpdateRestApi to answer 200, got {} with \
                 {:?}",
                patched.status, patched.body
            );
            for (reporter, after) in [
                (
                    "UpdateRestApi",
                    string_member(&patched.body, "policy", case),
                ),
                ("GetRestApi", read_policy(&gateway, &api, case).await),
                ("GetRestApis", listed_policy(&gateway, &api, case).await),
            ] {
                assert_eq!(
                    after, reported,
                    "case {case} cycle {cycle}: expected {reporter} to report the member \
                     unchanged by the rewrite"
                );
            }
            reported = read_policy(&gateway, &api, case).await;
        }
    }
}

/// Every `Resource` value of a policy document, over both spellings a statement names one in —
/// one value or a list of them.
fn resources_of(document: &Value) -> Vec<String> {
    document
        .get("Statement")
        .and_then(Value::as_array)
        .map(|statements| {
            statements
                .iter()
                .filter_map(|statement| statement.get("Resource"))
                .flat_map(|resource| match resource {
                    Value::Array(values) => values.clone(),
                    value => vec![value.clone()],
                })
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
