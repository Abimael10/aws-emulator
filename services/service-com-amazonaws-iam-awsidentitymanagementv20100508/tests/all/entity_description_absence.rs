// SPDX-License-Identifier: CC0-1.0

//! What a read reports for the DESCRIPTION an entity holds, end to end over the awsQuery wire.
//!
//! RULE: a description is free text an account writes about one of its own entities, and the
//! members carrying one are optional with a published length whose floor is zero
//! (`Policy$Description` targets `policyDescriptionType` and `Role$Description`
//! `roleDescriptionType`, both `@length(min 0)`) — a floor of zero makes the empty string a
//! legal VALUE of the member rather than a way of supplying none, so a request supplying the
//! empty string describes the entity by that empty text and every read of the entity reports
//! the member as empty. Only a request carrying no member at all leaves the entity
//! undescribed, and only then does a read omit the member.
//!
//! Why the wire form matters: in the awsQuery/XML protocol an absent member and one
//! serialized as `<Description></Description>` decode differently, so a client that reads a
//! description back after CLEARING it — writing the empty string is what clears one — sees
//! the empty text it wrote rather than a missing member to unwrap.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: the entity
//! kind (managed policy, role), what the request supplied for the member (no member, the
//! empty string, text), the reader it is read back through (the entity's own read and the
//! account-wide authorization report — a listing reports "a subset of the available
//! attributes" and carries no description at all), the operation that wrote the description,
//! and the texts that are descriptions — one character, a sentence, and blank characters.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns; `optional` (a member
//! asserted ABSENT) is `tests/all/role_lifecycle.rs`'s.

use smithy_server_runtime::MultiServiceRouter;

use crate::role_lifecycle::optional;
use crate::user_lifecycle::{call, element, router};

/// The document every policy this suite creates carries.
const POLICY_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#;

/// The trust policy every role this suite creates carries.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"lambda.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// One description a request may supply: the case, what the request carries for the member
/// (`None` being a request carrying no member at all), and what every read must then report.
struct Described {
    case: &'static str,
    supplied: Option<&'static str>,
    reported: Option<&'static str>,
}

/// Rows spanning what a request supplies for the member: no member at all, the empty string,
/// and the texts that are descriptions.
const DESCRIBED: &[Described] = &[
    Described {
        case: "no Description member at all",
        supplied: None,
        reported: None,
    },
    Described {
        case: "the empty description",
        supplied: Some(""),
        reported: Some(""),
    },
    Described {
        case: "a single character",
        supplied: Some("x"),
        reported: Some("x"),
    },
    Described {
        case: "a sentence",
        supplied: Some("Grants access to production objects."),
        reported: Some("Grants access to production objects."),
    },
    Described {
        case: "a description of blank characters, which is text",
        supplied: Some(" "),
        reported: Some(" "),
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

/// RULE for a managed policy: the create's own response and every reader of the record it
/// persisted report the description the request supplied, and omit the member only where the
/// request carried none.
#[tokio::test]
async fn a_policy_reports_the_description_the_request_supplied() {
    for row in DESCRIBED {
        // One account per row, holding exactly ONE policy, so a member a reader reports is
        // this policy's and no other entity's.
        let router = router();
        let mut params = vec![
            ("PolicyName", "described"),
            ("PolicyDocument", POLICY_DOCUMENT),
        ];
        if let Some(supplied) = row.supplied {
            params.push(("Description", supplied));
        }
        let created = served(&router, row.case, "CreatePolicy", &params).await;
        assert_eq!(
            optional(&created, "Description").as_deref(),
            row.reported,
            "case {:?}: expected CreatePolicy to report {:?}, got {created}",
            row.case,
            row.reported
        );
        let arn = element(&created, "Arn");

        // Every reader of the same record that reports the member at all: the policy's own
        // read, and the account-wide authorization report. A listing reports "a subset of the
        // available attributes" and leaves a policy's description out altogether
        // (`tests/all/policy_listing.rs`), so it is not one of them.
        for (reader, params) in [
            ("GetPolicy", vec![("PolicyArn", arn.as_str())]),
            (
                "GetAccountAuthorizationDetails",
                vec![("Filter.member.1", "LocalManagedPolicy")],
            ),
        ] {
            let body = served(&router, row.case, reader, &params).await;
            assert_eq!(
                optional(&body, "Description").as_deref(),
                row.reported,
                "case {:?}: expected {reader} to report {:?}, got {body}",
                row.case,
                row.reported
            );
        }
    }
}

/// RULE for a role: the create's own response and `GetRole` report the description the
/// request supplied, and omit the member only where the request carried none.
#[tokio::test]
async fn a_role_reports_the_description_the_request_supplied() {
    let router = router();

    for (index, row) in DESCRIBED.iter().enumerate() {
        let role_name = format!("described-role-{index}");
        let mut params = vec![
            ("RoleName", role_name.as_str()),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ];
        if let Some(supplied) = row.supplied {
            params.push(("Description", supplied));
        }
        let created = served(&router, row.case, "CreateRole", &params).await;
        assert_eq!(
            optional(&created, "Description").as_deref(),
            row.reported,
            "case {:?}: expected CreateRole to report {:?}, got {created}",
            row.case,
            row.reported
        );
        let read = served(
            &router,
            row.case,
            "GetRole",
            &[("RoleName", role_name.as_str())],
        )
        .await;
        assert_eq!(
            optional(&read, "Description").as_deref(),
            row.reported,
            "case {:?}: expected GetRole to report {:?}, got {read}",
            row.case,
            row.reported
        );
    }
}

/// RULE through the two operations that REPLACE a role's description: the replacement is
/// reported back exactly as it was written, the empty string included — whichever operation
/// wrote it, and however the role was described before.
#[tokio::test]
async fn a_replacement_is_reported_back_exactly_as_it_was_written() {
    let router = router();

    for update in ["UpdateRoleDescription", "UpdateRole"] {
        let role_name = format!("replaced-{update}");
        served(
            &router,
            update,
            "CreateRole",
            &[
                ("RoleName", role_name.as_str()),
                ("AssumeRolePolicyDocument", TRUST_POLICY),
                ("Description", "the text the create carried"),
            ],
        )
        .await;

        // Each row replaces the SAME role's description, so the rule is asserted over a
        // sequence of replacements rather than over one write. The absent-member row belongs
        // to the create: a replacement always carries the member.
        for row in DESCRIBED.iter().filter(|row| row.supplied.is_some()) {
            let supplied = row.supplied.expect("filtered above");
            let case = format!("{update} / {}", row.case);
            served(
                &router,
                &case,
                update,
                &[("RoleName", role_name.as_str()), ("Description", supplied)],
            )
            .await;
            let read = served(
                &router,
                &case,
                "GetRole",
                &[("RoleName", role_name.as_str())],
            )
            .await;
            assert_eq!(
                optional(&read, "Description").as_deref(),
                row.reported,
                "case {case:?}: expected GetRole to report {:?}, got {read}",
                row.reported
            );
        }
    }
}
