// SPDX-License-Identifier: CC0-1.0

//! What the instance-profile LISTINGS carry in the `AssumeRolePolicyDocument` of every role
//! they embed: that role's own stored trust policy, percent-encoded, on every page and at
//! every page size.
//!
//! IAM holds a policy document as the text it was given and reports it URL-encoded: "the
//! policy document returned in this structure is URL-encoded compliant with RFC 3986. You
//! can use a URL decoding method to convert the policy back to plain JSON text"
//! (`smithy_query(query="doc",shape="PolicyVersion",member="Document")`), and
//! `Role$AssumeRolePolicyDocument` targets that same `policyDocumentType`
//! (`smithy_query(query="shape",shape="Role")`), so the rule is about the MEMBER wherever a
//! `Role` is written out — the roles `InstanceProfile$Roles` embeds included.
//!
//! `tests/all/policy_document_encoding.rs` is the suite for that rule across the readers of
//! one document. What this suite adds is the PER-MEMBER half of it in the two paginated
//! instance-profile listings: a page carrying several profiles whose roles hold DIFFERENT
//! documents must report each profile's own document, encoded — so neither a single encode
//! applied to one member nor a document read off the wrong role can pass — and it must do so
//! for whatever the account holds now, which is why a trust policy REPLACED after the
//! profiles were bound is asserted too.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the HTTP
//! behaviour of documents that were PERSISTED by the write operations, never a handler called
//! directly. The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns. The DECODE
//! every row checks is the runtime's URI decoder, not this crate's encoder, so a document
//! encoded wrongly cannot still match.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: which
//! characters the document carries (the punctuation JSON is made of, the spaces and tabs a
//! caller indents with, the `+` and `%` a decode of UNencoded text corrupts, the slashes of
//! an ARN, a byte above ASCII), the page size the collection is walked with, the path prefix
//! it is filtered by, and which of the two listings serves the page.

use std::collections::BTreeMap;

use smithy_server_runtime::MultiServiceRouter;

// The RFC-3986 unreserved set and the shape check over an encoded document are the sibling
// suite's, so this crate's tests define them once.
use crate::policy_document_encoding::{assert_percent_encoded, is_unreserved};
use crate::role_lifecycle::{borrowed, param};
use crate::user_lifecycle::{call, element, elements, router};

/// One trust policy the fixture stores on a role of its own.
struct TrustPolicyCase {
    case: &'static str,
    /// The text `CreateRole` carries.
    submitted: &'static str,
    /// The text the account HOLDS for it — the policy rendered, which is what every read
    /// reports (encoded): the whitespace between its JSON tokens gone, a one-element
    /// string-or-array element written as its string.
    stored: &'static str,
}

/// The compact trust policy of an instance profile's role: the EC2 service principal, which
/// is the one such a role carries.
const CANONICAL_TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// [`CANONICAL_TRUST_POLICY`] percent-encoded, written out here character by character rather
/// than computed, so this suite pins the TRANSFORM and not whatever the service happens to
/// do: every character outside RFC 3986's unreserved set (the ASCII letters and digits, `-`,
/// `.`, `_` and `~`) is the `%XX` of its byte — `{` is `%7B`, `"` is `%22`, `:` is `%3A`, `,`
/// is `%2C`, `[` is `%5B`, `]` is `%5D` — while `2012-10-17` and `ec2.amazonaws.com` carry
/// only unreserved characters and so stand as they are.
const CANONICAL_TRUST_POLICY_ENCODED: &str = "%7B%22Version%22%3A%222012-10-17%22%2C%22Statement%22%3A%5B%7B%22Effect%22%3A%22Allow%22%2C%22Principal%22%3A%7B%22Service%22%3A%22ec2.amazonaws.com%22%7D%2C%22Action%22%3A%22sts%3AAssumeRole%22%7D%5D%7D";

/// The path the fixture's profiles live under, so a listing can be filtered to exactly them.
const FIXTURE_PATH: &str = "/encoding/";

/// The path of the one further profile bound to the first fixture role, so a filtered listing
/// has something to leave out and the first role has more than one profile to report.
const OTHER_PATH: &str = "/elsewhere/";

/// The trust policies the fixture stores, one per role, each bound into its own instance
/// profile.
///
/// `policyDocumentType` is `@pattern ^[\u0009\u000A\u000D\u0020-\u00FF]+$`
/// (`smithy_query(query="shape",shape="policyDocumentType")`), so the tab, the line feed and
/// every byte up to U+00FF are part of what a caller may submit and therefore part of what a
/// report has to carry back.
fn trust_policies() -> Vec<TrustPolicyCase> {
    vec![
        TrustPolicyCase {
            case: "the compact trust policy of an instance profile's role",
            submitted: CANONICAL_TRUST_POLICY,
            stored: CANONICAL_TRUST_POLICY,
        },
        TrustPolicyCase {
            case: "the spaces an SDK's JSON serializer writes between the tokens",
            submitted: r#"{"Version": "2012-10-17", "Statement": [{"Effect": "Allow", "Principal": {"Service": "ec2.amazonaws.com"}, "Action": "sts:AssumeRole"}]}"#,
            stored: CANONICAL_TRUST_POLICY,
        },
        TrustPolicyCase {
            case: "a document indented with the tab and line feed the member admits",
            submitted: "{\n\t\"Version\": \"2012-10-17\",\n\t\"Statement\": [\n\t\t{\n\t\t\t\"Effect\": \"Allow\",\n\t\t\t\"Principal\": {\"Service\": \"ec2.amazonaws.com\"},\n\t\t\t\"Action\": \"sts:AssumeRole\"\n\t\t}\n\t]\n}",
            stored: CANONICAL_TRUST_POLICY,
        },
        TrustPolicyCase {
            case: "an external id carrying a literal plus, which a decode of raw text would \
                   turn into a space",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole","Condition":{"StringEquals":{"sts:ExternalId":"a+b/c=="}}}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole","Condition":{"StringEquals":{"sts:ExternalId":"a+b/c=="}}}]}"#,
        },
        TrustPolicyCase {
            case: "an external id carrying a literal percent, which a decode of raw text \
                   would read as an escape",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole","Condition":{"StringLike":{"sts:ExternalId":"100%7Bdone"}}}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole","Condition":{"StringLike":{"sts:ExternalId":"100%7Bdone"}}}]}"#,
        },
        TrustPolicyCase {
            case: "a role principal ARN, whose slashes and colons are not unreserved",
            submitted: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::210987654321:role/deploy~1_x.y-z"},"Action":"sts:AssumeRole"}]}"#,
            stored: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::210987654321:role/deploy~1_x.y-z"},"Action":"sts:AssumeRole"}]}"#,
        },
        TrustPolicyCase {
            case: "a statement id carrying a byte above ASCII",
            submitted: "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Sid\":\"caf\u{e9}\",\"Effect\":\"Allow\",\"Principal\":{\"Service\":\"ec2.amazonaws.com\"},\"Action\":\"sts:AssumeRole\"}]}",
            stored: "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Sid\":\"caf\u{e9}\",\"Effect\":\"Allow\",\"Principal\":{\"Service\":\"ec2.amazonaws.com\"},\"Action\":\"sts:AssumeRole\"}]}",
        },
    ]
}

/// The role holding the trust policy of row `index`.
fn role_name(index: usize) -> String {
    format!("encoding-role-{index}")
}

/// The instance profile row `index`'s role is bound into, under [`FIXTURE_PATH`].
fn profile_name(index: usize) -> String {
    format!("encoding-profile-{index}")
}

/// The further profile the first fixture role is bound into, under [`OTHER_PATH`].
const OTHER_PROFILE: &str = "encoding-profile-elsewhere";

/// Assert that every run of two or more unreserved characters of `stored` appears in
/// `reported` as itself: the transform escapes what RFC 3986 reserves and nothing else, so a
/// report that escaped the whole document byte by byte — which decodes correctly all the same
/// — is not the encoding AWS answers with.
fn assert_unreserved_runs_are_literal(case: &str, stored: &str, reported: &str) {
    for run in stored.split(|character| !is_unreserved(character)) {
        if run.chars().count() < 2 {
            continue;
        }
        assert!(
            reported.contains(run),
            "case {case:?}: expected the unreserved run {run:?} to be reported as itself, got \
             {reported:?}"
        );
    }
}

/// Assert that `reported` is `stored` as a response carries a policy document: the
/// percent-encoded form, which decodes back to `stored` byte for byte.
///
/// The decode is what IAM's own contract tells a caller to do, and it is the half that makes
/// this more than a shape check: a document carrying a literal `+` or `%` only survives it
/// when the member really was encoded.
fn assert_reported_document(case: &str, stored: &str, reported: &str) {
    assert_percent_encoded(case, reported);
    assert_unreserved_runs_are_literal(case, stored, reported);
    let decoded = ::smithy_server_runtime::percent_decode(reported);
    assert_eq!(
        decoded, stored,
        "case {case:?}: decoding the reported document must return the stored document, got \
         {decoded:?} from {reported:?}"
    );
}

/// Create a role whose trust policy is `submitted`, asserting the fixture create succeeded.
async fn create_role(router: &MultiServiceRouter, case: &str, name: &str, submitted: &str) {
    let (status, body) = call(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", submitted)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture CreateRole must succeed, got {status}: {body}"
    );
}

/// Create the instance profile `profile` under `path` and bind `role` into it, asserting both
/// fixture writes succeeded.
async fn bind_profile(router: &MultiServiceRouter, profile: &str, path: &str, role: &str) {
    let (status, body) = call(
        router,
        "CreateInstanceProfile",
        &[("InstanceProfileName", profile), ("Path", path)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {profile:?}: the fixture CreateInstanceProfile must succeed, got {status}: {body}"
    );
    let (status, body) = call(
        router,
        "AddRoleToInstanceProfile",
        &[("InstanceProfileName", profile), ("RoleName", role)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {profile:?}: the fixture AddRoleToInstanceProfile must succeed, got {status}: \
         {body}"
    );
}

/// The fixture: one role per row of [`trust_policies`], each bound into its own profile under
/// [`FIXTURE_PATH`], plus one further profile under [`OTHER_PATH`] holding the first role.
async fn with_fixture_profiles() -> MultiServiceRouter {
    let router = router();
    for (index, row) in trust_policies().iter().enumerate() {
        let role = role_name(index);
        create_role(&router, row.case, &role, row.submitted).await;
        bind_profile(&router, &profile_name(index), FIXTURE_PATH, &role).await;
    }
    bind_profile(&router, OTHER_PROFILE, OTHER_PATH, &role_name(0)).await;
    router
}

/// One walk of a listing, page by page: the `(InstanceProfileName, AssumeRolePolicyDocument)`
/// pair of every profile reported, in the order the pages reported them.
///
/// Each fixture profile holds exactly one role, so a page carries one document per profile
/// and the two member lists pair up position by position — which the walk asserts before it
/// pairs them.
async fn walk(
    router: &MultiServiceRouter,
    listing: &str,
    params: &[(String, String)],
    max_items: Option<usize>,
) -> Vec<(String, String)> {
    let case = format!("{listing} with MaxItems={max_items:?} and {params:?}");
    // The walk ends when a page says it is not truncated; this is the runaway bound, computed
    // once: a page carries at least one profile, and the fixture holds one profile per row
    // plus the one under the other path.
    let max_pages = trust_policies().len() + 2;
    let mut reported = Vec::new();
    let mut marker: Option<String> = None;
    let mut pages = 0;
    loop {
        let mut page_params = params.to_vec();
        if let Some(size) = max_items {
            page_params.push(param("MaxItems", &size.to_string()));
        }
        if let Some(token) = &marker {
            page_params.push(param("Marker", token));
        }
        let (status, body) = call(router, listing, &borrowed(&page_params)).await;
        assert_eq!(status, 200, "case {case:?}: expected 200, got {status}: {body}");

        let names = elements(&body, "InstanceProfileName");
        let documents = elements(&body, "AssumeRolePolicyDocument");
        assert_eq!(
            documents.len(),
            names.len(),
            "case {case:?}: every profile of the page holds one role, so the page must carry \
             one document per profile, got {documents:?} for {names:?}"
        );
        reported.extend(names.into_iter().zip(documents));

        pages += 1;
        assert!(
            pages <= max_pages,
            "case {case:?}: the pagination never ended, saw {reported:?}"
        );
        if element(&body, "IsTruncated") == "true" {
            marker = Some(element(&body, "Marker"));
        } else {
            return reported;
        }
    }
}

/// The stored trust policy of every fixture profile, by profile name.
fn stored_by_profile() -> BTreeMap<String, (&'static str, &'static str)> {
    let mut stored = BTreeMap::new();
    for (index, row) in trust_policies().iter().enumerate() {
        stored.insert(profile_name(index), (row.case, row.stored));
    }
    stored.insert(
        OTHER_PROFILE.to_string(),
        (
            "the further profile holding the first fixture role",
            trust_policies()[0].stored,
        ),
    );
    stored
}

/// Assert that every pair a walk reported carries that PROFILE's own stored document,
/// encoded.
fn assert_each_profile_reports_its_own_document(case: &str, reported: &[(String, String)]) {
    let stored = stored_by_profile();
    for (profile, document) in reported {
        let (row, expected) = stored
            .get(profile)
            .unwrap_or_else(|| panic!("case {case:?}: the walk reported an unknown profile {profile:?}"));
        assert_reported_document(&format!("{case} / {profile} / {row}"), expected, document);
    }
}

/// RULE: every page of `ListInstanceProfiles` reports, for each profile on it, the trust
/// policy of the role that profile holds, percent-encoded — whatever characters the document
/// carries, at every page size, and under a path prefix that selects part of the collection.
#[tokio::test]
async fn every_page_of_list_instance_profiles_reports_each_profiles_own_encoded_trust_policy() {
    let router = with_fixture_profiles().await;
    let rows = trust_policies();

    // Rows: the page size the walk uses — one profile per page (so every page after the
    // first is reached through a `Marker`), two, a page that holds the filtered collection
    // exactly, one past it, the published maximum, and no `MaxItems` at all — crossed with
    // the path prefix the listing is filtered by.
    let page_sizes: &[Option<usize>] = &[Some(1), Some(2), Some(rows.len()), Some(rows.len() + 1), Some(1000), None];
    for page_size in page_sizes {
        let filtered = walk(
            &router,
            "ListInstanceProfiles",
            &[param("PathPrefix", FIXTURE_PATH)],
            *page_size,
        )
        .await;
        let case = format!("ListInstanceProfiles under {FIXTURE_PATH} with MaxItems={page_size:?}");
        assert_eq!(
            filtered.len(),
            rows.len(),
            "case {case:?}: expected the walk to report every profile under the prefix, got \
             {filtered:?}"
        );
        assert!(
            !filtered.iter().any(|(profile, _)| profile == OTHER_PROFILE),
            "case {case:?}: the prefix selects no profile outside it, got {filtered:?}"
        );
        assert_each_profile_reports_its_own_document(&case, &filtered);

        let all = walk(&router, "ListInstanceProfiles", &[], *page_size).await;
        let case = format!("ListInstanceProfiles with MaxItems={page_size:?}");
        assert_eq!(
            all.len(),
            rows.len() + 1,
            "case {case:?}: expected the walk to report every profile the account holds, got \
             {all:?}"
        );
        assert_each_profile_reports_its_own_document(&case, &all);
    }

    // And the exact text, pinned: the transform is the requirement, so the row whose stored
    // document is the compact trust policy asserts the whole encoded string rather than only
    // its invariants.
    let all = walk(&router, "ListInstanceProfiles", &[], Some(1)).await;
    let reported = all
        .iter()
        .find(|(profile, _)| profile == &profile_name(0))
        .map(|(_, document)| document.clone())
        .unwrap_or_else(|| panic!("the walk must report {:?}, got {all:?}", profile_name(0)));
    assert_eq!(
        reported, CANONICAL_TRUST_POLICY_ENCODED,
        "expected the percent-encoded compact trust policy, got {reported:?}"
    );
}

/// RULE: `ListInstanceProfilesForRole` reports the same encoded document on every page — for
/// a role holding one profile and for one holding several.
#[tokio::test]
async fn every_page_of_list_instance_profiles_for_role_reports_the_roles_encoded_trust_policy() {
    let router = with_fixture_profiles().await;
    let rows = trust_policies();

    for (index, row) in rows.iter().enumerate() {
        // The first fixture role is bound into two profiles, every other one into its own.
        let expected_profiles = if index == 0 { 2 } else { 1 };
        for page_size in [Some(1), Some(1000), None] {
            let role = role_name(index);
            let reported = walk(
                &router,
                "ListInstanceProfilesForRole",
                &[param("RoleName", &role)],
                page_size,
            )
            .await;
            let case = format!("{} through ListInstanceProfilesForRole {role} with MaxItems={page_size:?}", row.case);
            assert_eq!(
                reported.len(),
                expected_profiles,
                "case {case:?}: expected {expected_profiles} profile(s) for the role, got \
                 {reported:?}"
            );
            for (profile, document) in &reported {
                assert_reported_document(&format!("{case} / {profile}"), row.stored, document);
            }
        }
    }
}

/// RULE: the listings report what the account holds NOW, so a trust policy replaced after the
/// profiles were bound is reported — encoded — in its new form by both of them.
#[tokio::test]
async fn a_replaced_trust_policy_is_reported_encoded_by_both_listings() {
    let router = with_fixture_profiles().await;
    let role = role_name(0);
    // The replacement carries both characters a decode of UNencoded text corrupts, so the
    // round trip only holds if the new document was encoded as well.
    const REPLACEMENT: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole","Condition":{"StringEquals":{"sts:ExternalId":"x+y 50%"}}}]}"#;

    let (status, body) = call(
        &router,
        "UpdateAssumeRolePolicy",
        &[("RoleName", &role), ("PolicyDocument", REPLACEMENT)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture UpdateAssumeRolePolicy must succeed, got {status}: {body}"
    );

    // Through the role's own listing, EVERY profile reported holds that role, so every
    // document on the walk is the replacement, encoded.
    let reported = walk(
        &router,
        "ListInstanceProfilesForRole",
        &[param("RoleName", &role)],
        Some(1),
    )
    .await;
    assert!(
        !reported.is_empty(),
        "the role's listing must report the profiles it is bound into, got {reported:?}"
    );
    for (profile, document) in &reported {
        assert_reported_document(
            &format!("the replaced trust policy through ListInstanceProfilesForRole / {profile}"),
            REPLACEMENT,
            document,
        );
    }

    // Through the account's listing, the profile holding that role reports the replacement
    // and every other profile still reports its own role's document — so the replacement
    // reached one role and not the projection as a whole.
    let reported = walk(
        &router,
        "ListInstanceProfiles",
        &[param("PathPrefix", FIXTURE_PATH)],
        Some(1),
    )
    .await;
    let stored = stored_by_profile();
    let mut saw_replacement = false;
    for (profile, document) in &reported {
        if profile == &profile_name(0) {
            saw_replacement = true;
            assert_reported_document(
                "the replaced trust policy through ListInstanceProfiles",
                REPLACEMENT,
                document,
            );
            continue;
        }
        let (row, expected) = stored
            .get(profile)
            .unwrap_or_else(|| panic!("the walk reported an unknown profile {profile:?}"));
        assert_reported_document(
            &format!("{row} through ListInstanceProfiles"),
            expected,
            document,
        );
    }
    assert!(
        saw_replacement,
        "the walk must report {:?}, got {reported:?}",
        profile_name(0)
    );
}
