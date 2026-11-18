// SPDX-License-Identifier: CC0-1.0

//! The AWS CLI as a container's session provides it: the `aws s3` transfers one container's
//! program asks for, really performed against this emulator's own object store as the task
//! role.
//!
//! # The rule this module implements
//!
//! A container runs the program its image holds, and the image a task definition names is
//! routinely one whose program is the AWS CLI: a container whose command carries a file into a
//! bucket has that file carried, so the object exists once the container has run and the status
//! the command exits with is the status of the transfer. WHO made the transfer is the task role
//! — "the IAM role that grants containers in the task permission to call Amazon Web Services
//! APIs on your behalf" (`TaskDefinition$taskRoleArn`) — so the object store judges it against
//! the policies in force on that role: a container whose task role lacks the action is refused
//! by Amazon S3, and the command reports that refusal and exits as a failed transfer.
//!
//! What ONE command line asks for, and the calls that carry it, are the workspace's shared
//! reading of that program ([`::service_common::aws_s3_cli`]) — the same one every service that
//! runs a caller-supplied command line uses, so two of them cannot carry a different file or
//! answer a different status. Both of the CLI's object command groups are provided: the
//! file-oriented `aws s3` commands and the low-level `aws s3api` object operations. What this
//! module adds is the container's own half: the FILES a local path names (the container's layer
//! and the volumes mounted into it, [`super::container_filesystem`]), the CREDENTIAL the calls
//! present ([`super::task_role_credentials`]), and the bound one command's calls are made
//! inside.
//!
//! Every call goes over the framework's in-process callback, through this workspace's one
//! construction of that client ([`::service_common::s3_client`]) — the same transport a
//! client's own S3 request takes, so the object a container wrote is the object a later
//! `GetObject` of that bucket reads. Nothing is dialled, resolved or opened, and no host file
//! is read or written. The credential is held HERE and never reaches the program: it is not in
//! the session's variables, in its filesystem, or in anything the container writes.
//!
//! # Bounds
//!
//! Both bounds are this emulator's and neither is reachable from the command: how many bytes
//! one transfer carries ([`MAX_TRANSFERRED_BYTES`], enforced by the RANGED read so nothing
//! larger is buffered) and the wall clock ONE command's calls may spend ([`CALL_BUDGET`]). The
//! session's own bounds — its command budget, its line size, its filesystem — are
//! [`super::container_shell`]'s and [`super::container_filesystem`]'s.

use ::std::time::Duration;

use ::service_common::aws_s3_cli::{
    self as cli, Location, ObjectAddress, Requested, NO_CREDENTIAL_LOCATED, TRANSFER_FAILED,
    TRANSFER_SUCCEEDED,
};
use ::service_common::posix_shell_files::ShellFiles;

/// How many bytes ONE transfer carries.
///
/// THIS NUMBER IS THE EMULATOR'S, not an AWS limit: the bytes are a client's object or a
/// client's file, so what one transfer takes into this process is bounded here rather than by
/// the object's own size. A transfer carries TEXT only, for the same reason the container's
/// filesystem holds text; an object that is not text is a download that did not happen.
pub const MAX_TRANSFERRED_BYTES: usize = 256 * 1024;

/// The wall clock ONE command's calls may spend.
///
/// THIS NUMBER IS THE EMULATOR'S, not an AWS duration: the call is in-process, so a command
/// whose call cannot be answered reports that bound rather than being left outstanding for the
/// rest of the container's run.
pub const CALL_BUDGET: Duration = Duration::from_secs(5);

/// The provider name the credential a transfer is signed with carries: it names this emulator
/// rather than any AWS credential source, and never travels on the wire.
const TASK_CREDENTIAL_SOURCE: &str = "aws-ecs-task-role";

/// What one `aws` command of a container answered: the status it exits with, the line it wrote
/// on its standard output and the line it wrote on its standard error.
#[derive(Debug, Clone, Default)]
pub struct CliOutcome {
    /// The status the command exits with, which is what the program around it reads.
    pub status: i32,
    /// What it wrote to standard output, ending in a newline when it wrote anything.
    pub output: String,
    /// What it wrote to standard error, ending in a newline when it wrote anything.
    pub errors: String,
}

/// What the containers of ONE task PRESENT on the AWS calls their programs make — the
/// credential this program signs with, vended for the task before any container of it starts
/// ([`super::task_role_credentials`]).
///
/// It is held by host code and is reachable from nothing a container runs: it is put on the
/// request this module signs and on nothing else — not in the session's variables, not in its
/// filesystem, not in anything the container writes, and in no response of this service.
#[derive(Clone)]
pub enum TaskRoleCredential {
    /// The session minted for the task role this task runs under.
    Session {
        /// The access key id, which is where the service serving a call reads WHO made it.
        access_key_id: String,
        /// Its secret, used to sign and nowhere else.
        secret_access_key: String,
        /// The session token a temporary credential is served only with.
        session_token: String,
    },
    /// Nothing could be shown about the task role ([`super::task_role_credentials`]), so the
    /// calls present the credential the framework hands this emulator's handlers, as every
    /// other in-process call between two emulated services does.
    Emulator,
    /// The task resolves no task role, so its containers hold no credential: a program that
    /// makes an AWS call locates none.
    Absent,
}

/// Prints the KIND of credential held and no field of it, so neither a key id nor a secret nor
/// a session token can reach a log through a debug form of this type or of ANY type holding one
/// — this module's own [`ContainerAwsCli`], a [`super::container_runtime::ContainerLaunch`] and
/// a whole [`super::TaskRecord`] among them, each of which derives its debug form from this.
/// The test at the end of this module is what pins that.
impl ::std::fmt::Debug for TaskRoleCredential {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        let held = match self {
            Self::Session { .. } => "Session",
            Self::Emulator => "Emulator",
            Self::Absent => "Absent",
        };
        f.debug_struct(held).finish_non_exhaustive()
    }
}

/// The AWS CLI one container's session provides: the `aws s3` transfers of that container,
/// made against this emulator's object store as the task role.
///
/// Built per container run ([`super::container_runtime`]) and shared with the session
/// performing it, which is why it holds no mutable state of its own.
#[derive(Clone)]
pub struct ContainerAwsCli {
    /// The in-process callback every call is dispatched through.
    callback: ::smithy_server_runtime::EmbeddedEmulator,
    /// The Region the calls are made in, which is the Region the task was placed in.
    region: String,
    /// What every call presents ([`super::task_role_credentials`]).
    credential: TaskRoleCredential,
}

/// Prints the Region this CLI is addressed to and NOTHING of the credential it signs with: the
/// debug form of a type holding a credential never carries one, whichever way it is reached.
impl ::std::fmt::Debug for ContainerAwsCli {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_struct("ContainerAwsCli")
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

impl ContainerAwsCli {
    /// The CLI a container placed in `region` gets, presenting `credential` on every call it
    /// makes.
    #[must_use]
    pub fn new(
        callback: ::smithy_server_runtime::EmbeddedEmulator,
        region: &str,
        credential: TaskRoleCredential,
    ) -> Self {
        Self {
            callback,
            region: region.to_string(),
            credential,
        }
    }

    /// What this container's `aws` command answers for `arguments` — the words after the
    /// program name — or `None` for every invocation this program performs nothing of, which
    /// the session reports as a command it could not run.
    ///
    /// `standard_input` is what the command was given on its own standard input, which is what
    /// an upload from the CLI's stream operand carries.
    pub async fn perform(
        &self,
        arguments: &[String],
        standard_input: Option<&str>,
        files: &mut (dyn ShellFiles + Send),
    ) -> Option<CliOutcome> {
        if let Some(call) = cli::requested_api(arguments) {
            return Some(self.performed_api(&call, files).await);
        }
        let requested = cli::requested(arguments)?;
        let word = requested.reported_word();
        let shown = requested.shown();
        match self.carried(&requested, standard_input, files).await {
            Err(detail) => Some(CliOutcome {
                status: TRANSFER_FAILED,
                output: String::new(),
                // The CLI reports a transfer it could not make on its standard error, naming
                // what it was doing, the two ends and what stopped it.
                errors: format!("{word} failed: {shown} {detail}\n"),
            }),
            // A download to the command's own standard OUTPUT writes the object's bytes there
            // and nothing else: they are what the stage after it reads, so no report of this
            // emulator's is written onto that stream.
            Ok(Some(text)) => Some(CliOutcome {
                status: TRANSFER_SUCCEEDED,
                output: text,
                errors: String::new(),
            }),
            Ok(None) => Some(CliOutcome {
                status: TRANSFER_SUCCEEDED,
                output: format!("{word}: {shown}\n"),
                errors: String::new(),
            }),
        }
    }

    /// Perform one LOW-LEVEL call (`aws s3api …`) and answer what the CLI reports for it: the
    /// response document on standard output, or the service's own refusal on standard error
    /// with the status that refusal gives the command
    /// ([`::service_common::aws_s3_cli::API_SERVICE_ERROR`]).
    async fn performed_api(
        &self,
        call: &::service_common::aws_s3_cli::ApiCall,
        files: &mut (dyn ShellFiles + Send),
    ) -> CliOutcome {
        use ::service_common::aws_s3_cli::{ApiOperation, API_COMMAND_ERROR, API_SERVICE_ERROR};

        let region = call.region.as_deref().unwrap_or(&self.region);
        let client = match self.client(region) {
            Ok(client) => client,
            // No credential to sign with is the CLI's own failure rather than a service
            // refusal: no call was made at all.
            Err(detail) => {
                return CliOutcome {
                    status: API_COMMAND_ERROR,
                    output: String::new(),
                    errors: format!("{detail}\n"),
                }
            }
        };
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
                        Ok(held) => held,
                        // A local path that is not there is the CLI's own failure, in the
                        // words it words a path it was pointed at and could not read.
                        Err(_) => {
                            return CliOutcome {
                                status: API_COMMAND_ERROR,
                                output: String::new(),
                                errors: format!(
                                    "The user-provided path {path} does not exist.\n"
                                ),
                            }
                        }
                    },
                    None => String::new(),
                };
                self.made(cli::put_object_api(
                    &client,
                    target,
                    &contents,
                    content_type.as_deref(),
                    MAX_TRANSFERRED_BYTES,
                ))
                .await
            }
            ApiOperation::GetObject { source, outfile } => {
                match self
                    .made(cli::get_object_api(&client, source, MAX_TRANSFERRED_BYTES))
                    .await
                {
                    Ok((contents, document)) => match files.write(outfile, &contents, false) {
                        Ok(()) => Ok(document),
                        Err(refusal) => Err(format!(
                            "The file {outfile} was not written: {}.",
                            refusal.message()
                        )),
                    },
                    Err(detail) => Err(detail),
                }
            }
            ApiOperation::HeadObject { target } => {
                self.made(cli::head_object_api(&client, target)).await
            }
            ApiOperation::DeleteObject { target } => self
                .made(cli::delete_object(&client, target))
                .await
                .map(|()| String::new()),
        };
        match answered {
            Ok(document) => CliOutcome {
                status: TRANSFER_SUCCEEDED,
                output: if document.is_empty() {
                    String::new()
                } else {
                    format!("{document}\n")
                },
                errors: String::new(),
            },
            Err(detail) => CliOutcome {
                status: API_SERVICE_ERROR,
                output: String::new(),
                errors: format!("{detail}\n"),
            },
        }
    }

    /// Perform one asked-for transfer; `Ok(Some(text))` is one whose destination is the
    /// command's own standard output, and `Err` carries what the CLI reports about a transfer
    /// that was not made.
    async fn carried(
        &self,
        requested: &Requested,
        standard_input: Option<&str>,
        files: &mut (dyn ShellFiles + Send),
    ) -> Result<Option<String>, String> {
        let (from, to, taking_source_away) = match requested {
            Requested::Remove { target } => {
                self.made(cli::delete_object(&self.client(&self.region)?, target))
                    .await?;
                return Ok(None);
            }
            Requested::Carry {
                from,
                to,
                taking_source_away,
            } => (from, to, *taking_source_away),
        };
        let streamed = self.copied(from, to, standard_input, files).await?;
        // `mv` takes the source away once the destination holds it, so a move that could not be
        // completed leaves the file where it was.
        if taking_source_away {
            match from {
                Location::Object(object) => {
                    self.made(cli::delete_object(&self.client(&self.region)?, object))
                        .await?;
                }
                Location::Local(path) => {
                    let _ = files.remove_file(path);
                }
                // Filtered out by the shared parse: `mv` takes no stream operand.
                Location::Stream => {}
            }
        }
        Ok(streamed)
    }

    /// Carry the file at `from` to `to`, or answer why it was not carried.
    async fn copied(
        &self,
        from: &Location,
        to: &Location,
        standard_input: Option<&str>,
        files: &mut (dyn ShellFiles + Send),
    ) -> Result<Option<String>, String> {
        match (from, to) {
            (Location::Object(source), Location::Object(destination)) => {
                self.made(cli::copy_object(
                    &self.client(&self.region)?,
                    source,
                    destination,
                ))
                .await?;
                Ok(None)
            }
            (Location::Local(path), Location::Object(destination)) => {
                let contents = files
                    .read(path)
                    .map_err(|_| format!("The user-provided path {path} does not exist."))?;
                self.put(destination, &contents).await?;
                Ok(None)
            }
            (Location::Object(source), Location::Local(path)) => {
                let contents = self.get(source).await?;
                files.write(path, &contents, false).map_err(|refusal| {
                    format!("The file {path} was not written: {}.", refusal.message())
                })?;
                Ok(None)
            }
            // An upload FROM the command's own standard input carries what the stage before it
            // wrote. A command that was given no standard input at all carries nothing, so
            // nothing is written: an empty object would be a write the command never made.
            (Location::Stream, Location::Object(destination)) => {
                let contents = standard_input
                    .ok_or_else(|| "The command was given no standard input to upload.".to_string())?;
                self.put(destination, contents).await?;
                Ok(None)
            }
            // A download TO it answers the object's own text, which the caller writes onto that
            // stream.
            (Location::Object(source), Location::Stream) => self.get(source).await.map(Some),
            // Filtered out by the shared parse: a transfer with no end in the object store is
            // not one this program performs.
            (Location::Local(_) | Location::Stream, Location::Local(_) | Location::Stream) => {
                Err("This command performs no transfer that does not reach Amazon S3.".to_string())
            }
        }
    }

    /// The object's own text, bounded by [`MAX_TRANSFERRED_BYTES`].
    async fn get(&self, address: &ObjectAddress) -> Result<String, String> {
        self.made(cli::get_object_text(
            &self.client(&self.region)?,
            address,
            MAX_TRANSFERRED_BYTES,
        ))
        .await
    }

    /// Write `contents` as the object at `address`, up to the same bound.
    async fn put(&self, address: &ObjectAddress, contents: &str) -> Result<(), String> {
        self.made(cli::put_object_text(
            &self.client(&self.region)?,
            address,
            contents,
            MAX_TRANSFERRED_BYTES,
        ))
        .await
    }

    /// Make ONE call, inside [`CALL_BUDGET`].
    async fn made<T>(
        &self,
        call: impl ::std::future::Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        match ::tokio::time::timeout(CALL_BUDGET, call).await {
            Ok(answered) => answered,
            Err(_elapsed) => Err(format!(
                "The transfer was stopped because this command's calls ran past the \
                 {CALL_BUDGET:?} this emulator gives them."
            )),
        }
    }

    /// The Amazon S3 client this command's calls are made with, addressed to `region`: the
    /// in-process transport, signed as the session of the task role where one was vended for
    /// the task.
    ///
    /// `Err` where the task holds no credential at all: the CLI makes no call it has none for
    /// and reports [`NO_CREDENTIAL_LOCATED`], which is the failure of the call.
    fn client(&self, region: &str) -> Result<::aws_sdk_s3::Client, String> {
        match &self.credential {
            TaskRoleCredential::Session {
                access_key_id,
                secret_access_key,
                session_token,
            } => Ok(::service_common::s3_client::s3_client_signed_as(
                &self.callback,
                region,
                ::aws_sdk_s3::config::Credentials::new(
                    access_key_id.clone(),
                    secret_access_key.clone(),
                    Some(session_token.clone()),
                    None,
                    TASK_CREDENTIAL_SOURCE,
                ),
            )),
            TaskRoleCredential::Emulator => Ok(::service_common::s3_client::s3_client(
                &self.callback,
                region,
            )),
            TaskRoleCredential::Absent => Err(NO_CREDENTIAL_LOCATED.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TaskRoleCredential;

    /// RULE: a credential this emulator vends for a task is reachable from no debug form —
    /// neither its own, nor that of any type holding one, which is how a `tracing` line that
    /// formats a whole record (a task, a launch, this module's CLI) stays free of it.
    ///
    /// The rows are the three ways the debug form can be reached: the credential itself, a
    /// struct that DERIVES its debug form from it (which is how `TaskRecord` and
    /// `ContainerLaunch` hold one), and the one this module builds around it.
    #[test]
    fn no_debug_form_of_a_vended_credential_carries_its_secrets() {
        // The three values a session carries, each a string nothing else here contains.
        let (key_id, secret, token) = (
            "ASIAVENDEDKEYIDPROBE",
            "vended-secret-access-key-probe",
            "vended-session-token-probe",
        );
        let credential = TaskRoleCredential::Session {
            access_key_id: key_id.to_string(),
            secret_access_key: secret.to_string(),
            session_token: token.to_string(),
        };

        /// A record that holds a credential the way this store's own records do: by DERIVING
        /// its debug form from the credential's.
        #[derive(Debug)]
        struct Holder {
            task_arn: String,
            credential: TaskRoleCredential,
        }

        let holder = Holder {
            task_arn: "arn:aws:ecs:us-west-2:000000000000:task/probe/0".to_string(),
            credential: credential.clone(),
        };
        // (case, the debug form to inspect)
        let rows: [(&str, String); 2] = [
            ("the credential itself", format!("{credential:?}")),
            ("a record that derives its debug form from it", format!("{holder:?}")),
        ];
        for (case, written) in rows {
            for secret in [key_id, secret, token] {
                assert!(
                    !written.contains(secret),
                    "case {case:?}: expected no part of the credential in {written:?}, found \
                     {secret:?}"
                );
            }
        }
        // The kind held is still reported, which is what makes such a line worth logging.
        let written = format!("{credential:?}");
        assert!(
            written.contains("Session"),
            "expected the KIND of credential held in {written:?}"
        );
        // And the record's own members are still reported, so the redaction is the
        // credential's alone — read off the record itself, which is where a log line reads
        // them.
        assert!(
            holder.task_arn.contains("probe"),
            "the record holds the task it was vended for, got {:?}",
            holder.task_arn
        );
        assert!(
            matches!(holder.credential, TaskRoleCredential::Session { .. }),
            "the record holds the session that was vended, got {:?}",
            holder.credential
        );
        let written = format!("{holder:?}");
        assert!(
            written.contains("probe"),
            "expected the record's own members in {written:?}"
        );
    }
}
