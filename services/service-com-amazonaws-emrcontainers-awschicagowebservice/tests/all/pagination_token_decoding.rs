// SPDX-License-Identifier: CC0-1.0

//! How a `@paginated` listing of this service reads the `nextToken` a caller sends, end to
//! end.
//!
//! Every call goes through the production path: the service is mounted in the real
//! `MultiServiceRouter` by the harness [`crate::virtual_cluster_job_run_loop::Emr`] and
//! driven with restJson1 HTTP requests, so routing, deserialization, constraint
//! validation, the handler and serialization all run exactly as they do for an AWS SDK
//! client.
//!
//! The rule under test: a continuation token of this service is an opaque BASE64 value, so
//! it is DECODED before anything is resolved from it, and a value the decoder cannot read
//! is refused in the decoder's own words — `Illegal base64 character <hh>`, naming the
//! lower-case hex code of the first character of the supplied token that the base64
//! alphabet does not hold. A token that decodes but names no position this listing issued
//! is a different refusal and says so. Both are the modelled `ValidationException` at HTTP
//! 400 (smithy_query: operation `ListJobTemplates` — `nextToken` is a `NextToken`,
//! `@httpQuery`, len 1..1024, pattern `\S`; the operation models `InternalServerException`
//! and `ValidationException` only).
//!
//! The tables span what the rule generalises over: which listing is asked (all five
//! `@paginated` operations, the two scoped to a virtual cluster included), WHICH character
//! the alphabet does not hold (seven of them, so the hex code in the message is a variable
//! part and not a literal), WHERE in the token it sits (first, middle, last) and the token's
//! length (both published bounds of `NextToken`).
//!
//! The other half of "opaque base64" is driven too: the token a listing hands out is itself
//! a base64 value a client can send back unescaped, and it resumes the walk.

use serde_json::{json, Value};
use service_com_amazonaws_emrcontainers_awschicagowebservice::state;

use crate::managed_endpoint_listing::{create_endpoint, endpoint_body};
use crate::virtual_cluster_job_run_loop::{field, text, Answer, Emr, REGION};
use crate::virtual_cluster_scheduler_status::run_id;

/// The class every refusal below is carried by (smithy_query: digest — the only client
/// error a listing of this service models).
const INVALID_REQUEST: &str = "ValidationException";

/// The wording a token that DECODES but names no position this listing issued is refused
/// with (`state::PageRefusal::UnknownNextToken`), which the decoder's sentence must not be.
const UNKNOWN_TOKEN: &str = "nextToken is not a valid pagination token";

/// The sentence a base64 decoder refuses a character its alphabet does not hold with: fixed
/// text and the lower-case hex code of that character's byte, taken from the token the
/// request carried.
fn illegal_character(hex: &str) -> String {
    format!("Illegal base64 character {hex}")
}

/// The smallest `jobTemplateData` the model accepts: `executionRoleArn`, `releaseLabel` and
/// `jobDriver` are all `@required` (smithy_query: shape `JobTemplateData`).
fn minimal_template_data() -> Value {
    json!({
        "executionRoleArn": "arn:aws:iam::000000000000:role/EMRJobRole",
        "releaseLabel": "emr-6.15.0-latest",
        "jobDriver": {"sparkSubmitJobDriver": {"entryPoint": "s3://bucket/token.py"}},
    })
}

/// A job template this emulator holds, and its id.
async fn create_template(emr: &Emr, index: usize) -> String {
    let case = format!("a job template to page, {index}");
    let created = emr
        .send(
            "POST",
            "/jobtemplates",
            Some(json!({
                "name": format!("token-template-{index}"),
                "clientToken": format!("tok-token-template-{index}"),
                "jobTemplateData": minimal_template_data(),
            })),
        )
        .await;
    text(created.ok(&case), "/id", &case)
}

/// Every `@paginated` listing this service serves, as `(label, path)`: the three collections
/// of the account and the two scoped to a virtual cluster, which a cluster the fixture
/// created really holds.
///
/// Each listing holds two records, so a one-row page leaves a `nextToken` to resume from.
async fn every_listing(emr: &Emr) -> Vec<(&'static str, String)> {
    let cluster = emr.cluster("token-host", "tok-token-host").await;
    for index in 0..2 {
        create_template(emr, index).await;
        emr.cluster(
            &format!("token-cluster-{index}"),
            &format!("tok-token-cluster-{index}"),
        )
        .await;
        emr.send(
            "POST",
            "/securityconfigurations",
            Some(json!({
                "name": format!("token-config-{index}"),
                "clientToken": format!("tok-token-config-{index}"),
                "securityConfigurationData": {},
            })),
        )
        .await
        .ok("a security configuration to page");
        run_id(
            emr,
            "a job run to page",
            &cluster,
            &format!("tok-token-run-{index}"),
        )
        .await;
        create_endpoint(
            emr,
            &cluster,
            endpoint_body(
                &format!("token-endpoint-{index}"),
                &format!("tok-token-endpoint-{index}"),
                "JUPYTER_ENTERPRISE_GATEWAY",
            ),
        )
        .await
        .ok("a managed endpoint to page");
    }

    vec![
        ("ListVirtualClusters", "/virtualclusters".to_string()),
        ("ListJobTemplates", "/jobtemplates".to_string()),
        (
            "ListSecurityConfigurations",
            "/securityconfigurations".to_string(),
        ),
        (
            "ListJobRuns",
            format!("/virtualclusters/{cluster}/jobruns"),
        ),
        (
            "ListManagedEndpoints",
            format!("/virtualclusters/{cluster}/endpoints"),
        ),
    ]
}

/// One token the base64 alphabet does not admit, and the hex code the refusal must name.
struct Undecodable {
    case: String,
    /// The token as it goes into the query string.
    token: String,
    /// The lower-case hex code of the FIRST character the alphabet does not hold.
    hex: &'static str,
}

fn undecodable(case: &str, token: String, hex: &'static str) -> Undecodable {
    Undecodable {
        case: case.to_string(),
        token,
        hex,
    }
}

/// Tokens no base64 decoder can read, spanning WHICH character is illegal, WHERE it sits
/// and how long the token is.
///
/// The characters and their codes are ASCII: `-` 0x2d, `_` 0x5f (so a token in the URL-safe
/// alphabet is refused as well), `.` 0x2e, `:` 0x3a, `!` 0x21, `~` 0x7e and the space 0x20,
/// which has to be percent-escaped to survive the query string. The last two rows are at
/// both published bounds of `NextToken`'s `@length` (1..1024).
fn undecodable_tokens() -> Vec<Undecodable> {
    vec![
        undecodable(
            "a hyphen in the first position",
            "not-a-cursor".to_string(),
            "2d",
        ),
        undecodable(
            "a hyphen after several legal characters",
            "QUJDRA-QUJD".to_string(),
            "2d",
        ),
        undecodable(
            "a hyphen in the last position",
            "QUJDRA-".to_string(),
            "2d",
        ),
        undecodable(
            "an underscore, the other URL-safe character",
            "QUJD_RAB".to_string(),
            "5f",
        ),
        undecodable("a full stop", "QUJD.RAB".to_string(), "2e"),
        undecodable("a colon", "jobtemplates:1:2".to_string(), "3a"),
        undecodable("an exclamation mark", "QUJD!RAB".to_string(), "21"),
        undecodable("a tilde", "QUJD~RAB".to_string(), "7e"),
        undecodable("an escaped space", "QUJD%20RAB".to_string(), "20"),
        undecodable(
            "the shortest token the model allows",
            "-".to_string(),
            "2d",
        ),
        undecodable(
            "the longest token the model allows",
            format!("{}-", "A".repeat(1023)),
            "2d",
        ),
    ]
}

/// One call to `path` with `query` appended.
async fn list(emr: &Emr, path: &str, query: &str) -> Answer {
    emr.send("GET", &format!("{path}{query}"), None).await
}

/// THE RULE: every listing refuses a `nextToken` the base64 decoder cannot read with the
/// decoder's own sentence, naming the first character the alphabet does not hold.
#[tokio::test]
async fn every_paginated_list_refuses_a_token_that_is_not_base64() {
    let emr = Emr::new();
    let listings = every_listing(&emr).await;

    for (label, path) in &listings {
        for row in undecodable_tokens() {
            let case = format!("{label} / {}", row.case);
            let answer = list(&emr, path, &format!("?nextToken={}", row.token)).await;
            answer.refused(&case, 400, INVALID_REQUEST);
            let reported = text(&answer.body, "/message", &case);
            let expected = illegal_character(row.hex);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the refusal {expected:?} for the token {:?}, got \
                 {reported:?}",
                row.token
            );
        }
    }

    // Every listing still answers its own records, so the refusals changed nothing.
    for (label, path) in &listings {
        list(&emr, path, "").await.ok(label);
    }
}

/// The rule's OTHER half: a token that IS base64 but names no position this listing issued
/// is refused as one it never issued, in a sentence that is NOT a decode failure's — which
/// is the distinction the two refusals exist to draw.
#[tokio::test]
async fn a_decodable_token_that_names_no_position_is_refused_in_the_other_wording() {
    let emr = Emr::new();
    let listings = every_listing(&emr).await;

    // The envelope every token of this service is handed out in, around values no cursor
    // encoder ever wrote; and a plain base64 value of the alphabet that is no token at all.
    let rows = vec![
        (
            "the envelope around a value that is no cursor".to_string(),
            state::encode_cursor_envelope("not a cursor at all"),
        ),
        (
            "the envelope around another listing's scope".to_string(),
            state::encode_list_cursor(&state::list_job_templates_scope("eu-west-1"), 1, 2),
        ),
        (
            "a base64 value that is no token of this service".to_string(),
            "QUJDRA==".to_string(),
        ),
    ];

    for (label, path) in &listings {
        for (row, token) in &rows {
            let case = format!("{label} / {row}");
            let answer = list(&emr, path, &format!("?nextToken={token}")).await;
            answer.refused(&case, 400, INVALID_REQUEST);
            let reported = text(&answer.body, "/message", &case);
            assert_eq!(
                reported, UNKNOWN_TOKEN,
                "case {case:?}: expected the refusal {UNKNOWN_TOKEN:?} for the token \
                 {token:?}, got {reported:?}"
            );
        }
    }
}

/// The token a listing HANDS OUT is the opaque base64 value this rule is about: it carries
/// only characters the base64 alphabet holds, so a client can send it back as it stands, and
/// it resumes the walk from where the page ended.
#[tokio::test]
async fn an_issued_token_is_a_base64_value_that_resumes_the_walk() {
    let emr = Emr::new();
    let case = "ListJobTemplates pages its two templates";
    let first = create_template(&emr, 0).await;
    let second = create_template(&emr, 1).await;

    let page = list(&emr, "/jobtemplates", "?maxResults=1").await;
    let body = page.ok(case).clone();
    let ids: Vec<String> = field(&body, "/templates", case)
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: /templates is not a list: {body}"))
        .iter()
        .map(|record| text(record, "/id", case))
        .collect();
    assert_eq!(
        ids,
        vec![second.clone()],
        "case {case:?}: expected the newest template on the first page, got {ids:?}"
    );

    let token = text(&body, "/nextToken", case);
    // The alphabet of the token itself: base64 of lower-case hex, so only `A-Za-z0-9` and
    // the `=` that closes the value's last unit — never the `+` a query string would read as
    // a space ([`state::encode_cursor_envelope`]).
    assert!(
        token.chars().all(|c| c.is_ascii_alphanumeric() || c == '='),
        "case {case:?}: the issued token {token:?} is not a base64 value a query string \
         carries unchanged"
    );

    let resumed = list(&emr, "/jobtemplates", &format!("?nextToken={token}")).await;
    let body = resumed.ok(case).clone();
    let ids: Vec<String> = field(&body, "/templates", case)
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: /templates is not a list: {body}"))
        .iter()
        .map(|record| text(record, "/id", case))
        .collect();
    assert_eq!(
        ids,
        vec![first],
        "case {case:?}: expected the rest of the set after the first page, got {ids:?}"
    );
    assert!(
        body.get("nextToken").is_none(),
        "case {case:?}: expected no nextToken once everything was reported, got {body}"
    );

    // The cursor is tagged with the listing and the region that minted it, so the token is
    // a position in THIS listing's order and nothing else.
    let scope = state::list_job_templates_scope(REGION);
    let payload = state::decode_cursor_envelope(&token)
        .unwrap_or_else(|refusal| panic!("case {case:?}: {token:?} does not decode: {refusal:?}"));
    assert!(
        payload.starts_with(&scope),
        "case {case:?}: expected the token's payload to carry the scope {scope:?}, got \
         {payload:?}"
    );
}
