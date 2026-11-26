// SPDX-License-Identifier: CC0-1.0

//! Searching the USERS of a collection for the ones a face matches: the bounds and the
//! defaults the user searches work to, how a submitted image's faces are split into the one
//! the search uses and the ones it did not, and the match a user is reported with.
//!
//! A user of a collection is an identity this service HOLDS — it is exactly the faces
//! `AssociateFaces` put on it — so a user search needs no trained model of WHO a face
//! belongs to: how well a face matches a user is the resemblance MEASURED between it and
//! the faces that user holds, the same measurement `CompareFaces` and `SearchFacesByImage`
//! report ([`super::face_detection::FaceAppearance`]), read through the one function
//! `AssociateFaces` already decides a candidate by
//! ([`super::face_association::user_match_confidence`]). A user that holds no face has
//! nothing to measure against and so matches nothing.
//!
//! The ordering, the threshold and the cap are one rule for every operation that searches a
//! collection's users by a face, so they live here rather than in the handler of each.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::ops::types::{MatchedUser, UnsearchedFace, UserMatch};

use super::face_association::user_match_confidence;
use super::face_detection::{FaceAppearance, MeasuredFace};
use super::Collection;

/// The user match confidence a user must reach to be reported when the request names no
/// `UserMatchThreshold`: "specifies the minimum confidence in the UserID match to return.
/// Default value is 80"
/// (`smithy_query(query="doc",shape="SearchUsersByImageRequest",member="UserMatchThreshold")`,
/// worded the same on `SearchUsersRequest`).
pub const DEFAULT_USER_MATCH_THRESHOLD: f32 = 80.0;

/// How many matched users are reported when the request names no `MaxUsers`: the largest a
/// `MaxUserResults` may be (`range 1..500`,
/// `smithy_query(query="operation",operation="SearchUsersByImage")`), which is also the top
/// of the `UserMatchList` length range the answer carries. The model documents no default
/// for the member, so an omitted cap reports every match the answer can carry.
pub const DEFAULT_MAX_USERS: usize = 500;

/// The `UnsearchedFaceReason` values a search by image of this service reports
/// (`smithy_query(query="shape",shape="UnsearchedFaceReason")`: FACE_NOT_LARGEST |
/// EXCEEDS_MAX_FACES | EXTREME_POSE | LOW_BRIGHTNESS | LOW_SHARPNESS | LOW_CONFIDENCE |
/// SMALL_BOUNDING_BOX | LOW_FACE_QUALITY).
///
/// Two of them are the reasons a face of a submitted image is left out of a search here: a
/// face the `QualityFilter`'s bar removed, which is a `Quality.Sharpness` floor
/// ([`super::face_detection::quality_bar`]), and a face the search did not use because the
/// operation "first detects the largest face in the image". Every other value names a
/// judgement nothing in this process makes, and a reason nothing established is not one
/// this service invents.
pub const REASON_LOW_SHARPNESS: &str = "LOW_SHARPNESS";
pub const REASON_FACE_NOT_LARGEST: &str = "FACE_NOT_LARGEST";

/// The faces of one submitted image, split into the one a search by image uses and the ones
/// it does not, each of the latter with the reason it was not used.
#[derive(Debug, Default)]
pub struct SearchableFaces {
    /// The face the search uses, and `None` for an image that left it none — the case the
    /// operation documents as answering "an empty UserMatches list and no SearchedFace
    /// object".
    pub searched: Option<MeasuredFace>,
    /// "Information about faces detected in the supplied image, but not used for the
    /// search" (`smithy_query(query="doc",shape="SearchUsersByImage")`), one entry per such
    /// face, in the order the detector reported them.
    pub unsearched: Vec<UnsearchedFace>,
}

/// Split `detected` — the faces of a submitted image, which the detector reports largest
/// box first — at the `QualityFilter` bar `bar`.
///
/// "Filtered faces aren't searched for in the collection"
/// (`smithy_query(query="doc",shape="SearchUsersByImageRequest",member="QualityFilter")`),
/// so the bar is applied BEFORE the largest face is picked: a filter can change WHICH face
/// the search uses, not only whether there is one. Each face the search did not use carries
/// the one reason it was not used.
pub fn searchable_faces(detected: Vec<MeasuredFace>, bar: f32) -> SearchableFaces {
    let mut split = SearchableFaces::default();
    for face in detected {
        if !face.meets_quality_bar(bar) {
            split.unsearched.push(unsearched(&face, REASON_LOW_SHARPNESS));
        } else if split.searched.is_none() {
            split.searched = Some(face);
        } else {
            split
                .unsearched
                .push(unsearched(&face, REASON_FACE_NOT_LARGEST));
        }
    }
    split
}

/// One face the search did not use, reported with what the detector measured for it and the
/// reason it was left out.
fn unsearched(face: &MeasuredFace, reason: &str) -> UnsearchedFace {
    UnsearchedFace {
        face_details: Some(face.detail.clone()),
        reasons: Some(vec![reason.to_string()]),
    }
}

/// The appearance of every face of `collection`, by the `FaceId` it is held under: one pass
/// over the collection's faces, so resolving the faces of every user below costs a lookup
/// each rather than a scan each.
fn appearances_by_face_id(collection: &Collection) -> BTreeMap<&str, &FaceAppearance> {
    collection
        .faces
        .iter()
        .map(|face| (face.face_id.as_str(), &face.appearance))
        .collect()
}

/// The users of `collection` that `candidate` matches: every user whose measured user match
/// confidence reaches `threshold`, "ordered by similarity score with the highest similarity
/// first" (`smithy_query(query="doc",shape="SearchUsersByImage")`) and capped at
/// `max_users`, each reported with the status it carries at `now`.
///
/// The sort is stable, so two users a candidate matches equally keep the collection's own
/// user order between them; the cap is applied to the ordered matches, so it keeps the
/// strongest. A user holding no face is matched by nothing: there is no confidence to
/// compare with the threshold.
pub fn matching_users(
    collection: &Collection,
    candidate: &FaceAppearance,
    threshold: f32,
    max_users: usize,
    now: SystemTime,
) -> Vec<UserMatch> {
    let appearances = appearances_by_face_id(collection);
    let mut matches: Vec<UserMatch> = collection
        .users
        .values()
        .filter_map(|user| {
            let held: Vec<&FaceAppearance> = user
                .face_ids
                .iter()
                .filter_map(|face_id| appearances.get(face_id.as_str()).copied())
                .collect();
            let confidence = user_match_confidence(candidate, held)?;
            (confidence >= threshold).then(|| UserMatch {
                similarity: Some(confidence),
                user: Some(MatchedUser {
                    user_id: Some(user.user_id.clone()),
                    user_status: Some(user.status_at(now).to_string()),
                }),
            })
        })
        .collect();
    matches.sort_by(|left, right| {
        right
            .similarity
            .unwrap_or_default()
            .total_cmp(&left.similarity.unwrap_or_default())
    });
    matches.truncate(max_users);
    matches
}
