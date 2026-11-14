// SPDX-License-Identifier: CC0-1.0

//! What an identity's MANAGED-POLICY ATTACHMENTS contribute to the authorization decision
//! this service takes, end to end over the wire.
//!
//! RULE: the identity-based decision is made from the documents in force on the principal,
//! and a managed-policy attachment contributes the document of its default version wherever
//! this emulator holds one — the account's own policy store for a policy the account created,
//! and IAM's published catalog of AWS-managed policies for one AWS owns. So a role whose only
//! attachment grants an unrelated service's actions is refused this service's action
//! (`AccessDeniedException`, 403), and the same role is let through to the resource once a
//! statement in force on it allows that action — which is what makes an under-permissioned
//! role distinguishable from a permitted one, rather than every attachment being read as a
//! grant nothing can refuse.
//!
//! The rows differ along what the rule generalises over: WHERE the deciding document comes
//! from (an AWS-managed attachment alone, an attachment beside an inline statement, an
//! attachment beside an inline statement that denies), and which operation is called (the
//! agent invocation, whose answer is an event stream, and the knowledge-base retrieval).
//!
//! The emulator, the credential minting and the reply reader are
//! [`crate::agent_memory`]'s, so this suite states nothing of its own about how a role is
//! created, assumed or called.

use serde_json::json;

use crate::agent_memory::{allow, deny, Emulator, ACCESS_DENIED, NOT_FOUND};

/// The AWS-managed policy every Lambda execution role carries, whose document IAM's own
/// catalog publishes: it allows the three CloudWatch Logs actions and nothing else, so it
/// grants no action of this service.
const LOGGING_POLICY: &str = "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// A well-formed `agentId` no agent here carries (len ..10, pattern `^[0-9a-zA-Z]+$`), and
/// the draft alias every agent carries.
const UNKNOWN_AGENT: &str = "ZZZZZZZZZ9";
const DRAFT_ALIAS: &str = "TSTALIASID";

/// A well-formed `knowledgeBaseId` nothing holds (pattern `^[0-9a-zA-Z]{10}$`).
const UNKNOWN_KNOWLEDGE_BASE: &str = "ZZZZZZZZZZ";

/// The two operations a row drives, each with the IAM action it is decided against.
struct Call {
    operation: &'static str,
    action: &'static str,
}

const CALLS: &[Call] = &[
    Call {
        operation: "InvokeAgent",
        action: "bedrock:InvokeAgent",
    },
    Call {
        operation: "Retrieve",
        action: "bedrock:Retrieve",
    },
];

/// One principal: the inline statement in force on the role beside the attachment, and what
/// both calls answer it.
struct Row {
    case: &'static str,
    /// The inline policy the role carries beside [`LOGGING_POLICY`], if any.
    policy: Option<fn() -> String>,
    expected: (&'static str, u16),
}

const ROWS: &[Row] = &[
    Row {
        case: "a role whose only grant is the attached logging policy",
        policy: None,
        expected: ACCESS_DENIED,
    },
    Row {
        case: "a role granted an unrelated action beside that attachment",
        policy: Some(|| allow(&["bedrock:ListSessions"])),
        expected: ACCESS_DENIED,
    },
    Row {
        case: "a role whose inline statement denies the actions outright",
        policy: Some(|| deny(&["bedrock:InvokeAgent", "bedrock:Retrieve"])),
        expected: ACCESS_DENIED,
    },
    Row {
        case: "a role granted both actions beside that attachment",
        policy: Some(|| allow(&["bedrock:InvokeAgent", "bedrock:Retrieve"])),
        // Allowed through to the resource, which is one nothing holds.
        expected: NOT_FOUND,
    },
];

/// RULE: an attachment to a policy whose document this emulator holds is READ, so what the
/// policy grants is what the principal may do — a role carrying only the logging policy is
/// refused this service's actions, and the same role is let through to the resource once a
/// statement allows them.
#[tokio::test]
async fn an_attached_managed_policy_decides_what_the_caller_may_do() {
    let emulator = Emulator::new();
    for (index, row) in ROWS.iter().enumerate() {
        let role_name = format!("attachment-caller-{index}");
        let policy = row.policy.map(|build| build());
        let key = emulator
            .assumed_role_key(&role_name, policy.as_deref())
            .await;
        // The grant the role did not write itself, put on with IAM's own operation.
        emulator.attach_role_policy(&role_name, LOGGING_POLICY).await;
        for call in CALLS {
            let reply = match call.operation {
                "InvokeAgent" => {
                    emulator
                        .bedrock(
                            &key,
                            "POST",
                            &format!(
                                "/agents/{UNKNOWN_AGENT}/agentAliases/{DRAFT_ALIAS}/sessions\
                                 /attachment-session/text"
                            ),
                            Some(json!({"inputText": "hi"})),
                        )
                        .await
                }
                _ => {
                    emulator
                        .bedrock(
                            &key,
                            "POST",
                            &format!("/knowledgebases/{UNKNOWN_KNOWLEDGE_BASE}/retrieve"),
                            Some(json!({"retrievalQuery": {"text": "hi"}})),
                        )
                        .await
                }
            };
            let case = format!("{}, calling {}", row.case, call.operation);
            assert_eq!(
                (reply.error_code(&case).as_str(), reply.status),
                row.expected,
                "case {case:?}: expected {:?}, got {} {:?}",
                row.expected,
                reply.status,
                reply.json(&case)
            );
            if row.expected == ACCESS_DENIED {
                assert!(
                    reply.message(&case).contains(call.action),
                    "case {case:?}: the refusal must name the action it was taken on, got {:?}",
                    reply.message(&case)
                );
            }
        }
    }
}
