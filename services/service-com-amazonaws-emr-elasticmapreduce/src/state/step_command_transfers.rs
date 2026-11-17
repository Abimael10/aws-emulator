// SPDX-License-Identifier: CC0-1.0

//! The Amazon S3 TRANSFERS a step's command line asks for, really performed against this
//! emulator's own object store.
//!
//! # The rule this module implements
//!
//! "command-runner.jar … lets you run commands on your cluster" (Amazon EMR
//! documentation), so a `command-runner.jar` step's arguments are a command line and what
//! that command line does is the step's own work. A step whose command line carries a file
//! between the cluster and Amazon S3 therefore has that file carried: the object the command
//! named exists once the step has run, and the status the step ends in is the status of the
//! transfer. A command whose transfer the store refused answers the failure the store
//! answered — never a success for a write nothing made, and never a success for a write the
//! store did not take.
//!
//! The command line reaches the ONE bounded shell session this workspace has
//! ([`::service_common::posix_shell_session`], whose catalog of session-local utilities and
//! whose bounds are stated there), and this module is one PROGRAM of the environment that
//! session runs in: the AWS CLI, asked for the `aws s3` commands below and for nothing else.
//! A command line naming any other program, any other command group, any other command of
//! this group, or any form of these commands this program does not perform, is a command the
//! session reports it could not run — and the step then reports that its work was not run.
//! No host command, process or shell is started here or anywhere this calls.
//!
//! # Where the bytes go
//!
//! The transfer is made with the AWS SDK for Amazon S3 driven over the framework's
//! in-process callback ([`smithy_server_runtime::OperationContext::callback`], through this
//! workspace's one construction of that client, [`::service_common::s3_client`]) — the same
//! transport a client's own S3 request takes, so the object a step wrote is the object a
//! later `HeadObject` or `GetObject` of that bucket reads. The call is SIGNED as the role
//! the step runs as, so the object store authorizes it against the policies in force on that
//! role and the bucket's own policy, exactly as it authorizes any other signed request; where
//! nothing is known about the role, it is signed with the credential the framework hands this
//! emulator's handlers, as every other in-process call between two emulated services is. The
//! credential is held here and never reaches the command: it is not in the session's
//! variables, in its tree, or in anything this program writes.
//!
//! WHICH credential that is, is [`super::instance_profile_credentials`]'s: a step with no
//! runtime role of its own presents what the cluster's Amazon EC2 instances obtain from their
//! instance profile. An environment that holds NO credential makes no call at all, and the
//! transfer reports the CLI's own [`NO_CREDENTIAL_LOCATED`] — never a write made with a
//! credential the cluster's instances do not hold.
//!
//! The local side of a transfer is the session's own TREE
//! ([`::service_common::posix_shell_session::ProgramFiles`]): the file the commands around it
//! wrote with a redirection is the file it uploads, and the file it downloads is the one they
//! read back. A path is a key of that tree; no host file is opened, created or removed.
//!
//! # Bounds
//!
//! Every bound is this emulator's and none is reachable from the command: how many bytes one
//! transfer carries ([`MAX_TRANSFERRED_BYTES`], enforced by the RANGED read so nothing larger
//! is buffered) and the whole wall clock one command's transfers may spend
//! ([`TRANSFER_BUDGET`], shared by every call that command makes, so a command cannot hold the
//! operation that performs it for longer whatever it writes). A transfer also carries TEXT
//! only, because the session's tree holds text: an object that is not text is reported as a
//! download that did not happen, naming that bound, rather than as a file nothing could write.
//! The session's own bounds — the command budget, the line size, the tree size — are that
//! module's.

use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::aws_api::CallerIdentity;
use ::service_com_amazonaws_lambda_awsgirapiservice::state::execution::execution_role_session::execution_role_identity;
use ::service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles, ProgramOutcome};
use ::smithy_server_runtime::EmbeddedEmulator;

use super::*;

/// The name a command line invokes the AWS CLI by.
pub const OBJECT_CLI_PROGRAM: &str = "aws";

/// The CLI's command GROUP this program answers for: `aws s3`, "high-level S3 commands".
const S3_COMMAND_GROUP: &str = "s3";

/// The commands of that group this program performs, as the CLI spells them: `cp` ("copies a
/// local file or S3 object to another location locally or in S3"), `mv` ("moves a local file
/// or S3 object to another location locally or in S3") and `rm` ("deletes an S3 object").
/// SOURCE: the `aws s3` command reference. Every other command of the group — and every
/// other group — is one this program performs nothing of.
pub const PERFORMED_S3_COMMANDS: [&str; 3] = ["cp", "mv", "rm"];

/// How a command line addresses an object in Amazon S3: `s3://<bucket>/<key>`, the form the
/// CLI's S3 commands take. The other schemes this crate reads an object under
/// ([`S3_URI_SCHEMES`]) are the Hadoop filesystem's, which these commands do not take, so a
/// path written in one of them is a local path to them.
const S3_SCHEME: &str = "s3://";

/// The status a command answers when its transfer was made: "0 -- Command was successful.
/// There were no errors thrown by either the CLI or by the service the request was made to."
/// (AWS CLI return codes.)
const TRANSFER_SUCCEEDED: i32 = 0;

/// The status a command answers when its transfer was not made: "1 -- Limited errors … For
/// `s3` commands, this means at least one or more s3 transfers failed." (AWS CLI return
/// codes.) The step carrying it then ends `FAILED` with that status, which is what
/// `AddJobFlowSteps` documents a step's end as.
const TRANSFER_FAILED: i32 = 1;

/// How many bytes ONE transfer carries.
///
/// CHOSEN, not an AWS limit: the bytes are a caller's object or a caller's file, so what one
/// transfer takes into this process is bounded here rather than by the object's own size. On
/// a download it is enforced by the RANGED read, so nothing larger is ever buffered. A
/// transfer carries TEXT only, for the same reason the session's tree holds text; an object
/// that is not text is a download that did not happen.
pub const MAX_TRANSFERRED_BYTES: usize = 256 * 1024;

/// The whole wall clock ONE command's transfers may spend, shared by every call that command
/// makes.
///
/// CHOSEN, not an AWS duration: the operation that reports the step waits for the command, so
/// this is what a command's transfers may hold it for; a call with nothing left of it is not
/// made and the transfer reports that bound rather than being left outstanding.
pub const TRANSFER_BUDGET: Duration = Duration::from_secs(5);

/// The provider name the credential a transfer is signed with carries: it names this emulator
/// rather than any AWS credential source, and never travels on the wire.
const STEP_CREDENTIAL_SOURCE: &str = "aws-emr-step-role";

/// What the CLI reports for a command it has no credential to sign with: "Unable to locate
/// credentials" (AWS CLI). It is the detail of the transfer's own failure line, so a command
/// run on an instance that obtained no credential for its instance profile exits
/// [`TRANSFER_FAILED`] and its step FAILS ([`super::instance_profile_credentials`]).
const NO_CREDENTIAL_LOCATED: &str = "Unable to locate credentials";

/// The AWS CLI as the environment of a step's command line provides it: the `aws s3`
/// transfers of ONE command, made against this emulator's object store as the role the step
/// runs as.
///
/// Built per command ([`step_object_cli`]), which is what gives each command its own
/// [`TRANSFER_BUDGET`].
#[derive(Debug, Clone)]
pub struct StepObjectCli {
    /// The in-process callback every call is dispatched through.
    callback: EmbeddedEmulator,
    /// The async runtime that dispatch is driven on. A step's command is performed on a
    /// thread of its own, which is not one of this runtime's workers, so a call is made by
    /// blocking that thread on this handle rather than by making the session asynchronous.
    runtime: ::tokio::runtime::Handle,
    /// The Region the calls are made in, which is the one the request that performs the step
    /// was addressed to.
    region: String,
    /// What every call presents: the session of the role the step runs as, the credential the
    /// framework hands this emulator's handlers where no session could be minted for that
    /// role, or nothing at all where the environment holds no credential
    /// ([`HeldCredential`]). Its debug form carries no field.
    credential: HeldCredential,
    /// The instant this command's transfers have spent [`TRANSFER_BUDGET`].
    deadline: ::std::time::Instant,
}

/// What a step's calls PRESENT, as the environment that makes them holds it.
#[derive(Debug, Clone)]
enum HeldCredential {
    /// The session minted for the role the step runs as. Its debug form carries no field.
    Session(CallerIdentity),
    /// No session could be minted for that role, so the call presents the credential the
    /// framework hands this emulator's handlers, as every other in-process call between two
    /// emulated services does.
    Emulator,
    /// The environment holds NO credential: the cluster's instances obtained none for their
    /// instance profile, so no call is made at all
    /// ([`super::instance_profile_credentials`]).
    Absent,
}

/// The AWS CLI one command of the step `step_id` gets, presenting `credentials` on every call
/// it makes — `None` where there is no async runtime to dispatch a call on, in which case the
/// command reaches no object store at all and the session reports it as a command it could not
/// run.
#[must_use]
pub fn step_object_cli<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    credentials: &StepCredentials,
    step_id: &str,
) -> Option<StepObjectCli> {
    // The SESSION of the role the step runs as, through the one mint this workspace has for
    // an execution role's session. Nothing minted means nothing is known about the role,
    // which is the position every in-process call between two emulated services is in; an
    // environment that holds no credential at all mints none and presents none.
    let credential = match credentials {
        StepCredentials::Role(role_arn) => execution_role_identity(
            engine,
            ctx.credential_store(),
            role_arn,
            step_id,
            SystemTime::now(),
        )
        .map_or(HeldCredential::Emulator, HeldCredential::Session),
        StepCredentials::Absent => HeldCredential::Absent,
    };
    Some(StepObjectCli {
        callback: ctx.callback().clone(),
        runtime: ::tokio::runtime::Handle::try_current().ok()?,
        region: region.to_string(),
        credential,
        deadline: ::std::time::Instant::now() + TRANSFER_BUDGET,
    })
}

/// One end of a transfer: a path of the session's tree, or an object of the store.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Location {
    /// A path the command line named, which is a key of the session's own tree.
    Local(String),
    /// An `s3://<bucket>/<key>` address.
    Object(S3Object),
}

impl Location {
    /// `written` as the CLI reads it: an `s3://` address is an object and everything else is a
    /// local path. `None` for an `s3://` address that names no ONE object.
    fn of(written: &str) -> Option<Self> {
        if written.starts_with(S3_SCHEME) {
            return Some(Self::Object(addressed_object(written)?));
        }
        (!written.is_empty()).then(|| Self::Local(written.to_string()))
    }

    /// This end as the command line wrote it, which is how the CLI names it in its own report.
    fn shown(&self) -> String {
        match self {
            Self::Local(path) => path.clone(),
            Self::Object(object) => object_shown(object),
        }
    }
}

/// The object `written` addresses, read through this crate's ONE Amazon S3 URI parse
/// ([`s3_object`]).
///
/// `None` when `written` is not the address of one object: a path that is not an S3 address at
/// all, an address with no bucket or no key, or a key naming a PREFIX rather than an object —
/// which these commands take only with an option, and an option is a form this program
/// performs nothing of.
fn addressed_object(written: &str) -> Option<S3Object> {
    if !written.starts_with(S3_SCHEME) {
        return None;
    }
    let object = s3_object(written)?;
    (!object.key.ends_with('/')).then_some(object)
}

/// `object` as a command line writes it.
fn object_shown(object: &S3Object) -> String {
    format!("{S3_SCHEME}{}/{}", object.bucket, object.key)
}

/// What one `aws s3` command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Requested {
    /// `cp` and `mv`: the file at one end, carried to the other — and, for `mv`, taken away
    /// from the first.
    Carry {
        from: Location,
        to: Location,
        /// Set for `mv`, which "moves" rather than copies.
        taking_source_away: bool,
    },
    /// `rm`: the object deleted.
    Remove { target: S3Object },
}

/// What the command line `arguments` asks of the CLI, or `None` for every invocation this
/// program performs nothing of.
///
/// It performs `aws s3 cp|mv|rm` over the operands those commands take, and nothing else: an
/// invocation naming another command group or another command of this one, carrying an OPTION
/// (every option of these commands — a recursive transfer, another Region, a filter — changes
/// what the command does, so a line carrying one is not this invocation), carrying the wrong
/// number of operands, naming no object at all, or asking for a transfer with no end in the
/// object store is one this program answers nothing for.
fn requested(arguments: &[String]) -> Option<Requested> {
    let mut words = arguments.iter();
    if words.next()?.as_str() != S3_COMMAND_GROUP {
        return None;
    }
    let command = words.next()?.as_str();
    if !PERFORMED_S3_COMMANDS.contains(&command) {
        return None;
    }
    let operands: Vec<&String> = words.collect();
    if operands.iter().any(|operand| operand.starts_with('-')) {
        return None;
    }
    match command {
        "rm" => {
            let [target] = operands[..] else {
                return None;
            };
            Some(Requested::Remove {
                target: addressed_object(target)?,
            })
        }
        // `cp` and `mv` take the two ends, at least one of which is in the object store: a
        // transfer between two local paths reaches no store, so it is not a transfer this
        // program performs.
        _ => {
            let [from, to] = operands[..] else {
                return None;
            };
            let from = Location::of(from)?;
            let to = Location::of(to)?;
            if matches!(from, Location::Local(_)) && matches!(to, Location::Local(_)) {
                return None;
            }
            Some(Requested::Carry {
                from,
                to,
                taking_source_away: command == "mv",
            })
        }
    }
}

impl EnvironmentPrograms for StepObjectCli {
    /// The `aws s3` transfers above, performed against the object store; `None` for every
    /// other program name and every invocation [`requested`] answers nothing for, which the
    /// session reports as a command it could not run.
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        if name != OBJECT_CLI_PROGRAM {
            return None;
        }
        let requested = requested(arguments)?;
        Some(self.performed(&requested, files))
    }
}

impl StepObjectCli {
    /// Perform one asked-for transfer and answer what the command exits with and the line it
    /// wrote, the way the CLI reports one.
    fn performed(&self, requested: &Requested, files: &mut dyn ProgramFiles) -> ProgramOutcome {
        match requested {
            Requested::Remove { target } => match self.delete_object(target) {
                Ok(()) => reported(
                    TRANSFER_SUCCEEDED,
                    format!("delete: {}", object_shown(target)),
                ),
                Err(detail) => reported(
                    TRANSFER_FAILED,
                    format!("delete failed: {} {detail}", object_shown(target)),
                ),
            },
            Requested::Carry {
                from,
                to,
                taking_source_away,
            } => {
                // The word the CLI reports the transfer under: what it did with the file.
                let did = match (*taking_source_away, from, to) {
                    (true, _, _) => "move",
                    (false, Location::Local(_), _) => "upload",
                    (false, Location::Object(_), Location::Local(_)) => "download",
                    (false, _, _) => "copy",
                };
                let shown = format!("{} to {}", from.shown(), to.shown());
                if let Err(detail) = self.carried(from, to, files) {
                    return reported(TRANSFER_FAILED, format!("{did} failed: {shown} {detail}"));
                }
                // `mv` takes the source away once the destination holds it, so a move that
                // could not be completed leaves the file where it was.
                if *taking_source_away {
                    match from {
                        Location::Object(object) => {
                            if let Err(detail) = self.delete_object(object) {
                                return reported(
                                    TRANSFER_FAILED,
                                    format!("{did} failed: {shown} {detail}"),
                                );
                            }
                        }
                        Location::Local(path) => {
                            files.remove(path);
                        }
                    }
                }
                reported(TRANSFER_SUCCEEDED, format!("{did}: {shown}"))
            }
        }
    }

    /// Carry the file at `from` to `to`, or answer why it was not carried.
    ///
    /// An object carried to another object is copied in the store itself, which is the one
    /// transfer whose bytes never enter this process.
    fn carried(
        &self,
        from: &Location,
        to: &Location,
        files: &mut dyn ProgramFiles,
    ) -> Result<(), String> {
        match (from, to) {
            (Location::Object(source), Location::Object(destination)) => {
                self.copy_object(source, destination)
            }
            (Location::Local(path), Location::Object(destination)) => {
                let Some(contents) = files.read(path) else {
                    return Err(format!("The user-provided path {path} does not exist."));
                };
                self.put_object(destination, &contents)
            }
            (Location::Object(source), Location::Local(path)) => {
                let contents = self.get_object(source)?;
                if !files.write(path, &contents) {
                    return Err(format!(
                        "The file {path} was not written: it is past a bound this emulator \
                         gives the files one command may hold."
                    ));
                }
                Ok(())
            }
            // Filtered out by [`requested`]: a transfer with no end in the object store is
            // not one this program performs.
            (Location::Local(_), Location::Local(_)) => Err(
                "This command performs no transfer between two local paths.".to_string(),
            ),
        }
    }

    /// The Amazon S3 client this command's calls are made with: the in-process transport,
    /// signed as the role the step runs as where a session for it was minted.
    ///
    /// `Err` where the environment holds no credential: the CLI makes no call it has none for
    /// and reports [`NO_CREDENTIAL_LOCATED`], which is the failure of the transfer.
    fn client(&self) -> Result<::aws_sdk_s3::Client, String> {
        match &self.credential {
            HeldCredential::Session(identity) => {
                Ok(::service_common::s3_client::s3_client_signed_as(
                    &self.callback,
                    &self.region,
                    ::aws_sdk_s3::config::Credentials::new(
                        identity.access_key_id.clone(),
                        identity.secret_access_key.clone(),
                        Some(identity.session_token.clone()),
                        None,
                        STEP_CREDENTIAL_SOURCE,
                    ),
                ))
            }
            HeldCredential::Emulator => Ok(::service_common::s3_client::s3_client(
                &self.callback,
                &self.region,
            )),
            HeldCredential::Absent => Err(NO_CREDENTIAL_LOCATED.to_string()),
        }
    }

    /// Make ONE call, inside what this command has left of [`TRANSFER_BUDGET`].
    ///
    /// The thread this runs on is the one the step's command is performed on rather than a
    /// worker of the runtime, so the call is driven by blocking it on the runtime handle. The
    /// wait itself is built INSIDE that block, which is where this thread is in the runtime's
    /// own context and so where a timer of it can be armed at all.
    fn made<T, F: ::std::future::Future<Output = Result<T, String>>>(
        &self,
        call: impl FnOnce() -> F,
    ) -> Result<T, String> {
        let left = self
            .deadline
            .checked_duration_since(::std::time::Instant::now())
            .filter(|left| !left.is_zero());
        let Some(left) = left else {
            return Err(spent_budget());
        };
        self.runtime.block_on(async {
            match ::tokio::time::timeout(left, call()).await {
                Ok(answered) => answered,
                Err(_elapsed) => Err(spent_budget()),
            }
        })
    }

    /// The object's own text, read whole and bounded by [`MAX_TRANSFERRED_BYTES`].
    fn get_object(&self, address: &S3Object) -> Result<String, String> {
        self.made(|| async {
            let read = self
                .client()?
                .get_object()
                .bucket(&address.bucket)
                .key(&address.key)
                // RANGED, so an object past the bound is never buffered whole: the one byte
                // past it is what shows it is.
                .range(format!("bytes=0-{MAX_TRANSFERRED_BYTES}"))
                .send()
                .await
                .map_err(|error| store_answered("GetObject", &error))?;
            let bytes = read
                .body
                .collect()
                .await
                .map_err(|error| format!("The object did not arrive in full: {error}"))?
                .into_bytes();
            if bytes.len() > MAX_TRANSFERRED_BYTES {
                return Err(format!(
                    "The object is past the {MAX_TRANSFERRED_BYTES} bytes one transfer carries."
                ));
            }
            String::from_utf8(bytes.to_vec())
                .map_err(|_| "The object is not text, so no file was written.".to_string())
        })
    }

    /// Write `contents` as the object at `address`.
    fn put_object(&self, address: &S3Object, contents: &str) -> Result<(), String> {
        if contents.len() > MAX_TRANSFERRED_BYTES {
            return Err(format!(
                "The file is past the {MAX_TRANSFERRED_BYTES} bytes one transfer carries."
            ));
        }
        self.made(|| async {
            self.client()?
                .put_object()
                .bucket(&address.bucket)
                .key(&address.key)
                .body(::aws_sdk_s3::primitives::ByteStream::from(
                    contents.as_bytes().to_vec(),
                ))
                .send()
                .await
                .map(|_written| ())
                .map_err(|error| store_answered("PutObject", &error))
        })
    }

    /// Copy the object at `source` to `destination`, in the store itself.
    fn copy_object(
        &self,
        source: &S3Object,
        destination: &S3Object,
    ) -> Result<(), String> {
        self.made(|| async {
            self.client()?
                .copy_object()
                .bucket(&destination.bucket)
                .key(&destination.key)
                .copy_source(format!("{}/{}", source.bucket, source.key))
                .send()
                .await
                .map(|_copied| ())
                .map_err(|error| store_answered("CopyObject", &error))
        })
    }

    /// Delete the object at `address`.
    fn delete_object(&self, address: &S3Object) -> Result<(), String> {
        self.made(|| async {
            self.client()?
                .delete_object()
                .bucket(&address.bucket)
                .key(&address.key)
                .send()
                .await
                .map(|_deleted| ())
                .map_err(|error| store_answered("DeleteObject", &error))
        })
    }
}

/// What the command answered: a status and the one line it wrote about the transfer.
fn reported(status: i32, line: String) -> ProgramOutcome {
    ProgramOutcome {
        status,
        output: vec![line],
    }
}

/// What a call the object store refused reports, as the CLI words a service's refusal: "An
/// error occurred (<code>) when calling the <operation> operation: <message>".
fn store_answered<E: ProvideErrorMetadata>(operation: &str, error: &E) -> String {
    format!(
        "An error occurred ({}) when calling the {operation} operation: {}",
        error.code().unwrap_or("Unknown"),
        error.message().unwrap_or_default()
    )
}

/// What a transfer with nothing left of [`TRANSFER_BUDGET`] reports: the bound that stopped
/// it, named, rather than a store answer nothing received.
fn spent_budget() -> String {
    format!(
        "The transfer was stopped because this command's transfers ran past the \
         {TRANSFER_BUDGET:?} this emulator gives them."
    )
}

#[cfg(test)]
mod tests {
    use super::{requested, Location, Requested, S3Object};

    /// The words of a command line, as the session hands them to a program.
    fn words(line: &str) -> Vec<String> {
        ::shell_words::split(line).expect("the line lexes")
    }

    /// RULE: `aws s3 cp|mv|rm` over the operands those commands take is what this program
    /// performs, and every other invocation is one it answers nothing for — so the session
    /// reports it as a command it could not run rather than as a transfer.
    #[test]
    fn the_invocations_this_program_performs() {
        let object = |bucket: &str, key: &str| S3Object {
            bucket: bucket.to_string(),
            key: key.to_string(),
        };
        // (case, the arguments after `aws`, what it asks for)
        let rows: [(&str, &str, Option<Requested>); 13] = [
            (
                "an upload",
                "s3 cp /tmp/out.txt s3://reports/output/out.txt",
                Some(Requested::Carry {
                    from: Location::Local("/tmp/out.txt".to_string()),
                    to: Location::Object(object("reports", "output/out.txt")),
                    taking_source_away: false,
                }),
            ),
            (
                "a download",
                "s3 cp s3://reports/input/in.txt /tmp/in.txt",
                Some(Requested::Carry {
                    from: Location::Object(object("reports", "input/in.txt")),
                    to: Location::Local("/tmp/in.txt".to_string()),
                    taking_source_away: false,
                }),
            ),
            (
                "a copy between two objects",
                "s3 cp s3://reports/a s3://archive/b",
                Some(Requested::Carry {
                    from: Location::Object(object("reports", "a")),
                    to: Location::Object(object("archive", "b")),
                    taking_source_away: false,
                }),
            ),
            (
                "a move",
                "s3 mv /tmp/out.txt s3://reports/out.txt",
                Some(Requested::Carry {
                    from: Location::Local("/tmp/out.txt".to_string()),
                    to: Location::Object(object("reports", "out.txt")),
                    taking_source_away: true,
                }),
            ),
            (
                "a removal",
                "s3 rm s3://reports/out.txt",
                Some(Requested::Remove {
                    target: object("reports", "out.txt"),
                }),
            ),
            ("another command of the group", "s3 sync /tmp s3://reports", None),
            (
                "another command group",
                "ec2 describe-instances",
                None,
            ),
            (
                "an option, which changes what the command does",
                "s3 cp --recursive /tmp s3://reports/tree",
                None,
            ),
            (
                "a transfer between two local paths",
                "s3 cp /tmp/one /tmp/two",
                None,
            ),
            ("an address naming no object", "s3 cp /tmp/one s3://reports", None),
            (
                "an address naming a prefix",
                "s3 cp /tmp/one s3://reports/tree/",
                None,
            ),
            ("too few operands", "s3 cp /tmp/one", None),
            (
                "a Hadoop filesystem scheme, which these commands do not take",
                "s3 cp s3a://reports/a /tmp/one",
                None,
            ),
        ];
        for (case, line, expected) in rows {
            let asked = requested(&words(line));
            assert_eq!(
                asked, expected,
                "case {case:?}: expected {expected:?}, got {asked:?}"
            );
        }
    }
}
