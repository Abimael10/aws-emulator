// SPDX-License-Identifier: CC0-1.0

//! The AUTHORIZATION refusal fits the member the model declares it in, driven end to end
//! over the real wire path.
//!
//! RULE: the prose an error carries is held by a MODELLED member, so what a refusal may say
//! is bounded by that member's own declared `@length` maximum — on every path this service
//! words one, and whatever shape carries it. `AccessDeniedException` declares exactly one
//! member, `Message`, targeting `FailureReason`, `@length` min 0 max 1024
//! (`smithy_query(query="shape",shape="AccessDeniedException,FailureReason")`), the same
//! member and maximum every other error shape of this service declares. The authorization
//! gate's sentence states two values it does not size itself: the `JobArn` the request named,
//! and the ARN of the principal the credential speaks for — which is read out of the IAM
//! store and is therefore bounded by the IAM model rather than by this one, an IAM user's own
//! `Path` (max 512) and `UserName` (max 64) alone admitting an ARN longer than half this
//! member. So the sentence is held to what the carrying member leaves: each value is stated
//! whole where it fits, and by a prefix plus the marker naming its byte count where it does
//! not, while the wording around them — who, which action, which resource, which of IAM's
//! rules refused — is IAM's unchanged.
//!
//! The rows span what the rule generalises over rather than one request:
//!
//! * the PRINCIPAL's size — an IAM user under the root path, one whose name is at the IAM
//!   model's `UserName` maximum, one whose path is half the `Path` maximum, and one at both
//!   maxima at once, which is the longest ARN that model admits;
//! * the RESOURCE's size — a `JobArn` of the length a control plane mints, and one at
//!   `JobArn`'s own modelled maximum of 256;
//! * the REASON — IAM's default deny, whose clause names the action twice, and its explicit
//!   deny, whose clause is shorter, so the same values fit one sentence and not the other;
//! * the FORM — the identity-based denial for a resource of the caller's own account, and the
//!   sentence naming the principal alone for a resource of another;
//! * the OPERATION — all four the model binds, whose names differ in length and so leave the
//!   values different room.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::User;
use service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam;
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    job_arn, mounted_with_identity, send_as, Reply, ACCOUNT, COMPLETE_ROLLOUT_PATH,
    JOB_ARN_HEADER, REGION, SAMPLE_PATH, SAMPLE_STREAM_PATH, TRAJECTORY_ID_HEADER,
    UPDATE_REWARD_PATH,
};

/// The maximum the model declares for the member every refusal of this service is carried
/// in: `Message` targets `FailureReason`, `@length` min 0 max 1024
/// (`smithy_query(query="shape",shape="AccessDeniedException,FailureReason")`). No message
/// below may pass it.
const LARGEST_MESSAGE_BYTES: usize = 1024;

/// `JobArn`'s own modelled `@length` maximum
/// (`smithy_query(query="shape",shape="JobArn")`), which is the longest resource a request
/// of this service can name.
const JOB_ARN_MAX: usize = 256;

/// The IAM model's maxima for the parts an IAM user's ARN is built from: `pathType`
/// `@length` max 512 and `userNameType` `@length` max 64 (the IAM model's
/// `smithy_query(query="shape",shape="pathType,userNameType")`). Together with the ARN's own
/// fixed prefix they bound the principal this gate can be handed.
const IAM_PATH_MAX: usize = 512;
const IAM_USER_NAME_MAX: usize = 64;

/// This service's IAM action prefix, from its `arnNamespace`
/// (`smithy_query(query="service_info")`).
const ACTION_PREFIX: &str = "sagemaker";

/// The access key id every row signs with: a long-term key the IAM store maps to the user
/// the row created.
const USER_KEY_ID: &str = "AKIDEXAMPLELONGTERM1";

/// The name of the inline policy a row puts in force on that user.
const POLICY_NAME: &str = "inline";

/// The job every row addresses, and the trajectory the calls name.
const JOB_CATEGORY: &str = "AgentRFT";
const JOB_NAME: &str = "authz-run";
const TRAJECTORY: &str = "traj-1";

/// An account that is NOT the one this emulator serves, for the cross-account form.
const FOREIGN_ACCOUNT: &str = "999999999999";

/// The four operations the model binds, each with the path it is bound to.
const OPERATIONS: [(&str, &str); 4] = [
    ("Sample", SAMPLE_PATH),
    ("SampleWithResponseStream", SAMPLE_STREAM_PATH),
    ("CompleteRollout", COMPLETE_ROLLOUT_PATH),
    ("UpdateReward", UPDATE_REWARD_PATH),
];

/// Record the IAM user `path` / `user_name` holding [`USER_KEY_ID`], as `iam:CreateUser` and
/// `iam:CreateAccessKey` record them — the user under the case-folded name key, and the
/// access key in the registry that maps a key id to the user that owns it — and hand back the
/// ARN the IAM store holds it under, which is what a refusal names.
fn given_user(engine: &Arc<engine::InMemoryEngine>, path: &str, user_name: &str) -> String {
    let arn = iam::user_arn(path, user_name);
    let store = iam::service_state(&**engine).expect("the IAM store");
    let mut store = store.lock();
    store.users.insert(
        iam::name_key(user_name),
        User {
            path: path.to_string(),
            user_name: user_name.to_string(),
            user_id: "AIDAEXAMPLEUSER000001".to_string(),
            arn: arn.clone(),
            create_date: ::aws_smithy_types::DateTime::from_secs(0),
            password_last_used: None,
            permissions_boundary: None,
            tags: None,
        },
    );
    store.access_keys.insert(
        USER_KEY_ID.to_string(),
        iam::AccessKeyRecord {
            user_key: iam::name_key(user_name),
            status: iam::ACCESS_KEY_ACTIVE_STATUS.to_string(),
            create_date: ::aws_smithy_types::DateTime::from_secs(0),
        },
    );
    arn
}

/// Put `document` in force on that user as its inline policy, as `iam:PutUserPolicy` does.
fn given_user_policy(engine: &Arc<engine::InMemoryEngine>, user_name: &str, document: &Value) {
    let store = iam::service_state(&**engine).expect("the IAM store");
    store.lock().user_inline_policies.insert(
        iam::name_key(user_name),
        BTreeMap::from([(
            iam::name_key(POLICY_NAME),
            iam::InlinePolicy {
                policy_name: POLICY_NAME.to_string(),
                policy_document: document.to_string(),
            },
        )]),
    );
}

/// A policy whose explicit `Deny` covers every action of this service, which is the shorter
/// of the two reasons IAM refuses with.
fn deny_everything_of_this_service() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
            { "Effect": "Deny", "Action": format!("{ACTION_PREFIX}:*"), "Resource": "*" },
        ],
    })
}

/// A `JobArn` of `account` carrying exactly `bytes` bytes, padded in the segment the modelled
/// `@pattern` leaves unbounded — the job category, `[a-zA-Z0-9_\-]+` — so the ARN is
/// well-formed at any size from the one a control plane mints up to `JobArn`'s own maximum.
fn job_arn_of(account: &str, bytes: usize) -> String {
    let minted = job_arn("aws", REGION, account, JOB_CATEGORY, JOB_NAME);
    assert!(
        bytes >= minted.len(),
        "a job ARN of {bytes} bytes is shorter than the {} a minted one carries",
        minted.len()
    );
    let category = format!("{JOB_CATEGORY}{}", "c".repeat(bytes - minted.len()));
    job_arn("aws", REGION, account, &category, JOB_NAME)
}

/// How long a `JobArn` the control plane mints for the job every row names.
fn minted_bytes() -> usize {
    job_arn("aws", REGION, ACCOUNT, JOB_CATEGORY, JOB_NAME).len()
}

/// An IAM `Path` of exactly `bytes` bytes, in the form the IAM model's `pathType` pattern
/// admits (`/…/`, or the root path alone).
fn path_of(bytes: usize) -> String {
    if bytes <= 1 {
        return "/".to_string();
    }
    format!("/{}/", "p".repeat(bytes - 2))
}

/// An IAM `UserName` of exactly `bytes` bytes, in the form `userNameType` admits.
fn user_name_of(bytes: usize) -> String {
    format!("u{}", "s".repeat(bytes - 1))
}

/// One call of the operation bound at `path`, signed with [`USER_KEY_ID`] and addressing
/// `job`: the sampling operations carry `TrajectoryId` in a header, the rollout operations in
/// the JSON body.
async fn call(router: &MultiServiceRouter, path: &str, job: &str) -> Reply {
    let json_header = ("content-type", "application/json");
    let (headers, body): (Vec<(&str, &str)>, Value) = match path {
        SAMPLE_PATH | SAMPLE_STREAM_PATH => (
            vec![
                (JOB_ARN_HEADER, job),
                (TRAJECTORY_ID_HEADER, TRAJECTORY),
                json_header,
            ],
            json!({ "messages": [{ "role": "user", "content": "go" }] }),
        ),
        COMPLETE_ROLLOUT_PATH => (
            vec![(JOB_ARN_HEADER, job), json_header],
            json!({ "TrajectoryId": TRAJECTORY, "Status": "ready" }),
        ),
        UPDATE_REWARD_PATH => (
            vec![(JOB_ARN_HEADER, job), json_header],
            json!({ "TrajectoryId": TRAJECTORY, "Rewards": [1.0] }),
        ),
        other => unreachable!("this model binds no operation at {other}"),
    };
    send_as(
        router,
        USER_KEY_ID,
        path,
        &headers,
        serde_json::to_vec(&body).expect("a serializable body"),
    )
    .await
}

/// One row of the table: who the request speaks for, which job it names and which of IAM's
/// two refusals it meets.
struct Row {
    case: &'static str,
    /// The IAM user's `Path`, in bytes — 1 is the root path and 512 the modelled maximum.
    path_bytes: usize,
    /// The IAM user's `UserName`, in bytes, up to the modelled maximum of 64.
    name_bytes: usize,
    /// The account the `JobArn` names: the caller's own, or another one.
    job_account: &'static str,
    /// How many bytes the `JobArn` carries, up to its own modelled maximum of 256.
    job_bytes: usize,
    /// Whether an explicit `Deny` is in force, which is the shorter of IAM's two reasons.
    explicit_deny: bool,
}

/// The reason clause IAM closes a refusal with, in the form the row's policy brings about.
fn reason(explicit_deny: bool, action: &str) -> String {
    if explicit_deny {
        "with an explicit deny in an identity-based policy".to_string()
    } else {
        format!("because no identity-based policy allows the {action} action")
    }
}

/// The sentence IAM states this refusal with BEFORE anything is held to the carrying member:
/// the identity-based denial for a resource of the caller's own account, and the sentence
/// naming the principal alone for a resource of another.
fn unheld_sentence(principal: &str, action: &str, resource: &str, row: &Row) -> String {
    if row.job_account == ACCOUNT {
        format!(
            "User: {principal} is not authorized to perform: {action} on resource: {resource} {}",
            reason(row.explicit_deny, action)
        )
    } else {
        format!("User: {principal} is not authorized to access this resource")
    }
}

/// How much of a value a held sentence must still state, so a caller can tell WHICH principal
/// or resource was refused even where the whole of it does not fit.
const LEADING_BYTES_STATED: usize = 32;

/// Assert that `message` states `value` as the rule requires: whole where the sentence fit
/// the carrying member, and otherwise as a leading prefix plus the marker naming the bytes
/// there were, with the value not carried whole.
fn assert_states(case: &str, message: &str, value: &str, whole: bool) {
    if whole {
        assert!(
            message.contains(value),
            "case {case:?}: expected the {} byte value stated whole, got the {} byte message \
             {:?}",
            value.len(),
            message.len(),
            message.chars().take(160).collect::<String>()
        );
        return;
    }
    let marker = format!("... [{} bytes]", value.len());
    assert!(
        message.contains(marker.as_str()),
        "case {case:?}: expected the held value to name the {} bytes there were with \
         {marker:?}, got the {} byte message {:?}",
        value.len(),
        message.len(),
        message.chars().take(160).collect::<String>()
    );
    assert!(
        !message.contains(value),
        "case {case:?}: expected the {} byte value NOT to be carried whole in a message held \
         to {LARGEST_MESSAGE_BYTES} bytes",
        value.len()
    );
    assert!(
        message.contains(&value[..LEADING_BYTES_STATED.min(value.len())]),
        "case {case:?}: expected the held value to still state its first \
         {LEADING_BYTES_STATED} bytes, got {:?}",
        message.chars().take(160).collect::<String>()
    );
}

/// THE RULE: every authorization refusal this service answers fits the `Message` member that
/// carries it, with IAM's own wording around values stated whole where they fit and by their
/// byte count where they do not.
#[tokio::test]
async fn an_authorization_refusal_fits_the_modelled_message_member() {
    let rows = [
        Row {
            case: "a short user under the root path, a minted job ARN",
            path_bytes: 1,
            name_bytes: 8,
            job_account: ACCOUNT,
            job_bytes: minted_bytes(),
            explicit_deny: false,
        },
        Row {
            case: "a user name at the modelled maximum, a job ARN at its own maximum",
            path_bytes: 1,
            name_bytes: IAM_USER_NAME_MAX,
            job_account: ACCOUNT,
            job_bytes: JOB_ARN_MAX,
            explicit_deny: false,
        },
        Row {
            case: "a path half the modelled maximum, a job ARN at its own maximum",
            path_bytes: IAM_PATH_MAX / 2,
            name_bytes: IAM_USER_NAME_MAX,
            job_account: ACCOUNT,
            job_bytes: JOB_ARN_MAX,
            explicit_deny: false,
        },
        Row {
            case: "a user at both modelled maxima, a minted job ARN",
            path_bytes: IAM_PATH_MAX,
            name_bytes: IAM_USER_NAME_MAX,
            job_account: ACCOUNT,
            job_bytes: minted_bytes(),
            explicit_deny: false,
        },
        Row {
            // The reported instance: the longest principal the IAM model admits beside the
            // longest resource this one does, which words a sentence past the maximum the
            // carrying member declares.
            case: "a user at both modelled maxima, a job ARN at its own maximum",
            path_bytes: IAM_PATH_MAX,
            name_bytes: IAM_USER_NAME_MAX,
            job_account: ACCOUNT,
            job_bytes: JOB_ARN_MAX,
            explicit_deny: false,
        },
        Row {
            // The same values under IAM's shorter reason, which leaves them more room.
            case: "a user at both modelled maxima refused by an explicit deny",
            path_bytes: IAM_PATH_MAX,
            name_bytes: IAM_USER_NAME_MAX,
            job_account: ACCOUNT,
            job_bytes: JOB_ARN_MAX,
            explicit_deny: true,
        },
        Row {
            case: "a user at both modelled maxima naming a job of another account",
            path_bytes: IAM_PATH_MAX,
            name_bytes: IAM_USER_NAME_MAX,
            job_account: FOREIGN_ACCOUNT,
            job_bytes: JOB_ARN_MAX,
            explicit_deny: false,
        },
    ];

    for row in &rows {
        for (operation, path) in OPERATIONS {
            let case = format!("{} on {operation}", row.case);
            let action = format!("{ACTION_PREFIX}:{operation}");
            let resource = job_arn_of(row.job_account, row.job_bytes);

            // A fresh store per row and operation: no call can pass on another's write, and
            // no job is created, since the gate decides before anything is resolved.
            let (engine, router) = mounted_with_identity();
            let path_value = path_of(row.path_bytes);
            let user_name = user_name_of(row.name_bytes);
            let principal = given_user(&engine, &path_value, &user_name);
            if row.explicit_deny {
                given_user_policy(&engine, &user_name, &deny_everything_of_this_service());
            }

            let reply = call(&router, path, &resource).await;
            assert_eq!(
                (reply.status, reply.error_code().as_str()),
                (403, "AccessDeniedException"),
                "case {case:?}: expected 403 AccessDeniedException, got {} {:?} / {}",
                reply.status,
                reply.error_type,
                reply.text().chars().take(200).collect::<String>()
            );
            let message = reply.message();
            assert!(
                message.len() <= LARGEST_MESSAGE_BYTES,
                "case {case:?}: expected a refusal within the {LARGEST_MESSAGE_BYTES} byte \
                 maximum the carrying member declares, got {} bytes: {:?}",
                message.len(),
                message.chars().take(200).collect::<String>()
            );

            let unheld = unheld_sentence(&principal, &action, &resource, row);
            if unheld.len() <= LARGEST_MESSAGE_BYTES {
                assert_eq!(
                    message, unheld,
                    "case {case:?}: expected IAM's wording unchanged, got {:?}",
                    message.chars().take(200).collect::<String>()
                );
                continue;
            }

            // Past the maximum, the wording around the values is still IAM's: who was
            // refused, which action, which resource and which of IAM's rules refused it.
            let fixed = format!(" is not authorized to perform: {action} on resource: ");
            assert!(
                message.starts_with("User: ")
                    && message.contains(fixed.as_str())
                    && message.ends_with(&reason(row.explicit_deny, &action)),
                "case {case:?}: expected IAM's identity-based denial wording around the held \
                 values, got {:?}",
                message.chars().take(200).collect::<String>()
            );
            // The principal is what the IAM model sizes and this one does not, so it is the
            // value the sentence holds; the resource fits what is left of the member.
            assert_states(&case, &message, &principal, false);
            assert_states(&case, &message, &resource, true);
        }
    }
}
