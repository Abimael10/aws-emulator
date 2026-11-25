// SPDX-License-Identifier: CC0-1.0

//! The `Service` object `GetService` reports a DISCOVERED service through, as AWS carries
//! it.
//!
//! Application Signals does not create services: it DISCOVERS them from the telemetry an
//! instrumented application sends, so the members of this object are the ones it observed
//! for the identity and window a read asks about. A read of an identity it observed nothing
//! for therefore carries no key attributes and no metric references at all — the recorded
//! 200 for such a read carries the `Service` object with NEITHER member — and an absent
//! member has to be absent on the wire rather than present and empty, because "no metrics
//! observed" and "metrics observed, and there are none" are different answers to the
//! caller.
//!
//! That is why the shape is declared here rather than beside `GetService`'s other types:
//! the published snapshot declares `KeyAttributes` and `MetricReferences` `@required`, and
//! that snapshot is kept as shipped (see `smithy-model/LOCAL-ADDITIONS.md`), so the shape is
//! generated from the small overlay model `smithy-model/local-additions.json`, which
//! restates it with those two members optional — their targets, and so every length and
//! pattern constraint, exactly as the snapshot spells them. Nothing about the wire is
//! hand-written: the member names, the shape ids and the constraints all come from that
//! file.
//!
//! What the members CARRY is not decided here: it is
//! [`crate::state::ComAmazonawsApplicationsignalsApplicationsignalsState::observed_service`],
//! so this read and the topology reads beside it cannot disagree about one entity.
#![allow(unused_imports)]

use super::*;

smithy_macros::smithy_model!("smithy-model/local-additions.json");

/// One service as `GetService` reports it.
///
/// Every member is optional: each is populated from what this emulator observed for the
/// requested identity over the requested window, and an identity nothing was observed for
/// carries none of them.
#[smithy_macros::smithy(shape = "com.amazonaws.applicationsignals#Service")]
#[derive(Debug, Clone, Default)]
pub struct Service {
    pub key_attributes: Option<std::collections::HashMap<String, String>>,
    pub attribute_maps: Option<Vec<std::collections::HashMap<String, String>>>,
    pub service_groups: Option<Vec<ServiceGroup>>,
    pub metric_references: Option<Vec<MetricReference>>,
    pub log_group_references: Option<Vec<std::collections::HashMap<String, String>>>,
}
