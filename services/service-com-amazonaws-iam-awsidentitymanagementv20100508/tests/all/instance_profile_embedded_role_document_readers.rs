// SPDX-License-Identifier: CC0-1.0

//! EVERY reader of the embedded-role projection reports ONE encoded text.
//!
//! IAM holds a policy document as the text it was given and reports it URL-encoded: "the
//! policy document returned in this structure is URL-encoded compliant with RFC 3986. You can
//! use a URL decoding method to convert the policy back to plain JSON text"
//! (`smithy_query(query="doc",shape="PolicyVersion",member="Document")`), and
//! `Role$AssumeRolePolicyDocument` targets that same `policyDocumentType`
//! (`smithy_query(query="shape",shape="Role")`), so the transform is a property of the MEMBER
//! wherever a `Role` is written out — the roles `InstanceProfile$Roles` embeds included.
//!
//! `tests/all/instance_profile_listing_document_encoding.rs` is the suite for that rule across
//! the PAGES of the two paginated instance-profile listings, and
//! `tests/all/instance_profile_listing_document_alphabet.rs` the suite for the characters a
//! document carries. What this suite adds is the READER dimension: the single-profile read,
//! each listing walked a page at a time, and the account-wide configuration report, whose
//! `RoleDetail$InstanceProfileList` carries that same projection for every profile a role is
//! contained in (`smithy_query(query="shape",shape="RoleDetail")`) — a reader no suite asserted
//! the embedded document of. Each must report the stored document encoded, as many times as the
//! records it reports carry it, and in a text that is byte-identical across all of them: a
//! document cannot be reported in one form on one read and another form on the next, and no one
//! reader can be the one that skips the encode.
//!
//! The fixture, the pinned constants, the paginated walk, the per-document assertion and the
//! one-page read of the account report are the sibling suites', imported rather than copied, so
//! one definition of each serves both: every assertion therefore drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, over documents PERSISTED by the
//! write operations, and the DECODE each check makes is the runtime's URI decoder rather than
//! this crate's encoder.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;

use crate::instance_profile_listing_document_encoding::{
    assert_reported_document, bind_profile, create_role, page, walk, CANONICAL_TRUST_POLICY,
    CANONICAL_TRUST_POLICY_ENCODED,
};
use crate::role_lifecycle::param;
use crate::user_lifecycle::{call, elements, router};

/// The path this suite's profiles live under, so a listing selects exactly them.
const READERS_PATH: &str = "/readers/";

/// How many instance profiles the fixture role is embedded in: more than one, so a page size of
/// one makes each listing serve a follow-up page reached through a `Marker`, and the
/// account-wide report carries the projection more than once.
const READERS_PROFILES: usize = 2;

/// Every `AssumeRolePolicyDocument` the account-wide configuration report carries for the roles
/// it reports: each role's own member, and the one inside every instance profile that contains
/// it.
///
/// The one-page read of that report — filtered to the roles, asked for at the published maximum
/// page size and asserted untruncated — is the account-report suite's
/// (`crate::account_authorization_details::report`), so this adds only the extraction.
async fn reported_by_the_account_report(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let body = crate::account_authorization_details::report(
        router,
        case,
        &[state::ENTITY_FILTER_ROLE],
    )
    .await;
    elements(&body, "AssumeRolePolicyDocument")
}

/// A role holding `document`, embedded in [`READERS_PROFILES`] instance profiles under
/// [`READERS_PATH`], over a router of its own, and the names of those profiles.
async fn with_embedded_role(
    case: &str,
    role: &str,
    document: &str,
) -> (MultiServiceRouter, Vec<String>) {
    let router = router();
    create_role(&router, case, role, document).await;
    let profiles: Vec<String> = (0..READERS_PROFILES)
        .map(|index| format!("{role}-profile-{index}"))
        .collect();
    for profile in &profiles {
        bind_profile(&router, profile, READERS_PATH, role).await;
    }
    (router, profiles)
}

/// Every reader of that projection, each with the documents it reported and how many of them it
/// must report: the profile read on its own carries the one role it holds, each listing reports
/// every profile the role is embedded in, and the account report carries the role's own member
/// beside the projection inside each of those profiles.
async fn documents_by_reader(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    profile: &str,
) -> Vec<(&'static str, Vec<String>, usize)> {
    let single = page(
        router,
        "GetInstanceProfile",
        &[param("InstanceProfileName", profile)],
    )
    .await
    .1;
    let documents_of = |walked: Vec<(String, String)>| -> Vec<String> {
        walked.into_iter().map(|(_, document)| document).collect()
    };
    let listed = documents_of(
        walk(
            router,
            "ListInstanceProfiles",
            &[param("PathPrefix", READERS_PATH)],
            Some(1),
        )
        .await,
    );
    let for_role = documents_of(
        walk(
            router,
            "ListInstanceProfilesForRole",
            &[param("RoleName", role)],
            Some(1),
        )
        .await,
    );
    let account = reported_by_the_account_report(router, case).await;

    vec![
        ("GetInstanceProfile", single, 1),
        ("ListInstanceProfiles", listed, READERS_PROFILES),
        ("ListInstanceProfilesForRole", for_role, READERS_PROFILES),
        (
            "GetAccountAuthorizationDetails",
            account,
            READERS_PROFILES + 1,
        ),
    ]
}

/// RULE: every reader of the embedded-role projection reports the stored trust policy
/// percent-encoded, as many times as the records it reports carry it, in one and the same text.
#[tokio::test]
async fn every_reader_of_the_embedded_role_projection_reports_one_encoded_document() {
    // Rows: the characters the document carries — the compact punctuation JSON is made of, and
    // the `+`, space and `%` a decode of UNencoded text corrupts, which is what makes the decode
    // each row checks more than a shape check. Both are stored as written: they carry no
    // whitespace between their JSON tokens, no one-element string-or-array value and no account
    // named by its id alone, which are the three things a save rewrites.
    let rows: [(&str, &str); 2] = [
        ("the canonical trust policy", CANONICAL_TRUST_POLICY),
        (
            "an external id carrying a plus, a space and a percent",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole","Condition":{"StringEquals":{"sts:ExternalId":"a+b 100%7Bc"}}}]}"#,
        ),
    ];

    for (case, document) in rows {
        let (router, profiles) = with_embedded_role(case, "readers-role", document).await;

        let mut every_reported = Vec::new();
        for (reader, reported, expected_count) in
            documents_by_reader(&router, case, "readers-role", &profiles[0]).await
        {
            let case = format!("{case} through {reader}");
            assert_eq!(
                reported.len(),
                expected_count,
                "case {case:?}: expected {expected_count} reported document(s), got {reported:?}"
            );
            for value in &reported {
                assert_reported_document(&case, document, value);
            }
            every_reported.extend(reported);
        }

        // One encoded text, whichever reader served it: a reader applying a different transform
        // — or none — differs here even where its own decode still matched.
        let first = every_reported
            .first()
            .expect("every reader reported at least one document")
            .clone();
        for value in &every_reported {
            assert_eq!(
                *value, first,
                "case {case:?}: every reader must report the same encoded text, got {value:?} \
                 beside {first:?}"
            );
        }
    }
}

/// RULE: the exact transform, pinned at the reader that serves the projection embedded in an
/// account-wide report — so an encoding that merely round-trips (a different escape set,
/// lower-case escapes) cannot pass there either.
#[tokio::test]
async fn the_account_report_carries_the_embedded_roles_encoded_trust_policy() {
    let case = "the canonical trust policy";
    let (router, _) = with_embedded_role(case, "account-readers-role", CANONICAL_TRUST_POLICY).await;

    let reported = reported_by_the_account_report(&router, case).await;
    assert_eq!(
        reported.len(),
        READERS_PROFILES + 1,
        "case {case:?}: expected the role's own member beside the one in each profile containing \
         it, got {reported:?}"
    );
    for value in &reported {
        assert_eq!(
            value, CANONICAL_TRUST_POLICY_ENCODED,
            "case {case:?}: expected the percent-encoded trust policy, got {value:?}"
        );
    }
}

/// RULE: the report is of what the account holds NOW, so a trust policy written over the one a
/// role was created with is what every reader of that projection reports, encoded, and the
/// replaced text is reported by none of them.
#[tokio::test]
async fn a_trust_policy_written_over_the_created_one_is_reported_encoded_by_every_reader() {
    // The replacement differs from the created policy in its principal as well as in the
    // characters it carries, so a report left over from the first document cannot pass for it.
    const REPLACEMENT: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"lambda.amazonaws.com"},"Action":"sts:AssumeRole","Condition":{"StringEquals":{"sts:ExternalId":"x+y 50%"}}}]}"#;
    let role = "replaced-readers-role";

    let (router, profiles) =
        with_embedded_role("before the replacement", role, CANONICAL_TRUST_POLICY).await;
    let (status, body) = call(
        &router,
        "UpdateAssumeRolePolicy",
        &[("RoleName", role), ("PolicyDocument", REPLACEMENT)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture UpdateAssumeRolePolicy must succeed, got {status}: {body}"
    );

    for (reader, reported, expected_count) in
        documents_by_reader(&router, "after the replacement", role, &profiles[0]).await
    {
        let case = format!("after the replacement, through {reader}");
        assert_eq!(
            reported.len(),
            expected_count,
            "case {case:?}: expected {expected_count} reported document(s), got {reported:?}"
        );
        for value in &reported {
            assert_reported_document(&case, REPLACEMENT, value);
            assert_ne!(
                value, CANONICAL_TRUST_POLICY_ENCODED,
                "case {case:?}: the replaced document must not be reported, got {value:?}"
            );
        }
    }
}
