// SPDX-License-Identifier: CC0-1.0

//! A MicroVM guest whose program is PYTHON.
//!
//! Which interpreter starts a guest is the image's own declaration: a container
//! specification whose command starts `python3` on a source file names a Python program
//! ([`super::guest_program::GuestLanguage::Python`]), and the engine this crate embeds
//! ([`super::guest_runtime`]) runs JavaScript. So such an image holds a program this
//! emulator cannot run, and the MicroVM it is run as is one the proxy reaches nothing
//! inside of — reported as exactly that, rather than answered for in the guest's name.
//!
//! Nothing is synthesised here: no status, header or body is produced for a program that
//! did not run.

use super::guest_runtime::GuestOutcome;

/// What a MicroVM whose image holds a Python program is answered with: a MicroVM this
/// emulator's engine reached nothing inside of.
#[must_use]
pub fn no_engine_for_a_python_guest() -> GuestOutcome {
    GuestOutcome::NoGuest
}
