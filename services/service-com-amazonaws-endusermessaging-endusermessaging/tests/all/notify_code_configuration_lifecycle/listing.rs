// SPDX-License-Identifier: CC0-1.0

//! `ListNotifyCodeConfigurations`: the notify code configurations this service holds,
//! reported and paged over the wire.
//!
//! Every assertion goes through `MultiServiceRouter::dispatch`, so it exercises the
//! same restJson1 path a real SDK client takes: routing, deserialization, boundary
//! constraint validation, the handler, and error rendering. The configurations are
//! created by `CreateNotifyCodeConfiguration` over that same wire, so the list is
//! read off the state a caller actually wrote.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * the list reports every stored configuration and nothing else, each projected
//!   exactly as `GetNotifyCodeConfiguration` reports the same record, member for
//!   member — for every name, passcode policy and channel parameter set a create
//!   accepts;
//! * every configuration is returned exactly once across pages, in one order, for
//!   every page size, and a `nextToken` is emitted only while configurations remain;
//! * a `maxResults` inside the modelled range is served and one outside it is
//!   refused;
//! * a `nextToken` this service never issued — including one another paginated
//!   operation of this service issued — is `ValidationException` (400);
//! * listing is a read: what a later caller sees is the same before and after.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_endusermessaging_endusermessaging::state::INVALID_PAGE_TOKEN;
use service_com_amazonaws_endusermessaging_endusermessaging::EmulatedService;

/// This service's sigv4 signing name, from the model (`OPERATIONS.md`, "Wire protocol").
const SIGNING_NAME: &str = "end-user-messaging";

/// The Region every request of this suite is signed for.
const REGION: &str = "us-east-1";

/// One response, reduced to what the assertions read.
#[derive(Debug)]
struct Reply {
    status: u16,
    /// The wire error code (`x-amzn-errortype`, else the body's `__type`), with any
    /// `namespace#` prefix stripped. `None` on success.
    error: Option<String>,
    body: Value,
}

impl Reply {
    /// A top-level member of the output body.
    fn member(&self, name: &str) -> &Value {
        &self.body[name]
    }

    /// The `notifyCodeConfigurations` this page carries.
    fn page(&self, case: &str) -> Vec<Value> {
        self.member("notifyCodeConfigurations")
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: notifyCodeConfigurations is not a list, got {:?}",
                    self.body
                )
            })
            .clone()
    }
}

/// One emulator over one shared engine: every call sees the same store.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path_and_query: &str, body: Option<Value>) -> Reply {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/{SIGNING_NAME}/aws4_request, \
             SignedHeaders=host, Signature=00"
        );
        let payload = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("a serializable body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(format!(
                "https://{SIGNING_NAME}.{REGION}.amazonaws.com{path_and_query}"
            ))
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .body(payload)
            .expect("a well-formed test request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_error = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        let error = if status < 400 {
            None
        } else {
            header_error
                .or_else(|| body["__type"].as_str().map(str::to_string))
                .map(|code| {
                    code.rsplit('#')
                        .next()
                        .unwrap_or(code.as_str())
                        .split('/')
                        .next()
                        .unwrap_or(code.as_str())
                        .to_string()
                })
        };
        Reply {
            status,
            error,
            body,
        }
    }

    async fn create(&self, body: Value) -> Reply {
        self.call("POST", "/v1/notify-code-configurations", Some(body))
            .await
    }

    async fn get(&self, id: &str) -> Reply {
        self.call("GET", &format!("/v1/notify-code-configurations/{id}"), None)
            .await
    }

    async fn list(&self, query: &str) -> Reply {
        self.call(
            "GET",
            &format!("/v1/notify-code-configurations{query}"),
            None,
        )
        .await
    }

    /// Create `count` configurations and answer their ids in the order the list
    /// reports them (ascending `notifyCodeConfigurationId`).
    async fn create_sorted(&self, count: usize) -> Vec<String> {
        let mut ids = Vec::new();
        for index in 0..count {
            let created = self
                .create(json!({ "notifyCodeConfigurationName": format!("page-{index}") }))
                .await;
            assert_eq!(
                created.status, 200,
                "case create page-{index}: expected 200, got {} / {:?}",
                created.status, created.body
            );
            ids.push(
                created.body["notifyCodeConfiguration"]["notifyCodeConfigurationId"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case create page-{index}: no id in {:?}", created.body)
                    })
                    .to_string(),
            );
        }
        ids.sort();
        ids
    }
}

/// The create bodies this suite spans: the rows differ in the name
/// (`NotifyCodeConfigurationName`, `@length(1..64)`, pattern
/// `^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$`), in which members of
/// `codeConfigurationParameters` and `channelParameters` are present, in
/// `deletionProtectionEnabled` and in the tag set — every member a listed
/// `NotifyCodeConfiguration` can carry, and its absence.
fn create_bodies() -> Vec<Value> {
    vec![
        json!({ "notifyCodeConfigurationName": "a" }),
        json!({
            "notifyCodeConfigurationName": "signin-otp",
            "codeConfigurationParameters": { "codeType": "NUMERIC", "codeLength": 6 },
        }),
        json!({
            "notifyCodeConfigurationName": "My_Code configuration-1",
            "codeConfigurationParameters": {
                "codeType": "ALPHANUMERIC",
                "codeLength": 8,
                "validityPeriodMinutes": 60,
                "maxAttempts": 5,
            },
            "channelParameters": { "text": { "inlineTemplateBody": "Your code is {{code}}" } },
            "deletionProtectionEnabled": true,
            "tags": [{ "key": "team", "value": "messaging" }],
        }),
        json!({
            "notifyCodeConfigurationName": "N".repeat(64),
            "codeConfigurationParameters": { "codeType": "ALPHA", "codeLength": 4 },
            "channelParameters": {
                "voice": { "inlineTemplateBody": "Your code is {{code}}", "voiceId": "Joanna" },
                "whatsApp": { "whatsAppTemplateName": "otp_template" },
            },
            "deletionProtectionEnabled": false,
        }),
        json!({
            "notifyCodeConfigurationName": "0",
            "channelParameters": { "notify": { "notifyTemplateId": "tmpl-1" } },
        }),
    ]
}

/// One listed item, keyed by its id, for every item across every page.
fn by_id(items: &[Value], case: &str) -> Map<String, Value> {
    let mut indexed = Map::new();
    for item in items {
        let id = item["notifyCodeConfigurationId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: a listed item has no id, got {item:?}"))
            .to_string();
        assert!(
            indexed.insert(id.clone(), item.clone()).is_none(),
            "case {case}: {id} was listed twice"
        );
    }
    indexed
}

/// Walk every page of `query_prefix`, answering the items in the order they were
/// returned. `page_size` bounds each page when the request carries a `maxResults`.
async fn walk(em: &Emulator, page_size: Option<usize>, case: &str, total: usize) -> Vec<Value> {
    let mut seen = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;

    loop {
        let mut query = Vec::new();
        if let Some(size) = page_size {
            query.push(format!("maxResults={size}"));
        }
        if let Some(ref next) = token {
            query.push(format!("nextToken={next}"));
        }
        let query = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };

        let page = em.list(&query).await;
        assert_eq!(
            page.status, 200,
            "case {case}: ListNotifyCodeConfigurations must succeed, got {} / {:?}",
            page.status, page.body
        );
        let items = page.page(case);
        if let Some(size) = page_size {
            assert!(
                items.len() <= size,
                "case {case}: a page holds at most maxResults items, got {}",
                items.len()
            );
        }
        seen.extend(items.iter().cloned());
        pages += 1;
        assert!(
            pages <= total + 1,
            "case {case}: paging did not terminate after {pages} pages"
        );

        match page.member("nextToken").as_str() {
            Some(next) => {
                assert!(
                    !items.is_empty(),
                    "case {case}: a token must not be emitted beside an empty page"
                );
                token = Some(next.to_string());
            }
            None => break,
        }
    }

    seen
}

#[tokio::test]
async fn the_list_reports_every_stored_configuration_as_the_reader_does() {
    let em = Emulator::new();
    let mut created: Vec<Value> = Vec::new();
    for body in create_bodies() {
        let created_reply = em.create(body.clone()).await;
        assert_eq!(
            created_reply.status, 200,
            "case {body:?}: expected 200, got {} / {:?}",
            created_reply.status, created_reply.body
        );
        created.push(created_reply.body["notifyCodeConfiguration"].clone());
    }

    let listed = walk(&em, Some(100), "whole list", created.len()).await;
    assert_eq!(
        listed.len(),
        created.len(),
        "case whole list: expected {} configurations, got {listed:?}",
        created.len()
    );
    let indexed = by_id(&listed, "whole list");

    for resource in &created {
        let id = resource["notifyCodeConfigurationId"]
            .as_str()
            .expect("a created id");
        let case = format!("id={id}");
        let item = indexed
            .get(id)
            .unwrap_or_else(|| panic!("case {case}: the created configuration is not listed"));
        // A list entry and a read of the same record are one projection, so the two
        // agree member for member, absent members included.
        assert_eq!(
            item, resource,
            "case {case}: the listed entry must equal the created one, got {item:?}"
        );
        let read = em.get(id).await;
        assert_eq!(
            read.status, 200,
            "case {case}: the read must succeed, got {} / {:?}",
            read.status, read.body
        );
        assert_eq!(
            item, &read.body["notifyCodeConfiguration"],
            "case {case}: the listed entry must equal what the reader reports, got {item:?}"
        );
    }
}

#[tokio::test]
async fn an_empty_store_lists_nothing_and_offers_no_token() {
    let em = Emulator::new();
    for query in ["", "?maxResults=1", "?maxResults=100"] {
        let case = format!("query={query:?}");
        let reply = em.list(query).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected 200, got {} / {:?}",
            reply.status, reply.body
        );
        assert!(
            reply.page(&case).is_empty(),
            "case {case}: nothing is stored, so nothing is listed, got {:?}",
            reply.body
        );
        assert!(
            reply.member("nextToken").is_null(),
            "case {case}: no token is offered when nothing remains, got {:?}",
            reply.body
        );
    }
}

#[tokio::test]
async fn every_page_size_walks_the_whole_list_exactly_once() {
    const CONFIGURATIONS: usize = 7;
    let em = Emulator::new();
    let expected = em.create_sorted(CONFIGURATIONS).await;

    // Each row is a page size: both published bounds of `maxResults` (1 and 100), an
    // interior size that divides the set, one that does not, exactly the set's own
    // size, and no `maxResults` at all.
    for page_size in [Some(1), Some(2), Some(3), Some(7), Some(100), None] {
        let case = format!("maxResults={page_size:?}");
        let seen = walk(&em, page_size, &case, CONFIGURATIONS).await;
        let ids: Vec<String> = seen
            .iter()
            .map(|item| {
                item["notifyCodeConfigurationId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case}: an item has no id, got {item:?}"))
                    .to_string()
            })
            .collect();
        assert_eq!(
            ids, expected,
            "case {case}: paging must yield every configuration exactly once, in id order"
        );
    }
}

#[tokio::test]
async fn max_results_is_served_inside_its_modelled_range_and_refused_outside_it() {
    let em = Emulator::new();
    em.create_sorted(3).await;

    // `maxResults` targets `MaxResults`, modelled `@range(1..100)`: each published
    // bound is served, each value one step past a bound is refused.
    for accepted in [1, 2, 99, 100] {
        let case = format!("maxResults={accepted}");
        let reply = em.list(&format!("?maxResults={accepted}")).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: a value inside the modelled range must be served, got {} / {:?}",
            reply.status, reply.body
        );
        assert!(
            reply.page(&case).len() <= accepted,
            "case {case}: a page holds at most maxResults items, got {:?}",
            reply.body
        );
    }

    for refused in [0, 101, -1] {
        let case = format!("maxResults={refused}");
        let reply = em.list(&format!("?maxResults={refused}")).await;
        assert_eq!(
            reply.status, 400,
            "case {case}: expected 400, got {} / {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ValidationException"),
            "case {case}: unexpected error code, got {:?}",
            reply.body
        );
    }
}

#[tokio::test]
async fn a_token_this_operation_never_issued_is_refused() {
    let em = Emulator::new();
    em.create_sorted(3).await;

    // A token another paginated operation of this service issued: well formed, and
    // still not a resume point for this list.
    let created_profile = em
        .call(
            "POST",
            "/v1/brand-profiles",
            Some(json!({ "brandProfileName": "paging" })),
        )
        .await;
    assert_eq!(
        created_profile.status, 200,
        "a brand profile is needed to borrow a foreign token, got {} / {:?}",
        created_profile.status, created_profile.body
    );
    em.call(
        "POST",
        "/v1/brand-profiles",
        Some(json!({ "brandProfileName": "paging-2" })),
    )
    .await;
    let foreign = em.call("GET", "/v1/brand-profiles?maxResults=1", None).await;
    let foreign_token = foreign.body["nextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("ListBrandProfiles must offer a token, got {:?}", foreign.body))
        .to_string();

    // Each row is a value the service never issued as a continuation of this list:
    // plain text, hex of a resume point with no marker, hex of a marker this
    // operation does not use, a token of another operation of this service, and a
    // value at `NextToken`'s modelled maximum length.
    for token in [
        "not-a-token".to_string(),
        hex::encode("ncc-0000000000000001"),
        hex::encode("ListNotifyCodeConfigurations/1/"),
        foreign_token,
        "f".repeat(2048),
    ] {
        let case = format!("nextToken={token:?}");
        let reply = em.list(&format!("?nextToken={token}")).await;
        assert_eq!(
            reply.status, 400,
            "case {case}: expected 400, got {} / {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error.as_deref(),
            Some("ValidationException"),
            "case {case}: unexpected error code, got {:?}",
            reply.body
        );
        assert_eq!(
            reply.body["message"].as_str(),
            Some(INVALID_PAGE_TOKEN),
            "case {case}: unexpected refusal wording, got {:?}",
            reply.body
        );
    }
}

#[tokio::test]
async fn listing_does_not_change_what_a_later_caller_sees() {
    let em = Emulator::new();
    let ids = em.create_sorted(4).await;
    let before: Vec<Value> = {
        let mut reads = Vec::new();
        for id in &ids {
            reads.push(em.get(id).await.body["notifyCodeConfiguration"].clone());
        }
        reads
    };

    // Read the list every way this operation can be asked for it.
    for query in ["", "?maxResults=1", "?maxResults=2", "?maxResults=100"] {
        let case = format!("query={query:?}");
        let first = em.list(query).await;
        let second = em.list(query).await;
        assert_eq!(
            first.body, second.body,
            "case {case}: two identical list calls must answer the same, got {:?} then {:?}",
            first.body, second.body
        );
    }

    for (id, expected) in ids.iter().zip(before.iter()) {
        let case = format!("id={id}");
        let read = em.get(id).await;
        assert_eq!(
            &read.body["notifyCodeConfiguration"],
            expected,
            "case {case}: a read after listing must report what it did before, got {:?}",
            read.body
        );
    }
}
