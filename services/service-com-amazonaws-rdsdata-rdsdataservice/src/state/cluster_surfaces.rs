// SPDX-License-Identifier: CC0-1.0

//! Superseded, and intentionally empty.
//!
//! The ONE backing database each database of a DB cluster is held in — the storage every
//! SURFACE of that cluster opens its own connection onto — is
//! [`::service_common::cluster_sql_volumes`], which this service's own databases are opened
//! through (`super::Database::open`). This file held an earlier reading of the same rule, in
//! which this service lent its databases to the cluster's engine endpoint for copying; it
//! carries nothing now and is declared nowhere.
