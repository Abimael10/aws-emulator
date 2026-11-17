// SPDX-License-Identifier: CC0-1.0

//! The two readings a knowledge base's resource-based policy turns on, as
//! `crate::state::resource_policies` makes them: which knowledge base a
//! resource-policy operation's `resourceArn` names, and which KIND of knowledge
//! base carries a policy at all.
//!
//! `PutResourcePolicy` and `GetResourcePolicy` are still scaffold stubs (each has
//! an issue of its own, and the wording of the refusals AWS answers there has to
//! come from the recorded responses), so what is pinned here is the pair of
//! readings those operations will agree on — over a table of ARN forms and over
//! every `KnowledgeBaseType` the model declares — rather than either operation's
//! wire behaviour.

use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state::{
    carries_resource_policy, policied_knowledge_base, MANAGED_KNOWLEDGE_BASE_TYPE,
};

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
