// SPDX-License-Identifier: CC0-1.0

//! The two readings a knowledge base's resource-based POLICY turns on: which
//! knowledge base a resource-policy operation's `resourceArn` names, and which
//! KIND of knowledge base carries a policy at all.
//!
//! `PutResourcePolicy` "associates a resource policy with a knowledge base. A
//! resource policy allows other AWS accounts to access the knowledge base", and it
//! does so only for a knowledge base Amazon Bedrock MANAGES
//! ([`MANAGED_KNOWLEDGE_BASE_TYPE`]): the recorded refusals of both
//! `PutResourcePolicy` and `GetResourcePolicy` against a knowledge base of another
//! kind are `ValidationException`s, so for every other `KnowledgeBaseType` a
//! resource policy cannot exist and naming one is a bad request rather than a
//! policy that is merely absent.
//!
//! Both operations are still scaffold stubs — `src/ops/put_resource_policy.rs` and
//! `src/ops/get_resource_policy.rs` carry the census marker, and each has an issue
//! of its own. The two readings live here, out of the handlers and out of this
//! module root, because they are what those operations agree on: an ARN resolves
//! through this plane's one ARN reader, and the KIND test is a single sentence both
//! the write and the read have to make the same way. What this emulator cannot
//! derive for them is the WORDING of the refusals AWS answers — neither the model
//! nor the operations' documentation states a sentence for either class — so the
//! messages belong to the fixer who has the recorded responses in front of them.

use super::*;

/// The knowledge base a resource-policy operation's `resourceArn` names, or `None`
/// when the value names no knowledge base of this plane.
///
/// The ARN is resolved through the one reader of this plane's own ARNs
/// ([`taggable_resource`]), so `arn:…:knowledge-base/<id>` selects the very record
/// the knowledge base's own reads report, and an ARN of another of this plane's
/// resources — an agent, a flow — names no knowledge base to carry a policy.
/// Whether that knowledge base EXISTS is a different question: an ARN of the right
/// syntax naming nothing stored is a missing resource.
pub fn policied_knowledge_base(resource_arn: &str) -> Option<String> {
    match taggable_resource(resource_arn)? {
        TaggableResource::KnowledgeBase(knowledge_base_id) => Some(knowledge_base_id),
        _ => None,
    }
}

/// Whether a knowledge base of this `knowledgeBaseConfiguration.type` carries a
/// resource-based policy at all: the service-managed kind, and no other (see the
/// module docs).
pub fn carries_resource_policy(knowledge_base_type: &str) -> bool {
    knowledge_base_type == MANAGED_KNOWLEDGE_BASE_TYPE
}
