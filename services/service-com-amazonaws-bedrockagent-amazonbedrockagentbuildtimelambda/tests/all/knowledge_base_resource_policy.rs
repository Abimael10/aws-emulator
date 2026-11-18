// SPDX-License-Identifier: CC0-1.0

//! A knowledge base's resource-based policy, end to end over HTTP, plus the two
//! readings it turns on as `crate::state::resource_policies` makes them: which
//! knowledge base a resource-policy operation's `resourceArn` names, and which KIND
//! of knowledge base carries a policy at all.
//!
//! RULE: `GetResourcePolicy` reports the policy document the knowledge base its
//! `resourceArn` names holds, that document's revision and the knowledge base's own
//! ARN — all three read off the stored record, never off the request. A knowledge
//! base whose `knowledgeBaseConfiguration.type` carries no resource policy at all is
//! the bad request `ValidationException` refuses; an ARN naming a knowledge base
//! this store does not hold, and a knowledge base carrying no policy, are the
//! missing resources `ResourceNotFoundException` refuses — the modelled output makes
//! `policy` and `revisionId` both @required, so a knowledge base with no policy has
//! no successful answer. Reading a policy changes nothing a later caller sees.
//!
//! The rows span what the rule generalises over: the form and size of the policy
//! document (minified, whitespace-carrying, multi-statement, the shortest the shape
//! admits, one carrying a Latin-1 character, a long one), how many writes the policy
//! has taken, every `KnowledgeBaseType` the model declares, and every way an ARN can
//! fail to name a policy-bearing knowledge base.
//!
//! `PutResourcePolicy` and `DeleteResourcePolicy` are still scaffold stubs, each with
//! an issue of its own, so the WRITE that attaches a policy is made here through the
//! one state writer those handlers will use
//! (`KnowledgeBaseRecord::attach_resource_policy`), exactly as the suites of this
//! crate drive a store no operation reaches yet; everything the read answers is
//! asserted over the router, on the real wire path.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state::{
    carries_resource_policy, policied_knowledge_base, service_state, ResourcePolicyRefusal,
    MANAGED_KNOWLEDGE_BASE_TYPE,
};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// Every value the modelled `KnowledgeBaseType` enum declares.
///
/// Source: the `KnowledgeBaseType` enum of this service's Smithy model
/// (`smithy_query(query="shape",shape="KnowledgeBaseType")`).
const KNOWLEDGE_BASE_TYPES: [&str; 4] = ["VECTOR", "KENDRA", "SQL", "MANAGED"];

/// RULE: a resource-policy operation's `resourceArn` names the knowledge base its
/// `knowledge-base/<id>` resource field names, and names none for an ARN of any
/// other resource of this plane or of any other syntax.
#[test]
fn a_resource_arn_names_the_knowledge_base_its_resource_field_names() {
    // The rows differ along what the reading generalises over: the partition, the
    // region, the form of the identifier, and which resource the ARN names.
    let rows: [(&str, &str, Option<&str>); 9] = [
        (
            "a knowledge base of the standard partition",
            "arn:aws:bedrock:eu-west-1:000000000000:knowledge-base/0AbCdEf123",
            Some("0AbCdEf123"),
        ),
        (
            "a knowledge base of another partition",
            "arn:aws-us-gov:bedrock:us-gov-west-1:000000000000:knowledge-base/ZZ99zz99ZZ",
            Some("ZZ99zz99ZZ"),
        ),
        (
            "a knowledge base of another region",
            "arn:aws:bedrock:ap-southeast-2:123456789012:knowledge-base/abcdefghij",
            Some("abcdefghij"),
        ),
        (
            "an agent of the same plane",
            "arn:aws:bedrock:eu-west-1:000000000000:agent/0AbCdEf123",
            None,
        ),
        (
            "a flow of the same plane",
            "arn:aws:bedrock:eu-west-1:000000000000:flow/0AbCdEf123",
            None,
        ),
        (
            "a knowledge base with no identifier at all",
            "arn:aws:bedrock:eu-west-1:000000000000:knowledge-base/",
            None,
        ),
        (
            "an ARN of another service",
            "arn:aws:s3:::bucket/knowledge-base/0AbCdEf123",
            None,
        ),
        (
            "an account field that is not an account id",
            "arn:aws:bedrock:eu-west-1:account:knowledge-base/0AbCdEf123",
            None,
        ),
        ("a value that is no ARN at all", "0AbCdEf123", None),
    ];

    for (case, resource_arn, expected) in rows {
        let named = policied_knowledge_base(resource_arn);
        assert_eq!(
            named.as_deref(),
            expected,
            "case {case:?}: expected {resource_arn:?} to name {expected:?}, got {named:?}"
        );
    }
}

/// RULE: a resource policy belongs to a knowledge base Amazon Bedrock MANAGES, so
/// that one `KnowledgeBaseType` carries a policy and every other one carries none.
#[test]
fn only_the_service_managed_kind_carries_a_resource_policy() {
    for knowledge_base_type in KNOWLEDGE_BASE_TYPES {
        let carries = carries_resource_policy(knowledge_base_type);
        assert_eq!(
            carries,
            knowledge_base_type == MANAGED_KNOWLEDGE_BASE_TYPE,
            "case {knowledge_base_type:?}: expected carries_resource_policy \
             {}, got {carries}",
            knowledge_base_type == MANAGED_KNOWLEDGE_BASE_TYPE
        );
    }
    // A kind the model does not declare is not the managed kind either, however it
    // is spelled: the reading admits one value and nothing near it.
    for outside in ["managed", "MANAGED_KNOWLEDGE_BASE", "", " MANAGED"] {
        assert!(
            !carries_resource_policy(outside),
            "case {outside:?}: expected no resource policy for a kind outside the modelled \
             {KNOWLEDGE_BASE_TYPES:?}"
        );
    }
}

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the routing
/// key this service claims on, and the source of the region its ARNs carry.
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

/// An embedding model ARN matching the modelled `BedrockEmbeddingModelArn`.
const EMBEDDING_MODEL: &str =
    "arn:aws:bedrock:eu-west-1::foundation-model/amazon.titan-embed-text-v2:0";

/// A Kendra index ARN matching the modelled `KendraIndexArn` pattern.
const KENDRA_INDEX: &str =
    "arn:aws:kendra:eu-west-1:000000000000:index/aaaaaaaa-1111-2222-3333-444444444444";

/// The `ResourceNotFoundException` sentence AWS answers when the knowledge base a
/// resource-policy call names is not stored: fixed text but for the id the ARN named.
fn knowledge_base_absent(knowledge_base_id: &str) -> String {
    format!("KnowledgeBase with id {knowledge_base_id} is not found.")
}

/// The `ValidationException` sentence AWS answers for a knowledge base of a kind that
/// carries no resource policy: entirely fixed text, naming the one kind that does.
const KIND_CARRIES_NO_POLICY: &str =
    "Resource policies are only supported for MANAGED type knowledge bases.";

/// The same not-found sentence for the knowledge base that IS stored but carries no
/// policy: the resource that could not be retrieved is the policy.
const POLICY_ABSENT: &str = "Couldn't perform GetResourcePolicy operation. Failed to retrieve \
                             resource type ResourcePolicy because it doesn't exist. Check the \
                             resource ID and retry your request.";

/// One value as a percent-encoded URI component, as a client that sends a non-greedy
/// `@httpLabel` encodes it.
fn label(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace(':', "%3A")
        .replace('/', "%2F")
}

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The restJson1 error shape name, from `x-amzn-errortype` or the body's
    /// `__type`; `None` on a success.
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    fn member(&self, member: &str) -> &Value {
        &self.body[member]
    }

    fn string(&self, member: &str) -> String {
        self.member(member)
            .as_str()
            .unwrap_or_else(|| panic!("{member:?} is not a string in {:?}", self.body))
            .to_string()
    }

    fn message(&self) -> String {
        self.string("message")
    }
}

/// A knowledge base request of the kind `knowledge_base_type` names, carrying that
/// kind's own configuration member — and, for the self-managed vector kind, the
/// vector store it also requires.
///
/// Source: the `KnowledgeBaseType` enum and the `KnowledgeBaseConfiguration`,
/// `KendraKnowledgeBaseConfiguration`, `SqlKnowledgeBaseConfiguration` and
/// `ManagedKnowledgeBaseConfiguration` structures of this service's Smithy model
/// (`smithy_query(query="shape",shape="KnowledgeBaseConfiguration")`).
fn knowledge_base_of_type(name: &str, knowledge_base_type: &str) -> Value {
    let mut request = json!({ "name": name, "roleArn": KB_ROLE });
    match knowledge_base_type {
        "VECTOR" => {
            request["knowledgeBaseConfiguration"] = json!({
                "type": "VECTOR",
                "vectorKnowledgeBaseConfiguration": { "embeddingModelArn": EMBEDDING_MODEL },
            });
            request["storageConfiguration"] = json!({
                "type": "OPENSEARCH_SERVERLESS",
                "opensearchServerlessConfiguration": {
                    "collectionArn": format!("arn:aws:aoss:{REGION}:{ACCOUNT}:collection/policy"),
                    "vectorIndexName": format!("{name}-index"),
                    "fieldMapping": {
                        "vectorField": "bedrock-knowledge-base-default-vector",
                        "textField": "AMAZON_BEDROCK_TEXT_CHUNK",
                        "metadataField": "AMAZON_BEDROCK_METADATA",
                    },
                },
            });
        }
        "KENDRA" => {
            request["knowledgeBaseConfiguration"] = json!({
                "type": "KENDRA",
                "kendraKnowledgeBaseConfiguration": { "kendraIndexArn": KENDRA_INDEX },
            });
        }
        "SQL" => {
            request["knowledgeBaseConfiguration"] = json!({
                "type": "SQL",
                "sqlKnowledgeBaseConfiguration": { "type": "REDSHIFT" },
            });
        }
        "MANAGED" => {
            request["knowledgeBaseConfiguration"] = json!({
                "type": "MANAGED",
                "managedKnowledgeBaseConfiguration": { "embeddingModelType": "MANAGED" },
            });
        }
        other => panic!("{other} is not a modelled KnowledgeBaseType"),
    }
    request
}

/// One cross-account grant, as a resource policy of a knowledge base reads.
fn grant(sid: &str, account: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Sid":"{sid}","Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{account}:root"}},"Action":["bedrock:Retrieve"],"Resource":"*"}}]}}"#
    )
}

/// The emulator under test: one router over one shared engine, exactly as the
/// production router mounts this service.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
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

    /// Create one knowledge base of `knowledge_base_type` and report its id and ARN.
    async fn knowledge_base(
        &self,
        case: &str,
        name: &str,
        knowledge_base_type: &str,
    ) -> (String, String) {
        let created = self
            .call(
                "PUT",
                "/knowledgebases/",
                Some(knowledge_base_of_type(name, knowledge_base_type)),
            )
            .await;
        assert_eq!(
            created.status, 202,
            "case {case}: expected CreateKnowledgeBase -> 202, got {created:?}"
        );
        let knowledge_base = created.member("knowledgeBase");
        (
            knowledge_base["knowledgeBaseId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: expected a knowledgeBaseId"))
                .to_string(),
            knowledge_base["knowledgeBaseArn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: expected a knowledgeBaseArn"))
                .to_string(),
        )
    }

    /// `GetResourcePolicy`, with the ARN percent-encoded as a client that sends a
    /// non-greedy `@httpLabel` encodes it.
    async fn get_resource_policy(&self, resource_arn: &str) -> Answer {
        self.call("GET", &format!("/resourcepolicy/{}", label(resource_arn)), None)
            .await
    }

    /// Attach `policy` to the stored knowledge base through the one state writer the
    /// still-scaffolded `PutResourcePolicy` will use, and report the `revisionId` the
    /// write left it at.
    fn attach_policy(&self, case: &str, knowledge_base_id: &str, policy: &str) -> String {
        let state = service_state(&*self.engine).expect("the bedrock-agent store");
        let mut state = state.lock();
        state
            .live_knowledge_base_mut(knowledge_base_id)
            .unwrap_or_else(|| panic!("case {case}: expected the stored knowledge base"))
            .attach_resource_policy(policy.to_string())
    }

    /// Accept a delete of the knowledge base, which is what takes it out of the reach
    /// of every operation that acts on it.
    async fn delete_knowledge_base(&self, case: &str, knowledge_base_id: &str) {
        let deleted = self
            .call("DELETE", &format!("/knowledgebases/{knowledge_base_id}"), None)
            .await;
        assert_eq!(
            deleted.status, 202,
            "case {case}: expected DeleteKnowledgeBase -> 202, got {deleted:?}"
        );
    }
}

/// RULE: the policy `GetResourcePolicy` reports is the document the knowledge base
/// holds, reported with that document's revision and the knowledge base's own ARN —
/// over a table of documents differing in form, size and the characters they carry.
#[tokio::test]
async fn the_policy_reported_is_the_document_the_knowledge_base_holds() {
    let emulator = Emulator::new();

    // The rows differ along what the rule generalises over: the form of the document
    // (minified, indented, multi-statement), its size (the shortest the modelled
    // `ResourcePolicy` admits, one of a few kilobytes) and the characters it carries
    // (the modelled pattern ^[\u0009\u000A\u000D\u0020-\u00FF]+$ admits tabs,
    // newlines and Latin-1 beyond ASCII).
    let long_statements: Vec<String> = (0..40)
        .map(|index| {
            format!(
                r#"{{"Sid":"grant{index}","Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{:012}:root"}},"Action":["bedrock:Retrieve","bedrock:RetrieveAndGenerate"],"Resource":"*"}}"#,
                index + 1
            )
        })
        .collect();
    let cases: Vec<(&str, String)> = vec![
        ("a minified single-statement grant", grant("one", "111122223333")),
        (
            "the same grant with the whitespace a client's pretty-printer leaves",
            "{\n\t\"Version\": \"2012-10-17\",\r\n\t\"Statement\": [\n\t\t{\n\t\t\t\"Effect\": \
             \"Allow\",\n\t\t\t\"Principal\": {\"AWS\": \"arn:aws:iam::111122223333:root\"},\n\
             \t\t\t\"Action\": \"bedrock:Retrieve\",\n\t\t\t\"Resource\": \"*\"\n\t\t}\n\t]\n}"
                .to_string(),
        ),
        (
            "a multi-statement policy",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{},{}]}}"#,
                r#"{"Sid":"first","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::111122223333:root"},"Action":"bedrock:Retrieve","Resource":"*"}"#,
                r#"{"Sid":"second","Effect":"Deny","Principal":{"AWS":"arn:aws:iam::444455556666:root"},"Action":"bedrock:RetrieveAndGenerate","Resource":"*"}"#,
            ),
        ),
        (
            "the shortest document the modelled ResourcePolicy admits",
            "{".to_string(),
        ),
        (
            "a document carrying a Latin-1 character the pattern admits",
            r#"{"Version":"2012-10-17","Statement":[{"Sid":"café","Effect":"Allow","Principal":{"AWS":"arn:aws:iam::111122223333:root"},"Action":"bedrock:Retrieve","Resource":"*"}]}"#
                .to_string(),
        ),
        (
            "a document of a few kilobytes",
            format!(
                r#"{{"Version":"2012-10-17","Statement":[{}]}}"#,
                long_statements.join(",")
            ),
        ),
    ];

    for (index, (case, policy)) in cases.iter().enumerate() {
        // Each row gets its own knowledge base, so no row can pass on another's
        // stored policy.
        let (knowledge_base_id, knowledge_base_arn) = emulator
            .knowledge_base(case, &format!("policy-kb-{index}"), MANAGED_KNOWLEDGE_BASE_TYPE)
            .await;
        let revision_id = emulator.attach_policy(case, &knowledge_base_id, policy);

        let read = emulator.get_resource_policy(&knowledge_base_arn).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected GetResourcePolicy -> 200, got {read:?}"
        );
        assert_eq!(
            read.string("policy"),
            *policy,
            "case {case}: expected the stored document back verbatim, got {:?}",
            read.string("policy")
        );
        assert_eq!(
            read.string("resourceArn"),
            knowledge_base_arn,
            "case {case}: expected the knowledge base's own ARN, got {:?}",
            read.string("resourceArn")
        );
        assert_eq!(
            read.string("revisionId"),
            revision_id,
            "case {case}: expected the revision the write left the policy at, got {:?}",
            read.string("revisionId")
        );

        // A read reports the same thing however often it is made: nothing about the
        // policy moves because somebody looked at it.
        let again = emulator.get_resource_policy(&knowledge_base_arn).await;
        assert_eq!(
            (again.status, again.string("policy"), again.string("revisionId")),
            (200, policy.clone(), revision_id.clone()),
            "case {case}: expected a second read to report the same policy, got {again:?}"
        );
    }
}

/// RULE: the document and the revision a read reports are the ones of the knowledge
/// base the ARN names, so two knowledge bases report their own policies, and each
/// write past the first is reported with its own revision.
#[tokio::test]
async fn each_knowledge_base_reports_its_own_policy_at_its_own_revision() {
    let emulator = Emulator::new();

    let (first_id, first_arn) = emulator
        .knowledge_base("the first knowledge base", "policy-kb-first", "MANAGED")
        .await;
    let (second_id, second_arn) = emulator
        .knowledge_base("the second knowledge base", "policy-kb-second", "MANAGED")
        .await;

    let first_policy = grant("first", "111122223333");
    let second_policy = grant("second", "444455556666");
    let first_revision = emulator.attach_policy("the first knowledge base", &first_id, &first_policy);
    let second_revision =
        emulator.attach_policy("the second knowledge base", &second_id, &second_policy);

    for (case, resource_arn, policy, revision) in [
        ("the first knowledge base", &first_arn, &first_policy, &first_revision),
        (
            "the second knowledge base",
            &second_arn,
            &second_policy,
            &second_revision,
        ),
    ] {
        let read = emulator.get_resource_policy(resource_arn).await;
        assert_eq!(
            (read.status, read.string("policy"), read.string("revisionId")),
            (200, policy.clone(), revision.clone()),
            "case {case}: expected this knowledge base's own policy, got {read:?}"
        );
    }

    // Each write past the first leaves the policy at a revision of its own, which is
    // what `expectedRevisionId` compares against; the rows differ in how many writes
    // the policy has taken.
    let mut expected_revisions = vec![first_revision];
    for write in 1..4 {
        let replacement = grant(&format!("write{write}"), "111122223333");
        let revision = emulator.attach_policy("a replaced policy", &first_id, &replacement);
        assert!(
            !expected_revisions.contains(&revision),
            "case write {write}: expected a revision unlike every earlier one \
             {expected_revisions:?}, got {revision:?}"
        );
        expected_revisions.push(revision.clone());

        let read = emulator.get_resource_policy(&first_arn).await;
        assert_eq!(
            (read.status, read.string("policy"), read.string("revisionId")),
            (200, replacement.clone(), revision.clone()),
            "case write {write}: expected the replacing document at its own revision, \
             got {read:?}"
        );
    }

    // The other knowledge base's policy is untouched by every write to the first.
    let untouched = emulator.get_resource_policy(&second_arn).await;
    assert_eq!(
        (untouched.status, untouched.string("policy")),
        (200, second_policy),
        "case the second knowledge base after the writes: expected its own policy, \
         got {untouched:?}"
    );
}

/// RULE: a knowledge base whose kind carries no resource policy is a bad request,
/// over every `KnowledgeBaseType` the model declares but the service-managed one.
#[tokio::test]
async fn a_knowledge_base_of_a_kind_that_carries_no_policy_is_a_bad_request() {
    let emulator = Emulator::new();

    for knowledge_base_type in KNOWLEDGE_BASE_TYPES {
        if knowledge_base_type == MANAGED_KNOWLEDGE_BASE_TYPE {
            continue;
        }
        let case = knowledge_base_type;
        let (_, knowledge_base_arn) = emulator
            .knowledge_base(
                case,
                &format!("policy-kind-{}", knowledge_base_type.to_lowercase()),
                knowledge_base_type,
            )
            .await;

        let refused = emulator.get_resource_policy(&knowledge_base_arn).await;
        assert_eq!(
            (refused.status, refused.error_type.as_deref()),
            (400, Some("ValidationException")),
            "case {case}: expected 400 ValidationException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            KIND_CARRIES_NO_POLICY,
            "case {case}: expected {KIND_CARRIES_NO_POLICY:?}, got {:?}",
            refused.message()
        );
    }
}

/// RULE: an ARN naming a knowledge base this store does not hold is a missing
/// resource, and so is a knowledge base that carries no policy — the modelled output
/// makes `policy` and `revisionId` required, so there is no successful answer for one.
#[tokio::test]
async fn an_absent_knowledge_base_and_an_absent_policy_are_missing_resources() {
    let emulator = Emulator::new();

    let (stored_id, stored_arn) = emulator
        .knowledge_base("a knowledge base with no policy", "policy-absent-kb", "MANAGED")
        .await;
    let (deleted_id, deleted_arn) = emulator
        .knowledge_base("a deleted knowledge base", "policy-deleted-kb", "MANAGED")
        .await;
    emulator.attach_policy("a deleted knowledge base", &deleted_id, &grant("gone", "111122223333"));
    emulator
        .delete_knowledge_base("a deleted knowledge base", &deleted_id)
        .await;

    // The rows differ along what the rule generalises over: which half of "no policy
    // to report" holds, and — for an ARN naming nothing — the partition, the region
    // and the account the ARN carries, none of which is what selects the record.
    let cases: Vec<(&str, String, String)> = vec![
        (
            "a knowledge base id nothing holds",
            format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:knowledge-base/ZZZZZZZZZZ"),
            knowledge_base_absent("ZZZZZZZZZZ"),
        ),
        (
            "an id nothing holds, in another partition, region and account",
            "arn:aws-us-gov:bedrock:us-gov-west-1:123456789012:knowledge-base/0a1b2c3d4e"
                .to_string(),
            knowledge_base_absent("0a1b2c3d4e"),
        ),
        (
            "a knowledge base a delete was accepted for",
            deleted_arn,
            knowledge_base_absent(&deleted_id),
        ),
        (
            "a stored knowledge base carrying no policy",
            stored_arn.clone(),
            POLICY_ABSENT.to_string(),
        ),
    ];

    for (case, resource_arn, expected) in cases {
        let refused = emulator.get_resource_policy(&resource_arn).await;
        assert_eq!(
            (refused.status, refused.error_type.as_deref()),
            (404, Some("ResourceNotFoundException")),
            "case {case}: expected 404 ResourceNotFoundException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            expected,
            "case {case}: expected {expected:?}, got {:?}",
            refused.message()
        );
    }

    // The refusals left the store as it was: a policy attached now is reported, so
    // nothing a read did made the knowledge base unreachable.
    let policy = grant("after", "111122223333");
    let revision_id = emulator.attach_policy("after the refusals", &stored_id, &policy);
    let read = emulator.get_resource_policy(&stored_arn).await;
    assert_eq!(
        (read.status, read.string("policy"), read.string("revisionId")),
        (200, policy, revision_id),
        "case after the refusals: expected the newly attached policy, got {read:?}"
    );
}

/// RULE: the ONE resolution every resource-policy operation shares answers either the
/// knowledge base whose policy the call is about or the reason there is none — the
/// value naming no knowledge base of this plane, the knowledge base nothing holds, and
/// the kind that carries no policy, in that order.
#[tokio::test]
async fn the_shared_resolution_answers_each_reason_there_is_no_policy() {
    let emulator = Emulator::new();

    let (managed_id, managed_arn) = emulator
        .knowledge_base("a managed knowledge base", "policy-resolve-managed", "MANAGED")
        .await;
    let (_, vector_arn) = emulator
        .knowledge_base("a vector knowledge base", "policy-resolve-vector", "VECTOR")
        .await;

    // The rows cover every outcome of the resolution, including the one no request can
    // reach over the wire — the modelled `ResourceArn` pattern refuses a value that
    // names another of this plane's resources before any handler runs, so the
    // resolution is asserted here, where the write and the delete will reach it too.
    let cases: Vec<(&str, String, Result<String, ResourcePolicyRefusal>)> = vec![
        (
            "a managed knowledge base",
            managed_arn,
            Ok(managed_id.clone()),
        ),
        (
            "an agent of the same plane",
            format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:agent/0a1b2c3d4e"),
            Err(ResourcePolicyRefusal::NotAKnowledgeBase),
        ),
        (
            "a value that is no ARN at all",
            "0a1b2c3d4e".to_string(),
            Err(ResourcePolicyRefusal::NotAKnowledgeBase),
        ),
        (
            "a knowledge base id nothing holds",
            format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:knowledge-base/ZZZZZZZZZZ"),
            Err(ResourcePolicyRefusal::NoSuchKnowledgeBase("ZZZZZZZZZZ".to_string())),
        ),
        (
            "a vector knowledge base",
            vector_arn,
            Err(ResourcePolicyRefusal::KindCarriesNoPolicy),
        ),
    ];

    let state = service_state(&*emulator.engine).expect("the bedrock-agent store");
    let state = state.lock();
    for (case, resource_arn, expected) in cases {
        let resolved = state
            .policy_bearing_knowledge_base(&resource_arn)
            .map(|record| record.knowledge_base_id.clone());
        assert_eq!(
            resolved, expected,
            "case {case} ({resource_arn}): expected {expected:?}, got {resolved:?}"
        );
    }
}
