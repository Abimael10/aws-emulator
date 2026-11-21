// SPDX-License-Identifier: CC0-1.0

//! How this service refuses a parameter whose value its model does not admit.
//!
//! Every such refusal of this service is the one class its model documents for it —
//! `InvalidParameterValueException`, "The value that you provided for the specified
//! parameter is invalid." (`smithy_query(query="shape",shape="InvalidParameterValueException")`:
//! a client error, `@httpError` 400, carrying one `message`) — and one SENTENCE: the
//! parameter the value sat at, as the API addresses it, and the requirement that value had
//! to satisfy. The sentence and every clause in it are the shared statements of them
//! (`service_common::validation_message`), so this module chooses only what is this
//! service's: which constraint classes it states that way, and how it quotes the
//! expression an ARN member's value had to match.
//!
//! ## Which parameter the sentence names
//!
//! The one the REQUEST carried, spelled as the API addresses it: a list element by its
//! zero-based index on the member that holds it
//! (`distributions[0].amiDistributionConfiguration.name`), never by the framework walk's own
//! `distributions.1.member.…`, which names no parameter of this API.
//!
//! ## Which requirement it states
//!
//! The one the value broke, for the constraint classes this service's recorded refusals
//! establish a clause for: a `@pattern`, a `@range` bound, an enumerated value set, and both
//! `@length` bounds of a COLLECTION ([`StatedCollectionLength`]). A class they do not — a
//! `@length` bound on a STRING, a repeated element of a `@uniqueItems` list — is left to the
//! framework's own refusal rather than being given a clause this service has never been seen
//! to state. Three of them, the `@range` MINIMUM and a collection's two bounds, this service
//! words its own way ([`must_be_at_least`], [`StatedCollectionLength`]); every other clause is
//! the shared statement of it.
//!
//! A collection's `@length` bound is the class two of this service's listings word
//! differently from each other, so which clause is stated is a property of the operation the
//! request addressed, like the ARN form below: [`StatedCollectionLength`] carries the two, and
//! each operation is served behind the wording that operation states.
//!
//! A member the shape REQUIRES and the request OMITS is not one of those classes at all:
//! there is no value to state a requirement about, so the refusal is about the request as a
//! whole and names the properties it had to carry ([`missing_properties`]).
//!
//! ## How it quotes an ARN expression, and why that is per BINDING
//!
//! A refusal of this service quotes the expression an ARN member's value had to match in
//! the form the layer that READ the value writes it, and the two layers spell the partition
//! and Region fields differently: a value read out of the request BODY is refused with the
//! Region-code form (`arn:aws(?:-[a-z]+)*:imagebuilder:[a-z]{2,}(?:-[a-z]+)+-[0-9]+:`),
//! and a value read out of the REQUEST LINE — an `@httpQuery` parameter, an `@httpLabel`
//! path segment — with the character-class form
//! (`arn:aws(?:-[a-z0-9]+)*:imagebuilder:[a-z0-9-]*[a-z0-9]:`). The same member name is
//! bound both ways by different operations of this model (`imagePipelineArn` is
//! `GetImagePipeline`'s query parameter and `StartImagePipelineExecution`'s body member), so
//! no rule over the violated member alone can place a refusal: the binding is a property of
//! the operation's own input shape, which is why these wordings are attached to the front
//! door each operation is served behind (`service_common::front_door::FrontDoor`) rather
//! than to the service-level refusal seam.
//!
//! Every other field of an expression — the account, the resource kind, the name, the
//! version — is quoted exactly as the model carries it, and a pattern that is not an ARN's
//! (`semanticVersion`, `stepExecutionId`) is quoted as the model writes it, anchors
//! included: there is nothing to restate in it.
//!
//! One operation states no expression at all ([`unstated_pattern_refusal`]): its own check
//! reports that the value does not match the pattern the parameter requires and echoes none
//! of it.

use service_common::validation::SubmittedEcho;
use service_common::validation_message::{
    api_parameter_path, list_item_count, must_be_at_most, must_be_one_of, must_match_pattern,
    parameter_value_mismatch, pattern_requirement_unstated,
};
use smithy_server_runtime::{Refused, Violation, ViolationKind};

/// The error code this service answers a parameter whose value it refuses with: the modelled
/// `InvalidParameterValueException` (HTTP 400), whose own documentation is "The value that
/// you provided for the specified parameter is invalid."
///
/// This model declares NO `ValidationException` — its only service-common error is
/// `ServiceException` (`smithy_query(query="service_info")`) — so the framework's synthetic
/// one is not an error this service can answer with.
pub const INVALID_PARAMETER_VALUE_EXCEPTION: &str = "InvalidParameterValueException";

/// The error code this service answers a request that left out a member its operation
/// requires with: the modelled `InvalidRequestException` (HTTP 400), whose own documentation
/// is "The request is malformed or otherwise invalid. Verify the request and try again."
///
/// A member the shape REQUIRES and the request omits is not a value this service could refuse
/// as that parameter's — there is no value — so the refusal is about the request as a whole,
/// which is the class this one names ([`missing_properties`]).
pub const INVALID_REQUEST_EXCEPTION: &str = "InvalidRequestException";

/// The ARN partition field as the model spells it: `aws` followed by anything up to the
/// next colon, which is all a pattern needs in order to bound ONE field.
const MODELLED_PARTITION_FIELD: &str = "arn:aws[^:]*:";

/// The ARN Region field of this service as the model spells it: anything up to the next
/// colon, for the same reason as [`MODELLED_PARTITION_FIELD`].
const MODELLED_REGION_FIELD: &str = ":imagebuilder:[^:]+:";

/// The ARN partition field as a refusal of a BODY value states it: `aws` followed by the
/// hyphenated words a partition name is made of (`aws-cn`, `aws-us-gov`).
const REGION_CODE_PARTITION_FIELD: &str = "arn:aws(?:-[a-z]+)*:";

/// The ARN Region field as a refusal of a BODY value states it: the form a Region code takes
/// — an area, one or more hyphenated words, and a number (`us-east-1`, `ap-southeast-3`).
const REGION_CODE_REGION_FIELD: &str = ":imagebuilder:[a-z]{2,}(?:-[a-z]+)+-[0-9]+:";

/// The ARN partition field as a refusal of a value read out of the REQUEST LINE states it:
/// the characters a partition name is made of rather than the words.
const CHARACTER_CLASS_PARTITION_FIELD: &str = "arn:aws(?:-[a-z0-9]+)*:";

/// The ARN Region field as a refusal of a value read out of the REQUEST LINE states it: the
/// characters a Region code is made of, closing on one that is not a hyphen.
const CHARACTER_CLASS_REGION_FIELD: &str = ":imagebuilder:[a-z0-9-]*[a-z0-9]:";

/// How a refusal quotes the expression an ARN member's value had to match. See the module
/// docs: the form follows the layer that READ the value, not the member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatedPattern {
    /// The form a refusal of a value read out of the request BODY states.
    RegionCode,
    /// The form a refusal of a value read out of the REQUEST LINE states.
    CharacterClass,
    /// No expression at all: the requirement is stated as the pattern the parameter
    /// requires, with none of it echoed.
    Unstated,
}

impl StatedPattern {
    /// The requirement clause this form states for `member`, whose value had to match
    /// `pattern` as the model writes it.
    fn pattern_requirement(self, member: &str, pattern: &str) -> String {
        match self {
            Self::Unstated => pattern_requirement_unstated(member),
            Self::RegionCode => must_match_pattern(
                member,
                &pattern
                    .replace(MODELLED_PARTITION_FIELD, REGION_CODE_PARTITION_FIELD)
                    .replace(MODELLED_REGION_FIELD, REGION_CODE_REGION_FIELD),
            ),
            Self::CharacterClass => must_match_pattern(
                member,
                &pattern
                    .replace(MODELLED_PARTITION_FIELD, CHARACTER_CLASS_PARTITION_FIELD)
                    .replace(MODELLED_REGION_FIELD, CHARACTER_CLASS_REGION_FIELD),
            ),
        }
    }
}

/// The requirement clause this service states for a value below its member's `@range`
/// minimum: the bound the value had to reach.
///
/// Image Builder's own wording for this clause is "bigger than or equal to", which is not
/// how the shared statement of it reads ("greater than or equal to"), so it is stated here
/// rather than taken from `service_common::validation_message`. Its sibling — a value above
/// the maximum — is the shared clause ([`must_be_at_most`]), which Image Builder words the
/// same way the rest of the tree does.
fn must_be_at_least(member: &str, bound: &str) -> String {
    format!("{member} must be bigger than or equal to {bound}")
}

/// How a refusal of this service states the requirement a COLLECTION member whose `@length`
/// bound was broken had to satisfy.
///
/// A collection has no scalar rendering, so the clause states a COUNT rather than the length
/// of a value — which is why it is stated here instead of being taken from
/// `service_common::validation_message` — and in TWO wordings, because this service's
/// listings do not state one: the pipeline listing's own refusals name the modelled length
/// constraint the member had to satisfy, where every other refusal of this service that
/// states this class states how many items the member has to carry or may carry. Both quote
/// the bound as the model declares it, so a bound of one reads the same way a bound of several
/// does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatedCollectionLength {
    /// How many items the member had to carry: the clauses every operation of this service
    /// states but the one below.
    ItemCount,
    /// The modelled length constraint the member had to satisfy, under the name that refusal
    /// addresses the bound by: the clauses the pipeline listing states.
    ModelledConstraint,
}

impl StatedCollectionLength {
    /// The requirement clause this wording states for `member`, whose `@length` MINIMUM
    /// `bound` the request fell short of.
    fn at_least(self, member: &str, bound: i64) -> String {
        match self {
            Self::ItemCount => format!("{member} must contain at least {bound} items"),
            Self::ModelledConstraint => {
                format!("{member} length must satisfy min_items {bound}")
            }
        }
    }

    /// The requirement clause this wording states for `member`, whose `@length` MAXIMUM
    /// `bound` the request went past.
    fn at_most(self, member: &str, bound: i64) -> String {
        match self {
            Self::ItemCount => {
                format!("{member} must contain less than or equal to {bound} items")
            }
            Self::ModelledConstraint => {
                format!("{member} length must satisfy max_items {bound}")
            }
        }
    }
}

/// The requirement clause this service states for `violation`, naming the parameter as
/// `member` spells it, or `None` for a constraint class no recorded refusal of this service
/// establishes a clause for — which keeps the framework's own wording for it rather than
/// inventing one.
fn requirement(
    member: &str,
    violation: &Violation,
    form: StatedPattern,
    length: StatedCollectionLength,
) -> Option<String> {
    match &violation.kind {
        ViolationKind::Pattern(pattern) => Some(form.pattern_requirement(member, pattern)),
        ViolationKind::MaxValue(bound) => Some(must_be_at_most(member, bound)),
        ViolationKind::MinValue(bound) => Some(must_be_at_least(member, bound)),
        ViolationKind::EnumValue(values) => Some(must_be_one_of(member, values)),
        // A `@length` minimum is stated for a LIST and left to the framework for a string:
        // which one broke it is read from the validating walk's own rendering of the size it
        // checked (`<n> items`), through this tree's one reading of that rendering, so a
        // string whose minimum was broken — and an absent member, which the walk renders no
        // size for — keeps the framework's wording.
        ViolationKind::MinLength(bound) => violation
            .value
            .as_deref()
            .and_then(list_item_count)
            .map(|_| length.at_least(member, *bound)),
        // A `@length` MAXIMUM is read the same way and for the same reason: the clause is
        // stated for a LIST, whose size the walk renders, and a string's keeps the
        // framework's wording.
        ViolationKind::MaxLength(bound) => violation
            .value
            .as_deref()
            .and_then(list_item_count)
            .map(|_| length.at_most(member, *bound)),
        _ => None,
    }
}

/// The refusal this service states for a request that left out properties its operation
/// requires: the properties it had to carry, each quoted inside one pair of brackets.
///
/// `None` unless EVERY violation is a member the shape requires and the request omitted: a
/// request that also broke a constraint of a value it DID carry is left to the clauses
/// below, which state that value's own requirement.
fn missing_properties(refused: &[(Violation, SubmittedEcho)]) -> Option<Refused> {
    let missing: Vec<String> = refused
        .iter()
        .map(|(violation, _)| match violation.kind {
            ViolationKind::MissingRequiredMember => Some(api_parameter_path(&violation.path)),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let quoted: Vec<String> = missing.iter().map(|member| format!("'{member}'")).collect();
    (!quoted.is_empty()).then(|| Refused {
        code: INVALID_REQUEST_EXCEPTION.to_owned(),
        message: format!(
            "Your request is invalid. Request must contain [{}] properties. Verify your \
             request and try again.",
            quoted.join(", ")
        ),
    })
}

/// The refusal this service states for `refused`, quoting an ARN expression in `form`, or
/// `None` to leave the request to the layers behind the front door.
///
/// `None` unless EVERY violation is one this service states a requirement for: a request
/// that also broke a constraint of a class this service has not been seen to word is
/// refused by the framework's own walk, which states the constraint the value broke, rather
/// than being restated as one parameter's.
///
/// ONE parameter is named, the first the walk found: the sentence states the parameter whose
/// value was refused, and a sentence naming several would be a different sentence. The one
/// refusal that does name several is the request that carried none of the properties it had
/// to ([`missing_properties`]), which is about no single value.
fn refusal(
    refused: &[(Violation, SubmittedEcho)],
    form: StatedPattern,
    length: StatedCollectionLength,
) -> Option<Refused> {
    if let Some(stated) = missing_properties(refused) {
        return Some(stated);
    }
    let stated: Vec<(String, String)> = refused
        .iter()
        .map(|(violation, _)| {
            let member = api_parameter_path(&violation.path);
            requirement(&member, violation, form, length).map(|requirement| (member, requirement))
        })
        .collect::<Option<Vec<_>>>()?;
    let (member, requirement) = stated.first()?;
    Some(Refused {
        code: INVALID_PARAMETER_VALUE_EXCEPTION.to_owned(),
        message: parameter_value_mismatch(member, requirement),
    })
}

/// How this service refuses a value its request carried in the BODY.
pub fn body_parameter_refusal(refused: &[(Violation, SubmittedEcho)]) -> Option<Refused> {
    refusal(
        refused,
        StatedPattern::RegionCode,
        StatedCollectionLength::ItemCount,
    )
}

/// How the PIPELINE LISTING refuses a value its request carried in the body: every clause
/// [`body_parameter_refusal`] states, with its own statement of a collection whose `@length`
/// bound was broken ([`StatedCollectionLength::ModelledConstraint`]).
pub fn pipeline_listing_body_refusal(refused: &[(Violation, SubmittedEcho)]) -> Option<Refused> {
    refusal(
        refused,
        StatedPattern::RegionCode,
        StatedCollectionLength::ModelledConstraint,
    )
}

/// How this service refuses a value its request carried in the REQUEST LINE — an
/// `@httpQuery` parameter, an `@httpLabel` path segment.
pub fn uri_parameter_refusal(refused: &[(Violation, SubmittedEcho)]) -> Option<Refused> {
    refusal(
        refused,
        StatedPattern::CharacterClass,
        StatedCollectionLength::ItemCount,
    )
}

/// How the one operation whose own check echoes no expression refuses a value its request
/// carried in the REQUEST LINE: the parameter, and the fact that it does not match the
/// pattern that parameter requires.
pub fn unstated_pattern_refusal(refused: &[(Violation, SubmittedEcho)]) -> Option<Refused> {
    refusal(
        refused,
        StatedPattern::Unstated,
        StatedCollectionLength::ItemCount,
    )
}

#[cfg(test)]
mod tests {
    //! The sentence over the constraint classes and the pattern forms it is stated for. The
    //! wire behaviour — that every operation's refusal carries it, with the parameter the
    //! request named — is asserted through the router in
    //! `tests/all/parameter_value_refusals.rs`.

    use super::*;

    /// One violation of `kind` at `path`, for the value a request sent.
    fn violation(path: &str, kind: ViolationKind) -> (Violation, SubmittedEcho) {
        (
            Violation {
                path: path.to_owned(),
                kind,
                value: Some("submitted".to_owned()),
                sensitive: false,
            },
            SubmittedEcho::Rendered("submitted".to_owned()),
        )
    }

    /// The modelled `ImagePipelineArn` pattern
    /// (`smithy_query(query="shape",shape="ImagePipelineArn")`), the one member this model
    /// binds both to a request line and to a body.
    const MODELLED_IMAGE_PIPELINE_ARN: &str =
        "^arn:aws[^:]*:imagebuilder:[^:]+:(?:[0-9]{12}|aws):image-pipeline/[a-z0-9-_]+$";

    /// RULE: the same refused value states the expression in the form the layer that read it
    /// writes, and every other field of the expression is the model's.
    #[test]
    fn an_arn_expression_is_quoted_in_the_form_of_the_layer_that_read_it() {
        // (case, the form, the expression the refusal quotes)
        let rows = [
            (
                "a body member",
                StatedPattern::RegionCode,
                "^arn:aws(?:-[a-z]+)*:imagebuilder:[a-z]{2,}(?:-[a-z]+)+-[0-9]+:\
                 (?:[0-9]{12}|aws):image-pipeline/[a-z0-9-_]+$",
            ),
            (
                "a request-line parameter",
                StatedPattern::CharacterClass,
                "^arn:aws(?:-[a-z0-9]+)*:imagebuilder:[a-z0-9-]*[a-z0-9]:\
                 (?:[0-9]{12}|aws):image-pipeline/[a-z0-9-_]+$",
            ),
        ];

        for (case, form, expression) in rows {
            let refused = [violation(
                "imagePipelineArn",
                ViolationKind::Pattern(MODELLED_IMAGE_PIPELINE_ARN.to_owned()),
            )];
            let stated = refusal(&refused, form, StatedCollectionLength::ItemCount)
                .expect("a stated refusal");
            let expected = format!(
                "The value supplied for parameter 'imagePipelineArn' is not valid. \
                 imagePipelineArn must match pattern {expression}"
            );
            assert_eq!(
                stated.code, INVALID_PARAMETER_VALUE_EXCEPTION,
                "case {case:?}: expected this service's own code, got {:?}",
                stated.code
            );
            assert_eq!(
                stated.message, expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                stated.message
            );
        }

        // The operation that echoes no expression states the requirement without one,
        // whatever the pattern.
        let refused = [violation(
            "distributionConfigurationArn",
            ViolationKind::Pattern(MODELLED_IMAGE_PIPELINE_ARN.to_owned()),
        )];
        let stated = refusal(
            &refused,
            StatedPattern::Unstated,
            StatedCollectionLength::ItemCount,
        )
        .expect("a stated refusal");
        assert_eq!(
            stated.message,
            "The value supplied for parameter 'distributionConfigurationArn' is not valid. \
             distributionConfigurationArn does not match the required pattern",
            "case \"an unstated expression\": got {:?}",
            stated.message
        );
    }

    /// RULE: a pattern that is not an ARN's is quoted as the model writes it — there is no
    /// partition or Region field in it to restate — in every form.
    #[test]
    fn a_pattern_that_is_not_an_arns_is_quoted_as_the_model_writes_it() {
        for form in [StatedPattern::RegionCode, StatedPattern::CharacterClass] {
            let refused = [violation(
                "semanticVersion",
                ViolationKind::Pattern(r"^[0-9]+\.[0-9]+\.[0-9]+$".to_owned()),
            )];
            let stated = refusal(&refused, form, StatedCollectionLength::ItemCount)
                .expect("a stated refusal");
            assert_eq!(
                stated.message,
                "The value supplied for parameter 'semanticVersion' is not valid. \
                 semanticVersion must match pattern ^[0-9]+\\.[0-9]+\\.[0-9]+$",
                "case {form:?}: got {:?}",
                stated.message
            );
        }
    }

    /// RULE: every constraint class this service states a requirement for is stated in the
    /// one sentence, naming the parameter as the API addresses it; a class it states none
    /// for leaves the whole refusal to the framework.
    #[test]
    fn each_constraint_class_states_its_own_requirement() {
        // (case, the violation, the sentence — or `None` to leave it to the framework)
        let rows: Vec<(&str, (Violation, SubmittedEcho), Option<&str>)> = vec![
            (
                "an upper bound inside a structure member",
                violation(
                    "imageTestsConfiguration.timeoutMinutes",
                    ViolationKind::MaxValue("1440".to_owned()),
                ),
                Some(
                    "The value supplied for parameter 'imageTestsConfiguration.timeoutMinutes' \
                     is not valid. imageTestsConfiguration.timeoutMinutes must be smaller than \
                     or equal to 1440",
                ),
            ),
            (
                "a lower bound",
                violation("maxResults", ViolationKind::MinValue("1".to_owned())),
                Some(
                    "The value supplied for parameter 'maxResults' is not valid. maxResults \
                     must be bigger than or equal to 1",
                ),
            ),
            (
                "an enumerated value set",
                violation(
                    "owner",
                    ViolationKind::EnumValue(vec![
                        "Self".to_owned(),
                        "Shared".to_owned(),
                        "Amazon".to_owned(),
                        "ThirdParty".to_owned(),
                        "AWSMarketplace".to_owned(),
                    ]),
                ),
                Some(
                    "The value supplied for parameter 'owner' is not valid. owner must be one \
                     of ['Self', 'Shared', 'Amazon', 'ThirdParty', 'AWSMarketplace']",
                ),
            ),
            (
                "a pattern broken inside a list element",
                violation(
                    "distributions.1.member.amiDistributionConfiguration.name",
                    ViolationKind::Pattern("^[-_A-Za-z0-9]+$".to_owned()),
                ),
                Some(
                    "The value supplied for parameter \
                     'distributions[0].amiDistributionConfiguration.name' is not valid. \
                     distributions[0].amiDistributionConfiguration.name must match pattern \
                     ^[-_A-Za-z0-9]+$",
                ),
            ),
            (
                "a length bound on a string, which this service has not been seen to state",
                violation("description", ViolationKind::MaxLength(1024)),
                None,
            ),
            (
                "an empty list under its minimum",
                (
                    Violation {
                        path: "components".to_owned(),
                        kind: ViolationKind::MinLength(1),
                        value: Some("0 items".to_owned()),
                        sensitive: false,
                    },
                    SubmittedEcho::Rendered("[]".to_owned()),
                ),
                Some(
                    "The value supplied for parameter 'components' is not valid. components \
                     must contain at least 1 items",
                ),
            ),
            (
                "a list under a minimum of several, inside a structure member",
                (
                    Violation {
                        path: "supportedOsVersions".to_owned(),
                        kind: ViolationKind::MinLength(2),
                        value: Some("1 items".to_owned()),
                        sensitive: false,
                    },
                    SubmittedEcho::Rendered("[one]".to_owned()),
                ),
                Some(
                    "The value supplied for parameter 'supportedOsVersions' is not valid. \
                     supportedOsVersions must contain at least 2 items",
                ),
            ),
            (
                "a string under its minimum, which keeps the framework's wording",
                (
                    Violation {
                        path: "nextToken".to_owned(),
                        kind: ViolationKind::MinLength(1),
                        value: Some(String::new()),
                        sensitive: false,
                    },
                    SubmittedEcho::Rendered(String::new()),
                ),
                None,
            ),
            (
                "an absent member carrying a length minimum, which renders no size",
                (
                    Violation {
                        path: "components".to_owned(),
                        kind: ViolationKind::MinLength(1),
                        value: None,
                        sensitive: false,
                    },
                    SubmittedEcho::Unrendered,
                ),
                None,
            ),
            (
                "a member the shape requires and the request omits",
                violation("clientToken", ViolationKind::MissingRequiredMember),
                Some(
                    "Your request is invalid. Request must contain ['clientToken'] properties. \
                     Verify your request and try again.",
                ),
            ),
        ];

        for (case, refused, expected) in &rows {
            let stated = refusal(
                std::slice::from_ref(refused),
                StatedPattern::RegionCode,
                StatedCollectionLength::ItemCount,
            );
            match expected {
                Some(message) => {
                    let stated =
                        stated.unwrap_or_else(|| panic!("case {case:?}: expected a refusal"));
                    assert_eq!(
                        stated.message, *message,
                        "case {case:?}: expected {message:?}, got {:?}",
                        stated.message
                    );
                }
                None => assert!(
                    stated.is_none(),
                    "case {case:?}: expected the framework's own wording, got {stated:?}"
                ),
            }
        }

        // A request that breaks one constraint this service states and one it does not is
        // left to the framework as a whole: the sentence names ONE parameter, and the other
        // violation would go unstated.
        let mixed = [
            violation(
                "semanticVersion",
                ViolationKind::Pattern("^[0-9]+$".to_owned()),
            ),
            violation("description", ViolationKind::MaxLength(1024)),
        ];
        assert!(
            refusal(
                &mixed,
                StatedPattern::RegionCode,
                StatedCollectionLength::ItemCount
            )
            .is_none(),
            "case \"a stated class beside an unstated one\": expected the framework's own \
             wording"
        );
    }

    /// RULE: a COLLECTION whose `@length` bound was broken states the count that bound sets,
    /// in the wording the operation that was addressed states; a STRING whose own was broken
    /// keeps the framework's.
    #[test]
    fn a_collection_whose_length_bound_was_broken_states_the_count_that_bound_sets() {
        /// One list `@length` violation of `member`, carrying `found` items against `kind`, as
        /// the validating walk renders one.
        fn of_a_list(
            member: &str,
            found: usize,
            kind: ViolationKind,
        ) -> (Violation, SubmittedEcho) {
            (
                Violation {
                    path: member.to_owned(),
                    kind,
                    value: Some(format!("{found} items")),
                    sensitive: false,
                },
                SubmittedEcho::Rendered("[…]".to_owned()),
            )
        }

        /// One row of the table: the case, the violation, the wording the addressed operation
        /// states, and the sentence — or `None` to leave the refusal to the framework.
        type Row = (
            &'static str,
            (Violation, SubmittedEcho),
            StatedCollectionLength,
            Option<&'static str>,
        );

        let rows: Vec<Row> = vec![
            (
                "a filter list one over its maximum",
                of_a_list("filters", 11, ViolationKind::MaxLength(10)),
                StatedCollectionLength::ItemCount,
                Some(
                    "The value supplied for parameter 'filters' is not valid. filters must \
                     contain less than or equal to 10 items",
                ),
            ),
            (
                "the same list under the wording the pipeline listing states",
                of_a_list("filters", 11, ViolationKind::MaxLength(10)),
                StatedCollectionLength::ModelledConstraint,
                Some(
                    "The value supplied for parameter 'filters' is not valid. filters length \
                     must satisfy max_items 10",
                ),
            ),
            (
                "an empty filter list under its minimum",
                of_a_list("filters", 0, ViolationKind::MinLength(1)),
                StatedCollectionLength::ItemCount,
                Some(
                    "The value supplied for parameter 'filters' is not valid. filters must \
                     contain at least 1 items",
                ),
            ),
            (
                "the same empty list under the wording the pipeline listing states",
                of_a_list("filters", 0, ViolationKind::MinLength(1)),
                StatedCollectionLength::ModelledConstraint,
                Some(
                    "The value supplied for parameter 'filters' is not valid. filters length \
                     must satisfy min_items 1",
                ),
            ),
            (
                "a list far over a maximum of several, inside a list element",
                of_a_list("filters.1.member.values", 64, ViolationKind::MaxLength(10)),
                StatedCollectionLength::ItemCount,
                Some(
                    "The value supplied for parameter 'filters[0].values' is not valid. \
                     filters[0].values must contain less than or equal to 10 items",
                ),
            ),
            (
                "a list under a minimum of several, under the pipeline listing's wording",
                of_a_list("supportedOsVersions", 1, ViolationKind::MinLength(2)),
                StatedCollectionLength::ModelledConstraint,
                Some(
                    "The value supplied for parameter 'supportedOsVersions' is not valid. \
                     supportedOsVersions length must satisfy min_items 2",
                ),
            ),
            (
                "a string over its own maximum, which renders no item count",
                violation("description", ViolationKind::MaxLength(1024)),
                StatedCollectionLength::ItemCount,
                None,
            ),
        ];

        for (case, refused, length, expected) in &rows {
            let stated = refusal(
                std::slice::from_ref(refused),
                StatedPattern::RegionCode,
                *length,
            );
            match expected {
                Some(message) => {
                    let stated =
                        stated.unwrap_or_else(|| panic!("case {case:?}: expected a refusal"));
                    assert_eq!(
                        stated.code, INVALID_PARAMETER_VALUE_EXCEPTION,
                        "case {case:?}: expected this service's own code, got {:?}",
                        stated.code
                    );
                    assert_eq!(
                        stated.message, *message,
                        "case {case:?}: expected {message:?}, got {:?}",
                        stated.message
                    );
                }
                None => assert!(
                    stated.is_none(),
                    "case {case:?}: expected the framework's own wording, got {stated:?}"
                ),
            }
        }
    }
}
