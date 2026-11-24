// SPDX-License-Identifier: CC0-1.0

//! The HTTP CLIENT in an instance's guest, reaching a server of the SAME machine through its
//! LOOPBACK interface.
//!
//! # Why a guest needs one
//!
//! A program that starts a server checks that the server answers before anything else uses it,
//! and the way it checks is to fetch a document from it over `127.0.0.1` — the one request a
//! machine can make entirely inside itself. A script delivered to the guest then reports what
//! came back (`StandardOutputContent` is what its commands wrote), so this leg is what makes
//! "the server I just started serves my document" observable to a client.
//!
//! # What is real here
//!
//! The request never leaves this process and nothing of the host is reached: the address is
//! only ever compared against the loopback interface of the machine the program is running ON,
//! and the answer is computed by that machine's OWN server out of that machine's OWN storage —
//! the one reading of a guest's static server this crate has
//! ([`super::guest_services::served_from`]), over the LIVE tree of the session the command runs
//! in, so the document a command of the same script just wrote is the document that comes back.
//! No socket is opened, no body is invented, and a port no server of this guest is listening on
//! is a connection that could not be made, reported the way the client reports one.
//!
//! The command line itself is read by the workspace's ONE reading of that client's invocation
//! ([`::service_common::instance_metadata_service::Invocation`]) — the same one a node's own
//! metadata read and a container's own health probe are performed through — and the statuses it
//! exits with are that client's documented ones, exported beside it. A request written in a form
//! that reading does not perform runs nothing, like every other command a guest's session cannot
//! perform. What the invocation asks the client to WRITE once the transfer is over is read by
//! that same shared reading ([`written_out`]), which spells the response code in the three-digit
//! form the client's own `http_code` documentation states.
//!
//! An address of any other host is not this module's: nothing of this guest can reach it, which
//! is what the transfer program itself answers for one ([`super::guest_agents::downloaded`]).
//! Neither is a form this client does not perform — a headers-only request, whose answer beside
//! the status and the media type is not a fact this emulator holds — which is reported as a
//! transfer that was not made rather than as a document.

use ::std::collections::BTreeMap;

use ::service_common::instance_metadata_service::{
    written_out, Invocation, TransferOutcome, COULD_NOT_CONNECT, HTTP_RETURNED_ERROR, WRITE_ERROR,
};
use ::service_common::posix_shell_session::{ProgramFiles, ProgramStreams};

use super::guest_agents::{machine_path, TRANSFER_PROGRAM};
use super::guest_services::{served_from, GuestHttpResponse, GuestServer};

/// The status the client exits with when it fetched what it was asked for.
const FETCHED: i32 = 0;

/// The status it exits with when it could not start the transfer it was asked for at all:
/// "2  Failed to initialize" (the curl manual's exit codes), which is what this client answers
/// for a form it does not perform.
const NOT_PERFORMED: i32 = 2;

/// The lowest status the server answers that is an error of the request or of the server, which
/// is what `--fail` reports rather than serving.
const FIRST_ERROR_STATUS: u16 = 400;

/// The port a request that names none is made to: the one the scheme states.
const DEFAULT_PORT: i32 = 80;

/// The scheme this client fetches over, which is what the guest's own servers serve.
const SCHEME: &str = "http";

/// The method the shared reading answers for an invocation asking for the headers alone, which
/// is the form this client performs nothing of.
const HEAD_METHOD: &str = "HEAD";

/// The name every machine resolves to its own loopback interface, and the names a machine of
/// this family publishes for the IPv6 one — written by its own `/etc/hosts`, which is why a
/// program reaches its own machine by them without any DNS being asked.
const LOOPBACK_NAMES: &[&str] = &["localhost", "localhost.localdomain", "ip6-localhost"];

/// The destination that names the client's own standard output rather than a file of the
/// machine, which is how a script pipes what it fetched into the command after it.
const STANDARD_OUTPUT: &str = "-";

/// What the client fetched from a server of its own machine, or `None` when the invocation
/// addresses no server of it — another host, another scheme, or a form the shared reading does
/// not perform — which is left to the program's own answer for an address this guest cannot
/// reach ([`super::guest_agents::downloaded`]).
///
/// `listening` is what the guest is listening with right now, and `files` the tree of the
/// session the command was written in, which is both where the answer is computed from and
/// where a saved answer is written.
#[must_use]
pub fn fetched(
    program: &str,
    arguments: &[String],
    files: &mut dyn ProgramFiles,
    listening: &BTreeMap<i32, GuestServer>,
) -> Option<ProgramStreams> {
    if program != TRANSFER_PROGRAM {
        return None;
    }
    let invocation = Invocation::read(arguments)?;
    let target = Target::read(&invocation.url)?;
    if invocation.method == HEAD_METHOD {
        // A headers-only request asks for what a server SAYS about a document beside the
        // document itself, and what a guest's server says beside its status and the media type
        // of what it serves is not a fact this emulator holds. The client reports a form it
        // did not perform rather than a document for a request it did not make.
        return Some(not_performed(
            &invocation,
            "this emulator's client does not report a server's own headers",
        ));
    }
    let Some(server) = listening.get(&target.port) else {
        // Nothing of this machine is listening there, so no connection could be made at all.
        return Some(could_not_connect(&invocation, &target));
    };
    let answered = served_from(server, files, &invocation.method, &target.path);
    Some(answer_of(&invocation, &target, &answered, files))
}

/// WHERE one request goes: a port of the machine the program is running on, and the path it
/// asks that machine's server for.
#[derive(Debug)]
struct Target {
    /// The host as the invocation wrote it, which is what the client names in its own
    /// diagnostic.
    host: String,
    /// The port the request is made to.
    port: i32,
    /// The path it asks for, with the query it carries.
    path: String,
}

impl Target {
    /// The target `url` names, or `None` when it names no server of THIS machine: another
    /// host, or a scheme this client does not fetch over.
    ///
    /// The address is read by the `http` crate's own URI type — the maintained parser this
    /// workspace already reads a node's own metadata address with
    /// ([`::service_common::instance_metadata_service::metadata_path`]) — rather than by
    /// anything written here. An address written with no scheme is fetched over the default
    /// one, as the client itself assumes.
    fn read(url: &str) -> Option<Self> {
        let written = if url.contains("://") {
            url.to_string()
        } else {
            format!("{SCHEME}://{url}")
        };
        let parsed = ::http::Uri::try_from(written.as_str()).ok()?;
        if parsed.scheme_str() != Some(SCHEME) {
            return None;
        }
        let host = parsed.host()?;
        if !is_own_machine(host) {
            return None;
        }
        Some(Self {
            host: host.to_string(),
            port: parsed.port_u16().map_or(DEFAULT_PORT, i32::from),
            path: parsed
                .path_and_query()
                .map_or_else(|| "/".to_string(), |path| path.as_str().to_string()),
        })
    }
}

/// Whether `host` addresses the machine the program is running ON through its loopback
/// interface: the loopback address block ("127.0.0.0/8 … loopback"), the IPv6 loopback address,
/// or one of the names a machine publishes for them ([`LOOPBACK_NAMES`]).
///
/// Nothing else is this module's: an address of another machine is a packet that leaves this
/// one, which no program of this guest makes.
fn is_own_machine(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.');
    if LOOPBACK_NAMES
        .iter()
        .any(|name| host.eq_ignore_ascii_case(name))
    {
        return true;
    }
    host.parse::<::std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

/// What the client answers for a form it does not perform: no document, the status it exits
/// with when it could not start a transfer, and the reason unless it was asked to say nothing.
fn not_performed(invocation: &Invocation, said: &str) -> ProgramStreams {
    ProgramStreams {
        status: NOT_PERFORMED,
        output: Vec::new(),
        errors: diagnostic(invocation, format!("({NOT_PERFORMED}) {said}")),
    }
}

/// What the client answers for a port nothing of this machine is listening on: no document, the
/// status it exits with for a connection it could not make, and the diagnostic it writes for one
/// unless it was asked to say nothing.
fn could_not_connect(invocation: &Invocation, target: &Target) -> ProgramStreams {
    ProgramStreams {
        status: COULD_NOT_CONNECT,
        output: Vec::new(),
        errors: diagnostic(
            invocation,
            format!(
                "({COULD_NOT_CONNECT}) Failed to connect to {} port {}: Connection refused",
                target.host, target.port
            ),
        ),
    }
}

/// The client's own diagnostic line, which it writes unless it was asked to say nothing.
fn diagnostic(invocation: &Invocation, said: String) -> Vec<String> {
    if invocation.silent {
        return Vec::new();
    }
    vec![format!("{TRANSFER_PROGRAM}: {said}")]
}

/// Where what was fetched is WRITTEN: the path of the machine the invocation named, and `None`
/// for an invocation that writes it to the client's own output.
fn written_at(invocation: &Invocation, target: &Target) -> Option<String> {
    if let Some(named) = &invocation.output {
        if named == STANDARD_OUTPUT {
            return None;
        }
        return machine_path(named);
    }
    if invocation.remote_name {
        let path = target.path.split(['?', '#']).next().unwrap_or(&target.path);
        let name = path.trim_end_matches('/').rsplit('/').next()?;
        return machine_path(name);
    }
    None
}

/// What the client answers for the response `answered`: the document where the invocation
/// writes it, the format it was asked to write, and the status it exits with.
fn answer_of(
    invocation: &Invocation,
    target: &Target,
    answered: &GuestHttpResponse,
    files: &mut dyn ProgramFiles,
) -> ProgramStreams {
    let mut output: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut status = FETCHED;
    let body = String::from_utf8_lossy(&answered.body).into_owned();
    if invocation.fail && answered.status >= FIRST_ERROR_STATUS {
        // "Fail silently (no output at all) on server errors": the document is not served at
        // all, and the client reports the status the server answered as its own failure.
        status = HTTP_RETURNED_ERROR;
        errors = diagnostic(
            invocation,
            format!(
                "({HTTP_RETURNED_ERROR}) The requested URL returned error: {}",
                answered.status
            ),
        );
    } else if let Some(path) = written_at(invocation, target) {
        // What was fetched goes in that file, so the client's own output carries nothing of it;
        // a path the machine's tree will not take is the client's own write failure.
        if !files.write(&path, &body) {
            status = WRITE_ERROR;
            errors = diagnostic(
                invocation,
                format!("({WRITE_ERROR}) Failure writing output to {path}"),
            );
        }
    } else {
        output.extend(lines_of(&body));
    }
    if let Some(format) = &invocation.write_out {
        output.extend(lines_of(&written_out(format, &transfer_outcome(answered))));
    }
    ProgramStreams {
        status,
        output,
        errors,
    }
}

/// The lines `written` carries, as the session writes a program's output line by line: a text
/// ending in a line break describes no line after the break, and a text with no break at all is
/// still the one line it wrote.
fn lines_of(written: &str) -> Vec<String> {
    let mut lines: Vec<String> = written.split('\n').map(str::to_string).collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// What this request came to, as the variables a write-out format names stand for.
fn transfer_outcome(answered: &GuestHttpResponse) -> TransferOutcome {
    TransferOutcome {
        status: Some(answered.status),
        size: answered.body.len(),
        content_type: answered.content_type.clone(),
    }
}
