// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DOMAIN of `DescribeDBRecommendations`' `Filters[].Name`: the
//! character set the report's filter names are drawn from, and how a value outside it is
//! refused.
//!
//! The rules under test:
//!
//! * a `Filters` entry whose `Name` is outside the character set the names are drawn from
//!   (lowercase letters and the hyphen that separates their words) carries a value the
//!   MEMBER does not admit at all, so it is refused as the member constraint it broke: the
//!   service's standard constraint report — the count of findings, the offending value, the
//!   path of the member that carried it and the regular expression it had to satisfy —
//!   under `InvalidParameterValue` with HTTP 400;
//! * the path names the POSITION of the offending entry, so the entry a caller has to fix
//!   is identifiable in a list of several;
//! * that domain is checked BEFORE the report's own vocabulary, so a request carrying both
//!   kinds of wrong name is refused for the one the member does not admit;
//! * a name the member DOES admit but the report does not narrow by keeps the sentence
//!   Amazon RDS answers an unrecognized filter name with, and a name the report publishes is
//!   served.
//!
//! The rows span what the rule generalises over: each class of character outside the set
//! (digit, upper case, underscore, dot, space), one and several entries, and both control
//! legs.

use crate::db_instances::{call, emulator, expect_error, expect_ok, text};

/// The Query API code a value a member does not accept is refused under.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The expression the report's filter names are drawn from, as the refusal states it.
const FILTER_NAME_PATTERN: &str = "[a-z-]+";

/// The constraint report the service answers a value one of its member constraints does not
/// admit with: one finding, naming the value, the member path it sat at and the requirement.
fn constraint_report(value: &str, member_path: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{member_path}' failed to satisfy \
         constraint: Member must satisfy regular expression pattern: {FILTER_NAME_PATTERN}"
    )
}

/// One request, the filter names it carries in order, and the refusal it must answer with.
struct FilterNameCase {
    case: &'static str,
    /// The `Filters[].Name` values, in the order the request writes them.
    names: &'static [&'static str],
    /// The name the refusal is about and the 1-based position it was written at, or `None`
    /// for a request that is served.
    offending: Option<(&'static str, usize)>,
    /// Set where the refusal is the unrecognized-filter sentence rather than the constraint
    /// report: the name that sentence names.
    unrecognized: Option<&'static str>,
}

const FILTER_NAME_CASES: &[FilterNameCase] = &[
    FilterNameCase {
        case: "a name carrying a digit",
        names: &["status2"],
        offending: Some(("status2", 1)),
        unrecognized: None,
    },
    FilterNameCase {
        case: "a name carrying an upper-case letter",
        names: &["Status"],
        offending: Some(("Status", 1)),
        unrecognized: None,
    },
    FilterNameCase {
        case: "a name carrying an underscore",
        names: &["type_id"],
        offending: Some(("type_id", 1)),
        unrecognized: None,
    },
    FilterNameCase {
        case: "a name carrying a dot",
        names: &["pg.arn"],
        offending: Some(("pg.arn", 1)),
        unrecognized: None,
    },
    FilterNameCase {
        case: "a name carrying a space",
        names: &["type id"],
        offending: Some(("type id", 1)),
        unrecognized: None,
    },
    FilterNameCase {
        case: "the offending entry is the second of two",
        names: &["status", "severity-1"],
        offending: Some(("severity-1", 2)),
        unrecognized: None,
    },
    FilterNameCase {
        case: "the offending entry is the third of three",
        names: &["status", "severity", "TYPE-ID"],
        offending: Some(("TYPE-ID", 3)),
        unrecognized: None,
    },
    FilterNameCase {
        case: "a name the member does not admit beside one the report does not publish",
        names: &["engine-name", "status1"],
        offending: Some(("status1", 2)),
        unrecognized: None,
    },
    FilterNameCase {
        case: "a name the member admits that the report does not publish",
        names: &["engine-name"],
        offending: None,
        unrecognized: Some("engine-name"),
    },
    FilterNameCase {
        case: "a hyphen-only name the member admits",
        names: &["--"],
        offending: None,
        unrecognized: Some("--"),
    },
    FilterNameCase {
        case: "every name the report publishes",
        names: &["status", "severity", "type-id", "recommendation-id"],
        offending: None,
        unrecognized: None,
    },
];

#[tokio::test]
async fn a_filter_name_outside_the_members_character_set_is_a_constraint_refusal() {
    let router = emulator();
    for row in FILTER_NAME_CASES {
        let mut params: Vec<(String, String)> = Vec::new();
        for (index, name) in row.names.iter().enumerate() {
            let position = index + 1;
            params.push((
                format!("Filters.Filter.{position}.Name"),
                (*name).to_string(),
            ));
            // `Values` is required of every entry, so each row sends one match term: what is
            // under test is the NAME, and a request missing the values would be refused for
            // the member it left out instead.
            params.push((
                format!("Filters.Filter.{position}.Values.Value.1"),
                "emu-filter-name".to_string(),
            ));
        }
        let response = call(&router, "DescribeDBRecommendations", &params).await;
        match (row.offending, row.unrecognized) {
            (Some((value, position)), _) => {
                let body = expect_error(row.case, response, 400, INVALID_PARAMETER_VALUE);
                let message = text(&body, "Message");
                let expected =
                    constraint_report(value, &format!("filters.{position}.member.name"));
                assert_eq!(
                    message, expected,
                    "case {:?}: expected {expected:?}, got {message:?}",
                    row.case
                );
            }
            (None, Some(named)) => {
                let body = expect_error(row.case, response, 400, INVALID_PARAMETER_VALUE);
                let message = text(&body, "Message");
                let expected = format!("Unrecognized filter name: {named}");
                assert_eq!(
                    message, expected,
                    "case {:?}: expected {expected:?}, got {message:?}",
                    row.case
                );
            }
            (None, None) => {
                let body = expect_ok(row.case, response);
                assert!(
                    body.contains("DescribeDBRecommendationsResult"),
                    "case {:?}: expected the report, got {body}",
                    row.case
                );
            }
        }
    }
}
