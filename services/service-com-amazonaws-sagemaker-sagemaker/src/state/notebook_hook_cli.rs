// SPDX-License-Identifier: CC0-1.0

//! The AWS CLI as a notebook instance's LIFECYCLE HOOK gets it: the `aws s3` transfers and
//! the `aws s3api` object calls one hook's script asks for, really performed against this
//! emulator's own object store as the instance's execution role.
//!
//! # The rule this module implements
//!
//! A lifecycle configuration's hook is a shell script the PLATFORM runs for the notebook
//! instance that names the configuration ("a base64-encoded string that contains a shell
//! script", `NotebookInstanceLifecycleHook$Content`), and such a script is written to
//! BOOTSTRAP the instance: it stages data, writes a readiness marker and reads back what it
//! wrote, through the AWS CLI. So a hook whose script carries a file into a bucket has that
//! file carried: the object exists once the hook has run, and the status the command exits
//! with is the status of the call. WHO makes it is the instance's execution role — "when you
//! send any requests to Amazon Web Services resources from the notebook instance, SageMaker
//! AI assumes this role to perform tasks on your behalf" (`CreateNotebookInstance$RoleArn`) —
//! so a hook of an instance whose role is not granted the action makes no object at all and
//! the command reports that refusal.
//!
//! The hook runs while the instance is still coming up, so its effects are at the destination
//! by the time the instance reports `InService` — which is when a client that waited starts
//! reading them.
//!
//! # What one command line asks for, and what carries it
//!
//! What ONE command line asks of that program is the workspace's shared reading of it
//! ([`::service_common::aws_s3_cli`]) — the same one every service that runs a
//! caller-supplied command line uses, so two of them cannot carry a different file or answer
//! a different status. Both of the CLI's object command groups are provided: the
//! file-oriented `aws s3 cp|mv|rm` and the low-level `aws s3api` object operations. A command
//! line naming any other program, any other command group, any other command of these
//! groups, or any FORM of them that reading performs nothing of, is a command the session
//! reports it could not run.
//!
//! The call is made with the AWS SDK for Amazon S3 over the framework's in-process callback,
//! through this crate's one construction of that client ([`super::s3_access::s3_client_through`])
//! — the same transport a client's own S3 request takes, so the object a hook wrote is the
//! object a later `GetObject` of that bucket reads. WHETHER it is made is the one question
//! this crate asks about every Amazon S3 call made on behalf of an execution role
//! ([`super::s3_access::object_action_is_unauthorized_through`]): the bucket's policy refuses
//! what it EXPLICITLY DENIES, and otherwise the grant in force on the role decides, as IAM
//! answers it. Only what was ANSWERED refuses, so a role this account's IAM holds nothing
//! under, and an emulator with no IAM to ask, withhold nothing.
//!
//! The local side of a transfer is the session's own TREE
//! ([`::service_common::posix_shell_session::ProgramFiles`]): the file the commands around it
//! wrote with a redirection is the file it uploads, and the file it downloads is the one they
//! read back. A path is a key of that tree; no host file is opened, created or removed, and
//! no process is started, no name resolved and no socket opened.
//!
//! # What a hook's script can and cannot reach
//!
//! The script is caller-supplied text, so what performing it may reach is stated here rather
//! than left to be read off the code. It is performed by the workspace's ONE bounded,
//! session-local shell session ([`::service_common::posix_shell_session`], whose own docs state
//! why that session and not a shell engine): the session STARTS NO PROCESS, resolves no name,
//! opens no socket and opens, creates or removes no path of the host — it performs a fixed
//! catalog of utilities over its own in-memory tree, and every command and every command FORM
//! outside that catalog fails closed as one it could not run. This module is the ONE program
//! that reaches anything outside the session, and the only thing it reaches is THIS emulator's
//! own Amazon S3, over the framework's in-process callback: there is no host, no shell and no
//! argument vector anywhere on this path, and nothing of the machine the emulator runs on is
//! readable, writable or startable from a script. The CREDENTIAL the calls present is held here
//! and never reaches the program: it is not in the session's variables, in its tree, or in
//! anything a script writes.
//!
//! # Bounds
//!
//! Both bounds are this emulator's and neither is reachable from the script: how many bytes
//! one transfer carries ([`MAX_TRANSFERRED_BYTES`], enforced by the RANGED read so nothing
//! larger is buffered) and the whole wall clock ONE hook's calls may spend
//! ([`HOOK_CALL_BUDGET`], shared by every call that hook makes, so a script cannot hold the
//! operation that performs it for longer whatever it writes). A transfer carries TEXT only,
//! because the session's tree holds text. The session's own bounds — how many commands one
//! script performs at all, its line size, what it may expand, how large its tree may grow and
//! how much output it keeps — are [`super::notebook_lifecycle_hook`]'s and that module's.

use ::std::time::{Duration, Instant};

use ::service_common::aws_s3_cli::{
    self as cli, ApiOperation, Location, ObjectAddress, Requested, API_COMMAND_ERROR,
    API_SERVICE_ERROR, TRANSFER_FAILED, TRANSFER_SUCCEEDED,
};
use ::service_common::posix_shell_session::{
    EnvironmentPrograms, ProgramFiles, ProgramInvocation, ProgramOutcome, ProgramStreams,
};

use super::*;

/// How many bytes ONE transfer of a hook's command carries.
///
/// THIS NUMBER IS THE EMULATOR'S, not an AWS limit: the bytes are a client's object or a
/// client's file, so what one transfer takes into this process is bounded here rather than by
/// the object's own size. A transfer carries TEXT only, for the same reason the session's tree
/// holds text; an object that is not text is a download that did not happen.
pub const MAX_TRANSFERRED_BYTES: usize = 256 * 1024;

/// The whole wall clock ONE hook's calls may spend, shared by every call its script makes.
///
/// CHOSEN, not an AWS duration: the operation that creates the notebook instance performs the
/// hook, so this is what a script's calls may hold it for; a call with nothing left of it is
/// not made and reports that bound rather than being left outstanding.
pub const HOOK_CALL_BUDGET: Duration = Duration::from_secs(5);

/// The AWS CLI as the environment of ONE hook's script provides it: the object calls of that
/// script, made against this emulator's object store as the instance's execution role.
///
/// Built per hook ([`notebook_hook_cli`]), which is what gives each hook its own
/// [`HOOK_CALL_BUDGET`].
#[derive(Debug, Clone)]
pub struct NotebookHookCli {
    /// The in-process callback every call is dispatched through.
    callback: ::smithy_server_runtime::EmbeddedEmulator,
    /// The async runtime those calls are driven on. A hook's script is performed on a thread
    /// of the blocking pool rather than on a worker of this runtime, so a call is made by
    /// blocking that thread on this handle rather than by making the session asynchronous.
    runtime: ::tokio::runtime::Handle,
    /// The Region the calls are made in for a command line naming none, which is the Region
    /// the instance was created in; a line that names one addresses that Region instead.
    region: String,
    /// The execution role the instance RUNS AS, whose permissions every call is authorized
    /// against.
    role_arn: String,
    /// The partition an Amazon S3 ARN the authorization asks about carries, read off that
    /// role's own ARN.
    partition: String,
    /// The instant this hook's calls have spent [`HOOK_CALL_BUDGET`].
    deadline: Instant,
}

/// The CLI the script of ONE hook of an instance placed in `region` and running as `role_arn`
/// gets — `None` where there is no async runtime to dispatch a call on, in which case the
/// script reaches no object store at all and the session reports its `aws` commands as
/// commands it could not run.
#[must_use]
pub fn notebook_hook_cli(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
) -> Option<NotebookHookCli> {
    Some(NotebookHookCli {
        callback: ctx.callback().clone(),
        runtime: ::tokio::runtime::Handle::try_current().ok()?,
        region: region.to_string(),
        role_arn: role_arn.to_string(),
        partition: arn_partition(role_arn),
        deadline: Instant::now() + HOOK_CALL_BUDGET,
    })
}

/// What the script `script` of one hook wrote, with `cli` as the AWS CLI its command lines
/// reach — performed OFF the request worker.
///
/// Performing a caller's own script is synchronous work, and the calls its commands make are
/// driven by BLOCKING the thread it runs on, which a runtime worker may not do: the script
/// therefore goes to the blocking pool, exactly as a job container's program does
/// ([`super::container_run::run_container_program_off_worker`]), and the worker serving the
/// request is released while it runs — so the health endpoint, a timeout and a shutdown signal
/// are all still served while a hook runs.
///
/// A run this process could not perform at all wrote nothing, and it says so in this service's
/// own tracing: a task that did not answer is a fault of the emulator rather than a script that
/// produced no output, so the two are not left indistinguishable.
pub async fn hook_output_performed(script: &str, cli: Option<NotebookHookCli>) -> Vec<String> {
    let script = script.to_string();
    let performed = ::tokio::task::spawn_blocking(move || {
        super::notebook_lifecycle_hook::hook_output_with(
            &script,
            cli.map(|cli| Box::new(cli) as Box<dyn EnvironmentPrograms>),
        )
    })
    .await;
    match performed {
        Ok(written) => written,
        Err(error) => {
            ::tracing::warn!(
                error = %error,
                "a notebook instance's lifecycle hook was not performed, so nothing it would \
                 have written exists"
            );
            Vec::new()
        }
    }
}

impl EnvironmentPrograms for NotebookHookCli {
    /// The object calls above, performed against the store; `None` for every other program
    /// name and every invocation the shared reading answers nothing for, which the session
    /// reports as a command it could not run.
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        self.run_with_input(name, arguments, None, files)
    }

    /// The same, for the invocation that READS the command's own standard input: an upload
    /// whose source is the CLI's stream operand carries what the stage before it wrote.
    fn run_with_input(
        &self,
        name: &str,
        arguments: &[String],
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let answered = self.performed(name, arguments, standard_input, files)?;
        Some(ProgramOutcome {
            status: answered.status,
            output: answered.output,
        })
    }

    /// The form the session asks, which keeps the two streams apart: what the CLI REPORTED it
    /// could not do is a diagnostic rather than part of what the command wrote, so it never
    /// reaches the stage after it in a pipeline.
    fn run_invocation(
        &self,
        invocation: &ProgramInvocation<'_>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramStreams> {
        self.performed(
            invocation.name,
            invocation.arguments,
            invocation.standard_input,
            files,
        )
    }
}

impl NotebookHookCli {
    /// What this hook's `aws` command answers for `arguments` — the words after the program
    /// name — or `None` for every program name and every invocation this program performs
    /// nothing of.
    fn performed(
        &self,
        name: &str,
        arguments: &[String],
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramStreams> {
        if name != cli::CLI_PROGRAM {
            return None;
        }
        if let Some(call) = cli::requested_api(arguments) {
            return Some(self.performed_api(&call, files));
        }
        let requested = cli::requested(arguments)?;
        // The Region the calls go to is the one the command line names, and the Region the
        // instance was created in for a line naming none — the CLI's own reading of that
        // option, which selects where the call goes and nothing about the transfer.
        let region = cli::requested_region(arguments).unwrap_or_else(|| self.region.clone());
        let word = requested.reported_word();
        let shown = requested.shown();
        match self.carried(&requested, &region, standard_input, files) {
            // The CLI reports a transfer it could not make on its standard error, naming what
            // it was doing, the two ends and what stopped it.
            Err(detail) => Some(reported_error(
                TRANSFER_FAILED,
                format!("{word} failed: {shown} {detail}"),
            )),
            // A download to the command's own standard OUTPUT writes the object's bytes there
            // and nothing else: they are what the stage after it reads, so no report of this
            // emulator's is written onto that stream.
            Ok(Some(text)) => Some(ProgramStreams {
                status: TRANSFER_SUCCEEDED,
                output: text.lines().map(str::to_string).collect(),
                errors: Vec::new(),
            }),
            Ok(None) => Some(reported(TRANSFER_SUCCEEDED, format!("{word}: {shown}"))),
        }
    }

    /// Perform one LOW-LEVEL call (`aws s3api …`) and answer what the CLI reports for it: the
    /// response document on standard output, or the refusal on standard error with the status
    /// that refusal gives the command.
    fn performed_api(
        &self,
        call: &cli::ApiCall,
        files: &mut dyn ProgramFiles,
    ) -> ProgramStreams {
        let region = call.region.as_deref().unwrap_or(&self.region);
        let client = self.client(region);
        let answered = match &call.operation {
            ApiOperation::PutObject {
                target,
                body,
                content_type,
            } => {
                // The bytes the write carries are the file the command names; an invocation
                // naming none writes an empty object, which is what that form does.
                let contents = match body {
                    Some(path) => match files.read(path) {
                        Some(held) => held,
                        // A local path that is not there is the CLI's own failure, in the
                        // words it words a path it was pointed at and could not read.
                        None => {
                            return reported_error(
                                API_COMMAND_ERROR,
                                format!("The user-provided path {path} does not exist."),
                            )
                        }
                    },
                    None => String::new(),
                };
                self.made(
                    PUT_OBJECT_ACTION,
                    target,
                    region,
                    &client,
                    cli::put_object_api(
                        &client,
                        target,
                        &contents,
                        content_type.as_deref(),
                        MAX_TRANSFERRED_BYTES,
                    ),
                )
            }
            ApiOperation::GetObject { source, outfile } => {
                match self.made(
                    GET_OBJECT_ACTION,
                    source,
                    region,
                    &client,
                    cli::get_object_api(&client, source, MAX_TRANSFERRED_BYTES),
                ) {
                    Ok((contents, document)) => {
                        if files.write(outfile, &contents) {
                            Ok(document)
                        } else {
                            Err(unwritten_file(outfile))
                        }
                    }
                    Err(detail) => Err(detail),
                }
            }
            ApiOperation::HeadObject { target } => self.made(
                GET_OBJECT_ACTION,
                target,
                region,
                &client,
                cli::head_object_api(&client, target),
            ),
            ApiOperation::DeleteObject { target } => self
                .made(
                    DELETE_OBJECT_ACTION,
                    target,
                    region,
                    &client,
                    cli::delete_object(&client, target),
                )
                .map(|()| String::new()),
        };
        match answered {
            Ok(document) if document.is_empty() => ProgramStreams {
                status: TRANSFER_SUCCEEDED,
                output: Vec::new(),
                errors: Vec::new(),
            },
            Ok(document) => reported(TRANSFER_SUCCEEDED, document),
            Err(detail) => reported_error(API_SERVICE_ERROR, detail),
        }
    }

    /// Perform one asked-for transfer; `Ok(Some(text))` is one whose destination is the
    /// command's own standard output, and `Err` carries what the CLI reports about a transfer
    /// that was not made.
    fn carried(
        &self,
        requested: &Requested,
        region: &str,
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Result<Option<String>, String> {
        let (from, to, taking_source_away) = match requested {
            Requested::Remove { target } => {
                let client = self.client(region);
                self.made(
                    DELETE_OBJECT_ACTION,
                    target,
                    region,
                    &client,
                    cli::delete_object(&client, target),
                )?;
                return Ok(None);
            }
            Requested::Carry {
                from,
                to,
                taking_source_away,
            } => (from, to, *taking_source_away),
        };
        let streamed = self.copied(from, to, region, standard_input, files)?;
        // `mv` takes the source away once the destination holds it, so a move that could not
        // be completed leaves the file where it was.
        if taking_source_away {
            match from {
                Location::Object(object) => {
                    let client = self.client(region);
                    self.made(
                        DELETE_OBJECT_ACTION,
                        object,
                        region,
                        &client,
                        cli::delete_object(&client, object),
                    )?;
                }
                Location::Local(path) => {
                    files.remove(path);
                }
                // Filtered out by the shared reading: `mv` takes no stream operand.
                Location::Stream => {}
            }
        }
        Ok(streamed)
    }

    /// Carry the file at `from` to `to`, or answer why it was not carried.
    fn copied(
        &self,
        from: &Location,
        to: &Location,
        region: &str,
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Result<Option<String>, String> {
        match (from, to) {
            (Location::Object(source), Location::Object(destination)) => {
                // A copy reads one object and writes another, so both ends are authorized:
                // the role has to be granted the read it makes and the write it makes.
                let client = self.client(region);
                self.authorized(GET_OBJECT_ACTION, source, region, &client)?;
                self.made(
                    PUT_OBJECT_ACTION,
                    destination,
                    region,
                    &client,
                    cli::copy_object(&client, source, destination),
                )?;
                Ok(None)
            }
            (Location::Local(path), Location::Object(destination)) => {
                let contents = files
                    .read(path)
                    .ok_or_else(|| format!("The user-provided path {path} does not exist."))?;
                self.put(destination, &contents, region)?;
                Ok(None)
            }
            (Location::Object(source), Location::Local(path)) => {
                let contents = self.get(source, region)?;
                if !files.write(path, &contents) {
                    return Err(unwritten_file(path));
                }
                Ok(None)
            }
            // An upload FROM the command's own standard input carries what the stage before it
            // wrote. A command that was given no standard input at all carries nothing, so
            // nothing is written: an empty object would be a write the command never made.
            (Location::Stream, Location::Object(destination)) => {
                let contents = standard_input
                    .ok_or_else(|| "The command was given no standard input to upload.".to_string())?;
                self.put(destination, contents, region)?;
                Ok(None)
            }
            // A download TO it answers the object's own text, which the caller writes onto
            // that stream.
            (Location::Object(source), Location::Stream) => self.get(source, region).map(Some),
            // Filtered out by the shared reading: a transfer with no end in the object store
            // is not one this program performs.
            (Location::Local(_) | Location::Stream, Location::Local(_) | Location::Stream) => Err(
                "This command performs no transfer that does not reach Amazon S3.".to_string(),
            ),
        }
    }

    /// The object's own text, bounded by [`MAX_TRANSFERRED_BYTES`].
    fn get(&self, address: &ObjectAddress, region: &str) -> Result<String, String> {
        let client = self.client(region);
        self.made(
            GET_OBJECT_ACTION,
            address,
            region,
            &client,
            cli::get_object_text(&client, address, MAX_TRANSFERRED_BYTES),
        )
    }

    /// Write `contents` as the object at `address`, up to the same bound.
    fn put(&self, address: &ObjectAddress, contents: &str, region: &str) -> Result<(), String> {
        let client = self.client(region);
        self.made(
            PUT_OBJECT_ACTION,
            address,
            region,
            &client,
            cli::put_object_text(&client, address, contents, MAX_TRANSFERRED_BYTES),
        )
    }

    /// The Amazon S3 client this hook's calls are made with, addressed to `region`: this
    /// crate's one construction of the in-process hop into this same emulator.
    fn client(&self, region: &str) -> ::aws_sdk_s3::Client {
        s3_client_through(&self.callback, region)
    }

    /// Make ONE call on `address`, authorized as the execution role, inside what this hook's
    /// calls have left of [`HOOK_CALL_BUDGET`].
    ///
    /// The thread this runs on is the one the hook's script is performed on rather than a
    /// worker of the runtime, so the call is driven by blocking it on the runtime handle. The
    /// wait itself is built INSIDE that block, which is where this thread is in the runtime's
    /// own context and so where a timer of it can be armed at all.
    fn made<T>(
        &self,
        action: &str,
        address: &ObjectAddress,
        region: &str,
        client: &::aws_sdk_s3::Client,
        call: impl ::std::future::Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let Some(left) = self.left() else {
            return Err(spent_budget());
        };
        self.runtime.block_on(async {
            let made = async {
                self.withheld(action, address, region, client).await?;
                call.await
            };
            match ::tokio::time::timeout(left, made).await {
                Ok(answered) => answered,
                Err(_elapsed) => Err(spent_budget()),
            }
        })
    }

    /// `Ok(())` when the permissions in force on the execution role let it perform `action` on
    /// the object `address` names, and the refusal the CLI reports for a call Amazon S3 would
    /// not have served otherwise.
    fn authorized(
        &self,
        action: &str,
        address: &ObjectAddress,
        region: &str,
        client: &::aws_sdk_s3::Client,
    ) -> Result<(), String> {
        let Some(left) = self.left() else {
            return Err(spent_budget());
        };
        self.runtime.block_on(async {
            match ::tokio::time::timeout(left, self.withheld(action, address, region, client)).await
            {
                Ok(answered) => answered,
                Err(_elapsed) => Err(spent_budget()),
            }
        })
    }

    /// The one authorization question every call of this program asks — asked of the role the
    /// instance runs as, through this crate's one reading of it
    /// ([`super::s3_access::object_action_is_unauthorized_through`]).
    async fn withheld(
        &self,
        action: &str,
        address: &ObjectAddress,
        region: &str,
        client: &::aws_sdk_s3::Client,
    ) -> Result<(), String> {
        let location = S3Location {
            bucket: address.bucket.clone(),
            key: address.key.clone(),
        };
        if object_action_is_unauthorized_through(
            &self.callback,
            client,
            region,
            &self.role_arn,
            &self.partition,
            action,
            &location,
        )
        .await
        {
            return Err(access_denied(action));
        }
        Ok(())
    }

    /// What is left of this hook's call budget, and `None` once it is spent.
    fn left(&self) -> Option<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
    }
}

/// What the CLI reports for a call the object store would refuse: the service's own refusal,
/// in the form the CLI words a service error in ("An error occurred (<code>) when calling the
/// <operation> operation: <message>") — the code and the sentence Amazon S3 publishes for a
/// call the caller's permissions do not allow.
fn access_denied(action: &str) -> String {
    format!(
        "An error occurred (AccessDenied) when calling the {operation} operation: Access Denied",
        operation = operation_of(action)
    )
}

/// The S3 API operation `action` authorizes, which is what the CLI names in its account of a
/// refusal.
fn operation_of(action: &str) -> &'static str {
    match action {
        PUT_OBJECT_ACTION => "PutObject",
        DELETE_OBJECT_ACTION => "DeleteObject",
        _ => "GetObject",
    }
}

/// What a command whose calls ran past [`HOOK_CALL_BUDGET`] reports.
fn spent_budget() -> String {
    format!(
        "The command was stopped because this hook's calls ran past the {HOOK_CALL_BUDGET:?} \
         this emulator gives them."
    )
}

/// What the CLI reports for a local file its download could not write.
fn unwritten_file(path: &str) -> String {
    format!(
        "The file {path} was not written: it is past a bound this emulator gives the files one \
         script may hold."
    )
}

/// What the command WROTE on standard output, as the lines the session keeps, with the status
/// it exits with.
fn reported(status: i32, written: String) -> ProgramStreams {
    ProgramStreams {
        status,
        output: written.lines().map(str::to_string).collect(),
        errors: Vec::new(),
    }
}

/// One line on standard ERROR, with the status the command exits with.
fn reported_error(status: i32, line: String) -> ProgramStreams {
    ProgramStreams {
        status,
        output: Vec::new(),
        errors: vec![line],
    }
}
