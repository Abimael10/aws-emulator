// SPDX-License-Identifier: CC0-1.0

//! A configuration-set cursor is a POSITION in the account's one collection, not a count
//! of what one API already reported.
//!
//! The rule under test: **a `NextToken` of a configuration-set listing names the position
//! the next page starts at in the ACCOUNT's single configuration-set collection, so
//! presenting it to any API or wire form over that collection reports exactly the sets the
//! account holds at or after that position, in one name order — whatever was created or
//! deleted behind it, and whichever API cut the page.** Amazon SES publishes two APIs over
//! one per-account, per-Region configuration-set collection: the classic `awsQuery`
//! `ListConfigurationSets` (`{MaxItems, NextToken}` in, `{ConfigurationSets, NextToken}`
//! out) and the v2 `ListConfigurationSets` (`{Filter, PageSize, NextToken}` in,
//! `{ConfigurationSets, NextToken}` out), whose model binds `POST
//! /v2/email/list-configuration-sets` while every SDK sends the `GET` form over the
//! collection itself — three ways to ask one listing for one page.
//!
//! The rows differ along what the rule generalises over: which wire form ISSUED the cursor,
//! which wire form each later page is asked of, and what changed in the collection between
//! the two calls (nothing; the set the cursor names deleted; a set deleted behind the
//! position; the last set deleted; a set inserted before the position; a set inserted after
//! it) — plus, in the second test, the page size the walk asks for. Every expectation is
//! recomputed from the sets the account holds at continuation time, so no row asserts a
//! page offset: a cursor that behaved as an index into the listing, or as a value private
//! to the API that minted it, fails the rows that change the collection behind it.
//!
//! The sibling suite [`crate::ses_configuration_set_cursor_shared_across_v1_and_v2`] spans
//! the hand-off over an UNCHANGING collection and the refusal of a cursor no listing
//! minted; this suite spans what the cursor survives, and the v2 API's modelled route
//! beside the GET form that suite asks. Its assembly of the `ses` signing-name family over
//! one engine, and its two wire forms over that assembly, are reused from it rather than
//! copied — see that suite's header. [`crate::ses_configuration_sets_shared_across_v1_and_v2`]
//! spans the set rather than the cursor over it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ses_configuration_set_cursor_shared_across_v1_and_v2::{
    create, emulator_holding_sets, page, v1, v2, Surface, STORED,
};

/// The page the cursor under test is issued behind: the first call reports this many sets,
/// so the cursor names the one after them and sets remain behind it.
const PAGE: usize = 2;

/// Which wire form a listing call is made on: the two APIs over the account's
/// configuration sets, with the v2 one in both the form its model binds and the form every
/// SDK sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireForm {
    /// The classic `awsQuery` API: `POST /` with `Action=ListConfigurationSets`.
    Classic,
    /// The v2 API as an SDK sends it: `GET` over the collection, paging in the query
    /// string.
    V2Collection,
    /// The v2 API on the route its model binds: `POST /v2/email/list-configuration-sets`,
    /// paging in the body — the only form that can also carry `Filter`.
    V2Modelled,
}

impl WireForm {
    /// Every wire form, so a row can issue its cursor on any of them.
    fn all() -> [Self; 3] {
        [Self::Classic, Self::V2Collection, Self::V2Modelled]
    }

    /// The wire forms in the order a continued walk asks them, starting at the one AFTER
    /// `self`: the page right after the cursor was issued is asked of a different form, and
    /// every form ends up reading a cursor some other form minted.
    fn after(self) -> Vec<Self> {
        let all = Self::all();
        let start = all
            .iter()
            .position(|form| *form == self)
            .expect("every wire form is in `all`");
        (1..=all.len())
            .map(|step| all[(start + step) % all.len()])
            .collect()
    }

    /// The API a row MUTATES the collection through: the other API from the one that issued
    /// the cursor, so the change the cursor outlives was not made by the surface that
    /// minted it.
    fn mutating_surface(self) -> Surface {
        match self {
            Self::Classic => Surface::V2,
            Self::V2Collection | Self::V2Modelled => Surface::V1,
        }
    }
}

/// One page of the account's configuration-set listing, asked on `form`: `(the names it
/// reported, the NextToken it carried)`.
///
/// The two forms the sibling suite asks are its own ([`page`]); the modelled route is this
/// suite's, because it is the form that suite does not cover.
async fn listing_page(
    router: &MultiServiceRouter,
    case: &str,
    form: WireForm,
    page_size: usize,
    token: Option<&str>,
) -> (Vec<String>, Option<String>) {
    match form {
        WireForm::Classic => page(router, case, Surface::V1, page_size, token).await,
        WireForm::V2Collection => page(router, case, Surface::V2, page_size, token).await,
        WireForm::V2Modelled => {
            let mut body = json!({"PageSize": page_size});
            if let Some(token) = token {
                body["NextToken"] = Value::String(token.to_string());
            }
            let answered = v2(
                router,
                case,
                "POST",
                "/v2/email/list-configuration-sets",
                Some(body),
            )
            .await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: SESv2 ListConfigurationSets on its modelled route expected \
                 HTTP 200, got {}: {}",
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
            let next = answered
                .json
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|token| !token.is_empty());
            (names, next)
        }
    }
}

/// Remove the configuration set `name` through `surface`, asserting the delete succeeded.
async fn delete(router: &MultiServiceRouter, case: &str, surface: Surface, name: &str) {
    let answered = match surface {
        Surface::V1 => {
            v1(
                router,
                case,
                "DeleteConfigurationSet",
                &[("ConfigurationSetName", name.to_string())],
            )
            .await
        }
        Surface::V2 => {
            v2(
                router,
                case,
                "DELETE",
                &format!("/v2/email/configuration-sets/{name}"),
                None,
            )
            .await
        }
    };
    assert_eq!(
        answered.status, 200,
        "case {case:?}: the delete of {name:?} on {surface:?} expected HTTP 200, got {}: {}",
        answered.status, answered.text
    );
}

/// Every configuration set the listing reports from `token` on, asking each page of the
/// next wire form in `forms` — so a cursor is read by a form other than the one that
/// minted it.
async fn walk_from(
    router: &MultiServiceRouter,
    case: &str,
    page_size: usize,
    forms: &[WireForm],
    token: Option<String>,
) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut token = token;
    // Bounded above every page count a row here can ask for, so a walk that never
    // terminates fails instead of looping forever.
    for round in 0..1_000 {
        let form = forms[round % forms.len()];
        let (names, next) = listing_page(router, case, form, page_size, token.as_deref()).await;
        assert!(
            names.len() <= page_size,
            "case {case:?} page {round} on {form:?}: expected at most {page_size} names, got \
             {names:?}"
        );
        seen.extend(names);
        match next {
            None => return seen,
            Some(next) => token = Some(next),
        }
    }
    panic!("case {case:?}: the walk never ran out of pages");
}

/// What a row changes in the collection after a cursor has been issued over it and before
/// the next page is asked for.
#[derive(Debug, Clone, Copy)]
enum Disturbance {
    /// Nothing at all: the control row.
    Nothing,
    /// The set the cursor names, so the position itself is gone.
    TheSetAtThePosition,
    /// A set page one already reported, so the collection shrinks BEHIND the cursor.
    TheFirstSet,
    /// The last set of the collection, so the continued walk ends one set earlier.
    TheLastSet,
    /// A set whose name sorts before the position, which the continuation must NOT report.
    ASetInsertedBeforeThePosition,
    /// A set whose name sorts after the position, which the continuation MUST report in
    /// name order.
    ASetInsertedAfterThePosition,
}

impl Disturbance {
    /// Every row.
    fn all() -> [Self; 6] {
        [
            Self::Nothing,
            Self::TheSetAtThePosition,
            Self::TheFirstSet,
            Self::TheLastSet,
            Self::ASetInsertedBeforeThePosition,
            Self::ASetInsertedAfterThePosition,
        ]
    }

    /// Apply the change through `surface` and report the collection as the account then
    /// holds it, in name order.
    ///
    /// An inserted name is derived from the collection rather than written out, so a row
    /// stays "a set before/after the position" however the sets are named: a name one
    /// suffix past an existing set sorts immediately after that set, hence before the next
    /// one. The asserts pin that, so a row cannot silently stop testing the side it names.
    async fn apply(
        self,
        router: &MultiServiceRouter,
        case: &str,
        surface: Surface,
        held: &[String],
        position: &str,
    ) -> Vec<String> {
        let mut held: Vec<String> = held.to_vec();
        match self {
            Self::Nothing => {}
            Self::TheSetAtThePosition => {
                delete(router, case, surface, position).await;
                held.retain(|name| name != position);
            }
            Self::TheFirstSet => {
                let first = held.first().expect("a non-empty collection").clone();
                delete(router, case, surface, &first).await;
                held.retain(|name| *name != first);
            }
            Self::TheLastSet => {
                let last = held.last().expect("a non-empty collection").clone();
                delete(router, case, surface, &last).await;
                held.retain(|name| *name != last);
            }
            Self::ASetInsertedBeforeThePosition => {
                let inserted = format!("{}-1", held[PAGE - 1]);
                assert!(
                    inserted.as_str() < position,
                    "case {case:?}: {inserted:?} must sort before the position {position:?}"
                );
                create(router, case, surface, &inserted).await;
                held.push(inserted);
            }
            Self::ASetInsertedAfterThePosition => {
                let inserted = format!("{position}-1");
                assert!(
                    inserted.as_str() > position,
                    "case {case:?}: {inserted:?} must sort after the position {position:?}"
                );
                create(router, case, surface, &inserted).await;
                held.push(inserted);
            }
        }
        held.sort();
        held
    }
}

#[tokio::test]
async fn a_cursor_resumes_at_its_position_in_the_collection_as_it_then_stands() {
    for disturbance in Disturbance::all() {
        for issuer in WireForm::all() {
            let case =
                format!("{disturbance:?} between a cursor issued on {issuer:?} and its page");
            let (router, held) = emulator_holding_sets(&case).await;

            // Page one, asked of the form under test: its cursor names the set the next
            // page starts at, which is the first one it did NOT report.
            let (first, token) = listing_page(&router, &case, issuer, PAGE, None).await;
            assert_eq!(
                first,
                held[..PAGE].to_vec(),
                "case {case:?}: page one must report the first {PAGE} sets in name order, got \
                 {first:?}"
            );
            let token = token.unwrap_or_else(|| {
                panic!("case {case:?}: a collection larger than the page must carry a NextToken")
            });
            let position = held[PAGE].clone();

            // The collection changes under the cursor, through the OTHER API.
            let held = disturbance
                .apply(
                    &router,
                    &case,
                    issuer.mutating_surface(),
                    &held,
                    &position,
                )
                .await;

            // The rule: the continuation reports exactly the sets the account holds at or
            // after the position, in name order — nothing about how many sets page one
            // reported or how many the account held when the cursor was minted.
            let expected: Vec<String> = held
                .iter()
                .filter(|name| name.as_str() >= position.as_str())
                .cloned()
                .collect();
            let seen = walk_from(&router, &case, PAGE, &issuer.after(), Some(token)).await;
            assert_eq!(
                seen, expected,
                "case {case:?}: a cursor at {position:?} must continue the account's one \
                 collection, got {seen:?}"
            );

            // And the collection itself is whole: the continuation reported a window of it,
            // not a listing of its own.
            let whole = walk_from(&router, &case, PAGE, &WireForm::all(), None).await;
            assert_eq!(
                whole, held,
                "case {case:?}: the account must still report every set it holds, got {whole:?}"
            );
        }
    }
}

#[tokio::test]
async fn every_wire_form_of_the_listing_cuts_one_sequence_at_every_page_size() {
    let case = "one sequence over three wire forms";
    let (router, held) = emulator_holding_sets(case).await;

    // Each row is a page size, from one entry per page to one page larger than the whole
    // collection, walked once on each wire form alone and once with the form changing at
    // every page — so each cursor is read both by the form that minted it and by the others.
    for page_size in [1_usize, 2, 3, STORED, STORED + 1] {
        for form in WireForm::all() {
            let alone = format!("page size {page_size} on {form:?} throughout");
            let seen = walk_from(&router, &alone, page_size, &[form], None).await;
            assert_eq!(
                seen, held,
                "case {alone:?}: expected the account's sets once each in name order, got \
                 {seen:?}"
            );

            // This form, then the other two in turn.
            let rotating = format!("page size {page_size} starting on {form:?}, rotating");
            let mut forms = vec![form];
            forms.extend(form.after().into_iter().take(WireForm::all().len() - 1));
            let seen = walk_from(&router, &rotating, page_size, &forms, None).await;
            assert_eq!(
                seen, held,
                "case {rotating:?}: a cursor must continue the same sequence on whichever \
                 form reads it, got {seen:?}"
            );
        }
    }
}
