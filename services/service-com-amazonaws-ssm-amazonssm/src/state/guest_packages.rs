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
//! # What is real here
//!
//! The one package this client answers for is the agent this service publishes, and the
//! version it reports is the node's OWN reported build, read off the node record: the package
//! database of the guest and the inventory of the node cannot disagree, which is what makes
//! the reconciliation a client performs meaningful. Nothing here reads a package database of
//! the host, starts a process, or answers for a package whose version this service does not
//! hold — a query naming any other package is left to the machine's own programs, and an
//! invocation written in a form this client does not read is reported the way the session
//! reports a command it could not run.

use ::service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles, ProgramOutcome};

/// The program a guest of this platform queries its package database with.
pub const QUERY_PROGRAM: &str = "rpm";

/// The package the SSM Agent is installed as, which is also the name of the init-system unit
/// it ships ("`amazon-ssm-agent`", the agent's own package name on the RPM-based Amazon Linux
/// distributions).
pub const AGENT_PACKAGE: &str = "amazon-ssm-agent";

/// The PACKAGE BUILD of one agent release, which `rpm` reports as a package's `RELEASE`: the
/// agent publishes one package per release, so every build of [`super::agent_releases`] is the
/// first package build of its own version.
pub const AGENT_PACKAGE_RELEASE: &str = "1";

/// The architecture `rpm` names the one Amazon EC2 reports as `arm64`: the same machines, under
/// the name the package tooling of these distributions uses.
pub const RPM_ARM64: &str = "aarch64";

/// The architecture name Amazon EC2 reports for those machines (`Image$Architecture`).
pub const EC2_ARM64: &str = "arm64";

/// What a package built for no particular machine reports as its architecture, which is what a
/// package of a machine whose own architecture this emulator does not hold is reported as.
pub const ARCHITECTURE_INDEPENDENT: &str = "noarch";

/// The format `rpm` renders a queried package with where the invocation named none:
/// `name-version-release.architecture`, the form `rpm -q` writes one line of per package.
pub const DEFAULT_QUERY_FORMAT: &str = "%{NAME}-%{VERSION}-%{RELEASE}.%{ARCH}";

/// The status the query exits with for a package it found.
const FOUND: i32 = 0;

/// How many bytes ONE invocation writes, however long a format the caller wrote: the format is
/// a caller's text, so what one command can make this process build is bounded here.
const MAX_OUTPUT_BYTES: usize = 4 * 1024;

/// The package-query client of the guest of ONE node.
///
/// Holds the facts of the agent package that node has installed and nothing else — no store
/// guard, no engine and no host handle is reachable from a program.
#[derive(Debug)]
pub struct PackageQuery {
    /// The agent build the node reports, which is the version installed in its guest.
    version: String,
    /// The architecture the installed package was built for, as `rpm` names one.
    architecture: String,
}

/// The query client for the guest of a node whose agent build is `version`, on a machine of
/// Amazon EC2's `architecture` (empty for a machine whose architecture this emulator does not
/// hold).
#[must_use]
pub fn agent_package_programs(version: &str, architecture: &str) -> Box<dyn EnvironmentPrograms> {
    Box::new(PackageQuery {
        version: version.to_string(),
        architecture: match architecture.trim() {
            "" => ARCHITECTURE_INDEPENDENT.to_string(),
            EC2_ARM64 => RPM_ARM64.to_string(),
            held => held.to_string(),
        },
    })
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
        let invocation = Invocation::read(arguments)?;
        if !invocation.names_agent_package() {
            // A query about any other package is one whose answer this service does not hold:
            // it is left to the machine's own programs rather than answered here.
            return None;
        }
        Some(ProgramOutcome {
            status: FOUND,
            output: self.rendered(invocation.format.as_deref().unwrap_or(DEFAULT_QUERY_FORMAT)),
        })
    }
}

impl PackageQuery {
    /// The lines one queried package renders to under `format`: the format's own text with
    /// every tag it names replaced by that package's value for it, split where the format
    /// breaks a line.
    fn rendered(&self, format: &str) -> Vec<String> {
        let mut rendered = String::new();
        let mut rest = format;
        while let Some(start) = rest.find("%{") {
            if rendered.len() + start > MAX_OUTPUT_BYTES {
                break;
            }
            rendered.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else {
                // A tag the format never closes is text, which is what rpm writes back for a
                // format it cannot read as a tag.
                rendered.push_str(&rest[start..]);
                rest = "";
                break;
            };
            // A tag this package has no value for renders as nothing, the way rpm renders a
            // tag the package does not carry.
            if let Some(value) = self.tag(&after[..end]) {
                rendered.push_str(&value);
            }
            rest = &after[end + 1..];
        }
        rendered.push_str(rest);
        if rendered.len() > MAX_OUTPUT_BYTES {
            let mut at = MAX_OUTPUT_BYTES;
            while at > 0 && !rendered.is_char_boundary(at) {
                at -= 1;
            }
            rendered.truncate(at);
        }
        // The format's own escapes are what break its output into lines: rpm reads `\n` and
        // `\t` in a query format as the characters they name.
        let mut lines: Vec<String> = rendered
            .replace("\\t", "\t")
            .split("\\n")
            .map(str::to_string)
            .collect();
        // A format ending in a line break describes no line after the break.
        if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        lines
    }

    /// This package's value for the query tag `tag` names, or `None` for a tag it carries no
    /// value for.
    ///
    /// The names are the ones `rpm` documents for its query format, read case-insensitively as
    /// rpm reads them.
    fn tag(&self, tag: &str) -> Option<String> {
        let name = tag.trim().to_ascii_uppercase();
        let evr = || format!("{}-{AGENT_PACKAGE_RELEASE}", self.version);
        let nvr = || format!("{AGENT_PACKAGE}-{}", evr());
        match name.as_str() {
            "NAME" => Some(AGENT_PACKAGE.to_string()),
            "VERSION" => Some(self.version.clone()),
            "RELEASE" => Some(AGENT_PACKAGE_RELEASE.to_string()),
            "ARCH" | "ARCHITECTURE" => Some(self.architecture.clone()),
            "EVR" => Some(evr()),
            "NVR" => Some(nvr()),
            "NVRA" => Some(format!("{}.{}", nvr(), self.architecture)),
            _ => None,
        }
    }
}

/// One invocation of the query client, as its words state it.
#[derive(Debug, Default)]
struct Invocation {
    /// The packages it names, each as the caller wrote it — a name or a glob.
    patterns: Vec<String>,
    /// Whether it queries EVERY installed package (`-a`, `--all`), which names no package of
    /// its own.
    all: bool,
    /// The format it renders each package with (`--queryformat`, `--qf`), where it named one.
    format: Option<String>,
}

impl Invocation {
    /// The invocation `arguments` state, or `None` for one written in a form this client does
    /// not read — anything that is not a QUERY, and any option it performs nothing of.
    fn read(arguments: &[String]) -> Option<Self> {
        let mut invocation = Self::default();
        let mut query = false;
        let mut words = arguments.iter().map(String::as_str);
        while let Some(word) = words.next() {
            if let Some(long) = word.strip_prefix("--") {
                let (option, inline) = match long.split_once('=') {
                    Some((option, value)) => (option, Some(value)),
                    None => (long, None),
                };
                match option {
                    "query" => query = true,
                    "all" => invocation.all = true,
                    "queryformat" | "qf" => {
                        let value = match inline {
                            Some(value) => value,
                            // The format is the word after the option, which an invocation
                            // that ends on the option never wrote.
                            None => words.next()?,
                        };
                        invocation.format = Some(value.to_string());
                    }
                    // Every other option is one this client performs nothing of.
                    _ => return None,
                }
                continue;
            }
            if let Some(short) = word.strip_prefix('-').filter(|short| !short.is_empty()) {
                for letter in short.chars() {
                    match letter {
                        'q' => query = true,
                        'a' => invocation.all = true,
                        _ => return None,
                    }
                }
                continue;
            }
            invocation.patterns.push(word.to_string());
        }
        // Only a QUERY is read here, and a query has to name what it is about: rpm reports an
        // invocation naming neither a package nor every package as its own failure, which is
        // not a form this client performs.
        (query && (invocation.all || !invocation.patterns.is_empty())).then_some(invocation)
    }

    /// Whether this invocation asks about the agent package: every installed package, or a
    /// name or glob the agent's package name satisfies.
    fn names_agent_package(&self) -> bool {
        self.all && self.patterns.is_empty()
            || self
                .patterns
                .iter()
                .any(|pattern| matches_pattern(pattern, AGENT_PACKAGE))
    }
}

/// Whether the package name `name` satisfies the glob `pattern` a query named it by: `*`
/// stands for any run of characters, as a package glob's does, and every other character
/// stands for itself.
fn matches_pattern(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.trim().split('*');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let mut last: Option<&str> = None;
    for part in parts {
        if let Some(previous) = last.replace(part) {
            // A part between two wildcards may sit anywhere after what matched before it.
            match rest.find(previous) {
                Some(at) => rest = &rest[at + previous.len()..],
                None => return false,
            }
        }
    }
    match last {
        // The pattern ended with a wildcard's trailing text, which the name has to end with.
        Some(tail) => rest.len() >= tail.len() && rest.ends_with(tail),
        // The pattern carried no wildcard at all, so it is the whole name.
        None => rest.is_empty(),
    }
}
