// SPDX-License-Identifier: CC0-1.0

//! The instance-profile listings over the WHOLE character range a policy document may carry:
//! whatever characters the trust policy of an embedded role is made of, both listings report
//! it percent-encoded, and a caller that decodes gets the stored text back byte for byte.
//!
//! IAM holds a policy document as the text it was given and reports it URL-encoded: "the
//! policy document returned in this structure is URL-encoded compliant with RFC 3986. You can
//! use a URL decoding method to convert the policy back to plain JSON text"
//! (`smithy_query(query="doc",shape="PolicyVersion",member="Document")`), and
//! `Role$AssumeRolePolicyDocument` targets that same `policyDocumentType`
//! (`smithy_query(query="shape",shape="Role")`), so the rule is about the MEMBER wherever a
//! `Role` is written out — the roles `InstanceProfile$Roles` embeds included.
//!
//! `tests/all/policy_document_encoding.rs` is the suite for that rule across the readers of
//! one document, and `tests/all/instance_profile_listing_document_encoding.rs` the one for the
//! per-profile half of it in the two paginated listings, each over a table of hand-written
//! documents. What this suite adds is the RANGE the member is defined over: `policyDocumentType`
//! is `@pattern ^[\u0009\u000A\u000D\u0020-\u00FF]+$`
//! (`smithy_query(query="shape",shape="policyDocumentType")`), so this sweeps every character
//! of `\u0020-\u00FF` — both published bounds included — through the trust policy of a role an
//! instance profile holds, and asserts the listings report each of them as RFC 3986 writes it.
//! The tab, the line feed and the carriage return the pattern also admits cannot stand inside a
//! JSON string unescaped, so those three are exercised as the whitespace BETWEEN a document's
//! tokens by the sibling suite named above; a document here carries them only as the two-character
//! JSON escapes a caller writes them with.
//!
//! The escape every character is expected in is written out here — each byte of its UTF-8 form
//! as `%XX`, upper case — rather than taken from the service's encoder, and the DECODE every
//! assertion round-trips through is the runtime's URI decoder, so a character encoded wrongly
//! (a non-ASCII one written as one byte, a literal `%` left as itself) cannot still pass.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the HTTP
//! behaviour of documents that were PERSISTED by the write operations, never a handler called
//! directly. The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns.

use std::collections::BTreeMap;

use smithy_server_runtime::MultiServiceRouter;

// The RFC-3986 unreserved set and the shape check over an encoded document are the sibling
// suite's, so this crate's tests define them once.
use crate::policy_document_encoding::{assert_percent_encoded, is_unreserved};
use crate::role_lifecycle::{borrowed, param};
use crate::user_lifecycle::{call, element, elements, router};

/// The first character of the range `policyDocumentType` admits beside the three control
/// characters, and the last: `@pattern ^[\u0009\u000A\u000D\u0020-\u00FF]+$`.
const FIRST_ADMITTED: u32 = 0x20;
const LAST_ADMITTED: u32 = 0xFF;

/// How many of those characters one fixture document carries, so a report that dropped or
/// mangled a character names the group it was in rather than one document of 224 characters.
const CHARACTERS_PER_DOCUMENT: usize = 16;

/// The path the fixture's profiles live under, so a listing can be filtered to exactly them.
const FIXTURE_PATH: &str = "/alphabet/";

/// Every character of the admitted range, in order, both bounds included.
fn admitted_range() -> Vec<char> {
    (FIRST_ADMITTED..=LAST_ADMITTED)
        .map(|code| char::from_u32(code).expect("every code point of this range is a scalar value"))
        .collect()
}

/// The admitted range in the groups one fixture document carries each.
fn character_groups() -> Vec<String> {
    admitted_range()
        .chunks(CHARACTERS_PER_DOCUMENT)
        .map(|group| group.iter().collect())
        .collect()
}

/// The trust policy of an instance profile's role — the EC2 service principal, which is the
/// principal such a role carries — with `characters` in a condition value.
///
/// A condition value is the slot of a trust policy whose CONTENT is the caller's: the document
/// is a well-formed IAM trust policy whatever characters it carries there, so the fixture varies
/// the characters without varying what the policy means. It is built through the JSON
/// serializer, so the two characters JSON itself reserves inside a string (`"` and `\`) are
/// escaped the way a caller's SDK escapes them rather than by hand.
fn document_with(characters: &str) -> String {
    ::serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "ec2.amazonaws.com"},
            "Action": "sts:AssumeRole",
            "Condition": {"StringEquals": {"sts:ExternalId": characters}}
        }]
    })
    .to_string()
}

/// The condition value of `document`, so a report can be checked for the characters that were
/// submitted rather than only for being decodable.
fn condition_value(case: &str, document: &str) -> String {
    let parsed: ::serde_json::Value = ::serde_json::from_str(document)
        .unwrap_or_else(|error| panic!("case {case:?}: expected plain JSON text, got {document:?}: {error}"));
    // "The Statement element can contain a single statement or an array of individual
    // statements" (IAM's policy grammar), so a reader of one takes either spelling.
    let statement = match &parsed["Statement"] {
        ::serde_json::Value::Array(statements) => statements
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected a statement, got {document:?}"))
            .clone(),
        other => other.clone(),
    };
    statement["Condition"]["StringEquals"]["sts:ExternalId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the condition value, got {document:?}"))
        .to_string()
}

/// How RFC 3986 writes one character that is not unreserved: the `%XX` of each byte of its
/// UTF-8 form, upper case — written here rather than taken from the service's encoder, which is
/// what makes a character encoded some other way a failure instead of a match.
fn expected_escape(character: char) -> String {
    let mut buffer = [0u8; 4];
    character
        .encode_utf8(&mut buffer)
        .bytes()
        .map(|byte| format!("%{byte:02X}"))
        .collect()
}

/// Assert that every character of `group` reaches the caller the way RFC 3986 writes it: as
/// itself when it is unreserved, and otherwise as the escape of its UTF-8 bytes.
fn assert_each_character_is_escaped(case: &str, group: &str, reported: &str) {
    for character in group.chars() {
        if is_unreserved(character) {
            assert!(
                reported.contains(character),
                "case {case:?}: expected the unreserved character {character:?} to be reported as \
                 itself, got {reported:?}"
            );
            continue;
        }
        let escape = expected_escape(character);
        assert!(
            reported.contains(&escape),
            "case {case:?}: expected {character:?} to be reported as {escape}, got {reported:?}"
        );
    }
}

/// Assert that every run of two or more unreserved characters of `stored` appears in `reported`
/// as itself: the transform escapes what RFC 3986 reserves and nothing else, so a report that
/// escaped the whole document byte by byte — which decodes correctly all the same — is not the
/// encoding AWS answers with.
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

/// The role holding the trust policy of group `index`.
fn role_name(index: usize) -> String {
    format!("alphabet-role-{index}")
}

/// The instance profile group `index`'s role is bound into, under [`FIXTURE_PATH`].
fn profile_name(index: usize) -> String {
    format!("alphabet-profile-{index}")
}

/// One fixture profile: which characters its role's trust policy carries, and the document the
/// account HOLDS for it — read back through the single-resource read, decoded, so every later
/// assertion compares the listings with what the service itself says it stored.
struct Held {
    case: String,
    group: String,
    document: String,
}

/// The fixture: one role per group of [`character_groups`], each bound into its own instance
/// profile under [`FIXTURE_PATH`], keyed by profile name.
async fn with_fixture_profiles() -> (MultiServiceRouter, BTreeMap<String, Held>) {
    let router = router();
    let mut held = BTreeMap::new();
    for (index, group) in character_groups().into_iter().enumerate() {
        let first = group.chars().next().expect("a non-empty group") as u32;
        let last = group.chars().next_back().expect("a non-empty group") as u32;
        let case = format!("the characters U+{first:04X} through U+{last:04X} in a condition value");
        let role = role_name(index);
        let submitted = document_with(&group);

        let (status, body) = call(
            &router,
            "CreateRole",
            &[("RoleName", &role), ("AssumeRolePolicyDocument", &submitted)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture CreateRole must succeed, got {status}: {body}"
        );

        let profile = profile_name(index);
        let (status, body) = call(
            &router,
            "CreateInstanceProfile",
            &[("InstanceProfileName", &profile), ("Path", FIXTURE_PATH)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture CreateInstanceProfile must succeed, got {status}: {body}"
        );
        let (status, body) = call(
            &router,
            "AddRoleToInstanceProfile",
            &[("InstanceProfileName", &profile), ("RoleName", &role)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture AddRoleToInstanceProfile must succeed, got {status}: {body}"
        );

        // What the account holds, as the single-resource read reports it: that read carries the
        // document in the same one form, so the listings are compared against a document the
        // service itself wrote out, and the characters that were submitted are asserted to have
        // survived the store.
        let (status, body) = call(&router, "GetRole", &[("RoleName", &role)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture GetRole must succeed, got {status}: {body}"
        );
        let reported = element(&body, "AssumeRolePolicyDocument");
        assert_percent_encoded(&case, &reported);
        let document = ::smithy_server_runtime::percent_decode(&reported);
        let value = condition_value(&case, &document);
        assert_eq!(
            value, group,
            "case {case:?}: every submitted character must survive the store, got {value:?}"
        );

        held.insert(
            profile,
            Held {
                case,
                group,
                document,
            },
        );
    }
    (router, held)
}

/// One walk of a listing, page by page: the `(InstanceProfileName, AssumeRolePolicyDocument)`
/// pair of every profile reported, in the order the pages reported them.
///
/// Each fixture profile holds exactly one role, so a page carries one document per profile and
/// the two member lists pair up position by position — which the walk asserts before it pairs
/// them.
async fn walk(
    router: &MultiServiceRouter,
    listing: &str,
    params: &[(String, String)],
    max_items: Option<usize>,
) -> Vec<(String, String)> {
    let case = format!("{listing} with MaxItems={max_items:?} and {params:?}");
    // The walk ends when a page says it is not truncated; this is the runaway bound: a page
    // carries at least one profile, and the fixture holds one profile per group.
    let max_pages = character_groups().len() + 1;
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
            "case {case:?}: every profile of the page holds one role, so the page must carry one \
             document per profile, got {documents:?} for {names:?}"
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

/// Assert that every pair a walk reported carries that PROFILE's own held document, encoded,
/// with each character of its group written as RFC 3986 writes it.
fn assert_each_profile_reports_its_own_document(
    case: &str,
    held: &BTreeMap<String, Held>,
    reported: &[(String, String)],
) {
    for (profile, document) in reported {
        let expected = held
            .get(profile)
            .unwrap_or_else(|| panic!("case {case:?}: the walk reported an unknown profile {profile:?}"));
        let case = format!("{case} / {profile} / {}", expected.case);
        assert_reported_document(&case, &expected.document, document);
        assert_each_character_is_escaped(&case, &expected.group, document);
    }
}

/// RULE: every page of `ListInstanceProfiles` reports, for each profile on it, the trust policy
/// of the role that profile holds percent-encoded — for every character the member admits, at
/// every page size, and under a path prefix that selects part of the collection.
#[tokio::test]
async fn list_instance_profiles_reports_every_admitted_character_percent_encoded() {
    let (router, held) = with_fixture_profiles().await;
    let groups = character_groups().len();

    // Rows: the page size the walk uses — one profile per page (so every page after the first is
    // reached through a `Marker`), a page that holds the filtered collection exactly, one past
    // it, the published maximum of `maxItemsType`, and no `MaxItems` at all.
    for page_size in [Some(1), Some(groups), Some(groups + 1), Some(1000), None] {
        let reported = walk(
            &router,
            "ListInstanceProfiles",
            &[param("PathPrefix", FIXTURE_PATH)],
            page_size,
        )
        .await;
        let case = format!("ListInstanceProfiles under {FIXTURE_PATH} with MaxItems={page_size:?}");
        assert_eq!(
            reported.len(),
            groups,
            "case {case:?}: expected the walk to report every profile under the prefix, got \
             {reported:?}"
        );
        assert_each_profile_reports_its_own_document(&case, &held, &reported);
    }
}

/// RULE: the admitted range is a constraint on the member, so a document carrying a character
/// past its published bound is refused rather than stored — nothing outside the swept range
/// ever reaches a listing to be encoded.
#[tokio::test]
async fn a_document_carrying_a_character_past_the_admitted_range_is_refused() {
    let router = router();
    let past = char::from_u32(LAST_ADMITTED + 1).expect("one past the last admitted character");
    let role = "alphabet-role-past";

    let (status, body) = call(
        &router,
        "CreateRole",
        &[
            ("RoleName", role),
            ("AssumeRolePolicyDocument", &document_with(&past.to_string())),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "expected the create of a role whose trust policy carries {past:?} to be refused, got \
         {status}: {body}"
    );

    let (status, body) = call(&router, "GetRole", &[("RoleName", role)]).await;
    assert_eq!(
        status, 404,
        "expected no role to have been stored for a refused create, got {status}: {body}"
    );
}

/// RULE: `ListInstanceProfilesForRole` reports the same document, encoded, for every character
/// the member admits.
#[tokio::test]
async fn list_instance_profiles_for_role_reports_every_admitted_character_percent_encoded() {
    let (router, held) = with_fixture_profiles().await;

    for index in 0..character_groups().len() {
        let role = role_name(index);
        for page_size in [Some(1), None] {
            let reported = walk(
                &router,
                "ListInstanceProfilesForRole",
                &[param("RoleName", &role)],
                page_size,
            )
            .await;
            let case = format!("ListInstanceProfilesForRole {role} with MaxItems={page_size:?}");
            assert_eq!(
                reported.len(),
                1,
                "case {case:?}: expected the one profile the role is bound into, got {reported:?}"
            );
            assert!(
                reported
                    .iter()
                    .all(|(profile, _)| profile == &profile_name(index)),
                "case {case:?}: expected {:?}, got {reported:?}",
                profile_name(index)
            );
            assert_each_profile_reports_its_own_document(&case, &held, &reported);
        }
    }
}
