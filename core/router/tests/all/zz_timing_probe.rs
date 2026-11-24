// SPDX-License-Identifier: CC0-1.0

//! Nothing: this module carries no suite, like `zz_scratch_toggle.rs` and `appended_prose.rs`
//! beside it.
//!
//! It is where a MEASUREMENT of the ALB fail-open scenario's own convergence stood while the
//! suite that pins the rule behind it was written
//! ([`crate::alb_health_check_cadence_the_request_asked_for`], whose header states that rule).
//! A suite of this target may not assert an elapsed time, so what the measurement showed is
//! asserted there as the CONDITION each step polls for, and nothing of it is left here. The file
//! stays because `automod` declares one module per `.rs` file under `tests/all/`, and this
//! target refuses a file it cannot declare — it is to be removed by any change that can delete a
//! path.
