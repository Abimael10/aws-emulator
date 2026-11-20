// SPDX-License-Identifier: CC0-1.0

//! The AWS AGENTS one instance's guest can hold, and the programs the documented procedure
//! that installs one invokes.
//!
//! # Why a guest needs them
//!
//! An agent is a unit of the machine rather than a server on a port: the CodeDeploy agent
//! "must be installed and running on each instance" before that instance can take a
//! deployment, and the way a client proves it is there is to ask the machine's init system —
//! `systemctl is-active codedeploy-agent` run through Systems Manager Run Command. A guest
//! whose own program installed and started that unit therefore has to report it ACTIVE, and
//! one that never installed it has to report what the init system reports for a unit it does
//! not have. Nothing else in this crate could answer that: the services a guest was already
//! modelled with are the servers that LISTEN ([`super::guest_services`]), and an agent listens
//! on nothing.
//!
//! # What is real here
//!
//! The catalog below is what this guest can hold: the two AWS agents an instance installs, and
//! the packages the agents' own installation procedure names. Installing one records it on the
//! machine ([`super::guest_services::GuestRuntime::installed`]) and nothing else; the unit is
//! RUNNING only once something started it — the init system's control program, or the agent's
//! own installer. A unit this machine never installed is not active, and `systemctl` answers
//! for it the way it answers for a unit file it does not have.
//!
//! The documented procedure for the CodeDeploy agent is "download the agent's installer from
//! the Amazon S3 bucket for your Region, make it executable and run it" (the CodeDeploy user
//! guide's Amazon Linux and RHEL instructions), so this module also holds the three programs
//! that procedure invokes:
//!
//! * the DOWNLOADER ([`downloaded`]) — the agent's installer is an object of a public bucket
//!   AWS publishes it in ([`INSTALLER_BUCKET_PREFIX`]), which this emulator holds the identity
//!   of ([`AGENT_INSTALLER_FILE`]); an address of any other host is one this guest cannot
//!   reach, and the downloader reports that rather than inventing a document for it. Nothing
//!   here opens a socket or resolves a name.
//! * the FILE-MODE program ([`mode_changed`]) — this machine's tree keys a file by its path
//!   and holds no mode for it ([`::service_common::posix_shell_volume`]), so changing a mode
//!   changes nothing it holds; what the command reports is still the difference between a path
//!   the machine has and one it does not.
//! * the INSTALLER itself ([`installer_outcome`]) — a command naming a path whose contents are
//!   that artifact installs the agent and starts its unit, which is what running the installer
//!   does.
//!
//! Nothing is started, installed or reported for an agent nothing installed, and nothing here
//! starts a process or reaches a host path.

use ::service_common::posix_shell_session::{ProgramFiles, ProgramOutcome};

/// One agent a guest of this emulator can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestAgent {
    /// The name a package manager installs it under, which is also the name of the
    /// init-system unit it ships.
    pub name: &'static str,
    /// Whether this entry is an agent with a UNIT of its own, as against a package the
    /// agents' own installation procedure names (its runtime, its downloader) which ships no
    /// unit: `systemctl` answers about the first and knows nothing of the second.
    pub unit: bool,
}

/// The packages a guest of this emulator can install.
///
/// The two AWS agents an instance holds as units of its own — the CodeDeploy agent, whose
/// unit is `codedeploy-agent`, and the Systems Manager agent, whose unit is
/// `amazon-ssm-agent` — and beside them the packages the agents' documented installation
/// procedure installs first: `ruby`, the runtime the CodeDeploy agent is written for, and
/// `wget`, the downloader that fetches its installer ("install ruby and wget", the CodeDeploy
/// user guide's Amazon Linux instructions). A name that is not one of these is a package this
/// guest has nothing for.
pub const GUEST_AGENT_PACKAGES: &[GuestAgent] = &[
    GuestAgent {
        name: "codedeploy-agent",
        unit: true,
    },
    GuestAgent {
        name: "amazon-ssm-agent",
        unit: true,
    },
    GuestAgent {
        name: "ruby",
        unit: false,
    },
    GuestAgent {
        name: "wget",
        unit: false,
    },
];

/// The agent of the CodeDeploy family, whose unit the gate in front of every EC2 deployment
/// asks about.
pub const CODEDEPLOY_AGENT: &str = "codedeploy-agent";

/// The package `name` names, where this guest has one of that name.
///
/// The name is trimmed and an init-system unit's own suffix taken off it, so `codedeploy-agent`
/// and `codedeploy-agent.service` name the same package — the same reading
/// [`::service_common::static_web_servers::package_named`] takes of a web server's name.
#[must_use]
pub fn agent_named(name: &str) -> Option<&'static GuestAgent> {
    let named = name
        .trim()
        .trim_end_matches(::service_common::static_web_servers::UNIT_SUFFIX);
    GUEST_AGENT_PACKAGES
        .iter()
        .find(|package| package.name == named)
}

/// The agent UNIT `name` names, where this guest has one: a package that ships no unit is not
/// one the init system knows about.
#[must_use]
pub fn agent_unit_named(name: &str) -> Option<&'static GuestAgent> {
    agent_named(name).filter(|package| package.unit)
}

/// The name of the bucket family AWS publishes the CodeDeploy agent's installer in:
/// "aws-codedeploy-*region-identifier*" (the CodeDeploy user guide's resource-kit bucket
/// names), reached as `s3://aws-codedeploy-<region>/latest/install` or over that bucket's
/// HTTPS address.
pub const INSTALLER_BUCKET_PREFIX: &str = "aws-codedeploy-";

/// The key that bucket publishes the current installer at: "latest/install".
pub const INSTALLER_KEY: &str = "latest/install";

/// The ARTIFACT that address serves, as this emulator holds it: the identity of the CodeDeploy
/// agent's installer, which is what a machine that downloaded it holds at the path it saved it
/// to. Running a file holding this is running the installer ([`installer_outcome`]).
///
/// It is the artifact's identity rather than its program text: this emulator performs what the
/// installer DOES ([`installer_outcome`]) instead of interpreting a copy of its source.
pub const AGENT_INSTALLER_FILE: &str =
    "#!/usr/bin/env ruby\n# AWS CodeDeploy agent installer (aws-codedeploy-<region>/latest/install)\n";

/// The directory a path a program named WITHOUT one is under: whichever directory the session
/// that ran the command is in, which the session itself resolves such a path against
/// ([`::service_common::posix_shell_session`]) — so a program that changed directory and then
/// saved a file names the same file back by the name alone.
///
/// `named` as that program wrote it, with `./name` and `name` folded into one as a program
/// means them, and `None` for a word that names no path at all.
#[must_use]
pub fn machine_path(named: &str) -> Option<String> {
    let trimmed = named.trim();
    let folded = trimmed.strip_prefix("./").unwrap_or(trimmed);
    (!folded.is_empty()).then(|| folded.to_string())
}

/// The name a downloaded object is saved under where the command named no destination: the
/// last segment of the address, which is what both downloaders write.
#[must_use]
fn saved_as(address: &str) -> Option<String> {
    let path = address.split(['?', '#']).next().unwrap_or(address);
    let name = path.trim_end_matches('/').rsplit('/').next()?;
    (!name.is_empty() && !name.contains(':')).then(|| name.to_string())
}

/// Whether `address` names the CodeDeploy agent's installer in the bucket AWS publishes it in:
/// an `s3://` address of such a bucket, or the HTTPS address of one, whose key is the
/// installer's ([`INSTALLER_KEY`]).
#[must_use]
pub fn is_agent_installer_address(address: &str) -> bool {
    let address = address.split(['?', '#']).next().unwrap_or(address);
    let (host, path) = match address.split_once("://") {
        Some((_, rest)) => match rest.split_once('/') {
            Some((host, path)) => (host, path),
            None => (rest, ""),
        },
        None => return false,
    };
    let names_bucket = host.contains(INSTALLER_BUCKET_PREFIX)
        || path
            .split('/')
            .next()
            .is_some_and(|first| first.starts_with(INSTALLER_BUCKET_PREFIX));
    names_bucket && path.trim_end_matches('/').ends_with(INSTALLER_KEY)
}

/// The status the downloader exits with when the transfer was made.
const DOWNLOAD_SUCCEEDED: i32 = 0;

/// The status `wget` exits with for an address it could not reach: "4   Network failure" (the
/// GNU Wget manual's exit-status list), which is what a guest with no route to a host answers.
const WGET_NETWORK_FAILURE: i32 = 4;

/// The status `curl` exits with for the same: "6  Couldn't resolve host" (the curl manual's
/// exit codes).
const CURL_COULD_NOT_RESOLVE: i32 = 6;

/// The name of the downloader the CodeDeploy agent's documented installation procedure uses.
pub const DOWNLOAD_PROGRAM: &str = "wget";

/// The name of the transfer program a guest of this platform ships, which the same procedure
/// is also written with.
pub const TRANSFER_PROGRAM: &str = "curl";

/// The name of the program that changes a file's mode.
pub const MODE_PROGRAM: &str = "chmod";

/// One invocation of a downloader, as its words state it: the address it names and the path it
/// was told to save it under.
#[derive(Debug, Default)]
struct Download {
    /// The address the invocation names, empty where it named none.
    address: String,
    /// The path the answer is saved at, for an invocation that named one.
    destination: Option<String>,
    /// Whether the invocation asked for the address's own last segment as the destination
    /// (`curl -O`).
    remote_name: bool,
}

/// The options either downloader takes a VALUE for that this program reads: where to save what
/// it fetched.
const DESTINATION_OPTIONS: &[&str] = &["-O", "--output-document", "-o", "--output"];

impl Download {
    /// `arguments` as one invocation, or `None` for a form this program does not perform: an
    /// option carrying a value it does not read, or more than one address.
    ///
    /// `remote_name_flag` is the valueless spelling of "save it under its own name", which
    /// `curl` writes `-O` and `wget` does not have — so for `wget` the same word is the option
    /// that CARRIES the destination.
    fn read(arguments: &[String], remote_name_flag: bool) -> Option<Self> {
        let mut download = Self::default();
        let mut words = arguments.iter();
        while let Some(word) = words.next() {
            if !word.starts_with('-') {
                if !download.address.is_empty() {
                    return None;
                }
                download.address = word.clone();
                continue;
            }
            if remote_name_flag && (word == "-O" || word == "--remote-name") {
                download.remote_name = true;
                continue;
            }
            let (option, value) = match word.starts_with("--").then(|| word.split_once('=')) {
                Some(Some((option, value))) => (option.to_string(), Some(value.to_string())),
                _ => (word.clone(), None),
            };
            if DESTINATION_OPTIONS.contains(&option.as_str()) {
                download.destination = Some(match value {
                    Some(value) => value,
                    None => words.next()?.clone(),
                });
                continue;
            }
            // Every other option is one that changes how much the program says about itself or
            // how long it would wait, neither of which changes what it fetched.
        }
        (!download.address.is_empty()).then_some(download)
    }

    /// The path what was fetched is saved at, as the machine's tree keys one: the destination
    /// the invocation named, else the address's own last segment.
    fn destination(&self) -> Option<String> {
        let named = match &self.destination {
            Some(named) => named.clone(),
            None => saved_as(&self.address)?,
        };
        machine_path(&named)
    }
}

/// What a downloader answers for `arguments`, or `None` for an invocation it performs nothing
/// of — which the session reports as a command it could not run.
///
/// `program` is the downloader the command named, which decides the status an address this
/// guest cannot reach answers with and whether `-O` carries a path or asks for the address's
/// own name. A fetch of the agent's installer writes that artifact at the path the invocation
/// saves to; every other address is one nothing of this guest can reach, which is a failure of
/// the transfer rather than a document.
#[must_use]
pub fn downloaded(
    program: &str,
    arguments: &[String],
    files: &mut dyn ProgramFiles,
) -> Option<ProgramOutcome> {
    let curl = program == TRANSFER_PROGRAM;
    let download = Download::read(arguments, curl)?;
    let unreachable = || {
        Some(ProgramOutcome {
            status: if curl {
                CURL_COULD_NOT_RESOLVE
            } else {
                WGET_NETWORK_FAILURE
            },
            output: Vec::new(),
        })
    };
    if !is_agent_installer_address(&download.address) {
        return unreachable();
    }
    // `curl` writes what it fetched to its own output unless it was told to save it, which is
    // what `-O` and `-o` are for; `wget` saves it either way.
    if curl && download.destination.is_none() && !download.remote_name {
        return Some(ProgramOutcome {
            status: DOWNLOAD_SUCCEEDED,
            output: AGENT_INSTALLER_FILE
                .lines()
                .map(str::to_string)
                .collect(),
        });
    }
    let destination = download.destination()?;
    if !files.write(&destination, AGENT_INSTALLER_FILE) {
        return unreachable();
    }
    Some(ProgramOutcome {
        status: DOWNLOAD_SUCCEEDED,
        output: Vec::new(),
    })
}

/// What the file-mode program answers for `arguments`: the machine's tree holds no mode, so a
/// path it HAS is a change that leaves nothing behind, and a path it does not is the command's
/// own failure, in the wording that program reports one in.
///
/// `None` for an invocation naming no path at all, which is a form this program performs
/// nothing of.
#[must_use]
pub fn mode_changed(arguments: &[String], files: &mut dyn ProgramFiles) -> Option<ProgramOutcome> {
    let mut named = arguments.iter().filter(|word| !word.starts_with('-'));
    // The first operand is the mode and the rest are the paths it applies to.
    let _mode = named.next()?;
    let paths: Vec<&String> = named.collect();
    if paths.is_empty() {
        return None;
    }
    let mut output: Vec<String> = Vec::new();
    let mut status = 0;
    for path in paths {
        let held = machine_path(path);
        let held = held.as_deref().unwrap_or_default();
        if files.read(held).is_none() && files.entries_in(held).is_empty() {
            output.push(format!(
                "chmod: cannot access '{path}': No such file or directory"
            ));
            status = 1;
        }
    }
    Some(ProgramOutcome { status, output })
}

/// What running the file `path` names answers, where that file is the agent's installer and
/// `install` is what installing it does — and `None` for every other file, which is not a
/// program this guest has.
///
/// Running the installer is what INSTALLS the agent: "run the installer", after which the
/// agent's unit is on the machine and running, which is why `install` both records the package
/// and starts its unit.
#[must_use]
pub fn installer_outcome(
    path: &str,
    files: &dyn ProgramFiles,
    install: impl FnOnce(&'static GuestAgent),
) -> Option<ProgramOutcome> {
    let named = machine_path(path)?;
    if files.read(&named).as_deref() != Some(AGENT_INSTALLER_FILE) {
        return None;
    }
    install(agent_unit_named(CODEDEPLOY_AGENT)?);
    Some(ProgramOutcome {
        status: 0,
        output: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree of its own for a case, so the programs above can be asked what they write.
    #[derive(Debug, Default)]
    struct Tree(::std::collections::BTreeMap<String, String>);

    impl ProgramFiles for Tree {
        fn read(&self, path: &str) -> Option<String> {
            self.0.get(path).cloned()
        }

        fn write(&mut self, path: &str, contents: &str) -> bool {
            self.0.insert(path.to_string(), contents.to_string());
            true
        }

        fn remove(&mut self, path: &str) -> bool {
            self.0.remove(path).is_some()
        }
    }

    /// RULE: the agent's installer is the object its own bucket publishes, and no other
    /// address names it.
    #[test]
    fn the_installer_address_is_the_agents_own_bucket() {
        // (the address, whether it names the installer)
        let rows: &[(&str, bool)] = &[
            (
                "https://aws-codedeploy-us-west-2.s3.us-west-2.amazonaws.com/latest/install",
                true,
            ),
            (
                "https://aws-codedeploy-eu-central-1.s3.amazonaws.com/latest/install",
                true,
            ),
            ("s3://aws-codedeploy-us-east-1/latest/install", true),
            // The same bucket, but an object that is not the installer.
            (
                "https://aws-codedeploy-us-west-2.s3.us-west-2.amazonaws.com/releases/codedeploy-agent.noarch.rpm",
                false,
            ),
            // Another host altogether, which this guest reaches nothing at.
            ("https://example.invalid/latest/install", false),
            ("not-an-address", false),
            ("", false),
        ];
        for (address, expected) in rows {
            let named = is_agent_installer_address(address);
            assert!(
                named == *expected,
                "case {address:?}: expected {expected}, got {named}"
            );
        }
    }

    /// RULE: a downloader fetching the installer saves that artifact where the invocation
    /// saves it, and an address this guest cannot reach writes nothing and fails.
    #[test]
    fn a_download_writes_the_artifact_it_fetched() {
        let installer = "https://aws-codedeploy-us-west-2.s3.us-west-2.amazonaws.com/latest/install";
        /// One row: the program the case invokes, the words it invokes it with, the path the
        /// fetched artifact is written at, and the status the program exits with.
        struct Fetch {
            case: &'static str,
            program: &'static str,
            words: &'static [&'static str],
            written: Option<&'static str>,
            status: i32,
        }
        let rows: &[Fetch] = &[
            Fetch {
                case: "wget naming the address alone",
                program: DOWNLOAD_PROGRAM,
                words: &["https://aws-codedeploy-us-west-2.s3.us-west-2.amazonaws.com/latest/install"],
                written: Some("install"),
                status: 0,
            },
            Fetch {
                case: "wget saving it elsewhere",
                program: DOWNLOAD_PROGRAM,
                words: &[
                    "-O",
                    "/tmp/agent-install",
                    "https://aws-codedeploy-us-west-2.s3.us-west-2.amazonaws.com/latest/install",
                ],
                written: Some("/tmp/agent-install"),
                status: 0,
            },
            Fetch {
                case: "curl under the address's own name",
                program: TRANSFER_PROGRAM,
                words: &[
                    "-O",
                    "https://aws-codedeploy-us-west-2.s3.us-west-2.amazonaws.com/latest/install",
                ],
                written: Some("install"),
                status: 0,
            },
            Fetch {
                case: "curl saving it elsewhere",
                program: TRANSFER_PROGRAM,
                words: &[
                    "-o",
                    "/tmp/install",
                    "https://aws-codedeploy-us-west-2.s3.us-west-2.amazonaws.com/latest/install",
                ],
                written: Some("/tmp/install"),
                status: 0,
            },
            Fetch {
                case: "an address this guest cannot reach",
                program: DOWNLOAD_PROGRAM,
                words: &["https://example.invalid/latest/install"],
                written: None,
                status: WGET_NETWORK_FAILURE,
            },
        ];
        for Fetch {
            case,
            program,
            words,
            written,
            status,
        } in rows
        {
            let mut tree = Tree::default();
            let words: Vec<String> = words.iter().map(|word| (*word).to_string()).collect();
            let answered = downloaded(program, &words, &mut tree)
                .unwrap_or_else(|| panic!("case {case:?}: the program must answer"));
            assert_eq!(
                answered.status, *status,
                "case {case:?}: expected status {status}, got {}",
                answered.status
            );
            match written {
                Some(path) => assert_eq!(
                    tree.read(path).as_deref(),
                    Some(AGENT_INSTALLER_FILE),
                    "case {case:?}: expected the artifact at {path:?}, got {:?}",
                    tree.0
                ),
                None => assert!(
                    tree.0.is_empty(),
                    "case {case:?}: expected nothing written, got {:?}",
                    tree.0
                ),
            }
        }
        // The artifact the installer address serves is the one the installer program runs.
        let mut tree = Tree::default();
        let words = vec![installer.to_string()];
        downloaded(DOWNLOAD_PROGRAM, &words, &mut tree).expect("the fetch must answer");
        let mut installed: Vec<&str> = Vec::new();
        let answered = installer_outcome("./install", &tree, |agent| installed.push(agent.name))
            .expect("running the fetched artifact must answer");
        assert_eq!(
            answered.status, 0,
            "expected the installer to succeed, got {}",
            answered.status
        );
        assert_eq!(
            installed,
            [CODEDEPLOY_AGENT],
            "expected the installer to install the agent, got {installed:?}"
        );
        assert!(
            installer_outcome("./other", &tree, |_| ()).is_none(),
            "expected a path the machine holds nothing at to be no program of this guest"
        );
    }

    /// RULE: a mode change names a path the machine has or reports the one it does not.
    #[test]
    fn a_mode_change_reports_a_path_the_machine_does_not_hold() {
        let mut tree = Tree::default();
        tree.write("install", AGENT_INSTALLER_FILE);
        let words = vec!["+x".to_string(), "./install".to_string()];
        let answered = mode_changed(&words, &mut tree).expect("the program must answer");
        assert_eq!(
            answered.status, 0,
            "expected a path the machine holds to be a change it takes, got {}",
            answered.status
        );
        let words = vec!["+x".to_string(), "/tmp/absent".to_string()];
        let answered = mode_changed(&words, &mut tree).expect("the program must answer");
        assert_eq!(
            answered.status, 1,
            "expected a path the machine does not hold to fail, got {}",
            answered.status
        );
        assert_eq!(
            answered.output,
            ["chmod: cannot access '/tmp/absent': No such file or directory"],
            "expected the program's own wording, got {:?}",
            answered.output
        );
    }
}
