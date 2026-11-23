// SPDX-License-Identifier: CC0-1.0

//! The SERVICES one instance's guest runs — the web server its own program installed and
//! started — and what such a server answers for a request made to the port it listens on.
//!
//! A unit that listens on NO port is one of these too: an agent the machine installed
//! ([`super::guest_agents`]) is a unit of the init system like any other, so what makes it
//! active is that something started it ([`GuestRuntime::running`]) rather than a port it
//! answers on.
//!
//! # Why a guest needs them
//!
//! A machine's whole purpose is to SERVE something: "a script that installs and starts a web
//! server" is what the EC2 user guide's own tutorials put in an instance's user data, and
//! what a client then does with that instance is send it traffic — directly, or through an
//! Elastic Load Balancing target group that registers it. So an instance whose program
//! installed a server and started it has to answer on the port that server listens on;
//! otherwise every reader of the instance is told it is running while nothing it runs can be
//! reached, and the load balancer in front of it has nothing to forward a request to.
//!
//! # What is real here
//!
//! The program is PERFORMED, in the one bounded, in-process shell session this workspace
//! performs caller-supplied shell text with ([`super::guest_console`], which states why that
//! session and not a host process). This module is what the machine has INSTALLED in it
//! beside the metadata client ([`super::guest_metadata`]): the package manager that installs
//! a server, the init system's control program that starts and enables its unit, and the
//! servers themselves — each a program of the guest, performed over that session's own tree.
//! Nothing here starts a process, opens a socket or reaches a host path.
//!
//! What such a server then answers is computed from the guest's OWN STORAGE: the document
//! root the installed package serves is a directory of the machine's tree, and the response
//! is the file the request's path names under it — the bytes the guest's own program wrote
//! there — or the status a static server answers when the root holds no such file. No body
//! is invented for a server nothing started, and a port no server of the guest listens on is
//! a port nothing answers at.
//!
//! # Bounds
//!
//! How many ports one guest may listen on is bounded ([`MAX_GUEST_SERVERS`]), and everything
//! a server answers with comes out of the guest's own tree, which is bounded where that tree
//! is built ([`super::guest_console::MAX_GUEST_FILES`],
//! [`super::guest_console::MAX_GUEST_FILE_BYTES`]).

use ::std::collections::{BTreeMap, BTreeSet};
use ::std::sync::{Arc, Mutex, PoisonError};

use ::service_common::posix_shell_session::{EnvironmentPrograms, ProgramFiles, ProgramOutcome};
// The ONE reading this workspace has of which web server an invocation names, what its shipped
// configuration serves, and what an interpreter's own HTTP server module was invoked for —
// shared with every other service that runs a caller-supplied program and then has to answer an
// HTTP request at its machine's address.
use ::service_common::static_web_servers::{
    served_module as module_invocation, HTTP_SERVER_MODULE, UNIT_SUFFIX,
};
pub use ::service_common::static_web_servers::{
    package_named, WebServerPackage, INDEX_DOCUMENT, WEB_SERVER_PACKAGES,
};

use super::guest_console::GuestMachine;
use super::{ComAmazonawsEc2Amazonec2State, InstanceRecord, InstanceStateName};

/// How many ports ONE guest may listen on at once.
///
/// The emulator's own bound: a boot program is caller-supplied text, so what it can make this
/// process hold is bounded here. Far above the one or two ports a workload serves.
pub const MAX_GUEST_SERVERS: usize = 8;

/// The package managers a guest of this emulator has installed: the two of the Amazon Linux
/// family, the two of the Debian family, and the Amazon Linux 2 topic installer.
const PACKAGE_MANAGERS: &[&str] = &["yum", "dnf", "apt-get", "apt", "amazon-linux-extras"];

/// The init system's own control program, which starts, stops and enables a unit.
const INIT_CONTROL: &str = "systemctl";

/// The System V wrapper for the same, whose operand order is the other way round
/// (`service <unit> <verb>`).
const SERVICE_CONTROL: &str = "service";

/// Apache's own control program, which controls the one server it belongs to.
const APACHE_CONTROL: &str = "apachectl";

/// The unit [`APACHE_CONTROL`] controls.
const APACHE_UNIT: &str = "httpd";

/// The directory the interpreter's own HTTP server module serves when no `--directory` names
/// one: the working directory of the session the command was written in — which, for a session
/// with no `cd` to change it, is the root of the machine's tree.
const SESSION_WORKING_DIRECTORY: &str = "/";

/// The status a program of this guest exits with when it did what it was asked.
const SUCCESS: i32 = 0;

/// The status a package manager exits with when it was asked for a package this guest has
/// none of.
const NO_SUCH_PACKAGE: i32 = 1;

/// The status the init system's control program exits with for a unit this machine does not
/// have.
const NO_SUCH_UNIT: i32 = 5;

/// The status it answers a question about a unit that is not running with.
const UNIT_INACTIVE: i32 = 3;

/// The status it answers a question about a unit that is not enabled with.
const UNIT_NOT_ENABLED: i32 = 1;

/// One server one guest is listening with: which program serves it, on which port, out of
/// which directory of the machine's own tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestServer {
    /// The unit or program serving it.
    pub program: String,
    /// The port it listens on.
    pub port: i32,
    /// The directory of the machine's tree it serves.
    pub document_root: String,
    /// Whether it answers a DIRECTORY that holds no index document with a listing of what
    /// that directory holds: the interpreter's own HTTP server module does
    /// (`SimpleHTTPRequestHandler` lists the directory when it finds no `index.html`), and a
    /// web server package of this guest does not — both ship with that setting as it is here.
    pub lists_directories: bool,
}

/// What one guest's init system holds: which packages are installed, which units are enabled
/// to come up on every boot, and what is listening right now.
///
/// Stored on the instance ([`InstanceRecord::guest_runtime`]), so what one boot's program
/// installed and started is what the next command sent to that guest finds, and what a client
/// sending the instance traffic reaches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuestRuntime {
    /// The packages the guest's own program installed.
    pub installed: BTreeSet<String>,
    /// The units it ENABLED, which the init system starts on every boot.
    pub enabled: BTreeSet<String>,
    /// What is listening, by the port each server listens on.
    pub listening: BTreeMap<i32, GuestServer>,
    /// The units that are RUNNING and listen on no port: an AGENT is a unit of the machine
    /// rather than a server on a port ([`super::guest_agents`]), so what makes it active is
    /// that something STARTED it — the init system's control program, or the agent's own
    /// installer — and nothing about it can be read off [`Self::listening`].
    pub running: BTreeSet<String>,
}

impl GuestRuntime {
    /// The same machine the moment its operating system has come up again: what it has
    /// installed and what it has enabled survive a boot, because they are on its storage,
    /// and what was LISTENING does not — a server runs until the machine restarts, and the
    /// init system then starts the units that are enabled.
    #[must_use]
    pub fn restarted(&self) -> Self {
        let mut started = Self {
            installed: self.installed.clone(),
            enabled: self.enabled.clone(),
            listening: BTreeMap::new(),
            running: BTreeSet::new(),
        };
        for unit in &self.enabled {
            if let Some(package) = package_named(unit) {
                started.listen(unit, package.port, package.document_root, false);
                continue;
            }
            // An enabled unit that listens on nothing is RUNNING again after the boot, the
            // same way an enabled server is listening again.
            if super::guest_agents::agent_unit_named(unit).is_some() {
                started.running.insert(unit.clone());
            }
        }
        started
    }

    /// Record that `program` is listening on `port`, serving `document_root` — answering
    /// whether it is. A port something else already listens on is not taken from it, which
    /// is what a second server bound to one port finds, and a guest already listening on
    /// [`MAX_GUEST_SERVERS`] ports takes no further one.
    fn listen(
        &mut self,
        program: &str,
        port: i32,
        document_root: &str,
        lists_directories: bool,
    ) -> bool {
        if let Some(held) = self.listening.get(&port) {
            return held.program == program;
        }
        if self.listening.len() >= MAX_GUEST_SERVERS {
            return false;
        }
        self.listening.insert(
            port,
            GuestServer {
                program: program.to_string(),
                port,
                document_root: document_root.to_string(),
                lists_directories,
            },
        );
        true
    }

    /// Stop whatever `program` is listening with, answering whether it was.
    fn stop(&mut self, program: &str) -> bool {
        let ports: Vec<i32> = self
            .listening
            .iter()
            .filter(|(_, server)| server.program == program)
            .map(|(port, _)| *port)
            .collect();
        for port in &ports {
            self.listening.remove(port);
        }
        !ports.is_empty()
    }

    /// Whether `program` is listening on any port.
    fn is_listening(&self, program: &str) -> bool {
        self.listening
            .values()
            .any(|server| server.program == program)
    }
}

/// The services of one guest, as the programs of that guest change them while a session runs.
///
/// Shared with the session's own [`EnvironmentPrograms`] ([`installed_programs`]), which is
/// handed to the session by value: the caller keeps this handle and reads the runtime back out
/// of it once the program has run ([`Self::taken`]).
#[derive(Debug, Clone, Default)]
pub struct GuestServices(Arc<Mutex<GuestRuntime>>);

impl GuestServices {
    /// The services of a guest that is in `runtime` now.
    #[must_use]
    pub fn starting_from(runtime: &GuestRuntime) -> Self {
        Self(Arc::new(Mutex::new(runtime.clone())))
    }

    /// The runtime as the programs of the session have left it.
    #[must_use]
    pub fn taken(&self) -> GuestRuntime {
        self.held().clone()
    }

    /// The shared runtime. A guard another thread panicked while holding still holds this
    /// guest's services, so it is taken back rather than propagated: the records are a plain
    /// map, and nothing of them is left half written by a panic in a program.
    fn held(&self) -> ::std::sync::MutexGuard<'_, GuestRuntime> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Install the packages `arguments` names, as a package manager invocation.
    fn install(&self, packages: &[&str], files: &mut dyn ProgramFiles) -> ProgramOutcome {
        let mut output: Vec<String> = Vec::new();
        let mut status = SUCCESS;
        for name in packages {
            let Some(package) = package_named(name) else {
                // An AGENT, or one of the packages an agent's own installation procedure
                // names: installing it records it on the machine and starts nothing, because
                // installing a package does not start its unit ([`super::guest_agents`]).
                if let Some(agent) = super::guest_agents::agent_named(name) {
                    self.held().installed.insert(agent.name.to_string());
                    output.push(format!("Installed: {}", agent.name));
                    continue;
                }
                // A package this guest has none of installs nothing, and the manager reports
                // it the way one reports a name it cannot resolve.
                output.push(format!("No package {name} available."));
                status = NO_SUCH_PACKAGE;
                continue;
            };
            self.held().installed.insert(package.name.to_string());
            // What installing a web server leaves on the machine: the directory its shipped
            // configuration serves, so the commands around it can write the page it serves —
            // and the page the distribution SHIPS inside that directory, which is what a
            // server that was installed and started, and nothing else, answers its root with.
            // A page the machine already holds there is left alone: what a program wrote is
            // the page that machine serves.
            files.make_directories(package.document_root);
            let index = format!("{}/{INDEX_DOCUMENT}", package.document_root);
            if files.read(&index).is_none() {
                files.write(&index, package.default_document);
            }
            output.push(format!("Installed: {}", package.name));
        }
        if packages.is_empty() {
            status = NO_SUCH_PACKAGE;
        }
        ProgramOutcome { status, output }
    }

    /// Remove the packages `packages` names: what they had listening stops with them.
    fn remove(&self, packages: &[&str]) -> ProgramOutcome {
        let mut output: Vec<String> = Vec::new();
        for name in packages {
            if let Some(agent) = super::guest_agents::agent_named(name) {
                let mut held = self.held();
                held.installed.remove(agent.name);
                held.enabled.remove(agent.name);
                held.running.remove(agent.name);
                drop(held);
                output.push(format!("Removed: {}", agent.name));
                continue;
            }
            let Some(package) = package_named(name) else {
                continue;
            };
            let mut held = self.held();
            held.installed.remove(package.name);
            held.enabled.remove(package.name);
            held.stop(package.name);
            drop(held);
            output.push(format!("Removed: {}", package.name));
        }
        ProgramOutcome {
            status: SUCCESS,
            output,
        }
    }

    /// Perform one verb of the init system's control program on `unit`.
    ///
    /// `now` is the `--now` an `enable` or a `disable` may carry, which starts or stops the
    /// unit beside enabling it.
    fn control(&self, verb: &str, unit: &str, now: bool) -> Option<ProgramOutcome> {
        let unit = unit.trim().trim_end_matches(UNIT_SUFFIX);
        if let Some(agent) = super::guest_agents::agent_unit_named(unit) {
            return self.control_agent(verb, agent, unit, now);
        }
        let known = package_named(unit);
        let answered = |status: i32, line: String| {
            Some(ProgramOutcome {
                status,
                output: vec![line],
            })
        };
        match verb {
            "start" | "restart" | "reload-or-restart" | "try-restart" => {
                let Some(package) = known.filter(|package| self.has(package.name)) else {
                    return answered(
                        NO_SUCH_UNIT,
                        format!("Failed to {verb} {unit}{UNIT_SUFFIX}: Unit {unit}{UNIT_SUFFIX} not found."),
                    );
                };
                self.held()
                    .listen(package.name, package.port, package.document_root, false);
                Some(ProgramOutcome {
                    status: SUCCESS,
                    output: Vec::new(),
                })
            }
            "stop" => {
                let Some(package) = known else {
                    return answered(
                        NO_SUCH_UNIT,
                        format!("Failed to stop {unit}{UNIT_SUFFIX}: Unit {unit}{UNIT_SUFFIX} not found."),
                    );
                };
                self.held().stop(package.name);
                Some(ProgramOutcome {
                    status: SUCCESS,
                    output: Vec::new(),
                })
            }
            "enable" | "disable" => {
                let Some(package) = known.filter(|package| self.has(package.name)) else {
                    return answered(
                        NO_SUCH_UNIT,
                        format!("Failed to {verb} unit: Unit file {unit}{UNIT_SUFFIX} does not exist."),
                    );
                };
                let mut held = self.held();
                if verb == "enable" {
                    held.enabled.insert(package.name.to_string());
                    if now {
                        held.listen(package.name, package.port, package.document_root, false);
                    }
                } else {
                    held.enabled.remove(package.name);
                    if now {
                        held.stop(package.name);
                    }
                }
                drop(held);
                Some(ProgramOutcome {
                    status: SUCCESS,
                    output: Vec::new(),
                })
            }
            "is-active" | "status" => {
                let running = known.is_some_and(|package| self.held().is_listening(package.name));
                answered(
                    if running { SUCCESS } else { UNIT_INACTIVE },
                    if running { "active" } else { "inactive" }.to_string(),
                )
            }
            "is-enabled" => {
                let enabled =
                    known.is_some_and(|package| self.held().enabled.contains(package.name));
                answered(
                    if enabled { SUCCESS } else { UNIT_NOT_ENABLED },
                    if enabled { "enabled" } else { "disabled" }.to_string(),
                )
            }
            // Reloading the init system's own configuration changes no service of this guest.
            "daemon-reload" | "daemon-reexec" => Some(ProgramOutcome {
                status: SUCCESS,
                output: Vec::new(),
            }),
            // Every other verb is one this guest's control program performs nothing of.
            _ => None,
        }
    }

    /// Perform one verb of the init system's control program on the AGENT unit `agent` names.
    ///
    /// An agent listens on nothing, so what makes its unit active is that something started
    /// it: a `start` of a unit this machine has records it running
    /// ([`GuestRuntime::running`]), an `enable --now` does the same beside enabling it, and
    /// a unit this machine never installed is one the init system has no unit file for —
    /// which is what it reports rather than starting anything.
    fn control_agent(
        &self,
        verb: &str,
        agent: &'static super::guest_agents::GuestAgent,
        unit: &str,
        now: bool,
    ) -> Option<ProgramOutcome> {
        let installed = self.has(agent.name);
        let answered = |status: i32, line: String| {
            Some(ProgramOutcome {
                status,
                output: vec![line],
            })
        };
        let changed = Some(ProgramOutcome {
            status: SUCCESS,
            output: Vec::new(),
        });
        match verb {
            "start" | "restart" | "reload-or-restart" | "try-restart" => {
                if !installed {
                    return answered(
                        NO_SUCH_UNIT,
                        format!(
                            "Failed to {verb} {unit}{UNIT_SUFFIX}: Unit {unit}{UNIT_SUFFIX} not found."
                        ),
                    );
                }
                self.held().running.insert(agent.name.to_string());
                changed
            }
            "stop" => {
                self.held().running.remove(agent.name);
                changed
            }
            "enable" | "disable" => {
                if !installed {
                    return answered(
                        NO_SUCH_UNIT,
                        format!("Failed to {verb} unit: Unit file {unit}{UNIT_SUFFIX} does not exist."),
                    );
                }
                let mut held = self.held();
                if verb == "enable" {
                    held.enabled.insert(agent.name.to_string());
                    if now {
                        held.running.insert(agent.name.to_string());
                    }
                } else {
                    held.enabled.remove(agent.name);
                    if now {
                        held.running.remove(agent.name);
                    }
                }
                drop(held);
                changed
            }
            "is-active" | "status" => {
                let running = self.held().running.contains(agent.name);
                answered(
                    if running { SUCCESS } else { UNIT_INACTIVE },
                    if running { "active" } else { "inactive" }.to_string(),
                )
            }
            "is-enabled" => {
                let enabled = self.held().enabled.contains(agent.name);
                answered(
                    if enabled { SUCCESS } else { UNIT_NOT_ENABLED },
                    if enabled { "enabled" } else { "disabled" }.to_string(),
                )
            }
            "daemon-reload" | "daemon-reexec" => changed,
            _ => None,
        }
    }

    /// Install `agent` and start its unit, which is what running the agent's OWN installer
    /// does: "run the installer" leaves the agent on the machine, enabled and running
    /// ([`super::guest_agents::installer_outcome`]).
    fn install_agent_and_start(&self, agent: &'static super::guest_agents::GuestAgent) {
        let mut held = self.held();
        held.installed.insert(agent.name.to_string());
        if agent.unit {
            held.enabled.insert(agent.name.to_string());
            held.running.insert(agent.name.to_string());
        }
    }

    /// Whether the package `name` names is installed in this guest.
    fn has(&self, name: &str) -> bool {
        self.held().installed.contains(name)
    }

    /// Start the server `package` names in the FOREGROUND form a program invokes it by —
    /// `httpd` and `nginx` both put themselves in the background as shipped, so such an
    /// invocation returns once the server is listening.
    fn start_daemon(&self, package: &WebServerPackage) -> Option<ProgramOutcome> {
        if !self.has(package.name) {
            // A server this guest has not installed is a program it does not have.
            return None;
        }
        self.held()
            .listen(package.name, package.port, package.document_root, false);
        Some(ProgramOutcome {
            status: SUCCESS,
            output: Vec::new(),
        })
    }

    /// Serve a directory with the interpreter's own HTTP server module, read through the ONE
    /// reading of that invocation this workspace has
    /// ([`::service_common::static_web_servers::served_module`]): the port it was given and the
    /// directory `--directory` names, else this session's working directory.
    ///
    /// `None` for every other invocation of the interpreter: this guest performs the module
    /// below and nothing else of it.
    fn serve_module(&self, arguments: &[String]) -> Option<ProgramOutcome> {
        let served = module_invocation(arguments, SESSION_WORKING_DIRECTORY)?;
        // The module LISTS a directory that holds no index document, which is what
        // `SimpleHTTPRequestHandler` answers such a request with.
        self.held()
            .listen(HTTP_SERVER_MODULE, served.port, &served.document_root, true);
        Some(ProgramOutcome {
            status: SUCCESS,
            output: Vec::new(),
        })
    }
}

/// The programs the GUEST of `machine` has installed in it: the metadata client
/// ([`super::guest_metadata`]), the programs that install, start and run its services, which
/// change `services`, the ones that report its CPU layout ([`super::guest_cpu`]), the one
/// that lists its block devices ([`super::guest_block_devices`]) and the ones that report what
/// is MOUNTED into it ([`super::guest_mounts`]).
///
/// The ONE construction of them, so the boot's own user-data program and a command something
/// later sends the guest have the same programs over the same machine.
#[must_use]
pub fn installed_programs(
    machine: &GuestMachine,
    services: &GuestServices,
) -> Box<dyn EnvironmentPrograms> {
    let own: Box<dyn EnvironmentPrograms> = Box::new(GuestEnvironment {
        metadata: super::guest_metadata::guest_programs(machine),
        services: services.clone(),
    });
    // The programs that report the machine's own CPU layout ([`super::guest_cpu`]), for a
    // machine whose layout this emulator knows: asked AFTER the programs above, so nothing
    // here can shadow one of them.
    let own = match super::guest_cpu::cpu_programs(machine) {
        Some(cpu) => ::service_common::posix_shell_session::layered(own, cpu),
        None => own,
    };
    // ... and the one that lists its block devices ([`super::guest_block_devices`]), for a
    // machine with any attached, asked after both for the same reason.
    let own = match super::guest_block_devices::block_device_programs(machine) {
        Some(devices) => ::service_common::posix_shell_session::layered(own, devices),
        None => own,
    };
    // ... and the ones that report the filesystems mounted into it, which every machine has
    // (what they answer is what the session's own mount table holds), asked last for the same
    // reason.
    ::service_common::posix_shell_session::layered(own, super::guest_mounts::mount_programs())
}

/// Everything one guest has installed, as one environment.
#[derive(Debug)]
struct GuestEnvironment {
    /// The metadata client, which is asked first because it is the program with a name of its
    /// own ([`super::guest_metadata::METADATA_CLIENT`]).
    metadata: Box<dyn EnvironmentPrograms>,
    /// The services of this guest, which the programs below change.
    services: GuestServices,
}

impl EnvironmentPrograms for GuestEnvironment {
    fn run(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        if let Some(outcome) = self.metadata.run(name, arguments, files) {
            return Some(outcome);
        }
        self.service_program(name, arguments, files)
    }

    fn run_with_input(
        &self,
        name: &str,
        arguments: &[String],
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        if let Some(outcome) = self
            .metadata
            .run_with_input(name, arguments, standard_input, files)
        {
            return Some(outcome);
        }
        self.service_program(name, arguments, files)
    }
}

impl GuestEnvironment {
    /// What one of this guest's service programs answers for `name`, or `None` when this
    /// guest has no program of that name — or performs nothing of that invocation of it.
    fn service_program(
        &self,
        name: &str,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        // A path of this machine's OWN storage holding an agent's installer is a program of
        // this guest: running it installs that agent ([`super::guest_agents`]). Asked before
        // the name is read as a program name, because what makes it one is the file at the
        // path as the command wrote it.
        if let Some(outcome) = super::guest_agents::installer_outcome(name, &*files, |agent| {
            self.services.install_agent_and_start(agent);
        }) {
            return Some(outcome);
        }
        // A program named by its absolute path is the same program: `/usr/sbin/httpd` and
        // `httpd` are one, as a shell resolving a name on `PATH` finds.
        let name = name.rsplit('/').next().unwrap_or(name);
        if PACKAGE_MANAGERS.contains(&name) {
            return Some(self.package_manager(arguments, files));
        }
        // The programs an agent's documented installation procedure invokes: the downloader
        // that fetches its installer, and the one that makes what it fetched executable.
        if name == super::guest_agents::DOWNLOAD_PROGRAM
            || name == super::guest_agents::TRANSFER_PROGRAM
        {
            return super::guest_agents::downloaded(name, arguments, files);
        }
        if name == super::guest_agents::MODE_PROGRAM {
            return super::guest_agents::mode_changed(arguments, files);
        }
        if name == INIT_CONTROL {
            let (verb, operands, now) = invocation(arguments);
            return self.services.control(verb?, operands.first().copied()?, now);
        }
        if name == SERVICE_CONTROL {
            // `service <unit> <verb>`: the operands are the other way round.
            let (unit, operands, now) = invocation(arguments);
            return self
                .services
                .control(operands.first().copied()?, unit?, now);
        }
        if name == APACHE_CONTROL {
            let (verb, _, now) = invocation(arguments);
            return self.services.control(verb?, APACHE_UNIT, now);
        }
        if ::service_common::static_web_servers::INTERPRETERS.contains(&name) {
            return self.services.serve_module(arguments);
        }
        if let Some(package) = package_named(name) {
            // The server itself, invoked with no option: every other form is one this guest
            // performs nothing of.
            if !arguments.is_empty() {
                return None;
            }
            return self.services.start_daemon(package);
        }
        None
    }

    /// One package-manager invocation: its subcommand decides what it does, and the operands
    /// after it are the packages.
    fn package_manager(
        &self,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> ProgramOutcome {
        let (subcommand, packages, _) = invocation(arguments);
        match subcommand.unwrap_or_default() {
            "install" | "reinstall" | "localinstall" => self.services.install(&packages, files),
            "remove" | "erase" | "autoremove" => self.services.remove(&packages),
            // A manager asked to refresh its view of what it could install changes nothing
            // about this guest: every package it has is installed by name above.
            "update" | "upgrade" | "makecache" | "clean" | "check-update" | "list" => {
                ProgramOutcome {
                    status: SUCCESS,
                    output: vec!["Nothing to do.".to_string()],
                }
            }
            // An invocation naming no subcommand at all does nothing and says so, as a
            // manager that was given no command to run does.
            "" => ProgramOutcome {
                status: NO_SUCH_PACKAGE,
                output: vec!["No command given.".to_string()],
            },
            unknown => ProgramOutcome {
                status: NO_SUCH_PACKAGE,
                output: vec![format!("No such command: {unknown}.")],
            },
        }
    }
}

/// One invocation read as a verb and its operands: the words that are not OPTIONS, the first
/// of them being the verb, with `--now` reported separately because it changes what an
/// `enable` or a `disable` does.
///
/// An option carrying a value of its own (`-t <type>`) is not among the forms these programs
/// perform: an option is one word here, which is what every invocation below is written with.
fn invocation(arguments: &[String]) -> (Option<&str>, Vec<&str>, bool) {
    let now = arguments.iter().any(|word| word == "--now");
    let mut words = arguments
        .iter()
        .map(String::as_str)
        .filter(|word| !word.starts_with('-'));
    let verb = words.next();
    (verb, words.collect(), now)
}

/// What one request to a guest's own server answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestHttpResponse {
    /// The status the server answered with.
    pub status: u16,
    /// The media type of the document it served, where it served one.
    pub content_type: Option<String>,
    /// The bytes of that document.
    pub body: Vec<u8>,
}

/// The status a static server answers the document it was asked for with.
pub const GUEST_HTTP_OK: u16 = 200;

/// The status it answers a DIRECTORY holding no index document with: a server with directory
/// listing off serves nothing for one.
pub const GUEST_HTTP_FORBIDDEN: u16 = 403;

/// The status it answers a path its document root holds nothing at with.
pub const GUEST_HTTP_NOT_FOUND: u16 = 404;

/// The status it answers a method it serves no document for with.
pub const GUEST_HTTP_METHOD_NOT_ALLOWED: u16 = 405;

/// The methods a static server serves a document for: the one that asks for it, and the one
/// that asks for everything but its body.
const GET_METHOD: &str = "GET";
const HEAD_METHOD: &str = "HEAD";

/// What `server` answers for a request for `path` with `method`, out of the guest's own files.
///
/// The path is resolved under the server's document root — its segments normalised, so a
/// request cannot name anything outside that root — and the answer is the file there, the
/// directory's own [`INDEX_DOCUMENT`] for a directory that holds one, a LISTING of what that
/// directory holds for a server that lists one ([`GuestServer::lists_directories`]), and
/// otherwise the status a server with no listing answers.
#[must_use]
pub fn served_by(
    server: &GuestServer,
    files: &BTreeMap<String, String>,
    method: &str,
    path: &str,
) -> GuestHttpResponse {
    let method = method.trim().to_ascii_uppercase();
    if method != GET_METHOD && method != HEAD_METHOD {
        return GuestHttpResponse {
            status: GUEST_HTTP_METHOD_NOT_ALLOWED,
            content_type: None,
            body: Vec::new(),
        };
    }
    let requested = path.split(['?', '#']).next().unwrap_or(path);
    let under = resolved_path(&server.document_root, requested);
    let (status, served) = match files.get(&under) {
        Some(contents) => (
            GUEST_HTTP_OK,
            Some((
                ::service_common::web_media_type::media_type_of_path(&under).to_string(),
                contents.clone(),
            )),
        ),
        None => {
            let index = format!("{}/{INDEX_DOCUMENT}", under.trim_end_matches('/'));
            match files.get(&index) {
                Some(contents) => (
                    GUEST_HTTP_OK,
                    Some((
                        ::service_common::web_media_type::media_type_of_path(&index).to_string(),
                        contents.clone(),
                    )),
                ),
                // A directory the root holds without an index document: a server that LISTS
                // one answers what it holds, and one that does not will not serve it. Every
                // other path the root holds nothing at.
                None if holds_directory(files, &under) => {
                    if server.lists_directories {
                        (
                            GUEST_HTTP_OK,
                            Some((LISTING_MEDIA_TYPE.to_string(), listing(files, &under))),
                        )
                    } else {
                        (GUEST_HTTP_FORBIDDEN, None)
                    }
                }
                None => (GUEST_HTTP_NOT_FOUND, None),
            }
        }
    };
    let Some((content_type, contents)) = served else {
        return GuestHttpResponse {
            status,
            content_type: None,
            body: Vec::new(),
        };
    };
    GuestHttpResponse {
        status,
        content_type: Some(content_type),
        // A `HEAD` asks for everything about the document but its body.
        body: if method == HEAD_METHOD {
            Vec::new()
        } else {
            contents.into_bytes()
        },
    }
}

/// The media type a directory LISTING is served as, as the module that writes one declares
/// it.
const LISTING_MEDIA_TYPE: &str = "text/html; charset=utf-8";

/// The LISTING of the directory `path`: the document a server that lists a directory answers
/// with, naming what that directory holds — each entry once, in path order, a directory with
/// the trailing `/` a listing gives it.
///
/// The shape is the one `SimpleHTTPRequestHandler` writes: the directory's own name as the
/// title and heading, and one link per entry. Every entry is a file of the guest's own
/// storage; nothing is listed that the storage does not hold.
fn listing(files: &BTreeMap<String, String>, path: &str) -> String {
    let under = format!("{}/", path.trim_end_matches('/'));
    let mut entries: Vec<String> = Vec::new();
    for held in files.keys() {
        let Some(rest) = held.strip_prefix(&under) else {
            continue;
        };
        let entry = match rest.split_once('/') {
            // A path deeper than this directory names the DIRECTORY it is under.
            Some((directory, _)) => format!("{directory}/"),
            None => rest.to_string(),
        };
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    let title = format!("Directory listing for {under}");
    let items: String = entries
        .iter()
        .map(|entry| format!("<li><a href=\"{entry}\">{entry}</a></li>\n"))
        .collect();
    format!(
        "<!DOCTYPE HTML>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{title}</title>\n</head>\n<body>\n<h1>{title}</h1>\n<hr>\n<ul>\n{items}\
         </ul>\n<hr>\n</body>\n</html>\n"
    )
}

/// The absolute path a request for `path` names under `root`, with its segments normalised:
/// an empty segment and `.` name the directory itself, and `..` names the one above — never
/// above the root, so nothing outside the served directory can be named.
fn resolved_path(root: &str, path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    let root = root.trim_end_matches('/');
    if segments.is_empty() {
        return root.to_string();
    }
    format!("{root}/{}", segments.join("/"))
}

/// Whether the guest's files hold a DIRECTORY at `path`: some file lies under it.
fn holds_directory(files: &BTreeMap<String, String>, path: &str) -> bool {
    let under = format!("{}/", path.trim_end_matches('/'));
    files.keys().any(|held| held.starts_with(&under))
}

impl ComAmazonawsEc2Amazonec2State {
    /// The ports the guest of the instance `instance_id` names is listening on, in order.
    ///
    /// What another service asks to learn which ports of a registered instance anything
    /// answers on — a load balancer's health check of a target reads exactly this. A READ:
    /// nothing about the instance moves.
    #[must_use]
    pub fn instance_guest_server_ports(&self, instance_id: &str) -> Vec<i32> {
        self.serving_instance(instance_id)
            .map(|instance| instance.guest_runtime.listening.keys().copied().collect())
            .unwrap_or_default()
    }

    /// What the guest of the instance `instance_id` names answers for a request for `path`
    /// with `method` on `port` — `None` when nothing of that guest is listening there, which
    /// is a connection that cannot be made rather than a response.
    ///
    /// The one way another service reaches a workload an instance of this account runs: a
    /// load balancer forwarding a request to a target registered as this instance serves it
    /// from here. A READ: serving a request moves nothing about the instance, exactly as it
    /// moves nothing about the load balancer that forwarded it.
    #[must_use]
    pub fn serve_instance_guest_http(
        &self,
        instance_id: &str,
        port: i32,
        method: &str,
        path: &str,
    ) -> Option<GuestHttpResponse> {
        let instance = self.serving_instance(instance_id)?;
        let server = instance.guest_runtime.listening.get(&port)?;
        Some(served_by(server, &instance.guest_files, method, path))
    }

    /// The instance whose own interface holds `address`, for a caller that reaches a workload
    /// by the address it is registered at rather than by the instance's id.
    #[must_use]
    pub fn instance_id_at_address(&self, address: &str) -> Option<String> {
        let address = address.trim();
        if address.is_empty() {
            return None;
        }
        self.instances
            .values()
            .find(|instance| {
                instance.private_ip == address || instance.public_ip.as_deref() == Some(address)
            })
            .map(|instance| instance.instance_id.clone())
    }

    /// The instance `instance_id` names, where it has a guest that can answer at all: it is
    /// running and its operating system has come up.
    fn serving_instance(&self, instance_id: &str) -> Option<&InstanceRecord> {
        self.instances.get(instance_id).filter(|instance| {
            instance.state == InstanceStateName::Running && !instance.boots.is_empty()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a web server package names the same server however its name is written, and a
    /// name no package of this guest carries names none.
    #[test]
    fn a_package_name_names_the_server_the_distribution_ships() {
        // (the name, the package it names)
        let rows: &[(&str, Option<&str>)] = &[
            ("httpd", Some("httpd")),
            ("httpd.service", Some("httpd")),
            ("httpd24", Some("httpd")),
            ("nginx", Some("nginx")),
            ("nginx1", Some("nginx")),
            ("apache2", Some("apache2")),
            ("  nginx  ", Some("nginx")),
            ("httpdx", None),
            ("mariadb", None),
            ("", None),
        ];
        for (name, expected) in rows {
            let named = package_named(name).map(|package| package.name);
            assert_eq!(
                named, *expected,
                "case {name:?}: expected {expected:?}, got {named:?}"
            );
        }
    }

    /// RULE: what a server answers is the file its document root holds for the request's
    /// path — the root's own index document for the root itself — and the status a static
    /// server answers when the root holds nothing there; nothing outside the root can be
    /// named.
    #[test]
    fn a_server_answers_out_of_its_document_root() {
        let server = GuestServer {
            program: "httpd".to_string(),
            port: 80,
            document_root: "/var/www/html".to_string(),
            lists_directories: false,
        };
        let files: BTreeMap<String, String> = [
            ("/var/www/html/index.html", "<h1>root</h1>"),
            ("/var/www/html/health.txt", "ok"),
            ("/var/www/html/deeper/page.html", "deep"),
            ("/etc/not-under-the-root", "not served"),
        ]
        .into_iter()
        .map(|(path, contents)| (path.to_string(), contents.to_string()))
        .collect();
        /// One request a row makes of that server, and what it must answer.
        struct Asked {
            case: &'static str,
            method: &'static str,
            path: &'static str,
            status: u16,
            body: &'static str,
            content_type: Option<&'static str>,
        }
        let rows: &[Asked] = &[
            Asked {
                case: "the root",
                method: "GET",
                path: "/",
                status: GUEST_HTTP_OK,
                body: "<h1>root</h1>",
                content_type: Some("text/html"),
            },
            Asked {
                case: "a file by name",
                method: "GET",
                path: "/health.txt",
                status: GUEST_HTTP_OK,
                body: "ok",
                content_type: Some("text/plain"),
            },
            Asked {
                case: "a file in a subdirectory",
                method: "GET",
                path: "/deeper/page.html",
                status: GUEST_HTTP_OK,
                body: "deep",
                content_type: Some("text/html"),
            },
            Asked {
                case: "a query string, which names no file",
                method: "GET",
                path: "/health.txt?v=2",
                status: GUEST_HTTP_OK,
                body: "ok",
                content_type: Some("text/plain"),
            },
            Asked {
                case: "the body of a HEAD is empty",
                method: "HEAD",
                path: "/health.txt",
                status: GUEST_HTTP_OK,
                body: "",
                content_type: Some("text/plain"),
            },
            Asked {
                case: "a directory with no index document",
                method: "GET",
                path: "/deeper/",
                status: GUEST_HTTP_FORBIDDEN,
                body: "",
                content_type: None,
            },
            Asked {
                case: "a path the root holds nothing at",
                method: "GET",
                path: "/missing.html",
                status: GUEST_HTTP_NOT_FOUND,
                body: "",
                content_type: None,
            },
            Asked {
                case: "a path walking above the root",
                method: "GET",
                path: "/../../etc/not-under-the-root",
                status: GUEST_HTTP_NOT_FOUND,
                body: "",
                content_type: None,
            },
            Asked {
                case: "a method a static server does not serve",
                method: "POST",
                path: "/",
                status: GUEST_HTTP_METHOD_NOT_ALLOWED,
                body: "",
                content_type: None,
            },
        ];
        for Asked {
            case,
            method,
            path,
            status,
            body,
            content_type,
        } in rows
        {
            let answered = served_by(&server, &files, method, path);
            assert_eq!(
                answered.status, *status,
                "case {case:?}: expected status {status}, got {}",
                answered.status
            );
            assert_eq!(
                String::from_utf8_lossy(&answered.body),
                *body,
                "case {case:?}: expected body {body:?}, got {:?}",
                String::from_utf8_lossy(&answered.body)
            );
            assert_eq!(
                answered.content_type.as_deref(),
                *content_type,
                "case {case:?}: expected content type {content_type:?}, got {:?}",
                answered.content_type
            );
        }
    }

    /// RULE: a server that LISTS a directory answers one holding no index document with what
    /// that directory holds, and a server that does not list one serves nothing for it.
    #[test]
    fn a_directory_with_no_index_is_listed_only_by_a_server_that_lists_one() {
        let files: BTreeMap<String, String> = [
            ("/srv/site/notes.txt", "notes"),
            ("/srv/site/deeper/page.html", "deep"),
        ]
        .into_iter()
        .map(|(path, contents)| (path.to_string(), contents.to_string()))
        .collect();
        let listing_server = GuestServer {
            program: "http.server".to_string(),
            port: 8000,
            document_root: "/srv/site".to_string(),
            lists_directories: true,
        };
        let answered = served_by(&listing_server, &files, "GET", "/");
        assert_eq!(
            answered.status, GUEST_HTTP_OK,
            "expected a listing server to answer the directory, got {}",
            answered.status
        );
        let body = String::from_utf8_lossy(&answered.body).into_owned();
        for entry in ["notes.txt", "deeper/"] {
            assert!(
                body.contains(entry),
                "expected the listing to name {entry:?}, got {body:?}"
            );
        }
        assert_eq!(
            answered.content_type.as_deref(),
            Some(LISTING_MEDIA_TYPE),
            "expected the listing's own media type, got {:?}",
            answered.content_type
        );

        let quiet_server = GuestServer {
            lists_directories: false,
            ..listing_server
        };
        let answered = served_by(&quiet_server, &files, "GET", "/");
        assert_eq!(
            answered.status, GUEST_HTTP_FORBIDDEN,
            "expected a server that lists no directory to serve nothing for one, got {}",
            answered.status
        );
    }

    /// RULE: what a guest has installed and enabled survives a boot and what was listening
    /// does not — the init system starts the enabled units again, and nothing else.
    #[test]
    fn a_boot_starts_the_enabled_units_and_nothing_else() {
        let mut runtime = GuestRuntime::default();
        runtime.installed.insert("httpd".to_string());
        runtime.installed.insert("nginx".to_string());
        runtime.enabled.insert("httpd".to_string());
        runtime.listen("nginx", 80, "/usr/share/nginx/html", false);
        let restarted = runtime.restarted();
        assert_eq!(
            restarted.installed, runtime.installed,
            "expected the installed packages to survive the boot, got {:?}",
            restarted.installed
        );
        let listening: Vec<&str> = restarted
            .listening
            .values()
            .map(|server| server.program.as_str())
            .collect();
        assert_eq!(
            listening,
            ["httpd"],
            "expected only the enabled unit to be listening, got {listening:?}"
        );
        let server = restarted
            .listening
            .get(&80)
            .expect("the enabled unit listens on its own port");
        assert_eq!(
            server.document_root, "/var/www/html",
            "expected the package's own document root, got {:?}",
            server.document_root
        );
    }
}
