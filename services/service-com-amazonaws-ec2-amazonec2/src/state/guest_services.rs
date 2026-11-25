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

use ::service_common::posix_shell_session::{
    EnvironmentPrograms, ProgramFiles, ProgramInvocation, ProgramOutcome, ProgramStreams,
};
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
/// one and the caller of this module's programs states none: the ROOT of the machine's tree,
/// which is the directory a session nothing has changed with `cd` is in.
const SESSION_WORKING_DIRECTORY: &str = "/";

/// The status a program of this guest exits with when it did what it was asked.
const SUCCESS: i32 = 0;

/// The status a package manager exits with when it was asked for a package this guest has
/// none of.
const NO_SUCH_PACKAGE: i32 = 1;

/// The status the init system's control program exits with for a unit this machine does not
/// have.
const NO_SUCH_UNIT: i32 = 5;

/// The status it exits with when the unit it started did not come up: the program the unit runs
/// exited, which for an agent is one that has nothing to run with.
const UNIT_START_FAILED: i32 = 1;

/// The names the program that SENDS a datagram is invoked under, which is how every documented
/// "send a StatsD value from inside the instance" line is written.
const DATAGRAM_PROGRAMS: &[&str] = &["nc", "ncat", "netcat"];

/// The options of that program whose VALUE is the next word, so the address it was given is
/// read off the operands alone: how long it waits, how long it waits after the stream ends,
/// which local port and address it sends from, how long it waits between lines, and the proxy
/// options.
const DATAGRAM_VALUE_OPTIONS: &[&str] = &["-w", "-q", "-p", "-s", "-i", "-X", "-x"];

/// Whether `word` is the option that makes that program send a DATAGRAM rather than open a
/// connection — the only form of it this guest performs.
fn is_datagram_option(word: &str) -> bool {
    word == "-u" || word == "--udp" || (word.starts_with('-') && !word.starts_with("--") && word.contains('u'))
}

/// Whether `host` is one of the machine's OWN loopback addresses, which is what makes a
/// datagram sent to it one the machine sends to itself.
fn is_loopback(host: &str) -> bool {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host == "::1"
        || host == ALL_ADDRESSES
        || host.starts_with("127.")
}

/// The address every port of the machine is reachable at from inside it, which a program
/// addresses its own machine by as well.
const ALL_ADDRESSES: &str = "0.0.0.0";

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
    /// The CloudWatch agent as this guest holds it: the configuration it was handed, whether
    /// it is running, the datagrams its StatsD listener received and the collections it has
    /// published ([`super::guest_cloudwatch_agent`]).
    ///
    /// It is not among [`Self::running`]: that set is what the init system has started, and
    /// what the agent PUBLISHES depends on the configuration it was handed as well, which is
    /// the agent's own state rather than the init system's.
    pub cloudwatch_agent: super::guest_cloudwatch_agent::AgentRuntime,
    /// The space the guest's own programs RESERVED on its root filesystem, by the path each
    /// reservation was taken for ([`super::guest_filesystem`]): a count of bytes the
    /// filesystem holds against that file, which is what `fallocate` takes and what the
    /// filesystem reports in use from then on.
    pub reserved: BTreeMap<String, i64>,
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
            // What the CloudWatch agent was CONFIGURED with is on the machine's storage, so it
            // survives a boot; it is not running until something starts it, exactly as a unit
            // is not.
            cloudwatch_agent: super::guest_cloudwatch_agent::AgentRuntime {
                config: self.cloudwatch_agent.config.clone(),
                ..super::guest_cloudwatch_agent::AgentRuntime::default()
            },
            // The space a file holds on the filesystem is on the machine's storage, so it
            // survives a boot exactly as the file it was reserved for does.
            reserved: self.reserved.clone(),
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
                // ... and an agent that is running again is COLLECTING again, from the boot
                // it came up in: the CloudWatch agent's unit is what starts it, and the
                // configuration it kept is what it runs with.
                if unit == super::guest_cloudwatch_agent::AGENT_PACKAGE
                    && started.cloudwatch_agent.config.is_some()
                {
                    started.cloudwatch_agent.started_at = Some(::std::time::SystemTime::now());
                }
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
    pub(super) fn held(&self) -> ::std::sync::MutexGuard<'_, GuestRuntime> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How much space this machine's filesystem holds for `path`, or `None` when it holds none
    /// for it ([`super::guest_filesystem`]).
    #[must_use]
    pub fn reserved_at(&self, path: &str) -> Option<i64> {
        self.held().reserved.get(path).copied()
    }

    /// Hold `bytes` of this machine's filesystem for `path`, answering whether the filesystem
    /// now holds them: a reservation this machine already has for that path is REPLACED, and a
    /// machine already holding `most_paths` reservations for other paths takes no further one.
    ///
    /// Whether there is ROOM for it is the filesystem's own question, asked by the program that
    /// takes the reservation ([`super::guest_filesystem`]); what this answers is whether the
    /// machine has a record left to keep it in.
    pub fn reserve(&self, path: &str, bytes: i64, most_paths: usize) -> bool {
        let mut held = self.held();
        if !held.reserved.contains_key(path) && held.reserved.len() >= most_paths {
            return false;
        }
        held.reserved.insert(path.to_string(), bytes);
        true
    }

    /// Install the packages `arguments` names, as a package manager invocation.
    fn install(&self, packages: &[&str], files: &mut dyn ProgramFiles) -> ProgramOutcome {
        let mut output: Vec<String> = Vec::new();
        let mut status = SUCCESS;
        for name in packages {
            let Some(package) = package_named(name) else {
                // An AGENT, or one of the packages an agent's own installation procedure
                // names: installing it records it on the machine and puts the files that
                // package ships on its storage, and starts nothing, because installing a
                // package does not start its unit ([`super::guest_agents`]).
                if let Some(agent) = super::guest_agents::agent_named(name) {
                    self.held().installed.insert(agent.name.to_string());
                    for directory in agent.directories {
                        files.make_directories(directory);
                    }
                    for (path, contents) in agent.shipped {
                        if let Some(directory) = ::service_common::posix_shell_volume::parent_path(
                            path,
                        ) {
                            files.make_directories(&directory);
                        }
                        files.write(path, contents);
                    }
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
                if agent.name == super::guest_cloudwatch_agent::AGENT_PACKAGE {
                    // A package that is no longer installed has no agent state left either:
                    // what it was configured with went with it.
                    held.cloudwatch_agent =
                        super::guest_cloudwatch_agent::AgentRuntime::default();
                }
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
    /// unit beside enabling it. `host` is the machine the unit's own program runs on, which is
    /// what an agent resolves its configuration against, and `files` is the machine's own
    /// storage — where that configuration is, and what starting or stopping a unit that LISTENS
    /// changes what the kernel publishes about
    /// ([`super::guest_cloudwatch_agent::udp_socket_table`]).
    fn control(
        &self,
        verb: &str,
        unit: &str,
        now: bool,
        host: &super::guest_cloudwatch_agent::AgentHost,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramOutcome> {
        let unit = unit.trim().trim_end_matches(UNIT_SUFFIX);
        if let Some(agent) = super::guest_agents::agent_unit_named(unit) {
            return self.control_agent(verb, agent, unit, now, host, files);
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
    ///
    /// One of these agents does more than be running: starting the CloudWatch agent's unit
    /// starts the AGENT, with the configuration the machine handed it
    /// ([`super::guest_cloudwatch_agent`]) — so its unit comes up only once it has one, which
    /// is what the init system reports for a unit whose program exits, and stopping it closes
    /// what it had open.
    fn control_agent(
        &self,
        verb: &str,
        agent: &'static super::guest_agents::GuestAgent,
        unit: &str,
        now: bool,
        host: &super::guest_cloudwatch_agent::AgentHost,
        files: &mut dyn ProgramFiles,
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
                if !self.agent_unit_started(agent, host, files) {
                    return answered(
                        UNIT_START_FAILED,
                        format!(
                            "Job for {unit}{UNIT_SUFFIX} failed because the control process \
                             exited with error code."
                        ),
                    );
                }
                self.held().running.insert(agent.name.to_string());
                changed
            }
            "stop" => {
                self.held().running.remove(agent.name);
                self.agent_unit_stopped(agent, files);
                changed
            }
            "enable" | "disable" => {
                if !installed {
                    return answered(
                        NO_SUCH_UNIT,
                        format!("Failed to {verb} unit: Unit file {unit}{UNIT_SUFFIX} does not exist."),
                    );
                }
                if verb == "enable" {
                    self.held().enabled.insert(agent.name.to_string());
                    if now {
                        return self.control_agent("start", agent, unit, false, host, files);
                    }
                } else {
                    self.held().enabled.remove(agent.name);
                    if now {
                        return self.control_agent("stop", agent, unit, false, host, files);
                    }
                }
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

    /// Start whatever the agent `agent` names IS, beside its unit being up — answering whether
    /// the unit came up at all.
    ///
    /// The CloudWatch agent is the one that is more than a unit: starting it starts the agent
    /// with the configuration the MACHINE holds
    /// ([`super::guest_cloudwatch_agent::started`]), and a unit whose agent has no
    /// configuration anywhere does not come up. Every other agent here is up as soon as
    /// something started its unit.
    fn agent_unit_started(
        &self,
        agent: &'static super::guest_agents::GuestAgent,
        host: &super::guest_cloudwatch_agent::AgentHost,
        files: &mut dyn ProgramFiles,
    ) -> bool {
        if agent.name != super::guest_cloudwatch_agent::AGENT_PACKAGE {
            return true;
        }
        let mut held = self.held();
        super::guest_cloudwatch_agent::started(
            &mut held.cloudwatch_agent,
            host,
            files,
            ::std::time::SystemTime::now(),
        )
    }

    /// Stop whatever that agent IS, beside its unit being down: the CloudWatch agent stops
    /// collecting and closes its listener, which is what the kernel then publishes about the
    /// machine's sockets.
    fn agent_unit_stopped(
        &self,
        agent: &'static super::guest_agents::GuestAgent,
        files: &mut dyn ProgramFiles,
    ) {
        if agent.name != super::guest_cloudwatch_agent::AGENT_PACKAGE {
            return;
        }
        let mut held = self.held();
        held.cloudwatch_agent.stop();
        let table = super::guest_cloudwatch_agent::udp_socket_table(&held.cloudwatch_agent);
        drop(held);
        super::guest_cloudwatch_agent::publish_socket_tables(&table, files);
    }

    /// What one invocation of the CloudWatch agent's own control program answers.
    ///
    /// `None` when this machine has not installed the agent — the control program is a file
    /// that package ships, so a machine without it has no such program at all — or when the
    /// invocation is written in a form that program performs nothing of
    /// ([`super::guest_cloudwatch_agent::ControlInvocation`]).
    ///
    /// The agent's UNIT is active exactly while the agent is running, because the unit is what
    /// runs it: a `fetch-config … -s` through this program is what the documented procedure
    /// starts the agent with, and `systemctl is-active` has to report it.
    fn cloudwatch_agent_control(
        &self,
        host: &super::guest_cloudwatch_agent::AgentHost,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramStreams> {
        if !self.has(super::guest_cloudwatch_agent::AGENT_PACKAGE) {
            return None;
        }
        let invocation = super::guest_cloudwatch_agent::ControlInvocation::read(arguments)?;
        let mut held = self.held();
        let GuestRuntime {
            running,
            cloudwatch_agent,
            ..
        } = &mut *held;
        let streams = super::guest_cloudwatch_agent::controlled(
            &invocation,
            cloudwatch_agent,
            host,
            files,
            ::std::time::SystemTime::now(),
        );
        if cloudwatch_agent.is_running() {
            running.insert(super::guest_cloudwatch_agent::AGENT_PACKAGE.to_string());
        } else {
            running.remove(super::guest_cloudwatch_agent::AGENT_PACKAGE);
        }
        Some(streams)
    }

    /// Hand the datagram `payload` addressed to `port` at `host` to this machine's own network
    /// stack, answering whether the SEND happened.
    ///
    /// A datagram addressed to one of the machine's own LOOPBACK addresses is one it sends to
    /// itself, so whatever of its own programs is listening on that port receives it — which
    /// for a guest of this emulator is the CloudWatch agent's StatsD listener. An address of
    /// any other host is one this emulator carries no datagram to: the send happened, as a
    /// send of a datagram always does, and nothing received it.
    pub fn send_datagram(&self, host: &str, port: u16, payload: &str) -> bool {
        if !is_loopback(host) {
            return true;
        }
        self.held().cloudwatch_agent.received_datagram(
            port,
            payload,
            ::std::time::SystemTime::now(),
        );
        true
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
    /// directory `--directory` names, else `working_directory` — the directory the session that
    /// invoked it is IN, which is what the module's own default ("the current directory") is.
    ///
    /// `None` for every other invocation of the interpreter: this guest performs the module
    /// below and nothing else of it.
    fn serve_module(&self, arguments: &[String], working_directory: &str) -> Option<ProgramOutcome> {
        let served = module_invocation(arguments, working_directory)?;
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

/// What one guest's own NETWORK STACK does with a datagram a program of it sends: a write to
/// one of the machine's datagram addresses
/// ([`::service_common::posix_shell_volume::DATAGRAM_PATH_PREFIX`]) is handed here, so a shell
/// redirection and the sender program reach the machine's listeners the one way
/// ([`GuestServices::send_datagram`]).
impl ::service_common::posix_shell_volume::DatagramSink for GuestServices {
    fn send(&self, host: &str, port: u16, payload: &str) -> bool {
        self.send_datagram(host, port, payload)
    }
}

/// The programs the GUEST of `machine` has installed in it: the metadata client
/// ([`super::guest_metadata`]), the programs that install, start and run its services, which
/// change `services`, the ones that report its CPU layout ([`super::guest_cpu`]), the one
/// that lists its block devices ([`super::guest_block_devices`]), the ones that perform I/O
/// THROUGH one ([`super::guest_disk_io`], which change `disks`), the one that reads the clock
/// ([`super::guest_clock`]), the ones that report its own root FILESYSTEM
/// ([`super::guest_filesystem`]) and the ones that report what is MOUNTED into it
/// ([`super::guest_mounts`]).
///
/// The ONE construction of them, so the boot's own user-data program and a command something
/// later sends the guest have the same programs over the same machine.
#[must_use]
pub fn installed_programs(
    machine: &GuestMachine,
    services: &GuestServices,
    disks: &super::guest_disk_io::GuestDisks,
) -> Box<dyn EnvironmentPrograms> {
    let own: Box<dyn EnvironmentPrograms> = Box::new(GuestEnvironment {
        metadata: super::guest_metadata::guest_programs(machine),
        services: services.clone(),
        agent_host: super::guest_cloudwatch_agent::AgentHost {
            instance_id: machine.instance_id.clone(),
            image_id: machine.image_id.clone(),
            instance_type: machine.instance_type.clone(),
        },
    });
    // The programs a guest reads its own MEMORY with ([`super::guest_memory`]), for a machine
    // whose memory size this emulator holds: asked AFTER the programs above, so nothing here
    // can shadow one of them.
    let own = match super::guest_memory::memory_programs(super::guest_memory::guest_memory(
        machine.processor.map(|(_, memory_mib)| memory_mib),
    )) {
        Some(memory) => ::service_common::posix_shell_session::layered(own, memory),
        None => own,
    };
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
    // ... and the one it queries its own PACKAGE DATABASE with
    // ([`super::guest_cloudwatch_agent::package_query_programs`]), which answers out of what
    // this machine has installed at the instant the query runs, asked after the programs above
    // for the same reason.
    let own = ::service_common::posix_shell_session::layered(
        own,
        super::guest_cloudwatch_agent::package_query_programs(services, &machine.architecture),
    );
    // ... and the ones that perform I/O THROUGH one of those devices and resolve the links the
    // kernel publishes for it ([`super::guest_disk_io`]), asked after them for the same reason.
    let own = ::service_common::posix_shell_session::layered(
        own,
        super::guest_disk_io::disk_programs(disks),
    );
    // ... and the one that reads the machine's CLOCK ([`super::guest_clock`]), which every machine
    // has, asked after the programs above for the same reason.
    let own =
        ::service_common::posix_shell_session::layered(own, super::guest_clock::clock_programs());
    // ... and the ones that report its own ROOT FILESYSTEM and RESERVE space on it
    // ([`super::guest_filesystem`]), for a machine whose root volume size this store holds,
    // asked after the programs above for the same reason.
    let own = match super::guest_filesystem::filesystem_programs(machine, services) {
        Some(filesystem) => ::service_common::posix_shell_session::layered(own, filesystem),
        None => own,
    };
    // ... and the ones that report the filesystems mounted into it, which every machine has
    // (what they answer is what the session's own mount table holds), asked last for the same
    // reason — which is also what lets the FILESYSTEM form of `stat` reach that module once the
    // file form above has performed nothing of it.
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
    /// The facts of this machine the CloudWatch agent's configuration resolves its own
    /// `${aws:…}` references against ([`super::guest_cloudwatch_agent::AgentHost`]).
    agent_host: super::guest_cloudwatch_agent::AgentHost,
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
        // What this form of the seam reports is the program's standard OUTPUT alone, which is
        // what a caller asking it reads; the form the session asks carries both streams.
        self.service_program(name, arguments, SESSION_WORKING_DIRECTORY, None, files)
            .map(|streams| ProgramOutcome {
                status: streams.status,
                output: streams.output,
            })
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
        self.service_program(
            name,
            arguments,
            SESSION_WORKING_DIRECTORY,
            standard_input,
            files,
        )
        .map(|streams| ProgramOutcome {
            status: streams.status,
            output: streams.output,
        })
    }

    /// The form the session asks, which is answered on BOTH streams: what one of these
    /// programs REPORTED IT COULD NOT DO is a diagnostic, and a diagnostic is not part of what
    /// the program wrote to standard output.
    fn run_invocation(
        &self,
        invocation: &ProgramInvocation<'_>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramStreams> {
        if let Some(outcome) = self.metadata.run_invocation(invocation, files) {
            return Some(outcome);
        }
        self.service_program(
            invocation.name,
            invocation.arguments,
            invocation.working_directory,
            invocation.standard_input,
            files,
        )
    }
}

impl GuestEnvironment {
    /// What one of this guest's service programs answers for `name`, or `None` when this
    /// guest has no program of that name — or performs nothing of that invocation of it.
    ///
    /// `working_directory` is the directory the session is in, which is what a program whose
    /// own default is the directory it was started in serves out of
    /// ([`GuestServices::serve_module`]), and `standard_input` is what the stage before this
    /// command wrote — which is what a program that SENDS what it reads sends.
    fn service_program(
        &self,
        name: &str,
        arguments: &[String],
        working_directory: &str,
        standard_input: Option<&str>,
        files: &mut dyn ProgramFiles,
    ) -> Option<ProgramStreams> {
        // A path of this machine's OWN storage holding an agent's installer is a program of
        // this guest: running it installs that agent ([`super::guest_agents`]). Asked before
        // the name is read as a program name, because what makes it one is the file at the
        // path as the command wrote it.
        if let Some(outcome) = super::guest_agents::installer_outcome(name, &*files, |agent| {
            self.services.install_agent_and_start(agent);
        }) {
            return Some(written(outcome));
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
            // A request to an address of this machine's OWN loopback interface is answered by
            // the server of THIS guest listening there ([`super::guest_http_client`]): the
            // client and the server are in one machine, which is the one request a machine
            // makes entirely inside itself.
            let listening = self.held().listening.clone();
            if let Some(streams) =
                super::guest_http_client::fetched(name, arguments, files, &listening)
            {
                return Some(streams);
            }
            return super::guest_agents::downloaded(name, arguments, files).map(written);
        }
        if name == super::guest_agents::MODE_PROGRAM {
            return super::guest_agents::mode_changed(arguments, files);
        }
        if name == INIT_CONTROL {
            let (verb, operands, now) = invocation(arguments);
            return self
                .services
                .control(
                    verb?,
                    operands.first().copied()?,
                    now,
                    &self.agent_host,
                    files,
                )
                .map(written);
        }
        if name == SERVICE_CONTROL {
            // `service <unit> <verb>`: the operands are the other way round.
            let (unit, operands, now) = invocation(arguments);
            return self
                .services
                .control(
                    operands.first().copied()?,
                    unit?,
                    now,
                    &self.agent_host,
                    files,
                )
                .map(written);
        }
        if name == APACHE_CONTROL {
            let (verb, _, now) = invocation(arguments);
            return self
                .services
                .control(verb?, APACHE_UNIT, now, &self.agent_host, files)
                .map(written);
        }
        // The CloudWatch agent's own control program, which the documented procedure hands the
        // agent's configuration to and starts it with
        // ([`super::guest_cloudwatch_agent::controlled`]).
        if name == super::guest_cloudwatch_agent::AGENT_CONTROL_PROGRAM {
            return self
                .services
                .cloudwatch_agent_control(&self.agent_host, arguments, files);
        }
        // The program that SENDS what it reads to an address of this machine, which is how a
        // program inside the guest hands a datagram to something listening in it.
        if DATAGRAM_PROGRAMS.contains(&name) {
            return self.sent_datagram(arguments, standard_input);
        }
        if ::service_common::static_web_servers::INTERPRETERS.contains(&name) {
            return self
                .services
                .serve_module(arguments, working_directory)
                .map(written);
        }
        if let Some(package) = package_named(name) {
            // The server itself, invoked with no option: every other form is one this guest
            // performs nothing of.
            if !arguments.is_empty() {
                return None;
            }
            return self.services.start_daemon(package).map(written);
        }
        None
    }

    /// The services of this guest, for a program that reads what it is listening with.
    fn held(&self) -> ::std::sync::MutexGuard<'_, GuestRuntime> {
        self.services.held()
    }

    /// What the datagram sender answers for one invocation: the address it names takes what the
    /// command read on standard input ([`GuestServices::send_datagram`]).
    ///
    /// Only its DATAGRAM form is a program of this guest (`nc -u <host> <port>`, the form every
    /// documented "send a StatsD value" line is written in): a connection is not something this
    /// emulator carries, so an invocation without it is one this guest performs nothing of, and
    /// so is one naming no address.
    fn sent_datagram(
        &self,
        arguments: &[String],
        standard_input: Option<&str>,
    ) -> Option<ProgramStreams> {
        if !arguments.iter().any(|word| is_datagram_option(word)) {
            return None;
        }
        let mut operands: Vec<&String> = Vec::new();
        let mut words = arguments.iter();
        while let Some(word) = words.next() {
            if word.starts_with('-') {
                // An option whose value is the NEXT word takes that word with it, so the
                // address is read off the operands alone.
                if DATAGRAM_VALUE_OPTIONS.contains(&word.as_str()) {
                    let _ = words.next();
                }
                continue;
            }
            operands.push(word);
        }
        let [host, port] = operands.as_slice() else {
            return None;
        };
        let port: u16 = port.parse().ok()?;
        self.services
            .send_datagram(host, port, standard_input.unwrap_or_default());
        Some(ProgramStreams {
            status: SUCCESS,
            output: Vec::new(),
            errors: Vec::new(),
        })
    }

    /// One package-manager invocation: its subcommand decides what it does, and the operands
    /// after it are the packages.
    fn package_manager(
        &self,
        arguments: &[String],
        files: &mut dyn ProgramFiles,
    ) -> ProgramStreams {
        let (subcommand, packages, _) = invocation(arguments);
        match subcommand.unwrap_or_default() {
            "install" | "reinstall" | "localinstall" => written(self.services.install(&packages, files)),
            "remove" | "erase" | "autoremove" => written(self.services.remove(&packages)),
            // A manager asked to refresh its view of what it could install changes nothing
            // about this guest: every package it has is installed by name above.
            "update" | "upgrade" | "makecache" | "clean" | "check-update" | "list" => {
                ProgramStreams {
                    status: SUCCESS,
                    output: vec!["Nothing to do.".to_string()],
                    errors: Vec::new(),
                }
            }
            // An invocation naming no subcommand at all does nothing and SAYS so, as a
            // manager that was given no command to run does — on standard error, where what a
            // program reports it could not do goes.
            "" => ProgramStreams {
                status: NO_SUCH_PACKAGE,
                output: Vec::new(),
                errors: vec!["No command given.".to_string()],
            },
            unknown => ProgramStreams {
                status: NO_SUCH_PACKAGE,
                output: Vec::new(),
                errors: vec![format!("No such command: {unknown}.")],
            },
        }
    }
}

/// What a program that writes its answer to standard OUTPUT alone came to, as the two streams
/// the session takes ([`ProgramStreams`]): the lines it wrote and the status it exited with.
fn written(outcome: ProgramOutcome) -> ProgramStreams {
    ProgramStreams {
        status: outcome.status,
        output: outcome.output,
        errors: Vec::new(),
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
    served_from(server, &HeldFiles(files), method, path)
}

/// [`served_by`] over the tree as a PROGRAM of that guest reaches it
/// ([`ProgramFiles`]) — the LIVE tree of the session a command runs in, which is how a program
/// inside the machine reaches a server of the same machine
/// ([`super::guest_http_client`]) and sees the document the command before it wrote.
///
/// The ONE reading of what a guest's static server answers, so a request made from inside the
/// machine and one forwarded to it from outside cannot be answered two ways.
#[must_use]
pub fn served_from(
    server: &GuestServer,
    files: &dyn ProgramFiles,
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
    let (status, served) = match files.read(&under) {
        Some(contents) => (
            GUEST_HTTP_OK,
            Some((
                ::service_common::web_media_type::media_type_of_path(&under).to_string(),
                contents,
            )),
        ),
        None => {
            let index = format!("{}/{INDEX_DOCUMENT}", under.trim_end_matches('/'));
            match files.read(&index) {
                Some(contents) => (
                    GUEST_HTTP_OK,
                    Some((
                        ::service_common::web_media_type::media_type_of_path(&index).to_string(),
                        contents,
                    )),
                ),
                // A directory the root holds without an index document: a server that LISTS
                // one answers what it holds, and one that does not will not serve it. Every
                // other path the root holds nothing at.
                None if !files.entries_in(&under).is_empty() => {
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

/// The files an instance HOLDS, as the seam a server reads them through: the map that
/// instance's storage is kept in ([`InstanceRecord::guest_files`]), which is what a caller
/// OUTSIDE the machine serves a request out of ([`served_by`]).
///
/// Read-only: serving a request writes nothing and removes nothing, so the writing half of the
/// seam does nothing and says so.
struct HeldFiles<'a>(&'a BTreeMap<String, String>);

impl ProgramFiles for HeldFiles<'_> {
    fn read(&self, path: &str) -> Option<String> {
        self.0.get(path).cloned()
    }

    fn write(&mut self, _path: &str, _contents: &str) -> bool {
        false
    }

    fn remove(&mut self, _path: &str) -> bool {
        false
    }

    fn entries_in(&self, path: &str) -> Vec<String> {
        let under = format!("{}/", path.trim_end_matches('/'));
        let mut names: Vec<String> = Vec::new();
        for held in self.0.keys() {
            let Some(rest) = held.strip_prefix(&under) else {
                continue;
            };
            // Only what lies DIRECTLY under the directory is an entry of it: a deeper path
            // names the directory between them instead.
            let name = rest.split('/').next().unwrap_or(rest).to_string();
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
        names
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
fn listing(files: &dyn ProgramFiles, path: &str) -> String {
    let under = format!("{}/", path.trim_end_matches('/'));
    let mut entries: Vec<String> = Vec::new();
    for name in files.entries_in(path) {
        // A DIRECTORY of that directory is named with the trailing separator a listing gives
        // one, which is what makes it a link to the listing below it.
        let entry = if files.entries_in(&format!("{under}{name}")).is_empty() {
            name
        } else {
            format!("{name}/")
        };
        if !entries.contains(&entry) {
            entries.push(entry);
        }
    }
    entries.sort();
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
