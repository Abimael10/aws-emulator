// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DeleteContent`, driven over HTTP through the same router the
//! production emulator mounts.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one recorded
//! request:
//!
//! * a delete of a content the knowledge base holds FORGETS that one record: the call
//!   answers the modelled 204 with no document, every later read of it — by any label
//!   form that used to name it, full or summary, listed or searched — is
//!   `ResourceNotFoundException` (HTTP 404), the asset URL it published stops serving its
//!   bytes, its ARN names nothing for the tagging surface, a repeated delete is that same
//!   not-found, and the `name` and the `clientToken` it held are free for another create;
//! * every sibling is untouched: the other contents of the knowledge base, the contents
//!   of another knowledge base, and the knowledge base itself, all keep answering exactly
//!   as they did;
//! * a label pair naming no content — an absent knowledge base, an absent content id, a
//!   content of ANOTHER knowledge base — is the modelled not-found naming the resource
//!   the request addressed, and removes nothing; a label outside the modelled `UuidOrArn`
//!   pattern is `ValidationException` (HTTP 400) and removes nothing. No request is
//!   answered by the scaffold's `NotImplemented`.
//!
//! The store is reached directly in exactly one place — the instant a content's ingestion
//! falls due, backdated so the "already ingested" branch is reachable without waiting out
//! the span the clock owes. Everything else goes over the wire, so a response that is not
//! computed from persisted state cannot pass.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_wisdom_wisdomservice::{state, EmulatedService};
use smithy_server_runtime::MultiServiceRouter;

/// The region every modelled request in this suite is signed for; the handlers read it
/// out of the SigV4 credential scope to build ARNs.
const REGION: &str = "us-east-2";

/// The endpoint host those requests address, and so the host the asset URLs this suite
/// follows are minted on.
const ENDPOINT_HOST: &str = "wisdom.us-east-2.amazonaws.com";

/// The routing key this service answers under (`src/service.rs`'s
/// `OWN_REGISTRATION_KEY`).
const ROUTE_KEY: &str = "com-amazonaws-wisdom-wisdomservice";

/// A UUID no create ever mints, for the not-found assertions.
const ABSENT_UUID: &str = "00000000-0000-0000-0000-000000000000";

/// The only `KnowledgeBaseType` a content may be uploaded into, which
/// `StartContentUpload`'s own documentation states ("You can only upload content to a
/// knowledge base of type CUSTOM").
const TYPE_CUSTOM: &str = "CUSTOM";

/// The fixed sentences this endpoint answers the two not-found classes with, written out
/// here rather than imported from the implementation so the assertions pin the wire text.
const MESSAGE_KNOWLEDGE_BASE_NOT_FOUND: &str = "Knowledge Base does not exist.";
const MESSAGE_CONTENT_NOT_FOUND: &str = "Content does not exist.";

/// The error codes this suite distinguishes.
const NOT_FOUND: &str = "ResourceNotFoundException";
const CONFLICT: &str = "ConflictException";
const VALIDATION: &str = "ValidationException";

/// The code a scaffolded operation answers with, which an implemented one never reaches:
/// no request in this suite may be answered by it.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The two `ContentStatus` values a stored content reports
/// (`smithy_query(query="shape",shape="ContentStatus")` declares them beside the import
/// and delete states), written out as literals so the assertions pin the modelled value.
const STATUS_CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
const STATUS_ACTIVE: &str = "ACTIVE";

/// The `SearchExpression` field and operator the searched-for name is selected by
/// (`smithy_query(query="shape",shape="SearchExpression")`).
const FIELD_NAME: &str = "NAME";
const OPERATOR_EQUALS: &str = "EQUALS";

/// One wire answer, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_type: String,
    content_type: String,
    body: Value,
}

impl Reply {
    fn string(&self, pointer: &str) -> String {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The error code a client branches on: the `x-amzn-errortype` header, falling back
    /// to the body's `__type` for a protocol that carries it only there.
    fn code(&self) -> String {
        if self.error_type.is_empty() {
            self.string("/__type")
        } else {
            self.error_type.clone()
        }
    }

    /// The `contentId`s a listing or a search reports, in the order it reported them.
    fn content_ids(&self) -> Vec<String> {
        self.body
            .pointer("/contentSummaries")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        entry
                            .pointer("/contentId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A path label as a URI segment: an ARN carries `/`, which must not be read as path
/// structure.
fn segment(label: &str) -> String {
    label.replace('/', "%2F")
}

/// Dispatch one already-built request and read the answer.
async fn dispatch(router: &MultiServiceRouter, request: http::Request<Bytes>) -> Reply {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let error_type = header("x-amzn-errortype");
    let content_type = header("content-type");
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            // A non-JSON body is an asset the plane served: keep the bytes so the
            // assertions can compare them.
            json!({ "assetBody": String::from_utf8_lossy(&bytes) })
        })
    };
    Reply {
        status,
        error_type,
        content_type,
        body,
    }
}

/// An emulator with just this service mounted on a fresh engine, driven in-process
/// through the production router.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signatures are not what these tests are about; the router still routes by the
        // SigV4 credential scope below, which is also where an ARN's region comes from.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// One signed call of this endpoint, built the way an SDK builds it.
    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> Reply {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{ROUTE_KEY}/aws4_request, \
             SignedHeaders=host, Signature=not-verified-in-this-suite"
        );
        let builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{ENDPOINT_HOST}{uri}"))
            .header("host", ENDPOINT_HOST)
            .header("authorization", authorization);
        let request = match &body {
            Some(value) => builder
                .header("content-type", "application/json")
                .body(Bytes::from(
                    serde_json::to_vec(value).expect("serialize body"),
                ))
                .expect("build request"),
            None => builder.body(Bytes::new()).expect("build request"),
        };
        dispatch(&self.router, request).await
    }

    /// Follow a URL this service handed out exactly as a client does: `method` against
    /// the absolute URL, with NO credential of any kind.
    async fn follow(&self, method: &str, url: &str, headers: &[(&str, &str)], body: &[u8]) -> Reply {
        let mut builder = http::Request::builder().method(method).uri(url);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(Bytes::copy_from_slice(body))
            .expect("build request");
        dispatch(&self.router, request).await
    }

    /// A `CUSTOM` knowledge base, as its create persisted it: its id and its ARN.
    async fn knowledge_base(&self, name: &str) -> (String, String) {
        let reply = self
            .send(
                "POST",
                "/knowledgeBases",
                Some(json!({ "name": name, "knowledgeBaseType": TYPE_CUSTOM })),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case \"knowledge base {name}\": expected HTTP 200, got {} with {}",
            reply.status, reply.body
        );
        (
            reply.string("/knowledgeBase/knowledgeBaseId"),
            reply.string("/knowledgeBase/knowledgeBaseArn"),
        )
    }

    /// Start an upload of `content_type` under `knowledge_base_label` and write `asset`
    /// through the URL it hands out, so the `uploadId` names bytes this store holds.
    async fn written_upload(
        &self,
        knowledge_base_label: &str,
        content_type: &str,
        asset: &[u8],
    ) -> String {
        let reply = self
            .send(
                "POST",
                &format!(
                    "/knowledgeBases/{}/upload",
                    segment(knowledge_base_label)
                ),
                Some(json!({ "contentType": content_type })),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case \"upload of {content_type}\": expected HTTP 200, got {} with {}",
            reply.status, reply.body
        );
        let upload_id = reply.string("/uploadId");
        let put = self
            .follow(
                "PUT",
                &reply.string("/url"),
                &[("content-type", content_type)],
                asset,
            )
            .await;
        assert_eq!(
            put.status, 200,
            "case \"PUT the asset of {upload_id}\": expected HTTP 200, got {} with {}",
            put.status, put.body
        );
        upload_id
    }

    /// A content holding `asset`, as its create persisted it. `extra` carries whichever
    /// optional members the row names.
    async fn content(
        &self,
        knowledge_base_label: &str,
        name: &str,
        content_type: &str,
        asset: &[u8],
        extra: &Value,
    ) -> Reply {
        let upload_id = self
            .written_upload(knowledge_base_label, content_type, asset)
            .await;
        let mut body = json!({ "name": name, "uploadId": upload_id });
        if let Some(members) = extra.as_object() {
            for (member, value) in members {
                body[member.clone()] = value.clone();
            }
        }
        self.create_content(knowledge_base_label, body).await
    }

    async fn create_content(&self, knowledge_base_label: &str, body: Value) -> Reply {
        self.send(
            "POST",
            &format!(
                "/knowledgeBases/{}/contents",
                segment(knowledge_base_label)
            ),
            Some(body),
        )
        .await
    }

    async fn get_content(&self, knowledge_base_label: &str, content_label: &str) -> Reply {
        self.send(
            "GET",
            &format!(
                "/knowledgeBases/{}/contents/{}",
                segment(knowledge_base_label),
                segment(content_label)
            ),
            None,
        )
        .await
    }

    async fn content_summary(&self, knowledge_base_label: &str, content_label: &str) -> Reply {
        self.send(
            "GET",
            &format!(
                "/knowledgeBases/{}/contents/{}/summary",
                segment(knowledge_base_label),
                segment(content_label)
            ),
            None,
        )
        .await
    }

    async fn delete_content(&self, knowledge_base_label: &str, content_label: &str) -> Reply {
        self.send(
            "DELETE",
            &format!(
                "/knowledgeBases/{}/contents/{}",
                segment(knowledge_base_label),
                segment(content_label)
            ),
            None,
        )
        .await
    }

    async fn list_contents(&self, knowledge_base_label: &str) -> Reply {
        self.send(
            "GET",
            &format!(
                "/knowledgeBases/{}/contents?maxResults=100",
                segment(knowledge_base_label)
            ),
            None,
        )
        .await
    }

    /// `SearchContent` for one name under one knowledge base.
    async fn search_by_name(&self, knowledge_base_label: &str, name: &str) -> Reply {
        self.send(
            "POST",
            &format!("/knowledgeBases/{}/search", segment(knowledge_base_label)),
            Some(json!({
                "searchExpression": {
                    "filters": [{
                        "field": FIELD_NAME,
                        "operator": OPERATOR_EQUALS,
                        "value": name,
                    }],
                },
            })),
        )
        .await
    }

    async fn list_tags(&self, resource_arn: &str) -> Reply {
        self.send("GET", &format!("/tags/{}", segment(resource_arn)), None)
            .await
    }

    async fn get_knowledge_base(&self, knowledge_base_label: &str) -> Reply {
        self.send(
            "GET",
            &format!("/knowledgeBases/{}", segment(knowledge_base_label)),
            None,
        )
        .await
    }

    /// Bring the instant one content's ingestion falls due forward, so the status every
    /// read reports is the terminal one without waiting out the span the clock owes.
    fn finish_ingestion(&self, content_id: &str) {
        let store = state::service_state(&*self.engine).expect("the service state");
        let mut store = store.lock();
        let content = store
            .contents
            .get_mut(content_id)
            .expect("the content just created");
        content.active_at = SystemTime::now() - Duration::from_secs(1);
    }
}

/// One row of the delete table: the shape of the content deleted, and which label form
/// each half of the route carries.
struct Row {
    case: &'static str,
    name: &'static str,
    /// A media type from the modelled `ContentType` catalog
    /// (`smithy_query(query="shape",shape="ContentType")`:
    /// `^(text/(plain|html|csv))|(application/(pdf|vnd\.openxmlformats-officedocument\.wordprocessingml\.document))|(application/x\.wisdom-json;source=(salesforce|servicenow|zendesk))$`),
    /// including entries the recorded request used none of.
    content_type: &'static str,
    /// The asset the content holds, from no bytes at all upwards.
    asset: &'static [u8],
    /// The optional members the create names.
    extra: Value,
    /// Whether the delete addresses the knowledge base by its ARN rather than its id.
    knowledge_base_by_arn: bool,
    /// Whether it addresses the content by its ARN rather than its id.
    content_by_arn: bool,
}

/// The contents walked through create → read → delete, spanning the media types the model
/// admits, the optional members a create may name, the asset sizes from empty upwards, and
/// both label forms of both halves of the route.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a text content, both labels as ids",
            name: "runbook-escalation",
            content_type: "text/plain",
            asset: b"escalate to tier two",
            extra: json!({}),
            knowledge_base_by_arn: false,
            content_by_arn: false,
        },
        Row {
            case: "an html content carrying every optional member, by its ARN",
            name: "Billing FAQ v2.1",
            content_type: "text/html",
            asset: b"<p>how billing works</p>",
            extra: json!({
                "title": "Billing FAQ",
                "metadata": { "source": "crm", "locale": "en-GB" },
                "tags": { "team": "support", "tier": "gold" },
                "overrideLinkOutUri": "https://example.test/articles/billing",
            }),
            knowledge_base_by_arn: false,
            content_by_arn: true,
        },
        Row {
            case: "a csv content under a knowledge base addressed by ARN",
            name: "returns, refunds and credits",
            content_type: "text/csv",
            asset: b"policy,days\nreturns,30\n",
            extra: json!({ "metadata": { "table": "kb" } }),
            knowledge_base_by_arn: true,
            content_by_arn: false,
        },
        Row {
            case: "a pdf content holding no bytes at all, both labels as ARNs",
            name: "policy_2026_01",
            content_type: "application/pdf",
            asset: b"",
            extra: json!({ "title": "A policy", "tags": { "owner": "ops" } }),
            knowledge_base_by_arn: true,
            content_by_arn: true,
        },
        Row {
            case: "a wisdom-json content holding a larger asset",
            name: "20260101",
            content_type: "application/x.wisdom-json;source=salesforce",
            asset: b"{\"article\":\"0123456789 0123456789 0123456789 0123456789 0123456789\"}",
            extra: json!({ "metadata": { "org": "salesforce" } }),
            knowledge_base_by_arn: false,
            content_by_arn: false,
        },
    ]
}

/// RULE: a delete of a content the knowledge base holds forgets that one record — the
/// modelled 204 with no document, and every later view of it, in either label form, is the
/// modelled not-found.
#[tokio::test]
async fn a_delete_forgets_the_content_and_every_view_of_it() {
    for row in rows() {
        let emulator = Emulator::new();
        let (knowledge_base_id, knowledge_base_arn) =
            emulator.knowledge_base("deletion-kb").await;
        let created = emulator
            .content(
                &knowledge_base_id,
                row.name,
                row.content_type,
                row.asset,
                &row.extra,
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected HTTP 200 from the create, got {} with {}",
            row.case, created.status, created.body
        );
        let content_id = created.string("/content/contentId");
        let content_arn = created.string("/content/contentArn");
        let asset_url = created.string("/content/url");

        // The record and its asset are there to begin with, so what the delete changes is
        // not something that was never served.
        let before = emulator.get_content(&knowledge_base_id, &content_id).await;
        assert_eq!(
            before.status, 200,
            "case {:?}: the content must be readable before the delete, got {} with {}",
            row.case, before.status, before.body
        );
        assert_eq!(
            before.string("/content/status"),
            STATUS_CREATE_IN_PROGRESS,
            "case {:?}: a content whose ingestion is still due reports the transient \
             status, got {}",
            row.case,
            before.body
        );
        let asset_before = emulator.follow("GET", &asset_url, &[], b"").await;
        assert_eq!(
            asset_before.status, 200,
            "case {:?}: the asset URL must serve the asset before the delete, got {} with \
             {}",
            row.case, asset_before.status, asset_before.body
        );

        let knowledge_base_label = if row.knowledge_base_by_arn {
            knowledge_base_arn.clone()
        } else {
            knowledge_base_id.clone()
        };
        let content_label = if row.content_by_arn {
            content_arn.clone()
        } else {
            content_id.clone()
        };

        let deleted = emulator
            .delete_content(&knowledge_base_label, &content_label)
            .await;
        assert_eq!(
            deleted.status, 204,
            "case {:?}: expected the modelled 204, got {} with {}",
            row.case, deleted.status, deleted.body
        );
        assert!(
            deleted
                .body
                .as_object()
                .is_none_or(serde_json::Map::is_empty),
            "case {:?}: the 204 names no resource, got {}",
            row.case,
            deleted.body
        );

        // Every read of the record, by every label form that used to name it, and the
        // summary beside the full read.
        for (view, reply) in [
            (
                "GetContent by id",
                emulator.get_content(&knowledge_base_id, &content_id).await,
            ),
            (
                "GetContent by ARN",
                emulator.get_content(&knowledge_base_arn, &content_arn).await,
            ),
            (
                "GetContentSummary by id",
                emulator
                    .content_summary(&knowledge_base_id, &content_id)
                    .await,
            ),
            (
                "GetContentSummary by ARN",
                emulator
                    .content_summary(&knowledge_base_arn, &content_arn)
                    .await,
            ),
            (
                "a repeated delete",
                emulator
                    .delete_content(&knowledge_base_label, &content_label)
                    .await,
            ),
            (
                "a repeated delete in the other label form",
                emulator
                    .delete_content(&knowledge_base_arn, &content_arn)
                    .await,
            ),
        ] {
            assert_eq!(
                reply.status, 404,
                "case {:?} ({view}): expected HTTP 404 after the delete, got {} with {}",
                row.case, reply.status, reply.body
            );
            assert!(
                reply.code().contains(NOT_FOUND),
                "case {:?} ({view}): expected {NOT_FOUND}, got {:?} with {}",
                row.case,
                reply.code(),
                reply.body
            );
            assert_eq!(
                reply.string("/message"),
                MESSAGE_CONTENT_NOT_FOUND,
                "case {:?} ({view}): expected the content not-found sentence, got {}",
                row.case,
                reply.body
            );
            assert!(
                !reply.code().contains(NOT_IMPLEMENTED),
                "case {:?} ({view}): an implemented operation never answers \
                 {NOT_IMPLEMENTED}, got {}",
                row.case,
                reply.body
            );
        }

        // The listing and the search select out of the same collection, so neither names
        // it any more.
        let listed = emulator.list_contents(&knowledge_base_id).await;
        assert_eq!(
            listed.content_ids(),
            Vec::<String>::new(),
            "case {:?}: the listing must not name a deleted content, got {}",
            row.case,
            listed.body
        );
        let searched = emulator.search_by_name(&knowledge_base_id, row.name).await;
        assert_eq!(
            searched.content_ids(),
            Vec::<String>::new(),
            "case {:?}: the search must not name a deleted content, got {}",
            row.case,
            searched.body
        );

        // The asset locator the create published pointed INTO the record, so it stops
        // serving the bytes with it.
        let asset_after = emulator.follow("GET", &asset_url, &[], b"").await;
        assert!(
            asset_after.status >= 400,
            "case {:?}: the asset URL of a deleted content must not be served, got {} with \
             {}",
            row.case,
            asset_after.status,
            asset_after.body
        );
        assert_eq!(
            asset_after.string("/assetBody"),
            "",
            "case {:?}: no asset bytes may come back after the delete, got {}",
            row.case,
            asset_after.body
        );

        // The tagging surface reaches a content through the ARN index the delete released.
        let tags = emulator.list_tags(&content_arn).await;
        assert_eq!(
            tags.status, 404,
            "case {:?}: the ARN of a deleted content names nothing for the tagging \
             surface, got {} with {}",
            row.case, tags.status, tags.body
        );

        // The content's parent is NOT deleted with it: a delete forgets one record.
        let parent = emulator.get_knowledge_base(&knowledge_base_id).await;
        assert_eq!(
            parent.status, 200,
            "case {:?}: the knowledge base must survive its content's delete, got {} with \
             {}",
            row.case, parent.status, parent.body
        );
    }
}

/// RULE: a delete forgets exactly ONE record, so every sibling — in the same knowledge
/// base and in another — keeps answering as it did.
#[tokio::test]
async fn a_delete_leaves_every_sibling_untouched() {
    let emulator = Emulator::new();
    let (first_kb, _) = emulator.knowledge_base("siblings-one").await;
    let (second_kb, _) = emulator.knowledge_base("siblings-two").await;

    let mut first_ids: Vec<String> = Vec::new();
    let mut second_ids: Vec<String> = Vec::new();
    for (index, row) in rows().into_iter().enumerate() {
        let created = emulator
            .content(
                &first_kb,
                row.name,
                row.content_type,
                row.asset,
                &row.extra,
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: expected HTTP 200 from the create, got {} with {}",
            row.case, created.status, created.body
        );
        first_ids.push(created.string("/content/contentId"));

        // The same name under ANOTHER knowledge base is another content: the two
        // collections are independent, which is what the sibling assertions below rest on.
        let other = emulator
            .content(
                &second_kb,
                row.name,
                row.content_type,
                row.asset,
                &json!({ "title": format!("copy {index}") }),
            )
            .await;
        assert_eq!(
            other.status, 200,
            "case {:?}: expected HTTP 200 from the create under the other knowledge base, \
             got {} with {}",
            row.case, other.status, other.body
        );
        second_ids.push(other.string("/content/contentId"));
    }

    // Each content is deleted in turn, so the rule is asserted at every width from the
    // full collection down to one entry.
    let mut survivors = first_ids.clone();
    while let Some(doomed) = survivors.pop() {
        let before: Vec<Value> = {
            let mut bodies = Vec::new();
            for id in &survivors {
                bodies.push(emulator.get_content(&first_kb, id).await.body);
            }
            bodies
        };
        let deleted = emulator.delete_content(&first_kb, &doomed).await;
        assert_eq!(
            deleted.status, 204,
            "case \"delete {doomed}\": expected the modelled 204, got {} with {}",
            deleted.status, deleted.body
        );

        for (id, body) in survivors.iter().zip(&before) {
            let reply = emulator.get_content(&first_kb, id).await;
            assert_eq!(
                reply.status, 200,
                "case \"sibling {id} after deleting {doomed}\": expected HTTP 200, got {} \
                 with {}",
                reply.status, reply.body
            );
            assert_eq!(
                &reply.body, body,
                "case \"sibling {id} after deleting {doomed}\": the record must be \
                 untouched, got {}",
                reply.body
            );
        }

        let mut expected = survivors.clone();
        expected.sort();
        let listed = emulator.list_contents(&first_kb).await;
        assert_eq!(
            listed.content_ids(),
            expected,
            "case \"the listing after deleting {doomed}\": expected exactly the surviving \
             contents, got {}",
            listed.body
        );
    }

    // Nothing in the other knowledge base moved, whatever happened in this one.
    let mut expected_second = second_ids.clone();
    expected_second.sort();
    let listed = emulator.list_contents(&second_kb).await;
    assert_eq!(
        listed.content_ids(),
        expected_second,
        "case \"the other knowledge base\": its contents must be untouched, got {}",
        listed.body
    );
    for id in &second_ids {
        let reply = emulator.get_content(&second_kb, id).await;
        assert_eq!(
            reply.status, 200,
            "case \"the other knowledge base's content {id}\": expected HTTP 200, got {} \
             with {}",
            reply.status, reply.body
        );
    }
}

/// RULE: a delete releases what the record held — its `name` conflicts with no later
/// create, and the `clientToken` that created it names nothing any more, so a replay of
/// that token neither resurrects the record nor conflicts with it.
#[tokio::test]
async fn the_name_and_the_client_token_a_deleted_content_held_are_free_again() {
    let emulator = Emulator::new();
    let (knowledge_base_id, _) = emulator.knowledge_base("reuse-kb").await;
    const NAME: &str = "runbook-escalation";
    const TOKEN: &str = "a-client-token-for-the-first-create";
    const TYPE: &str = "text/plain";

    let first_upload = emulator
        .written_upload(&knowledge_base_id, TYPE, b"first")
        .await;
    let created = emulator
        .create_content(
            &knowledge_base_id,
            json!({ "name": NAME, "uploadId": first_upload, "clientToken": TOKEN }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case \"the first create\": expected HTTP 200, got {} with {}",
        created.status, created.body
    );
    let first_id = created.string("/content/contentId");

    // While that content is LIVE, its name is taken: the conflict below is what the
    // delete must release.
    let second_upload = emulator
        .written_upload(&knowledge_base_id, TYPE, b"second")
        .await;
    let clash = emulator
        .create_content(
            &knowledge_base_id,
            json!({ "name": NAME, "uploadId": second_upload }),
        )
        .await;
    assert_eq!(
        clash.status, 409,
        "case \"the name of a live content\": expected HTTP 409, got {} with {}",
        clash.status, clash.body
    );
    assert!(
        clash.code().contains(CONFLICT),
        "case \"the name of a live content\": expected {CONFLICT}, got {:?} with {}",
        clash.code(),
        clash.body
    );

    assert_eq!(
        emulator
            .delete_content(&knowledge_base_id, &first_id)
            .await
            .status,
        204,
        "case \"delete the content holding the name\": expected the modelled 204"
    );

    // The name is free, and the token that created the deleted record names nothing: the
    // create below carries BOTH and is a fresh create rather than a replay or a conflict.
    let third_upload = emulator
        .written_upload(&knowledge_base_id, TYPE, b"third")
        .await;
    let recreated = emulator
        .create_content(
            &knowledge_base_id,
            json!({ "name": NAME, "uploadId": third_upload, "clientToken": TOKEN }),
        )
        .await;
    assert_eq!(
        recreated.status, 200,
        "case \"the name and the token of a deleted content\": expected HTTP 200, got {} \
         with {}",
        recreated.status, recreated.body
    );
    let recreated_id = recreated.string("/content/contentId");
    assert_ne!(
        recreated_id, first_id,
        "case \"the name and the token of a deleted content\": the create must mint a new \
         content rather than answer with the deleted one, got {}",
        recreated.body
    );
    assert_eq!(
        emulator
            .get_content(&knowledge_base_id, &first_id)
            .await
            .status,
        404,
        "case \"the deleted content after the re-create\": it must still name nothing"
    );
    assert_eq!(
        emulator
            .get_content(&knowledge_base_id, &recreated_id)
            .await
            .status,
        200,
        "case \"the re-created content\": it must be readable"
    );
}

/// RULE: the span between a create and the ingestion it starts is the clock's, not the
/// caller's — a delete answers the same whichever side of it the call falls on.
#[tokio::test]
async fn a_delete_answers_the_same_once_the_ingestion_is_done() {
    let emulator = Emulator::new();
    let (knowledge_base_id, _) = emulator.knowledge_base("ingested-kb").await;
    let created = emulator
        .content(
            &knowledge_base_id,
            "ingested-article",
            "text/plain",
            b"already ingested",
            &json!({}),
        )
        .await;
    let content_id = created.string("/content/contentId");

    emulator.finish_ingestion(&content_id);
    let read = emulator.get_content(&knowledge_base_id, &content_id).await;
    assert_eq!(
        read.string("/content/status"),
        STATUS_ACTIVE,
        "case \"an ingested content\": expected the terminal status, got {}",
        read.body
    );

    let deleted = emulator.delete_content(&knowledge_base_id, &content_id).await;
    assert_eq!(
        deleted.status, 204,
        "case \"delete an ingested content\": expected the modelled 204, got {} with {}",
        deleted.status, deleted.body
    );
    let after = emulator.get_content(&knowledge_base_id, &content_id).await;
    assert_eq!(
        after.status, 404,
        "case \"an ingested content after its delete\": expected HTTP 404, got {} with {}",
        after.status, after.body
    );
}

/// One row of the refusal table: the pair the route carries, and the answer it is owed.
struct Miss {
    case: &'static str,
    knowledge_base_label: String,
    content_label: String,
    status: u16,
    code: &'static str,
    /// The sentence this endpoint states for the class, where it states a fixed one.
    message: Option<&'static str>,
    /// The label the answer names as the resource that is missing.
    resource_name: Option<String>,
}

/// RULE: a label pair naming no content of this account is the modelled not-found, naming
/// the resource the request addressed; a label outside the modelled `UuidOrArn` pattern
/// never reaches the store at all. Neither removes anything.
#[tokio::test]
async fn a_pair_naming_no_content_removes_nothing() {
    let emulator = Emulator::new();
    let (first_kb, first_kb_arn) = emulator.knowledge_base("miss-one").await;
    let (second_kb, _) = emulator.knowledge_base("miss-two").await;
    let live = emulator
        .content(
            &first_kb,
            "live-article",
            "text/plain",
            b"still here",
            &json!({}),
        )
        .await;
    let live_id = live.string("/content/contentId");
    let live_arn = live.string("/content/contentArn");
    let other = emulator
        .content(
            &second_kb,
            "other-article",
            "text/html",
            b"<p>elsewhere</p>",
            &json!({}),
        )
        .await;
    let other_id = other.string("/content/contentId");
    let other_arn = other.string("/content/contentArn");
    let account = engine::EMULATOR_ACCOUNT_ID;

    let misses: Vec<Miss> = vec![
        Miss {
            case: "a bare knowledge base id nothing holds",
            knowledge_base_label: ABSENT_UUID.to_string(),
            content_label: live_id.clone(),
            status: 404,
            code: NOT_FOUND,
            message: Some(MESSAGE_KNOWLEDGE_BASE_NOT_FOUND),
            resource_name: Some(ABSENT_UUID.to_string()),
        },
        Miss {
            case: "this account's knowledge base ARN for an absent id",
            knowledge_base_label: format!(
                "arn:aws:wisdom:{REGION}:{account}:knowledge-base/{ABSENT_UUID}"
            ),
            content_label: live_id.clone(),
            status: 404,
            code: NOT_FOUND,
            message: Some(MESSAGE_KNOWLEDGE_BASE_NOT_FOUND),
            resource_name: Some(format!(
                "arn:aws:wisdom:{REGION}:{account}:knowledge-base/{ABSENT_UUID}"
            )),
        },
        Miss {
            case: "another account's ARN for the live knowledge base",
            knowledge_base_label: format!(
                "arn:aws:wisdom:{REGION}:123456789012:knowledge-base/{first_kb}"
            ),
            content_label: live_id.clone(),
            status: 404,
            code: NOT_FOUND,
            message: Some(MESSAGE_KNOWLEDGE_BASE_NOT_FOUND),
            resource_name: Some(format!(
                "arn:aws:wisdom:{REGION}:123456789012:knowledge-base/{first_kb}"
            )),
        },
        Miss {
            case: "another partition's ARN for the live knowledge base",
            knowledge_base_label: format!(
                "arn:aws-cn:wisdom:{REGION}:{account}:knowledge-base/{first_kb}"
            ),
            content_label: live_id.clone(),
            status: 404,
            code: NOT_FOUND,
            message: Some(MESSAGE_KNOWLEDGE_BASE_NOT_FOUND),
            resource_name: Some(format!(
                "arn:aws-cn:wisdom:{REGION}:{account}:knowledge-base/{first_kb}"
            )),
        },
        Miss {
            case: "a bare content id this knowledge base does not hold",
            knowledge_base_label: first_kb.clone(),
            content_label: ABSENT_UUID.to_string(),
            status: 404,
            code: NOT_FOUND,
            message: Some(MESSAGE_CONTENT_NOT_FOUND),
            resource_name: Some(ABSENT_UUID.to_string()),
        },
        Miss {
            case: "this account's content ARN for an absent content id",
            knowledge_base_label: first_kb.clone(),
            content_label: format!(
                "arn:aws:wisdom:{REGION}:{account}:content/{first_kb}/{ABSENT_UUID}"
            ),
            status: 404,
            code: NOT_FOUND,
            message: Some(MESSAGE_CONTENT_NOT_FOUND),
            resource_name: Some(format!(
                "arn:aws:wisdom:{REGION}:{account}:content/{first_kb}/{ABSENT_UUID}"
            )),
        },
        Miss {
            case: "a content of ANOTHER knowledge base, by id",
            knowledge_base_label: first_kb.clone(),
            content_label: other_id.clone(),
            status: 404,
            code: NOT_FOUND,
            message: Some(MESSAGE_CONTENT_NOT_FOUND),
            resource_name: Some(other_id.clone()),
        },
        Miss {
            case: "a content of ANOTHER knowledge base, by ARN",
            knowledge_base_label: first_kb_arn.clone(),
            content_label: other_arn.clone(),
            status: 404,
            code: NOT_FOUND,
            message: Some(MESSAGE_CONTENT_NOT_FOUND),
            resource_name: Some(other_arn.clone()),
        },
        Miss {
            case: "this knowledge base's content ARN under the other knowledge base",
            knowledge_base_label: second_kb.clone(),
            content_label: live_arn.clone(),
            status: 404,
            code: NOT_FOUND,
            message: Some(MESSAGE_CONTENT_NOT_FOUND),
            resource_name: Some(live_arn.clone()),
        },
        Miss {
            case: "another account's ARN for the live content",
            knowledge_base_label: first_kb.clone(),
            content_label: format!(
                "arn:aws:wisdom:{REGION}:123456789012:content/{first_kb}/{live_id}"
            ),
            status: 404,
            code: NOT_FOUND,
            message: Some(MESSAGE_CONTENT_NOT_FOUND),
            resource_name: Some(format!(
                "arn:aws:wisdom:{REGION}:123456789012:content/{first_kb}/{live_id}"
            )),
        },
        // A label outside the modelled `UuidOrArn` pattern is refused by the model's own
        // constraint, before any lookup; the pattern pins the service segment to `wisdom`
        // and the hex to lowercase
        // (`smithy_query(query="operation",operation="DeleteContent")`).
        Miss {
            case: "a knowledge base label that is no id at all",
            knowledge_base_label: "not-a-knowledge-base".to_string(),
            content_label: live_id.clone(),
            status: 400,
            code: VALIDATION,
            message: None,
            resource_name: None,
        },
        Miss {
            case: "a content label that is no id at all",
            knowledge_base_label: first_kb.clone(),
            content_label: "not-a-content".to_string(),
            status: 400,
            code: VALIDATION,
            message: None,
            resource_name: None,
        },
        Miss {
            case: "an uppercase uuid, which the modelled pattern does not admit",
            knowledge_base_label: first_kb.clone(),
            content_label: "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA".to_string(),
            status: 400,
            code: VALIDATION,
            message: None,
            resource_name: None,
        },
        Miss {
            case: "a uuid one hex digit short",
            knowledge_base_label: first_kb.clone(),
            content_label: "0000000-0000-0000-0000-000000000000".to_string(),
            status: 400,
            code: VALIDATION,
            message: None,
            resource_name: None,
        },
        Miss {
            case: "another service's ARN for the live content",
            knowledge_base_label: first_kb.clone(),
            content_label: format!(
                "arn:aws:connect:{REGION}:{account}:content/{first_kb}/{live_id}"
            ),
            status: 400,
            code: VALIDATION,
            message: None,
            resource_name: None,
        },
    ];

    for miss in &misses {
        let reply = emulator
            .delete_content(&miss.knowledge_base_label, &miss.content_label)
            .await;
        assert_eq!(
            reply.status, miss.status,
            "case {:?}: expected HTTP {}, got {} with {}",
            miss.case, miss.status, reply.status, reply.body
        );
        assert!(
            reply.code().contains(miss.code),
            "case {:?}: expected {}, got {:?} with {}",
            miss.case,
            miss.code,
            reply.code(),
            reply.body
        );
        assert_eq!(
            reply.content_type, "application/json",
            "case {:?}: restJson1 answers application/json even for an error, got {:?}",
            miss.case, reply.content_type
        );
        if let Some(message) = miss.message {
            assert_eq!(
                reply.string("/message"),
                message,
                "case {:?}: expected the sentence this endpoint states for the class, got \
                 {}",
                miss.case,
                reply.body
            );
        }
        if let Some(resource_name) = &miss.resource_name {
            assert_eq!(
                &reply.string("/resourceName"),
                resource_name,
                "case {:?}: the answer must name the resource the request addressed, got \
                 {}",
                miss.case,
                reply.body
            );
        }
        assert!(
            !reply.code().contains(NOT_IMPLEMENTED),
            "case {:?}: an implemented operation never answers {NOT_IMPLEMENTED}, got {}",
            miss.case,
            reply.body
        );
    }

    // Not one of those refusals removed anything.
    for (case, knowledge_base, content) in [
        ("the live content", &first_kb, &live_id),
        ("the other knowledge base's content", &second_kb, &other_id),
    ] {
        let reply = emulator.get_content(knowledge_base, content).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: it must still be readable after every refusal, got {} with {}",
            reply.status, reply.body
        );
    }
}
