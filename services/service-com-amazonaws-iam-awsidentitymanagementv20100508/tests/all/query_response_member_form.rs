// SPDX-License-Identifier: CC0-1.0

//! The FORM an IAM query/XML response carries a modelled member in, whichever operation's
//! projection writes that member out.
//!
//! RULE under test: a modelled response member is carried in the form the model and IAM's own
//! documentation give it — PRESENT where it reports a set the entity or the result holds
//! (empty when it holds nothing), and in the documented transfer encoding where it carries a
//! policy document — and that form is a property of the MEMBER, not of the operation the
//! member happens to be read through. The three properties this suite asserts, each on the
//! operations whose projection writes the member:
//!
//! * a member whose target is `policyDocumentType` carries the document percent-encoded: "the
//!   policy document returned in this structure is URL-encoded compliant with RFC 3986. You
//!   can use a URL decoding method to convert the policy back to plain JSON text"
//!   (`smithy_query(query="doc",shape="PolicyVersion",member="Document")`), and
//!   `Role$AssumeRolePolicyDocument` targets that same type — so the roles
//!   `InstanceProfile$Roles` embeds carry it encoded in the instance-profile LISTINGS exactly
//!   as a role read on its own does;
//! * a member that reports a SET is present whatever that set is:
//!   `GetOpenIDConnectProviderResponse$Tags` is "a list of tags that are attached to the
//!   specified IAM OIDC provider" and `EvaluationResult$MissingContextValues` "a list of
//!   context keys that are required by the included input policies but that were not provided
//!   by one of the input parameters", which "in the top-level result … contains the
//!   deduplicated set of missing context values across all requested resources". Absent is
//!   not one of their answers, since a caller could not tell "the set is empty" from "the set
//!   was not reported";
//! * a matched statement is LOCATED in the document it was read from — `Statement$StartPosition`
//!   is "the row and column of the beginning of the Statement in an IAM policy" and
//!   `$EndPosition` "the row and column of the end of a Statement in an IAM policy", each a
//!   `Position` of `Line` and `Column` — on the action's own result and on each
//!   `ResourceSpecificResults` entry alike; while `EvaluationResult$EvalDecisionDetails` is
//!   "present but … empty" where "the simulation evaluates policies within the same account
//!   and includes a resource ARN" and "is not returned" where it "specifies all resources
//!   (*)".
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: which
//! characters and which principal form the trust policy carries, which reader of the embedded
//! role answers, how many tags the provider holds and what form its URL takes, how many
//! actions one simulation decides, whether the request names resource ARNs, and where in its
//! document the matched statement sits. No row states a value read out of a response: the
//! statement spans are COMPUTED from the document the row submits, so a row's expectation
//! follows from the rule rather than from one recorded answer.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does, so what is under test is the HTTP
//! behaviour of state the write operations PERSISTED, never a handler called directly. The
//! awsQuery harness is `tests/all/user_lifecycle.rs`'s; the encoded-document check and the
//! fixture writes are `tests/all/instance_profile_listing_document_encoding.rs`'s, the
//! provider fixture `tests/all/open_id_connect_provider_tag_updates.rs`'s, and the position
//! reader `tests/all/simulated_statement_positions.rs`'s — so nothing here is a second copy
//! of a reader this crate's tests already own.

use crate::custom_policy_simulation::simulate;
use crate::instance_profile_listing_document_encoding::{
    assert_reported_document, bind_profile, create_role,
};
use crate::open_id_connect_provider_tag_updates::create_provider;
use crate::open_id_connect_providers::{issuer_of, list_members};
use crate::policy_simulation::{reported, ALLOWED};
use crate::role_lifecycle::{borrowed, param, reported_tags};
use crate::role_tags::{expected_map, tags};
use crate::simulated_statement_positions::{positions, reports_no_missing_key};
use crate::user_lifecycle::{call, element, router};

// ---------------------------------------------------------------------------
// Readers of a document: the role a response embeds
// ---------------------------------------------------------------------------

/// One trust policy a fixture role is created with.
struct TrustPolicyCase {
    case: &'static str,
    /// The document `CreateRole` submits.
    ///
    /// Each is minified, states no one-element string-or-array element and names no account
    /// by its id alone, so what the account STORES for it is this same text
    /// (`crate::state::stored_trust_policy`) and every reader must report exactly it.
    document: &'static str,
}

/// The rows differ along what the rule generalises over: which characters the document
/// carries — the punctuation JSON is made of, the `+` and the `%` that a decode of UNencoded
/// text silently corrupts, the slashes and colons of an ARN, a space inside a string literal
/// — and which principal form it names.
const TRUST_POLICIES: &[TrustPolicyCase] = &[
    TrustPolicyCase {
        case: "the service principal an instance-profile role carries",
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#,
    },
    TrustPolicyCase {
        case: "a condition value carrying a '+'",
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole","Condition":{"StringEquals":{"sts:ExternalId":"team+ops"}}}]}"#,
    },
    TrustPolicyCase {
        case: "a condition value carrying a '%' and a space",
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole","Condition":{"StringEquals":{"sts:ExternalId":"100% audited"}}}]}"#,
    },
    TrustPolicyCase {
        case: "another account's root user as the trusted principal",
        document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::210987654321:root"},"Action":"sts:AssumeRole"}]}"#,
    },
];

/// RULE: every reader that writes out a role — the role read on its own, the instance profile
/// that embeds it, and both paginated instance-profile listings — reports that role's
/// `AssumeRolePolicyDocument` as the stored document percent-encoded, so URL-decoding it
/// returns the text the account holds whatever characters that text carries.
#[tokio::test]
async fn every_reader_of_an_embedded_role_reports_its_document_encoded() {
    let router = router();
    for (index, row) in TRUST_POLICIES.iter().enumerate() {
        let role = format!("member-form-role-{index}");
        let profile = format!("member-form-profile-{index}");
        // One path per row, so a listing filtered by it reports exactly this row's profile
        // and the document asserted below is unambiguously that profile's own.
        let path = format!("/member-form-{index}/");
        create_role(&router, row.case, &role, row.document).await;
        bind_profile(&router, &profile, &path, &role).await;

        // The four readers that write out this role's trust policy. The role-level reader is
        // among them because the rule is one rule: a caller comparing what a listing reported
        // with what a read reported must see the same text.
        let readers: [(&str, Vec<(String, String)>); 4] = [
            ("GetRole", vec![param("RoleName", &role)]),
            (
                "GetInstanceProfile",
                vec![param("InstanceProfileName", &profile)],
            ),
            ("ListInstanceProfiles", vec![param("PathPrefix", &path)]),
            (
                "ListInstanceProfilesForRole",
                vec![param("RoleName", &role)],
            ),
        ];
        for (reader, params) in readers {
            let case = format!("{}/{reader}", row.case);
            let (status, body) = call(&router, reader, &borrowed(&params)).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status}: {body}"
            );
            // Exactly one document: the fixture holds one role, bound into one profile under
            // this row's own path.
            let reported = element(&body, "AssumeRolePolicyDocument");
            assert_reported_document(&case, row.document, &reported);
        }
    }
}

// ---------------------------------------------------------------------------
// Readers of a set: the tags an OIDC provider carries
// ---------------------------------------------------------------------------

/// One provider the fixture registers.
struct ProviderCase {
    case: &'static str,
    /// The URL `CreateOpenIDConnectProvider` is called with, carrying the scheme the model
    /// requires: "the URL must begin with https://" (`$Url`). AWS's own examples span the
    /// forms the rows below take — "typically the URL consists of only a hostname, like
    /// https://server.example.com or https://example.com:8443", with path components
    /// allowed.
    url: &'static str,
    /// The tags it is registered with.
    tags: &'static [(&'static str, &'static str)],
}

/// The rows differ along what the rule generalises over: how many tags the provider holds
/// (none, one, several) and what form its URL takes (a host, a host and a path, a host with a
/// port and a path).
const PROVIDERS: &[ProviderCase] = &[
    ProviderCase {
        case: "a host, holding no tags",
        url: "https://oidc-member-form-host.example.com",
        tags: &[],
    },
    ProviderCase {
        case: "a host and a path, holding one tag",
        url: "https://oidc-member-form-path.example.com/tenants/emulator",
        tags: &[("team", "payments")],
    },
    ProviderCase {
        case: "a host with a port and a path, holding several tags",
        url: "https://oidc-member-form-port.example.com:8443/issuer",
        tags: &[("env", "test"), ("owner", "ops")],
    },
];

/// RULE: `GetOpenIDConnectProvider` reports the provider's `Tags` member whatever the
/// provider holds in it — the tags attached to it, and the EMPTY list for one attached to
/// none — beside the URL it was registered with, reported as the issuer form that URL
/// identifies the provider by.
#[tokio::test]
async fn get_open_id_connect_provider_reports_its_tag_set_and_registered_url() {
    let router = router();
    for row in PROVIDERS {
        let held = tags(row.tags);
        let arn = create_provider(&router, row.url, &held).await;
        let (status, body) = call(
            &router,
            "GetOpenIDConnectProvider",
            &[("OpenIDConnectProviderArn", arn.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status}: {body}",
            row.case
        );

        // The member is the report of the provider's tag set, so the read carries exactly one
        // of it however many tags the provider holds.
        let carried = member_count(&body, "Tags");
        assert_eq!(
            carried, 1,
            "case {:?}: the read must carry exactly one <Tags> member, got {carried}: {body}",
            row.case
        );
        let actual = reported_tags(&body);
        let expected = expected_map(&held);
        assert_eq!(
            actual, expected,
            "case {:?}: expected the held tags {expected:?}, got {actual:?}: {body}",
            row.case
        );
        if row.tags.is_empty() {
            // The empty set has a report: the member with nothing in it, which is how the
            // query protocol writes a list holding nothing.
            let empty = empty_member_count(&body, "Tags");
            assert_eq!(
                empty, 1,
                "case {:?}: a provider holding no tags must be reported with an empty <Tags> \
                 member, got {empty}: {body}",
                row.case
            );
        }

        // The URL is the caller's own value, reported as the issuer the provider is held
        // under, so a client matching an ID token's `iss` claim compares like for like.
        let reported_url = element(&body, "Url");
        assert_eq!(
            reported_url,
            issuer_of(row.url),
            "case {:?}: the read must report the registered URL, got {reported_url:?}: {body}",
            row.case
        );
        // The other two list members the fixture registered one entry in each of, so a read
        // that reported a set member emptily everywhere could not pass.
        for (member, held) in [("ClientIDList", 1), ("ThumbprintList", 1)] {
            let entries = list_members(&body, member);
            assert_eq!(
                entries.len(),
                held,
                "case {:?}: expected {held} <{member}> entry, got {entries:?}: {body}",
                row.case
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Members of a simulated decision
// ---------------------------------------------------------------------------

/// A document stating ONE statement that allows every object action the rows ask about.
const OBJECT_ACTIONS: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:*","Resource":"*"}]}"#;

/// That statement, as [`OBJECT_ACTIONS`] writes it.
const OBJECT_ACTIONS_STATEMENT: &str = r#"{"Effect":"Allow","Action":"s3:*","Resource":"*"}"#;

/// The same document written with a space after each `:` and `,`, which moves the statement's
/// columns without changing which statement decides.
const SPACED: &str = r#"{"Version": "2012-10-17", "Statement": [{"Effect": "Allow", "Action": "s3:*", "Resource": "*"}]}"#;

/// That statement, as [`SPACED`] writes it.
const SPACED_STATEMENT: &str = r#"{"Effect": "Allow", "Action": "s3:*", "Resource": "*"}"#;

/// The same statement on the document's SECOND line, which moves its row.
const ON_THE_SECOND_LINE: &str = "{\n\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\"Action\":\"s3:*\",\"Resource\":\"*\"}]\n}";

/// Two objects of one bucket, named by ARNs that identify no owning account — so with no
/// `ResourceOwner` supplied the simulation is the SAME-account one
/// (`crate::state::simulated_decisions::cross_account`).
const FIRST_OBJECT: &str = "arn:aws:s3:::reports/q1.csv";
const SECOND_OBJECT: &str = "arn:aws:s3:::reports/q2.csv";

/// One simulation row.
struct SimulationCase {
    case: &'static str,
    /// The document the request supplies in `PolicyInputList`.
    document: &'static str,
    /// The statement of that document every matched entry must be located at. The span is
    /// computed from where this text sits in the document, never stated as a number.
    statement: &'static str,
    actions: &'static [&'static str],
    /// The resource ARNs the request names, if any.
    resources: &'static [&'static str],
}

/// The rows differ along what the rule generalises over: how many actions one page decides,
/// whether the request names resource ARNs (and how many), and where in its document the
/// deciding statement sits — minified on one line, spaced on one line, and on a second line.
const SIMULATIONS: &[SimulationCase] = &[
    SimulationCase {
        case: "three actions, no resource named",
        document: OBJECT_ACTIONS,
        statement: OBJECT_ACTIONS_STATEMENT,
        actions: &["s3:GetObject", "s3:PutObject", "s3:DeleteObject"],
        resources: &[],
    },
    SimulationCase {
        case: "two actions, asked about two same-account resources",
        document: OBJECT_ACTIONS,
        statement: OBJECT_ACTIONS_STATEMENT,
        actions: &["s3:GetObject", "s3:PutObject"],
        resources: &[FIRST_OBJECT, SECOND_OBJECT],
    },
    SimulationCase {
        case: "a spaced document, asked about one same-account resource",
        document: SPACED,
        statement: SPACED_STATEMENT,
        actions: &["s3:GetObject"],
        resources: &[FIRST_OBJECT],
    },
    SimulationCase {
        case: "a statement on the document's second line, no resource named",
        document: ON_THE_SECOND_LINE,
        statement: OBJECT_ACTIONS_STATEMENT,
        actions: &["s3:GetObject"],
        resources: &[],
    },
];

/// RULE: every result a simulation answers with reports the members its projection owns — the
/// matched statements LOCATED by a `StartPosition` and an `EndPosition` in the submitted
/// document, on the action's own result and on each per-resource result alike;
/// `MissingContextValues` present, empty where the decision lacked no value; and
/// `EvalDecisionDetails` present-and-empty exactly where the same-account simulation names
/// resource ARNs.
#[tokio::test]
async fn every_simulated_result_reports_the_members_of_its_decision() {
    let router = router();
    for row in SIMULATIONS {
        let (status, body) = simulate(
            &router,
            &[row.document],
            row.actions,
            row.resources,
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status}: {body}",
            row.case
        );

        // One result per action asked about, each allowed by the one statement the document
        // states — so the entries asserted below are that statement's.
        let decisions = reported(&body);
        let expected: Vec<(String, String)> = row
            .actions
            .iter()
            .map(|action| ((*action).to_string(), ALLOWED.to_string()))
            .collect();
        assert_eq!(
            decisions, expected,
            "case {:?}: expected the decisions {expected:?}, got {decisions:?}: {body}",
            row.case
        );

        let span = span_of(row.case, row.document, row.statement);
        // What each result reports ITSELF, read apart from what it reports per resource.
        let own = without_elements(&body, "ResourceSpecificResults");
        assert_located(row.case, "the action's own result", &own, span);

        // The member is the report of the keys the decision needed and did not get: present
        // on every result, and empty here because every row supplies the whole context its
        // document tests (it tests none).
        let missing = member_count(&own, "MissingContextValues");
        assert_eq!(
            missing,
            row.actions.len(),
            "case {:?}: each of the {} results must report <MissingContextValues>, got \
             {missing}: {body}",
            row.case,
            row.actions.len()
        );
        assert!(
            reports_no_missing_key(&own),
            "case {:?}: no condition key lacked a value, so every result must report the \
             member with nothing in it: {body}",
            row.case
        );

        let details = member_count(&own, "EvalDecisionDetails");
        let empty_details = empty_member_count(&own, "EvalDecisionDetails");
        assert_eq!(
            details, empty_details,
            "case {:?}: a same-account simulation explains no per-type decision, so every \
             <EvalDecisionDetails> it reports must be empty, got {empty_details} empty of \
             {details}: {body}",
            row.case
        );
        if row.resources.is_empty() {
            // "If the simulation evaluates policies within the same account and specifies all
            // resources (*), then the parameter is not returned."
            assert_eq!(
                details, 0,
                "case {:?}: a same-account simulation naming no resource must not report \
                 <EvalDecisionDetails>, got {details}: {body}",
                row.case
            );
        } else {
            // "If the simulation evaluates policies within the same account and includes a
            // resource ARN, then the parameter is present but the response is empty."
            assert_eq!(
                empty_details,
                row.actions.len(),
                "case {:?}: each of the {} results of a same-account simulation naming \
                 resources must report an empty <EvalDecisionDetails>, got {empty_details} \
                 empty of {details}: {body}",
                row.case,
                row.actions.len()
            );
        }

        // Each result reports the resources the request named, with the same statement
        // located inside its own per-resource entries.
        let per_resource = inner_elements(&body, "ResourceSpecificResults");
        assert_eq!(
            per_resource.len(),
            if row.resources.is_empty() {
                0
            } else {
                row.actions.len()
            },
            "case {:?}: expected one <ResourceSpecificResults> per result of a simulation \
             naming resources and none otherwise, got {}: {body}",
            row.case,
            per_resource.len()
        );
        for part in &per_resource {
            let names = crate::user_lifecycle::elements(part, "EvalResourceName");
            assert_eq!(
                names.len(),
                row.resources.len(),
                "case {:?}: each result must name every resource the request asked about, \
                 got {names:?}: {body}",
                row.case
            );
            assert_located(row.case, "a per-resource result", part, span);
        }
    }
}

/// Assert that every matched statement `part` reports is located at `span`: both `Position`
/// members present, and each the row and column the submitted document puts the statement at.
fn assert_located(case: &str, part_name: &str, part: &str, span: ((i32, i32), (i32, i32))) {
    let (start, end) = span;
    let starts = positions(part, "StartPosition");
    let ends = positions(part, "EndPosition");
    assert!(
        !starts.is_empty(),
        "case {case:?}: {part_name} must locate the statement that determined it, got \
         {part}"
    );
    assert_eq!(
        ends.len(),
        starts.len(),
        "case {case:?}: {part_name} must report one <EndPosition> per <StartPosition>, got \
         {ends:?} for {starts:?}"
    );
    for reported in &starts {
        assert_eq!(
            *reported, start,
            "case {case:?}: {part_name} must report the statement beginning at {start:?}, \
             got {reported:?}"
        );
    }
    for reported in &ends {
        assert_eq!(
            *reported, end,
            "case {case:?}: {part_name} must report the statement ending at {end:?}, got \
             {reported:?}"
        );
    }
}

/// Where `statement` sits in `document`, as the `(Line, Column)` its span begins and ends at:
/// its own first character, and the one past its last.
///
/// Computed from the text rather than stated per row, so a row varies the document's layout
/// without restating a number — and the rows all state ONE statement, which is the statement
/// a document's spans begin with ("the first statement of a document begins at its own first
/// character").
fn span_of(case: &str, document: &str, statement: &str) -> ((i32, i32), (i32, i32)) {
    let at = document
        .find(statement)
        .unwrap_or_else(|| panic!("case {case:?}: {statement:?} is not in {document:?}"));
    (
        position_at(document, at),
        position_at(document, at + statement.len()),
    )
}

/// The `(Line, Column)` of the byte offset `at` in `document`: the row counted from one at the
/// document's first line, the column counted from one in CHARACTERS at each line's first.
fn position_at(document: &str, at: usize) -> (i32, i32) {
    let before = &document[..at];
    let line = 1 + before.matches('\n').count();
    let column = 1 + match before.rfind('\n') {
        Some(break_at) => before[break_at + 1..].chars().count(),
        None => before.chars().count(),
    };
    (
        i32::try_from(line).expect("the fixture documents are short"),
        i32::try_from(column).expect("the fixture documents are short"),
    )
}

/// How many `<tag>` members `body` carries, counted on the open tag plus the self-closed
/// spelling an EMPTY one may take.
fn member_count(body: &str, tag: &str) -> usize {
    body.matches(&format!("<{tag}>")).count() + body.matches(&format!("<{tag}/>")).count()
}

/// How many `<tag>` members `body` carries with NOTHING in them, which is how the query
/// protocol writes a list or a map holding nothing: an element with nothing between its tags,
/// or a self-closed one.
fn empty_member_count(body: &str, tag: &str) -> usize {
    body.matches(&format!("<{tag}></{tag}>")).count()
        + body.matches(&format!("<{tag}/>")).count()
}

/// The text inside every `<tag>` element of `body`, in document order.
///
/// A reader for the STRUCTURED member this suite looks inside — the shared leaf reader refuses
/// an element carrying other elements — so what a result reports per RESOURCE is asserted
/// apart from what it reports itself. `tests/all/simulated_resource_policy.rs`'s `parts` splits
/// a report at its FIRST `ResourceSpecificResults`, which is what a one-result report needs,
/// and `tests/all/simulated_decision_attribution.rs`'s `projected_results` splits a report
/// into one slice per result; what every row here asserts is one fact about the WHOLE page —
/// every position it reports is the span of the one statement the document states — so the
/// page is read as the two halves that fact is about rather than result by result.
fn inner_elements(body: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut found = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find(&open) {
        let text = &rest[start + open.len()..];
        let end = text
            .find(&close)
            .unwrap_or_else(|| panic!("<{tag}> is never closed in {body}"));
        found.push(text[..end].to_string());
        rest = &text[end + close.len()..];
    }
    found
}

/// `body` with every `<tag>` element removed, so what is left is what the results report
/// OUTSIDE it.
fn without_elements(body: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut kept = String::new();
    let mut rest = body;
    while let Some(start) = rest.find(&open) {
        kept.push_str(&rest[..start]);
        let text = &rest[start + open.len()..];
        let end = text
            .find(&close)
            .unwrap_or_else(|| panic!("<{tag}> is never closed in {body}"));
        rest = &text[end + close.len()..];
    }
    kept.push_str(rest);
    kept
}
