// SPDX-License-Identifier: CC0-1.0

//! The DIAGNOSTIC an authorization failure hands the caller: the encoded authorization
//! failure message that names the decision this gate just took, and the statement it rested
//! on.
//!
//! "Some Amazon Web Services operations additionally return an encoded message that can
//! provide details about this authorization failure… The decoded message includes the
//! following type of information: Whether the request was denied due to an explicit deny or
//! due to the absence of an explicit allow… The principal who made the request. The
//! requested action. The requested resource. The values of condition keys in the context of
//! the user's request" (`sts:DecodeAuthorizationMessage`, the STS model). The blob is
//! therefore minted from the decision the gate ALREADY made — nothing is re-decided here —
//! and recorded in the registry STS decodes one from, so what a caller decodes is this very
//! decision rather than a document written to look like one.
//!
//! WHEN a refusal carries one is the gate's own reading ([`super::authorization`]); this
//! module is only how the decision is rendered and recorded.
//!
//! The statements are read off the policy documents the decision was taken over, by the
//! statement positions the shared evaluator reports
//! ([`::service_common::iam_authorization::MatchedStatement`]), so a statement the caller
//! decodes is the statement that decided — its `Sid`, its `Effect`, the actions and
//! resources it names and the condition keys it tests. A statement of a document attached
//! to an identity names no `Principal` of its own, so the principal reported for one is the
//! id of the principal that made the request: the statement applied to THAT principal and
//! to no other.

use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization as iam;

/// The elements of a policy statement that carry a list of values, each of which IAM also
/// spells with a `Not` prefix matching everything its values do not; both spellings name the
/// element's values, which is what the decoded message reports.
const LISTED_ELEMENTS: &[(&str, &str)] = &[("Action", "NotAction"), ("Resource", "NotResource")];

/// The statements `matched` names, read off the documents the decision was taken over.
///
/// `documents` are the documents in the order they were handed to the evaluator, which is
/// what [`iam::MatchedStatement::document`] counts in; a position neither the document list
/// nor its `Statement` element carries contributes nothing, so a report can never name a
/// statement that is not there.
#[must_use]
pub fn matched_statements(
    documents: &[String],
    matched: &[iam::MatchedStatement],
    principal_id: &str,
) -> Vec<sts_store::MatchedStatement> {
    let read: Vec<Option<::serde_json::Value>> = documents
        .iter()
        .map(|document| ::serde_json::from_str::<::serde_json::Value>(document).ok())
        .collect();
    matched
        .iter()
        .filter_map(|at| {
            let document = read.get(at.document)?.as_ref()?;
            let statement = statement_at(document, at.statement)?;
            Some(statement_record(statement, principal_id))
        })
        .collect()
}

/// The statement at `position` of `document`'s `Statement` element — the entry of the list,
/// or the single statement a document that states one object carries, which is position zero.
///
/// The SAME numbering the shared evaluator walks statements in, so a reported position names
/// the statement that decided.
fn statement_at(document: &::serde_json::Value, position: usize) -> Option<&::serde_json::Value> {
    let statement = document.get("Statement")?;
    match statement {
        ::serde_json::Value::Array(entries) => entries.get(position),
        ::serde_json::Value::Object(_) if position == 0 => Some(statement),
        _ => None,
    }
}

/// One statement as the decoded message reports it.
///
/// The effect is reported in upper case, which is how the decoded message words one, and the
/// principal is `principal_id` for the reason the module docs give.
fn statement_record(
    statement: &::serde_json::Value,
    principal_id: &str,
) -> sts_store::MatchedStatement {
    let mut listed = LISTED_ELEMENTS.iter().map(|(element, negated)| {
        let values = element_values(statement, element);
        if values.is_empty() {
            element_values(statement, negated)
        } else {
            values
        }
    });
    sts_store::MatchedStatement {
        statement_id: statement
            .get("Sid")
            .and_then(::serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        effect: statement
            .get("Effect")
            .and_then(::serde_json::Value::as_str)
            .unwrap_or_default()
            .to_ascii_uppercase(),
        principals: vec![principal_id.to_string()],
        actions: listed.next().unwrap_or_default(),
        resources: listed.next().unwrap_or_default(),
        conditions: statement_conditions(statement),
    }
}

/// The values a statement's `element` carries: IAM writes each of them "either as one string
/// or as a list of them", so both forms read as the same list of values.
fn element_values(statement: &::serde_json::Value, element: &str) -> Vec<String> {
    match statement.get(element) {
        Some(::serde_json::Value::String(value)) => vec![value.clone()],
        Some(::serde_json::Value::Array(values)) => values
            .iter()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// The condition keys a statement tests, each with the values its clause tests for.
///
/// A `Condition` is operator-keyed (`{"StringEquals":{"aws:PrincipalTag/team":"red"}}`) and
/// the decoded message reports the KEYS rather than the operators, so every operator's keys
/// are read into one list; a clause's value is "either one string or a list of them", as
/// everywhere else in the grammar.
fn statement_conditions(statement: &::serde_json::Value) -> Vec<(String, Vec<String>)> {
    let Some(::serde_json::Value::Object(operators)) = statement.get("Condition") else {
        return Vec::new();
    };
    let mut conditions: Vec<(String, Vec<String>)> = Vec::new();
    for clauses in operators.values() {
        let Some(clauses) = clauses.as_object() else {
            continue;
        };
        for (key, value) in clauses {
            let values = match value {
                ::serde_json::Value::Array(values) => {
                    values.iter().map(rendered_value).collect::<Vec<String>>()
                }
                other => vec![rendered_value(other)],
            };
            fold_values(&mut conditions, key, values);
        }
    }
    conditions
}

/// A condition clause's value as text: the string it is, or the JSON rendering of a value of
/// another type — IAM's own grammar admits a boolean or a number for its boolean, numeric and
/// date operators, and those read as the text the document wrote.
fn rendered_value(value: &::serde_json::Value) -> String {
    match value {
        ::serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Note `values` under `key`, appending to the entry a key already read rather than opening a
/// second one, so one key is one entry of the report.
fn fold_values(into: &mut Vec<(String, Vec<String>)>, key: &str, values: Vec<String>) {
    match into.iter_mut().find(|(held, _)| held == key) {
        Some((_, held)) => held.extend(values),
        None => into.push((key.to_string(), values)),
    }
}

/// The condition keys `pairs` carries, one entry per key with every value it was passed
/// under.
///
/// A multi-valued condition key travels as one `(key, value)` pair per value, which is how
/// the shared evaluator reads one, and the decoded message reports it as one key carrying
/// those values.
#[must_use]
pub fn folded_conditions(pairs: &[(String, String)]) -> Vec<(String, Vec<String>)> {
    let mut folded: Vec<(String, Vec<String>)> = Vec::new();
    for (key, value) in pairs {
        fold_values(&mut folded, key, vec![value.clone()]);
    }
    folded
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One document holding `statements` as its `Statement` element.
    fn document(statements: &str) -> String {
        format!("{{\"Version\":\"2012-10-17\",\"Statement\":{statements}}}")
    }

    /// RULE: a statement the decision matched is reported as the document wrote it — its
    /// `Sid`, its `Effect` in upper case, the actions and resources it names in either of
    /// IAM's two spellings for a list, and the keys its `Condition` tests — with the
    /// requesting principal's id as the principal it applied to.
    ///
    /// The rows span what the rule generalises over: how the element is written (one string,
    /// a list of them, the negated spelling, absent), whether the statement states a `Sid`,
    /// the effect's own casing, how the statement element itself is written (a list, a single
    /// object) and whether the statement tests conditions.
    #[test]
    fn a_matched_statement_is_reported_as_the_document_wrote_it() {
        let principal = "AROAEXAMPLEROLEID0001:probe";
        // (case, the `Statement` element, the position matched, the expected report)
        let rows: Vec<(&str, String, usize, sts_store::MatchedStatement)> = vec![
            (
                "one action and one resource, each a string, with a Sid",
                document(
                    "[{\"Sid\":\"DenyLaunch\",\"Effect\":\"Deny\",\
                     \"Action\":\"ec2:RunInstances\",\"Resource\":\"*\"}]",
                ),
                0,
                sts_store::MatchedStatement {
                    statement_id: "DenyLaunch".to_string(),
                    effect: "DENY".to_string(),
                    principals: vec![principal.to_string()],
                    actions: vec!["ec2:RunInstances".to_string()],
                    resources: vec!["*".to_string()],
                    conditions: Vec::new(),
                },
            ),
            (
                "lists of actions and resources, no Sid, the second statement",
                document(
                    "[{\"Effect\":\"Allow\",\"Action\":\"ec2:*\",\"Resource\":\"*\"},\
                     {\"Effect\":\"allow\",\"Action\":[\"ec2:StopInstances\",\
                     \"ec2:StartInstances\"],\"Resource\":[\"arn:aws:ec2:us-west-2:\
                     000000000000:instance/i-0123456789abcdef0\",\"*\"]}]",
                ),
                1,
                sts_store::MatchedStatement {
                    statement_id: String::new(),
                    effect: "ALLOW".to_string(),
                    principals: vec![principal.to_string()],
                    actions: vec![
                        "ec2:StopInstances".to_string(),
                        "ec2:StartInstances".to_string(),
                    ],
                    resources: vec![
                        "arn:aws:ec2:us-west-2:000000000000:instance/i-0123456789abcdef0"
                            .to_string(),
                        "*".to_string(),
                    ],
                    conditions: Vec::new(),
                },
            ),
            (
                "the negated spellings of both elements",
                document(
                    "[{\"Effect\":\"Deny\",\"NotAction\":[\"ec2:DescribeInstances\"],\
                     \"NotResource\":\"arn:aws:ec2:us-west-2:000000000000:subnet/*\"}]",
                ),
                0,
                sts_store::MatchedStatement {
                    statement_id: String::new(),
                    effect: "DENY".to_string(),
                    principals: vec![principal.to_string()],
                    actions: vec!["ec2:DescribeInstances".to_string()],
                    resources: vec![
                        "arn:aws:ec2:us-west-2:000000000000:subnet/*".to_string(),
                    ],
                    conditions: Vec::new(),
                },
            ),
            (
                "a single statement object carrying two condition clauses",
                document(
                    "{\"Effect\":\"Deny\",\"Action\":\"ec2:RunInstances\",\"Resource\":\"*\",\
                     \"Condition\":{\"StringEquals\":{\"ec2:InstanceType\":[\"t3.micro\",\
                     \"t3.small\"]},\"Bool\":{\"aws:SecureTransport\":false}}}",
                ),
                0,
                sts_store::MatchedStatement {
                    statement_id: String::new(),
                    effect: "DENY".to_string(),
                    principals: vec![principal.to_string()],
                    actions: vec!["ec2:RunInstances".to_string()],
                    resources: vec!["*".to_string()],
                    conditions: vec![
                        (
                            "aws:SecureTransport".to_string(),
                            vec!["false".to_string()],
                        ),
                        (
                            "ec2:InstanceType".to_string(),
                            vec!["t3.micro".to_string(), "t3.small".to_string()],
                        ),
                    ],
                },
            ),
            (
                "a statement naming neither element",
                document("[{\"Effect\":\"Deny\"}]"),
                0,
                sts_store::MatchedStatement {
                    statement_id: String::new(),
                    effect: "DENY".to_string(),
                    principals: vec![principal.to_string()],
                    actions: Vec::new(),
                    resources: Vec::new(),
                    conditions: Vec::new(),
                },
            ),
        ];
        for (case, policy, position, expected) in rows {
            let matched = [iam::MatchedStatement {
                document: 0,
                statement: position,
            }];
            let mut reported =
                matched_statements(::std::slice::from_ref(&policy), &matched, principal);
            // The clauses of one `Condition` are reported in a total order, so the row's
            // expectation does not depend on how a JSON object's members happened to be read.
            for statement in &mut reported {
                statement.conditions.sort();
            }
            assert_eq!(
                reported,
                vec![expected.clone()],
                "case {case:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// RULE: a position no document or no statement carries is reported as no statement at
    /// all, so a report never names a statement that is not there.
    #[test]
    fn a_position_nothing_carries_reports_no_statement() {
        let policy = document("[{\"Effect\":\"Deny\",\"Action\":\"ec2:RunInstances\"}]");
        // (case, the documents, the position matched)
        let rows: &[(&str, &[String], iam::MatchedStatement)] = &[
            (
                "no document at that position",
                &[],
                iam::MatchedStatement {
                    document: 0,
                    statement: 0,
                },
            ),
            (
                "no statement at that position",
                ::std::slice::from_ref(&policy),
                iam::MatchedStatement {
                    document: 0,
                    statement: 7,
                },
            ),
            (
                "a second document that is not there",
                ::std::slice::from_ref(&policy),
                iam::MatchedStatement {
                    document: 1,
                    statement: 0,
                },
            ),
        ];
        for (case, documents, at) in rows {
            let reported = matched_statements(documents, ::std::slice::from_ref(at), "AIDAPROBE");
            assert!(
                reported.is_empty(),
                "case {case:?}: expected no statement, got {reported:?}"
            );
        }
    }

    /// One row of the table below: the case, the `(key, value)` pairs the decision was taken
    /// over, and the entries the report folds them into.
    type FoldedRow = (
        &'static str,
        Vec<(String, String)>,
        Vec<(String, Vec<String>)>,
    );

    /// RULE: the condition context is reported one entry per KEY, carrying every value that
    /// key was passed under — a multi-valued key travels as one pair per value — in the order
    /// the keys were first named.
    #[test]
    fn the_condition_context_is_folded_by_key() {
        // (case, the pairs the decision was taken over, the entries reported)
        let rows: Vec<FoldedRow> = vec![
            ("no key at all", Vec::new(), Vec::new()),
            (
                "one key with one value",
                vec![("ec2:InstanceType".to_string(), "t3.micro".to_string())],
                vec![(
                    "ec2:InstanceType".to_string(),
                    vec!["t3.micro".to_string()],
                )],
            ),
            (
                "one key passed under three values, and another key between them",
                vec![
                    ("aws:TagKeys".to_string(), "tenant".to_string()),
                    ("ec2:Tenancy".to_string(), "default".to_string()),
                    ("aws:TagKeys".to_string(), "tier".to_string()),
                    ("aws:TagKeys".to_string(), "owner".to_string()),
                ],
                vec![
                    (
                        "aws:TagKeys".to_string(),
                        vec![
                            "tenant".to_string(),
                            "tier".to_string(),
                            "owner".to_string(),
                        ],
                    ),
                    ("ec2:Tenancy".to_string(), vec!["default".to_string()]),
                ],
            ),
        ];
        for (case, pairs, expected) in rows {
            let folded = folded_conditions(&pairs);
            assert_eq!(
                folded, expected,
                "case {case:?}: expected {expected:?}, got {folded:?}"
            );
        }
    }
}
