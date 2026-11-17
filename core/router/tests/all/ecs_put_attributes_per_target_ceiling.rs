// SPDX-License-Identifier: CC0-1.0

//! An EMPTY suite, deliberately.
//!
//! Its cases — the ceiling Amazon ECS applies to the CUSTOM attributes of one resource, the
//! refusal a write past it earns, and the slot a delete frees — live in the owning crate's
//! attribute suite,
//! `services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/tests/all/delete_attributes.rs`,
//! which drives them through the same operations without the cross-service estate this file
//! stood up (Systems Manager's published AMI id, an IAM instance profile, a public subnet
//! and an `ec2 RunInstances` whose user data configures the container agent). That estate is
//! already covered, for the registration it produces, by that crate's own
//! `tests/all/ec2_launched_cluster_capacity.rs`.
//!
//! The file is still here because the integration-test layout refuses an undeclared file
//! under `tests/all/` and this fixer's tool set can write a file but not remove one — the
//! same reason `all/zz_probe_whole_emulator.rs` is still there. The next change to touch
//! this directory should delete it.
