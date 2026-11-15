// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what the POLICY surfaces of CloudWatch Logs answer when the policy a
//! request addresses is not there, and when the document it carries cannot be read:
//!
//! * the data protection policy READ reports a log group that holds no policy by answering
//!   with none of the output's members — only the LOG GROUP, the one resource the request
//!   names, can be missing;
//! * the data protection policy DELETE reports the KEY it found no policy under: the account
//!   that owns the policy and the log group it would be attached to;
//! * a data protection `policyDocument` that is not JSON at all is refused with the service's
//!   own JSON parser's failure — the token it could not read and the line and column that
//!   token begins at — wherever that document is put (per log group, and as the account-level
//!   policy of the same type);
//! * a resource policy document the service's IAM policy parser will not take is refused with
//!   that parser's fixed sentence;
//! * the FIELD INDEX POLICY surface words its own refusals: a readable document naming no
//!   field index, and a log group the account does not hold.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a message
//! or a status a handler does not actually answer cannot pass.
//!
//! The tables span what each rule generalises over: how a group came to hold no policy, which
//! identifier form names it, which operation asks, where in a document the unreadable token
//! is and what it says, and which form a document that is not a policy takes. Any single
//! document or group an issue happens to mention is one row.

use serde_json::{json, Value};

use crate::harness::Logs;

/// The code CloudWatch Logs answers when the resource an operation names is not there.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The code CloudWatch Logs answers when a parameter is specified incorrectly.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The whole sentence the service's IAM policy parser refuses a document it will not take
/// with.
const IAM_GRAMMAR: &str = "Error occurred while parsing accessPolicy. Please check if the \
                           accessPolicy has been constructed correctly using IAM grammar.";

/// The whole sentence a readable field index policy document naming no field index is refused
/// with.
const NO_FIELD: &str = "Policy document should contain at least 1 field.";

/// The whole sentence the field index policy operations refuse a log group the account does
/// not hold with.
const INDEX_GROUP_NOT_FOUND: &str = "Log group cannot be found.";

/// The account-level policy type whose document is the data protection policy document.
/// Source: `smithy_query(query="shape", shape="PolicyType")`.
const DATA_PROTECTION_POLICY: &str = "DATA_PROTECTION_POLICY";

/// The account-level policy type whose document is the field index policy document. Source:
/// the same enum.
const FIELD_INDEX_POLICY: &str = "FIELD_INDEX_POLICY";

/// The whole message a data protection `policyDocument` the parser could not read is refused
/// with: the token it stopped at, then where that token begins in the submitted document,
/// with the document's source withheld.
fn unreadable_json(token: &str, line: usize, column: usize) -> String {
    format!(
        "Invalid policy JSON: Unrecognized token '{token}': was expecting (JSON String, \
         Number, Array, Object or token 'null', 'true' or 'false')\n at [Source: REDACTED \
         (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled); line: {line}, column: \
         {column}]"
    )
}

/// The message the data protection delete refuses a group holding no policy with.
fn missing_policy_key(log_group_name: &str) -> String {
    format!(
        "In DeleteDataProtectionPolicyInternalImpl request, there is no related \
         DataProtectionPolicy with policyKey {}:{log_group_name}.",
        engine::emulator_account_id()
    )
}

/// A data protection policy document of the form `PutDataProtectionPolicy$policyDocument`
/// documents: the Audit block with its `FindingsDestination`, then the Deidentify block with
/// the empty `MaskConfig`, both over the same `DataIdentifier` array. The identifier names
/// come from the catalogue that member links to ("Types of data that you can mask").
fn data_protection_document(identifier_name: &str) -> String {
    let identifiers =
        json!([format!("arn:aws:dataprotection::aws:data-identifier/{identifier_name}")]);
    json!({
        "Name": "policy-surface-refusals",
        "Version": "2021-06-01",
        "Statement": [
            {
                "Sid": "audit-sensitive-data",
                "DataIdentifier": identifiers,
                "Operation": {"Audit": {"FindingsDestination": {}}},
            },
            {
                "Sid": "mask-sensitive-data",
                "DataIdentifier": identifiers,
                "Operation": {"Deidentify": {"MaskConfig": {}}},
            },
        ],
    })
    .to_string()
}

/// Create one log group and answer the identifier form `use_arn` selects: its name, or the
/// `logGroupArn` the service reports for it.
async fn group_identifier(logs: &Logs, name: &str, use_arn: bool) -> String {
    logs.ok("CreateLogGroup", &json!({"logGroupName": name}))
        .await;
    if use_arn {
        logs.group_arns(name).await.1
    } else {
        name.to_string()
    }
}

/// How many members a response body carries.
fn reported_members(case: &str, body: &Value) -> usize {
    match body {
        Value::Null => 0,
        Value::Object(members) => members.len(),
        other => panic!("case {case:?}: the answer is a JSON object, got {other}"),
    }
}

/// Assert the data protection read of `identifier` answers HTTP 200 with no member at all.
async fn assert_reports_no_policy(logs: &Logs, case: &str, identifier: &str) {
    let (status, body) = logs
        .call(
            "GetDataProtectionPolicy",
            &json!({"logGroupIdentifier": identifier}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a group holding no policy is reported, not refused, got {status} {body}"
    );
    assert_eq!(
        reported_members(case, &body),
        0,
        "case {case:?}: the answer carries no member of the output, got {body}"
    );
}

/// A log group that exists and holds no data protection policy is REPORTED — HTTP 200 with
/// none of the output's members — however it came to hold none and whichever identifier form
/// names it; a log group the account does not hold is the one thing this read refuses.
#[tokio::test]
async fn a_group_holding_no_data_protection_policy_is_reported_not_refused() {
    let logs = Logs::new();

    // Rows: how the group came to hold no policy. Each is driven by both identifier forms.
    for (how, use_arn) in [
        ("never put on", false),
        ("never put on", true),
        ("put and deleted", false),
        ("put and deleted", true),
        ("a refused put", false),
        ("a refused put", true),
    ] {
        let form = if use_arn { "arn" } else { "name" };
        let group = format!("/absence/{}-by-{form}", how.replace(' ', "-"));
        let identifier = group_identifier(&logs, &group, use_arn).await;
        let case = format!("{how}, by {form}");

        match how {
            "put and deleted" => {
                logs.ok(
                    "PutDataProtectionPolicy",
                    &json!({
                        "logGroupIdentifier": identifier,
                        "policyDocument": data_protection_document("EmailAddress"),
                    }),
                )
                .await;
                // While the policy is there the read reports it, so the empty answer below
                // follows the store rather than the shape of the call.
                let got = logs
                    .ok(
                        "GetDataProtectionPolicy",
                        &json!({"logGroupIdentifier": identifier}),
                    )
                    .await;
                assert_eq!(
                    got.get("policyDocument").and_then(Value::as_str),
                    Some(data_protection_document("EmailAddress").as_str()),
                    "case {case:?}: a group holding a policy reports its document, got {got}"
                );
                logs.ok(
                    "DeleteDataProtectionPolicy",
                    &json!({"logGroupIdentifier": identifier}),
                )
                .await;
            }
            "a refused put" => {
                let code = logs
                    .err(
                        "PutDataProtectionPolicy",
                        &json!({
                            "logGroupIdentifier": identifier,
                            "policyDocument": "this is not json",
                        }),
                    )
                    .await;
                assert_eq!(
                    code, INVALID_PARAMETER,
                    "case {case:?}: a document that is not JSON is refused, got {code}"
                );
            }
            _ => {}
        }

        assert_reports_no_policy(&logs, &case, &identifier).await;
        // Asking twice answers the same: the read reports and changes nothing.
        assert_reports_no_policy(&logs, &format!("{case}, asked twice"), &identifier).await;
    }

    // The boundary: a log group the account does not hold IS a missing resource, and the read
    // names the group that was looked for and the account that would own it.
    for (form, identifier) in [
        ("name", "/absence/never-created".to_string()),
        (
            "arn",
            format!(
                "arn:aws:logs:{}:{}:log-group:/absence/never-created",
                crate::harness::REGION,
                engine::emulator_account_id()
            ),
        ),
    ] {
        let (code, message) = logs
            .refusal(
                "GetDataProtectionPolicy",
                &json!({"logGroupIdentifier": identifier}),
            )
            .await;
        let expected = format!(
            "Log group '/absence/never-created' for owner '{}' cannot be found",
            engine::emulator_account_id()
        );
        assert_eq!(
            code, NOT_FOUND,
            "case a group that does not exist, by {form}: expected {NOT_FOUND}, got {code} {message:?}"
        );
        assert_eq!(
            message, expected,
            "case a group that does not exist, by {form}: expected {expected:?}, got {message:?}"
        );
    }
}

/// The delete of a data protection policy a group does not hold reports the key it found none
/// under — the owning account and the group the request resolved — whichever identifier form
/// named the group, whatever the group's name looks like, and however it came to hold none.
#[tokio::test]
async fn a_data_protection_delete_names_the_policy_key_it_found_nothing_under() {
    let logs = Logs::new();

    // Rows: group names spanning what a log group name can carry, since the name is the
    // variable half of the key.
    for name in ["/key/slashed", "key.dotted", "KeyUpperCase", "key_9"] {
        for (form, use_arn) in [("name", false), ("arn", true)] {
            let group = format!("{name}-{form}");
            let identifier = group_identifier(&logs, &group, use_arn).await;
            let expected = missing_policy_key(&group);

            let (code, message) = logs
                .refusal(
                    "DeleteDataProtectionPolicy",
                    &json!({"logGroupIdentifier": identifier}),
                )
                .await;
            assert_eq!(
                code, NOT_FOUND,
                "case {group} never held one, by {form}: expected {NOT_FOUND}, got {code} {message:?}"
            );
            assert_eq!(
                message, expected,
                "case {group} never held one, by {form}: expected {expected:?}, got {message:?}"
            );

            // A group that HELD one is served, and refused identically once it holds none
            // again: the refusal follows the state.
            logs.ok(
                "PutDataProtectionPolicy",
                &json!({
                    "logGroupIdentifier": identifier,
                    "policyDocument": data_protection_document("Address"),
                }),
            )
            .await;
            logs.ok(
                "DeleteDataProtectionPolicy",
                &json!({"logGroupIdentifier": identifier}),
            )
            .await;
            let (code, message) = logs
                .refusal(
                    "DeleteDataProtectionPolicy",
                    &json!({"logGroupIdentifier": identifier}),
                )
                .await;
            assert_eq!(
                code, NOT_FOUND,
                "case {group} deleted twice, by {form}: expected {NOT_FOUND}, got {code} {message:?}"
            );
            assert_eq!(
                message, expected,
                "case {group} deleted twice, by {form}: expected {expected:?}, got {message:?}"
            );
        }
    }
}

/// A data protection `policyDocument` the service's JSON parser cannot read is refused with
/// that parser's own failure: the token it stopped at and where that token begins — on the
/// log-group-level put and on the account-level put of the same document alike. A document
/// the parser CAN read keeps the sentence of the rule it breaks instead.
#[tokio::test]
async fn an_unreadable_policy_document_states_the_parsers_token_and_position() {
    let logs = Logs::new();
    const GROUP: &str = "/parse/target";
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;

    // Rows: where the unreadable token is (first character, after leading space, on a later
    // line, inside an object) and what it says (a bare word, a word a JSON literal starts
    // like, one carrying digits and underscores).
    let unreadable: Vec<(&str, String, String)> = vec![
        (
            "a bare word",
            "this is not json".to_string(),
            unreadable_json("this", 1, 1),
        ),
        (
            "a word a literal starts like",
            "not json".to_string(),
            unreadable_json("not", 1, 1),
        ),
        (
            "a word after leading space",
            "  truthy".to_string(),
            unreadable_json("truthy", 1, 3),
        ),
        (
            "a word on a later line",
            "\n\nfalsy".to_string(),
            unreadable_json("falsy", 3, 1),
        ),
        (
            "a word where a member's value belongs",
            "{\"Statement\": oops}".to_string(),
            unreadable_json("oops", 1, 15),
        ),
        (
            "a word carrying digits and an underscore",
            "policy_2021".to_string(),
            unreadable_json("policy_2021", 1, 1),
        ),
    ];

    for (case, document, expected) in &unreadable {
        let requests: [(&str, Value); 2] = [
            (
                "PutDataProtectionPolicy",
                json!({"logGroupIdentifier": GROUP, "policyDocument": document}),
            ),
            (
                "PutAccountPolicy",
                json!({
                    "policyName": "parse",
                    "policyType": DATA_PROTECTION_POLICY,
                    "policyDocument": document,
                }),
            ),
        ];
        for (operation, body) in requests {
            let (code, message) = logs.refusal(operation, &body).await;
            assert_eq!(
                code, INVALID_PARAMETER,
                "case {case:?} ({operation}): expected {INVALID_PARAMETER}, got {code} {message:?}"
            );
            assert_eq!(
                message, *expected,
                "case {case:?} ({operation}): expected {expected:?}, got {message:?}"
            );
        }
    }

    // The boundary: a document the parser READS and the rules then reject states the rule,
    // not the parser — so the template above is not the answer to every bad document.
    let readable: Vec<(&str, String)> = vec![
        ("a JSON array", json!(["Statement"]).to_string()),
        ("a JSON string", json!("Statement").to_string()),
        ("a JSON number", "42".to_string()),
        (
            "an object without the two blocks",
            json!({"Statement": []}).to_string(),
        ),
    ];
    for (case, document) in &readable {
        let (code, message) = logs
            .refusal(
                "PutDataProtectionPolicy",
                &json!({"logGroupIdentifier": GROUP, "policyDocument": document}),
            )
            .await;
        assert_eq!(
            code, INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {code} {message:?}"
        );
        assert!(
            !message.starts_with("Invalid policy JSON:"),
            "case {case:?}: a readable document is not refused as unreadable, got {message:?}"
        );
    }

    // And the document the rules admit is still stored, so none of the above refused on the
    // shape of the call rather than on the document.
    logs.ok(
        "PutDataProtectionPolicy",
        &json!({
            "logGroupIdentifier": GROUP,
            "policyDocument": data_protection_document("EmailAddress"),
        }),
    )
    .await;
}

/// A resource policy document the service's IAM policy parser will not take is refused with
/// that parser's fixed sentence, whichever form the document takes and whichever scope the
/// put names; the smallest document the parser DOES take is still accepted.
#[tokio::test]
async fn a_resource_policy_the_iam_parser_rejects_states_its_fixed_sentence() {
    let logs = Logs::new();
    const GROUP: &str = "/iam-grammar/group";
    let group_arn = group_identifier(&logs, GROUP, true).await;

    // Rows: the forms a document can fail to be an IAM policy document — not JSON at all,
    // JSON that is not the policy object, and a truncated document.
    let rejected = [
        ("not JSON at all", "not a policy".to_string()),
        ("a JSON array", json!(["Statement"]).to_string()),
        ("a JSON string", json!("Statement").to_string()),
        ("a JSON number", "42".to_string()),
        ("a truncated object", "{\"Statement\": ".to_string()),
    ];

    for (case, document) in &rejected {
        // Both scopes a put can name: the account-scoped policy, and one attached to a log
        // group's ARN.
        let requests: [(&str, Value); 2] = [
            (
                "account-scoped",
                json!({"policyName": "iam-grammar", "policyDocument": document}),
            ),
            (
                "resource-scoped",
                json!({
                    "policyName": "iam-grammar",
                    "policyDocument": document,
                    "resourceArn": group_arn,
                }),
            ),
        ];
        for (scope, body) in requests {
            let (code, message) = logs.refusal("PutResourcePolicy", &body).await;
            assert_eq!(
                code, INVALID_PARAMETER,
                "case {case:?} ({scope}): expected {INVALID_PARAMETER}, got {code} {message:?}"
            );
            assert_eq!(
                message, IAM_GRAMMAR,
                "case {case:?} ({scope}): expected {IAM_GRAMMAR:?}, got {message:?}"
            );
        }
    }

    // The sentence is fixed for the class, so it carries nothing of the request.
    for carried in ["iam-grammar", group_arn.as_str(), GROUP] {
        assert!(
            !IAM_GRAMMAR.contains(carried),
            "case the fixed sentence: it carries nothing of the request, got {carried:?}"
        );
    }

    // The documented minimum a document can be is still accepted.
    logs.ok(
        "PutResourcePolicy",
        &json!({"policyName": "iam-grammar", "policyDocument": "{}"}),
    )
    .await;
}

/// The field index policy surface words its own refusals: a readable document that names no
/// field index states that rule, and a log group the account does not hold states the kind of
/// resource that could not be found — on every operation of the surface and under either
/// identifier form.
#[tokio::test]
async fn the_field_index_policy_surface_states_its_own_refusals() {
    let logs = Logs::new();
    const GROUP: &str = "/index/target";
    logs.ok("CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;

    // Rows: the ways a readable document can name no field index — through either member the
    // document names its fields with, and through both at once.
    let empty = [
        ("an empty Fields array", json!({"Fields": []}).to_string()),
        ("an empty FieldsV2 object", json!({"FieldsV2": {}}).to_string()),
        (
            "both members empty",
            json!({"Fields": [], "FieldsV2": {}}).to_string(),
        ),
    ];
    for (case, document) in &empty {
        let requests: [(&str, Value); 2] = [
            (
                "PutIndexPolicy",
                json!({"logGroupIdentifier": GROUP, "policyDocument": document}),
            ),
            (
                "PutAccountPolicy",
                json!({
                    "policyName": "index",
                    "policyType": FIELD_INDEX_POLICY,
                    "policyDocument": document,
                }),
            ),
        ];
        for (operation, body) in requests {
            let (code, message) = logs.refusal(operation, &body).await;
            assert_eq!(
                code, INVALID_PARAMETER,
                "case {case:?} ({operation}): expected {INVALID_PARAMETER}, got {code} {message:?}"
            );
            assert_eq!(
                message, NO_FIELD,
                "case {case:?} ({operation}): expected {NO_FIELD:?}, got {message:?}"
            );
        }
    }

    // A log group the account does not hold, on every operation of the surface: the two that
    // write a log-group level policy and the one that reports it.
    const ABSENT: &str = "/index/never-created";
    let absent_arn = format!(
        "arn:aws:logs:{}:{}:log-group:{ABSENT}",
        crate::harness::REGION,
        engine::emulator_account_id()
    );
    let document = json!({"Fields": ["requestId"]}).to_string();
    for (form, identifier) in [("name", ABSENT.to_string()), ("arn", absent_arn)] {
        let requests: [(&str, Value); 3] = [
            (
                "PutIndexPolicy",
                json!({"logGroupIdentifier": identifier, "policyDocument": document}),
            ),
            (
                "DeleteIndexPolicy",
                json!({"logGroupIdentifier": identifier}),
            ),
            (
                "DescribeIndexPolicies",
                json!({"logGroupIdentifiers": [identifier]}),
            ),
        ];
        for (operation, body) in requests {
            let (code, message) = logs.refusal(operation, &body).await;
            assert_eq!(
                code, NOT_FOUND,
                "case a group that does not exist, by {form} ({operation}): expected {NOT_FOUND}, got {code} {message:?}"
            );
            assert_eq!(
                message, INDEX_GROUP_NOT_FOUND,
                "case a group that does not exist, by {form} ({operation}): expected {INDEX_GROUP_NOT_FOUND:?}, got {message:?}"
            );
            assert!(
                !message.contains(ABSENT),
                "case a group that does not exist, by {form} ({operation}): the sentence carries nothing of the request, got {message:?}"
            );
        }
    }

    // The same three operations serve a group the account DOES hold, so the refusals above
    // followed the store and not the shape of the call.
    logs.ok(
        "PutIndexPolicy",
        &json!({"logGroupIdentifier": GROUP, "policyDocument": document}),
    )
    .await;
    let page = logs
        .ok(
            "DescribeIndexPolicies",
            &json!({"logGroupIdentifiers": [GROUP]}),
        )
        .await;
    let policies = page
        .get("indexPolicies")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("DescribeIndexPolicies answers indexPolicies, got {page}"));
    assert_eq!(
        policies.len(),
        1,
        "case the served group: the stored policy is reported, got {page}"
    );
    logs.ok("DeleteIndexPolicy", &json!({"logGroupIdentifier": GROUP}))
        .await;
}
