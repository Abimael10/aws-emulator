// SPDX-License-Identifier: CC0-1.0

//! WHY this service's `awsJson1_0` wire needs a correction for a TIMESTAMP, and where the
//! correction itself lives.
//!
//! RULE: a timestamp member whose model declares `@timestampFormat("date-time")` goes onto
//! the JSON wire as an RFC 3339 / ISO 8601 date-time STRING naming the instant
//! (`"2026-11-09T12:34:56.789Z"`), never as the epoch-seconds NUMBER the `awsJson1_0`
//! protocol defaults to.
//!
//! The rule is the whole service's, not one shape's. This model declares exactly ONE
//! timestamp shape, `com.amazonaws.mwaaserverless#TimestampValue`, and it carries
//! `@timestampFormat("date-time")`
//! (`smithy_query(query="shape", shape="TimestampValue",
//! model_path="services/service-com-amazonaws-mwaaserverless-amazonmwaaserverless/smithy-model/com-amazonaws-mwaaserverless-amazonmwaaserverless.json")`),
//! so every timestamp member of every shape of this service targets it and the model hoists
//! the format onto each one: a workflow's `CreatedAt` / `ModifiedAt` / `CodeSnapshottedAt`,
//! a run's `CreatedAt` / `StartedOn` / `CompletedOn` / `ModifiedAt` / `StartedAt` /
//! `EndedAt` / `CreatedOn`, and a task instance's `StartedAt` / `EndedAt` / `ModifiedAt`.
//! Each member's own documentation says the same in prose ("in ISO 8601 date-time format").
//! No INPUT member of this model is a timestamp, and no error shape of it carries one.
//!
//! WHY THE CORRECTION IS NEEDED AT ALL. The JSON codec resolves a timestamp's wire form
//! from the MEMBER schema alone (`vendor/smithy-schema-server/src/json.rs`'s
//! `write_timestamp` reads `Schema::timestamp_format()`) and falls back to the protocol
//! default of epoch seconds when that schema reports no format. The schemas built from a
//! model carry no member-level format at all (grep `vendor/smithy-macros`: it emits no
//! `with_timestamp_format`), so every instant this service reported went out as a bare
//! number, which a client deserializing a `date-time` member either fails to parse or
//! decodes to an instant decades away.
//!
//! WHERE IT LIVES: [`::service_common::date_time_json`], the tree's one reading of this
//! rule — the same module every other service of this class takes it from, so two services
//! cannot answer one instant differently. This service's own choice is only the one
//! re-exported below, taken at `crate::service::EmulatedService`'s `override_protocol`.

/// The model's own `awsJson1_0`, with each `date-time` timestamp written in the form the
/// model declares. See this module's own documentation for why this service takes it.
pub use ::service_common::date_time_json::DateTimeAwsJson;
