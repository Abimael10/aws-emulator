// SPDX-License-Identifier: CC0-1.0

//! An EMPTY suite — the husk of a scratch reproduction, kept only because it could not be
//! deleted.
//!
//! It held a throwaway reproduction of the recorded "a matched EventBridge rule delivers to a
//! Lambda target through the target's `RoleArn`" scenario, written while tracing that report.
//! What it established now lives in the suites that state the rule as a table:
//! `core/router/tests/all/events_target_role_grants_the_delivery.rs` (which document admits a
//! delivery to a function or a queue target) and
//! `services/service-com-amazonaws-eventbridge-awsevents/tests/all/role_granted_function_target_runs_its_handler.rs`
//! (the handler a role-granted target invocation really runs).
//!
//! What is left is this file, and only because the fixer tool set that wrote it can write a
//! file but not remove one. `automod` declares it as an empty module, which runs no test; the
//! next change to touch this directory should delete it.
