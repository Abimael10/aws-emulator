// SPDX-License-Identifier: CC0-1.0

//! Running a program in the GUEST of one running instance, for the agent that is already in
//! it.
//!
//! # Why this lives here
//!
//! An instance's guest is EC2's: its storage, its host name and the identity the kernel
//! published to it are facts of the instance this store holds, and the one place a program of
//! that guest runs is the bounded, in-process shell session the boot's own user-data program
//! runs in ([`super::guest_console`], which states why that session and not a host process).
//! So a service that has an agent inside the guest — Systems Manager's Run Command, which
//! delivers a document's commands to the agent — asks THIS store to perform them, rather than
//! keeping a second notion of what the machine is or what it holds.
//!
//! # What a run is, and what it is not
//!
//! The commands are performed by that session, in a tree holding exactly this machine's
//! storage and its devices: a command whose whole effect is inside the guest (writing a file,
//! reading one back, writing a line of output) really happens, and the file is still there on
//! the next run and on the next boot, because the tree IS the instance's storage. Every
//! command the session provides no program for — anything needing a process, a socket or a
//! host path — runs nothing and answers the status a shell answers for a utility it could not
//! find ([`::service_common::posix_shell_session::Statuses::could_not_run`]), exactly as it
//! does on a machine whose shell has no such program. Nothing here starts a process, opens a
//! host file or reaches a socket, and nothing is written for a command that did not run.
//!
//! # A guest has to be up
//!
//! A program runs in a guest that is RUNNING and has BOOTED: an instance that is still
//! `pending`, one that is stopped or gone, and one whose first boot the clock has not
//! materialised yet have no operating system to run anything
//! ([`ComAmazonawsEc2Amazonec2State::boot_due_instance_guests`] is what brings one up), so
//! each of those answers `None` — "there was no guest to run this in" — rather than an
//! outcome nothing produced.
//!
//! # An agent's own files
//!
//! Not everything such an agent does is a program: it also PUTS files on the machine — the
//! artifact it fetched for the work it was sent, and the files that work installs — and reads
//! back what an earlier run left there. Those writes go through the same tree and the same
//! bounds as a program's ([`ComAmazonawsEc2Amazonec2State::put_in_instance_guest`],
//! [`ComAmazonawsEc2Amazonec2State::read_in_instance_guest`]), so the agent and the guest's
//! own programs read and write one machine's storage rather than two notions of it.

use ::service_common::posix_shell_session::{
    layered, Bounds, EnvironmentPrograms, OutputLimits, SessionConfig, ShellSession,
};

use super::guest_console::{guest_environment, guest_volume, GuestIdentity};
use super::{ComAmazonawsEc2Amazonec2State, InstanceStateName};

/// How many commands ONE run performs before the session is given up on.
///
/// The bound on the work one run costs, and it matters where this runs: a run is performed
/// with this store's guard held, so what a caller-supplied program can make one cost is what
/// every other caller of the store waits for. Well above any command document's own script,
/// and far below what would cost this process anything noticeable.
pub const MAX_GUEST_RUN_COMMANDS: usize = 64;

/// How many lines of output ONE run keeps, and how many bytes one of those lines carries.
///
/// The output is caller-sized — the program is the caller's text — so what one run can
/// retain is bounded here. A run that wrote more keeps the first lines of it, which is what
/// a caller reading the head of a command's output sees.
pub const MAX_GUEST_RUN_OUTPUT_LINES: usize = 256;

/// See [`MAX_GUEST_RUN_OUTPUT_LINES`].
pub const MAX_GUEST_RUN_OUTPUT_LINE_BYTES: usize = 2_048;

/// What one program run in a guest answered: the lines it wrote and the status it exited
/// with.
#[derive(Debug, Clone, Default)]
pub struct GuestRun {
    /// The lines the program wrote to its standard output, in the order it wrote them.
    pub output: Vec<String>,
    /// The status the program exited with — the shell's own: zero for a program that ran to
    /// success, and the status a shell answers for a utility it could not find for a command
    /// this guest has no program for.
    pub status: i32,
}

impl ComAmazonawsEc2Amazonec2State {
    /// Perform `commands` in the guest of the instance `instance_id` names, in order,
    /// answering what the guest wrote and the status it ended on — or `None` when there is no
    /// running, booted guest to perform them in.
    ///
    /// The commands run in ONE session, so a variable one sets and a file one writes are
    /// there for the next; the files the session leaves behind become the instance's own
    /// storage, the way the boot's program's do. The run stops early when a command ends the
    /// session (`exit`), which is what a script's own `exit` does.
    pub fn run_in_instance_guest(
        &mut self,
        instance_id: &str,
        commands: &[String],
    ) -> Option<GuestRun> {
        self.run_in_instance_guest_with(instance_id, commands, None, &GuestIdentity::default())
    }

    /// [`Self::run_in_instance_guest`], with `supplied` as the programs the CALLER installs in
    /// the guest beside the machine's own ([`super::guest_metadata::guest_programs`]) and
    /// `identity` as the part of the machine's own identity another service owns.
    ///
    /// A guest has programs whose answers come from state this store cannot read — an agent's
    /// resolver client, which answers out of the account's DNS — and the component that CAN
    /// read that state is the one that reaches into the guest. So it hands those programs over
    /// here rather than this store keeping a second notion of what they answer; the machine's
    /// own programs are asked first, so nothing the caller supplies can shadow one of them
    /// ([`::service_common::posix_shell_session::layered`]). The one FACT of the machine in
    /// the same position — the name of the role its instance profile carries, which IAM holds
    /// — travels the same way, resolved by the caller
    /// ([`super::instance_profile_role_name`]).
    pub fn run_in_instance_guest_with(
        &mut self,
        instance_id: &str,
        commands: &[String],
        supplied: Option<Box<dyn EnvironmentPrograms>>,
        identity: &GuestIdentity,
    ) -> Option<GuestRun> {
        let instance = self.instances.get(instance_id)?;
        if instance.state != InstanceStateName::Running {
            return None;
        }
        // The guest's current boot: the session reads that boot's own identity, and an
        // instance whose guest has not come up yet has none.
        let boot = instance.boots.last()?;
        let (boot_id, number) = (boot.boot_id.clone(), boot.number);
        let files = instance.guest_files.clone();
        // The machine the program runs on, running as the role the caller resolved for it:
        // which role the instance's profile carries is a fact IAM holds, so it is handed in
        // rather than read here ([`GuestMachine::running_as`]).
        let machine = self.guest_machine(instance).running_as(identity);
        // The services the machine is running now, which a command of this run may install,
        // start or stop ([`super::guest_services`]).
        let services =
            super::guest_services::GuestServices::starting_from(&instance.guest_runtime);
        let volume = guest_volume(&machine, &boot_id, &files);
        let mut session = ShellSession::in_volume(
            guest_environment(&machine, number),
            SessionConfig {
                bounds: Bounds {
                    commands: MAX_GUEST_RUN_COMMANDS,
                    ..Bounds::default()
                },
                output: Some(OutputLimits {
                    lines: MAX_GUEST_RUN_OUTPUT_LINES,
                    line_bytes: MAX_GUEST_RUN_OUTPUT_LINE_BYTES,
                }),
                ..SessionConfig::default()
            },
            volume,
        )
        // The programs this machine has installed in it, which an agent's command reads the
        // instance's own facts with ([`super::guest_metadata`]) and installs and starts its
        // services with ([`super::guest_services`]) — the same programs the boot's own
        // program has — and, under them, the ones the caller installed.
        .with_environment_programs({
            let own = super::guest_services::installed_programs(&machine, &services);
            match supplied {
                Some(supplied) => layered(own, supplied),
                None => own,
            }
        });
        let mut status = 0;
        for command in commands {
            status = session.run(command);
            if let Some(exited) = session.exit_status() {
                // The program ended the session itself: that status is the run's.
                status = exited;
                break;
            }
        }
        let output = session.output().to_vec();
        let volume = session.into_volume().unwrap_or_default();
        let written: ::std::collections::BTreeMap<String, String> = volume
            .stored_files()
            .map(|(path, contents)| (path.to_string(), contents.to_string()))
            .collect();
        // What the program left on the machine's storage is what the machine holds now — the
        // same write the boot's own program makes, and so is what it left RUNNING.
        if let Some(instance) = self.instances.get_mut(instance_id) {
            instance.guest_files = written;
            instance.guest_runtime = services.taken();
        }
        Some(GuestRun { output, status })
    }

    /// What the guest of the instance `instance_id` names holds at `path`, or `None` when it
    /// holds nothing there.
    ///
    /// The counterpart of [`Self::put_in_instance_guest`] for an agent that READS the
    /// machine's storage — the artifact it put there on an earlier run — rather than
    /// performing a program in it. The path is resolved the way the tree a program runs in
    /// resolves one ([`::service_common::posix_shell_volume::resolved_path`]), so the agent and
    /// the guest's own programs name one file by one key.
    pub fn read_in_instance_guest(&self, instance_id: &str, path: &str) -> Option<String> {
        let resolved = ::service_common::posix_shell_volume::resolved_path(path)?;
        self.instances
            .get(instance_id)?
            .guest_files
            .get(&resolved)
            .cloned()
    }

    /// Whether that guest holds anything at `path`: a file, or a directory something under
    /// it makes.
    ///
    /// What an agent asks before it writes, because a path the machine already holds is the
    /// difference between replacing a file and creating one.
    pub fn instance_guest_holds(&self, instance_id: &str, path: &str) -> bool {
        let Some(resolved) = ::service_common::posix_shell_volume::resolved_path(path) else {
            return false;
        };
        let under = format!("{}/", resolved.trim_end_matches('/'));
        self.instances.get(instance_id).is_some_and(|instance| {
            instance.guest_files.contains_key(&resolved)
                || instance
                    .guest_files
                    .keys()
                    .any(|held| held.starts_with(&under))
        })
    }

    /// Put `files` on the storage of that guest, as `(path, contents)` — what an AGENT
    /// inside it writes: the artifact it fetched for the work it was sent, and the files that
    /// work installs.
    ///
    /// Answering the paths the guest holds afterwards, or `None` when there is no running,
    /// booted guest to write to ([`Self::run_in_instance_guest_with`] states what a guest has
    /// to be). The writes go through the SAME tree a program of this guest runs in
    /// ([`super::guest_console::guest_volume`]), so one path is resolved one way, the
    /// directories above a file are made the way a machine that holds it has them, a write to
    /// a path the kernel publishes is refused as it is for the guest's own program, and every
    /// write is held to that tree's bounds: a write past them does not happen and its path is
    /// left out of the answer.
    pub fn put_in_instance_guest(
        &mut self,
        instance_id: &str,
        files: &[(String, String)],
    ) -> Option<Vec<String>> {
        self.in_instance_guest_volume(instance_id, |volume| {
            let mut written: Vec<String> = Vec::new();
            for (path, contents) in files {
                if volume.put_file(path, contents) {
                    if let Some(resolved) = volume.resolve(path) {
                        written.push(resolved);
                    }
                }
            }
            written
        })
    }

    /// Remove what `paths` names from the storage of that guest, as an AGENT inside it
    /// does: the files of an earlier run of its work, taken off before the next run puts
    /// its own there.
    ///
    /// Answering the paths the guest held and no longer holds, or `None` when there is no
    /// running, booted guest. The removals go through the SAME tree
    /// ([`Self::put_in_instance_guest`] states why), so a path the kernel publishes is not
    /// one an agent can take away and a path the guest does not hold is simply not in the
    /// answer.
    pub fn remove_in_instance_guest(
        &mut self,
        instance_id: &str,
        paths: &[String],
    ) -> Option<Vec<String>> {
        self.in_instance_guest_volume(instance_id, |volume| {
            let mut removed: Vec<String> = Vec::new();
            for path in paths {
                if let Some(resolved) = volume.resolve(path) {
                    if volume.remove_file(&resolved) {
                        removed.push(resolved);
                    }
                }
            }
            removed
        })
    }

    /// Let `change` change the TREE of that guest's storage, and keep what it left there.
    ///
    /// The one place an agent's own write of a machine reaches that tree, so the two seams
    /// above cannot differ about which guest has one, which tree it is
    /// ([`super::guest_console::guest_volume`]) or what the machine holds afterwards:
    /// `None` for an instance that is not running or whose operating system has not come up,
    /// and otherwise what `change` answered, with the files it left behind kept as that
    /// machine's storage — the same write a program of this guest's own makes
    /// ([`Self::run_in_instance_guest_with`]).
    fn in_instance_guest_volume<T>(
        &mut self,
        instance_id: &str,
        change: impl FnOnce(&mut ::service_common::posix_shell_volume::ShellVolume) -> T,
    ) -> Option<T> {
        let instance = self.instances.get(instance_id)?;
        if instance.state != InstanceStateName::Running {
            return None;
        }
        let boot_id = instance.boots.last()?.boot_id.clone();
        let held = instance.guest_files.clone();
        let machine = self.guest_machine(instance);
        let mut volume = guest_volume(&machine, &boot_id, &held);
        let answered = change(&mut volume);
        let stored: ::std::collections::BTreeMap<String, String> = volume
            .stored_files()
            .map(|(path, contents)| (path.to_string(), contents.to_string()))
            .collect();
        if let Some(instance) = self.instances.get_mut(instance_id) {
            instance.guest_files = stored;
        }
        Some(answered)
    }

    /// Whether the instance `instance_id` names has a guest a program can run in now: it is
    /// running and its operating system has come up.
    ///
    /// Asked by a caller that reports WHETHER it delivered a program, so it does not have to
    /// read this store's instance shapes itself.
    pub fn instance_guest_is_up(&self, instance_id: &str) -> bool {
        self.instances.get(instance_id).is_some_and(|instance| {
            instance.state == InstanceStateName::Running && !instance.boots.is_empty()
        })
    }
}
