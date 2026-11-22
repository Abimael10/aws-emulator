// SPDX-License-Identifier: CC0-1.0

//! The ROUND TRIP of the trust policy an instance-profile listing embeds: what the two
//! paginated listings report for `InstanceProfiles.member.Roles.member.AssumeRolePolicyDocument`
//! is a form a caller decodes, hands straight back to a writer of that member, and reads again
//! unchanged.
//!
//! IAM holds a policy document as the text it was given and reports it URL-encoded: "the policy
//! document returned in this structure is URL-encoded compliant with RFC 3986. You can use a URL
//! decoding method to convert the policy back to plain JSON text"
//! (`smithy_query(query="doc",shape="PolicyVersion",member="Document")`), and
//! `Role$AssumeRolePolicyDocument` targets that same `policyDocumentType`
//! (`smithy_query(query="shape",shape="Role")`), so the rule holds for the roles
//! `InstanceProfile$Roles` embeds as much as for a role read on its own.
//!
//! `tests/all/instance_profile_listing_document_encoding.rs` pins that the listings report the
//! encoded form of what the account holds, over the characters, the sizes, the page sizes and
//! the path prefixes the rule generalises over, and
//! `tests/all/instance_profile_listing_document_alphabet.rs` pins it over every character the
//! member admits. What this suite adds is the INVARIANT the encoding exists for, which neither
//! of them drives: the wire form is a FIXED POINT of the documented contract. Decode what a
//! listing reported, write the result back through `UpdateAssumeRolePolicy` or into a new role
//! through `CreateRole`, read the listing again, and the member carries the identical bytes — so
//! a caller may diff two reads, or copy a trust policy from one role onto another, through the
//! wire form alone. A report of raw JSON cannot satisfy it: decoding raw text turns every `+`
//! into a space and reads every `%XX` the document itself carries as an escape, so the text
//! written back is not the text that was read.
//!
//! The documents driven here are the corpus the first of those suites owns
//! ([`crate::instance_profile_listing_document_encoding::trust_policies`]) — the compact trust
//! policy of an instance profile's role, the whitespace an SDK or a human writes between the
//! JSON tokens, the `+` and the `%` a decode of UNencoded text corrupts, a principal ARN made
//! of reserved characters, and a byte above ASCII — so the rows the rule generalises over are
//! written down once and the two suites cannot drift apart. The fixture writes, the paginated
//! walk and the encoded-shape assertion are that suite's too; what this file adds is its own
//! profiles (under paths of its own, so a walk selects exactly them) and the round trip.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the HTTP
//! behaviour of documents the write operations PERSISTED. The decode is the runtime's URI
//! decoder, never this crate's encoder.

use std::collections::BTreeMap;

use smithy_server_runtime::MultiServiceRouter;

use crate::instance_profile_listing_document_encoding::{
    assert_reported_document, bind_profile, create_role, trust_policies, walk,
    CANONICAL_TRUST_POLICY_ENCODED,
};
use crate::role_lifecycle::param;
use crate::user_lifecycle::{call, router};

/// The path this suite's profiles live under, so a listing selects exactly them.
const FIXTURE_PATH: &str = "/round-trip/";

/// The path the profiles of the roles CREATED from a decoded report live under.
const CLONE_PATH: &str = "/round-trip-clones/";

/// The role holding the trust policy of row `index`.
fn role_name(index: usize) -> String {
    format!("round-trip-role-{index}")
}

/// The instance profile row `index`'s role is bound into, under [`FIXTURE_PATH`].
fn profile_name(index: usize) -> String {
    format!("round-trip-profile-{index}")
}

/// The profile the role created from row `index`'s reported document is bound into, under
/// [`CLONE_PATH`].
fn clone_profile_name(index: usize) -> String {
    format!("round-trip-clone-profile-{index}")
}

/// The fixture: one role per row of [`trust_policies`], each bound into its own profile under
/// [`FIXTURE_PATH`].
async fn with_fixture_profiles() -> MultiServiceRouter {
    let router = router();
    for (index, row) in trust_policies().iter().enumerate() {
        let role = role_name(index);
        create_role(&router, row.case, &role, row.submitted).await;
        bind_profile(&router, &profile_name(index), FIXTURE_PATH, &role).await;
    }
    router
}

/// What one walk of the profiles under `path` reports, by profile name — paged one profile at a
/// time, so every page after the first is reached through a `Marker`.
async fn reported_under(router: &MultiServiceRouter, path: &str) -> BTreeMap<String, String> {
    walk(
        router,
        "ListInstanceProfiles",
        &[param("PathPrefix", path)],
        Some(1),
    )
    .await
    .into_iter()
    .collect()
}

/// The document a walk reported for `profile`.
fn reported_for(case: &str, reported: &BTreeMap<String, String>, profile: &str) -> String {
    reported.get(profile).cloned().unwrap_or_else(|| {
        panic!("case {case:?}: the walk must report {profile:?}, got {reported:?}")
    })
}

/// RULE: a listing's report of an embedded role's trust policy decodes to the document the
/// account holds, which a writer of that member accepts back, and a read after that write
/// reports the identical wire form — so the encoded value is a fixed point of decode, write,
/// read.
#[tokio::test]
async fn a_decoded_listing_report_written_back_is_reported_unchanged() {
    let router = with_fixture_profiles().await;
    let rows = trust_policies();
    let before = reported_under(&router, FIXTURE_PATH).await;
    assert_eq!(
        before.len(),
        rows.len(),
        "expected the walk to report every fixture profile, got {before:?}"
    );

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let report = reported_for(case, &before, &profile_name(index));
        // The report is the encoded form of the document the account holds, which is what the
        // contract tells a caller it may decode.
        assert_reported_document(case, row.stored, &report);
        let decoded = ::smithy_server_runtime::percent_decode(&report);
        // Every row's document is made of characters RFC 3986 reserves, so the wire form is
        // NOT the text it carries: a report of raw JSON would decode to itself.
        assert_ne!(
            decoded, report,
            "case {case:?}: the reported member must be the encoded form of the document, not \
             its text, got {report:?}"
        );

        // The decoded text, handed straight back to the writer of that member.
        let role = role_name(index);
        let (status, body) = call(
            &router,
            "UpdateAssumeRolePolicy",
            &[("RoleName", &role), ("PolicyDocument", &decoded)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: writing back the decoded report must succeed, got {status}: {body}"
        );
    }

    // Read again: every profile reports exactly what it reported before the write-back, through
    // both listings, so nothing was lost or doubly encoded on the way.
    let after = reported_under(&router, FIXTURE_PATH).await;
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let expected = reported_for(case, &before, &profile_name(index));
        let actual = reported_for(case, &after, &profile_name(index));
        assert_eq!(
            actual, expected,
            "case {case:?}: ListInstanceProfiles must report the same wire form after the \
             write-back, expected {expected:?}, got {actual:?}"
        );

        let role = role_name(index);
        let by_role = walk(
            &router,
            "ListInstanceProfilesForRole",
            &[param("RoleName", &role)],
            Some(1),
        )
        .await;
        assert_eq!(
            by_role.len(),
            1,
            "case {case:?}: the role is bound into one profile, got {by_role:?}"
        );
        for (profile, actual) in &by_role {
            assert_eq!(
                actual, &expected,
                "case {case:?}: ListInstanceProfilesForRole must report the same wire form for \
                 {profile:?}, expected {expected:?}, got {actual:?}"
            );
        }
    }
}

/// RULE: the same fixed point holds through the other writer of the member, so a trust policy
/// copied from a listing's report onto a NEW role is reported in the identical wire form.
#[tokio::test]
async fn a_decoded_listing_report_creates_a_role_reported_in_the_same_wire_form() {
    let router = with_fixture_profiles().await;
    let rows = trust_policies();
    let source = reported_under(&router, FIXTURE_PATH).await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let report = reported_for(case, &source, &profile_name(index));
        let decoded = ::smithy_server_runtime::percent_decode(&report);
        let clone_role = format!("round-trip-clone-role-{index}");
        create_role(&router, case, &clone_role, &decoded).await;
        bind_profile(&router, &clone_profile_name(index), CLONE_PATH, &clone_role).await;
    }

    let clones = reported_under(&router, CLONE_PATH).await;
    assert_eq!(
        clones.len(),
        rows.len(),
        "expected the walk to report every cloned profile, got {clones:?}"
    );
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let expected = reported_for(case, &source, &profile_name(index));
        let actual = reported_for(case, &clones, &clone_profile_name(index));
        assert_eq!(
            actual, expected,
            "case {case:?}: the role created from the decoded report must be reported in the \
             same wire form, expected {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: the wire form is the percent-encoded document itself, which this row pins against the
/// encoded text spelled out character by character rather than against whatever the service
/// answers — through the ROLE's listing, the path the sibling suite pins by profile path.
#[tokio::test]
async fn the_canonical_trust_policy_is_reported_percent_encoded_by_the_roles_listing() {
    let router = with_fixture_profiles().await;
    let case = trust_policies()[0].case;
    let by_role = walk(
        &router,
        "ListInstanceProfilesForRole",
        &[param("RoleName", &role_name(0))],
        None,
    )
    .await;
    assert_eq!(
        by_role.len(),
        1,
        "case {case:?}: the role is bound into one profile, got {by_role:?}"
    );
    for (profile, actual) in &by_role {
        assert_eq!(
            actual, CANONICAL_TRUST_POLICY_ENCODED,
            "case {case:?}: expected the percent-encoded compact trust policy for {profile:?}, \
             got {actual:?}"
        );
    }
}
