// SPDX-License-Identifier: CC0-1.0

//! An IAM identity whose calls this control plane's access decision allows, shared by the
//! suites whose subject is what an operation ANSWERS rather than who may call it.
//!
//! The access decision this endpoint takes ahead of validation
//! (`src/service/authorization.rs`) is the caller's: the credential this emulator publishes
//! carries no IAM document at all, and the standing exemption that lets its calls through
//! stands in for the account's authority over its own resources — so a call addressing a
//! resource by an identifier no resource of its family can carry, which addresses none of
//! them, is decided by IAM's own evaluation and refused for a principal no readable statement
//! allows (`unnameable_identifier_authorization.rs` is that rule's suite).
//!
//! A suite that sends such a request to read the answer the OPERATION gives it therefore signs
//! as a principal the policies allow: an IAM user holding an inline `Allow` of every action of
//! this endpoint's namespace on every resource, recorded in the IAM store of the suite's own
//! engine the way `iam:CreateUser`, `iam:CreateAccessKey` and `iam:PutUserPolicy` record one.
//! Nothing else about those suites changes: every grant is in force, so each one's calls reach
//! the operation exactly as they did when they were signed with the published credential.

use std::collections::BTreeMap;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts;

/// The access key id a suite signs with to be judged as an allowed principal.
pub(crate) const AUTHORIZED_KEY_ID: &str = "AKIAEXAMPLEALLOWALL1";

/// The IAM user that key is held for.
const USER: &str = "agentcore-allowed";

/// The name of its inline policy.
const POLICY_NAME: &str = "allow-agentcore";

/// This endpoint's IAM action namespace: its `sigv4Name` and `arnNamespace`
/// (`smithy_query(query="service_info")`).
const ACTION_PREFIX: &str = "bedrock-agentcore";

/// Record the IAM user [`AUTHORIZED_KEY_ID`] belongs to, with an inline policy allowing every
/// action of this endpoint's namespace on every resource, in `engine`'s IAM store — and answer
/// the ARN the account holds that user under, which is the principal any refusal would name.
pub(crate) fn given_authorized_caller(engine: &engine::InMemoryEngine) -> String {
    let arn = format!(
        "arn:aws:iam::{}:user/{USER}",
        ::engine::emulator_account_id()
    );
    let minted = ::aws_smithy_types::DateTime::from_secs(0);
    let store = iam::service_state(engine).expect("the IAM store of this engine");
    let mut store = store.lock();
    store.users.insert(
        iam::name_key(USER),
        User {
            path: "/".to_string(),
            user_name: USER.to_string(),
            user_id: sts::unique_id("AIDA", &arn),
            arn: arn.clone(),
            create_date: minted,
            password_last_used: None,
            permissions_boundary: None,
            tags: None,
        },
    );
    store.access_keys.insert(
        AUTHORIZED_KEY_ID.to_string(),
        iam::AccessKeyRecord {
            user_key: iam::name_key(USER),
            status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
            create_date: minted,
        },
    );
    store.user_inline_policies.insert(
        iam::name_key(USER),
        BTreeMap::from([(
            iam::name_key(POLICY_NAME),
            iam::InlinePolicy {
                policy_name: POLICY_NAME.to_string(),
                policy_document: format!(
                    r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow",
                     "Action":"{ACTION_PREFIX}:*","Resource":"*"}}]}}"#
                ),
            },
        )]),
    );
    arn
}
