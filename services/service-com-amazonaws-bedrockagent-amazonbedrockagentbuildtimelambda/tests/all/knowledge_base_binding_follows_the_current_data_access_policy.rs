// SPDX-License-Identifier: CC0-1.0

//! An `OPENSEARCH_SERVERLESS` binding is decided against the collection's data access
//! policies AS THEY STAND when the call is made — and the call is ANSWERED either way.
//!
//! Amazon Bedrock reads and writes a knowledge base's vector index as the role the request
//! names, so `CreateKnowledgeBase` resolves the index its `storageConfiguration` names
//! against the owning collection's own `data` access policies and answers synchronously:
//! `202` where those policies grant that role the permissions a knowledge base performs on
//! the index, and `400 ValidationException` with the invalid-storage-configuration refusal
//! where they do not. Nothing about that decision is deferred or cached, so a write that
//! changes what those policies grant changes what the IDENTICAL next create answers.
//!
//! The sibling suite `knowledge_base_role_and_vector_store_access` pins the decision over
//! policies that were in place before the first call; this one pins it over the policy
//! WRITES that move a collection from one state to the other — a create, an update that
//! widens, narrows or re-principals a grant, an update that touches only the description,
//! and a delete — by making the same `CreateKnowledgeBase` request twice, once on each side
//! of the write, and asserting the answer each time. Every accepted create is read back
//! through `GetKnowledgeBase` and polled to `ACTIVE`, so an acceptance is backed by a
//! knowledge base that really carries the binding.
//!
//! Every call goes through the router, against the real IAM and OpenSearch Serverless
//! services on the same engine, so the records these rules are decided against are the ones
//! those services' own operations wrote.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every call of this suite is made in.
const REGION: &str = "us-east-1";

/// The embedding model every row names: a published model whose output modality is an
/// embedding, so no row is refused on the model.
const EMBEDDING_MODEL: &str =
    "arn:aws:bedrock:us-east-1::foundation-model/amazon.titan-embed-text-v2:0";

/// The vector index every row binds its knowledge base to.
const VECTOR_INDEX: &str = "bedrock-knowledge-base-index";

/// The `ValidationException` message Amazon Bedrock answers when the vector store refuses
/// the knowledge base's own role.
const UNREACHABLE_STORAGE: &str = "The knowledge base storage configuration provided is \
                                   invalid... Dependency error document status code: 403, \
                                   error message: Bad Authorization";

/// The index-level data access permissions a knowledge base performs on its vector index —
/// it reads the index's definition to check the field mapping, writes a document per
/// ingested chunk and reads them back for a retrieval.
///
/// Source: the permissions the owning service publishes as
/// `state::data_access::permission::{DESCRIBE_INDEX, READ_DOCUMENT, WRITE_DOCUMENT}`, from
/// AWS's supported data-access policy permissions.
const KNOWLEDGE_BASE_PERMISSIONS: &[&str] = &[
    "aoss:DescribeIndex",
    "aoss:ReadDocument",
    "aoss:WriteDocument",
];

/// Every `aoss` data action, the whole-element wildcard a policy may grant instead of
/// naming each permission.
const EVERY_PERMISSION: &[&str] = &["aoss:*"];

/// How long a row waits for a created knowledge base to reach `ACTIVE`: generously past the
/// activation span the clock owes it, so a loaded host does not decide the outcome.
const ACTIVE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Which identity a data access policy statement names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// The knowledge base's own role — the principal the binding is decided for.
    KnowledgeBaseRole,
    /// The harness's own identity, which is not the knowledge base's role.
    CallerIdentity,
    /// A third identity, neither of the two above.
    OtherIdentity,
}

/// One data access policy statement: the permissions it grants on the collection's indexes
/// and the identities it names.
#[derive(Debug, Clone, Copy)]
struct Grant {
    permissions: &'static [&'static str],
    principals: &'static [Named],
}

/// What happens to the collection's data access policy between the two identical
/// `CreateKnowledgeBase` calls.
#[derive(Debug, Clone, Copy)]
enum Write {
    /// `CreateAccessPolicy` of a `data` policy carrying this grant.
    Create(Grant),
    /// `UpdateAccessPolicy` replacing the stored document with this grant.
    Replace(Grant),
    /// `UpdateAccessPolicy` carrying only a new description, so the document is untouched.
    Describe,
    /// `DeleteAccessPolicy` of the stored policy.
    Delete,
}

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The restJson1 error shape name, from `x-amzn-errortype` or the body's `__type`;
    /// `None` on a success.
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    fn member(&self, path: &[&str]) -> &Value {
        let mut at = &self.body;
        for key in path {
            at = &at[*key];
        }
        at
    }

    fn text(&self, path: &[&str]) -> String {
        self.member(path).as_str().unwrap_or_default().to_string()
    }
}

/// A SigV4 `Authorization` header scoped to one signing name, which is the routing key the
/// addressed service claims on.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0123456789abcdef"
    )
}

/// This service, IAM and OpenSearch Serverless on ONE engine — the shape
/// `core/router/src/lib.rs` mounts them in.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_opensearchserverless_opensearchserverless::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router }
    }

    /// One restJson1 call to this service.
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let bytes = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("serialize request body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("bedrock-agent.{REGION}.amazonaws.com"))
            .header("authorization", authorization("bedrock"))
            .header("content-type", "application/json")
            .body(bytes)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_error = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let raw = response.into_body();
        let body: Value = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or_else(|error| {
                panic!(
                    "{method} {uri} -> {status} body is not JSON ({error}): {}",
                    String::from_utf8_lossy(&raw)
                )
            })
        };
        let error_type = header_error.or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        });
        Answer {
            status,
            error_type,
            body,
        }
    }

    async fn create_knowledge_base(&self, request: Value) -> Answer {
        self.call("PUT", "/knowledgebases/", Some(request)).await
    }

    async fn get_knowledge_base(&self, knowledge_base_id: &str) -> Answer {
        self.call("GET", &format!("/knowledgebases/{knowledge_base_id}"), None)
            .await
    }

    /// The names `ListKnowledgeBases` reports.
    async fn knowledge_base_names(&self) -> Vec<String> {
        let listed = self.call("POST", "/knowledgebases/", Some(json!({}))).await;
        assert_eq!(
            listed.status, 200,
            "case setup: expected ListKnowledgeBases -> 200, got {listed:?}"
        );
        listed
            .member(&["knowledgeBaseSummaries"])
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .map(|summary| summary["name"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// One IAM call: IAM speaks `awsQuery`, so its parameters travel as a form body and its
    /// answer comes back as XML.
    async fn iam(&self, action: &str, params: &[(&str, &str)]) -> (u16, String) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("iam.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", authorization("iam"))
            .body(Bytes::from(body))
            .expect("build the IAM request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (
            status,
            String::from_utf8_lossy(&response.into_body()).to_string(),
        )
    }

    /// One OpenSearch Serverless call, over its own `awsJson1_0` front door.
    async fn aoss(&self, operation: &str, body: Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("aoss.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("OpenSearchServerless.{operation}"))
            .header("authorization", authorization("aoss"))
            .body(Bytes::from(
                serde_json::to_vec(&body).expect("serialize the request body"),
            ))
            .expect("build the aoss request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed: Value = serde_json::from_slice(&response.into_body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// Create an IAM role trusted by `bedrock.amazonaws.com`, answering the ARN IAM
    /// persisted.
    async fn given_role(&self, name: &str) -> String {
        let (status, created) = self
            .iam(
                "CreateRole",
                &[
                    ("RoleName", name),
                    ("AssumeRolePolicyDocument", &admitting_trust()),
                ],
            )
            .await;
        assert_eq!(
            status, 200,
            "case CreateRole({name}): expected 200, got {status} {created}"
        );
        let arn = xml_text(&created, "Arn");
        assert!(
            !arn.is_empty(),
            "case CreateRole({name}): expected an ARN, got {created}"
        );
        arn
    }

    /// Create a VECTORSEARCH collection, answering the ARN its create reported.
    async fn given_collection(&self, name: &str) -> String {
        let (status, created) = self
            .aoss(
                "CreateCollection",
                json!({
                    "name": name,
                    "type": "VECTORSEARCH",
                    "encryptionConfig": { "aWSOwnedKey": true },
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case CreateCollection({name}): expected 200, got {status} {created}"
        );
        let arn = created["createCollectionDetail"]["arn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !arn.is_empty(),
            "case CreateCollection({name}): expected an ARN, got {created}"
        );
        arn
    }

    /// Create a `data` access policy, answering the `policyVersion` it was stored under.
    async fn create_access_policy(&self, name: &str, policy: &Value) -> String {
        let (status, created) = self
            .aoss(
                "CreateAccessPolicy",
                json!({ "type": "data", "name": name, "policy": policy.to_string() }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case CreateAccessPolicy(data/{name}): expected 200, got {status} {created}"
        );
        let version = created["accessPolicyDetail"]["policyVersion"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !version.is_empty(),
            "case CreateAccessPolicy(data/{name}): expected a policyVersion, got {created}"
        );
        version
    }

    /// Update a `data` access policy, answering the `policyVersion` the write minted.
    async fn update_access_policy(&self, name: &str, request: Value) -> String {
        let (status, updated) = self.aoss("UpdateAccessPolicy", request).await;
        assert_eq!(
            status, 200,
            "case UpdateAccessPolicy(data/{name}): expected 200, got {status} {updated}"
        );
        updated["accessPolicyDetail"]["policyVersion"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// Delete a `data` access policy.
    async fn delete_access_policy(&self, name: &str) {
        let (status, deleted) = self
            .aoss(
                "DeleteAccessPolicy",
                json!({ "type": "data", "name": name }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case DeleteAccessPolicy(data/{name}): expected 200, got {status} {deleted}"
        );
    }

    /// The status of the knowledge base `knowledge_base_id` once it is no longer in the
    /// transient `CREATING` status, or the last status seen when the deadline passes.
    ///
    /// Waits for the CONDITION — a settled status — rather than for a fixed time, so the
    /// row's outcome does not depend on how fast the host runs.
    async fn settled_status(&self, knowledge_base_id: &str) -> String {
        let deadline = Instant::now() + ACTIVE_DEADLINE;
        loop {
            let fetched = self.get_knowledge_base(knowledge_base_id).await;
            let status = fetched.text(&["knowledgeBase", "status"]);
            if status != "CREATING" || Instant::now() >= deadline {
                return status;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// Percent-encode one form value the way an AWS SDK encodes an `awsQuery` body.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(*byte));
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// The text of the first `<tag>` of an XML body.
fn xml_text(xml: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let Some(start) = xml.find(&open).map(|at| at + open.len()) else {
        return String::new();
    };
    let Some(end) = xml[start..].find(&close).map(|at| at + start) else {
        return String::new();
    };
    xml[start..end].to_string()
}

/// The trust policy a knowledge-base role carries on AWS: `bedrock.amazonaws.com` may
/// assume it, so no row of this suite is refused on the role itself.
fn admitting_trust() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "bedrock.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The identity ARNs one grant names, in the order it names them.
fn principal_arns(grant: &Grant, identities: &[(Named, String)]) -> Vec<String> {
    grant
        .principals
        .iter()
        .map(|named| {
            identities
                .iter()
                .find(|(which, _)| which == named)
                .map(|(_, arn)| arn.clone())
                .expect("every named identity of a row is created by its setup")
        })
        .collect()
}

/// A `data` access policy document granting `grant` on the indexes of `collection` — the
/// shape AWS's data-access documentation writes.
fn data_policy(collection: &str, grant: &Grant, identities: &[(Named, String)]) -> Value {
    json!([{
        "Rules": [{
            "ResourceType": "index",
            "Resource": [format!("index/{collection}/*")],
            "Permission": grant.permissions,
        }],
        "Principal": principal_arns(grant, identities),
    }])
}

/// A `CreateKnowledgeBase` body naming `role_arn` and the OpenSearch Serverless collection
/// `collection_arn`: identical across a row's two legs but for the knowledge base's name.
fn create_request(name: &str, role_arn: &str, collection_arn: &str) -> Value {
    json!({
        "name": name,
        "roleArn": role_arn,
        "knowledgeBaseConfiguration": {
            "type": "VECTOR",
            "vectorKnowledgeBaseConfiguration": { "embeddingModelArn": EMBEDDING_MODEL },
        },
        "storageConfiguration": {
            "type": "OPENSEARCH_SERVERLESS",
            "opensearchServerlessConfiguration": {
                "collectionArn": collection_arn,
                "vectorIndexName": VECTOR_INDEX,
                "fieldMapping": {
                    "vectorField": "vector",
                    "textField": "text",
                    "metadataField": "metadata",
                },
            },
        },
    })
}

/// Assert one leg's answer: an accepted create is read back and settles in `ACTIVE`
/// carrying the binding the request named, a refused one answers the modelled
/// `ValidationException` and leaves no knowledge base behind.
async fn assert_leg(
    emulator: &Emulator,
    case: &str,
    leg: &str,
    name: &str,
    collection_arn: &str,
    answered: Answer,
    accepted: bool,
) {
    if accepted {
        assert_eq!(
            answered.status, 202,
            "case {case} ({leg} the policy write): expected CreateKnowledgeBase -> 202, got \
             {answered:?}"
        );
        let knowledge_base_id = answered.text(&["knowledgeBase", "knowledgeBaseId"]);
        assert!(
            !knowledge_base_id.is_empty(),
            "case {case} ({leg} the policy write): expected a knowledgeBaseId, got {answered:?}"
        );
        let fetched = emulator.get_knowledge_base(&knowledge_base_id).await;
        assert_eq!(
            fetched.status, 200,
            "case {case} ({leg} the policy write): expected GetKnowledgeBase -> 200, got \
             {fetched:?}"
        );
        assert_eq!(
            fetched.text(&[
                "knowledgeBase",
                "storageConfiguration",
                "opensearchServerlessConfiguration",
                "collectionArn",
            ]),
            collection_arn,
            "case {case} ({leg} the policy write): expected the stored knowledge base to carry \
             the collection the request named, got {fetched:?}"
        );
        assert_eq!(
            fetched.text(&[
                "knowledgeBase",
                "storageConfiguration",
                "opensearchServerlessConfiguration",
                "vectorIndexName",
            ]),
            VECTOR_INDEX,
            "case {case} ({leg} the policy write): expected the stored knowledge base to carry \
             the index the request named, got {fetched:?}"
        );
        let settled = emulator.settled_status(&knowledge_base_id).await;
        assert_eq!(
            settled, "ACTIVE",
            "case {case} ({leg} the policy write): expected the accepted knowledge base to reach \
             ACTIVE, got {settled:?}"
        );
    } else {
        assert_eq!(
            answered.status, 400,
            "case {case} ({leg} the policy write): expected CreateKnowledgeBase -> 400, got \
             {answered:?}"
        );
        assert!(
            answered
                .error_type
                .as_deref()
                .is_some_and(|held| held.contains("ValidationException")),
            "case {case} ({leg} the policy write): expected ValidationException, got {answered:?}"
        );
        assert_eq!(
            answered.text(&["message"]),
            UNREACHABLE_STORAGE,
            "case {case} ({leg} the policy write): expected the refusal Amazon Bedrock words for \
             a storage configuration its role cannot reach, got {answered:?}"
        );
    }
    let held = emulator.knowledge_base_names().await;
    assert_eq!(
        held.contains(&name.to_string()),
        accepted,
        "case {case} ({leg} the policy write): expected the knowledge base {}, got {held:?}",
        if accepted { "listed" } else { "absent" }
    );
}

/// RULE: `CreateKnowledgeBase` resolves an `OPENSEARCH_SERVERLESS` storage configuration
/// against the collection's `data` access policies as they stand WHEN THE CALL IS MADE, and
/// answers synchronously — `202` where they grant the knowledge base's role the permissions
/// a knowledge base performs on the vector index, `400 ValidationException` with the
/// invalid-storage-configuration refusal where they do not — so a policy write between two
/// identical creates is what changes the answer.
///
/// The rows span the kind of write (a create, an update that widens, narrows or
/// re-principals the grant, an update that touches only the description, a delete) and both
/// directions the answer moves in, including a write that moves it nowhere.
#[tokio::test]
async fn an_opensearch_serverless_binding_follows_the_policy_as_it_stands_at_the_call() {
    /// One case: the policy the collection holds before the write (`None` for none at all),
    /// the write performed between the two identical creates, and whether each create is
    /// accepted.
    struct Row {
        case: &'static str,
        before: Option<Grant>,
        write: Write,
        accepted_before: bool,
        accepted_after: bool,
    }
    let rows: &[Row] = &[
        Row {
            case: "an update adds the knowledge-base role beside the caller identity the \
                   policy already named",
            before: Some(Grant {
                permissions: KNOWLEDGE_BASE_PERMISSIONS,
                principals: &[Named::CallerIdentity],
            }),
            write: Write::Replace(Grant {
                permissions: KNOWLEDGE_BASE_PERMISSIONS,
                principals: &[Named::CallerIdentity, Named::KnowledgeBaseRole],
            }),
            accepted_before: false,
            accepted_after: true,
        },
        Row {
            case: "a collection holding no data access policy at all, then a create of one \
                   granting the knowledge-base role",
            before: None,
            write: Write::Create(Grant {
                permissions: KNOWLEDGE_BASE_PERMISSIONS,
                principals: &[Named::KnowledgeBaseRole],
            }),
            accepted_before: false,
            accepted_after: true,
        },
        Row {
            case: "an update widens the knowledge-base role's grant from the document read \
                   to every aoss action",
            before: Some(Grant {
                permissions: &["aoss:ReadDocument"],
                principals: &[Named::KnowledgeBaseRole],
            }),
            write: Write::Replace(Grant {
                permissions: EVERY_PERMISSION,
                principals: &[Named::KnowledgeBaseRole],
            }),
            accepted_before: false,
            accepted_after: true,
        },
        Row {
            case: "an update adds a third identity but still not the knowledge-base role",
            before: Some(Grant {
                permissions: EVERY_PERMISSION,
                principals: &[Named::CallerIdentity],
            }),
            write: Write::Replace(Grant {
                permissions: EVERY_PERMISSION,
                principals: &[Named::CallerIdentity, Named::OtherIdentity],
            }),
            accepted_before: false,
            accepted_after: false,
        },
        Row {
            case: "an update narrows the knowledge-base role's grant to the document read",
            before: Some(Grant {
                permissions: KNOWLEDGE_BASE_PERMISSIONS,
                principals: &[Named::KnowledgeBaseRole],
            }),
            write: Write::Replace(Grant {
                permissions: &["aoss:ReadDocument"],
                principals: &[Named::KnowledgeBaseRole],
            }),
            accepted_before: true,
            accepted_after: false,
        },
        Row {
            case: "a delete takes away the policy that granted the knowledge-base role",
            before: Some(Grant {
                permissions: EVERY_PERMISSION,
                principals: &[Named::KnowledgeBaseRole],
            }),
            write: Write::Delete,
            accepted_before: true,
            accepted_after: false,
        },
        Row {
            case: "an update changes only the policy description, leaving the grant alone",
            before: Some(Grant {
                permissions: KNOWLEDGE_BASE_PERMISSIONS,
                principals: &[Named::KnowledgeBaseRole],
            }),
            write: Write::Describe,
            accepted_before: true,
            accepted_after: true,
        },
    ];
    for (
        index,
        Row {
            case,
            before,
            write,
            accepted_before,
            accepted_after,
        },
    ) in rows.iter().enumerate()
    {
        let emulator = Emulator::new();
        let identities = vec![
            (
                Named::KnowledgeBaseRole,
                emulator.given_role(&format!("kb-policy-role-{index}")).await,
            ),
            (
                Named::CallerIdentity,
                emulator
                    .given_role(&format!("kb-policy-caller-{index}"))
                    .await,
            ),
            (
                Named::OtherIdentity,
                emulator
                    .given_role(&format!("kb-policy-other-{index}"))
                    .await,
            ),
        ];
        let role_arn = identities[0].1.clone();
        let collection = format!("kb-policy-{index}");
        let collection_arn = emulator.given_collection(&collection).await;
        let policy_name = format!("kb-policy-{index}");
        let mut policy_version = match before {
            Some(grant) => Some(
                emulator
                    .create_access_policy(
                        &policy_name,
                        &data_policy(&collection, grant, &identities),
                    )
                    .await,
            ),
            None => None,
        };

        // The first leg: the same request the second leg makes, answered against the policy
        // as it stands now.
        let first = format!("kb-policy-{index}-before");
        let answered = emulator
            .create_knowledge_base(create_request(&first, &role_arn, &collection_arn))
            .await;
        assert_leg(
            &emulator,
            case,
            "before",
            &first,
            &collection_arn,
            answered,
            *accepted_before,
        )
        .await;

        // The write that moves the collection from one state to the other.
        match write {
            Write::Create(grant) => {
                policy_version = Some(
                    emulator
                        .create_access_policy(
                            &policy_name,
                            &data_policy(&collection, grant, &identities),
                        )
                        .await,
                );
            }
            Write::Replace(grant) => {
                let version = policy_version
                    .clone()
                    .expect("a replacing row holds a policy to replace");
                policy_version = Some(
                    emulator
                        .update_access_policy(
                            &policy_name,
                            json!({
                                "type": "data",
                                "name": policy_name,
                                "policyVersion": version,
                                "policy": data_policy(&collection, grant, &identities)
                                    .to_string(),
                            }),
                        )
                        .await,
                );
            }
            Write::Describe => {
                let version = policy_version
                    .clone()
                    .expect("a re-described row holds a policy to describe");
                policy_version = Some(
                    emulator
                        .update_access_policy(
                            &policy_name,
                            json!({
                                "type": "data",
                                "name": policy_name,
                                "policyVersion": version,
                                "description": "the same grant, described again",
                            }),
                        )
                        .await,
                );
            }
            Write::Delete => {
                emulator.delete_access_policy(&policy_name).await;
                policy_version = None;
            }
        }
        assert_eq!(
            policy_version.is_some(),
            !matches!(write, Write::Delete),
            "case {case}: the policy write must leave the policy {}",
            if matches!(write, Write::Delete) {
                "gone"
            } else {
                "stored"
            }
        );

        // The second leg: the identical request, answered against the policy as it stands
        // after the write.
        let second = format!("kb-policy-{index}-after");
        let answered = emulator
            .create_knowledge_base(create_request(&second, &role_arn, &collection_arn))
            .await;
        assert_leg(
            &emulator,
            case,
            "after",
            &second,
            &collection_arn,
            answered,
            *accepted_after,
        )
        .await;
    }
}
