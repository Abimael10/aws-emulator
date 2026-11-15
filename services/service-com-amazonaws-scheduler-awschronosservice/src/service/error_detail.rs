// SPDX-License-Identifier: CC0-1.0

//! WHICH member of an error body this service reports a failure's text in: `Message`.
//!
//! # The rule
//!
//! Every error shape this model declares carries exactly one member, `Message`, `@required`
//! and targeting `String`, and none of them carries a `@jsonName`
//! (`smithy_query(query="shape",shape="ConflictException,InternalServerException,ResourceNotFoundException,ServiceQuotaExceededException,ThrottlingException,ValidationException")`),
//! and a `restJson1` member with no `@jsonName` goes on the wire under its Smithy name. So
//! `Message` is where one of this service's errors carries its text, whatever produced it:
//!
//! ```json
//! HTTP/1.1 400 Bad Request
//! content-type: application/json
//! x-amzn-errortype: ValidationException
//!
//! {"__type":"ValidationException","Message":"1 validation error detected: …"}
//! ```
//!
//! An error a HANDLER returns is serialized from the model's own shape, so it already carries
//! that member. The body the FRAMEWORK renders for a request it refused before any handler ran
//! does not: that body is built from a synthetic shape whose own member is spelled `message`,
//! and the JSON error renderer writes that spelling even when the refusal is named by one of
//! this service's MODELLED error shapes — which every constraint refusal here is, since
//! `ValidationException` is what [`super::validation`] names it. A client, or an SDK
//! deserializing the modelled exception that code names, then finds no `Message` member at all
//! and surfaces the refusal with no text at all, though its status and its code are right.
//!
//! This layer therefore reports that text in `Message` on every error body this service answers
//! whose `__type` names one of those shapes, wherever it was produced. The renaming itself is
//! `service-common`'s, so this file states only the two member names and the shapes the rule
//! covers.
//!
//! What is deliberately left alone: the status, the `__type` discriminator, the
//! `x-amzn-errortype` header, every other member and the whole success path; a body that
//! already carries `Message` — every error a handler serialized — is returned untouched, so
//! the rename can never overwrite words a handler chose; and a failure whose class this model
//! does NOT declare (a request naming no modelled operation, which this service answers with
//! the framework's `NotImplemented`) keeps the member the framework wrote its text in, because
//! the rename is keyed on the modelled shapes rather than on the status.
//!
//! The structural home for the rule is the framework's error renderer, which writes the
//! synthetic shape's `message` for every service; that renderer is in the agent-immutable
//! `vendor/` tree, so this service states it at the one seam that sees a whole rendered
//! response — the per-operation wrapper in [`super`], which every modelled operation is
//! already served through.

use aws_smithy_runtime_api::http::Response as RtResponse;

/// The body member this service's errors carry their text in — the member name every error
/// shape in the model declares; see the module header for the `smithy_query` call it comes
/// from.
const MODELLED_MESSAGE_MEMBER: &str = "Message";

/// The body member the FRAMEWORK's own refusal renderer writes that text in: the member of the
/// synthetic shape it builds a pre-handler refusal from.
const RENDERED_MESSAGE_MEMBER: &str = "message";

/// Every error shape this model declares, by the wire code (`__type`) it renders under — the
/// same `smithy_query` call the module header cites, which is where this list comes from and
/// the only thing that may extend it. `tests` below reads the set off the model, so a shape
/// the model gains and this list does not fails there.
const MODELLED_ERROR_SHAPES: [&str; 6] = [
    "ConflictException",
    "InternalServerException",
    "ResourceNotFoundException",
    "ServiceQuotaExceededException",
    "ThrottlingException",
    "ValidationException",
];

/// `resp` as this service answers it: a modelled error body whose text is in its `Message`
/// member, through the one renaming `service-common` states for every service whose wire
/// member differs from the one a layer wrote.
#[must_use]
pub(super) fn with_modelled_message_member(resp: RtResponse) -> RtResponse {
    ::service_common::error_detail_member::with_renamed_detail_member(
        resp,
        &MODELLED_ERROR_SHAPES,
        RENDERED_MESSAGE_MEMBER,
        MODELLED_MESSAGE_MEMBER,
    )
}

#[cfg(test)]
mod tests {
    use aws_smithy_types::body::SdkBody;
    use serde_json::Value;

    use super::*;

    /// This service's Smithy model, as it ships beside this crate. Read at test time rather
    /// than embedded: it is megabytes of JSON that no non-test build needs.
    const MODEL_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/smithy-model/com-amazonaws-scheduler-awschronosservice.json"
    );

    /// A response at `status` carrying `body`.
    fn response(status: u16, body: &str) -> RtResponse {
        RtResponse::new(
            aws_smithy_runtime_api::http::StatusCode::try_from(status).expect("a valid status"),
            SdkBody::from(body),
        )
    }

    /// The body of `resp` as JSON.
    fn decoded(resp: &RtResponse) -> Value {
        serde_json::from_slice(resp.body().bytes().expect("an in-memory body"))
            .expect("the body is JSON")
    }

    /// GUARD on [`MODELLED_ERROR_SHAPES`]: the set is read off the MODEL rather than off a
    /// second hand list, so an error shape the model declares and this list does not — whose
    /// refusals would keep the framework's member — fails here.
    #[test]
    fn the_modelled_error_shapes_are_the_ones_the_model_declares() {
        let model = std::fs::read_to_string(MODEL_PATH)
            .expect("the model that generated this crate ships beside it");
        let model: Value =
            serde_json::from_str(&model).expect("the model that generated this crate is JSON");
        let shapes = model["shapes"]
            .as_object()
            .expect("a Smithy model declares shapes");
        let mut declared: Vec<&str> = shapes
            .iter()
            .filter(|(_, shape)| {
                shape["type"] == "structure" && shape["traits"]["smithy.api#error"].is_string()
            })
            .filter_map(|(id, _)| id.rsplit('#').next())
            .collect();
        declared.sort_unstable();
        assert_eq!(
            declared,
            MODELLED_ERROR_SHAPES.to_vec(),
            "expected the model's error shapes, got {declared:?}"
        );

        // Each of them carries its text in the member this rule moves it to, and none renames
        // it on the wire: that is what makes writing `Message` the faithful answer.
        for shape in MODELLED_ERROR_SHAPES {
            let members = shapes
                .iter()
                .find(|(id, _)| id.rsplit('#').next() == Some(shape))
                .map(|(_, shape)| shape["members"].clone())
                .unwrap_or(Value::Null);
            assert!(
                members[MODELLED_MESSAGE_MEMBER].is_object(),
                "case {shape:?}: expected a {MODELLED_MESSAGE_MEMBER} member, got {members}"
            );
            assert!(
                members[MODELLED_MESSAGE_MEMBER]["traits"]["smithy.api#jsonName"].is_null(),
                "case {shape:?}: a @jsonName would change the wire member, got {members}"
            );
        }
    }

    /// RULE: one row per error shape this model declares — a refusal the framework rendered
    /// under each of the codes this service can name — so the member is a property of the
    /// error envelope rather than of one recorded body.
    #[test]
    fn every_modelled_error_body_reports_its_text_in_the_message_member() {
        for code in MODELLED_ERROR_SHAPES {
            let text = format!("{code} states its own sentence");
            let body = format!(r#"{{"__type":"{code}","message":"{text}"}}"#);
            let answered = with_modelled_message_member(response(400, &body));
            let document = decoded(&answered);
            assert_eq!(
                document.get(MODELLED_MESSAGE_MEMBER).and_then(Value::as_str),
                Some(text.as_str()),
                "case {code:?}: expected the text in {MODELLED_MESSAGE_MEMBER}, got {document}"
            );
            assert!(
                document.get(RENDERED_MESSAGE_MEMBER).is_none(),
                "case {code:?}: the text must be carried under ONE member, got {document}"
            );
            assert_eq!(
                document.get("__type").and_then(Value::as_str),
                Some(code),
                "case {code:?}: the discriminator must survive the rename, got {document}"
            );
        }
    }

    /// RULE: a body this rule does not own is answered byte for byte as its renderer wrote it
    /// — a success, a class this model does not declare, and a body that already carries the
    /// modelled member.
    #[test]
    fn a_body_this_rule_does_not_own_is_answered_unchanged() {
        // (case, status, the body)
        let rows: &[(&str, u16, &str)] = &[
            (
                "a success whose own members are not an error's",
                200,
                r#"{"Name":"a-schedule","GroupName":"default"}"#,
            ),
            (
                "a request naming no modelled operation, a class this model does not declare",
                501,
                r#"{"__type":"NotImplemented","message":"operation GET /nope is not implemented"}"#,
            ),
            (
                "a body the codec itself refused, another class this model does not declare",
                400,
                r#"{"__type":"SerializationException","message":"serde error"}"#,
            ),
            (
                "a modelled error a handler already serialized",
                404,
                r#"{"__type":"ResourceNotFoundException","Message":"Schedule a does not exist."}"#,
            ),
        ];

        for (case, status, body) in rows {
            let answered = with_modelled_message_member(response(*status, body));
            assert_eq!(
                answered.body().bytes().expect("an in-memory body"),
                body.as_bytes(),
                "case {case:?}: expected the body unchanged"
            );
        }
    }
}
