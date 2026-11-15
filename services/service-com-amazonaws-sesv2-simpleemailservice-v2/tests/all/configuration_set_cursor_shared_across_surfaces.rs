// SPDX-License-Identifier: CC0-1.0

//! ONE paged collection of configuration sets behind the account's two email APIs.
//!
//! The rule under test: **a `NextToken` of a configuration-set listing names a position in
//! the ACCOUNT's one configuration-set collection, so a cursor either API issued continues
//! that collection when it is presented to either API, and a value no listing issued is
//! refused by both.** Amazon SES publishes two APIs over one per-account, per-Region
//! configuration-set collection — this surface's `ListConfigurationSets`
//! (`{Filter, PageSize, NextToken}` in, `{ConfigurationSets, NextToken}` out) and the
//! classic `awsQuery` `ListConfigurationSets` (`{MaxItems, NextToken}` in,
//! `{ConfigurationSets, NextToken}` out) — so a walk of it visits every set of the account
//! exactly once in one stable order, whichever API cut each page and whichever API created
//! each set.
//!
//! The rows differ along what the rule generalises over: the page size a walk asks for (one
//! entry per page up to one page larger than the whole collection), the surface the FIRST
//! page is asked of, whether each later page is asked of the other surface or the same one,
//! the surface each set was created through, and — for the refusal — the form a cursor no
//! listing minted arrives in.
//!
//! Both services are mounted on ONE engine here, as the assembled router mounts them,
//! because the hand-off being asserted exists only when the engine behind them is shared.
//! The sibling `page_cursor_refusal_wording` suite spans this surface's wording for a
//! cursor it did not issue; this one spans the cursor crossing the two APIs.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::json_wire::{query_component, Reply, Ses, HOME};

/// SES v1's API version, which an `awsQuery` body carries beside its `Action`
/// (`smithy_query(query="service_info")` of the classic model: version 2010-12-01).
const SES_V1_API_VERSION: &str = "2010-12-01";

/// How many configuration sets a walk pages over: more than the largest page a row asks
/// for, so every row's walk crosses at least one page boundary.
const STORED: usize = 7;

/// Which API a call is made on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    /// The classic `awsQuery` API.
    V1,
    /// This service's v2 REST API.
    V2,
}

impl Surface {
    /// Both surfaces, so a row can start its walk on either.
    fn both() -> [Self; 2] {
        [Self::V1, Self::V2]
    }

    /// The other one, which is the surface a crossing walk asks the next page of.
    fn other(self) -> Self {
        match self {
            Self::V1 => Self::V2,
            Self::V2 => Self::V1,
        }
    }
}

/// One answered call, on either surface: the status, the error code it named and the body
/// text a reader of that surface's envelope pulls values out of.
struct Answer {
    status: u16,
    code: String,
    message: String,
    /// The configuration-set names the page reported, in the order it reported them.
    names: Vec<String>,
    /// The `NextToken` the page handed back, when it handed one back.
    next_token: Option<String>,
    /// The whole answer, for an assertion's message.
    text: String,
}

/// The two services over one engine: this surface, and the classic surface that publishes
/// the same account configuration sets.
fn emulator() -> Ses {
    let mut ses = Ses::new();
    ses.router.register_service(
        service_com_amazonaws_ses_simpleemailservice::EmulatedService::new(Arc::clone(&ses.engine)),
    );
    ses
}

/// One classic-surface call: `awsQuery`, `POST /` with the operation in the `Action` form
/// field, answered in XML.
///
/// The fields are percent-encoded by the crate this crate's suites declare for it
/// ([`query_component`]), so no suite writes the escaping itself.
async fn v1_call(ses: &Ses, action: &str, fields: &[(&str, String)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version={SES_V1_API_VERSION}");
    for (name, value) in fields {
        body.push('&');
        body.push_str(&query_component(name));
        body.push('=');
        body.push_str(&query_component(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://email.{HOME}.amazonaws.com/"))
        .header("host", format!("email.{HOME}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR0000000/20260101/{HOME}/ses/\
                 aws4_request, SignedHeaders=host, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("build request");
    let response = ses.router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    (status, text)
}

/// The text of the first `<name>…</name>` element of an XML answer, with the five entities
/// the serializer escapes decoded back, or `None` when the answer carries no such element.
///
/// A reader, not an XML parser, and only for documents this emulator itself produced: this
/// crate's own wire harness answers JSON, so the classic surface's XML is read here, where
/// the one suite that reads it lives.
fn element(text: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(unescape(&text[start..end]))
}

/// The five entities an XML serializer escapes, decoded back. `&amp;` LAST, so a value that
/// carried a literal `&lt;` is not turned into a `<`.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Each `<member>` block of an awsQuery list, in document order: the serializer writes the
/// FIRST member with the service's namespace attribute and the rest bare, so a block starts
/// after its own tag's closing bracket.
fn members_of(text: &str) -> Vec<&str> {
    text.split("<member")
        .skip(1)
        .filter_map(|block| block.split_once('>'))
        .filter_map(|(_, body)| body.split("</member>").next())
        .collect()
}

/// Record the configuration set `name` on `surface`, asserting the create succeeded.
async fn create(ses: &Ses, case: &str, surface: Surface, name: &str) {
    match surface {
        Surface::V1 => {
            let (status, text) = v1_call(
                ses,
                "CreateConfigurationSet",
                &[("ConfigurationSet.Name", name.to_string())],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the create of {name:?} on {surface:?} expected HTTP 200, got \
                 {status}: {text}"
            );
        }
        Surface::V2 => {
            let created: Reply = ses
                .call(
                    "POST",
                    "/v2/email/configuration-sets",
                    Some(json!({"ConfigurationSetName": name})),
                )
                .await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: the create of {name:?} on {surface:?} expected HTTP 200, got {}: \
                 {}",
                created.status, created.body
            );
        }
    }
}

/// One page of the account's configuration-set listing, asked of `surface`, with
/// `page_size` as the page it asks for and `token` as the cursor it presents.
async fn page(
    ses: &Ses,
    surface: Surface,
    page_size: Option<usize>,
    token: Option<&str>,
) -> Answer {
    match surface {
        Surface::V1 => {
            let mut fields: Vec<(&str, String)> = Vec::new();
            if let Some(page_size) = page_size {
                fields.push(("MaxItems", page_size.to_string()));
            }
            if let Some(token) = token {
                fields.push(("NextToken", token.to_string()));
            }
            let (status, text) = v1_call(ses, "ListConfigurationSets", &fields).await;
            // The page's names are read off the list block, so a `<Name>` of some other
            // element of the answer is not mistaken for one of them.
            let listed = text
                .split_once("<ConfigurationSets>")
                .map(|(_, rest)| rest)
                .unwrap_or("");
            let names = members_of(listed)
                .into_iter()
                .filter_map(|member| element(member, "Name"))
                .collect();
            Answer {
                status,
                code: element(&text, "Code").unwrap_or_default(),
                message: element(&text, "Message").unwrap_or_default(),
                names,
                next_token: element(&text, "NextToken"),
                text,
            }
        }
        Surface::V2 => {
            // `PageSize` and `NextToken` are bound to `@httpQuery` on the GET form every
            // SDK sends this listing on, and a cursor is base64 — whose alphabet includes
            // `+`, `/` and `=`, characters a query string gives its own meaning to — so the
            // value is escaped the way an SDK escapes it.
            let mut query: Vec<String> = Vec::new();
            if let Some(page_size) = page_size {
                query.push(format!("PageSize={page_size}"));
            }
            if let Some(token) = token {
                query.push(format!("NextToken={}", query_component(token)));
            }
            let path = if query.is_empty() {
                "/v2/email/configuration-sets".to_string()
            } else {
                format!("/v2/email/configuration-sets?{}", query.join("&"))
            };
            let listed = ses.call("GET", &path, None).await;
            let names = listed
                .body
                .get("ConfigurationSets")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_string))
                .collect();
            Answer {
                status: listed.status,
                code: listed.error_code(),
                message: listed.message(),
                names,
                next_token: listed.next_token().filter(|token| !token.is_empty()),
                text: listed.body.to_string(),
            }
        }
    }
}

/// One page that must be served, as `(the names it reported, the cursor it handed back)`.
async fn served_page(
    ses: &Ses,
    case: &str,
    surface: Surface,
    page_size: usize,
    token: Option<&str>,
) -> (Vec<String>, Option<String>) {
    let answered = page(ses, surface, Some(page_size), token).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: ListConfigurationSets on {surface:?} expected HTTP 200, got {}: {}",
        answered.status, answered.text
    );
    assert!(
        answered.names.len() <= page_size,
        "case {case:?}: expected at most {page_size} names on a page, got {:?}",
        answered.names
    );
    (answered.names, answered.next_token)
}

/// Every configuration set the listing reports, asked of `first` for the first page and of
/// the surface `switch` answers for each page after it — so a walk can hand every cursor to
/// the OTHER API.
async fn walk(
    ses: &Ses,
    case: &str,
    page_size: usize,
    mut surface: Surface,
    switch: impl Fn(Surface) -> Surface,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // Bounded above every page count a row here can ask for, so a walk that never
    // terminates fails instead of looping forever.
    for _ in 0..1_000 {
        let (names, next) = served_page(ses, case, surface, page_size, token.as_deref()).await;
        seen.extend(names);
        match next {
            None => return seen,
            Some(next) => {
                token = Some(next);
                surface = switch(surface);
            }
        }
    }
    panic!("case {case:?}: the walk never ran out of pages");
}

/// An emulator holding [`STORED`] configuration sets, created alternately through the two
/// APIs, and the names in the one order a listing reports them (lexicographic).
async fn emulator_holding_sets(case: &str) -> (Ses, Vec<String>) {
    let names: Vec<String> = (0..STORED).map(|index| format!("set-{index:02}")).collect();
    let ses = emulator();
    for (index, name) in names.iter().enumerate() {
        let surface = if index % 2 == 0 {
            Surface::V1
        } else {
            Surface::V2
        };
        create(&ses, case, surface, name).await;
    }
    let mut expected = names;
    expected.sort();
    (ses, expected)
}

#[tokio::test]
async fn a_configuration_set_cursor_issued_by_either_surface_continues_the_other() {
    let (ses, expected) = emulator_holding_sets("the shared walk").await;

    // Each row is a page size and the surface the FIRST page is asked of; every page after
    // it is asked of the other API, so every cursor crosses the surface that issued it.
    for page_size in [1_usize, 2, 3, STORED, STORED + 1] {
        for first in Surface::both() {
            let case = format!("page size {page_size} starting on {first:?}, alternating");
            let seen = walk(&ses, &case, page_size, first, Surface::other).await;
            assert_eq!(
                seen, expected,
                "case {case:?}: a cursor must continue the one collection of the account's \
                 configuration sets, visiting each exactly once in name order, got {seen:?}"
            );

            // And the same walk asked of ONE surface throughout reports the same
            // collection, so crossing surfaces neither repeats nor skips an entry.
            let case = format!("page size {page_size} on {first:?} throughout");
            let same = walk(&ses, &case, page_size, first, |surface| surface).await;
            assert_eq!(
                same, expected,
                "case {case:?}: expected the same collection, got {same:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_cursor_no_configuration_set_listing_issued_is_refused_by_both_surfaces() {
    let (ses, expected) = emulator_holding_sets("a cursor no listing issued").await;

    // The cursor one surface hands out, which the rows below are the counter-cases of.
    let (first_page, issued) = served_page(&ses, "the issued cursor", Surface::V1, 2, None).await;
    let issued = issued.expect("a collection larger than the page size yields a NextToken");

    // Each row is a value a caller could send that no listing of this collection minted:
    // base64 of text no listing ever wrote, a position sent bare, text that is not base64
    // at all, a minted cursor with its last byte dropped, and one with a byte appended.
    let rows: Vec<(&str, String)> = vec![
        (
            "base64 of text no listing minted",
            ::aws_smithy_types::base64::encode("somewhere else"),
        ),
        ("a configuration set name sent bare", expected[0].clone()),
        (
            "text that is not base64 at all",
            "!!not-a-token!!".to_string(),
        ),
        (
            "a minted cursor with its last byte dropped",
            issued[..issued.len() - 2].to_string(),
        ),
        ("a minted cursor with a byte appended", format!("{issued}AA")),
    ];
    for (form, token) in &rows {
        for surface in Surface::both() {
            let case = format!("{form} on {surface:?}");
            // The error each API's own model declares for an input it cannot read, in that
            // API's own wording for this condition.
            let (expected_code, expected_message) = match surface {
                Surface::V1 => (
                    "InvalidParameterValue",
                    format!("Invalid PageToken <{token}>."),
                ),
                Surface::V2 => ("BadRequestException", "Invalid next token.".to_string()),
            };
            let refused = page(&ses, surface, None, Some(token)).await;
            assert_eq!(
                (refused.status, refused.code.as_str()),
                (400, expected_code),
                "case {case:?}: expected HTTP 400 {expected_code}, got {} {:?}: {}",
                refused.status,
                refused.code,
                refused.text
            );
            assert_eq!(
                refused.message, expected_message,
                "case {case:?}: expected the message {expected_message:?}, got {:?}",
                refused.message
            );
        }
    }

    // A refused cursor changes nothing: the collection is whole on both surfaces, and the
    // cursor issued before the refusals still continues it on the OTHER surface.
    for surface in Surface::both() {
        let case = format!("after {} refusals, walked on {surface:?}", rows.len() * 2);
        let seen = walk(&ses, &case, 2, surface, Surface::other).await;
        assert_eq!(
            seen, expected,
            "case {case:?}: the collection must be untouched, got {seen:?}"
        );
    }
    let (resumed, _) = served_page(
        &ses,
        "the issued cursor after the refusals",
        Surface::V2,
        2,
        Some(&issued),
    )
    .await;
    assert_eq!(
        resumed,
        expected[first_page.len()..first_page.len() + 2].to_vec(),
        "case the issued cursor after the refusals: expected the page after the first on the \
         other surface, got {resumed:?}"
    );
}
