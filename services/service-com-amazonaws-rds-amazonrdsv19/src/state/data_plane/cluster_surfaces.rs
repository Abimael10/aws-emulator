// SPDX-License-Identifier: CC0-1.0

//! Superseded, and intentionally empty.
//!
//! The ONE backing database each database of a DB cluster is held in — the storage every
//! SURFACE of that cluster, this endpoint and the cluster's RDS Data API HTTP endpoint alike,
//! opens its own connection onto — is [`::service_common::cluster_sql_volumes`], which
//! [`super::sql_engine`] opens this endpoint's databases through, with the row locks over it
//! in [`super::row_locks`]. This file held an earlier reading of the same rule, in which this
//! endpoint copied the Data API's database in and out around each statement; it carries
//! nothing now and is declared nowhere.
