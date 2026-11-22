// SPDX-License-Identifier: CC0-1.0

//! How this service's `restJson1` ERROR document carries a modelled exception's
//! `message` member.
//!
//! RULE: every error document SES API v2 answers with CARRIES the error shape's
//! `message` member — when the service supplies no message the member is written with
//! JSON `null` rather than left out — so a client can tell "no message was supplied"
//! from "an empty message was supplied".
//!
//! Every error shape in this service's model carries exactly one optional member,
//! `message` (target `ErrorMessage`, a string with no HTTP binding:
//! `smithy_query(query="shape",shape="NotFoundException",
//! model_path="services/service-com-amazonaws-sesv2-simpleemailservice-v2/smithy-model/com-amazonaws-sesv2-simpleemailservice-v2.json")`),
//! and the multi-region-endpoint reads answer `NotFoundException` with no message at
//! all. The framework's generic serializer writes an absent optional member by writing
//! NOTHING, which is right for a success body but drops the key out of the error
//! document: every SDK then reports the error's message as the empty string it
//! substitutes for an absent key, where SES's own answer reads as "none".
//!
//! That rule is not this service's alone, so the codec implementing it is the shared
//! [`service_common::error_message_member`]'s and this module only states which codec
//! this service takes. [`MessagePresentRestJson`] is the protocol `service.rs` hands the
//! dispatch sequence through [`smithy_server_runtime::Service::override_protocol`], so
//! EVERY modelled failure of EVERY operation goes out with the member present and no
//! handler, shape declaration or error constructor mentions the rendering at all.
//! Everything else — routing, request deserialization, success bodies, and an error whose
//! message IS supplied — is the framework's own restJson1 protocol, delegated to
//! unchanged.

pub use service_common::error_message_member::MessagePresentRestJson;
