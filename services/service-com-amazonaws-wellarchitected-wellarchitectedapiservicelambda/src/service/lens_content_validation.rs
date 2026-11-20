// SPDX-License-Identifier: CC0-1.0

//! How this service words a request that NAMES lens content it cannot resolve.
//!
//! A lens a request asks to APPLY, the lens whose review of a workload or of a review
//! template a request addresses, and the question or the choice of such a review are
//! VALUES the request carries, not resources this API identifies. The resources it
//! identifies are the workload, the review template, the lens itself, the profile and the
//! milestone — each named by a member that resolves to a stored record on its own. So a
//! value of the first kind that resolves to nothing is INPUT the service refuses:
//! `ValidationException` (HTTP 400), carrying `Reason` and the `Fields` entry that names
//! the member the value came in, which is what tells a caller to fix the REQUEST.
//! `ResourceNotFoundException` (HTTP 404) stays what it is — the answer for a resource of
//! the second kind that the store does not hold — so a client can still tell "fix the
//! request" from "fix the state", and an SDK's modelled `ValidationException` catch fires
//! for the first.
//!
//! It is the same rule the Trusted Advisor check walk already applies to the pillar, the
//! question and the choice a check is read by
//! ([`crate::state::ComAmazonawsWellarchitectedWellarchitectedapiservicelambdaState::best_practice_checks`]),
//! stated here for the lens and the lens review as well and shared by every operation
//! that resolves one, so those operations cannot word the same failure differently. Each
//! operation still wraps what it gets back in its OWN modelled error enum.
//!
//! The envelope — this service's `[Validation]` message tag, `Reason` and the `Fields`
//! list — is built by the one place this service builds a field validation
//! ([`crate::state::field_validation_exception`]), which states each finding in `Message`
//! behind that tag as well as in its `Fields` entry, and by [`fields_failed`] for the one
//! class recorded the other way round. What lives here is the SENTENCES these failures are
//! worded with and the rule that decides them.
//!
//! How a finding is worded follows how the value reached the service. A member that
//! IDENTIFIES the lens content addressed — the lens whose review it is, the question an
//! answer is of — reports the lookup that found nothing. A value bound OUTSIDE the body
//! that the parameter it was read as does not admit — a `PillarId` a read is scoped to, as
//! much as a query parameter whose enumerated type does not admit its value — reports that
//! parameter as invalid and nothing about the value, because the parameter is what the
//! caller has to fix. Both put the finding in `Message` too, so a client that attributes a
//! failure from `Message` reads the same diagnostic as one that reads `Fields`.
//!
//! A BODY member carrying a LIST of values matched against that content — the choices of
//! the question being answered — is the one class worded the other way round
//! ([`fields_failed`]): `Message` states only the class of the failure, and the `Fields`
//! entry carries the rule the value broke together with the values that content DOES
//! define, which are what the caller needs to repair the request and are not discoverable
//! from the refusal otherwise.

use std::collections::HashMap;

use crate::ops::types::{ChoiceUpdate, ValidationException, ValidationExceptionField};
use crate::state;

/// The member a finding about the lens of a review names: `LensAlias`, the member every
/// operation that addresses one lens review carries it in (`GetLensReviewInput$LensAlias`,
/// `GetAnswerInput$LensAlias`, `GetReviewTemplateLensReviewInput$LensAlias`, …).
pub const LENS_ALIAS_MEMBER: &str = "LensAlias";

/// The `Message` of a refusal whose findings are ALL in `Fields`: the message states the
/// class of the failure and each finding states itself, so the sentence carries no
/// variable part.
const FIELDS_FAILED_SENTENCE: &str = "The validation of one or more fields has failed.";

/// `sentence` behind this service's failure-class tag, from the one place this service
/// tags a validation message ([`state::unclassified_exception`]), so the tag is not
/// restated here.
fn tagged(sentence: &str) -> String {
    state::unclassified_exception(sentence.to_string()).message
}

/// The refusal for a list of lenses a request asks to APPLY that the store will not
/// apply ([`state::LensesRefusal`]).
///
/// A lens is applied BY NAME, and the name is a value of the request: a name the caller
/// cannot reach a lens through is refused as that member's value rather than reported as a
/// missing resource, and the two ways it can fail — a name that resolves to no lens at all
/// and one that names a custom lens no version has been published from — are worded
/// IDENTICALLY, because the service does not tell the caller which of them it is.
pub fn unapplicable_lenses(refusal: state::LensesRefusal) -> ValidationException {
    match refusal {
        // The store reports the entry it could not resolve as that lookup's `ResourceId`,
        // which is the alias AS SENT — how the caller named the lens. This refusal is not a
        // per-member finding: the service carries `Reason` without classifying it
        // ([`state::unstated_reason_exception`]), so a client branching on the member reads
        // "no classification" rather than a field validation it would then look in `Fields`
        // for.
        state::LensesRefusal::NotFound(unresolved) => {
            state::unstated_reason_exception(format!(
                "Failed to get lenses {}. User might be not authorized to access lens.",
                unresolved.resource_id
            ))
        }
    }
}

/// The refusal for a `LensAlias` that names no lens review of the workload the request
/// addresses.
///
/// A lens review of a workload exists only while that lens is associated with it
/// ([`state::WorkloadRecord::reviews_lens`]), so the alias is judged against the lenses
/// the workload carries; both the alias and the workload id are named as the request sent
/// them.
pub fn no_lens_review(workload_id: &str, lens: &str) -> ValidationException {
    state::field_validation_exception(&[(
        LENS_ALIAS_MEMBER.to_string(),
        format!("No lens review for lens alias {lens} was found in workload {workload_id}."),
    )])
}

/// The refusal for a `LensAlias` that names no lens review of the review template the
/// request addresses.
///
/// The same rule as [`no_lens_review`] on the resource that holds the other kind of lens
/// review ([`state::ReviewTemplateRecord::applies_lens`]), so the finding names the same
/// member and reads the same way, with the template as the request named it.
pub fn no_review_template_lens_review(template_arn: &str, lens: &str) -> ValidationException {
    state::field_validation_exception(&[(
        LENS_ALIAS_MEMBER.to_string(),
        format!(
            "No lens review for lens alias {lens} was found in review template {template_arn}."
        ),
    )])
}

/// The refusal for a `QuestionId` the lens under review does not ask.
///
/// The questions of a review are the LENS's own content, as of the version the review is
/// pinned at, so the finding names the lens the question was looked for in rather than
/// the workload or template whose review it is: the same `QuestionId` is unknown in that
/// lens wherever it is reviewed.
pub fn no_question(lens: &str, question_id: &str) -> ValidationException {
    state::field_validation_exception(&[(
        state::QUESTION_ID_MEMBER.to_string(),
        format!("No question with ID {question_id} was found in the lens with alias {lens}."),
    )])
}

/// The refusal for a `PillarId` a request supplies to SCOPE a read of a lens review that
/// the lens under review does not define — or `None` when it names a pillar of that lens,
/// carries none, or names a lens this emulator holds no content for, which has no pillar
/// set to judge a value against.
///
/// The pillars of a lens are that lens's own content (`content`, the lens as of the version
/// the review is pinned at), so a value naming none of them belongs to no pillar of this
/// review: it SELECTS nothing because there is nothing of that name to select, which is a
/// bad value rather than an empty answer set.
///
/// Every operation that takes one takes it OUTSIDE the body, in the `PillarId` query
/// parameter (`ListAnswersInput$PillarId`, `ListLensReviewImprovementsInput$PillarId`,
/// `ListReviewTemplateAnswersInput$PillarId`, each bound `@httpQuery("PillarId")` and
/// targeting `PillarId`), so a value it cannot accept is refused the way this service
/// refuses any value bound outside its body that the parameter it was read as does not
/// admit (the enumerated-type finding this service's front door words): that parameter
/// reported as invalid, in `Message` as well as in its `Fields` entry, and nothing about
/// the pillars this emulator holds — those are the lens's content, which a caller discovers
/// from the lens's own review (`GetLensReview`'s `PillarReviewSummaries`) rather than from
/// a refusal.
pub fn undefined_pillar(
    content: &state::LensContentView,
    pillar_id: Option<&str>,
) -> Option<ValidationException> {
    let pillar_id = pillar_id?;
    // The pillar set the value is judged against is the lens's own
    // ([`state::LensContentView::pillar_ids`]) — the same set every read of that lens's
    // review reports a summary for — so membership is tested in one place.
    let defined = content.pillar_ids();
    if defined.is_empty() || defined.contains(&pillar_id) {
        return None;
    }
    Some(undefined_pillar_finding())
}

/// The finding [`undefined_pillar`] reports: the parameter the value arrived in, named as
/// the model names it, and this service's sentence for a parameter value it would not
/// accept ([`state::invalid_parameter_sentence`]) — the one place that sentence is worded,
/// so a `PillarId` the lens does not define reads exactly like a query parameter whose
/// enumerated type does not admit its value, which is the only wording this service's
/// recordings show for a refused value bound outside the body.
///
/// The envelope is the one place this service builds a field validation
/// ([`state::field_validation_exception`]), which states the finding in `Message` behind
/// this service's tag as well as in `Fields`, so a client attributing the failure from
/// `Message` reads the same diagnostic as one reading `Fields`.
fn undefined_pillar_finding() -> ValidationException {
    state::field_validation_exception(&[(
        state::PILLAR_ID_MEMBER.to_string(),
        state::invalid_parameter_sentence(state::PILLAR_ID_MEMBER),
    )])
}

/// The refusal for the `ChoiceId`s an answer-recording request names that the question it
/// answers does not offer, if either member that carries one does — or `None` when every
/// id named is a choice of that question.
///
/// A choice belongs to the question that offers it, and the question's own choice catalog
/// (`Answer$Choices` / `ReviewTemplateAnswer$Choices`) is the only place a client
/// discovers those ids, so the finding hands that catalog back: the valid ids, in the
/// lens's own order. The members are judged in a fixed order, so the member a refusal
/// names does not depend on the request map's iteration order. Shared by both operations
/// that record an answer, so they cannot word it differently.
pub fn undefined_choices(
    question: &state::ReviewQuestion,
    selected_choices: Option<&[String]>,
    choice_updates: Option<&HashMap<String, ChoiceUpdate>>,
) -> Option<ValidationException> {
    // Each member's ids are BORROWED from the request and only recognised, never
    // collected, so a request whose choices are all valid allocates nothing.
    let named: [(&str, Box<dyn Iterator<Item = &str>>); 2] = [
        (
            state::SELECTED_CHOICES_MEMBER,
            Box::new(
                selected_choices
                    .unwrap_or_default()
                    .iter()
                    .map(String::as_str),
            ),
        ),
        (
            state::CHOICE_UPDATES_MEMBER,
            Box::new(
                choice_updates
                    .into_iter()
                    .flat_map(|updates| updates.keys().map(String::as_str)),
            ),
        ),
    ];
    for (member, mut choice_ids) in named {
        if choice_ids.any(|choice_id| !question.defines_choice(choice_id)) {
            return Some(invalid_choices(member, question));
        }
    }
    None
}

/// One member's finding for [`undefined_choices`]: the member that carried the bad id,
/// and the question's whole choice catalog as the valid values.
fn invalid_choices(member: &str, question: &state::ReviewQuestion) -> ValidationException {
    let valid: Vec<&str> = question.each_choice_id().collect();
    fields_failed(
        member,
        format!(
            "{member} contains invalid choices. Valid choices are: [{}].",
            valid.join(", ")
        ),
    )
}

/// The envelope a BODY member carrying a LIST of values matched against lens content and
/// found outside it is refused with: the CLASS of the failure in `Message` — which
/// therefore carries no value from the request or from the store — the modelled
/// `FIELD_VALIDATION_FAILED` classification, and ONE `Fields` entry naming the input member
/// the values arrived in and carrying that member's own detail, which is where the values
/// the content defines are handed back. That split is what this service's recorded refusal
/// of this class does, and it is why a value bound outside the body is worded the other way
/// round ([`undefined_pillar`]).
///
/// Every finding of this class is built here, so the member a client attributes the failure
/// to, and the place the actionable detail lives, cannot drift between the members and the
/// operations that carry one.
fn fields_failed(member: &str, detail: String) -> ValidationException {
    ValidationException {
        message: tagged(FIELDS_FAILED_SENTENCE),
        reason: Some(state::REASON_FIELD_VALIDATION_FAILED.to_string()),
        fields: Some(vec![ValidationExceptionField {
            name: member.to_string(),
            message: detail,
        }]),
    }
}
