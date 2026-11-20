// SPDX-License-Identifier: CC0-1.0

//! What a request's `RevisionLocation` has to carry before anything is done with
//! it: the one place a revision a request names is turned into the revision's
//! identity, or into the refusal it earns.
//!
//! The model holds ONE location per `revisionType`
//! (`smithy_query(query="shape",shape="RevisionLocation")`: s3Location,
//! gitHubLocation, string, appSpecContent), and the two INLINE forms carry the
//! document itself rather than naming it elsewhere: `appSpecContent` has
//! `content` beside "the SHA256 hash value of the revision content"
//! (`smithy_query(query="shape",shape="AppSpecContent")`), and the deprecated
//! `RawString` the `string` form uses has the same pair
//! (`smithy_query(query="shape",shape="RawString")`). An inline form that states
//! NEITHER of them hands the service no document at all, so the request carrying
//! it is refused where it is read — which is what divides a revision that is not
//! a revision from one whose content is merely WRONG. The latter IS a revision:
//! the request is accepted, and it is the DEPLOYMENT that fails on it once it
//! reads what the content declares (`super::appspec`).
//!
//! Every operation taking a `revision` — the one that deploys it, the one that
//! registers it, and the two that read one back — asks here, so the same request
//! earns the same refusal whichever of them it was sent to.

use crate::ops::types::RevisionLocation;

/// Why a `RevisionLocation` names no revision. Both are
/// `InvalidRevisionException` (HTTP 400), which every operation taking a
/// revision models; [`message`] carries the sentence each one answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The revision names no location at all: no member of it says where the
    /// revision is, so a `revisionType` only says which absent member to read.
    NoLocation,
    /// The revision is an inline one stating neither its content nor its digest.
    EmptyInlineContent,
}

/// The sentence `refusal` is answered with. Fixed text in both cases: neither
/// quotes the member or the value the caller sent, so a client matches the
/// sentence rather than parsing it.
#[must_use]
pub fn message(refusal: Refusal) -> &'static str {
    match refusal {
        Refusal::NoLocation => {
            "The revision was specified in an invalid format: it names no S3, GitHub, string or \
             AppSpec content location"
        }
        Refusal::EmptyInlineContent => "Content and Sha256 value of the string can't both be empty",
    }
}

/// The identity of the revision `revision` names ([`super::revision_key`]), or
/// why it names none.
///
/// The form that IDENTIFIES the revision is the one asked about: a revision
/// naming an artifact elsewhere carries no inline document to be empty, and an
/// inline member beside such a location is not what that revision is read from.
pub fn key(revision: &RevisionLocation) -> Result<String, Refusal> {
    if revision.s3_location.is_none() && revision.git_hub_location.is_none() {
        // The inline forms in the precedence `super::revision_key` reads them in,
        // so the member asked about is the member the identity is cut from.
        let inline = revision
            .string
            .as_ref()
            .map(|raw| (raw.content.as_deref(), raw.sha256.as_deref()))
            .or_else(|| {
                revision
                    .app_spec_content
                    .as_ref()
                    .map(|spec| (spec.content.as_deref(), spec.sha256.as_deref()))
            });
        if let Some((content, sha256)) = inline {
            if !states(content) && !states(sha256) {
                return Err(Refusal::EmptyInlineContent);
            }
        }
    }
    super::revision_key(revision).ok_or(Refusal::NoLocation)
}

/// Whether an inline revision's member STATES something: a member the request
/// left out and one it sent empty both state nothing, while any content at all
/// — content that is not a usable AppSpec included — is something the
/// deployment can go and read.
fn states(member: Option<&str>) -> bool {
    member.is_some_and(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    //! Which revisions [`key`] identifies and which it refuses, over the forms
    //! the model holds. The same rule is asserted end to end, through the
    //! operations a client calls, in `tests/all/application_revisions.rs`.

    use super::*;
    use crate::ops::types::{AppSpecContent, GitHubLocation, RawString, S3Location};

    /// An `AppSpecContent` revision carrying `content` and `sha256` exactly as a
    /// request spelled them.
    fn app_spec(content: Option<&str>, sha256: Option<&str>) -> RevisionLocation {
        RevisionLocation {
            revision_type: Some("AppSpecContent".to_string()),
            app_spec_content: Some(AppSpecContent {
                content: content.map(str::to_string),
                sha256: sha256.map(str::to_string),
            }),
            ..Default::default()
        }
    }

    /// The deprecated `string` form of the same pair.
    fn raw_string(content: Option<&str>, sha256: Option<&str>) -> RevisionLocation {
        RevisionLocation {
            revision_type: Some("String".to_string()),
            string: Some(RawString {
                content: content.map(str::to_string),
                sha256: sha256.map(str::to_string),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn an_inline_revision_stating_nothing_is_refused_and_any_content_is_not() {
        // Rows differ along WHICH inline form carries the pair and what the pair
        // states: nothing at all, an empty member, each member alone, and
        // content that is present but not a usable AppSpec.
        let cases: &[(&str, RevisionLocation, Option<Refusal>)] = &[
            (
                "an AppSpecContent stating neither member",
                app_spec(None, None),
                Some(Refusal::EmptyInlineContent),
            ),
            (
                "an AppSpecContent whose content is empty",
                app_spec(Some(""), None),
                Some(Refusal::EmptyInlineContent),
            ),
            (
                "an AppSpecContent whose content and digest are both empty",
                app_spec(Some(""), Some("")),
                Some(Refusal::EmptyInlineContent),
            ),
            (
                "an AppSpecContent carrying content",
                app_spec(Some("version: 0.0\n"), None),
                None,
            ),
            (
                "an AppSpecContent carrying content that is not an AppSpec",
                app_spec(Some("not-an-appspec"), None),
                None,
            ),
            (
                "an AppSpecContent carrying only a digest",
                app_spec(
                    None,
                    Some("4355a46b19d348dc2f57c046f8ef63d4538ebb936000f3c9ee954a27460dd865"),
                ),
                None,
            ),
            (
                "a string revision stating neither member",
                raw_string(None, None),
                Some(Refusal::EmptyInlineContent),
            ),
            (
                "a string revision whose content is empty",
                raw_string(Some(""), None),
                Some(Refusal::EmptyInlineContent),
            ),
            (
                "a string revision carrying content",
                raw_string(Some("version: 0.0\n"), None),
                None,
            ),
            (
                "no location at all",
                RevisionLocation::default(),
                Some(Refusal::NoLocation),
            ),
            (
                "a revisionType and nothing else",
                RevisionLocation {
                    revision_type: Some("S3".to_string()),
                    ..Default::default()
                },
                Some(Refusal::NoLocation),
            ),
            (
                "an S3 location, which carries no inline document",
                RevisionLocation {
                    revision_type: Some("S3".to_string()),
                    s3_location: Some(S3Location {
                        bucket: Some("artifacts".to_string()),
                        key: Some("app.zip".to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                None,
            ),
            (
                "a GitHub location beside an empty inline member",
                RevisionLocation {
                    revision_type: Some("GitHub".to_string()),
                    git_hub_location: Some(GitHubLocation {
                        repository: Some("acct/repo".to_string()),
                        commit_id: Some("0123456789abcdef".to_string()),
                    }),
                    app_spec_content: Some(AppSpecContent::default()),
                    ..Default::default()
                },
                None,
            ),
        ];

        for (case, revision, expected) in cases {
            let answered = key(revision).err();
            assert_eq!(
                &answered, expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }

    #[test]
    fn the_refusals_carry_their_own_sentence() {
        let empty = message(Refusal::EmptyInlineContent);
        assert_eq!(
            empty, "Content and Sha256 value of the string can't both be empty",
            "case the empty inline revision: expected the sentence for an inline revision that \
             states nothing, got {empty:?}"
        );
        let no_location = message(Refusal::NoLocation);
        assert_ne!(
            no_location, empty,
            "case the two refusals: each states its own cause, got {no_location:?}"
        );
    }
}
