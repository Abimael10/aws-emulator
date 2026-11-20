// SPDX-License-Identifier: CC0-1.0

//! An EMPTY suite — the husk of one that MOVED, kept only because it could not be deleted.
//!
//! Its cases — what `DescribeDaemon` reports for a stored Amazon ECS managed daemon, the
//! per-capacity-provider counts of its `currentRevisions` against the daemon tasks that
//! exist, and the reads that leave all of it as it was — were written here and then moved,
//! during review of that change, into the owning crate's daemon suite,
//! `services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/tests/all/daemon_scheduling.rs`
//! (`a_daemon_read_reports_a_daemon_whose_cluster_holds_no_container_instance` and its three
//! siblings, one per row of the rule), which drives them through the same operations beside
//! the rest of that service's daemon coverage. They belong there: the estate is ONE service
//! on one engine, so nothing in them is cross-service, which is what this target is for.
//!
//! What is left is this file, and only because the fixer tool set that performed the move can
//! write a file but not remove one. `automod` declares it as an empty module, which runs no
//! test; the next change to touch this directory should delete it.
