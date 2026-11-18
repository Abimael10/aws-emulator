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
//! The two readings live here, out of the handlers and out of this module root,
//! because they are what every resource-policy operation agrees on: an ARN
//! resolves through this plane's one ARN reader, and the KIND test is a single
//! sentence the write and the read have to make the same way. Both are now folded
//! into ONE resolution, [`ComAmazonawsBedrockagentAmazonbedrockagentbuildtimelambdaState::policy_bearing_knowledge_base`],
//! which answers either the knowledge base whose policy the call is about or the
//! reason there is none to be about — so `GetResourcePolicy` and the write and
//! delete beside it (each still a scaffold stub with an issue of its own) cannot
//! disagree on which ARN names what.
//!
//! The policy itself is held on the knowledge base's own record
//! ([`KnowledgeBaseRecord::resource_policy`]), as one [`ResourcePolicyRecord`]: the
//! document a write stored, and the revision that write left it at.

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

/// One knowledge base's resource-based policy: the document a write stored, and the
/// revision that write left it at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourcePolicyRecord {
    /// The policy document, held exactly as the write supplied it — the modelled
    /// `ResourcePolicy` is a string, so a read reports the same bytes back.
    pub policy: String,
    /// How many writes this knowledge base's policy has taken, which is the
    /// revision it is now at ([`Self::revision_id`] is how the wire carries it).
    pub revision: u64,
}

impl ResourcePolicyRecord {
    /// This revision as the modelled `revisionId`.
    ///
    /// The shape states only a length (1..255), and what the operations state the
    /// value is FOR settles its form: `PutResourcePolicy$expectedRevisionId` and
    /// `DeleteResourcePolicy$expectedRevisionId` are "the revisionId from the most
    /// recent GetResourcePolicy or PutResourcePolicy response", used "to prevent
    /// conflicts when multiple users update the same policy concurrently" — so the
    /// value has to change on every write and be comparable by equality, and
    /// nothing else about it is observable. It therefore COUNTS this policy's
    /// writes rather than being drawn at random: it is derived from stored state,
    /// stable across reads of an unchanged policy, and different after each change.
    pub fn revision_id(&self) -> String {
        self.revision.to_string()
    }
}

impl KnowledgeBaseRecord {
    /// Attach `policy` to this knowledge base, answering the `revisionId` the write
    /// left the policy at.
    ///
    /// The one writer of [`Self::resource_policy`], so every write advances the
    /// revision the same way: a first policy is revision 1 and each write past it
    /// counts one more.
    pub fn attach_resource_policy(&mut self, policy: String) -> String {
        let revision = self
            .resource_policy
            .as_ref()
            .map_or(0, |stored| stored.revision)
            .saturating_add(1);
        let record = ResourcePolicyRecord { policy, revision };
        let revision_id = record.revision_id();
        self.resource_policy = Some(record);
        revision_id
    }
}

/// Why a resource-policy operation has no knowledge-base policy to act on.
///
/// The three readings are ordered as the questions nest: a value that names no
/// knowledge base of this plane is answered before any store is consulted, a
/// knowledge base nothing holds before its kind is read, and the kind last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourcePolicyRefusal {
    /// The `resourceArn` names no knowledge base of this plane — a bad request,
    /// however well-formed the value is as some other ARN.
    NotAKnowledgeBase,
    /// The ARN names a knowledge base this store does not hold (or holds only on
    /// its way out) — a missing resource, carrying the id the ARN named.
    NoSuchKnowledgeBase(String),
    /// The knowledge base is there, but its KIND carries no resource policy at all,
    /// so there is none to read or write: a bad request.
    KindCarriesNoPolicy,
}

impl ComAmazonawsBedrockagentAmazonbedrockagentbuildtimelambdaState {
    /// The knowledge base whose resource-based policy a call about `resource_arn` is
    /// about, or why there is no such policy.
    ///
    /// The ONE resolution every resource-policy operation shares, so the read, the
    /// write and the delete answer one ARN alike.
    pub fn policy_bearing_knowledge_base(
        &self,
        resource_arn: &str,
    ) -> Result<&KnowledgeBaseRecord, ResourcePolicyRefusal> {
        let knowledge_base_id = policied_knowledge_base(resource_arn)
            .ok_or(ResourcePolicyRefusal::NotAKnowledgeBase)?;
        let record = self.live_knowledge_base(&knowledge_base_id).ok_or(
            ResourcePolicyRefusal::NoSuchKnowledgeBase(knowledge_base_id.clone()),
        )?;
        let knowledge_base_type = record.knowledge_base_configuration.r#type.as_str();
        if !carries_resource_policy(knowledge_base_type) {
            return Err(ResourcePolicyRefusal::KindCarriesNoPolicy);
        }
        Ok(record)
    }
}

/// The modelled `ResourceArn` pattern, quoted verbatim so a refusal can name the
/// constraint the value failed.
pub const RESOURCE_POLICY_ARN_PATTERN: &str =
    "^arn:aws(-[^:]+)?:bedrock:[a-z0-9-]{1,20}:[0-9]{12}:knowledge-base/[0-9a-zA-Z]+$";

/// The `ValidationException` message a resource-policy operation answers for a
/// `resourceArn` that names no knowledge base of this plane: the member and the
/// constraint the value failed, with the value from the request.
///
/// The same counted-constraint frame this service answers for every other member
/// whose value is outside the syntax its shape spells (see
/// [`taggable_resource_arn_pattern_message`] and [`flow_identifier_pattern_message`]).
pub fn resource_policy_arn_pattern_message(resource_arn: &str) -> String {
    format!(
        "Value '{resource_arn}' at 'resourceArn' failed to satisfy constraint: \
         Member must satisfy regular expression pattern: {RESOURCE_POLICY_ARN_PATTERN}"
    )
}

/// The `ValidationException` message a resource-policy operation answers when the
/// knowledge base it names is of a kind that carries no resource policy.
///
/// AWS's wording for this error class is ENTIRELY fixed text: it names the one kind
/// that does carry a policy rather than the kind the request reached, so no part of
/// it comes from the request. The kind it names is the one the reading above admits
/// ([`MANAGED_KNOWLEDGE_BASE_TYPE`]), so the sentence and the rule it refuses cannot
/// drift apart.
pub fn unsupported_resource_policy_message() -> String {
    format!(
        "Resource policies are only supported for {MANAGED_KNOWLEDGE_BASE_TYPE} \
         type knowledge bases."
    )
}

/// The `ResourceNotFoundException` message a resource-policy operation answers when
/// the knowledge base it names carries no policy: this service's own missing-resource
/// sentence, with the resource type that could not be retrieved being the policy
/// itself rather than the knowledge base, which is there.
///
/// The recorded refusals of this surface cover the knowledge base that is not there
/// ([`policied_knowledge_base_not_found_message`]) and the kind that carries no policy
/// ([`unsupported_resource_policy_message`]); for the knowledge base that is there
/// with no policy on it, this service's own sentence for a resource it could not
/// retrieve is the one answered, through the one helper that states it.
pub fn resource_policy_not_found_message(operation: &str) -> String {
    resource_not_found_message(operation, "ResourcePolicy")
}

/// The `ResourceNotFoundException` message a resource-policy operation answers when
/// the ARN names a knowledge base this store does not hold.
///
/// AWS's wording for this error class, which is fixed text but for the knowledge
/// base's id — the one the `resourceArn` named, taken from the request. It is this
/// plane's resource-policy sentence rather than [`resource_not_found_message`], which
/// the operations addressed by a `knowledgeBaseId` answer.
pub fn policied_knowledge_base_not_found_message(knowledge_base_id: &str) -> String {
    format!("KnowledgeBase with id {knowledge_base_id} is not found.")
}
