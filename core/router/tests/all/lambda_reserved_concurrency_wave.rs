// SPDX-License-Identifier: CC0-1.0

//! Nothing: this module is empty on purpose.
//!
//! It was started as a cross-service suite over the invocations a function's reserved
//! concurrency admits, and the rule it was to pin — every invocation Lambda admits runs beside
//! the other admitted ones, whatever each run's `MemorySize` — belongs to the service that
//! schedules those runs: it is asserted end to end, with Amazon DynamoDB and IAM mounted
//! beside Lambda on one engine, by that crate's own `tests/all/reserved_concurrency_wave.rs`.
//! A second copy here would deploy the same handler through a stack to assert the same thing.
//!
//! The module stays because every file under `tests/all/` is compiled as a suite of this
//! target, and nothing here removes one.
