// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `ListEngineVersions`: the engine versions Athena publishes as
//! available to choose from, driven over the wire (awsJson1_1, `x-amz-target:
//! AmazonAthena.<Operation>`) through the same router the emulator boots, so every
//! assertion is about what an SDK client observes.
//!
//! The rule under test: the listing reports the service's catalog of selectable engine
//! versions — one entry per version a request may select, including the Auto option — each
//! pairing that selection with the version a query selecting it then runs on, in pages of
//! at most `MaxResults` entries, each page but the last followed by a `NextToken` this
//! service minted and no other value. The catalog belongs to the service, not to the
//! account: no request adds to it or takes from it, so two listings report the same
//! entries, and every version it publishes is one a workgroup may be created with and
//! then reports as its own.
//!
//! Each rule is asserted over a TABLE rather than one request: the rows differ along what
//! the rule generalises over — which published version is selected, the page size asked
//! for (every size the model allows, none, and one past the catalog), and the form of a
//! continuation token a caller sends back.

use serde_json::{json, Value};
use service_com_amazonaws_athena_amazonathena::state::{
    page_token_malformed, DEFAULT_EFFECTIVE_ENGINE_VERSION, ENGINE_VERSION_AUTO,
    ERROR_CODE_INVALID_INPUT, PYSPARK_ENGINE_VERSION_PREFIX, SELECTABLE_ENGINE_VERSIONS,
};
use service_common::page_tokens::AUTHENTICATOR_BYTES;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, error_code, message, router};

/// The largest page `MaxEngineVersionsCount` (range 1..10) allows, which is also the page
/// a request naming no size gets and the most `EngineVersionsList` (len 0..10) can carry.
const MAX_PAGE: usize = 10;

/// The entries a `ListEngineVersions` answer carries.
fn entries(listed: &Value) -> Vec<Value> {
    listed["EngineVersions"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("a listing must carry EngineVersions, got {listed:?}"))
}

/// The `SelectedEngineVersion` of one entry.
fn selected(entry: &Value) -> String {
    entry["SelectedEngineVersion"]
        .as_str()
        .unwrap_or_else(|| panic!("every entry names the version selected, got {entry:?}"))
        .to_string()
}

/// The `EffectiveEngineVersion` of one entry.
fn effective(entry: &Value) -> String {
    entry["EffectiveEngineVersion"]
        .as_str()
        .unwrap_or_else(|| panic!("every entry names the version it runs on, got {entry:?}"))
        .to_string()
}

/// The versions a listing reported selecting, in the order it reported them.
fn selections(listed: &Value) -> Vec<String> {
    entries(listed).iter().map(selected).collect()
}

/// The `NextToken` a page handed back, if it handed one back.
fn next_token(listed: &Value) -> Option<String> {
    listed["NextToken"].as_str().map(str::to_string)
}

/// One `ListEngineVersions` call that must be accepted.
async fn list(router: &MultiServiceRouter, request: &Value) -> Value {
    let (status, listed) = call(router, "ListEngineVersions", request).await;
    assert_eq!(
        status, 200,
        "ListEngineVersions({request}) must answer 200, got {status} with {listed:?}"
    );
    listed
}

/// The whole catalog in one call: every selectable version, each with the version it
/// resolves to, and nothing a caller has to page for.
#[tokio::test]
async fn the_listing_publishes_every_selectable_engine_version_with_its_effective_one() {
    let router = router();
    let listed = list(&router, &json!({})).await;

    let published = selections(&listed);
    let expected: Vec<String> = SELECTABLE_ENGINE_VERSIONS
        .iter()
        .map(|version| (*version).to_string())
        .collect();
    assert_eq!(
        published, expected,
        "expected the catalog of selectable engine versions {expected:?}, got {published:?}"
    );
    assert!(
        published.len() <= MAX_PAGE,
        "EngineVersionsList holds at most {MAX_PAGE} entries, got {published:?}"
    );
    assert_eq!(
        next_token(&listed),
        None,
        "a listing that reported everything hands back no token, got {listed:?}"
    );

    // The rule for each entry: the Auto option is the one Athena resolves itself, and any
    // other selection runs on the very version it names.
    for entry in entries(&listed) {
        let case = selected(&entry);
        let expected_effective = if case == ENGINE_VERSION_AUTO {
            DEFAULT_EFFECTIVE_ENGINE_VERSION.to_string()
        } else {
            case.clone()
        };
        assert_eq!(
            effective(&entry),
            expected_effective,
            "case {case:?}: expected the effective version {expected_effective:?}, got \
             {entry:?}"
        );
    }

    // The two choices the catalog has to offer whatever else is in it: the Auto option
    // `ListEngineVersions` documents it includes, and one version of each engine family —
    // Athena SQL and PySpark, the families a workgroup's selection picks between.
    assert!(
        published.iter().any(|version| version == ENGINE_VERSION_AUTO),
        "the catalog includes the Auto option, got {published:?}"
    );
    assert!(
        published
            .iter()
            .any(|version| version == DEFAULT_EFFECTIVE_ENGINE_VERSION),
        "the catalog includes the Athena SQL engine version {DEFAULT_EFFECTIVE_ENGINE_VERSION:?}, \
         got {published:?}"
    );
    assert!(
        published
            .iter()
            .any(|version| version.starts_with(PYSPARK_ENGINE_VERSION_PREFIX)),
        "the catalog includes a {PYSPARK_ENGINE_VERSION_PREFIX:?}, got {published:?}"
    );

    // A READ, of a catalog no request owns: a second listing sees exactly what the first
    // one did, and so does one made after the account has been written to.
    let again = list(&router, &json!({})).await;
    assert_eq!(
        again, listed,
        "two listings must publish the same engine versions, got {again:?} after {listed:?}"
    );
    crate::harness::create_work_group(&router, "engine-version-suite").await;
    let after_a_write = list(&router, &json!({})).await;
    assert_eq!(
        after_a_write, listed,
        "the catalog is the service's own, so a write to the account leaves it alone, got \
         {after_a_write:?} after {listed:?}"
    );
}

/// A paged walk reports every published version exactly once, in pages of at most the
/// size asked for, and hands back a token exactly while versions remain behind the page.
#[tokio::test]
async fn a_paged_listing_walks_every_engine_version_exactly_once() {
    let router = router();
    let expected: Vec<String> = selections(&list(&router, &json!({})).await);
    let total = expected.len();

    // Each row is a page size: every size `MaxEngineVersionsCount` allows (1..10), which
    // spans sizes below the catalog, at it and past it, plus no size at all — the page a
    // request naming none gets.
    let sizes: Vec<Option<usize>> = (1..=MAX_PAGE).map(Some).chain([None]).collect();
    for case in sizes {
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0_usize;
        loop {
            let mut request = json!({});
            if let Some(size) = case {
                request["MaxResults"] = json!(size);
            }
            if let Some(token) = token.as_deref() {
                request["NextToken"] = json!(token);
            }
            let listed = list(&router, &request).await;
            pages += 1;
            assert!(
                pages <= total + 1,
                "case {case:?}: a token walk must terminate, walked {walked:?}"
            );

            let page = selections(&listed);
            let page_size = case.unwrap_or(MAX_PAGE);
            let remaining = total - walked.len();
            assert_eq!(
                page.len(),
                page_size.min(remaining),
                "case {case:?}: expected a page of {} after {walked:?}, got {page:?}",
                page_size.min(remaining)
            );
            walked.extend(page);

            token = next_token(&listed);
            let more = walked.len() < total;
            assert_eq!(
                token.is_some(),
                more,
                "case {case:?}: expected a NextToken to be present={more} after {walked:?}, \
                 got {listed:?}"
            );
            if token.is_none() {
                break;
            }
        }

        assert_eq!(
            walked, expected,
            "case {case:?}: expected the walk to report every version once, in catalog order: \
             {expected:?}, got {walked:?}"
        );
        assert_eq!(
            pages,
            total.div_ceil(case.unwrap_or(MAX_PAGE)),
            "case {case:?}: expected the walk to take {} pages, took {pages}",
            total.div_ceil(case.unwrap_or(MAX_PAGE))
        );
    }
}

/// A `NextToken` is "a token generated by the Athena service": a value this service did
/// not mint for THIS listing names no page, and is refused rather than answered with one.
#[tokio::test]
async fn a_next_token_this_service_did_not_mint_names_no_page() {
    let account = router();

    // A token another store minted for the same listing: another account's registries, so
    // another secret.
    let other_account = router();
    let other_store_token = next_token(&list(&other_account, &json!({ "MaxResults": 1 })).await)
        .expect("a page of one of several engine versions hands back a token");

    // A token THIS store minted for a DIFFERENT listing: the account's workgroups, which
    // resume at a position in another sequence entirely.
    let work_groups_token = {
        for index in 0..2 {
            crate::harness::create_work_group(&account, &format!("token-case-{index}")).await;
        }
        let (status, listed) = call(&account, "ListWorkGroups", &json!({ "MaxResults": 1 })).await;
        assert_eq!(
            status, 200,
            "the workgroup listing must be readable, got {status} with {listed:?}"
        );
        next_token(&listed).expect("a page of one of three workgroups hands back a token")
    };

    // (case, the token sent)
    for (case, token) in [
        ("a value that is not a token at all", "made-up-token".to_string()),
        ("an empty-looking hex value", "00".to_string()),
        (
            "the shape of a minted token with an authenticator nobody computed",
            ::hex::encode(vec![0_u8; AUTHENTICATOR_BYTES + 16]),
        ),
        ("a token another store minted for this listing", other_store_token),
        ("a token this store minted for another listing", work_groups_token),
    ] {
        let (status, refused) = call(
            &account,
            "ListEngineVersions",
            &json!({ "MaxResults": 2, "NextToken": token }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            message(&refused),
            page_token_malformed(&token),
            "case {case:?}: expected the sentence {:?}, got {refused:?}",
            page_token_malformed(&token)
        );
        assert_eq!(
            refused["AthenaErrorCode"].as_str(),
            Some(ERROR_CODE_INVALID_INPUT),
            "case {case:?}: expected AthenaErrorCode {ERROR_CODE_INVALID_INPUT:?}, got \
             {refused:?}"
        );
    }

    // The refusals left the listing itself alone.
    let listed = list(&account, &json!({})).await;
    assert_eq!(
        selections(&listed),
        SELECTABLE_ENGINE_VERSIONS
            .iter()
            .map(|version| (*version).to_string())
            .collect::<Vec<String>>(),
        "expected the whole catalog after the refusals, got {listed:?}"
    );
}

/// Every version the listing publishes is one a workgroup may select, and a workgroup
/// that selects it reports the effective version the listing published for it — read back
/// through `GetWorkGroup`, so the claim is about persisted state and not about the create
/// call's own answer.
#[tokio::test]
async fn every_published_version_is_one_a_work_group_can_select() {
    let router = router();
    let listed = list(&router, &json!({})).await;

    for (index, entry) in entries(&listed).into_iter().enumerate() {
        let case = selected(&entry);
        let name = format!("selects-version-{index}");
        let request = json!({
            "Name": name,
            "Configuration": { "EngineVersion": { "SelectedEngineVersion": case } },
        });
        let (status, created) = call(&router, "CreateWorkGroup", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateWorkGroup({request}) must answer 200, got {status} with \
             {created:?}"
        );

        let (status, read) = call(&router, "GetWorkGroup", &json!({ "WorkGroup": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetWorkGroup({name}) must answer 200, got {status} with {read:?}"
        );
        let version = &read["WorkGroup"]["Configuration"]["EngineVersion"];
        assert_eq!(
            version["SelectedEngineVersion"].as_str(),
            Some(case.as_str()),
            "case {case:?}: expected the workgroup to report the version it selected, got \
             {read:?}"
        );
        assert_eq!(
            version["EffectiveEngineVersion"].as_str(),
            Some(effective(&entry).as_str()),
            "case {case:?}: expected the effective version the listing published, {:?}, got \
             {read:?}",
            effective(&entry)
        );
    }
}
