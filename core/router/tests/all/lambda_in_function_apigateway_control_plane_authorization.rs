// SPDX-License-Identifier: CC0-1.0

//! EMPTY — this file carries no test, so nothing here runs.
//!
//! It held a second in-function probe of API Gateway's control-plane authorization, written
//! before this branch merged the mainline tip that already carries
//! `all/apigateway_control_plane_role_authorization.rs` — which pins that same rule over a
//! wider table (AWS-managed policies, embedded documents, an explicit deny, an assumed-role
//! session, a function's execution-role session and the account's own credential) with one
//! harness. The rows that probe had and that suite did not — a document granting ONE verb of
//! the plane, which must serve only the calls that use it — were added to that suite's own
//! in-function test instead of carried here a second time. Its content is gone rather than the
//! file because this fixer's tool set can write a file but not remove one; the next change to
//! touch this directory should delete it.
