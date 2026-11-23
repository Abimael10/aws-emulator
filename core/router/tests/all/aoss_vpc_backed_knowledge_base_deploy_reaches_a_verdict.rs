// SPDX-License-Identifier: CC0-1.0

//! A knowledge-base deploy that provisions its OpenSearch Serverless store behind a MANAGED
//! INTERFACE ENDPOINT always reaches a verdict: Amazon EC2 owns the network, OpenSearch
//! Serverless owns the endpoint, the policies and the collection, and the Amazon Bedrock Agent
//! build-time plane owns the knowledge base — so the suite lives in this cross-service target.
//!
//! RULE: every resource such a deploy waits on is returned in a non-terminal status and
//! settles into its terminal one on its own, with nobody calling — the managed interface
//! endpoint reaches `ACTIVE` and the collection reaches `ACTIVE` — and `CreateKnowledgeBase`
//! decides the `storageConfiguration` binding IN THE CREATE CALL, answering either 202 with
//! the stored knowledge base or the modelled `ValidationException` (400) carrying the vector
//! store's own authorization detail. The binding is resolved against the collection's stored
//! `data` access policies in the control plane and not at the collection's published address,
//! so WHERE the collection's `network` policy admits traffic from never changes which of the
//! two answers arrives, nor whether it arrives at all: a client polling such a deploy is never
//! left without one.
//!
//! The rows span what the rule generalises over: the source form the collection's `network`
//! policy states (from anywhere, and through the managed endpoint this deploy created — the
//! two forms "Network access for Amazon OpenSearch Serverless" gives a statement), how many
//! private subnets the endpoint is placed in, and how the `data` access policy falls short or
//! does not (every index permission a knowledge base performs granted to its own role, a
//! wildcard grant, no policy at all, and a grant to another principal).
//!
//! What the sibling suites pin, and this one does not repeat: the ways a `data` access grant
//! falls short and what an ingestion and a retrieval over a granted collection then answer
//! (`aoss_data_access_grant_decides_where_a_knowledge_base_fails.rs`), which requests a
//! `network` boundary admits at the published address
//! (`aoss_network_boundary_gates_a_knowledge_base_pipeline.rs`), and that a managed endpoint's
//! placement is resolved in EC2 (`aoss_vpc_endpoint_placement_resolves_in_ec2.rs`). What is
//! pinned HERE is that the whole deploy CONVERGES — each waited-on status and the create's own
//! verdict — behind an endpoint-only boundary as well as a public one.
//!
//! Every wait is a poll for the CONDITION under a generous deadline; nothing here sleeps for a
//! fixed time or asserts how long a step took.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::aoss_vpc_endpoint_placement_resolves_in_ec2::ec2_vpc;
use crate::bedrock_knowledge_base_over_an_aoss_collection::{
    active_collection, aoss, at, await_active_knowledge_base, bedrock, encrypt_collections,
    opensearch_storage, text, KB_ROLE, REGION,
};

/// The terminal status both the managed interface endpoint and the collection settle into.
const ACTIVE: &str = "ACTIVE";

/// How many times a read looks again for a transition the clock owes, and how long one look
/// waits before the next: two minutes in all, looked at every 25 ms, so a stage that does
/// settle is seen to have settled at once while a host running every suite of this workspace
/// at once still has room. Every wait is for the CONDITION and nothing asserts how long it
/// took.
const SETTLE_ATTEMPTS: usize = 4_800;
const SETTLE_PAUSE: Duration = Duration::from_millis(25);

/// The embedding model every knowledge base of this suite names: a published model whose
/// output modality is an embedding, so no row is refused on the model.
const EMBEDDING_MODEL: &str = "amazon.titan-embed-text-v2:0";

/// The vector index every row's `storageConfiguration` names.
const VECTOR_INDEX: &str = "bedrock-knowledge-base-default-index";

/// Another role, for the row whose data access policy grants the index to someone other than
/// the knowledge base's own role. Matches the modelled `KnowledgeBaseRoleArn` pattern, as
/// [`KB_ROLE`] does.
const OTHER_ROLE: &str = "arn:aws:iam::000000000000:role/service-role/AmazonBedrockOtherRole";

/// The index permissions a knowledge base performs on the vector index it is bound to, which
/// are the ones its role has to be granted for the binding to be served: Amazon Bedrock reads
/// the index's own definition to check the field mapping, writes a document per ingested
/// chunk, and reads them back to serve a retrieval.
///
/// Source: the index-level permissions AWS's "Create a vector index in Amazon OpenSearch
/// Service" states for the knowledge-base role's data access policy.
const INDEX_PERMISSIONS: [&str; 3] = [
    "aoss:DescribeIndex",
    "aoss:ReadDocument",
    "aoss:WriteDocument",
];

/// The `ValidationException` message Amazon Bedrock answers when the vector store refuses the
/// knowledge base's own role: this service's opening for an invalid storage configuration and
/// the dependency's own authorization refusal.
const UNREACHABLE_STORAGE: &str = "The knowledge base storage configuration provided is \
                                   invalid... Dependency error document status code: 403, \
                                   error message: Bad Authorization";

/// Amazon EC2, OpenSearch Serverless and the Bedrock Agent build-time plane over ONE engine,
/// as `core/router/src/lib.rs` mounts them: the network the endpoint is placed in is EC2's own
/// inventory, and the collection the knowledge base binds to is the one the OpenSearch
/// Serverless operations wrote.
fn deployment_estate() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_opensearchserverless_opensearchserverless::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A `network` security policy for one collection, stating `source` as where traffic may reach
/// it from — the statement form "Network access for Amazon OpenSearch Serverless" gives, whose
/// source members sit beside the `Rules` that say which collection it applies to.
fn network_policy(collection: &str, source: Value) -> Value {
    let mut statement = json!({
        "Rules": [{
            "ResourceType": "collection",
            "Resource": [format!("collection/{collection}")],
        }],
    });
    if let (Some(members), Some(stated)) = (statement.as_object_mut(), source.as_object()) {
        for (name, value) in stated {
            members.insert(name.clone(), value.clone());
        }
    }
    json!([statement])
}

/// A `data` access policy granting `permissions` on every index of `collection` to
/// `principal`, in the shape AWS's data-access documentation writes.
fn data_policy(collection: &str, permissions: &[&str], principal: &str) -> Value {
    json!([{
        "Rules": [{
            "ResourceType": "index",
            "Resource": [format!("index/{collection}/*")],
            "Permission": permissions,
        }],
        "Principal": [principal],
    }])
}

/// Declare one security policy, asserting the control plane took it.
async fn given_security_policy(
    router: &MultiServiceRouter,
    case: &str,
    kind: &str,
    name: &str,
    policy: &Value,
) {
    let (status, answer) = aoss(
        router,
        "CreateSecurityPolicy",
        json!({ "type": kind, "name": name, "policy": policy.to_string() }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the {kind} policy {name} accepted, got {status} with {answer}"
    );
}

/// Declare one `data` access policy, asserting the control plane took it.
async fn given_data_access(router: &MultiServiceRouter, case: &str, name: &str, policy: &Value) {
    let (status, answer) = aoss(
        router,
        "CreateAccessPolicy",
        json!({ "type": "data", "name": name, "policy": policy.to_string() }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the data access policy {name} accepted, got {status} with \
         {answer}"
    );
}

/// Poll `BatchGetVpcEndpoint` until `id` reports [`ACTIVE`], answering the detail that read
/// reports. The move out of `PENDING` is owed by the clock, so this looks again for that
/// CONDITION under a generous deadline.
async fn await_active_vpc_endpoint(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    for _ in 0..SETTLE_ATTEMPTS {
        let (status, read) = aoss(router, "BatchGetVpcEndpoint", json!({ "ids": [id] })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected BatchGetVpcEndpoint({id}) to answer, got {status} with \
             {read}"
        );
        let detail = at(&read, "vpcEndpointDetails")
            .as_array()
            .and_then(|details| details.first())
            .cloned()
            .unwrap_or(Value::Null);
        if text(&detail, "status") == ACTIVE {
            return detail;
        }
        tokio::time::sleep(SETTLE_PAUSE).await;
    }
    panic!("case {case:?}: the managed endpoint {id} never reached {ACTIVE}");
}

/// The names `ListKnowledgeBases` reports.
async fn knowledge_base_names(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, listed) = bedrock(router, "POST", "/knowledgebases/", Some(&json!({}))).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListKnowledgeBases to answer, got {status} with {listed}"
    );
    at(&listed, "knowledgeBaseSummaries")
        .as_array()
        .map(|summaries| {
            summaries
                .iter()
                .map(|summary| text(summary, "name"))
                .collect()
        })
        .unwrap_or_default()
}

/// One case of the deploy table.
struct Row {
    case: &'static str,
    /// One private subnet is created per availability zone named here, and the managed
    /// endpoint is placed in all of them.
    zones: &'static [&'static str],
    /// Whether the collection's `network` policy admits traffic from anywhere, or only through
    /// the managed endpoint this row created.
    from_anywhere: bool,
    /// The `data` access policy the deploy declares: the index permissions it grants, and
    /// whether it grants them to the knowledge base's OWN role rather than to another
    /// principal. `None` for a deploy that declares none.
    data_access: Option<(&'static [&'static str], bool)>,
    /// Whether `CreateKnowledgeBase` is answered with the knowledge base.
    accepted: bool,
}

/// RULE: a deploy of an OpenSearch-Serverless-backed knowledge base behind a managed interface
/// endpoint reaches a verdict — the endpoint and the collection each settle into `ACTIVE` on
/// their own, and `CreateKnowledgeBase` answers the binding in the create call itself, 202 with
/// the knowledge base or `ValidationException` (400) — whatever the collection's `network`
/// policy admits traffic from, because the binding is resolved against the collection's stored
/// `data` access policies in the control plane.
#[tokio::test]
async fn an_endpoint_backed_knowledge_base_deploy_converges_to_one_verdict() {
    let rows: &[Row] = &[
        Row {
            case: "public traffic, one private subnet, every index permission granted to the \
                   role",
            zones: &["us-west-2a"],
            from_anywhere: true,
            data_access: Some((&INDEX_PERMISSIONS, true)),
            accepted: true,
        },
        Row {
            case: "only the managed endpoint admitted, two private subnets, every index \
                   permission granted to the role",
            zones: &["us-west-2a", "us-west-2b"],
            from_anywhere: false,
            data_access: Some((&INDEX_PERMISSIONS, true)),
            accepted: true,
        },
        Row {
            case: "only the managed endpoint admitted, three private subnets, a wildcard grant \
                   to the role",
            zones: &["us-west-2a", "us-west-2b", "us-west-2c"],
            from_anywhere: false,
            data_access: Some((&["aoss:*"], true)),
            accepted: true,
        },
        Row {
            case: "only the managed endpoint admitted, three private subnets, no data access \
                   policy at all",
            zones: &["us-west-2a", "us-west-2b", "us-west-2c"],
            from_anywhere: false,
            data_access: None,
            accepted: false,
        },
        Row {
            case: "public traffic, one private subnet, the index granted to another principal",
            zones: &["us-west-2a"],
            from_anywhere: true,
            data_access: Some((&["aoss:*"], false)),
            accepted: false,
        },
        Row {
            case: "only the managed endpoint admitted, two private subnets, the index reads \
                   granted without the write",
            zones: &["us-west-2a", "us-west-2b"],
            from_anywhere: false,
            data_access: Some((&["aoss:DescribeIndex", "aoss:ReadDocument"], true)),
            accepted: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let Row {
            case,
            zones,
            from_anywhere,
            data_access,
            accepted,
        } = row;
        let router = deployment_estate();

        // The network the endpoint is placed in is EC2's own, so every id below is one EC2
        // minted rather than one this suite made up.
        let vpc = ec2_vpc(&router, 150 + u8::try_from(index).expect("a small row count"), zones)
            .await;
        let endpoint_name = format!("kb-deploy-endpoint-{index}");
        let (status, created) = aoss(
            &router,
            "CreateVpcEndpoint",
            json!({
                "name": endpoint_name,
                "vpcId": vpc.id,
                "subnetIds": vpc.subnets,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateVpcEndpoint accepted, got {status} with {created}"
        );
        let endpoint_id = text(&created, "createVpcEndpointDetail.id");
        assert!(
            !endpoint_id.is_empty(),
            "case {case:?}: expected a minted endpoint id, got {created}"
        );

        // The first waited-on status: PENDING on the create, ACTIVE on its own a moment later.
        let endpoint = await_active_vpc_endpoint(&router, case, &endpoint_id).await;
        assert_eq!(
            at(&endpoint, "subnetIds"),
            json!(vpc.subnets),
            "case {case:?}: expected the settled endpoint to report the subnets it was placed \
             in, got {endpoint}"
        );

        // The policies the collection is created under: its encryption, and the boundary this
        // row states — stated BEFORE the collection, so the address it publishes is opened
        // behind the endpoint its covering statement names.
        let collection = format!("kb-deploy-{index}");
        encrypt_collections(&router).await;
        let source = if *from_anywhere {
            json!({ "AllowFromPublic": true })
        } else {
            json!({ "SourceVPCEs": [endpoint_id] })
        };
        given_security_policy(
            &router,
            case,
            "network",
            &format!("kb-deploy-net-{index}"),
            &network_policy(&collection, source),
        )
        .await;
        if let Some((permissions, names_the_role)) = data_access {
            let principal = if *names_the_role {
                KB_ROLE
            } else {
                OTHER_ROLE
            };
            given_data_access(
                &router,
                case,
                &format!("kb-deploy-data-{index}"),
                &data_policy(&collection, permissions, principal),
            )
            .await;
        }

        // The second waited-on status: CREATING on the create, ACTIVE on its own a moment
        // later, whichever source form the boundary states.
        let detail = active_collection(&router, &collection).await;
        assert_eq!(
            text(&detail, "status"),
            ACTIVE,
            "case {case:?}: expected the collection to settle into {ACTIVE}, got {detail}"
        );
        let collection_arn = text(&detail, "arn");
        assert!(
            !collection_arn.is_empty() && !text(&detail, "collectionEndpoint").is_empty(),
            "case {case:?}: expected a settled collection to report an ARN and the address it \
             publishes, got {detail}"
        );

        // ...and the verdict, in the create call itself.
        let name = format!("kb-deploy-{index}");
        let (status, answered) = bedrock(
            &router,
            "PUT",
            "/knowledgebases/",
            Some(&json!({
                "name": name,
                "roleArn": KB_ROLE,
                "knowledgeBaseConfiguration": {
                    "type": "VECTOR",
                    "vectorKnowledgeBaseConfiguration": {
                        "embeddingModelArn":
                            format!("arn:aws:bedrock:{REGION}::foundation-model/{EMBEDDING_MODEL}"),
                    },
                },
                "storageConfiguration": opensearch_storage(&collection_arn, VECTOR_INDEX),
            })),
        )
        .await;
        if *accepted {
            assert_eq!(
                status, 202,
                "case {case:?}: expected CreateKnowledgeBase accepted, got {status} with \
                 {answered}"
            );
            // The third waited-on status: CREATING on the create, ACTIVE on its own — and the
            // knowledge base reads back carrying the store it was bound to.
            let knowledge_base_id = text(&answered, "knowledgeBase.knowledgeBaseId");
            assert!(
                !knowledge_base_id.is_empty(),
                "case {case:?}: expected a minted knowledgeBaseId, got {answered}"
            );
            let read = await_active_knowledge_base(&router, case, &knowledge_base_id).await;
            assert_eq!(
                at(&read, "storageConfiguration"),
                opensearch_storage(&collection_arn, VECTOR_INDEX),
                "case {case:?}: expected the knowledge base to report the storage \
                 configuration it was created with, got {read}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected CreateKnowledgeBase refused, got {status} with \
                 {answered}"
            );
            assert!(
                text(&answered, "__type").contains("ValidationException"),
                "case {case:?}: expected the modelled ValidationException, got {answered}"
            );
            assert_eq!(
                text(&answered, "message"),
                UNREACHABLE_STORAGE,
                "case {case:?}: expected the refusal Amazon Bedrock words for a storage \
                 configuration its role cannot reach, got {answered}"
            );
            assert_eq!(
                at(&answered, "knowledgeBase"),
                Value::Null,
                "case {case:?}: expected a refusal to mint no knowledge base, got {answered}"
            );
        }

        let held = knowledge_base_names(&router, case).await;
        assert_eq!(
            held.contains(&name),
            *accepted,
            "case {case:?}: expected the knowledge base {}, got {held:?}",
            if *accepted { "listed" } else { "absent" }
        );
    }
}
