// SPDX-License-Identifier: CC0-1.0

//! How this service words a `SourceType` the model's `SourceType` enum does not
//! hold.
//!
//! `DescribeEvents$SourceType` is the one member of this service's API that
//! targets that enum, so the framework finds a value outside it while validating
//! the request and refuses it before the handler runs — structurally, as a
//! [`Refusal::Invalid`] carrying a [`ViolationKind::EnumValue`] at the member's
//! path. The framework's own wording for that violation names the constraint
//! (`Member must satisfy enum value set: [...]`) under the generic validation
//! code, which is not one of `DescribeEvents`' modelled errors; ElastiCache
//! states it as a bad parameter value instead: HTTP 400,
//! `InvalidParameterValue`, and one fixed sentence that lists the event source
//! types it serves.
//!
//! The sentence is written out here rather than built from the enum: the service
//! lists the types in its OWN order, which is not the model's, and it
//! interpolates nothing from the request — so a client reads the same sentence
//! whatever it asked for.

use smithy_server_runtime::{Refusal, Refused, ViolationKind};

/// The awsQuery error code ElastiCache names a bad parameter value with, which
/// is what the `InvalidParameterValueException` this operation models renders
/// as.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The path the framework spells `DescribeEvents$SourceType` at: a member names
/// itself in lowerCamel, and this one is at the top level of the request.
const SOURCE_TYPE_PATH: &str = "sourceType";

/// The whole sentence an unknown event source type is refused with.
const INVALID_EVENT_SOURCE_TYPE: &str = "Invalid event source type. Valid types are \
     cache-cluster,cache-parameter-group,cache-security-group,cache-subnet-group,\
     replication-group,user,user-group,serverless-cache,serverless-cache-snapshot";

/// How this service words `refusal` when it is an unknown event source type, or
/// `None` to leave it to the renderer that would otherwise answer.
///
/// Only a value outside the ENUM at that member is reworded: a request refused
/// for anything else keeps the diagnosis that names what it failed.
pub fn event_source_type_refusal(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    let names_the_source_type = violations.iter().any(|violation| {
        violation.path == SOURCE_TYPE_PATH
            && matches!(violation.kind, ViolationKind::EnumValue(_))
    });
    names_the_source_type.then(|| Refused {
        code: INVALID_PARAMETER_VALUE.to_string(),
        message: INVALID_EVENT_SOURCE_TYPE.to_string(),
    })
}
