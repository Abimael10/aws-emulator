// SPDX-License-Identifier: CC0-1.0

//! The sentence this service refuses an event source type outside the model's `SourceType`
//! enum with.
//!
//! `DescribeEvents$SourceType` is the one member of this API that targets that enum, so the
//! framework finds a value outside it while validating the request and refuses it before the
//! handler runs. WHICH code and status that refusal carries, and how it is recognised, is the
//! one rule every enum-valued member of this API shares and lives in
//! [`super::enum_value`]; the sentence below is this member's own.
//!
//! It is written out rather than built from the enum, and it is the WHOLE message: the
//! service lists the types in its OWN order, which is not the model's, and it interpolates
//! nothing from the request — so a client reads the same sentence whatever it asked for.

/// The whole sentence an unknown event source type is refused with; see the module docs.
pub const INVALID_EVENT_SOURCE_TYPE: &str = "Invalid event source type. Valid types are \
     cache-cluster,cache-parameter-group,cache-security-group,cache-subnet-group,\
     replication-group,user,user-group,serverless-cache,serverless-cache-snapshot";
