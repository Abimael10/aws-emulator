// SPDX-License-Identifier: CC0-1.0

//! Which condition context keys a policy document REFERENCES, as the two
//! `GetContextKeysFor…Policy` reads report them.
//!
//! RULE: a document references a context key in the two places IAM reads one out of a
//! statement. It TESTS one in a `Condition` element — every condition-operator block is a map
//! from context key to the values it is tested against, so those map keys are referenced —
//! and it SUBSTITUTES one through a policy VARIABLE, `${<context key>}`, which IAM admits in
//! a statement's `Resource`/`NotResource` element and in a `Condition` clause's values. Both
//! are keys "you must supply when you call SimulateCustomPolicy", so both are reported:
//! an element carrying a variable cannot be matched at all without a value for the key it
//! names. IAM's three literal escapes (`${*}`, `${?}`, `${$}`) stand for characters rather
//! than keys and are not reported, a `${` that is never closed opens no variable, and context
//! key names are not distinguished by case, so two spellings of one key are one entry. The
//! same set is reported for the same documents whichever of the two reads asks — the custom
//! read over the documents the request carries, the principal read over the documents an
//! entity carries — and asking twice answers the same, because a read changes nothing.
//!
//! The rows span what the rule generalises over: where the key is referenced (a `Condition`
//! block's key, a `Resource` variable, a `NotResource` variable, a variable inside a
//! `Condition` value, and a variable inside a value LIST), which key family it belongs to (a
//! fixed global key, a tag key family with a `/` in its name, an IAM-specific key), how many
//! statements and how many documents carry them, the spelling's case, and the forms that
//! reference nothing (the escapes, an unclosed variable, a document with neither element).
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns and the other suites in
//! this crate share.

use crate::identity_policy_enforcement::grant_action;
use crate::user_lifecycle::{account_id, call, element, elements, error_code, router};

/// One row: the documents submitted, and the keys they reference.
struct Row {
    case: &'static str,
    documents: Vec<String>,
    /// The keys the rule requires, in any order.
    keys: Vec<&'static str>,
}

/// One statement, as a whole document.
fn statement(body: &str) -> String {
    format!(r#"{{"Version":"2012-10-17","Statement":[{body}]}}"#)
}

/// The rows.
fn rows() -> Vec<Row> {
    let account = account_id();
    vec![
        Row {
            case: "a Condition testing two global keys",
            documents: vec![statement(concat!(
                r#"{"Effect":"Allow","Action":"iam:ListUsers","Resource":"*","#,
                r#""Condition":{"DateGreaterThan":{"aws:CurrentTime":"2020-01-01T00:00:00Z"},"#,
                r#""Bool":{"aws:MultiFactorAuthPresent":"true"}}}"#
            ))],
            keys: vec!["aws:CurrentTime", "aws:MultiFactorAuthPresent"],
        },
        Row {
            case: "a Resource naming a policy variable",
            documents: vec![statement(&format!(
                r#"{{"Effect":"Allow","Action":"iam:GetUser","Resource":"arn:aws:iam::{account}:user/${{aws:username}}"}}"#
            ))],
            keys: vec!["aws:username"],
        },
        Row {
            case: "a Condition beside a Resource variable, in one statement",
            documents: vec![statement(&format!(
                concat!(
                    r#"{{"Effect":"Allow","Action":"iam:ChangePassword","#,
                    r#""Resource":"arn:aws:iam::{account}:user/${{aws:username}}","#,
                    r#""Condition":{{"DateGreaterThan":{{"aws:CurrentTime":"2020-01-01T00:00:00Z"}},"#,
                    r#""Bool":{{"aws:MultiFactorAuthPresent":"true"}}}}}}"#
                ),
                account = account
            ))],
            keys: vec![
                "aws:CurrentTime",
                "aws:MultiFactorAuthPresent",
                "aws:username",
            ],
        },
        Row {
            case: "a NotResource naming a tag-family variable",
            documents: vec![statement(&format!(
                r#"{{"Effect":"Deny","Action":"iam:TagUser","NotResource":"arn:aws:iam::{account}:user/${{aws:PrincipalTag/team}}"}}"#
            ))],
            keys: vec!["aws:PrincipalTag/team"],
        },
        Row {
            case: "a variable inside a Condition value",
            documents: vec![statement(concat!(
                r#"{"Effect":"Allow","Action":"iam:TagRole","Resource":"*","#,
                r#""Condition":{"StringEquals":{"iam:ResourceTag/owner":"${aws:username}"}}}"#
            ))],
            keys: vec!["iam:ResourceTag/owner", "aws:username"],
        },
        Row {
            case: "a variable inside a Condition value LIST",
            documents: vec![statement(concat!(
                r#"{"Effect":"Allow","Action":"iam:TagRole","Resource":"*","#,
                r#""Condition":{"StringLike":{"aws:ResourceTag/owner":"#,
                r#"["${aws:userid}","${aws:PrincipalTag/team}"]}}}"#
            ))],
            keys: vec![
                "aws:ResourceTag/owner",
                "aws:userid",
                "aws:PrincipalTag/team",
            ],
        },
        Row {
            case: "a Resource naming the three literal escapes only",
            documents: vec![statement(&format!(
                r#"{{"Effect":"Allow","Action":"iam:GetUser","Resource":"arn:aws:iam::{account}:user/${{*}}${{?}}${{$}}"}}"#
            ))],
            keys: Vec::new(),
        },
        Row {
            case: "a Resource whose variable is never closed",
            documents: vec![statement(&format!(
                r#"{{"Effect":"Allow","Action":"iam:GetUser","Resource":"arn:aws:iam::{account}:user/${{aws:username"}}"#
            ))],
            keys: Vec::new(),
        },
        Row {
            case: "a document with neither a Condition nor a variable",
            documents: vec![statement(
                r#"{"Effect":"Allow","Action":"iam:ListUsers","Resource":"*"}"#,
            )],
            keys: Vec::new(),
        },
        Row {
            // Context key names are not distinguished by case, so the two spellings are ONE
            // key: one entry, reported in the spelling the document wrote first.
            case: "one key spelled two ways, in two statements of one document",
            documents: vec![format!(
                concat!(
                    r#"{{"Version":"2012-10-17","Statement":["#,
                    r#"{{"Effect":"Allow","Action":"iam:GetUser","#,
                    r#""Resource":"arn:aws:iam::{account}:user/${{AWS:UserName}}"}},"#,
                    r#"{{"Effect":"Allow","Action":"iam:ListUsers","Resource":"*","#,
                    r#""Condition":{{"StringEquals":{{"aws:username":"anybody"}}}}}}]}}"#
                ),
                account = account
            )],
            keys: vec!["AWS:UserName"],
        },
        Row {
            case: "two documents, each referencing a key the other does not",
            documents: vec![
                statement(concat!(
                    r#"{"Effect":"Allow","Action":"iam:ListRoles","Resource":"*","#,
                    r#""Condition":{"StringEquals":{"aws:RequestedRegion":"us-east-1"}}}"#
                )),
                statement(&format!(
                    r#"{{"Effect":"Allow","Action":"iam:GetUser","Resource":"arn:aws:iam::{account}:user/${{aws:username}}"}}"#
                )),
            ],
            keys: vec!["aws:RequestedRegion", "aws:username"],
        },
    ]
}

/// The keys an answer reports, sorted — the list's members, which is how a client reads
/// `ContextKeyNames`.
fn reported(body: &str) -> Vec<String> {
    let mut keys = elements(body, "member");
    keys.sort();
    keys
}

/// The keys a row requires, sorted. A key is reported in the spelling the document wrote,
/// and context key names are not distinguished by case, so a row that writes one key twice
/// expects the one entry in the spelling that came first.
fn expected(keys: &[&str]) -> Vec<String> {
    let mut folded: Vec<String> = keys.iter().map(|key| key.to_string()).collect();
    folded.sort();
    folded
}

/// RULE: `GetContextKeysForCustomPolicy` reports every key the submitted documents
/// reference — tested in a `Condition` or substituted through a policy variable — and nothing
/// else.
#[tokio::test]
async fn the_custom_read_reports_every_key_the_documents_reference() {
    let router = router();

    for row in rows() {
        let mut params: Vec<(String, String)> = Vec::new();
        for (position, document) in row.documents.iter().enumerate() {
            params.push((
                format!("PolicyInputList.member.{}", position + 1),
                document.clone(),
            ));
        }
        let borrowed: Vec<(&str, &str)> = params
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();

        let (status, body) = call(&router, "GetContextKeysForCustomPolicy", &borrowed).await;

        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status}: {body}",
            row.case
        );
        assert_eq!(
            reported(&body),
            expected(&row.keys),
            "case {:?}: expected exactly the keys the documents reference, got {body}",
            row.case
        );

        // Asking again answers the same: the read reports what the request carries and
        // changes nothing.
        let (status, again) = call(&router, "GetContextKeysForCustomPolicy", &borrowed).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the repeated read to be served, got {status}: {again}",
            row.case
        );
        assert_eq!(
            reported(&again),
            reported(&body),
            "case {:?}: expected the repeated read to report the same keys, got {again}",
            row.case
        );
    }
}

/// RULE: the same keys are reported for the same documents by the PRINCIPAL read, over the
/// documents the entity carries — so a caller enumerating what it must supply to a simulation
/// gets one answer whichever read it asks.
#[tokio::test]
async fn the_principal_read_reports_the_keys_the_entitys_documents_reference() {
    let router = router();

    for (position, row) in rows().into_iter().enumerate() {
        let user_name = format!("keyholder{position}");
        let (status, body) = call(&router, "CreateUser", &[("UserName", &user_name)]).await;
        assert_eq!(
            status, 200,
            "case {:?}: the fixture CreateUser must succeed, got {status}: {body}",
            row.case
        );
        for (at, document) in row.documents.iter().enumerate() {
            let (status, put) = call(
                &router,
                "PutUserPolicy",
                &[
                    ("UserName", &user_name),
                    ("PolicyName", &format!("carried{at}")),
                    ("PolicyDocument", document),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: the fixture PutUserPolicy must succeed, got {status}: {put}",
                row.case
            );
        }
        // The read is authorized like every other IAM call, so the entity whose documents
        // are read is not what decides it: the request is unsigned here, as the custom read
        // above is.
        let (status, body) = call(
            &router,
            "GetContextKeysForPrincipalPolicy",
            &[(
                "PolicySourceArn",
                &format!("arn:aws:iam::{}:user/{user_name}", account_id()),
            )],
        )
        .await;

        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status}: {body}",
            row.case
        );
        assert_eq!(
            reported(&body),
            expected(&row.keys),
            "case {:?}: expected exactly the keys the entity's documents reference, got {body}",
            row.case
        );
    }
}

/// RULE: a syntactically invalid entry in `PolicyInputList` is refused `InvalidInput` naming
/// the entry by its POSITION in the list, counted from one — so a caller submitting several
/// documents learns which one was rejected.
#[tokio::test]
async fn an_invalid_entry_is_refused_by_its_position_in_the_list() {
    let router = router();
    let valid = statement(r#"{"Effect":"Allow","Action":"iam:ListUsers","Resource":"*"}"#);
    // The rows walk the position of the offending entry: first, second and third of three,
    // so a refusal naming a fixed index would fail.
    for offending in 1..=3 {
        let documents: Vec<String> = (1..=3)
            .map(|position| {
                if position == offending {
                    "not a policy document at all".to_string()
                } else {
                    valid.clone()
                }
            })
            .collect();
        let params: Vec<(String, String)> = documents
            .iter()
            .enumerate()
            .map(|(at, document)| {
                (
                    format!("PolicyInputList.member.{}", at + 1),
                    document.clone(),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = params
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();

        let (status, body) = call(&router, "GetContextKeysForCustomPolicy", &borrowed).await;

        assert_eq!(
            status, 400,
            "case {offending}: expected HTTP 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidInput",
            "case {offending}: expected the error code InvalidInput, got {:?}: {body}",
            error_code(&body)
        );
        assert_eq!(
            element(&body, "Message"),
            format!("Policy input list item {offending} has invalid content"),
            "case {offending}: the refusal must name the offending entry by its position"
        );
    }
}

/// RULE: the read is authorized like every other IAM call, so a signing principal whose
/// documents do not allow it is refused — and the keys of the documents it submitted are not
/// reported to it.
#[tokio::test]
async fn the_read_is_authorized_against_the_signing_principals_policies() {
    let router = router();
    let user_name = "unprivileged";
    let access_key_id = crate::identity_policy_enforcement::principal(
        &router,
        "an unprivileged caller",
        user_name,
        &[],
    )
    .await;
    let document = statement(r#"{"Effect":"Allow","Action":"iam:ListUsers","Resource":"*"}"#);

    let (status, body) = crate::user_lifecycle::call_signed_by(
        &router,
        Some(&access_key_id),
        "GetContextKeysForCustomPolicy",
        &[("PolicyInputList.member.1", &document)],
    )
    .await;
    assert_eq!(
        status, 403,
        "expected the unprivileged read to be refused, got {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "AccessDenied",
        "expected the error code AccessDenied, got {:?}: {body}",
        error_code(&body)
    );

    // The same caller, granted the action, is served — so the refusal above is about its
    // permissions and not about the request.
    grant_action(
        &router,
        "a granted caller",
        user_name,
        "iam:GetContextKeysForCustomPolicy",
    )
    .await;
    let (status, body) = crate::user_lifecycle::call_signed_by(
        &router,
        Some(&access_key_id),
        "GetContextKeysForCustomPolicy",
        &[("PolicyInputList.member.1", &document)],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the granted read to be served, got {status}: {body}"
    );
}
