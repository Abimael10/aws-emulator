// SPDX-License-Identifier: CC0-1.0

//! WHAT a queue-role credential IS: a session of the IAM role the queue carries.
//!
//! A queue is created with "the IAM role ARN that workers will use while running jobs for
//! this queue" (`smithy_query(query="shape", shape="CreateQueueRequest")`, member
//! `roleArn`), so a credential this service vends for that queue is a session of THAT role:
//! what the holder may then do is what the role's own policies allow — "the temporary
//! security credentials … have the permissions of the role" (`sts:AssumeRole`).
//!
//! The session is recorded through the token service's own role-session mint
//! ([`sts_store::mint_role_session`]), because the service that MINTS a session is not the
//! service that serves the calls made with it: a later request signed with the credential is
//! resolved as a session of the queue's role wherever it lands, and
//! `sts:GetCallerIdentity` under it answers the assumed-role ARN of that role. Minting a bare
//! credential instead would hand out one that speaks for nobody, which every service serves
//! as the ambient account identity — the opposite of a role-scoped credential.
//!
//! Neither the access key id nor either secret is chosen here: all three come from the
//! emulator's credential store, which is the only thing that creates credential material,
//! and the sessions a repeated call records are bounded by the registry that holds them (the
//! mint makes room in it before any credential material is created).
//!
//! LOCK ORDER: this reaches the token service's and IAM's stores, so it is called with THIS
//! service's own store guard given up.

use std::time::SystemTime;

use ::emulator_server::CredentialStore;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;

/// How long a queue-role session this service vends lasts.
///
/// AWS documents no duration for the credentials these operations return, so this is the
/// emulator's chosen span: the default session duration STS documents for a role session
/// ("by default, the value is set to 3600 seconds", `AssumeRoleRequest$DurationSeconds`),
/// which is what a caller that asks for no duration gets. Nothing sleeps for it — the
/// instant is recorded on the session and reported as the credential's `expiration`.
pub const QUEUE_ROLE_SESSION_SECONDS: i32 = 3600;

/// Vend a session of `role_arn` named `session_name`, as the credentials a response carries.
///
/// `None` when no session of that role can be recorded at all — a value that names no IAM
/// role, an engine carrying no token-service store, or a bounded session registry with no
/// room left. The caller then vends NOTHING and refuses the request: a credential this
/// emulator cannot scope to the role is a credential that would act as the whole account.
pub fn vend_role_session<Eng: Send + Sync + 'static>(
    engine: &Eng,
    store: &CredentialStore,
    now: SystemTime,
    role_arn: &str,
    session_name: &str,
) -> Option<crate::ops::types::AwsCredentials> {
    let role = sts_store::parse_role_arn(role_arn)?;
    // The role's own id when the account's IAM holds the role, and the id STS derives from
    // the ARN otherwise: a queue may name a role provisioned out of band.
    let role_id = held_role_id(engine, role_arn);
    let session_arn = sts_store::sts_arn(
        &role.account,
        &format!("assumed-role/{}/{session_name}", role.name),
    );
    let identity = sts_store::Identity {
        user_id: format!("{role_id}:{session_name}"),
        account: role.account.clone(),
        arn: session_arn,
    };
    let registry = sts_store::service_state(engine)?;
    let minted = sts_store::mint_role_session(
        engine,
        store,
        &registry,
        now,
        sts_store::NewSession {
            // The credential stands for the queue's role, so the role's own policies are
            // what is in force on it; this service passes no session policy of its own.
            role_arn: Some(role_arn.to_string()),
            ..sts_store::NewSession::new(
                identity,
                sts_store::SessionKind::AssumedRole,
                QUEUE_ROLE_SESSION_SECONDS,
            )
        },
    )
    .ok()?;
    Some(crate::ops::types::AwsCredentials {
        access_key_id: minted.credentials.access_key_id,
        secret_access_key: minted.credentials.secret_access_key,
        session_token: minted.credentials.session_token,
        expiration: super::timestamp(minted.session.expires_at),
    })
}

/// The id the account's IAM holds for the role `role_arn`, and the id STS derives from the
/// ARN when that store holds no such role.
fn held_role_id<Eng: Send + Sync + 'static>(engine: &Eng, role_arn: &str) -> String {
    let held = sts_store::iam_state(engine).and_then(|iam| {
        let store = iam.lock();
        store
            .roles
            .values()
            .find(|role| role.arn == role_arn)
            .map(|role| role.role_id.clone())
    });
    held.unwrap_or_else(|| sts_store::role_id(role_arn))
}

/// The 409 this service answers a worker that asks for a queue's role while it is working on
/// nothing of that queue.
///
/// A worker holds the queue role only while the service has it working on that queue: the
/// credentials are for the jobs it runs for the queue, so a worker the scheduler holds no
/// open session of that queue for is asking for a role it has no work to use it under. The
/// worker is the resource in conflict — it is the resource whose own state refuses the call —
/// and the reason is the plain conflict rather than a status value: `WorkerStatus` is not what
/// decides this, the sessions the worker holds are.
pub fn worker_not_working_on_queue(
    worker_id: &str,
    queue_id: &str,
) -> crate::ops::types::ConflictException {
    crate::ops::types::ConflictException {
        // The sentence this service words the refusal with: the worker and the queue it was
        // asked about, and nothing else. The trailing period is part of it.
        message: format!("Worker {worker_id} is not working on queue {queue_id}."),
        // From `smithy_query(query="shape", shape="ConflictExceptionReason")`:
        // CONFLICT_EXCEPTION, CONCURRENT_MODIFICATION, RESOURCE_ALREADY_EXISTS,
        // RESOURCE_IN_USE, STATUS_CONFLICT.
        reason: "CONFLICT_EXCEPTION".to_string(),
        resource_id: worker_id.to_string(),
        resource_type: super::RESOURCE_TYPE_WORKER.to_string(),
        context: None,
    }
}
