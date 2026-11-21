// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ORDER in which a knowledge-base read answers: which resource
//! the request identifies first, what the request asks of it second.
//!
//! THE RULE UNDER TEST: a read resolves the resources the request identifies — answering
//! the modelled `ResourceNotFoundException` (404), whose message is this plane's "Knowledge
//! Base with id <knowledgeBaseId> does not exist" sentence for a knowledge base it does not
//! hold — BEFORE it interprets the members that only shape what the read reports. Such a
//! member identifies no resource, so it can neither make a resource exist nor hide that it
//! does not: which of the two modelled refusals a caller sees never depends on one. A
//! shaping leg this emulator cannot compute is still refused with the modelled
//! `ValidationException` (400) naming the member — once the resources have resolved.
//!
//! The tables span what the rule generalises over: WHICH shaping member the request carries
//! (both values of `GetDocumentContent`'s `outputFormat`, `userContext`, and `Retrieve`'s
//! guardrail, cursor, implicit-filter and reranking legs), which half of the resource
//! identity the plane does not hold (knowledge base, data source, document), and the form
//! of the identifier (bare id, ARN).
//!
//! Both harnesses beside this suite are reused rather than copied: the content-read one
//! (`tests/all/document_content.rs`, over the build-time plane, S3 and S3 Vectors) and the
//! retrieval one (`tests/all/knowledge_base_retrieval.rs`).

use serde_json::{json, Map, Value};

use crate::agent_memory::{Emulator, ACCOUNT_KEY, NOT_FOUND};
use crate::document_content::{
    inline_text_document, s3_document, UNKNOWN_DATA_SOURCE, UNKNOWN_KNOWLEDGE_BASE,
};
use crate::knowledge_base_retrieval::Emulator as RetrievalEmulator;
use crate::sessions::{call, error_code, label, REGION};

/// The modelled `ValidationException` and the status it carries, as the model binds it
/// (`smithy_query(query="operation",operation="GetDocumentContent")` -> ValidationException,
/// httpError 400).
const VALIDATION: (&str, u16) = ("ValidationException", 400);

/// A `knowledgeBaseId` matching `^[0-9a-zA-Z]{10}$` that no knowledge base here was minted
/// under, for the retrieval rows.
const UNRETRIEVABLE_KNOWLEDGE_BASE: &str = "ZZZZZZZZZZ";

/// The sentence this plane states for a knowledge base it does not hold: fixed text with
/// the request's own identifier as its one variable part.
fn knowledge_base_missing(identifier: &str) -> String {
    format!("Knowledge Base with id {identifier} does not exist")
}

/// One way a content read can be shaped, as the members the request carries beside the
/// path. Every one of them is modelled and optional
/// (`smithy_query(query="operation",operation="GetDocumentContent")` -> outputFormat
/// (DocumentOutputFormat: RAW | EXTRACTED), userContext (UserContext)).
struct Shaping {
    case: &'static str,
    output_format: Option<&'static str>,
    user_id: Option<&'static str>,
}

impl Shaping {
    /// The request body these shaping members travel in, or `None` for the read that
    /// carries none of them.
    fn body(&self) -> Option<Value> {
        let mut body = Map::new();
        if let Some(format) = self.output_format {
            body.insert("outputFormat".to_string(), json!(format));
        }
        if let Some(user_id) = self.user_id {
            body.insert("userContext".to_string(), json!({ "userId": user_id }));
        }
        (!body.is_empty()).then(|| Value::Object(body))
    }
}

/// Every shaping of a content read: no member at all, each value of the modelled
/// `DocumentOutputFormat`, the identity a read is scoped to, and the two together.
const CONTENT_SHAPINGS: [Shaping; 5] = [
    Shaping {
        case: "no shaping member at all",
        output_format: None,
        user_id: None,
    },
    Shaping {
        case: "the document as it was ingested",
        output_format: Some("RAW"),
        user_id: None,
    },
    Shaping {
        case: "the extracted rendering of the document",
        output_format: Some("EXTRACTED"),
        user_id: None,
    },
    Shaping {
        case: "a read scoped to an identity",
        output_format: None,
        user_id: Some("alice@example.com"),
    },
    Shaping {
        case: "the extracted rendering, scoped to an identity",
        output_format: Some("EXTRACTED"),
        user_id: Some("ops-oncall"),
    },
];

/// RULE: each half of a document's identity the build-time plane does not hold is the
/// modelled `ResourceNotFoundException` (404) naming what could not be found, WHATEVER
/// shaping members the request carries — a read whose knowledge base does not exist is
/// refused as a missing resource whether it asks for the raw document, the extracted
/// rendering, an identity-scoped read, or nothing at all.
#[tokio::test]
async fn a_content_read_answers_not_found_whatever_shapes_the_read() {
    let emulator = Emulator::new();
    let source = emulator.custom_source("resolution-order").await;
    emulator.put_object("held/order.txt", b"held", Some("text/plain"));
    emulator
        .ingest_documents(
            "the documents that exist",
            &source,
            json!([
                s3_document("doc-held", "held/order.txt"),
                inline_text_document("doc-inline", "coolant stays above 4 degrees"),
            ]),
        )
        .await;

    /// One read of something the plane does not hold, and the identifier the refusal has
    /// to name.
    struct Row {
        case: &'static str,
        knowledge_base: Option<&'static str>,
        data_source: Option<&'static str>,
        document: &'static str,
        names: &'static str,
    }

    let rows = [
        Row {
            case: "a knowledge base that was never created",
            knowledge_base: Some(UNKNOWN_KNOWLEDGE_BASE),
            data_source: None,
            document: "doc-held",
            names: UNKNOWN_KNOWLEDGE_BASE,
        },
        Row {
            case: "a data source that was never created",
            knowledge_base: None,
            data_source: Some(UNKNOWN_DATA_SOURCE),
            document: "doc-held",
            names: UNKNOWN_DATA_SOURCE,
        },
        Row {
            case: "a document that was never ingested",
            knowledge_base: None,
            data_source: None,
            document: "doc-never-ingested",
            names: "doc-never-ingested",
        },
    ];

    for row in &rows {
        for shaping in &CONTENT_SHAPINGS {
            let case = format!("{} asking for {}", row.case, shaping.case);
            let knowledge_base = row.knowledge_base.unwrap_or(&source.knowledge_base_id);
            let data_source = row.data_source.unwrap_or(&source.data_source_id);
            let reply = emulator
                .document_content(
                    ACCOUNT_KEY,
                    knowledge_base,
                    data_source,
                    row.document,
                    shaping.body(),
                )
                .await;
            assert_eq!(
                (reply.error_code(&case).as_str(), reply.status),
                NOT_FOUND,
                "case {case:?}: expected {NOT_FOUND:?}, got {} {:?}",
                reply.status,
                reply.json(&case)
            );
            assert!(
                reply.message(&case).contains(row.names),
                "case {case:?}: the refusal must name {:?}, got {:?}",
                row.names,
                reply.message(&case)
            );
            if row.knowledge_base == Some(UNKNOWN_KNOWLEDGE_BASE) {
                assert_eq!(
                    reply.message(&case),
                    knowledge_base_missing(UNKNOWN_KNOWLEDGE_BASE),
                    "case {case:?}: expected this plane's sentence for a knowledge base it \
                     does not hold, got {:?}",
                    reply.message(&case)
                );
            }
        }
    }
}

/// RULE: the shaping leg this emulator cannot compute — the parsed-text rendering of a
/// document, which no parser here produces — is still refused with the modelled
/// `ValidationException` (400) naming `outputFormat`, but only for a document that
/// RESOLVED; every other shaping of the same read is served.
#[tokio::test]
async fn the_rendering_this_service_cannot_produce_is_refused_once_the_document_resolves() {
    let emulator = Emulator::new();
    let source = emulator.custom_source("resolution-order-served").await;
    emulator.put_object("held/served.txt", b"held bytes", Some("text/plain"));
    emulator
        .ingest_documents(
            "the documents that exist",
            &source,
            json!([
                s3_document("doc-held", "held/served.txt"),
                inline_text_document("doc-inline", "the pump is serviced every 90 days"),
            ]),
        )
        .await;

    // Both ways a document reaches a knowledge base, so each row is read over each.
    for document in ["doc-held", "doc-inline"] {
        for shaping in &CONTENT_SHAPINGS {
            let case = format!("{document} asked for {}", shaping.case);
            let reply = emulator
                .document_content(
                    ACCOUNT_KEY,
                    &source.knowledge_base_id,
                    &source.data_source_id,
                    document,
                    shaping.body(),
                )
                .await;
            if shaping.output_format == Some("EXTRACTED") {
                assert_eq!(
                    (reply.error_code(&case).as_str(), reply.status),
                    VALIDATION,
                    "case {case:?}: expected {VALIDATION:?}, got {} {:?}",
                    reply.status,
                    reply.json(&case)
                );
                assert!(
                    reply.message(&case).contains("outputFormat"),
                    "case {case:?}: the refusal must name outputFormat, got {:?}",
                    reply.message(&case)
                );
            } else {
                assert_eq!(
                    reply.status,
                    200,
                    "case {case:?}: expected the read to be served, got {} {:?}",
                    reply.status,
                    reply.json(&case)
                );
                let answer = reply.json(&case);
                assert!(
                    answer["presignedUrl"].as_str().is_some_and(|url| !url.is_empty()),
                    "case {case:?}: expected an address to fetch the content from, got {answer:?}"
                );
            }
        }
    }
}

/// One way a retrieval can be shaped, beside the knowledge base it reads.
struct Retrieval {
    case: &'static str,
    /// The members the request carries beside `retrievalQuery`, as a JSON object.
    shaping: fn() -> Value,
    /// The member a refusal has to name for a knowledge base that DOES exist, or `None`
    /// for a shaping this service serves.
    refused_member: Option<&'static str>,
}

/// The retrieval shapings this suite reads the rule over: the guardrail leg, a cursor this
/// service never issued, the model-generated filter, the reranking leg, an `IMAGE` query
/// and an identity-scoped retrieval — every one of them a modelled optional member
/// (`smithy_query(query="operation",operation="Retrieve")`).
const RETRIEVAL_SHAPINGS: [Retrieval; 6] = [
    Retrieval {
        case: "a guardrailConfiguration, which asks for a guardrail policy",
        shaping: || {
            json!({
                "guardrailConfiguration": { "guardrailId": "abc123", "guardrailVersion": "DRAFT" },
            })
        },
        refused_member: Some("guardrailConfiguration"),
    },
    Retrieval {
        case: "a nextToken this service never issued",
        shaping: || json!({ "nextToken": "not-a-token-from-here" }),
        refused_member: Some("nextToken"),
    },
    Retrieval {
        case: "an implicitFilterConfiguration, which asks for a model-generated filter",
        shaping: || {
            json!({
                "retrievalConfiguration": {
                    "vectorSearchConfiguration": {
                        "implicitFilterConfiguration": {
                            "metadataAttributes": [
                                {
                                    "key": "department",
                                    "type": "STRING",
                                    "description": "the owning team",
                                },
                            ],
                            "modelArn": "arn:aws:bedrock:us-west-2::foundation-model/amazon.nova-lite-v1:0",
                        },
                    },
                },
            })
        },
        refused_member: Some("implicitFilterConfiguration"),
    },
    Retrieval {
        case: "a rerankingConfiguration, which asks a model to reorder the results",
        shaping: || {
            json!({
                "retrievalConfiguration": {
                    "vectorSearchConfiguration": {
                        "numberOfResults": 10,
                        "rerankingConfiguration": {
                            "type": "BEDROCK_RERANKING_MODEL",
                            "bedrockRerankingConfiguration": {
                                "modelConfiguration": {
                                    "modelArn": "arn:aws:bedrock:us-west-2::foundation-model/amazon.rerank-v1:0",
                                },
                                "numberOfRerankedResults": 2,
                            },
                        },
                    },
                },
            })
        },
        refused_member: Some("rerankingConfiguration"),
    },
    Retrieval {
        case: "an IMAGE query, which no embedding model here ranks",
        shaping: || {
            json!({
                "retrievalQuery": {
                    "type": "IMAGE",
                    "text": "launch code",
                    "image": { "format": "png", "inlineContent": "iVBORw0=" },
                },
            })
        },
        refused_member: Some("retrievalQuery.type"),
    },
    Retrieval {
        case: "a retrieval scoped to an identity",
        shaping: || json!({ "userContext": { "userId": "alice@example.com" } }),
        refused_member: None,
    },
];

/// The request body one retrieval shaping travels in: the shaping's own members over the
/// `retrievalQuery` every retrieval carries, which the shaping may replace.
fn retrieval_body(shaping: &Retrieval) -> Value {
    let mut body = json!({ "retrievalQuery": { "text": "launch code" } });
    let object = body
        .as_object_mut()
        .expect("the retrieval body is a JSON object");
    for (member, value) in (shaping.shaping)()
        .as_object()
        .expect("a shaping is a JSON object")
    {
        object.insert(member.clone(), value.clone());
    }
    body
}

/// RULE (the same one, on the other knowledge-base read): a retrieval from a knowledge base
/// the Bedrock Agent plane does not hold answers the modelled `ResourceNotFoundException`
/// (404) with this plane's "Knowledge Base with id <id> does not exist" sentence, whatever
/// retrieval-shaping members the request carries and in whichever form the identifier was
/// written; the shaping leg this service cannot compute is refused with the modelled
/// `ValidationException` (400) naming the member only for a knowledge base that exists.
#[tokio::test]
async fn a_retrieval_resolves_its_knowledge_base_before_the_members_that_shape_it() {
    let emulator = RetrievalEmulator::new("resolution-index");
    let existing = emulator
        .knowledge_base("resolution-kb", "resolution-index")
        .await;

    // Both forms the modelled `KnowledgeBaseIdentifier` admits, for an id nothing created.
    let unknown = [
        UNRETRIEVABLE_KNOWLEDGE_BASE.to_string(),
        format!(
            "arn:aws:bedrock:{REGION}:000000000000:knowledge-base/{UNRETRIEVABLE_KNOWLEDGE_BASE}"
        ),
    ];

    for shaping in &RETRIEVAL_SHAPINGS {
        for identifier in &unknown {
            let case = format!("{} against {identifier}", shaping.case);
            let (status, body) = call(
                &emulator.router,
                "POST",
                &format!("/knowledgebases/{}/retrieve", label(identifier)),
                retrieval_body(shaping),
            )
            .await;
            assert_eq!(
                (error_code(&body), status),
                NOT_FOUND,
                "case {case:?}: expected {NOT_FOUND:?}, got {status} {body:?}"
            );
            assert_eq!(
                body["message"].as_str(),
                Some(knowledge_base_missing(identifier).as_str()),
                "case {case:?}: expected this plane's sentence for a knowledge base it does \
                 not hold, got {body:?}"
            );
        }

        // The same request against the knowledge base that EXISTS, which is what proves
        // the answers above were about the resource and not about the shaping member.
        let case = format!("{} against the knowledge base that exists", shaping.case);
        let (status, body) = call(
            &emulator.router,
            "POST",
            &format!("/knowledgebases/{existing}/retrieve"),
            retrieval_body(shaping),
        )
        .await;
        match shaping.refused_member {
            Some(member) => {
                assert_eq!(
                    (error_code(&body), status),
                    VALIDATION,
                    "case {case:?}: expected {VALIDATION:?}, got {status} {body:?}"
                );
                assert!(
                    body["message"]
                        .as_str()
                        .is_some_and(|message| message.contains(member)),
                    "case {case:?}: the refusal must name {member:?}, got {body:?}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the retrieval to be served, got {status} {body:?}"
                );
                assert!(
                    body["retrievalResults"].is_array(),
                    "case {case:?}: expected a retrievalResults list, got {body:?}"
                );
            }
        }
    }
}
