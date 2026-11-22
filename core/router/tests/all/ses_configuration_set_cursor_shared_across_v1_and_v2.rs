// SPDX-License-Identifier: CC0-1.0

//! ONE paged collection of configuration sets behind the account's email APIs.
//!
//! The rule under test: **a `NextToken` of a configuration-set listing names a position in
//! the ACCOUNT's one configuration-set collection, so a cursor either API issued continues
//! that collection when it is presented to either API, and a value no listing issued is
//! refused by both.** Amazon SES publishes two APIs over one per-account, per-Region
//! configuration-set collection — the classic `awsQuery` `ListConfigurationSets`
//! (`{MaxItems, NextToken}` in, `{ConfigurationSets, NextToken}` out) and the v2 REST
//! `ListConfigurationSets` (`{PageSize, NextToken}` in, `{ConfigurationSets, NextToken}`
//! out) — and a walk of it therefore visits every set of the account exactly once in one
//! stable order, whichever API cut each page and whichever API created each set.
//!
//! The rows differ along what the rule generalises over: the page size a walk asks for
//! (one entry per page up to one page larger than the whole collection), the surface the
//! FIRST page is asked of, whether each later page is asked of the other surface or the
//! same one, and the surface each set was created through. The sibling suite
//! [`crate::ses_configuration_sets_shared_across_v1_and_v2`] spans the set itself — its
//! creation, description, options and deletion across the surfaces; this one spans the
//! CURSOR over them.
//!
//! This suite needs the two services over ONE engine, with the other members of the `ses`
//! signing-name family mounted beside them — the same assembly `core/router/src/lib.rs`
//! performs — because the hand-off being asserted exists only when one engine is shared,
//! and the claim that resolves each request to one service exists only when the peers are
//! there to compete for it.
//!
//! That assembly and the two wire forms over it ([`router`], [`v1`], [`v2`], [`create`],
//! [`page`], [`emulator_holding_sets`]) are `pub(crate)` because
//! [`crate::ses_configuration_set_cursor_outlives_collection_changes`] asks the same
//! listing the same way and must not grow a second copy of them — as does
//! [`crate::ses_identity_cursor_shared_across_v1_and_v2`], which holds the same rule over
//! the account's IDENTITY directory: a suite that writes its own harness drifts from the one
//! beside it, exactly as `crate::query_wire` exists to stop.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, members_of, opt_element};

/// The Region every row signs for and addresses.
const REGION: &str = "us-east-1";

/// SES v1's API version, which an `awsQuery` body carries beside its `Action`
/// (`smithy_query(query="service_info")`: version 2010-12-01).
const SES_V1_API_VERSION: &str = "2010-12-01";

/// The two routing refusals the router answers itself, which no row here may provoke:
/// `AmbiguousService` (several services claimed one request) and `UnknownService` (none
/// did). Neither is in any AWS model.
const ROUTER_REFUSALS: &[&str] = &["AmbiguousService", "UnknownService"];

/// How many configuration sets a walk pages over: more than the largest page a row asks
/// for, so every row's walk crosses at least one page boundary.
pub(crate) const STORED: usize = 7;

/// Which API a call is made on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Surface {
    /// The classic `awsQuery` API.
    V1,
    /// The v2 REST API.
    V2,
}

impl Surface {
    /// Both surfaces, so a row can start its walk on either.
    pub(crate) fn both() -> [Self; 2] {
        [Self::V1, Self::V2]
    }

    /// The other one, which is the surface a crossing walk asks the next page of.
    pub(crate) fn other(self) -> Self {
        match self {
            Self::V1 => Self::V2,
            Self::V2 => Self::V1,
        }
    }
}

/// The services sharing the SigV4 signing name `ses`, over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them.
pub(crate) fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These rows exercise routing and handler behaviour, not SigV4.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_ses_simpleemailservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_pinpointemail_amazonpinpointemailservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_mailmanager_mailmanagersvc::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `ses` — the signal the
/// router resolves a routing key from. The signature itself is never checked.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/ses/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One answered request: its status and its body, as text and as JSON.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) text: String,
    pub(crate) json: Value,
    /// The `x-amzn-errortype` header a restJson1 refusal names its shape in.
    error_type: String,
}

impl Answer {
    /// The modelled error code the answer names, whichever protocol carried it: the
    /// classic surface's `<Code>` element, or the v2 surface's error-type header or
    /// `__type` member.
    pub(crate) fn error_code(&self) -> String {
        if let Some(code) = opt_element(&self.text, "Code") {
            return code;
        }
        let raw = if self.error_type.is_empty() {
            self.json
                .get("__type")
                .and_then(Value::as_str)
                .unwrap_or_default()
        } else {
            self.error_type.as_str()
        };
        let name = raw.split('#').next_back().unwrap_or(raw);
        name.split(':').next().unwrap_or(name).to_string()
    }

    /// The message the answer carries, in either envelope, with the five entities the XML
    /// serializer escapes decoded back.
    pub(crate) fn message(&self) -> String {
        if let Some(message) = opt_element(&self.text, "Message") {
            return message
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&");
        }
        self.json
            .get("message")
            .or_else(|| self.json.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// Send one request, refusing at the door the two answers the ROUTER itself produces: a
/// request that reached nobody, or one several services claimed.
async fn send(
    router: &MultiServiceRouter,
    case: &str,
    leg: &str,
    request: http::Request<Bytes>,
) -> Answer {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .unwrap_or_default();
    let text = String::from_utf8_lossy(response.body()).to_string();
    for refusal in ROUTER_REFUSALS {
        assert!(
            !text.contains(refusal) && !error_type.contains(refusal),
            "case {case:?}: {leg} must be served by exactly one service, got HTTP {status}: \
             {text}"
        );
    }
    let json = serde_json::from_str(&text).unwrap_or(Value::Null);
    Answer {
        status,
        text,
        json,
        error_type,
    }
}

/// One classic-API call: `awsQuery` under the `ses` credential scope, `POST /` with the
/// operation in the `Action` form field.
pub(crate) async fn v1(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    fields: &[(&str, String)],
) -> Answer {
    let mut body = format!("Action={action}&Version={SES_V1_API_VERSION}");
    for (name, value) in fields {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://email.{REGION}.amazonaws.com/"))
        .header("host", format!("email.{REGION}.amazonaws.com"))
        .header("authorization", authorization())
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    send(router, case, &format!("SES v1 {action}"), request).await
}

/// One v2-API call: `restJson1` under the `ses` credential scope, in the `/v2/email/…`
/// path space.
pub(crate) async fn v2(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = match &body {
        Some(value) => Bytes::from(value.to_string()),
        None => Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://email.{REGION}.amazonaws.com{path}"))
        .header("host", format!("email.{REGION}.amazonaws.com"))
        .header("authorization", authorization())
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");
    send(router, case, &format!("SESv2 {method} {path}"), request).await
}

/// Record the configuration set `name` on `surface`, asserting the create succeeded.
pub(crate) async fn create(router: &MultiServiceRouter, case: &str, surface: Surface, name: &str) {
    let answered = match surface {
        Surface::V1 => {
            v1(
                router,
                case,
                "CreateConfigurationSet",
                &[("ConfigurationSet.Name", name.to_string())],
            )
            .await
        }
        Surface::V2 => {
            v2(
                router,
                case,
                "POST",
                "/v2/email/configuration-sets",
                Some(json!({"ConfigurationSetName": name})),
            )
            .await
        }
    };
    assert_eq!(
        answered.status, 200,
        "case {case:?}: the create of {name:?} on {surface:?} expected HTTP 200, got {}: {}",
        answered.status, answered.text
    );
}

/// One page of the account's configuration-set listing, asked of `surface`: `(the names it
/// reported, the NextToken it carried)`.
pub(crate) async fn page(
    router: &MultiServiceRouter,
    case: &str,
    surface: Surface,
    page_size: usize,
    token: Option<&str>,
) -> (Vec<String>, Option<String>) {
    match surface {
        Surface::V1 => {
            let mut fields: Vec<(&str, String)> = vec![("MaxItems", page_size.to_string())];
            if let Some(token) = token {
                fields.push(("NextToken", token.to_string()));
            }
            let answered = v1(router, case, "ListConfigurationSets", &fields).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: SES v1 ListConfigurationSets expected HTTP 200, got {}: {}",
                answered.status, answered.text
            );
            let block = opt_element(&answered.text, "ConfigurationSets").unwrap_or_default();
            let names = members_of(&block)
                .into_iter()
                .filter_map(|member| opt_element(member, "Name"))
                .collect();
            (names, opt_element(&answered.text, "NextToken"))
        }
        Surface::V2 => {
            let mut query = format!("PageSize={page_size}");
            if let Some(token) = token {
                // The v2 listing binds `NextToken` to `@httpQuery` on its `GET` form and
                // the token is base64, whose alphabet includes `+`, `/` and `=` —
                // characters a query string gives its own meaning to, so the value is
                // escaped the way an SDK escapes it.
                query.push_str(&format!("&NextToken={}", form_encode(token)));
            }
            let answered = v2(
                router,
                case,
                "GET",
                &format!("/v2/email/configuration-sets?{query}"),
                None,
            )
            .await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: SESv2 ListConfigurationSets expected HTTP 200, got {}: {}",
                answered.status, answered.text
            );
            let names = answered
                .json
                .get("ConfigurationSets")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_string))
                .collect();
            let token = answered
                .json
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|token| !token.is_empty());
            (names, token)
        }
    }
}

/// Every configuration set the listing reports, asked of `first` for the first page and of
/// the surface `switch` answers for each page after it — so a walk can hand every cursor to
/// the OTHER API.
async fn walk(
    router: &MultiServiceRouter,
    case: &str,
    page_size: usize,
    mut surface: Surface,
    switch: impl Fn(Surface) -> Surface,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // Bounded above every page count a row here can ask for, so a walk that never
    // terminates fails instead of looping forever.
    for round in 0..1_000 {
        let (names, next) = page(router, case, surface, page_size, token.as_deref()).await;
        assert!(
            names.len() <= page_size,
            "case {case:?} page {round}: expected at most {page_size} names, got {names:?}"
        );
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

/// A router holding [`STORED`] configuration sets, created alternately through the two
/// APIs, and the names in the one order a listing reports them (lexicographic).
pub(crate) async fn emulator_holding_sets(case: &str) -> (MultiServiceRouter, Vec<String>) {
    let names: Vec<String> = (0..STORED).map(|index| format!("set-{index:02}")).collect();
    let router = router();
    for (index, name) in names.iter().enumerate() {
        let surface = if index % 2 == 0 {
            Surface::V1
        } else {
            Surface::V2
        };
        create(&router, case, surface, name).await;
    }
    let expected: Vec<String> = {
        let mut sorted = names.clone();
        sorted.sort();
        sorted
    };
    (router, expected)
}

#[tokio::test]
async fn a_configuration_set_cursor_issued_by_either_surface_continues_the_other() {
    let (router, expected) = emulator_holding_sets("the shared walk").await;

    // Each row is a page size and the surface the FIRST page is asked of; every page after
    // it is asked of the other API, so every cursor crosses the surface that issued it.
    for page_size in [1_usize, 2, 3, STORED, STORED + 1] {
        for first in Surface::both() {
            let case = format!("page size {page_size} starting on {first:?}, alternating");
            let seen = walk(&router, &case, page_size, first, Surface::other).await;
            assert_eq!(
                seen, expected,
                "case {case:?}: a cursor must continue the one collection of the account's \
                 configuration sets, visiting each exactly once in name order, got {seen:?}"
            );

            // And the same walk asked of ONE surface throughout reports the same
            // collection, so crossing surfaces neither repeats nor skips an entry.
            let case = format!("page size {page_size} on {first:?} throughout");
            let same = walk(&router, &case, page_size, first, |surface| surface).await;
            assert_eq!(
                same, expected,
                "case {case:?}: expected the same collection, got {same:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_cursor_no_configuration_set_listing_issued_is_refused_by_both_surfaces() {
    let case = "a cursor no listing issued";
    let (router, expected) = emulator_holding_sets(case).await;

    // The cursor one surface hands out, which the rows below are the counter-cases of.
    let (_, issued) = page(&router, case, Surface::V1, 2, None).await;
    let issued = issued.expect("a collection larger than the page size yields a NextToken");

    // Each row is a value a caller could send that no listing of this collection minted:
    // base64 of text no listing ever wrote, a position sent bare, text that is not base64
    // at all, a minted cursor with its last byte dropped and one with a byte appended.
    let rows: Vec<(&str, String)> = vec![
        (
            "base64 of text no listing minted",
            ::aws_smithy_types::base64::encode("somewhere else"),
        ),
        ("a configuration set name sent bare", expected[0].clone()),
        ("text that is not base64 at all", "!!not-a-token!!".to_string()),
        (
            "a minted cursor with its last byte dropped",
            issued[..issued.len() - 2].to_string(),
        ),
        ("a minted cursor with a byte appended", format!("{issued}AA")),
    ];
    for (form, token) in &rows {
        for surface in Surface::both() {
            let case = format!("{form} on {surface:?}");
            // The error each API's own model declares for an input it cannot read, with
            // that API's own wording for this condition.
            let (expected_status, expected_code, expected_message) = match surface {
                Surface::V1 => (
                    400,
                    "InvalidParameterValue",
                    format!("Invalid PageToken <{token}>."),
                ),
                Surface::V2 => (400, "BadRequestException", "Invalid next token.".to_string()),
            };
            let refused = match surface {
                Surface::V1 => {
                    v1(
                        &router,
                        &case,
                        "ListConfigurationSets",
                        &[("NextToken", token.clone())],
                    )
                    .await
                }
                Surface::V2 => {
                    v2(
                        &router,
                        &case,
                        "GET",
                        &format!(
                            "/v2/email/configuration-sets?NextToken={}",
                            form_encode(token)
                        ),
                        None,
                    )
                    .await
                }
            };
            assert_eq!(
                (refused.status, refused.error_code().as_str()),
                (expected_status, expected_code),
                "case {case:?}: expected HTTP {expected_status} {expected_code}, got {} {:?}: {}",
                refused.status,
                refused.error_code(),
                refused.text
            );
            assert_eq!(
                refused.message(),
                expected_message,
                "case {case:?}: expected the message {expected_message:?}, got {:?}",
                refused.message()
            );
        }
    }

    // A refused cursor changes nothing: the collection is whole on both surfaces, and the
    // cursor issued before the refusals still continues it on the OTHER surface.
    for surface in Surface::both() {
        let case = format!("after {} refusals, walked on {surface:?}", rows.len() * 2);
        let seen = walk(&router, &case, 2, surface, Surface::other).await;
        assert_eq!(
            seen, expected,
            "case {case:?}: the collection must be untouched, got {seen:?}"
        );
    }
    let (resumed, _) = page(&router, case, Surface::V2, 2, Some(&issued)).await;
    assert_eq!(
        resumed,
        expected[2..4].to_vec(),
        "case the issued cursor after the refusals: expected the page after the first, got \
         {resumed:?}"
    );
}
