// SPDX-License-Identifier: CC0-1.0

//! A `ClusterIdentifier` outside the Redshift identifier GRAMMAR is a parameter
//! fault, decided before anything is looked up — end to end over the awsQuery
//! wire through the same router the production emulator builds.
//!
//! The rule asserted here: an operation that RESOLVES a cluster identifier judges
//! it against the grammar — begins with a letter, holds only ASCII letters,
//! digits and hyphens, neither ends with a hyphen nor carries two consecutive
//! ones — BEFORE any lookup, so a value outside the grammar is refused with HTTP
//! 400 `InvalidParameterValue` and that operation's fixed identifier sentence,
//! which names the offending member and echoes nothing of the value, while a
//! well-formed value that resolves to nothing is the operation's own HTTP 404
//! `ClusterNotFound`. Because the grammar is judged on the REQUEST, the refusal
//! cannot depend on what the account holds.
//!
//! That ORDER is what [`crate::describe_filter_validation`] pins for one store;
//! this suite is about its independence from the store and about the sentence
//! being fixed text: every row runs against two emulators — one holding no
//! cluster at all and one holding a cluster — and the same refusal must come
//! back from both. The rows differ along which part of the grammar the value
//! breaks and along the operation, whose validator reports the offending member
//! under its own name. The expected sentence is written out here rather than
//! taken from the crate under test, so wording that drifts fails this suite.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator};

/// The fixed sentence Amazon Redshift refuses a value outside the identifier
/// grammar with: entirely fixed text apart from the member the validator names,
/// with the offending value nowhere in it.
fn invalid_identifier_message(parameter: &str) -> String {
    format!(
        "The parameter {parameter} is not a valid identifier. Identifiers must begin with a \
         letter; must contain only ASCII letters, digits, and hyphens; and must not end with a \
         hyphen or contain two consecutive hyphens."
    )
}

/// A name of exactly `len` characters that satisfies every part of the
/// cluster-identifier grammar: a leading letter, then alphanumerics.
fn name_of_length(len: usize) -> String {
    std::iter::once('c')
        .chain(std::iter::repeat_n('x', len - 1))
        .collect()
}

/// Ask `action` about one cluster identifier.
async fn ask(emulator: &Emulator, action: &str, identifier: &str) -> Answer {
    emulator
        .call(action, &[("ClusterIdentifier", identifier)])
        .await
}

/// Fail unless `answer` is the refusal `status` / `code` / `message` names.
fn assert_refused(case: &str, answer: &Answer, status: u16, code: &str, message: &str) {
    assert_eq!(
        (answer.status, answer.error_code().as_deref()),
        (status, Some(code)),
        "case {case}: expected {status} / {code}, got {} / {:?} with body {}",
        answer.status,
        answer.error_code(),
        answer.body
    );
    assert_eq!(
        answer.text("Message").as_deref(),
        Some(message),
        "case {case}: expected the message {message:?}, got {:?} with body {}",
        answer.text("Message"),
        answer.body
    );
}

/// The cluster the seeded store holds, so one of the two emulators has something
/// a lookup could have resolved.
const STORED_CLUSTER: &str = "grammar-order-present";

#[tokio::test]
async fn an_identifier_outside_the_grammar_is_refused_the_same_way_whatever_the_store_holds() {
    // Two stores: the grammar is a property of the REQUEST, so a value outside it
    // is refused identically whether or not this account holds any cluster.
    let empty = Emulator::new();
    let seeded = Emulator::new();
    let created = seeded.create_minimal(STORED_CLUSTER).await;
    assert_eq!(
        created.status, 200,
        "case seed {STORED_CLUSTER}: expected 200, got {} with body {}",
        created.status, created.body
    );

    // Rows differ along WHICH part of the grammar the value breaks: the leading
    // letter, the alphabet, the trailing hyphen and the doubled hyphen — the four
    // constraints the sentence states — plus a value that is empty, which begins
    // with no letter at all.
    let malformed = [
        ("an underscore and capitals", "Grammar_Order_Probe"),
        ("an underscore alone", "has_underscore"),
        ("a leading digit", "7leading-digit"),
        ("a leading hyphen", "-leading-hyphen"),
        ("a trailing hyphen", "trailing-hyphen-"),
        ("two consecutive hyphens", "double--hyphen"),
        ("a dot", "dot.name"),
        ("a space", "with space"),
        ("a non-ASCII letter", "café-cluster"),
        ("the stored name in a spelling the grammar rejects", "grammar_order_present"),
        ("an empty value", ""),
    ];
    // The member each operation's validator names the offending value by:
    // `DescribeClusters` is judged by the validator shared with the rest of the
    // RDS family, the cluster-scoped read by the member's own name.
    let judged = [
        ("DescribeClusters", "DBInstanceIdentifier"),
        ("DescribeLoggingStatus", "ClusterIdentifier"),
    ];

    for (store_case, emulator) in [
        ("an empty store", &empty),
        ("a store holding a cluster", &seeded),
    ] {
        for (action, parameter) in judged {
            let expected = invalid_identifier_message(parameter);
            for (why, identifier) in malformed {
                let case = format!("{action} with {why} against {store_case}");
                let answer = ask(emulator, action, identifier).await;
                assert_refused(&case, &answer, 400, "InvalidParameterValue", &expected);
                // The sentence is fixed text: it names the member and carries
                // nothing of the value, so a caller matching it sees the same
                // sentence whatever it sent.
                let message = answer.text("Message").unwrap_or_default();
                assert!(
                    identifier.is_empty() || !message.contains(identifier),
                    "case {case}: expected the refusal to carry nothing of the value, got \
                     {message:?}"
                );
            }

            // A WELL-FORMED name that resolves to nothing takes the other branch:
            // the lookup's own fault rather than a parameter fault. The rows span
            // the spelling, the case and both bounds the grammar admits.
            for (why, identifier) in [
                ("a plain absent name", "no-such-cluster".to_string()),
                ("a one-letter absent name", name_of_length(1)),
                ("an absent name in mixed case", "No-Such-Cluster".to_string()),
                ("a hyphenated absent name", "a-b-c-d".to_string()),
                (
                    "an absent name of the greatest length",
                    // The documented maximum of a cluster identifier: "Must
                    // contain from 1 to 63 alphanumeric characters or hyphens"
                    // (`smithy_query(query="doc", shape="CreateClusterMessage",
                    // member="ClusterIdentifier")`), carried by
                    // `state::CLUSTER_IDENTIFIER_RULE`.
                    name_of_length(state::CLUSTER_IDENTIFIER_RULE.max_len),
                ),
            ] {
                let case = format!("{action} with {why} against {store_case}");
                let answer = ask(emulator, action, &identifier).await;
                assert_eq!(
                    (answer.status, answer.error_code().as_deref()),
                    (404, Some("ClusterNotFound")),
                    "case {case}: expected 404 / ClusterNotFound, got {} / {:?} with body {}",
                    answer.status,
                    answer.error_code(),
                    answer.body
                );
            }
        }
    }

    // The cluster that IS there is still served through both operations, so the
    // refusals above are the grammar and the lookup, not a blanket refusal.
    for action in ["DescribeClusters", "DescribeLoggingStatus"] {
        let served = ask(&seeded, action, STORED_CLUSTER).await;
        assert_eq!(
            served.status, 200,
            "case {action} with the stored cluster: expected 200, got {} with body {}",
            served.status, served.body
        );
    }
}
