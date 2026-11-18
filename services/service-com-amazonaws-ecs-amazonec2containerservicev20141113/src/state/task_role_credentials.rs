// SPDX-License-Identifier: CC0-1.0

//! The CREDENTIAL a task's containers present on the AWS calls their programs make: a SESSION
//! of the task's own task role.
//!
//! # The rule this module implements
//!
//! `TaskDefinition$taskRoleArn` is "the short name or full Amazon Resource Name (ARN) of the
//! Identity and Access Management role that grants containers in the task permission to call
//! Amazon Web Services APIs on your behalf", and a container calls them by signing with
//! temporary credentials FOR that role — the credentials the agent obtains for the task and
//! the AWS SDKs and the CLI inside the container pick up. So a call a container's program
//! makes is a call made BY THAT ROLE, and the service serving it judges it against the
//! policies in force on the role and against its own resource policy, exactly as it judges any
//! other signed request. That is the whole point of separating the task role from the task
//! EXECUTION role: a container whose task role lacks an action is refused it by the service
//! that owns the resource, and attaching the grant to the role lifts the refusal on the next
//! call.
//!
//! This module is where that credential comes from. The SESSION itself is not minted here: the
//! workspace has one mint of a role session for a service that runs other work AS a role, and
//! this calls it ([`execution_role_identity`], which EMR's step commands and Glue's job scripts
//! reach cross-crate the same way). What that mint gives this service:
//!
//! * the role is resolved against the IAM store of the SAME shared engine, where the account's
//!   roles live, so the session speaks for the very record `iam:CreateRole` wrote;
//! * the session is minted by the TOKEN SERVICE's own mint, so every service that authorizes a
//!   request resolves it the same way and the credential is one the emulator's own request
//!   verifier accepts. Nothing here constructs an access key id or a secret;
//! * one session is held per `(role, name)` and re-minted once the clock has brought it near its
//!   expiry, with the one it replaces given up in both stores;
//! * it is named for what it was OPENED FOR, which for a task is the TASK — so a refusal says
//!   which task was refused, as `arn:<partition>:sts::<account>:assumed-role/<role name>/<task
//!   id>`.
//!
//! Where nothing can be SHOWN about the role — this account's IAM holds no record of it (a
//! task role is routinely provisioned outside the client that runs the task), or there is no
//! token service to mint a session with — no session is minted and the container's calls are
//! signed with the credential the framework hands this emulator's handlers, which is what
//! every other in-process call between two emulated services presents: refusing on a role this
//! emulator knows nothing about would deny calls AWS permits. A task that resolves NO task
//! role holds no credential at all ([`TaskRoleCredential::Absent`]), and its containers'
//! calls are not signed with anything.
//!
//! The credential never reaches the container's program: it is held beside the run, put on the
//! calls that run makes ([`super::container_aws_cli`]), and in no response, log, environment
//! variable or file of the task.
//!
//! # When it is vended
//!
//! The agent has the task's credentials BEFORE any container of it runs, exactly as it has the
//! values it fetched for them ([`super::container_secrets`]), so this is one step of a launch:
//! [`settle_task_role_credentials`] vends for every task on its way up or running that has not
//! been vended for, and a container of a task that resolves a task role is not started until
//! it has been ([`super::container_runtime`]) — a container signed with nothing would be a
//! container whose role was never handed to it. Minting needs the emulator's credential store,
//! which only a request's own context carries, so the step is taken by the call that places or
//! reads the task ([`super::task_events::drive_task_clock`]).
//!
//! LOCK ORDER: this service's store is locked to collect the launches and released before the
//! mint takes the IAM store, the token service's registry and the credential store — the same
//! one-way order every other launch step takes ([`super::launch_role_trust`]).

use ::std::time::SystemTime;

use ::emulator_server::CredentialStore;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::execution_role_session::execution_role_identity;

use super::*;

/// One task waiting for the credentials of its task role.
#[derive(Debug, Clone)]
pub struct TaskRoleVend {
    /// The task the credential is vended for.
    pub task_arn: String,
    /// Its id, which is the name the session is opened under.
    pub task_id: String,
    /// The task role the launch resolved ([`resolved_task_role_arn`]).
    pub role_arn: String,
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Every task that resolves a task role and has not been vended its credentials yet: one
    /// whose `desiredStatus` is `RUNNING` that carries no launch failure, with the role its
    /// launch resolved.
    ///
    /// A task that is already running is listed too, not only one that is still pending: the
    /// containers of a task placed by a scheduler reach `RUNNING` without a call of this
    /// service's own being there to take this step, and they wait for it
    /// ([`super::container_runtime`]).
    pub(super) fn launches_awaiting_task_role_credentials(&self) -> Vec<TaskRoleVend> {
        self.tasks
            .values()
            .filter(|task| {
                task.task_role_credentials.is_none()
                    && task.launch_failure.is_none()
                    && task.desired_status == STATUS_RUNNING
                    && task.last_status != STATUS_STOPPED
            })
            .filter_map(|task| {
                let role_arn = self.launched_task_role_arn(task)?;
                Some(TaskRoleVend {
                    task_arn: task.task_arn.clone(),
                    task_id: task.task_id.clone(),
                    role_arn,
                })
            })
            .collect()
    }

    /// Record what one task's containers present, leaving a task that is already holding a
    /// credential with the one it was vended: the credentials are the task's for its whole
    /// life, so nothing re-vends them under it.
    pub(super) fn record_task_role_credential(
        &mut self,
        task_arn: &str,
        credential: TaskRoleCredential,
    ) {
        if let Some(task) = self.tasks.get_mut(task_arn) {
            if task.task_role_credentials.is_none() {
                task.task_role_credentials = Some(credential);
            }
        }
    }
}

/// Vend the task-role credentials of every task waiting for them.
///
/// Call it with NO guard of this store held, from a call that has just placed or read a task:
/// it takes this store's lock to collect the launches, releases it, and then takes the IAM
/// store's, the token service's registry and the credential store (see the module's LOCK ORDER
/// note). Every task it was made for is left holding a decision, so each is vended once.
pub fn settle_task_role_credentials<Eng: Send + Sync + 'static>(
    store: &CredentialStore,
    engine: &Eng,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let vends = state.lock().launches_awaiting_task_role_credentials();
    for vend in vends {
        let credential = vended_credential(engine, store, &vend, SystemTime::now());
        state
            .lock()
            .record_task_role_credential(&vend.task_arn, credential);
    }
}

/// The credential one task's containers present: a session of its task role, or the emulator's
/// own credential where nothing can be shown about that role (see the module docs).
///
/// The session itself comes from this workspace's ONE mint of a role session for a service that
/// runs other work AS a role ([`execution_role_identity`], which EMR's step commands and Glue's
/// job scripts reach the same way): the role is resolved against this engine's IAM store, the
/// session is minted by the token service's own mint under the name this service opens it
/// (`session_name`, the task's id), one is held per `(role, name)` and re-minted as the clock
/// brings it near its expiry, and `None` is "nothing could be shown about the role", which is
/// exactly [`TaskRoleCredential::Emulator`].
fn vended_credential<Eng: Send + Sync + 'static>(
    engine: &Eng,
    store: &CredentialStore,
    vend: &TaskRoleVend,
    now: SystemTime,
) -> TaskRoleCredential {
    execution_role_identity(engine, store, &vend.role_arn, &vend.task_id, now).map_or(
        TaskRoleCredential::Emulator,
        |identity| TaskRoleCredential::Session {
            access_key_id: identity.access_key_id,
            secret_access_key: identity.secret_access_key,
            session_token: identity.session_token,
        },
    )
}
