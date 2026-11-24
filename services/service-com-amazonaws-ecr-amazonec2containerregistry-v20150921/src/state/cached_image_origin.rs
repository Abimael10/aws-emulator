// SPDX-License-Identifier: CC0-1.0

//! REMOVED. What an image this registry holds as a CACHED COPY was imported FROM is
//! `registry_pull::cached_upstream_reference`, beside the import that writes it, and one
//! question has ONE answer, so that is the one a caller reaches.
//!
//! This module is deliberately empty and is not declared: an earlier revision of the
//! ECR-backed ECS convergence fix read the upstream reference out of the cached image's own
//! configuration annotation here, before that reading landed in `registry_pull` resolved from
//! the covering rule instead.
