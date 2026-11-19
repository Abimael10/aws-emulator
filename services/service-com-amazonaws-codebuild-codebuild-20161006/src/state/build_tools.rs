// SPDX-License-Identifier: CC0-1.0

//! The TOOLS a CodeBuild build environment provides beyond its shell's own utilities: the
//! AWS CLI and the Docker CLI, performing the calls a buildspec's commands really make, and
//! the Docker daemon they are served by.
//!
//! # The rule this module implements
//!
//! A build's phase commands are run by the shell of the build environment
//! ([`super::build_shell`]), and what a command DOES is what the program it names does: the
//! CodeBuild managed images carry the AWS CLI and the Docker CLI, so a buildspec that logs
//! in to a container registry and pushes an image to it has that login made and that image
//! pushed — and the build's status follows from whether those calls succeeded. A build whose
//! commands all succeed ends `SUCCEEDED`; a command whose call the owning service refused
//! exits non-zero, which fails the phase it is in.
//!
//! So nothing here decides an outcome from the text of a command: each program performs the
//! API calls its invocation names, against the services of THIS emulator, and reports what
//! they answered.
//!
//! * [`aws_cli`] performs `aws ecr get-login-password`, which is Amazon ECR's
//!   `GetAuthorizationToken` and whose answer is the registry password that service issued;
//! * [`docker_cli`] performs `docker login`, `docker build`, `docker tag` and `docker push`:
//!   the login presents the password to the registry it names, the build composes a real
//!   image out of the files in the build's own working tree ([`container_image`]), and the
//!   push uploads that image's blobs and manifest to the registry through the layer-upload
//!   and `PutImage` calls a registry client makes — so the repository really holds the image
//!   once the command has run, and a later `DescribeImages` of it reports exactly what was
//!   pushed;
//! * [`docker_daemon`] answers the command a container build starts the daemon those Docker
//!   commands are served by with, which a build environment runs only in privileged mode.
//!
//! Every other program name, and every form of these programs that is not performed, is
//! answered as a command the environment has no program for, which is the closed failure the
//! shell reports for one.
//!
//! # As whom the calls are made
//!
//! A build RUNS AS its project's service role ("the ARN of the IAM role that enables
//! CodeBuild to interact with dependent Amazon Web Services services on behalf of the Amazon
//! Web Services account"), so every call a build's command makes is authorized against the
//! permissions in force on THAT role, through the workspace's one reading of what a role may
//! do (`::service_common::iam_role`): a role whose policies do not allow the action is a
//! command that is refused, with the sentence AWS refuses it with
//! (`::service_common::iam_authorization`), and nothing is called. The principal a refusal
//! names is the session CodeBuild assumes the role in for the build
//! ([`super::role_session_arn`]).
//!
//! The call itself is the AWS SDK driven over the framework's in-process callback, exactly as
//! every other call between two emulated services in this crate: the credential the framework
//! hands this emulator's handlers is what signs it, and it is held here — never in the
//! session's variables, its tree, or anything a program writes.
//!
//! # Where the calls are driven
//!
//! A shell session is synchronous, and these calls are not, so the build's commands are
//! performed on a thread with no async context of its own ([`super::build_run`] hands the
//! whole command run to the runtime's blocking pool) and each call is driven by blocking that
//! thread on the runtime handle. That is the same seam the one other `aws` CLI of this
//! workspace is performed through
//! (`service-com-amazonaws-emr-elasticmapreduce`'s step command transfers). With no runtime
//! to drive a call on, the environment provides these programs AT ALL: a build then reports
//! them as commands it could not run rather than as commands that did nothing.
//!
//! # Bounds
//!
//! Every bound is this emulator's and none is reachable from a command: the whole wall clock
//! one build's tool calls may spend ([`TOOL_CALL_BUDGET`], shared by every call the build's
//! commands make, so a build cannot hold the operation that performs it for longer whatever
//! its buildspec says), how many bytes one pushed blob carries
//! ([`container_image::MAX_IMAGE_BYTES`]) and how many images one build composes
//! ([`MAX_LOCAL_IMAGES`]). The session's own bounds — the command budget, the line size, the
//! tree size — are `::service_common::posix_shell_session`'s.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;
use ::service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles, ProgramOutcome};
use ::smithy_server_runtime::EmbeddedEmulator;

/// The AWS CLI, as a build's commands invoke it.
mod aws_cli;
/// The image a `docker build` composes out of the build's working tree.
pub mod container_image;
/// The Docker CLI, as a build's commands invoke it.
mod docker_cli;
/// The Docker daemon the managed image carries, as a build's commands start it.
mod docker_daemon;
/// What a Dockerfile declares about the image built from it.
pub mod dockerfile;

/// The whole wall clock ONE build's tool calls may spend, shared by every call the build's
/// commands make.
///
/// CHOSEN, not an AWS duration: the operation that reports the build waits for its commands,
/// so this is what those commands' calls may hold it for. A call with nothing left of the
/// budget is not made at all, and the command carrying it reports that rather than being left
/// outstanding.
pub const TOOL_CALL_BUDGET: Duration = Duration::from_secs(10);

/// How many images ONE build's commands compose.
///
/// CHOSEN, not an AWS limit: each one is held in this process for the length of the build, so
/// how many a buildspec may ask for is bounded here. A `docker build` past it fails rather
/// than growing the set.
pub const MAX_LOCAL_IMAGES: usize = 8;

/// The status a program answers when its work was done ("0 -- Command was successful", AWS
/// CLI return codes; `docker` exits 0 for a command that succeeded).
pub const PROGRAM_SUCCEEDED: i32 = 0;

/// The status the Docker CLI answers for a command that failed.
pub const DOCKER_FAILED: i32 = 1;

/// The status the AWS CLI answers when the service it called answered an error: "254 -- The
/// command was successfully parsed … but the service returned an error" (AWS CLI v2 return
/// codes), which is the CLI the CodeBuild managed images carry.
pub const AWS_CLI_SERVICE_ERROR: i32 = 254;

/// What one of these programs answered, as [`ProgramOutcome`] carries it: the status the
/// command exits with and the lines it wrote on standard output.
///
/// A failed command writes its diagnostic on standard ERROR, which this session has no
/// channel for, so it writes NOTHING: a command in a pipeline must not hand its error text to
/// the stage after it as if it were output. The diagnostic is logged instead
/// ([`BuildTools::refused`]).
fn answered(status: i32, output: Vec<String>) -> ProgramOutcome {
    ProgramOutcome { status, output }
}

/// The programs ONE build's environment provides: the runtimes its buildspec installed, and
/// the tools the managed image carries.
///
/// The TOOLS are asked first, so a build that installed a runtime whose program shares a name
/// with one of them is still answered by the tool the image carries.
#[derive(Debug)]
pub struct BuildPrograms {
    /// The programs the runtimes this build installed carry.
    installed: super::InstalledRuntimes,
    /// The tools the build environment carries, and `None` for an environment with no runtime
    /// to drive their calls on.
    tools: Option<BuildTools>,
}

impl BuildPrograms {
    /// The environment of a build that installed `installed` and was given `tools`.
    #[must_use]
    pub fn new(installed: super::InstalledRuntimes, tools: Option<BuildTools>) -> Self {
        Self { installed, tools }
    }
}

impl EnvironmentPrograms for BuildPrograms {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        self.run_with_input(name, arguments, None, files)
    }

    fn run_with_input(
        &self,
        name: &str,
        arguments: &[String],
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        if let Some(tools) = self.tools.as_ref() {
            if let Some(outcome) = tools.performed(name, arguments, standard_input, files) {
                return Some(outcome);
            }
        }
        self.installed.run(name, arguments, files)
    }
}

/// What ONE build's commands have obtained and composed so far.
#[derive(Debug, Default)]
struct ToolState {
    /// The registry passwords Amazon ECR ISSUED to this build, so a login can only present a
    /// credential the registry really handed over.
    issued_passwords: BTreeSet<String>,
    /// The registries this build is logged in to, by the host a push addresses them at.
    logins: BTreeSet<String>,
    /// The images this build has composed, by the reference they were tagged with.
    images: BTreeMap<String, container_image::LocalImage>,
}

/// How many lines the tools of ONE build keep of what they wrote on standard error.
///
/// CHOSEN, not an AWS limit: the lines are kept in this process until the build's log is
/// delivered, so how many one build may keep is bounded here. Past it a line is not kept.
pub const MAX_DIAGNOSTIC_LINES: usize = 1_024;

/// The lines a build's TOOLS wrote on standard error — what a command reported it could not
/// do — shared with the run that delivers the build's log.
///
/// A program writes a diagnostic on standard ERROR, which the session has no channel for (see
/// [`answered`]), and a build's log carries both of its streams: the lines are kept here so
/// the run that delivers the log delivers them too, rather than being written on standard
/// output, where the next stage of a pipeline would read them as input.
#[derive(Debug, Clone, Default)]
pub struct ToolDiagnostics(::std::sync::Arc<Mutex<Vec<String>>>);

impl ToolDiagnostics {
    /// Keep `line`, up to [`MAX_DIAGNOSTIC_LINES`].
    fn record(&self, line: &str) {
        let mut held = self
            .0
            .lock()
            .unwrap_or_else(::std::sync::PoisonError::into_inner);
        if held.len() < MAX_DIAGNOSTIC_LINES {
            held.push(line.to_string());
        }
    }

    /// The lines kept so far, in the order they were written.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(::std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// The tools ONE build's environment carries: the calls its commands make, as the role the
/// build runs as, inside the budget one build's calls have.
#[derive(Debug)]
pub struct BuildTools {
    /// The in-process callback every call is dispatched through.
    callback: EmbeddedEmulator,
    /// The async runtime those calls are driven on. The build's commands are performed on a
    /// thread that is not one of this runtime's workers, so a call is made by blocking that
    /// thread on this handle rather than by making the shell asynchronous.
    runtime: ::tokio::runtime::Handle,
    /// The Region the build runs in, which is the signing scope of its calls and the Region
    /// of the registry a command that names none addresses.
    region: String,
    /// The service role the build RUNS AS, whose permissions every call is authorized
    /// against — `None` for a build whose record names none, which is a build nothing is
    /// known about the permissions of.
    role_arn: Option<String>,
    /// Whether the build environment was started in PRIVILEGED mode, which is what decides
    /// whether this build may run the container daemon at all
    /// ([`docker_daemon`]) — read off the build's own record.
    privileged: bool,
    /// The principal a refusal names: the session CodeBuild assumes the service role in for
    /// this build.
    principal: String,
    /// The instant this build's commands have spent [`TOOL_CALL_BUDGET`].
    deadline: Instant,
    /// The lines these tools wrote on standard error, which the build's log carries.
    diagnostics: ToolDiagnostics,
    /// What this build's commands have obtained and composed so far.
    state: Mutex<ToolState>,
}

impl BuildTools {
    /// The tools the build `build_id`'s environment carries, running as `role_arn`, in an
    /// environment `privileged` says may run the container daemon, and keeping what they
    /// write on standard error in `diagnostics` — `None` where there is no
    /// async runtime to drive a call on, in which case the environment provides neither tool
    /// and the build reports their commands as ones it could not run.
    #[must_use]
    pub fn new(
        callback: &EmbeddedEmulator,
        region: &str,
        role_arn: Option<&str>,
        privileged: bool,
        build_id: &str,
        diagnostics: &ToolDiagnostics,
    ) -> Option<Self> {
        let principal = role_arn
            .and_then(|role_arn| super::role_session_arn(role_arn, build_id))
            .or_else(|| role_arn.map(str::to_string))
            .unwrap_or_else(|| build_id.to_string());
        Some(Self {
            callback: callback.clone(),
            runtime: ::tokio::runtime::Handle::try_current().ok()?,
            region: region.to_string(),
            role_arn: role_arn.map(str::to_string),
            privileged,
            principal,
            deadline: Instant::now() + TOOL_CALL_BUDGET,
            diagnostics: diagnostics.clone(),
            state: Mutex::new(ToolState::default()),
        })
    }

    /// What the tool `name` answers for `arguments`, or `None` when this environment carries
    /// no tool of that name or performs nothing of that invocation.
    fn performed(
        &self,
        name: &str,
        arguments: &[String],
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        match name {
            aws_cli::AWS_CLI_PROGRAM => aws_cli::performed(self, arguments),
            docker_cli::DOCKER_PROGRAM => {
                docker_cli::performed(self, arguments, standard_input, files)
            }
            docker_daemon::DOCKER_DAEMON_PROGRAM => docker_daemon::performed(self, arguments),
            _ => None,
        }
    }

    /// The state this build's commands share, recovering a guard a panicking command left
    /// poisoned: one command must not stop the rest of the build's commands from running.
    fn state(&self) -> ::std::sync::MutexGuard<'_, ToolState> {
        self.state
            .lock()
            .unwrap_or_else(::std::sync::PoisonError::into_inner)
    }

    /// Report why a command was refused, and answer the status it exits with.
    ///
    /// A failed command writes its diagnostic on standard ERROR: the line is kept for the
    /// build's LOG ([`ToolDiagnostics`]) and left off standard output, where the next stage
    /// of a pipeline would read it as input. It is logged for an operator too.
    fn refused(&self, program: &str, detail: &str, status: i32) -> ProgramOutcome {
        ::tracing::debug!(
            program = %program,
            principal = %self.principal,
            "a build's command did not succeed"
        );
        self.diagnostics.record(detail);
        answered(status, Vec::new())
    }

    /// Make ONE call, inside what this build's commands have left of [`TOOL_CALL_BUDGET`].
    ///
    /// The thread this runs on is the one the build's commands are performed on rather than a
    /// worker of the runtime, so the call is driven by blocking it on the runtime handle.
    fn made<T, F: ::std::future::Future<Output = Result<T, String>>>(
        &self,
        call: impl FnOnce() -> F,
    ) -> Result<T, String> {
        if Instant::now() >= self.deadline {
            return Err(format!(
                "the calls of this build's commands have spent the {} seconds they may take",
                TOOL_CALL_BUDGET.as_secs()
            ));
        }
        self.runtime.block_on(call())
    }

    /// `Some(message)` when the permissions in force on the role this build runs as do NOT
    /// allow `action` on `resource` — the sentence AWS refuses the call with — and `None` when
    /// they do or when nothing is known about the role.
    ///
    /// The decision itself is [`role_withholds`], which every call made FOR a build goes
    /// through; what this adds is the budget the question is asked inside and the principal the
    /// refusal names.
    fn withheld(&self, action: &str, resource: &str) -> Option<String> {
        let role_arn = self.role_arn.clone()?;
        let withheld = self
            .made(|| async {
                Ok(role_withholds(
                    &self.callback,
                    &self.region,
                    &role_arn,
                    action,
                    resource,
                )
                .await)
            })
            .unwrap_or(None)?;
        Some(
            ::service_common::iam_authorization::not_authorized_by_identity_policy(
                &self.principal,
                action,
                resource,
                &withheld.reason(action),
            ),
        )
    }

    /// An Amazon ECR client over the in-process callback into this same emulator, for the
    /// registry of `region`.
    ///
    /// The configuration is the one every callback client in this crate uses: nothing to
    /// retry, time out or watch for a stalled stream, because the transport is in process, and
    /// the credentials the callback hands over are the emulator's own — WHETHER the build's
    /// role may make the call is decided ahead of it ([`Self::withheld`]).
    fn ecr(&self, region: &str) -> ::aws_sdk_ecr::Client {
        let config = ::aws_sdk_ecr::Config::builder()
            .behavior_version(::aws_sdk_ecr::config::BehaviorVersion::latest())
            .region(::aws_sdk_ecr::config::Region::new(region.to_string()))
            .endpoint_url(format!("https://ecr.{region}.amazonaws.com"))
            .credentials_provider(self.callback.aws_credentials_provider())
            .http_client(self.callback.http_client())
            .retry_config(::aws_sdk_ecr::config::retry::RetryConfig::disabled())
            .timeout_config(::aws_sdk_ecr::config::timeout::TimeoutConfig::disabled())
            .identity_cache(::aws_sdk_ecr::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                ::aws_sdk_ecr::config::StalledStreamProtectionConfig::disabled(),
            )
            .build();
        ::aws_sdk_ecr::Client::from_conf(config)
    }

    /// The ARN of the repository `name` of the registry of `region`, which is the resource an
    /// authorization decision about a push to it is taken on ("Resource types defined by
    /// Amazon Elastic Container Registry", `repository`).
    fn repository_arn(region: &str, name: &str) -> String {
        ::emulator_server::emulator_arn(ECR_ARN_SERVICE, region, &format!("repository/{name}"))
    }
}

/// The ARN namespace, and the IAM action prefix, of Amazon ECR.
const ECR_ARN_SERVICE: &str = "ecr";

/// WHY the permissions in force on a role withheld an action from a build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Withheld {
    /// A statement of the role's own documents DENIES the action.
    Denied,
    /// The role states identity-based policies and none of them allows the action, which is
    /// IAM's default deny.
    NotAllowed,
}

impl Withheld {
    /// Which of IAM's rules withheld the grant, as the reason clause the refusal carries
    /// (`::service_common::iam_authorization`).
    #[must_use]
    pub fn reason(self, action: &str) -> String {
        match self {
            Self::Denied => {
                ::service_common::iam_authorization::IDENTITY_POLICY_EXPLICIT_DENY.to_string()
            }
            Self::NotAllowed => {
                ::service_common::iam_authorization::no_identity_policy_allows(action)
            }
        }
    }
}

/// Whether the permissions in force on the role `role_arn` do NOT allow `action` on
/// `resource` — the ONE question every call made FOR a build is authorized by, whether the
/// build's own command makes it ([`BuildTools::withheld`]) or the build's log delivery does
/// ([`super::build_run`]).
///
/// Which answer counts is `::service_common::iam_role`'s own rule: a role that STATES
/// identity-based policies is the authority on what it may do, so it has to be ALLOWED the
/// action; a role IAM holds no policy for says nothing about the action — a build's service
/// role is routinely provisioned out of band — so only a statement that DENIES it holds the
/// call back. `None` — withhold nothing — for everything that is not an answer: no IAM to
/// ask, a value naming no role of this account, or a failed hop.
pub async fn role_withholds(
    callback: &EmbeddedEmulator,
    region: &str,
    role_arn: &str,
    action: &str,
    resource: &str,
) -> Option<Withheld> {
    if ::service_common::iam_role::role_states_permissions_through(callback, region, role_arn).await
    {
        ::service_common::iam_role::role_withholds_action_through(
            callback, region, role_arn, action, resource,
        )
        .await
        .then_some(Withheld::NotAllowed)
    } else {
        ::service_common::iam_role::role_denies_action_through(
            callback, region, role_arn, action, resource,
        )
        .await
        .then_some(Withheld::Denied)
    }
}

/// What the owning service answered, as a command's diagnostic states it: the error class and
/// the sentence that service published, which is what a CLI reports for a refused call.
fn service_answered<E: ProvideErrorMetadata>(operation: &str, error: &E) -> String {
    format!(
        "An error occurred ({}) when calling the {operation} operation: {}",
        error.code().unwrap_or("Unknown"),
        error.message().unwrap_or("no message")
    )
}
