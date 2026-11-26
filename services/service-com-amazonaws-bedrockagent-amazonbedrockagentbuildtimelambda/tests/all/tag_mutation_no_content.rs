// SPDX-License-Identifier: CC0-1.0

//! The SUCCESS STATUS and the FRAMING of this service's tag mutations — `TagResource` and
//! `UntagResource` — end to end over HTTP, through the same front door as every other suite
//! of this target.
//!
//! The RULE under test: an operation of this service whose modelled output structure declares
//! no member leaves its caller nothing to parse, so its success is answered `204 No Content`
//! with no body at all, and never with the 200 that promises a document. The two tag
//! mutations are the operations of this service that can be driven over that rule today:
//! `TagResourceResponse` and `UntagResourceResponse` declare no member
//! (`smithy_query(query="shape",shape="TagResourceResponse,UntagResourceResponse",\
//! model_path="services/service-com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda/\
//! smithy-model/com-amazonaws-bedrockagent-amazonbedrockagentbuildtimelambda.json")`), and
//! the model leaves their status at the protocol's 200 fallback while pinning 204 explicitly
//! on the other three members-less operations of the same model
//! (`DisassociateAgentKnowledgeBase`, `DisassociateAgentCollaborator` and
//! `DeleteAgentActionGroup`, each `@http code 204` — all three still scaffolded, so no
//! request of theirs reaches a handler to assert on). Every answer that DOES carry content
//! keeps its own status and its body: a reader's 200 with the document it reports, a create's
//! own status, and each modelled refusal's.
//!
//! The suites assert that over SETS of inputs rather than the one recorded call, the rows
//! differing along what the rule generalises over:
//!
//! * the tag map written and removed again — no pair at all, one pair, several pairs, a key
//!   at the published upper bound with the empty value the value shape admits, and keys and
//!   values spelled from the whole documented character set — each call made twice, so the
//!   status cannot depend on whether the call changed anything;
//! * the KIND of resource the ARN names — a prompt, a version cut from one, a knowledge base
//!   and a flow — so the status belongs to the mutation rather than to what it addressed;
//! * removals naming a stored key, a key the resource never carried, and the same key twice.
//!
//! What the two mutations DO to the tag map is [`crate::resource_tags`]' subject; this suite
//! owns the status and the framing. Each mutation is still read back through
//! `ListTagsForResource`, so the status asserted here is that of a write that landed rather
//! than of a refusal dressed up as a success.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The status a SUCCESSFUL tag mutation of this service answers with: `204 No Content`,
/// because `TagResourceResponse` and `UntagResourceResponse` carry no member.
///
/// `pub(crate)` and stated once here: [`crate::resource_tags`] and
/// [`crate::prompt_version_minting`] drive the same mutations while asserting what they do to
/// the tag map, so they name this rule rather than each spelling a status of their own.
pub(crate) const TAG_MUTATION_SUCCESS_STATUS: u16 = 204;

/// The status an answer that DOES carry members keeps, for the contrast the last suite draws.
const BODY_FOLLOWS_STATUS: u16 = 200;

/// The length a no-content answer states for the body it does not carry.
///
/// What the in-process front door delivers, which is what this suite drives: an answer served
/// over HTTP/1.1 reaches a client without it, the HTTP writer the vendored transport is built
/// on dropping `content-length` from a 204 — below anything a service crate decides.
const NO_CONTENT_LENGTH: &str = "0";

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the routing key every
/// Bedrock-family service claims on, and the source of the Region its ARNs carry.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// The region of [`AUTHORIZATION`]'s credential scope.
const REGION: &str = "eu-west-1";

/// The account id every emulated ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// A role ARN matching the modelled `KnowledgeBaseRoleArn` pattern.
const KB_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForKnowledgeBase";

/// A role ARN matching the modelled `FlowExecutionRoleArn` pattern.
const FLOW_ROLE: &str =
    "arn:aws:iam::000000000000:role/service-role/AmazonBedrockExecutionRoleForFlows";

/// An embedding model ARN naming a model whose published output is an embedding.
const EMBEDDING_MODEL: &str =
    "arn:aws:bedrock:eu-west-1::foundation-model/amazon.titan-embed-text-v2:0";

/// A key no row of this suite ever writes, for the removals that name a key the resource does
/// not carry.
const NEVER_STORED_KEY: &str = "never-stored";

/// One value as a percent-encoded URI component, as a client that sends a non-greedy
/// `@httpLabel` or a query value encodes it.
fn label(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
        .replace('=', "%3D")
        .replace('+', "%2B")
        .replace('&', "%26")
        .replace(' ', "%20")
        .replace('@', "%40")
}

/// One WHOLE HTTP answer from the emulator: the status line, the framing header and every
/// body byte, so the framing of an answer that carries no content can be asserted as well as
/// its status.
#[derive(Debug)]
struct Answer {
    status: u16,
    content_length: Option<String>,
    body: Bytes,
}

impl Answer {
    /// The body as JSON. Panics when there is none, which is what every row that reads a
    /// document expects there to be.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "a {} answer must carry a JSON body ({error}): {:?}",
                self.status,
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    /// One string member of the body.
    fn string(&self, member: &str) -> String {
        self.json()[member]
            .as_str()
            .unwrap_or_else(|| panic!("{member} is not a string in {:?}", self.json()))
            .to_string()
    }

    /// The reported `tags` map, as a sorted map so a comparison is order-free.
    fn tags(&self, case: &str) -> BTreeMap<String, String> {
        let body = self.json();
        body["tags"]
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: expected a tags map, got {body:?}"))
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case:?}: a tag value is a string"))
                        .to_string(),
                )
            })
            .collect()
    }
}

/// The emulator under test: one router over one shared engine, exactly as the production
/// router mounts this service.
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

    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let bytes = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("serialize request body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "bedrock-agent.eu-west-1.amazonaws.com")
            .header("authorization", AUTHORIZATION)
            .header("content-type", "application/json")
            .body(bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let content_length = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        Answer {
            status,
            content_length,
            body: response.into_body(),
        }
    }

    /// `TagResource`: `POST /tags/{resourceArn}`, whose body carries the `tags` map.
    async fn tag(&self, resource_arn: &str, tags: &BTreeMap<String, String>) -> Answer {
        self.call(
            "POST",
            &format!("/tags/{}", label(resource_arn)),
            Some(json!({ "tags": tags })),
        )
        .await
    }

    /// `UntagResource`: `DELETE /tags/{resourceArn}?tagKeys=…`, each key its own pair because
    /// `tagKeys` is bound to `@httpQuery("tagKeys")`.
    async fn untag(&self, resource_arn: &str, tag_keys: &[String]) -> Answer {
        let query: Vec<String> = tag_keys
            .iter()
            .map(|key| format!("tagKeys={}", label(key)))
            .collect();
        let path = if query.is_empty() {
            format!("/tags/{}", label(resource_arn))
        } else {
            format!("/tags/{}?{}", label(resource_arn), query.join("&"))
        };
        self.call("DELETE", &path, None).await
    }

    /// `ListTagsForResource`: the read of the same plane, which carries members.
    async fn list_tags(&self, resource_arn: &str) -> Answer {
        self.call("GET", &format!("/tags/{}", label(resource_arn)), None)
            .await
    }

    /// Create one prompt and report its ARN.
    async fn prompt(&self, case: &str, name: &str) -> String {
        let created = self
            .call(
                "POST",
                "/prompts/",
                Some(json!({
                    "name": name,
                    "defaultVariant": "summary",
                    "variants": [{
                        "name": "summary",
                        "templateType": "TEXT",
                        "templateConfiguration": { "text": { "text": "Summarise {{input}}" } },
                    }],
                })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected CreatePrompt -> 201, got {created:?}"
        );
        created.string("arn")
    }

    /// Cut one version of the prompt `prompt_arn` names and report the version's ARN: a
    /// second ARN form of the same resource type, carrying the version number.
    async fn prompt_version(&self, case: &str, prompt_id: &str) -> String {
        let cut = self
            .call(
                "POST",
                &format!("/prompts/{}/versions", label(prompt_id)),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            cut.status, 201,
            "case {case:?}: expected CreatePromptVersion -> 201, got {cut:?}"
        );
        cut.string("arn")
    }

    /// Create one knowledge base and report its ARN.
    async fn knowledge_base(&self, case: &str, name: &str) -> String {
        let created = self
            .call(
                "PUT",
                "/knowledgebases/",
                Some(json!({
                    "name": name,
                    "roleArn": KB_ROLE,
                    "knowledgeBaseConfiguration": {
                        "type": "VECTOR",
                        "vectorKnowledgeBaseConfiguration": {
                            "embeddingModelArn": EMBEDDING_MODEL,
                        },
                    },
                    "storageConfiguration": {
                        "type": "OPENSEARCH_SERVERLESS",
                        "opensearchServerlessConfiguration": {
                            "collectionArn":
                                format!("arn:aws:aoss:{REGION}:{ACCOUNT}:collection/framing"),
                            "vectorIndexName": format!("{name}-index"),
                            "fieldMapping": {
                                "vectorField": "bedrock-knowledge-base-default-vector",
                                "textField": "AMAZON_BEDROCK_TEXT_CHUNK",
                                "metadataField": "AMAZON_BEDROCK_METADATA",
                            },
                        },
                    },
                })),
            )
            .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: expected CreateKnowledgeBase -> 202, got {created:?}"
        );
        created.json()["knowledgeBase"]["knowledgeBaseArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a knowledgeBaseArn"))
            .to_string()
    }

    /// Create one flow and report its ARN.
    async fn flow(&self, case: &str, name: &str) -> String {
        let created = self
            .call(
                "POST",
                "/flows/",
                Some(json!({
                    "name": name,
                    "executionRoleArn": FLOW_ROLE,
                    "definition": passthrough_definition(),
                })),
            )
            .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected CreateFlow -> 201, got {created:?}"
        );
        created.string("arn")
    }
}

/// A passthrough flow definition: an `Input` node wired straight to an `Output` node.
fn passthrough_definition() -> Value {
    json!({
        "nodes": [
            {
                "name": "FlowInput",
                "type": "Input",
                "configuration": { "input": {} },
                "outputs": [{ "name": "document", "type": "String" }],
            },
            {
                "name": "FlowOutput",
                "type": "Output",
                "configuration": { "output": {} },
                "inputs": [{
                    "name": "document",
                    "type": "String",
                    "expression": "$.data",
                }],
            },
        ],
        "connections": [{
            "name": "InputToOutput",
            "type": "Data",
            "source": "FlowInput",
            "target": "FlowOutput",
            "configuration": { "data": {
                "sourceOutput": "document",
                "targetInput": "document",
            }},
        }],
    })
}

/// Tag maps every one of which the model admits, spanning what the rule generalises over: the
/// COUNT of pairs, the LENGTH of a key and of a value at the bounds their shapes publish, and
/// the FORM of the text.
///
/// The bounds and the character set are the model's own:
/// `TagKey` is `length 1..128, pattern ^[a-zA-Z0-9\s._:/=+@-]*$` and `TagValue` is
/// `length 0..256` with the same pattern
/// (`smithy_query(query="shape",shape="TagsMap,TagKey,TagValue")`).
fn legal_tag_maps() -> Vec<(&'static str, BTreeMap<String, String>)> {
    let pair = |key: String, value: String| -> BTreeMap<String, String> {
        [(key, value)].into_iter().collect()
    };
    vec![
        ("no pair at all", BTreeMap::new()),
        (
            "one pair",
            pair("estate".to_string(), "framing".to_string()),
        ),
        (
            "several pairs",
            [
                ("estate", "framing"),
                ("owner", "platform"),
                ("release", "2026.01"),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        ),
        (
            "a key at the published upper bound and the empty value the value shape admits",
            pair("k".repeat(128), String::new()),
        ),
        (
            "keys and values spelled from the whole documented character set",
            [
                ("aws.project:name/v1=1+2@x-y z", "a value with spaces"),
                ("_leading", "/=+@-.:_ 0"),
                ("Z9", "v".repeat(256).as_str()),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        ),
    ]
}

/// Assert one mutation answered the no-content success: the status, zero body bytes, and the
/// length it states for the body it does not carry. A 204 carrying a body is the protocol
/// inconsistency the status rules out.
fn assert_no_content(case: &str, operation: &str, answer: &Answer) {
    assert_eq!(
        answer.status, TAG_MUTATION_SUCCESS_STATUS,
        "case {case:?}/{operation}: expected {TAG_MUTATION_SUCCESS_STATUS}, got {} with {:?}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    assert!(
        answer.body.is_empty(),
        "case {case:?}/{operation}: a no-content answer carries no body bytes, got {:?}",
        String::from_utf8_lossy(&answer.body)
    );
    assert_eq!(
        answer.content_length.as_deref(),
        Some(NO_CONTENT_LENGTH),
        "case {case:?}/{operation}: a no-content answer states the length it has, got {:?}",
        answer.content_length
    );
}

/// The keys of a tag map, as a removal names them.
fn keys_of(tags: &BTreeMap<String, String>) -> Vec<String> {
    tags.keys().cloned().collect()
}

/// Every successful tag mutation answers no content, whatever map it writes or removes, and
/// however many times it is made.
#[tokio::test]
async fn every_successful_tag_mutation_answers_no_content() {
    let emulator = Emulator::new();
    let rows = legal_tag_maps();
    assert!(
        rows.len() >= 5,
        "the tag-map table must span the class, got {} rows",
        rows.len()
    );

    for (index, (case, tags)) in rows.into_iter().enumerate() {
        // A resource of its own per row, carrying no tag, so what a read reports afterwards is
        // exactly what these calls wrote and removed.
        let arn = emulator.prompt(case, &format!("framing-row-{index}")).await;

        // Twice: the status says "nothing to parse", which does not depend on whether the
        // call changed anything, so the second, no-op assignment answers the same way.
        for attempt in 1..=2 {
            let answered = emulator.tag(&arn, &tags).await;
            assert_no_content(case, &format!("TagResource attempt {attempt}"), &answered);
        }

        // The write LANDED: the status above is the status of a real mutation.
        let listed = emulator.list_tags(&arn).await;
        assert_eq!(
            listed.tags(case),
            tags,
            "case {case:?}: the assignment must persist before its status means anything, got \
             {listed:?}"
        );

        // The removal names every key the row wrote PLUS one the resource never carried, so
        // the status is asserted for a removal that drops something and for one that drops
        // nothing.
        let mut removed = keys_of(&tags);
        removed.push(NEVER_STORED_KEY.to_string());
        for attempt in 1..=2 {
            let answered = emulator.untag(&arn, &removed).await;
            assert_no_content(case, &format!("UntagResource attempt {attempt}"), &answered);
        }

        let listed = emulator.list_tags(&arn).await;
        assert!(
            listed.tags(case).is_empty(),
            "case {case:?}: the removal must persist too, got {listed:?}"
        );

        // A removal naming the same key TWICE is one removal, and still a no-content success:
        // the operation is `@idempotent`
        // (`smithy_query(query="operation",operation="UntagResource")`).
        let doubled: Vec<String> = removed.iter().chain(removed.iter()).cloned().collect();
        let answered = emulator.untag(&arn, &doubled).await;
        assert_no_content(case, "UntagResource naming each key twice", &answered);
    }
}

/// The no-content status belongs to the mutation, not to the kind of resource it addressed:
/// every resource kind this plane mints and this emulator stores answers alike.
#[tokio::test]
async fn the_no_content_status_holds_for_every_resource_kind_the_plane_mints() {
    let emulator = Emulator::new();
    let prompt = emulator.prompt("a prompt", "framing-prompt").await;
    let prompt_id = prompt
        .rsplit('/')
        .next()
        .expect("the prompt ARN carries an identifier")
        .to_string();
    let rows: Vec<(&str, String)> = vec![
        ("a prompt", prompt.clone()),
        (
            "a version cut from a prompt",
            emulator
                .prompt_version("a version cut from a prompt", &prompt_id)
                .await,
        ),
        (
            "a knowledge base",
            emulator.knowledge_base("a knowledge base", "framing-kb").await,
        ),
        ("a flow", emulator.flow("a flow", "framing-flow").await),
    ];

    for (case, arn) in &rows {
        let written: BTreeMap<String, String> = [("framing".to_string(), "no-content".to_string())]
            .into_iter()
            .collect();
        assert_no_content(case, "TagResource", &emulator.tag(arn, &written).await);

        let listed = emulator.list_tags(arn).await;
        assert_eq!(
            listed.tags(case),
            written,
            "case {case:?}: the assignment must persist before its status means anything, got \
             {listed:?}"
        );

        assert_no_content(
            case,
            "UntagResource",
            &emulator.untag(arn, &["framing".to_string()]).await,
        );

        let listed = emulator.list_tags(arn).await;
        assert!(
            listed.tags(case).is_empty(),
            "case {case:?}: the removal must persist too, got {listed:?}"
        );
    }
}

/// Every OTHER answer of the tagging plane keeps its own status and its own body: the
/// no-content answer is the mutation's success alone.
#[tokio::test]
async fn every_other_answer_of_the_tagging_plane_keeps_its_own_status() {
    let emulator = Emulator::new();
    let arn = emulator.prompt("the contrast", "framing-contrast").await;
    let written: BTreeMap<String, String> = [("framing".to_string(), "no-content".to_string())]
        .into_iter()
        .collect();
    assert_no_content(
        "the mutation the contrast is drawn against",
        "TagResource",
        &emulator.tag(&arn, &written).await,
    );

    // The read of the same plane carries members, so it keeps the status that says a body
    // follows — and carries one.
    let listed = emulator.list_tags(&arn).await;
    assert_eq!(
        listed.status, BODY_FOLLOWS_STATUS,
        "case \"ListTagsForResource\": an answer carrying members keeps its \
         {BODY_FOLLOWS_STATUS}, got {} with {:?}",
        listed.status,
        String::from_utf8_lossy(&listed.body)
    );
    assert!(
        listed.json().is_object(),
        "case \"ListTagsForResource\": an answer carrying members must carry a body, got {:?}",
        String::from_utf8_lossy(&listed.body)
    );

    // So does a listing of the resource this plane minted.
    let prompts = emulator.call("GET", "/prompts/", None).await;
    assert_eq!(
        prompts.status, BODY_FOLLOWS_STATUS,
        "case \"ListPrompts\": an answer carrying members keeps its {BODY_FOLLOWS_STATUS}, got \
         {} with {:?}",
        prompts.status,
        String::from_utf8_lossy(&prompts.body)
    );

    // A REFUSED mutation keeps the status and the body of its refusal. An ARN of the
    // published syntax naming nothing stored is the modelled missing resource; a value outside
    // that syntax is the modelled bad request. `crate::resource_tags` owns their wording.
    for (case, target, expected) in [
        (
            "an ARN of the published syntax naming nothing stored",
            format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:prompt/AZ09AZ09AZ"),
            404_u16,
        ),
        (
            "a value outside the published syntax",
            format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:prompt/"),
            400,
        ),
    ] {
        for (operation, answered) in [
            ("TagResource", emulator.tag(&target, &written).await),
            (
                "UntagResource",
                emulator.untag(&target, &["framing".to_string()]).await,
            ),
        ] {
            assert_eq!(
                answered.status, expected,
                "case {case:?}/{operation}: a refusal keeps its modelled status, got {} with \
                 {:?}",
                answered.status,
                String::from_utf8_lossy(&answered.body)
            );
            assert!(
                !answered.body.is_empty(),
                "case {case:?}/{operation}: a refusal carries its error shape, got an empty body"
            );
        }
    }
}
