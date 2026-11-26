// SPDX-License-Identifier: CC0-1.0

//! The guest of a MicroVM whose image's command starts an INTERPRETER'S OWN HTTP
//! FILE-SERVER module.
//!
//! `CodeArtifact$uri` is only "The URI of the code artifact in Amazon S3": what runs inside
//! a MicroVM is whatever the image's container specification builds and starts, and nothing
//! in the model says that has to be a program of the artifact. A command that starts the
//! interpreter's own server module names no file of the artifact at all
//! ([`super::guest_program::GuestStart::FileServer`]) — what it serves is the machine's own
//! filesystem, which the image's build steps are what materialise
//! ([`super::image_build`], [`super::guest_machine`]).
//!
//! So that server is the guest here: a proxied request is answered out of the files the
//! MicroVM really holds, under the directory the command runs in and on the port the
//! invocation names. Nothing is synthesised — a path the machine holds nothing at is the
//! status that server answers for one, and a port nothing inside the MicroVM bound reaches
//! nothing there, exactly as it does for a guest the engine runs.
//!
//! What such a server answers for a request is NOT read twice in this workspace: it is
//! `::service_com_amazonaws_ec2_amazonec2::state::guest_services::served_from`, the one
//! reading an Amazon EC2 instance's own static server is answered through, performed here
//! over the MicroVM's machine — beside
//! `::service_common::static_web_servers::served_module`, the one reading of the invocation
//! that STARTS such a server, which Amazon ECS shares as well
//! ([`super::guest_program`]). A reading corrected there is corrected for each of them.

use ::service_common::posix_shell_session::ProgramFiles;
use ::service_common::static_web_servers::{ServedModule, HTTP_SERVER_MODULE};
use ::service_com_amazonaws_ec2_amazonec2::state::guest_services::{served_from, GuestServer};

use super::guest_machine::GuestMachine;
use super::guest_runtime::{GuestAnswer, GuestOutcome, GuestRequest, MAX_RESPONSE_BYTES};

/// The header a document is served with, as the server names its media type.
const CONTENT_TYPE_HEADER: &str = "content-type";

/// What the server `served` answers `request` with, out of the files `machine` holds.
///
/// A request addressing a port this server is not listening on reached nothing inside the
/// MicroVM, whatever else it holds — the same answer a guest whose program bound no server
/// on that port gives ([`super::guest_runtime`]).
#[must_use]
pub fn answer(
    machine: &GuestMachine,
    served: &ServedModule,
    request: &GuestRequest,
) -> GuestOutcome {
    if request.port != served.port {
        return GuestOutcome::NoResponse(format!(
            "nothing inside the MicroVM is listening on port {}",
            request.port
        ));
    }
    let server = GuestServer {
        program: HTTP_SERVER_MODULE.to_string(),
        port: served.port,
        document_root: served.document_root.clone(),
        // The module LISTS a directory that holds no index document, which is what
        // `SimpleHTTPRequestHandler` answers such a request with.
        lists_directories: true,
    };
    // `request.url` is the path the client appended to the published endpoint, which is the
    // GUEST's own path in full.
    let answered = served_from(&server, &MachineFiles(machine), &request.method, &request.url);
    if answered.body.len() > MAX_RESPONSE_BYTES {
        return GuestOutcome::NoResponse(format!(
            "the guest's response was larger than the {MAX_RESPONSE_BYTES} bytes one request \
             may carry"
        ));
    }
    GuestOutcome::Answered(GuestAnswer {
        status: answered.status,
        headers: answered
            .content_type
            .map(|media_type| vec![(CONTENT_TYPE_HEADER.to_string(), media_type)])
            .unwrap_or_default(),
        body: answered.body,
    })
}

/// The MicroVM's own filesystem, as the seam that reading reads it through.
///
/// Read-only: serving a request writes nothing and removes nothing, so the writing half of
/// the seam does nothing and says so. Nothing of the host is behind it — the tree is the
/// MicroVM's machine ([`super::guest_machine`]).
struct MachineFiles<'a>(&'a GuestMachine);

impl ProgramFiles for MachineFiles<'_> {
    fn read(&self, path: &str) -> Option<String> {
        self.0.read(path)
    }

    fn write(&mut self, _path: &str, _contents: &str) -> bool {
        false
    }

    fn remove(&mut self, _path: &str) -> bool {
        false
    }

    fn entries_in(&self, path: &str) -> Vec<String> {
        self.0.entry_names(path)
    }
}
