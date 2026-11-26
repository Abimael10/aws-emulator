// SPDX-License-Identifier: CC0-1.0

//! No cases of its own. The host-readiness gate in front of every `Server` deployment is
//! asserted in `server_platform_host_readiness_gate.rs`, and the window in which a failed
//! deployment's revision is readable on the host in
//! `server_platform_rollback_window_on_the_host.rs`.
