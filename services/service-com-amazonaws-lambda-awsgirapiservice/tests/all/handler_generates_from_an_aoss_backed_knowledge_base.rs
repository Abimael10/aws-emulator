// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING `python3.x` function gets back when its handler asks a Bedrock knowledge
//! base — whose vector store is an index in an OpenSearch Serverless COLLECTION — to GENERATE
//! an answer from the passages it holds, end to end over the wire: AWS Lambda, the Bedrock
//! Agent build-time plane, the Bedrock Agent runtime plane, OpenSearch Serverless, IAM and the
//! token service on ONE engine.
//!
//! RULE: a managed runtime ships the AWS SDK with the service data of every service its call
//! seam reaches and dispatches the handler's call as the function's own execution-role
//! session, so a handler's `RetrieveAndGenerate` observes the runtime plane's OWN answer — a
//! response whose `sessionId` is one that plane minted or continued and whose every citation
//! quotes, character for character, a passage the ingestion really recorded for a document of
//! that knowledge base, when the role is granted the action; and that plane's own modelled
//! refusal (403 `AccessDeniedException` for a role the account grants the action to nowhere,
//! 404 `ResourceNotFoundException` for a well-formed identifier that resolves to nothing)
//! when it is not. A client-side failure of the SDK itself — `UnknownServiceError`, raised
//! before anything is signed — is neither, and tells the handler nothing about the service.
//!
//! The rows span what the rule generalises over: the bytes the queried document carries
//! (ASCII, four-byte UTF-8, and a combining sequence), the form `modelArn` names the
//! generating model in (a bare model id and the published foundation-model ARN) and which
//! model that is, whether the request OPENS a session or continues the one the plane handed
//! back, how the grant in force is stated (the action allowed, no Bedrock action stated at
//! all, and the action allowed AND explicitly denied), and whether the knowledge base the
//! call names resolves. None of them may change that the deployment package RAN — every
//! row's answer is the handler's own return value, never a runtime fault.
//!
//! The RETRIEVAL half of the same chain is
//! `crate::handler_retrieves_an_aoss_backed_knowledge_base`, whose harness this suite stands
//! its collection, index, knowledge base and function up with rather than copying it; the
//! citation-span rule of one response, and the legs that plane refuses, are pinned per class
//! in the Bedrock Agent runtime crate's own suites. What is pinned HERE is the generation
//! chain as a function sees it, which no suite drives.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `max_concurrent_runs` invocations at once for the whole process.

use serde_json::{json, Value};

use crate::event_source_mappings::call;
use crate::handler_retrieves_an_aoss_backed_knowledge_base::{
    deployed_function, emulator, ingested, ingested_knowledge_base, knowledge_base_arn, logs_only,
    policy, published_collection, role_policy, session_principal,
    vector_index_at_the_published_endpoint, ABSENT_KNOWLEDGE_BASE, ACCESS_DENIED,
    ACCESS_DENIED_STATUS, DOCUMENTS, FUNCTION, NOT_FOUND, NOT_FOUND_STATUS, REGION,
};

/// Where `Invoke` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The IAM action a knowledge-base generation is decided against, as a policy statement
/// spells it (`bedrock:<operation>`).
const GENERATE_ACTION: &str = "bedrock:RetrieveAndGenerate";

/// Two text models the Bedrock Runtime plane of this emulator publishes, taken from that
/// plane's own catalogue
/// (`service-com-amazonaws-bedrockruntime-amazonbedrockfrontendservice`, `state::model_catalog`)
/// rather than from a list written here: `modelArn` is "the ARN of the foundation model or
/// inference profile used to generate a response"
/// (`smithy_query(query="shape",shape="KnowledgeBaseRetrieveAndGenerateConfiguration")`), so a
/// row names one of them, and neither is the model the retrieval half of this chain cites.
const MODEL: &str = "anthropic.claude-3-5-sonnet-20240620-v1:0";
const SECOND_MODEL: &str = "amazon.nova-lite-v1:0";

/// What a `sessionId` the plane answers with has to be: `SessionId`, len 2..100, pattern
/// `^[0-9a-zA-Z._:-]+$`
/// (`smithy_query(query="operation",operation="RetrieveAndGenerate")`, whose output member is
/// `@required`).
const SESSION_ID_MIN: usize = 2;
const SESSION_ID_MAX: usize = 100;

/// The handler the function runs: it builds the Bedrock Agent runtime client, asks the one
/// generation the event names, and reports what the SDK gave it — the response text, the
/// session it ran in and each citation's own quoted segment, span and references, or the error
/// code, message and status of a refusal — so a refusal is the handler's own answer rather
/// than a fault of the run.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    client = boto3.client("bedrock-agent-runtime")
    request = {
        "input": {"text": event["query"]},
        "retrieveAndGenerateConfiguration": {
            "type": "KNOWLEDGE_BASE",
            "knowledgeBaseConfiguration": {
                "knowledgeBaseId": event["knowledgeBaseId"],
                "modelArn": event["modelArn"],
            },
        },
    }
    if event.get("sessionId"):
        request["sessionId"] = event["sessionId"]
    try:
        answered = client.retrieve_and_generate(**request)
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "raised": "ClientError",
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    except botocore.exceptions.BotoCoreError as error:
        return {"ok": False, "raised": type(error).__name__, "message": str(error)}
    citations = []
    for citation in answered.get("citations") or []:
        part = (citation.get("generatedResponsePart") or {}).get("textResponsePart") or {}
        span = part.get("span") or {}
        references = []
        for reference in citation.get("retrievedReferences") or []:
            location = reference.get("location") or {}
            custom = location.get("customDocumentLocation") or {}
            references.append(
                {
                    "text": (reference.get("content") or {}).get("text"),
                    "id": custom.get("id"),
                    "locationType": location.get("type"),
                }
            )
        citations.append(
            {
                "quoted": part.get("text"),
                "start": span.get("start"),
                "end": span.get("end"),
                "references": references,
            }
        )
    return {
        "ok": True,
        "answer": answered["output"]["text"],
        "sessionId": answered["sessionId"],
        "citations": citations,
    }
"#;

/// The sentence IAM refuses a generation with, built here from the principal, the action and
/// the resource rather than read off the crates under test.
fn denial(resource: &str, reason: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {GENERATE_ACTION} on resource: {resource} \
         {reason}",
        session_principal()
    )
}

/// The grant a row runs under: the action allowed.
fn allow_generation() -> String {
    policy(json!([{ "Effect": "Allow", "Action": [GENERATE_ACTION], "Resource": "*" }]))
}

/// The action allowed AND explicitly denied, which IAM resolves to a denial whatever else
/// allows it.
fn allow_and_deny_generation() -> String {
    policy(json!([
        { "Effect": "Allow", "Action": [GENERATE_ACTION], "Resource": "*" },
        { "Effect": "Deny", "Action": [GENERATE_ACTION], "Resource": "*" },
    ]))
}

/// How a row names the model its response is generated with. Both forms satisfy the modelled
/// `BedrockModelArn` pattern.
#[derive(Debug)]
enum Model {
    /// The model's own id, as a caller holding only that writes it.
    Id(&'static str),
    /// The published ARN of that same foundation model.
    FoundationModelArn(&'static str),
}

impl Model {
    /// The `modelArn` a row sends.
    fn named(&self) -> String {
        match self {
            Model::Id(id) => (*id).to_string(),
            Model::FoundationModelArn(id) => {
                format!("arn:aws:bedrock:{REGION}::foundation-model/{id}")
            }
        }
    }
}

/// Which knowledge base a row's call names.
#[derive(Debug)]
enum Target {
    /// The one this suite built over the collection's index.
    Held,
    /// A well-formed identifier nothing holds.
    Absent,
}

/// Which session a row's call runs in.
#[derive(Debug, PartialEq, Eq)]
enum Session {
    /// The request carries none, so the plane OPENS one.
    Opened,
    /// The request carries the id the plane last handed back, so that session CONTINUES.
    Continued,
}

/// What a row's call must come back as.
#[derive(Debug)]
enum Expected {
    /// The generation was served, and the document whose ingested passage it must cite.
    Generated { document: &'static str },
    /// The plane refused it with this error code and HTTP status, and — for an authorization
    /// refusal — IAM's own sentence about this action and resource, with `reason` as its tail.
    Refused {
        code: &'static str,
        status: i64,
        reason: Option<&'static str>,
    },
}

/// One row: the grant in force, the knowledge base named, the query sent, the model it is
/// generated with, the session it runs in, and what the handler must observe.
struct Row {
    case: &'static str,
    policy: fn() -> String,
    target: Target,
    /// The document whose own bytes the row queries with.
    query: &'static str,
    model: Model,
    session: Session,
    expected: Expected,
}

/// The rows, spanning the bytes the queried document carries, the form and identity of the
/// generating model, whether the session is opened or continued, how the grant in force is
/// stated, and whether the knowledge base the call names resolves.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "ascii bytes generated from, by a role the account grants the action to",
            policy: allow_generation,
            target: Target::Held,
            query: "doc-ascii",
            model: Model::Id(MODEL),
            session: Session::Opened,
            expected: Expected::Generated {
                document: "doc-ascii",
            },
        },
        Row {
            case: "four-byte UTF-8 bytes, in the session the plane just opened",
            policy: allow_generation,
            target: Target::Held,
            query: "doc-emoji",
            model: Model::Id(MODEL),
            session: Session::Continued,
            expected: Expected::Generated {
                document: "doc-emoji",
            },
        },
        Row {
            case: "a combining sequence, with the model named by its published ARN",
            policy: allow_generation,
            target: Target::Held,
            query: "doc-combining",
            model: Model::FoundationModelArn(MODEL),
            session: Session::Opened,
            expected: Expected::Generated {
                document: "doc-combining",
            },
        },
        Row {
            case: "ascii bytes generated with a second published model",
            policy: allow_generation,
            target: Target::Held,
            query: "doc-ascii",
            model: Model::FoundationModelArn(SECOND_MODEL),
            session: Session::Opened,
            expected: Expected::Generated {
                document: "doc-ascii",
            },
        },
        Row {
            case: "the same generation by a role whose policy names no Bedrock action",
            policy: logs_only,
            target: Target::Held,
            query: "doc-ascii",
            model: Model::Id(MODEL),
            session: Session::Opened,
            expected: Expected::Refused {
                code: ACCESS_DENIED,
                status: ACCESS_DENIED_STATUS,
                reason: Some(
                    "because no identity-based policy allows the bedrock:RetrieveAndGenerate \
                     action",
                ),
            },
        },
        Row {
            case: "the same generation by a role granted the action and explicitly denied it",
            policy: allow_and_deny_generation,
            target: Target::Held,
            query: "doc-ascii",
            model: Model::Id(MODEL),
            session: Session::Opened,
            expected: Expected::Refused {
                code: ACCESS_DENIED,
                status: ACCESS_DENIED_STATUS,
                reason: Some(
                    "with an explicit deny in an identity-based policy for the \
                     bedrock:RetrieveAndGenerate action",
                ),
            },
        },
        Row {
            case: "a granted role's generation from a knowledge base nobody created",
            policy: allow_generation,
            target: Target::Absent,
            query: "doc-ascii",
            model: Model::Id(MODEL),
            session: Session::Opened,
            expected: Expected::Refused {
                code: NOT_FOUND,
                status: NOT_FOUND_STATUS,
                reason: None,
            },
        },
    ]
}

/// Every citation the handler reported, checked against the rule: its span indexes the
/// response the handler got back, and each reference it carries is a passage this suite really
/// ingested, cited as the document it was pushed in under.
///
/// Answers the ids of the documents the response cites, so a row can say which document its
/// answer had to be grounded in.
fn cited_documents(case: &str, answer: &str, citations: &[Value]) -> Vec<String> {
    let characters: Vec<char> = answer.chars().collect();
    let mut cited = Vec::new();
    for citation in citations {
        let quoted = citation["quoted"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: expected the cited segment's own text, got {citation}")
        });
        let start = citation["start"].as_u64().unwrap_or_else(|| {
            panic!("case {case:?}: expected the cited segment's span start, got {citation}")
        }) as usize;
        let end = citation["end"].as_u64().unwrap_or_else(|| {
            panic!("case {case:?}: expected the cited segment's span end, got {citation}")
        }) as usize;
        assert!(
            start < end && end <= characters.len(),
            "case {case:?}: expected a span inside the {} characters of the response, got start \
             {start} end {end}",
            characters.len()
        );
        let indexed: String = characters[start..end].iter().collect();
        assert_eq!(
            indexed, quoted,
            "case {case:?}: expected the span to index the cited segment, got {indexed:?} at \
             start {start} end {end}"
        );
        let references = citation["references"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            !references.is_empty(),
            "case {case:?}: expected the passage the segment was quoted from, got {citation}"
        );
        for reference in &references {
            let passage = reference["text"].as_str().unwrap_or_default();
            let id = reference["id"].as_str().unwrap_or_default();
            // Nothing is invented: every passage cited is a document this suite really pushed
            // into the knowledge base, cited under the id it was pushed in with.
            assert_eq!(
                passage,
                ingested(id),
                "case {case:?}: expected the passage the ingestion recorded for {id:?}, got \
                 {reference}"
            );
            assert_eq!(
                reference["locationType"],
                json!("CUSTOM"),
                "case {case:?}: expected a document of a custom data source to be cited as \
                 CUSTOM, got {reference}"
            );
            assert!(
                passage.starts_with(quoted),
                "case {case:?}: expected the cited segment to open the passage it was quoted \
                 from, got {quoted:?} of {passage:?}"
            );
            cited.push(id.to_string());
        }
    }
    cited
}

/// RULE: an index created at a collection's own published endpoint backs a knowledge base, and
/// a RUNNING function's handler has the runtime plane GENERATE from it as its own
/// execution-role session — so what the handler reports is that plane's own answer: a response
/// grounded in the passages the ingestion recorded, in a session the plane minted or
/// continued, where the role is granted the action, and the plane's own refusal (403 for a
/// missing or denied grant, 404 for an identifier that resolves to nothing) where it is not.
#[tokio::test]
async fn a_handler_generates_from_the_knowledge_base_backed_by_its_collections_index() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = emulator();
    let (endpoint, collection_arn) = published_collection(&router).await;
    vector_index_at_the_published_endpoint(&router, &endpoint).await;
    let knowledge_base_id = ingested_knowledge_base(&router, &collection_arn).await;
    deployed_function(&router, HANDLER_SOURCE).await;

    // The session the plane last handed back, which a `Session::Continued` row carries, and
    // every document the served rows cited between them.
    let mut opened: Option<String> = None;
    let mut cited_across_rows: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for row in rows() {
        let case = row.case;
        role_policy(&router, case, &(row.policy)()).await;
        let named = match row.target {
            Target::Held => knowledge_base_id.clone(),
            Target::Absent => ABSENT_KNOWLEDGE_BASE.to_string(),
        };
        let mut event = json!({
            "knowledgeBaseId": named,
            "query": ingested(row.query),
            "modelArn": row.model.named(),
        });
        if row.session == Session::Continued {
            let carried = opened.clone().unwrap_or_else(|| {
                panic!("case {case:?}: expected an earlier row to have opened a session")
            });
            event["sessionId"] = json!(carried);
        }
        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
            &event,
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
            answered.status,
            answered.text()
        );
        // The DEPLOYMENT PACKAGE RAN: every row's answer below is the handler's own return
        // value, so a run that faulted is not a row's outcome.
        assert_eq!(
            answered.header("x-amz-function-error"),
            None,
            "case {case:?}: expected the handler to run to completion, got {}",
            answered.text()
        );
        let payload = answered.json();
        assert_ne!(
            payload["raised"].as_str(),
            Some("UnknownServiceError"),
            "case {case:?}: expected the client to be built and the call dispatched, got {payload}"
        );

        match row.expected {
            Expected::Generated { document } => {
                assert_eq!(
                    payload["ok"],
                    json!(true),
                    "case {case:?}: expected the generation to be served, got {payload}"
                );
                let answer = payload["answer"].as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: expected the generated response text, got {payload}")
                });
                let citations = payload["citations"].as_array().cloned().unwrap_or_default();
                assert!(
                    !citations.is_empty(),
                    "case {case:?}: expected the response to cite the passages it was generated \
                     from, got {payload}"
                );
                let cited = cited_documents(case, answer, &citations);
                cited_across_rows.extend(cited.iter().cloned());
                assert!(
                    cited.iter().any(|id| id == document),
                    "case {case:?}: expected the response to cite the document {document:?}, got \
                     {cited:?} in {payload}"
                );
                // The session is the plane's own, and a row that carried one back ran in THAT
                // session rather than in a second one the plane opened beside it.
                let session = payload["sessionId"].as_str().unwrap_or_else(|| {
                    panic!("case {case:?}: expected the plane's own sessionId, got {payload}")
                });
                let length = session.chars().count();
                assert!(
                    (SESSION_ID_MIN..=SESSION_ID_MAX).contains(&length)
                        && session
                            .chars()
                            .all(|at| at.is_ascii_alphanumeric() || "._:-".contains(at)),
                    "case {case:?}: expected a sessionId the model admits, got {session:?}"
                );
                if row.session == Session::Continued {
                    assert_eq!(
                        Some(session.to_string()),
                        opened,
                        "case {case:?}: expected the session the request carried to be \
                         continued, got {payload}"
                    );
                }
                opened = Some(session.to_string());
            }
            Expected::Refused {
                code,
                status,
                reason,
            } => {
                assert_eq!(
                    payload["ok"],
                    json!(false),
                    "case {case:?}: expected the plane's own refusal, got {payload}"
                );
                assert_eq!(
                    payload["raised"],
                    json!("ClientError"),
                    "case {case:?}: expected the refusal to reach the handler as the SDK's \
                     service error, got {payload}"
                );
                assert_eq!(
                    payload["code"],
                    json!(code),
                    "case {case:?}: expected the refusal {code:?}, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(status),
                    "case {case:?}: expected HTTP {status} for that refusal, got {payload}"
                );
                match reason {
                    Some(reason) => assert_eq!(
                        payload["message"],
                        json!(denial(&knowledge_base_arn(&named), reason)),
                        "case {case:?}: expected IAM's own denial sentence, got {payload}"
                    ),
                    None => assert!(
                        payload["message"]
                            .as_str()
                            .is_some_and(|message| !message.is_empty()),
                        "case {case:?}: expected the plane's own message, got {payload}"
                    ),
                }
            }
        }
    }

    // The rows really do span every encoding the chain has to carry through: each document
    // the ingestion recorded was cited, character for character, by one of them.
    for (id, _) in DOCUMENTS {
        assert!(
            cited_across_rows.contains(*id),
            "expected some row's generated response to cite the ingested document {id:?}, got \
             {cited_across_rows:?}"
        );
    }
}
