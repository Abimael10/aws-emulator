// SPDX-License-Identifier: CC0-1.0

//! REMOVED. The `AWS::ECR::PullThroughCacheRule` provider lives in
//! `state/provisioning/ecr_pull_through_cache_rule.rs`, and one resource type has ONE
//! provider, so the arm a stack reaches is that one.
//!
//! This module is deliberately empty and is not declared: an earlier revision of the
//! ECR-backed ECS convergence fix carried its own provider here, through the
//! `Provisioner` catalogue, before that arm landed.
