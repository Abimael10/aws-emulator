// SPDX-License-Identifier: CC0-1.0

//! The PACKAGE QUERY client in the guest of a managed node: `rpm`, answered for the SSM Agent
//! package out of the build that node reports.
//!
//! # Why it is installed from here
//!
//! A client reconciling the agent versions of its fleet reads what Systems Manager reports for
//! a node (`InstanceInformation$AgentVersion`) and checks it against what the node's own
//! package database says is installed — `rpm -q amazon-ssm-agent`, run in the guest through
//! Run Command. Which build the guest holds is a fact THIS service keeps for the node
//! ([`super::agent_releases`]) and one the service that owns the guest cannot read, so this
//! crate installs the query program into the command's session beside the machine's own
//! programs ([`super::guest_execution::perform_in_guest`]) — the same way the resolver clients
//! are installed ([`super::guest_resolver`]) — rather than either service keeping a second
//! notion of which agent build a node runs.
//!
//! The machine's own programs are asked FIRST, so a package the service that owns the guest
//! holds a build of is answered out of that machine's live database and this client answers
//! about the agent alone — one program per holder, over the one shared grammar
//! ([`::service_common::guest_package_query`]).
//!
//! # What is real here
//!
//! The one package this client answers for is the agent this service publishes, and the
//! version it reports is the node's OWN reported build, read off the node record: the package
//! database of the guest and the inventory of the node cannot disagree, which is what makes
//! the reconciliation a client performs meaningful. Nothing here reads a package database of
//! the host, starts a process, or answers for a package whose version this service does not
//! hold — a query naming any other package is left to the machine's own programs, and an
//! invocation written in a form the shared grammar does not read is reported the way the
//! session reports a command it could not run.

use ::service_common::guest_package_query::{
    rendered_package, rpm_architecture, PackageQueryInvocation, QueriedPackage, QUERY_PROGRAM,
};
use ::service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles, ProgramOutcome};

/// The names of the shared query grammar a reader of this module reaches through it: the
/// program a guest queries its database with, the format a package renders under where the
/// invocation named none, and the architecture names the package tooling uses
/// ([`::service_common::guest_package_query`]).
pub use ::service_common::guest_package_query::{
    ARCHITECTURE_INDEPENDENT, DEFAULT_QUERY_FORMAT, EC2_ARM64, RPM_ARM64,
};

/// The package the SSM Agent is installed as, which is also the name of the init-system unit
/// it ships ("`amazon-ssm-agent`", the agent's own package name on the RPM-based Amazon Linux
/// distributions).
pub const AGENT_PACKAGE: &str = "amazon-ssm-agent";

/// The PACKAGE BUILD of one agent release, which `rpm` reports as a package's `RELEASE`: the
/// agent publishes one package per release, so every build of [`super::agent_releases`] is the
/// first package build of its own version.
pub const AGENT_PACKAGE_RELEASE: &str = "1";

/// The status the query exits with for a package it found.
const FOUND: i32 = 0;

/// The package-query client of the guest of ONE node.
///
/// Holds the facts of the agent package that node has installed and nothing else — no store
/// guard, no engine and no host handle is reachable from a program.
#[derive(Debug)]
pub struct PackageQuery {
    /// The agent package that node reports, as a query of it reports one.
    agent: QueriedPackage,
}

/// The query client for the guest of a node whose agent build is `version`, on a machine of
/// Amazon EC2's `architecture` (empty for a machine whose architecture this emulator does not
/// hold).
#[must_use]
pub fn agent_package_programs(version: &str, architecture: &str) -> Box<dyn EnvironmentPrograms> {
    Box::new(PackageQuery {
        agent: agent_package_row(version, architecture),
    })
}

/// The row the SSM AGENT is in that guest's package database.
#[must_use]
pub fn agent_package_row(version: &str, architecture: &str) -> QueriedPackage {
    QueriedPackage {
        name: AGENT_PACKAGE.to_string(),
        version: version.to_string(),
        release: AGENT_PACKAGE_RELEASE.to_string(),
        architecture: rpm_architecture(architecture),
    }
}

impl EnvironmentPrograms for PackageQuery {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        _files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        // A program named by its absolute path is the same program, as a shell resolving a
        // name on `PATH` finds.
        if name.rsplit('/').next().unwrap_or(name) != QUERY_PROGRAM {
            return None;
        }
        let invocation = PackageQueryInvocation::read(arguments)?;
        if !invocation.names(&self.agent.name) {
            // A query about any other package is one whose answer this service does not hold:
            // it is left to the machine's own programs rather than answered here.
            return None;
        }
        Some(ProgramOutcome {
            status: FOUND,
            output: rendered_package(&self.agent, invocation.format()),
        })
    }
}
