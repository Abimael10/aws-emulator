// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ACCOUNT PRINCIPAL spelling the properties of a domain's
//! configuration change report an access policy document in.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees. The harness comes from [`super::domain_lifecycle`] and the batch read from
//! [`super::domain_batch_describe`]; this suite adds no second copy of either.
//!
//! The rule under test, asserted over a table of principal spellings rather than one
//! recorded call: a PROPERTY of a configuration change reports the value the service is
//! applying, and a principal that names a whole ACCOUNT is one value however the submitted
//! document spelled it — the account's root ARN and the bare account id name the same
//! account — so the change reports it as the account itself, while the configuration's own
//! `AccessPolicies` member keeps the document the caller submitted. The two therefore
//! differ exactly where the submitted document named an account by its root ARN, and
//! nothing else in the document moves.
//!
//! Every path that reports a change's properties is held to it: `ModifyingProperties` on
//! the `DomainStatus` a create and a read answer with, on the `DomainConfig` the
//! configuration read answers with, on the element the BATCH read serves, and the
//! `PendingProperties` of the change-progress read.

use serde_json::{json, Value};

use super::domain_batch_describe::describe_domains;
use super::domain_lifecycle::{settled_domain, Emulator, PREFIX};

/// `PropertyValueType`'s `STRINGIFIED_JSON`, as `ModifyingProperties$ValueType` reports a
/// value that is "content in JSON format"
/// (`smithy_query(query="shape",shape="com.amazonaws.opensearch#PropertyValueType")`).
const STRINGIFIED_JSON: &str = "STRINGIFIED_JSON";

/// An account OTHER than the one this emulator serves, so a row covers an account principal
/// that is not the caller's own: twelve digits, which is every account id's form.
const OTHER_ACCOUNT: &str = "111122223333";

/// One row of the table: the case it stands for, the policy document the request submits,
/// the principal text the CONFIGURATION reports it with, and the principal text a change's
/// PROPERTY reports it with.
struct Row {
    case: &'static str,
    submitted: String,
    configured_principal: String,
    changed_principal: String,
}

/// The rows differ along what the rule generalises over: the spelling the account principal
/// was submitted in (a root ARN, the bare account id already, a root ARN of another
/// partition), whose account it is, whether the principal sits in `Principal` or in
/// `NotPrincipal`, whether it is written alone or inside a list, whether the value names an
/// account at all (a role ARN, the wildcard), and whether a root ARN sits somewhere that is
/// no principal at all (a `Sid`).
fn rows() -> Vec<Row> {
    let account = ::engine::emulator_account_id();
    vec![
        Row {
            case: "an account named by its root ARN",
            submitted: statement(&format!(
                "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"arn:aws:iam::{account}:root\"}},\
                 \"Action\":\"es:*\",\"Resource\":\"*\""
            )),
            configured_principal: format!("\"AWS\":\"arn:aws:iam::{account}:root\""),
            changed_principal: format!("\"AWS\":\"{account}\""),
        },
        Row {
            case: "an account named by its bare id",
            submitted: statement(&format!(
                "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{account}\"}},\
                 \"Action\":\"es:*\",\"Resource\":\"*\""
            )),
            configured_principal: format!("\"AWS\":\"{account}\""),
            changed_principal: format!("\"AWS\":\"{account}\""),
        },
        Row {
            case: "another account named by its root ARN",
            submitted: statement(&format!(
                "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\
                 \"arn:aws:iam::{OTHER_ACCOUNT}:root\"}},\"Action\":\"es:ESHttpGet\",\
                 \"Resource\":\"*\""
            )),
            configured_principal: format!("\"AWS\":\"arn:aws:iam::{OTHER_ACCOUNT}:root\""),
            changed_principal: format!("\"AWS\":\"{OTHER_ACCOUNT}\""),
        },
        Row {
            case: "an account of another partition named by its root ARN",
            submitted: statement(&format!(
                "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\
                 \"arn:aws-cn:iam::{OTHER_ACCOUNT}:root\"}},\"Action\":\"es:*\",\
                 \"Resource\":\"*\""
            )),
            configured_principal: format!("\"AWS\":\"arn:aws-cn:iam::{OTHER_ACCOUNT}:root\""),
            changed_principal: format!("\"AWS\":\"{OTHER_ACCOUNT}\""),
        },
        Row {
            case: "two account root ARNs written as a list",
            submitted: statement(&format!(
                "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\
                 [\"arn:aws:iam::{account}:root\",\"arn:aws:iam::{OTHER_ACCOUNT}:root\"]}},\
                 \"Action\":\"es:*\",\"Resource\":\"*\""
            )),
            configured_principal: format!(
                "\"AWS\":[\"arn:aws:iam::{account}:root\",\"arn:aws:iam::{OTHER_ACCOUNT}:root\"]"
            ),
            changed_principal: format!("\"AWS\":[\"{account}\",\"{OTHER_ACCOUNT}\"]"),
        },
        Row {
            case: "an account root ARN a statement DENIES by naming every other principal",
            submitted: statement(&format!(
                "\"Effect\":\"Deny\",\"NotPrincipal\":{{\"AWS\":\
                 \"arn:aws:iam::{account}:root\"}},\"Action\":\"es:*\",\"Resource\":\"*\""
            )),
            configured_principal: format!("\"AWS\":\"arn:aws:iam::{account}:root\""),
            changed_principal: format!("\"AWS\":\"{account}\""),
        },
        Row {
            case: "an IAM role, which names no account",
            submitted: statement(&format!(
                "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\
                 \"arn:aws:iam::{account}:role/search\"}},\"Action\":\"es:*\",\
                 \"Resource\":\"*\""
            )),
            configured_principal: format!("\"AWS\":\"arn:aws:iam::{account}:role/search\""),
            changed_principal: format!("\"AWS\":\"arn:aws:iam::{account}:role/search\""),
        },
        Row {
            case: "the wildcard principal, which names no account",
            submitted: statement(
                "\"Effect\":\"Allow\",\"Principal\":\"*\",\"Action\":\"es:ESHttpGet\",\
                 \"Resource\":\"*\"",
            ),
            configured_principal: "\"Principal\":\"*\"".to_owned(),
            changed_principal: "\"Principal\":\"*\"".to_owned(),
        },
        Row {
            case: "a root ARN written somewhere that names no principal",
            submitted: statement(&format!(
                "\"Sid\":\"arn:aws:iam::{account}:root\",\"Effect\":\"Allow\",\
                 \"Principal\":\"*\",\"Action\":\"es:*\",\"Resource\":\"*\""
            )),
            configured_principal: format!("\"Sid\":\"arn:aws:iam::{account}:root\""),
            changed_principal: format!("\"Sid\":\"arn:aws:iam::{account}:root\""),
        },
    ]
}

/// A one-statement policy document carrying `statement` as its only statement.
fn statement(statement: &str) -> String {
    format!("{{\"Version\":\"2012-10-17\",\"Statement\":[{{{statement}}}]}}")
}

/// The entry `ModifyingProperties` carries for the property `name`.
fn modifying<'a>(status: &'a Value, name: &str, case: &str) -> &'a Value {
    status["ModifyingProperties"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected ModifyingProperties, got {status:?}"))
        .iter()
        .find(|entry| entry["Name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("case {case:?}: expected an entry for {name}, got {status:?}"))
}

/// The `ChangeProgressStatus` of the change in flight on `name`.
async fn progress(emulator: &Emulator, name: &str, case: &str) -> Value {
    let answer = emulator
        .call("GET", &format!("{PREFIX}/opensearch/domain/{name}/progress"), None)
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected DescribeDomainChangeProgress({name:?}) to answer 200, got {} \
         with {:?}",
        answer.status, answer.body
    );
    answer.body["ChangeProgressStatus"].clone()
}

/// What the document `submitted` carries where the principal text `expected` is expected,
/// asserted as a substring so the assertion holds whatever the rest of the document does.
fn assert_carries(reported: Option<&str>, expected: &str, case: &str, reader: &str) {
    assert!(
        reported.is_some_and(|document| document.contains(expected)),
        "case {case:?} ({reader}): expected the principal {expected:?}, got {reported:?}"
    );
}

/// RULE: the properties of the change a CREATE starts report the submitted access policy
/// with every whole-account principal written as the account itself, while the
/// configuration keeps the document the request submitted — on every path that reports
/// either, the batch read included.
#[tokio::test]
async fn a_change_property_reports_an_account_principal_as_the_account() {
    let emulator = Emulator::new();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let name = format!("policy-principal-{index}");
        let created = emulator
            .create(json!({ "DomainName": name, "AccessPolicies": row.submitted }))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200 from CreateDomain, got {} with {:?}",
            created.status, created.body
        );

        let described = emulator.describe(&name).await;
        let configured = emulator.describe_config(&name).await;
        let batch = describe_domains(&emulator, &[&name]).await;
        let batched = batch.body["DomainStatusList"][0].clone();

        // The configuration itself is the caller's document: every reader of it reports the
        // policy with the principal spelled as the request spelled it.
        for (reader, reported) in [
            (
                "CreateDomain",
                created.body["DomainStatus"]["AccessPolicies"].as_str(),
            ),
            (
                "DescribeDomain",
                described.body["DomainStatus"]["AccessPolicies"].as_str(),
            ),
            (
                "DescribeDomains",
                batched["AccessPolicies"].as_str(),
            ),
            (
                "DescribeDomainConfig",
                configured.body["DomainConfig"]["AccessPolicies"]["Options"].as_str(),
            ),
        ] {
            assert_carries(reported, &row.configured_principal, case, reader);
        }

        // ... and the property the change in flight reports for it carries the account
        // spelling, on each of the paths that report a change's properties.
        for (reader, status) in [
            ("CreateDomain", &created.body["DomainStatus"]),
            ("DescribeDomain", &described.body["DomainStatus"]),
            ("DescribeDomains", &batched),
            ("DescribeDomainConfig", &configured.body["DomainConfig"]),
        ] {
            let entry = modifying(status, "AccessPolicies", case);
            assert_carries(
                entry["PendingValue"].as_str(),
                &row.changed_principal,
                case,
                reader,
            );
            assert_eq!(
                entry["ValueType"].as_str(),
                Some(STRINGIFIED_JSON),
                "case {case:?} ({reader}): expected a policy document to be JSON content, got \
                 {entry:?}"
            );
        }

        // ... and so does the change-progress read's own list of the properties still
        // pending, which names the same property the same way.
        let pending = progress(&emulator, &name, case).await["PendingProperties"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected PendingProperties"))
            .iter()
            .map(|entry| entry.as_str().unwrap_or_default().to_owned())
            .collect::<Vec<String>>();
        assert!(
            pending.iter().any(|entry| entry.starts_with("AccessPolicies : ")
                && entry.contains(&row.changed_principal)),
            "case {case:?}: expected an AccessPolicies entry carrying {:?} among {pending:?}",
            row.changed_principal
        );

        // The batch read serves the element the single-domain read serves, member for
        // member, so the two cannot report one policy two ways.
        assert_eq!(
            batched, described.body["DomainStatus"],
            "case {case:?}: expected the batch element to equal the single read's DomainStatus \
             {:?}, got {batched:?}",
            described.body["DomainStatus"]
        );
    }
}

/// RULE: the same spelling holds for the change an UPDATE starts, so the rule is the
/// change's and not the create's.
#[tokio::test]
async fn an_updated_access_policy_reports_the_same_account_spelling() {
    let emulator = Emulator::new();
    let name = "policy-principal-update";
    let account = ::engine::emulator_account_id();
    let created = emulator.create(json!({ "DomainName": name })).await;
    assert_eq!(
        created.status, 200,
        "case update: expected 200 from CreateDomain, got {} with {:?}",
        created.status, created.body
    );
    // The create's own change is settled first — polled for, not waited out — so what the
    // properties below report is the change the UPDATE starts.
    settled_domain(&emulator, name).await;

    let submitted = statement(&format!(
        "\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":[\"arn:aws:iam::{account}:root\"]}},\
         \"Action\":\"es:*\",\"Resource\":\"*\""
    ));
    let updated = emulator
        .update_config(name, json!({ "AccessPolicies": submitted.clone() }))
        .await;
    assert_eq!(
        updated.status, 200,
        "case update: expected 200 from UpdateDomainConfig, got {} with {:?}",
        updated.status, updated.body
    );

    let case = "an update that names the account by its root ARN";
    let entry = modifying(&updated.body["DomainConfig"], "AccessPolicies", case);
    assert_carries(
        entry["PendingValue"].as_str(),
        &format!("\"AWS\":[\"{account}\"]"),
        case,
        "UpdateDomainConfig",
    );
    assert_carries(
        updated.body["DomainConfig"]["AccessPolicies"]["Options"].as_str(),
        &format!("\"AWS\":[\"arn:aws:iam::{account}:root\"]"),
        case,
        "UpdateDomainConfig",
    );
    // And the document the update put is the one in force for a later reader.
    let described = emulator.describe(name).await;
    assert_eq!(
        described.body["DomainStatus"]["AccessPolicies"].as_str(),
        Some(submitted.as_str()),
        "case {case:?}: expected the submitted document to be the one in force, got {:?}",
        described.body["DomainStatus"]["AccessPolicies"]
    );
}
