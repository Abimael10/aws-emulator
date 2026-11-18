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

use ::service_common::posix_shell_session::{
    layered, Bounds, EnvironmentPrograms, OutputLimits, SessionConfig, ShellSession,
};

use super::guest_console::{guest_environment, guest_volume};
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
        self.run_in_instance_guest_with(instance_id, commands, None)
    }

    /// [`Self::run_in_instance_guest`], with `supplied` as the programs the CALLER installs in
    /// the guest beside the machine's own ([`super::guest_metadata::guest_programs`]).
    ///
    /// A guest has programs whose answers come from state this store cannot read — an agent's
    /// resolver client, which answers out of the account's DNS — and the component that CAN
    /// read that state is the one that reaches into the guest. So it hands those programs over
    /// here rather than this store keeping a second notion of what they answer; the machine's
    /// own programs are asked first, so nothing the caller supplies can shadow one of them
    /// ([`::service_common::posix_shell_session::layered`]).
    pub fn run_in_instance_guest_with(
        &mut self,
        instance_id: &str,
        commands: &[String],
        supplied: Option<Box<dyn EnvironmentPrograms>>,
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
        let machine = self.guest_machine(instance);
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
