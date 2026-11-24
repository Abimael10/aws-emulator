// SPDX-License-Identifier: CC0-1.0

//! What a read reports for an entity whose DESCRIPTION is the empty string, end to end over
//! the awsQuery wire.
//!
//! RULE: the members that carry a description are published with a length whose floor is zero
//! (`Role$Description` targets `roleDescriptionType` and `Policy$Description`
//! `policyDescriptionType`, both `@length(min 0)`), so the empty string is a legal VALUE of
//! the member rather than a way of supplying none: a request that supplies it describes the
//! entity by that empty text, and every reader of the entity reports the member PRESENT and
//! empty. Only a request carrying no member at all leaves the entity undescribed, and only
//! then does a read omit the member.
//!
//! Why the wire form matters: in the awsQuery/XML protocol an absent member and one
//! serialized as `<Description></Description>` decode differently, so a client that CLEARS a
//! description — writing the empty string is what clears one — and reads it back gets the
//! empty text it wrote rather than a missing member its SDK unwraps.
//!
//! The suites are TABLES whose rows differ along what the rule generalises over: which
//! operation wrote the description (the two creates and the two role updates), what the
//! request carried for the member (no member, the empty string, blank characters, text),
//! which reader reports it back, and which entity kind carries it (role, managed policy).
//!
//! The awsQuery harness is `tests/all/user_lifecycle.rs`'s and `optional` (a member asserted
//! absent, or present and empty) is `tests/all/role_lifecycle.rs`'s.

use smithy_server_runtime::MultiServiceRouter;

use crate::role_lifecycle::optional;
use crate::user_lifecycle::{call, element, router};

/// The trust policy every role this suite creates carries.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"lambda.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The document every managed policy this suite creates carries.
const POLICY_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#;

/// The text a role is described by before a row clears it, so a cleared description is a
/// REPLACEMENT of text rather than the state the record was created in.
const INITIAL: &str = "the description the create carried";

/// One description a request may supply: what it carries for the member (`None` being a
/// request carrying no member at all) and what every read must then report.
struct Supplied {
    case: &'static str,
    supplied: Option<&'static str>,
    reported: Option<&'static str>,
}

/// Rows spanning what a request supplies for the member, from no member at all to text.
const SUPPLIED: &[Supplied] = &[
    Supplied {
        case: "no Description member at all",
        supplied: None,
        reported: None,
    },
    Supplied {
        case: "the empty string, which clears a description",
        supplied: Some(""),
        reported: Some(""),
    },
    Supplied {
        case: "a single blank character, which is text",
        supplied: Some(" "),
        reported: Some(" "),
    },
    Supplied {
        case: "a single character",
        supplied: Some("x"),
        reported: Some("x"),
    },
    Supplied {
        case: "a sentence",
        supplied: Some("Grants access to the production tables."),
        reported: Some("Grants access to the production tables."),
    },
];

/// One call that must be served, answering its body.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must succeed, got {status}: {body}"
    );
    body
}

/// Assert that `body` reports the description `expected` — a present member carrying exactly
/// that text, including the empty one, and no member at all for `None`.
fn assert_reports(case: &str, reader: &str, body: &str, expected: Option<&str>) {
    let reported = optional(body, "Description");
    assert_eq!(
        reported.as_deref(),
        expected,
        "case {case:?}: expected {reader} to report the description {expected:?}, got \
         {reported:?}: {body}"
    );
    // The wire form, not only the decoded value: an empty description is an element with
    // nothing between its tags, which is what tells a client the entity HAS one.
    if expected == Some("") {
        assert!(
            body.contains("<Description></Description>"),
            "case {case:?}: expected {reader} to serialize an empty <Description> element, \
             got {body}"
        );
    }
}

/// RULE through the two operations that REPLACE a role's description: the replacement is
/// reported back exactly as the request wrote it — the empty string included — by the write's
/// own response and by `GetRole`.
#[tokio::test]
async fn a_replacement_is_reported_back_including_the_empty_string() {
    // Both writers of a role's description. `UpdateRoleDescription` answers with the modified
    // role, while `UpdateRoleResponse` models no members, so the reader is what each row
    // asserts on.
    for update in ["UpdateRoleDescription", "UpdateRole"] {
        let router = router();
        let role_name = format!("cleared-{update}");
        served(
            &router,
            update,
            "CreateRole",
            &[
                ("RoleName", role_name.as_str()),
                ("AssumeRolePolicyDocument", TRUST_POLICY),
                ("Description", INITIAL),
            ],
        )
        .await;

        // Each row replaces the SAME role's description, so the rule is asserted over a
        // sequence of replacements. The absent-member row belongs to the create: a
        // replacement always carries the member.
        for row in SUPPLIED.iter().filter(|row| row.supplied.is_some()) {
            let supplied = row.supplied.expect("filtered above");
            let case = format!("{update} / {}", row.case);
            let written = served(
                &router,
                &case,
                update,
                &[("RoleName", role_name.as_str()), ("Description", supplied)],
            )
            .await;
            if update == "UpdateRoleDescription" {
                assert_reports(&case, update, &written, row.reported);
            }
            let read = served(
                &router,
                &case,
                "GetRole",
                &[("RoleName", role_name.as_str())],
            )
            .await;
            assert_reports(&case, "GetRole", &read, row.reported);
        }
    }
}

/// RULE at the CREATE of each entity kind that carries a description: the create's own
/// response and the entity's own reader report what the request supplied, and omit the member
/// only where the request carried none.
#[tokio::test]
async fn a_create_reports_the_description_it_was_given() {
    for row in SUPPLIED {
        // One account per row, holding exactly one role and one policy, so a member a reader
        // reports is that entity's and no other's.
        let router = router();

        let mut role_params = vec![
            ("RoleName", "described"),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ];
        if let Some(supplied) = row.supplied {
            role_params.push(("Description", supplied));
        }
        let created = served(&router, row.case, "CreateRole", &role_params).await;
        assert_reports(row.case, "CreateRole", &created, row.reported);
        let read = served(&router, row.case, "GetRole", &[("RoleName", "described")]).await;
        assert_reports(row.case, "GetRole", &read, row.reported);

        let mut policy_params = vec![
            ("PolicyName", "described"),
            ("PolicyDocument", POLICY_DOCUMENT),
        ];
        if let Some(supplied) = row.supplied {
            policy_params.push(("Description", supplied));
        }
        let created = served(&router, row.case, "CreatePolicy", &policy_params).await;
        assert_reports(row.case, "CreatePolicy", &created, row.reported);
        let arn = element(&created, "Arn");
        let read = served(
            &router,
            row.case,
            "GetPolicy",
            &[("PolicyArn", arn.as_str())],
        )
        .await;
        assert_reports(row.case, "GetPolicy", &read, row.reported);
    }
}
