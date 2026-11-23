// SPDX-License-Identifier: CC0-1.0

//! The Control Catalog serves a common control's PUBLISHED paragraph verbatim.
//!
//! The rule under test: **the Control Catalog is an Amazon Web Services-owned PUBLICATION,
//! so a library entry carries the published `Description` TEXT — not a paraphrase of its
//! subject — wherever the library is read**. `CommonControlSummary$Description` is
//! `@required` and documented as the description of the common control
//! (`smithy_query(query="shape", shape="CommonControlSummary",
//! model_path="services/service-com-amazonaws-controlcatalog-controlcatalog/smithy-model/com-amazonaws-controlcatalog-controlcatalog.json")`),
//! and `ListCommonControls`'s filter only "narrows the results to a specific objective"
//! while `MaxResults`/`NextToken` only page what is published — none of the three changes
//! what an entry says.
//!
//! The owning crate's `tests/all/published_common_control_identity.rs` pins the published
//! ARN SET and names an objective filter enumerates, and asserts only that each entry
//! carries SOME description; the paragraph itself is what this suite pins, over what the
//! rule generalises over: the entry (one row per published paragraph this project holds),
//! the listing the entry is read through (narrowed to its objective, and the whole library
//! unfiltered), and the page size the listing is walked with (its own page, one entry per
//! page so the token is resumed, and a size that splits the set unevenly).
//!
//! This suite lives here, beside the other suites of the assembled emulator, because the
//! owning crate declares its suites in a hand-written `tests/all.rs`, which a fix may not
//! edit; the router's `tests/all/` picks a new suite up with nothing to declare. Every call
//! goes through a router with the owning service mounted on one engine, exactly as the
//! production router mounts it, so each assertion exercises the real wire path.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header naming `controlcatalog`: the credential scope the router
/// picks the claiming service by. The signature is never verified here.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20240101/us-east-1/controlcatalog/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// Every page size a listing is walked with: its own page, one entry at a time (so a
/// filter's `NextToken` is resumed), and a size that splits the larger sets unevenly.
const PAGE_SIZES: [Option<u32>; 3] = [None, Some(1), Some(3)];

/// One common control whose published paragraph this project holds: `(published identifier,
/// the ARN of the objective it is published under, the published paragraph)`.
///
/// Source: the Control Catalog's own published common-control library, which names each
/// entry `arn:{PARTITION}:controlcatalog:::common-control/{identifier}` and publishes that
/// paragraph for it (the table `src/state/published_ontology.rs` seeds, whose header carries
/// that provenance and says which entries' wording this project holds). The rows are the
/// `Asset lifecycle management` set; the entry of that set whose published wording this
/// project does NOT hold is absent here, because no paragraph is reconstructed.
const PUBLISHED_PARAGRAPHS: &[(&str, &str, &str)] = &[
    (
        "aaaq1vhzpslfrk1o4hvgk3ti4",
        "5ve4jodybrg8wnky75fp50sbf",
        "Assess potential security and compliance risks as part of the asset acquisition \
         process.",
    ),
    (
        "eboqkjlzo44e0ia5wba1770tw",
        "5ve4jodybrg8wnky75fp50sbf",
        "Configure assets securely prior to deployment in the environment. Confirm the \
         proper integration of new assets with existing systems.",
    ),
    (
        "bme9uq29isv0jdrdy4yj2c702",
        "5ve4jodybrg8wnky75fp50sbf",
        "Retire and dispose of assets, including media, that are no longer needed. Securely \
         erase or destroy any sensitive data before asset disposal.",
    ),
];

/// A router with the Control Catalog mounted over one engine — one emulator session.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_controlcatalog_controlcatalog::EmulatedService::new(engine),
    );
    router
}

/// The ARN the catalog names an entry of `kind` and `identifier` by.
fn catalog_arn(kind: &str, identifier: &str) -> String {
    format!("arn:aws:controlcatalog:::{kind}/{identifier}")
}

/// POST one list request and require the modeled 200.
async fn list_page(router: &MultiServiceRouter, uri: &str, body: &str, case: &str) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("http://localhost{uri}"))
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case}: expected 200 from {uri} with {body}, got {status} and {parsed}"
    );
    parsed
}

/// Walk `/common-controls` to its end, following `NextToken`, and return every entry it
/// served, keyed by `Arn`.
async fn common_controls(
    router: &MultiServiceRouter,
    page_size: Option<u32>,
    body: &str,
    case: &str,
) -> BTreeMap<String, Value> {
    /// More pages than the published library could ever hold at one entry per page: a token
    /// that failed to advance would otherwise loop forever.
    const PAGE_LIMIT: usize = 2_000;

    let mut served: BTreeMap<String, Value> = BTreeMap::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        let mut query: Vec<String> = Vec::new();
        if let Some(size) = page_size {
            query.push(format!("maxResults={size}"));
        }
        if let Some(token) = token.as_deref() {
            query.push(format!("nextToken={token}"));
        }
        let uri = if query.is_empty() {
            "/common-controls".to_string()
        } else {
            format!("/common-controls?{}", query.join("&"))
        };
        let page = list_page(router, &uri, body, case).await;
        let items = page
            .get("CommonControls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case}: {uri} must answer a CommonControls list, got {page}")
            });
        for item in items {
            let arn = item
                .get("Arn")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case}: every entry carries its ARN, got {item}"))
                .to_string();
            served.insert(arn, item);
        }
        pages += 1;
        assert!(
            pages <= PAGE_LIMIT,
            "case {case}: {uri} did not terminate within {PAGE_LIMIT} pages"
        );
        token = page
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        match token.as_deref() {
            // The token is spliced into the query string above, so a spelling that needed
            // percent-encoding would silently turn a resumed walk into a first-page-only
            // one: it is checked here rather than assumed.
            Some(spelling) => assert!(
                !spelling.is_empty()
                    && spelling.chars().all(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '~')
                    }),
                "case {case}: a pagination token must be safe to carry in a query string, \
                 got {spelling:?}"
            ),
            None => return served,
        }
    }
}

/// Every published paragraph is served verbatim, through the entry's objective filter and
/// through the unfiltered library, at every page size.
#[tokio::test]
async fn every_published_common_control_paragraph_is_served_verbatim() {
    let router = emulator();

    for page_size in PAGE_SIZES {
        // The whole library, walked once per page size: the filtered read below must agree
        // with it entry for entry.
        let case = format!("library maxResults={page_size:?}");
        let whole = common_controls(&router, page_size, "{}", &case).await;
        assert!(
            !whole.is_empty(),
            "case {case}: the catalog must publish its common controls"
        );

        for (identifier, objective_identifier, published) in PUBLISHED_PARAGRAPHS {
            let arn = catalog_arn("common-control", identifier);
            let objective_arn = catalog_arn("objective", objective_identifier);
            let filter = format!(
                r#"{{"CommonControlFilter":{{"Objectives":[{{"Arn":"{objective_arn}"}}]}}}}"#
            );
            let case = format!("{identifier} maxResults={page_size:?}");
            let filtered = common_controls(&router, page_size, &filter, &case).await;

            for (listing, served) in [("objective filter", &filtered), ("unfiltered", &whole)] {
                let case = format!("{case} {listing}");
                let entry = served.get(&arn).unwrap_or_else(|| {
                    panic!(
                        "case {case}: the library must publish {arn}, got {:?}",
                        served.keys()
                    )
                });
                let description = entry
                    .get("Description")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case}: Description is required, got {entry}")
                    });
                assert_eq!(
                    description, *published,
                    "case {case}: expected the catalog's published paragraph \
                     {published:?}, got {description:?}"
                );
            }

            assert_eq!(
                filtered.get(&arn),
                whole.get(&arn),
                "case {case}: the filtered and the unfiltered listing must report one record"
            );
        }
    }
}
