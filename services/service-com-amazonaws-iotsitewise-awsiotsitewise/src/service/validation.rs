// SPDX-License-Identifier: CC0-1.0

//! How IoT SiteWise states the refusal the framework hands it for a request that
//! broke one of the model's own rules.
//!
//! The framework hands the service that refusal STRUCTURALLY — one
//! [`Violation`] per constraint the request broke, carrying the member path, the
//! requirement it failed and the value it held — and leaves the words to the
//! service, because no model records them. The envelope
//! (`N validation error(s) detected: `), the member paths and every requirement's
//! wording are the framework's and the shared home's, so nothing about them can
//! drift per crate. What this module decides is the four things THIS service's
//! report does differently:
//!
//! 1. a clause of a refusal from the CONTROL plane or the Monitor plane names the
//!    member and the requirement and states NO VALUE — neither the value a present
//!    member sent nor the `null` the framework writes for a member that was not
//!    there at all — while the DATA endpoint's quotes the value it rejected: the
//!    frontends word this report differently, and which of them answered is read
//!    off the model ([`super::endpoints`]).
//! 2. an enumerated type's value set is listed in the order the set's own hash
//!    table is walked
//!    (`service_common::validation_message::in_hashed_enum_order`), which is not the
//!    order the model declares the type's values in.
//! 3. a member that broke SEVERAL of its requirements states the expression it had
//!    to match first (`service_common::validation_message::in_pattern_first_order`):
//!    the expression is the shape of the value and every other requirement is a
//!    bound on it.
//! 4. the expression a `@pattern` requirement quotes is the one the ANSWERING
//!    endpoint publishes for the member's shape ([`published_expression_at`]).
//!
//! A fifth thing is not this service's choice but AWS's, and the whole refusal is
//! handed to the shared home that states it: a constraint one of a COLLECTION's
//! elements broke is stated at the collection MEMBER — no position and no `member`
//! step — with the element shape's whole constraint set nested inside
//! `Member must satisfy constraint: [ … ]`, while a violation on the collection
//! itself, or on a member inside a STRUCTURE element, keeps its own path. The set is
//! read off the input shape being read ([`super::input_shape::element_set`]),
//! because a refusal carries only the constraint that failed.
//!
//! Nothing here is keyed to an operation, a member name or a value: each rule is
//! applied to whatever violation the walk produced, so it holds for every
//! constrained member of every operation — the `@range` on a `maxResults`, the
//! `@length` and the expression on an id or a client token, the enumerated type of
//! a property's data type, and a member of a list element alike.
//!
//! The clauses themselves are stated over the violations the walk found PLUS the
//! requirement that walk declares but cannot apply
//! ([`super::input_shape::with_unapplied_patterns`]), so a member that broke its
//! expression and one of its bounds is reported with both.

use ::service_common::validation::{unanchored, unanchored_at_the_close};
use ::service_common::validation_message::{
    aws_validation_refusal_over_elements_stating_withholding, in_hashed_enum_order,
    in_pattern_first_order,
};
use ::smithy_server_runtime::{Refusal, Refused, Violation};

/// The sentence a refusal that names no member at all is stated with: this
/// service's own description of its client-input error
/// (`smithy_query(query="shape", shape="InvalidRequestException")`). Reached only
/// for an empty violation list, which carries no member to name.
const THE_REQUEST_IS_NOT_VALID: &str = "The request isn't valid.";

/// The expressions the endpoints OTHER than the data plane publish WITHOUT the `$`
/// that CLOSES them, keyed by the expression, because what is published belongs to
/// the member's SHAPE and so holds for every member targeting it — in any operation
/// of those endpoints, in whatever binding carried the value.
///
/// A `CustomID` reference is published that way: its text is an alternation of two
/// self-anchored branches, and those endpoints quote it less the anchor that closes
/// the second. Every other expression of this model is quoted there exactly as the
/// model declares it, anchors included (`AssetModelVersionFilter`'s
/// `^(LATEST|ACTIVE)$` is one). Which of the two a shape is published with cannot be
/// read off the text — both are spelled `^…$` — so it is reference data about the
/// shape; the DROP itself is the shared home's, called rather than restated. The
/// DATA endpoint publishes neither anchor of a wrapping pair, which is a rule about
/// the ENDPOINT rather than about one shape ([`published_expression_at`]).
const PUBLISHED_UNANCHORED_AT_THE_CLOSE: [&str; 1] = [crate::state::CUSTOM_ID_PATTERN];

/// `regex` as every endpoint OTHER than the data plane publishes it, which is what
/// a `@pattern` requirement of one of their operations quotes back.
///
/// The CHECK always runs the model's expression as written; only this rendering
/// decides how it is quoted. Every caller that states such a requirement — the
/// boundary's report below and the operations that state a reference's syntax
/// themselves ([`crate::state::custom_id_clause`]) — goes through here, so one
/// expression cannot be published two ways by one endpoint.
#[must_use]
pub fn published_expression(regex: &str) -> String {
    if PUBLISHED_UNANCHORED_AT_THE_CLOSE.contains(&regex) {
        return unanchored_at_the_close(regex);
    }
    regex.to_string()
}

/// `regex` as the endpoint that ANSWERED publishes it.
///
/// The DATA endpoint publishes an expression without the pair of anchors the model
/// wraps a whole-value expression in — the `^` that opens it and the `$` that
/// closes it — so what it quotes is the expression itself; the drop is the shared
/// home's one reading of that pair, called rather than restated. Every other
/// endpoint publishes what [`published_expression`] states. Which endpoint answered
/// is the model's own classification of the operation ([`super::endpoints`]), so
/// this is keyed to no operation, member or value.
#[must_use]
pub fn published_expression_at(data_endpoint: bool, regex: &str) -> String {
    if data_endpoint {
        return unanchored(regex);
    }
    published_expression(regex)
}

/// The violations one report is stated over, in the order it states them: those the
/// walk found plus the requirement it could not apply, each member's expression ahead
/// of that member's other requirements, and each enumerated value set in the order
/// this service lists one.
///
/// The `@pattern` RENDERING is not applied here: the report below hands
/// [`published_expression`] to the shared home, which quotes every clause's
/// expression through it — the nested set a clause states at a collection member
/// included.
fn restated(violations: &[Violation]) -> Vec<Violation> {
    let restored = super::input_shape::with_unapplied_patterns(violations);
    in_pattern_first_order(&restored)
        .into_iter()
        .map(in_hashed_enum_order)
        .collect()
}

/// How this service states `refusal`, or `None` to keep the framework's wording.
///
/// EVERY constraint refusal is restated, with no fall-through: the code travels with
/// the message, and a refusal this service declined to word would be answered with
/// the framework's default code, `ValidationException`, which names no shape of this
/// model at all — so no generated SDK exception class exists for it and a client's
/// `except InvalidRequestException` never fires. An authentication refusal is the
/// emulator's rather than this service's, and keeps the framework's 403 wording.
///
/// Only the CODE is this service's as a whole: the message is the shared home's
/// rendering of AWS's report over the violations above, in the dialect of the
/// ENDPOINT that answered — the control and Monitor planes withhold every value and
/// quote an expression as they publish one, the data plane states the value and
/// quotes the expression unanchored.
pub(super) fn refused(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    let data_endpoint = super::input_shape::reading_the_data_endpoint();
    let message = aws_validation_refusal_over_elements_stating_withholding(
        &Refusal::Invalid(restated(violations)),
        |regex| published_expression_at(data_endpoint, regex),
        |_| !data_endpoint,
        super::input_shape::element_set,
    )
    .map_or_else(
        || THE_REQUEST_IS_NOT_VALID.to_string(),
        |stated| stated.message,
    );
    Some(Refused {
        code: super::INVALID_REQUEST.to_string(),
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::smithy_server_runtime::ViolationKind;

    /// One violation of `kind` at `path`, for the value a request sent.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: value.map(str::to_string),
            sensitive: false,
        }
    }

    /// RULE: an expression is quoted as the service publishes it — the `CustomID`
    /// reference less the anchor that closes it, every other expression of this model
    /// as the model declares it.
    #[test]
    fn an_expression_is_quoted_as_the_service_publishes_it() {
        let custom_id = crate::state::CUSTOM_ID_PATTERN;
        let rows: [(&str, &str, String); 4] = [
            (
                "the CustomID reference",
                custom_id,
                custom_id
                    .strip_suffix('$')
                    .expect("the declared expression closes with an anchor")
                    .to_string(),
            ),
            (
                "an asset-model version filter",
                "^(LATEST|ACTIVE)$",
                "^(LATEST|ACTIVE)$".to_string(),
            ),
            (
                "a version number",
                r"^(0|([1-9]{1}\d*))$",
                r"^(0|([1-9]{1}\d*))$".to_string(),
            ),
            ("an expression with no anchors", "[a-z]+", "[a-z]+".to_string()),
        ];
        for (case, declared, expected) in rows {
            let actual = published_expression(declared);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The message this service states for `violations`, with nothing recorded about
    /// the input shape being read — so the dialect is the control plane's, which is
    /// what every row below is about.
    fn stated(violations: Vec<Violation>) -> String {
        super::super::input_shape::forget();
        refused(&Refusal::Invalid(violations))
            .expect("a constraint refusal is this service's to word")
            .message
    }

    /// The message this service states for `violations` with the input shape
    /// `input_schema` being read, which is what names the endpoint that answered.
    fn stated_reading(input_schema: &::aws_smithy_schema::Schema, violations: Vec<Violation>) -> String {
        super::super::input_shape::reading_input(input_schema);
        refused(&Refusal::Invalid(violations))
            .expect("a constraint refusal is this service's to word")
            .message
    }

    /// RULE: the DATA endpoint states the value it rejected and quotes the member's
    /// expression without the pair of anchors the model wraps it in, while every
    /// other endpoint states neither — the one difference between the frontends'
    /// reports.
    ///
    /// Driven over the real input shapes of one operation per endpoint
    /// (`smithy_query(query="operation", operation="GetAssetPropertyAggregates")` —
    /// hosted on `data.`, `resolution` carrying `@pattern ^1m|15m|1h|1d$` — and
    /// `DescribeAssetModel`, hosted on `api.`, `assetModelVersion` carrying
    /// `^(LATEST|ACTIVE)$`), so which dialect answers is the model's classification
    /// rather than anything written here.
    #[test]
    fn the_endpoint_that_answered_decides_the_dialect() {
        let data = stated_reading(
            crate::ops::types::GET_ASSET_PROPERTY_AGGREGATES_INPUT_SCHEMA,
            vec![violation(
                "resolution",
                ViolationKind::Pattern("^1m|15m|1h|1d$".to_string()),
                Some("2d"),
            )],
        );
        let expected = "1 validation error detected: Value '2d' at 'resolution' failed to \
                        satisfy constraint: Member must satisfy regular expression pattern: \
                        1m|15m|1h|1d";
        assert_eq!(
            data, expected,
            "the data endpoint: expected {expected:?}, got {data:?}"
        );

        let control = stated_reading(
            crate::ops::types::DESCRIBE_ASSET_MODEL_INPUT_SCHEMA,
            vec![violation(
                "assetModelVersion",
                ViolationKind::Pattern("^(LATEST|ACTIVE)$".to_string()),
                Some("active"),
            )],
        );
        let expected = "1 validation error detected: Value at 'assetModelVersion' failed to \
                        satisfy constraint: Member must satisfy regular expression pattern: \
                        ^(LATEST|ACTIVE)$";
        assert_eq!(
            control, expected,
            "the control plane: expected {expected:?}, got {control:?}"
        );
    }

    /// RULE: a clause states the member and the requirement and no value — not the
    /// value a present member sent, and not the `null` written for a member that was
    /// not there at all — whatever the requirement's class. The branch no input shape
    /// of this model can reach through a request — a `@sensitive` member's withheld
    /// value — is pinned here beside the ones it can.
    #[test]
    fn a_clause_states_the_requirement_and_no_value() {
        let rows: [(&str, Violation, &str); 4] = [
            (
                "a broken length minimum",
                violation("hierarchyId", ViolationKind::MinLength(13), Some("Turbines")),
                "1 validation error detected: Value at 'hierarchyId' failed to satisfy \
                 constraint: Member must have length greater than or equal to 13",
            ),
            (
                "a broken range minimum",
                violation("maxResults", ViolationKind::MinValue("1".to_string()), Some("0")),
                "1 validation error detected: Value at 'maxResults' failed to satisfy \
                 constraint: Member must have value greater than or equal to 1",
            ),
            (
                "an absent required member",
                violation("assetModelName", ViolationKind::MissingRequiredMember, None),
                "1 validation error detected: Value at 'assetModelName' failed to satisfy \
                 constraint: Member must not be null",
            ),
            (
                "a withheld value",
                Violation {
                    sensitive: true,
                    ..violation("clientToken", ViolationKind::MinLength(36), None)
                },
                "1 validation error detected: Value at 'clientToken' failed to satisfy \
                 constraint: Member must have length greater than or equal to 36",
            ),
        ];
        for (case, broken, expected) in rows {
            let actual = stated(vec![broken]);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the report counts its clauses, and a member that broke several of its
    /// requirements states the expression it had to match first.
    #[test]
    fn the_report_counts_its_clauses_and_states_an_expression_first() {
        let length = violation("hierarchyId", ViolationKind::MinLength(13), Some("Turbines"));
        let pattern = violation(
            "hierarchyId",
            ViolationKind::Pattern("^[0-9a-f]{8}$".to_string()),
            Some("Turbines"),
        );
        let actual = stated(vec![length, pattern]);
        let expected = "2 validation errors detected: Value at 'hierarchyId' failed to satisfy \
                        constraint: Member must satisfy regular expression pattern: \
                        ^[0-9a-f]{8}$; Value at 'hierarchyId' failed to satisfy constraint: \
                        Member must have length greater than or equal to 13";
        assert_eq!(
            actual, expected,
            "expected {expected:?}, got {actual:?}"
        );
    }

    /// RULE: an enumerated type's value set is listed in the order the set's own hash
    /// table is walked, not the order the model declares it in. Driven over
    /// `PropertyDataType` as the model declares it
    /// (`smithy_query(query="shape", shape="PropertyDataType")`).
    #[test]
    fn an_enumerated_value_set_is_listed_in_the_order_the_service_lists_it() {
        let declared = [
            "STRING",
            "INTEGER",
            "DOUBLE",
            "BOOLEAN",
            "STRUCT",
            "VIDEO",
            "ANNOTATION",
            "JSON",
        ]
        .iter()
        .map(|value| (*value).to_string())
        .collect();
        let actual = stated(vec![violation(
            "assetModelProperties.1.member.dataType",
            ViolationKind::EnumValue(declared),
            Some("double"),
        )]);
        let expected = "1 validation error detected: Value at \
                        'assetModelProperties.1.member.dataType' failed to satisfy constraint: \
                        Member must satisfy enum value set: [JSON, BOOLEAN, ANNOTATION, DOUBLE, \
                        STRING, STRUCT, VIDEO, INTEGER]";
        assert_eq!(
            actual, expected,
            "expected {expected:?}, got {actual:?}"
        );
    }

    /// An authentication refusal is not this service's to word.
    #[test]
    fn only_a_constraint_refusal_is_restated() {
        assert!(
            refused(&Refusal::Unauthenticated).is_none(),
            "an authentication refusal keeps the framework's wording"
        );
    }
}
