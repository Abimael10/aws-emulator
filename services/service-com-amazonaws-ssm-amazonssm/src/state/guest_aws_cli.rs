// SPDX-License-Identifier: CC0-1.0

//! The AWS CLI in the guest of a managed node: the `aws s3` transfers and the `aws s3api`
//! object calls a command's script asks for, really performed against this emulator's own
//! Amazon S3 as the role the instance runs as.
//!
//! # Why the guest has one
//!
//! "The AWS CLI is preinstalled on Amazon Linux 2023" (Amazon Linux 2023 User Guide), and the
//! universal shape of a Run Command script that moves data is a line that invokes it:
//!
//! ```text
//! printf '%s' "$PAYLOAD" > /tmp/payload
//! aws s3api put-object --bucket example --key under/it --body /tmp/payload --region us-east-1
//! ```
//!
//! A guest that provides no such program answers that line the way a shell answers a utility it
//! could not find, so a client that drives a machine by shipping it a script reads a failed
//! invocation instead of its script's output and its effects — and the WRITE the script was sent
//! to make never happens, which is the one observation point a control on the machine's network
//! can be seen through at all.
//!
//! # What this module adds, and what it reuses
//!
//! What one command line ASKS for, and the calls that carry it, are the workspace's shared
//! reading of that program ([`::service_common::aws_s3_cli`]) — the same one every other service
//! that runs a caller-supplied command line uses, so two of them cannot carry a different file
//! or answer a different status. What this module adds is the GUEST's own half:
//!
//! * the FILES a local path names, which are the machine's own storage — the tree the script's
//!   own redirections wrote and its `cat` reads ([`ProgramFiles`]);
//! * WHO the calls are made by: the session of the role the instance profile carries, minted for
//!   the machine by the service that owns instances, so Amazon S3 judges a write from this guest
//!   against the policies in force on that role exactly as it judges any other signed request.
//!   A machine holding no such credential makes no call and reports
//!   [`::service_common::aws_s3_cli::NO_CREDENTIAL_LOCATED`], which is what the CLI reports when
//!   it can locate none;
//! * WHAT the machine's own NETWORK carries. A call out of the guest travels through the VPC
//!   endpoints of the instance's VPC, and a call their policies do not allow is refused by the
//!   service however the role's policies read ([`::service_common::vpc_endpoint_calls`], where
//!   that decision lives) — the same control the guest's own interpreter is held to
//!   ([`super::guest_python`]), so a script reaches Amazon S3 on one set of terms whichever
//!   program it reaches it with. The CLI reports such a refusal exactly as it reports one the
//!   service worded itself, and exits with the status it gives a service error.
//!
//! Every call goes over the framework's in-process callback, through this workspace's one
//! construction of that client ([`::service_common::s3_client`]), so the object a script wrote is
//! the object a later `GetObject` of that bucket reads. Nothing here starts a process, opens a
//! socket or touches a host path, and the credential is held by host code: it is never put in the
//! session's variables, in its storage or in anything the script writes.

use ::std::sync::Arc;
use ::std::time::Duration;

use ::service_common::aws_s3_cli::{
    self as cli, ApiOperation, Location, ObjectAddress, Requested, API_COMMAND_ERROR,
    API_SERVICE_ERROR, CLI_PROGRAM, NO_CREDENTIAL_LOCATED, TRANSFER_FAILED, TRANSFER_SUCCEEDED,
};
use ::service_common::posix_shell_session::{
    EnvironmentPrograms, ProgramFiles, ProgramInvocation, ProgramOutcome, ProgramStreams,
};
use ::service_common::vpc_endpoint_calls::CallEgress;

/// How many bytes ONE transfer of a guest's CLI carries.
///
/// CHOSEN, not an AWS limit: the bytes are a client's object or a file of the machine's own
/// storage, so what one transfer takes into this process is bounded here rather than by the
/// object's size. It is the bound the other sessions of this workspace give one
/// (`…ecs…::state::container_aws_cli::MAX_TRANSFERRED_BYTES`), and a transfer carries TEXT only,
/// because the storage these sessions hold holds text.
pub const MAX_TRANSFERRED_BYTES: usize = 256 * 1024;

/// The wall clock ONE command's calls may spend.
///
/// CHOSEN: the call is in-process, so a command whose call cannot be answered reports that bound
/// rather than being left outstanding for the rest of the script's run.
pub const CALL_BUDGET: Duration = Duration::from_secs(5);

/// The provider name the credential a call is signed with carries: it names this emulator rather
/// than any AWS credential source, and never travels on the wire.
const GUEST_CREDENTIAL_SOURCE: &str = "aws-ec2-instance-profile";

/// The service token the calls of this program address, which is the namespace a control on the
/// machine's network decides them under.
const S3_SERVICE: &str = "s3";

/// The AWS CLI one managed node's guest provides.
///
/// Built once per delivery ([`super::guest_execution::delivery_in_guest`]) and shared with the
/// session performing the script, which is why it holds no mutable state of its own.
#[derive(Clone)]
pub struct GuestAwsCli {
    /// The in-process callback every call is dispatched through.
    callback: ::smithy_server_runtime::EmbeddedEmulator,
    /// The async runtime the dispatch is driven on. The script runs on a thread of its own, not
    /// on a worker of this runtime, so a call is driven by blocking that thread on this handle.
    runtime: ::tokio::runtime::Handle,
    /// The Region a command line naming none makes its calls in, which is the Region the command
    /// was sent to.
    region: String,
    /// What every call PRESENTS: the session of the role the instance profile carries. `None` for
    /// a machine this emulator minted none for, whose CLI then locates no credential.
    credentials: Option<::aws_sdk_s3::config::Credentials>,
    /// What decides whether a call of this program leaves the machine's network at all
    /// ([`::service_common::vpc_endpoint_calls`]). `None` for a machine with no such control in
    /// force on it.
    egress: Option<Arc<dyn CallEgress>>,
}

/// Prints the Region this CLI is addressed to and NOTHING of the credential it signs with, so no
/// part of that credential can reach a log through a debug form of this type or of anything
/// holding one.
impl ::std::fmt::Debug for GuestAwsCli {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("GuestAwsCli")
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

/// The AWS CLI for the guest of a managed node, as the programs its environment provides.
///
/// `access_key_id`, `secret_access_key` and `session_token` are the session the service that owns
/// the instance minted for it; `egress` is what its network carries. `None` where there is no
/// runtime to drive a call on, which is a guest whose session then provides no such program at
/// all rather than one whose calls go nowhere.
#[must_use]
pub fn aws_cli_programs(
    callback: ::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    credentials: Option<(String, String, String)>,
    egress: Option<Arc<dyn CallEgress>>,
) -> Option<Box<dyn EnvironmentPrograms>> {
    let runtime = ::tokio::runtime::Handle::try_current().ok()?;
    Some(Box::new(GuestAwsCli {
        callback,
        runtime,
        region: region.to_string(),
        credentials: credentials.map(|(access_key_id, secret_access_key, session_token)| {
            ::aws_sdk_s3::config::Credentials::new(
                access_key_id,
                secret_access_key,
                Some(session_token),
                None,
                GUEST_CREDENTIAL_SOURCE,
            )
        }),
        egress,
    }))
}

impl EnvironmentPrograms for GuestAwsCli {
    /// The invocation performed for a caller that asks this form: reporting what the command
    /// wrote to standard output alone.
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let invocation = ProgramInvocation {
            name,
            arguments,
            standard_input: None,
            working_directory: "",
            environment: &::std::collections::BTreeMap::new(),
        };
        self.run_invocation(&invocation, files)
            .map(|streams| ProgramOutcome {
                status: streams.status,
                output: streams.output,
            })
    }

    fn run_invocation(
        &self,
        invocation: &ProgramInvocation<'_>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramStreams> {
        // A program named by its absolute path is the same program, as a shell resolving a name
        // on `PATH` finds.
        let name = invocation
            .name
            .rsplit('/')
            .next()
            .unwrap_or(invocation.name);
        if name != CLI_PROGRAM {
            return None;
        }
        // The Region the calls go to is the one the command line names, and the Region the
        // command was sent to for a line naming none — the CLI's own reading of that option.
        let region = cli::requested_region(invocation.arguments).unwrap_or_else(|| self.region.clone());
        if let Some(call) = cli::requested_api(invocation.arguments) {
            return Some(self.performed_api(&call.operation, &region, files));
        }
        // Every other invocation of this program — another command group, another command of
        // these groups, a form they do not take — is one this guest performs nothing of, which
        // the session reports as a command it could not run.
        let requested = cli::requested(invocation.arguments)?;
        Some(self.performed_transfer(&requested, &region, invocation.standard_input, files))
    }
}

impl GuestAwsCli {
    /// Perform one LOW-LEVEL call (`aws s3api …`) and answer what the CLI reports for it: the
    /// response document on standard output, or the refusal on standard error with the status a
    /// refusal gives the command ([`API_SERVICE_ERROR`]).
    fn performed_api(
        &self,
        operation: &ApiOperation,
        region: &str,
        files: &mut dyn ProgramFiles,
    ) -> ProgramStreams {
        let answered = match operation {
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
                        // A local path that is not there is the CLI's own failure, in the words
                        // it words a path it was pointed at and could not read.
                        None => {
                            return failed(
                                API_COMMAND_ERROR,
                                &format!("The user-provided path {path} does not exist."),
                            )
                        }
                    },
                    None => String::new(),
                };
                self.carried_out(operation, target, region, |client| async move {
                    cli::put_object_api(
                        &client,
                        target,
                        &contents,
                        content_type.as_deref(),
                        MAX_TRANSFERRED_BYTES,
                    )
                    .await
                })
            }
            ApiOperation::GetObject { source, outfile } => {
                match self.carried_out(operation, source, region, |client| async move {
                    cli::get_object_api(&client, source, MAX_TRANSFERRED_BYTES).await
                }) {
                    // The bytes go to the local path the command named, and the document the CLI
                    // prints is what the service answered about the object.
                    Ok((contents, document)) => {
                        if files.write(outfile, &contents) {
                            Ok(document)
                        } else {
                            return failed(
                                API_COMMAND_ERROR,
                                &format!("The file {outfile} was not written."),
                            );
                        }
                    }
                    Err(detail) => Err(detail),
                }
            }
            ApiOperation::HeadObject { target } => {
                self.carried_out(operation, target, region, |client| async move {
                    cli::head_object_api(&client, target).await
                })
            }
            ApiOperation::DeleteObject { target } => self
                .carried_out(operation, target, region, |client| async move {
                    cli::delete_object(&client, target).await.map(|()| String::new())
                }),
        };
        match answered {
            Ok(document) => ProgramStreams {
                status: TRANSFER_SUCCEEDED,
                output: lines(&document),
                errors: Vec::new(),
            },
            Err(detail) => failed(API_SERVICE_ERROR, &detail),
        }
    }

    /// Perform one asked-for TRANSFER (`aws s3 cp|mv|rm …`) and answer what the CLI reports for
    /// it.
    fn performed_transfer(
        &self,
        requested: &Requested,
        region: &str,
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> ProgramStreams {
        let word = requested.reported_word();
        let shown = requested.shown();
        match self.transferred(requested, region, standard_input, files) {
            // A download to the command's own standard OUTPUT writes the object's bytes there and
            // nothing else: they are what the stage after it reads.
            Ok(Some(text)) => ProgramStreams {
                status: TRANSFER_SUCCEEDED,
                output: lines(&text),
                errors: Vec::new(),
            },
            Ok(None) => ProgramStreams {
                status: TRANSFER_SUCCEEDED,
                output: vec![format!("{word}: {shown}")],
                errors: Vec::new(),
            },
            // The CLI reports a transfer it could not make on its standard error, naming what it
            // was doing, the two ends and what stopped it.
            Err(detail) => failed(TRANSFER_FAILED, &format!("{word} failed: {shown} {detail}")),
        }
    }

    /// The transfer itself; `Ok(Some(text))` is one whose destination is the command's own
    /// standard output.
    fn transferred(
        &self,
        requested: &Requested,
        region: &str,
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Result<Option<String>, String> {
        let (from, to, taking_source_away) = match requested {
            Requested::Remove { target } => {
                self.carried_out(
                    &ApiOperation::DeleteObject {
                        target: target.clone(),
                    },
                    target,
                    region,
                    |client| async move { cli::delete_object(&client, target).await },
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
        // `mv` takes the source away once the destination holds it, so a move that could not be
        // completed leaves the file where it was.
        if taking_source_away {
            match from {
                Location::Object(object) => {
                    self.carried_out(
                        &ApiOperation::DeleteObject {
                            target: object.clone(),
                        },
                        object,
                        region,
                        |client| async move {
                            cli::delete_object(&client, object).await
                        },
                    )?;
                }
                Location::Local(path) => {
                    files.remove(path);
                }
                // Filtered out by the shared parse: `mv` takes no stream operand.
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
                // A copy READS the source and WRITES the destination, so both ends are decided:
                // the read of the source first, then the write.
                self.admits(
                    &ApiOperation::GetObject {
                        source: source.clone(),
                        outfile: String::new(),
                    },
                    source,
                    region,
                )?;
                self.carried_out(
                    &ApiOperation::PutObject {
                        target: destination.clone(),
                        body: None,
                        content_type: None,
                    },
                    destination,
                    region,
                    |client| async move { cli::copy_object(&client, source, destination).await },
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
                    return Err(format!("The file {path} was not written."));
                }
                Ok(None)
            }
            // An upload FROM the command's own standard input carries what the stage before it
            // wrote. A command given no standard input at all carries nothing, so nothing is
            // written: an empty object would be a write the command never made.
            (Location::Stream, Location::Object(destination)) => {
                let contents = standard_input
                    .ok_or_else(|| "The command was given no standard input to upload.".to_string())?;
                self.put(destination, contents, region)?;
                Ok(None)
            }
            // A download TO it answers the object's own text, which the caller writes onto that
            // stream.
            (Location::Object(source), Location::Stream) => self.get(source, region).map(Some),
            // Filtered out by the shared parse: a transfer with no end in the object store is not
            // one this program performs.
            (Location::Local(_) | Location::Stream, Location::Local(_) | Location::Stream) => {
                Err("This command performs no transfer that does not reach Amazon S3.".to_string())
            }
        }
    }

    /// The object's own text, bounded by [`MAX_TRANSFERRED_BYTES`].
    fn get(&self, address: &ObjectAddress, region: &str) -> Result<String, String> {
        self.carried_out(
            &ApiOperation::GetObject {
                source: address.clone(),
                outfile: String::new(),
            },
            address,
            region,
            |client| async move { cli::get_object_text(&client, address, MAX_TRANSFERRED_BYTES).await },
        )
    }

    /// Write `contents` as the object at `address`, up to the same bound.
    fn put(&self, address: &ObjectAddress, contents: &str, region: &str) -> Result<(), String> {
        self.carried_out(
            &ApiOperation::PutObject {
                target: address.clone(),
                body: None,
                content_type: None,
            },
            address,
            region,
            |client| async move {
                cli::put_object_text(&client, address, contents, MAX_TRANSFERRED_BYTES).await
            },
        )
    }

    /// Make ONE call of `operation` on `address`: whether the machine's own network carries it
    /// ([`Self::admits`]), then the call itself, inside [`CALL_BUDGET`].
    ///
    /// The call is DRIVEN on the async runtime by blocking this thread on it, which is the thread
    /// the script runs on and never a worker of that runtime.
    fn carried_out<T, F, C>(
        &self,
        operation: &ApiOperation,
        address: &ObjectAddress,
        region: &str,
        call: C,
    ) -> Result<T, String>
    where
        C: FnOnce(::aws_sdk_s3::Client) -> F,
        F: ::std::future::Future<Output = Result<T, String>>,
    {
        self.admits(operation, address, region)?;
        let client = self.client(region)?;
        self.runtime.block_on(async move {
            match ::tokio::time::timeout(CALL_BUDGET, call(client)).await {
                Ok(answered) => answered,
                Err(_elapsed) => Err(format!(
                    "The command was stopped because its calls ran past the {CALL_BUDGET:?} this \
                     emulator gives them."
                )),
            }
        })
    }

    /// Whether the machine's own network carries a call of `operation` on `address` at all — and
    /// the sentence the CLI reports where it does not, which is the refusal's own code and
    /// message in the words the CLI reports a service's refusal in
    /// ([`::service_common::aws_s3_cli::service_answered`]).
    fn admits(
        &self,
        operation: &ApiOperation,
        address: &ObjectAddress,
        region: &str,
    ) -> Result<(), String> {
        let Some(egress) = self.egress.as_ref() else {
            return Ok(());
        };
        // What the request SAYS, as the decision reads a request: the members that name the
        // resource it addresses.
        let body = ::serde_json::json!({ "Bucket": address.bucket, "Key": address.key }).to_string();
        let Some(refused) = egress.refusal(S3_SERVICE, operation.api_name(), region, &body) else {
            return Ok(());
        };
        Err(cli::service_answered(
            operation.api_name(),
            &refused.code,
            &refused.message,
        ))
    }

    /// The Amazon S3 client this command's calls are made with, addressed to `region`: the
    /// in-process transport, signed as the session of the role the instance runs as.
    ///
    /// `Err` where the machine holds no credential at all: the CLI makes no call it has none for
    /// and reports [`NO_CREDENTIAL_LOCATED`], which is the failure of the call.
    fn client(&self, region: &str) -> Result<::aws_sdk_s3::Client, String> {
        let credentials = self
            .credentials
            .clone()
            .ok_or_else(|| NO_CREDENTIAL_LOCATED.to_string())?;
        Ok(::service_common::s3_client::s3_client_signed_as(
            &self.callback,
            region,
            credentials,
        ))
    }
}

/// What the command reports for a call it could not make: the sentence on standard error and the
/// status the CLI exits with, and nothing on standard output — no output is written for work
/// nothing performed.
fn failed(status: i32, detail: &str) -> ProgramStreams {
    ProgramStreams {
        status,
        output: Vec::new(),
        errors: lines(detail),
    }
}

/// `written` as the LINES the session takes it in, with nothing reported for an empty answer.
fn lines(written: &str) -> Vec<String> {
    written
        .trim_end_matches('\n')
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}
