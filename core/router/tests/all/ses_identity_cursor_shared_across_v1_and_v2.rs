// SPDX-License-Identifier: CC0-1.0

//! ONE paged directory of email identities behind the account's email APIs.
//!
//! The rule under test: **a `NextToken` of an identity listing names a position in the
//! ACCOUNT's one email-identity directory, so a cursor either API issued continues that
//! directory when it is presented to either API, and a value no listing issued is refused by
//! both.** Amazon SES publishes two APIs over one per-account, per-Region identity directory
//! — the classic `awsQuery` `ListIdentities` (`{IdentityType, MaxItems, NextToken}` in,
//! `{Identities, NextToken}` out) and the v2 REST `ListEmailIdentities` (`{Filter, PageSize,
//! NextToken}` in, `{EmailIdentities, NextToken}` out) — and a walk of it therefore visits
//! every identity of the account exactly once in one stable order, whichever API cut each
//! page and whichever API recorded each identity.
//!
//! The rows differ along what the rule generalises over: the page size a walk asks for (one
//! entry per page up to one page larger than the whole directory), the surface the FIRST page
//! is asked of, whether each later page is asked of the other surface or the same one, the
//! surface each identity was recorded through, and the class of identity (a domain, a
//! subdomain, a mailbox address). The sibling suite
//! [`crate::ses_identities_shared_across_v1_and_v2`] spans the identity itself — its
//! recording, its verification state and its deletion across the surfaces; this one spans the
//! CURSOR over them, exactly as
//! [`crate::ses_configuration_set_cursor_shared_across_v1_and_v2`] does for the account's
//! configuration sets.
//!
//! This suite needs the two services over ONE engine, with the other members of the `ses`
//! signing-name family mounted beside them, because the hand-off being asserted exists only
//! when one engine is shared and the claim that resolves each request to one service exists
//! only when the peers are there to compete for it. That assembly and the two wire forms over
//! it are the `pub(crate)` harness of
//! [`crate::ses_configuration_set_cursor_shared_across_v1_and_v2`], so this suite grows no
//! second copy of them.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, elements, form_encode, members_of, opt_element};
use crate::ses_configuration_set_cursor_shared_across_v1_and_v2::{router, v1, v2, Surface};

/// How many identities a walk pages over: more than the largest page a row asks for, so
/// every row's walk crosses at least one page boundary.
const STORED: usize = 7;

/// The identities the account holds, in the order they are recorded. Spans the classes the
/// directory holds — a domain, a subdomain of one, and a mailbox address — because an
/// identity's class decides the key it is held under and so where a cursor cuts the walk.
///
/// All lowercase, so each identity's spelling IS the key the directory orders it by
/// (the domain part of an identity is compared without regard to case).
fn identity_names() -> Vec<String> {
    (0..STORED)
        .map(|index| match index % 3 {
            0 => format!("id-{index:02}.example.com"),
            1 => format!("mail.id-{index:02}.example.net"),
            _ => format!("user-{index:02}@id-{index:02}.example.org"),
        })
        .collect()
}

/// Record the identity `name` on `surface`, asserting the call succeeded.
///
/// The classic surface records a domain through `VerifyDomainIdentity` and a mailbox through
/// `VerifyEmailIdentity`; the v2 surface takes either through `CreateEmailIdentity`.
async fn record(router: &MultiServiceRouter, case: &str, surface: Surface, name: &str) {
    let answered = match (surface, name.contains('@')) {
        (Surface::V1, false) => {
            v1(
                router,
                case,
                "VerifyDomainIdentity",
                &[("Domain", name.to_string())],
            )
            .await
        }
        (Surface::V1, true) => {
            v1(
                router,
                case,
                "VerifyEmailIdentity",
                &[("EmailAddress", name.to_string())],
            )
            .await
        }
        (Surface::V2, _) => {
            v2(
                router,
                case,
                "POST",
                "/v2/email/identities",
                Some(json!({"EmailIdentity": name})),
            )
            .await
        }
    };
    assert_eq!(
        answered.status, 200,
        "case {case:?}: recording {name:?} on {surface:?} expected HTTP 200, got {}: {}",
        answered.status, answered.text
    );
}

/// One page of the account's identity listing, asked of `surface`: `(the identities it
/// reported, the NextToken it carried)`.
async fn page(
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
            let answered = v1(router, case, "ListIdentities", &fields).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: SES v1 ListIdentities expected HTTP 200, got {}: {}",
                answered.status, answered.text
            );
            let block = block_of(&answered.text, "Identities").unwrap_or_default();
            let names = members_of(block)
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<String>>();
            (names, opt_element(&answered.text, "NextToken"))
        }
        Surface::V2 => {
            let mut query = format!("PageSize={page_size}");
            if let Some(token) = token {
                // The v2 listing binds `NextToken` to `@httpQuery` on its `GET` form and the
                // token is base64, whose alphabet includes `+`, `/` and `=` — characters a
                // query string gives its own meaning to, so the value is escaped the way an
                // SDK escapes it.
                query.push_str(&format!("&NextToken={}", form_encode(token)));
            }
            let answered = v2(
                router,
                case,
                "GET",
                &format!("/v2/email/identities?{query}"),
                None,
            )
            .await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: SESv2 ListEmailIdentities expected HTTP 200, got {}: {}",
                answered.status, answered.text
            );
            let names = answered
                .json
                .get("EmailIdentities")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("IdentityName")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
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

/// Every identity the listing reports, asked of `first` for the first page and of the surface
/// `switch` answers for each page after it — so a walk can hand every cursor to the OTHER API.
async fn walk(
    router: &MultiServiceRouter,
    case: &str,
    page_size: usize,
    mut surface: Surface,
    switch: impl Fn(Surface) -> Surface,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // Bounded above every page count a row here can ask for, so a walk that never terminates
    // fails instead of looping forever.
    for round in 0..1_000 {
        let (names, next) = page(router, case, surface, page_size, token.as_deref()).await;
        assert!(
            names.len() <= page_size,
            "case {case:?} page {round}: expected at most {page_size} identities, got {names:?}"
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

/// A router holding [`STORED`] identities, recorded alternately through the two APIs, and the
/// names in the one order a listing reports them (by identity key, which for these names is
/// lexicographic).
async fn emulator_holding_identities(case: &str) -> (MultiServiceRouter, Vec<String>) {
    let names = identity_names();
    let router = router();
    for (index, name) in names.iter().enumerate() {
        let surface = if index % 2 == 0 {
            Surface::V1
        } else {
            Surface::V2
        };
        record(&router, case, surface, name).await;
    }
    let expected: Vec<String> = {
        let mut sorted = names.clone();
        sorted.sort();
        sorted
    };
    (router, expected)
}

#[tokio::test]
async fn an_identity_cursor_issued_by_either_surface_continues_the_other() {
    let (router, expected) = emulator_holding_identities("the shared walk").await;

    // Each row is a page size and the surface the FIRST page is asked of; every page after it
    // is asked of the other API, so every cursor crosses the surface that issued it.
    for page_size in [1_usize, 2, 3, STORED, STORED + 1] {
        for first in Surface::both() {
            let case = format!("page size {page_size} starting on {first:?}, alternating");
            let seen = walk(&router, &case, page_size, first, Surface::other).await;
            assert_eq!(
                seen, expected,
                "case {case:?}: a cursor must continue the one directory of the account's email \
                 identities, visiting each exactly once in key order, got {seen:?}"
            );

            // And the same walk asked of ONE surface throughout reports the same directory, so
            // crossing surfaces neither repeats nor skips an entry.
            let case = format!("page size {page_size} on {first:?} throughout");
            let same = walk(&router, &case, page_size, first, |surface| surface).await;
            assert_eq!(
                same, expected,
                "case {case:?}: expected the same directory, got {same:?}"
            );
        }
    }

    // The leg the hand-off is for: the identity page one reported is absent from the whole
    // continuation the OTHER surface serves from that cursor, and the two together are the
    // account's directory.
    for first in Surface::both() {
        let case = format!("one page on {first:?}, the rest on {:?}", first.other());
        let (page_one, issued) = page(&router, &case, first, 1, None).await;
        let issued = issued
            .unwrap_or_else(|| panic!("case {case:?}: a directory of {STORED} yields a cursor"));
        let mut continued: Vec<String> = Vec::new();
        let mut token = Some(issued);
        while let Some(cursor) = token {
            let (names, next) = page(&router, &case, first.other(), 2, Some(&cursor)).await;
            continued.extend(names);
            token = next;
        }
        assert!(
            !continued.contains(&page_one[0]),
            "case {case:?}: the identity page one reported ({:?}) must not reappear in the \
             continuation, got {continued:?}",
            page_one[0]
        );
        assert_eq!(
            [page_one.clone(), continued.clone()].concat(),
            expected,
            "case {case:?}: page one and the continuation must be the whole directory, got \
             {page_one:?} then {continued:?}"
        );
    }
}

#[tokio::test]
async fn a_cursor_no_identity_listing_issued_is_refused_by_both_surfaces() {
    let case = "a cursor no listing issued";
    let (router, expected) = emulator_holding_identities(case).await;

    // The cursor one surface hands out, which the rows below are the counter-cases of.
    let (_, issued) = page(&router, case, Surface::V1, 2, None).await;
    let issued = issued.expect("a directory larger than the page size yields a NextToken");

    // Each row is a value a caller could send that no listing of this directory minted:
    // base64 of text no listing ever wrote, a position sent bare, text that is not base64 at
    // all, a minted cursor with its last byte dropped and one with a byte appended.
    let rows: Vec<(&str, String)> = vec![
        (
            "base64 of text no listing minted",
            ::aws_smithy_types::base64::encode("somewhere else"),
        ),
        ("an identity name sent bare", expected[0].clone()),
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
            // The error each API's own model declares for an input it cannot read, with that
            // API's own wording for this condition.
            let (expected_status, expected_code) = match surface {
                Surface::V1 => (400, "InvalidParameterValue"),
                Surface::V2 => (400, "BadRequestException"),
            };
            let expected_message = format!("Invalid NextToken <{token}>.");
            let refused = match surface {
                Surface::V1 => {
                    v1(
                        &router,
                        &case,
                        "ListIdentities",
                        &[("NextToken", token.clone())],
                    )
                    .await
                }
                Surface::V2 => {
                    v2(
                        &router,
                        &case,
                        "GET",
                        &format!("/v2/email/identities?NextToken={}", form_encode(token)),
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

    // A refused cursor changes nothing: the directory is whole on both surfaces, and the
    // cursor issued before the refusals still continues it on the OTHER surface.
    for surface in Surface::both() {
        let case = format!("after {} refusals, walked on {surface:?}", rows.len() * 2);
        let seen = walk(&router, &case, 2, surface, Surface::other).await;
        assert_eq!(
            seen, expected,
            "case {case:?}: the directory must be untouched, got {seen:?}"
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

#[tokio::test]
async fn a_cursor_of_a_filtered_walk_continues_the_same_narrowed_listing() {
    let case = "a filtered walk";
    let (router, expected) = emulator_holding_identities(case).await;

    // Both APIs narrow the listing by identity class — the classic surface's `IdentityType`
    // (`Domain`/`EmailAddress`), the v2 surface's `IDENTITY_TYPE` filter
    // (`DOMAIN`/`EMAIL_ADDRESS`) — so a cursor cut from a narrowed walk is a position in the
    // directory, and the other surface's narrowed walk continues it.
    let rows = [
        (
            "domains",
            "Domain",
            "DOMAIN",
            expected
                .iter()
                .filter(|name| !name.contains('@'))
                .cloned()
                .collect::<Vec<String>>(),
        ),
        (
            "mailbox addresses",
            "EmailAddress",
            "EMAIL_ADDRESS",
            expected
                .iter()
                .filter(|name| name.contains('@'))
                .cloned()
                .collect::<Vec<String>>(),
        ),
    ];
    for (class, v1_type, v2_type, want) in rows {
        assert!(
            want.len() >= 2,
            "case {class}: the fixture must hold more than one {class} to page over, got {want:?}"
        );
        let case = format!("{class}, first page on V1");
        let answered = v1(
            &router,
            &case,
            "ListIdentities",
            &[
                ("IdentityType", v1_type.to_string()),
                ("MaxItems", "1".to_string()),
            ],
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected HTTP 200, got {}: {}",
            answered.status, answered.text
        );
        let first = elements(
            block_of(&answered.text, "Identities").unwrap_or_default(),
            "member",
        );
        assert_eq!(
            first,
            want[..1].to_vec(),
            "case {case:?}: expected the first {class} of the directory, got {first:?}"
        );
        let mut token = opt_element(&answered.text, "NextToken");
        assert!(
            token.is_some(),
            "case {case:?}: a narrowed listing with more to report must hand back a cursor, got \
             {}",
            answered.text
        );

        // Every later page is asked of the v2 surface, with ITS spelling of the same filter.
        let mut seen = first.clone();
        while let Some(cursor) = token {
            let answered = v2(
                &router,
                &case,
                "POST",
                "/v2/email/list-identities",
                Some(json!({
                    "PageSize": 1,
                    "NextToken": cursor,
                    "Filter": {"IDENTITY_TYPE": v2_type},
                })),
            )
            .await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: SESv2 ListEmailIdentities expected HTTP 200, got {}: {}",
                answered.status, answered.text
            );
            seen.extend(
                answered
                    .json
                    .get("EmailIdentities")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|entry| {
                        entry
                            .get("IdentityName")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    }),
            );
            token = answered
                .json
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|token| !token.is_empty());
        }
        assert_eq!(
            seen, want,
            "case {case:?}: a cursor cut from the narrowed walk must continue it on the other \
             surface, got {seen:?}"
        );
    }
}
