// SPDX-License-Identifier: CC0-1.0

//! The one page bound every list of this service serves its page under, end to end over
//! HTTP.
//!
//! The rule under test, which all four list operations implement: the number of records a
//! page carries is the `maxResults` the request named, and — when it names none — the
//! largest page that operation's `maxResults` shape admits, so a request naming no page
//! size is served the same bound whichever list it asks.
//!
//! `smithy_query(query="operation", operation="ListComponents,ListForms,ListThemes,\
//! ListCodegenJobs")` and `smithy_query(query="shape",
//! shape="ListEntityLimit,ListCodegenJobsLimit")` are where that comes from: all four are
//! `@paginated(pageSize: maxResults, items: entities)`, the entity lists bind `maxResults`
//! to `ListEntityLimit` and the codegen-job list to `ListCodegenJobsLimit`, and both
//! shapes are `@range(min: 1, max: 100)` — the codegen-job member with `@default(100)`,
//! the other three with no default at all.
//!
//! Each row drives one list, so the bound is exercised from every operation that derives
//! one — the two the derivation was written out in (`ListComponents` and `ListThemes`) and
//! the two that now share it. Every case goes through the router: routing,
//! deserialization, the handler, the persisted state and serialization.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_amplifyuibuilder_amplifyuibuilder::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 credential scope naming this service: it is what routes a request here.
const SIGNED_FOR_AMPLIFYUIBUILDER: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/amplifyuibuilder/aws4_request, \
     SignedHeaders=host, Signature=abc";

/// The app environment the cases address, and a second one whose records are no part of
/// its pages. `AppId` is `@length(min: 1, max: 20)` with pattern `^d[a-z0-9]+$`.
const APP_ID: &str = "d1234abcd";
const ENVIRONMENT: &str = "staging";
const OTHER_ENVIRONMENT: &str = "production";

/// The published bounds of `ListEntityLimit` and of `ListCodegenJobsLimit`, both
/// `@range(min: 1, max: 100)`. They are spelled out here rather than imported so a service
/// that serves a different default page size, or admits a value the model refuses, fails
/// this test.
const MAX_RESULTS_FLOOR: i32 = 1;
const MAX_RESULTS_CEILING: i32 = 100;

/// How many records each row creates in the addressed app environment: one more than the
/// largest page `maxResults` admits, so a request naming no page size cannot serve the
/// whole list and the bound it fell back to is visible in the length of its first page.
const RECORDS: usize = MAX_RESULTS_CEILING as usize + 1;

/// How many records another app environment of the same app holds, so a page of the
/// addressed one is bounded over its own records rather than over the store.
const RECORDS_ELSEWHERE: usize = 2;

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The ids the `entities` of this answer carry, in the order they are reported.
    fn entity_ids(&self) -> Vec<String> {
        self.body["entities"]
            .as_array()
            .map(|entities| {
                entities
                    .iter()
                    .map(|entity| entity["id"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the answer carries an `entities` list at all.
    fn has_entities(&self) -> bool {
        self.body["entities"].is_array()
    }

    /// The `nextToken` the answer carries, if any.
    fn next_token(&self) -> Option<String> {
        self.body["nextToken"].as_str().map(str::to_string)
    }

    /// The `id` a create answered.
    fn id(&self) -> String {
        self.body["id"]
            .as_str()
            .unwrap_or_else(|| panic!("a created record must carry an id, got {:?}", self.body))
            .to_string()
    }
}

async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "amplifyuibuilder.eu-west-1.amazonaws.com")
        .header("authorization", SIGNED_FOR_AMPLIFYUIBUILDER);
    if !payload.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

/// One list operation of this service: the collection it pages, and whether its
/// `maxResults` carries a `@default` the request walk fills in.
#[derive(Clone, Copy, Debug)]
struct Listed {
    operation: &'static str,
    /// The last path segment of the collection, as this service's `@http` URIs spell it.
    segment: &'static str,
}

/// Every operation of this crate that derives a page bound from `maxResults`, one row
/// each: the two lists the derivation was written out in, and the two that share it too.
fn lists() -> Vec<Listed> {
    vec![
        Listed {
            operation: "ListComponents",
            segment: "components",
        },
        Listed {
            operation: "ListThemes",
            segment: "themes",
        },
        Listed {
            operation: "ListForms",
            segment: "forms",
        },
        Listed {
            operation: "ListCodegenJobs",
            segment: "codegen-jobs",
        },
    ]
}

impl Listed {
    /// Where this collection of an app environment is created and listed.
    fn uri(&self, environment: &str) -> String {
        format!(
            "/app/{APP_ID}/environment/{environment}/{segment}",
            segment = self.segment
        )
    }

    /// The smallest payload the create of this collection admits — the members its request
    /// shape requires, which is all a case about the page bound needs.
    fn create_payload(&self, index: usize) -> Value {
        let name = format!("Bound-{index}");
        match self.segment {
            "components" => json!({
                "name": name,
                "componentType": "View",
                "properties": {},
                "variants": [],
                "overrides": {},
                "bindingProperties": {},
            }),
            "forms" => json!({
                "name": name,
                "formActionType": "create",
                "dataType": {"dataSourceType": "DataStore", "dataTypeName": "Post"},
                "fields": {},
                "style": {},
                "sectionalElements": {},
                "schemaVersion": "1.0",
            }),
            "themes" => json!({"name": name, "values": []}),
            // `StartCodegenJobData` requires only `renderConfig`, a union with one variant.
            _ => json!({"renderConfig": {"react": {}}}),
        }
    }

    /// Create one record of this collection in an app environment and answer its id.
    async fn create(&self, router: &MultiServiceRouter, environment: &str, index: usize) -> String {
        let answer = call(
            router,
            "POST",
            &self.uri(environment),
            Some(self.create_payload(index)),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {}: creating a record in {environment} must answer HTTP 200, got {} with {:?}",
            self.operation, answer.status, answer.body
        );
        answer.id()
    }

    /// Ask this list for one page under `max_results`, resumed at `token` when one is given.
    async fn page(
        &self,
        router: &MultiServiceRouter,
        max_results: Option<i32>,
        token: Option<&str>,
    ) -> Answer {
        let mut query: Vec<String> = Vec::new();
        if let Some(size) = max_results {
            query.push(format!("maxResults={size}"));
        }
        if let Some(token) = token {
            query.push(format!("nextToken={token}"));
        }
        let base = self.uri(ENVIRONMENT);
        let uri = if query.is_empty() {
            base
        } else {
            format!("{base}?{}", query.join("&"))
        };
        call(router, "GET", &uri, None).await
    }

    /// Every id a complete walk of this list under `max_results` reports, with the length
    /// of each page it took.
    async fn walk(
        &self,
        router: &MultiServiceRouter,
        max_results: Option<i32>,
    ) -> (Vec<String>, Vec<usize>) {
        let mut ids: Vec<String> = Vec::new();
        let mut pages: Vec<usize> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let answer = self.page(router, max_results, token.as_deref()).await;
            assert_eq!(
                answer.status,
                200,
                "case {} maxResults={max_results:?} page {}: a page must be served, got {} with \
                 {:?}",
                self.operation,
                pages.len(),
                answer.status,
                answer.body
            );
            let page = answer.entity_ids();
            pages.push(page.len());
            ids.extend(page);
            match answer.next_token() {
                Some(next) => token = Some(next),
                None => break,
            }
            assert!(
                pages.len() <= RECORDS + 1,
                "case {} maxResults={max_results:?}: a walk of {RECORDS} records must end within \
                 {} pages, took page lengths {pages:?}",
                self.operation,
                RECORDS + 1
            );
        }
        (ids, pages)
    }
}

/// The page sizes the rows are walked under: both published bounds of `maxResults`, two
/// sizes inside the range that cut the records into a different number of pages, and no
/// `maxResults` at all — which falls back to the operation's own ceiling.
fn page_sizes() -> Vec<Option<i32>> {
    vec![
        None,
        Some(MAX_RESULTS_FLOOR),
        Some(3),
        Some(50),
        Some(MAX_RESULTS_CEILING),
    ]
}

/// Every list of this service bounds its page the same way: at the `maxResults` the
/// request named, and at the largest page its `maxResults` shape admits when the request
/// names none — so a request naming no page size is served one page of exactly that many
/// records whichever list it asks, and walking the pages still enumerates every record of
/// the addressed app environment exactly once.
#[tokio::test]
async fn every_list_serves_the_same_page_bound() {
    let mut default_page_lengths: Vec<(&'static str, usize)> = Vec::new();

    for row in lists() {
        // A fresh store per row, so the records of the addressed app environment are
        // exactly the ones this row created, in creation order.
        let router = emulator();
        let mut created: Vec<String> = Vec::new();
        for index in 0..RECORDS {
            created.push(row.create(&router, ENVIRONMENT, index).await);
        }
        for index in 0..RECORDS_ELSEWHERE {
            row.create(&router, OTHER_ENVIRONMENT, index).await;
        }

        for max_results in page_sizes() {
            // The bound the rule derives: what the request asked for, or the ceiling of
            // the modelled range when it asked for nothing.
            let bound = max_results.unwrap_or(MAX_RESULTS_CEILING) as usize;

            let (walked, pages) = row.walk(&router, max_results).await;
            assert_eq!(
                walked, created,
                "case {} maxResults={max_results:?}: walking every page must enumerate each \
                 record of the addressed app environment once, in creation order, got {walked:?}",
                row.operation
            );
            for (index, length) in pages.iter().enumerate() {
                assert!(
                    *length <= bound,
                    "case {} maxResults={max_results:?} page {index}: a page must carry at most \
                     the {bound} records the bound allows, got {length}",
                    row.operation
                );
            }
            assert_eq!(
                pages.first().copied(),
                Some(bound.min(RECORDS)),
                "case {} maxResults={max_results:?}: the first page must carry the full bound of \
                 {} records, got page lengths {pages:?}",
                row.operation,
                bound.min(RECORDS)
            );
            let expected_pages = RECORDS.div_ceil(bound);
            assert_eq!(
                pages.len(),
                expected_pages,
                "case {} maxResults={max_results:?}: {RECORDS} records under a bound of {bound} \
                 must take {expected_pages} page(s), got page lengths {pages:?}",
                row.operation
            );
        }

        // Naming no `maxResults` serves the same page as naming the ceiling: the fallback
        // IS that ceiling, not some smaller size of this operation's own.
        let absent = row.page(&router, None, None).await;
        let at_the_ceiling = row.page(&router, Some(MAX_RESULTS_CEILING), None).await;
        assert_eq!(
            absent.entity_ids(),
            at_the_ceiling.entity_ids(),
            "case {}: a request naming no maxResults must be served the same page as one naming \
             the ceiling {MAX_RESULTS_CEILING}, got {} records against {}",
            row.operation,
            absent.entity_ids().len(),
            at_the_ceiling.entity_ids().len()
        );
        assert!(
            absent.next_token().is_some(),
            "case {}: with {RECORDS} records stored, a page of the fallback bound must carry a \
             nextToken, got {:?}",
            row.operation,
            absent.body
        );
        default_page_lengths.push((row.operation, absent.entity_ids().len()));

        // One past each published bound is refused at the boundary rather than corrected
        // to a page size: the derivation never admits a value outside the modelled range.
        for max_results in [MAX_RESULTS_FLOOR - 1, MAX_RESULTS_CEILING + 1] {
            let refused = row.page(&router, Some(max_results), None).await;
            assert_eq!(
                refused.status, 400,
                "case {} maxResults={max_results}: a value outside @range(1, 100) must be \
                 refused, got {} with {:?}",
                row.operation, refused.status, refused.body
            );
            assert!(
                !refused.has_entities(),
                "case {} maxResults={max_results}: a refusal must carry no page, got {:?}",
                row.operation, refused.body
            );
        }
    }

    // The fallback is ONE value for the whole service: every list served the same number
    // of records to a request that named no page size.
    let expected = MAX_RESULTS_CEILING as usize;
    for (operation, length) in &default_page_lengths {
        assert_eq!(
            *length, expected,
            "case {operation}: a request naming no maxResults must be served {expected} records, \
             got {length} (every list: {default_page_lengths:?})"
        );
    }
}
