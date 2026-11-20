// SPDX-License-Identifier: CC0-1.0

//! The ONE criterion an allow list specifies, and how a request specifying the
//! wrong number of them is refused.
//!
//! `AllowListCriteria` carries both of its members optionally, so a REQUEST can
//! spell criteria that specify both or neither, while a STORED list carries
//! exactly one — the rule the shape documents ("The criteria must specify a
//! regular expression (regex) or an S3 object (s3WordsList). It can't specify
//! both", `smithy_query(query="shape",shape="AllowListCriteria")`). That
//! translation is this file's whole concern; what Macie can then DO with the
//! criterion — compile the expression, or read the object out of this
//! emulator's own object store — is [`super::allow_lists`]'s.

use crate::ops::types::{AllowListCriteria, S3WordsList};

/// How the service refuses a `criteria` that does not specify exactly one
/// criterion. The whole sentence is the documented rule for the shape ("The
/// criteria must specify a regular expression (regex) or an S3 object
/// (s3WordsList). It can't specify both"), so it interpolates nothing from the
/// request.
pub const CRITERIA_MUST_SPECIFY_ONE_MESSAGE: &str =
    "The criteria must specify a regular expression (regex) or an S3 object (s3WordsList). \
     It can't specify both.";

/// How the service refuses an edit whose `criteria` specify a DIFFERENT kind of
/// criterion from the one the stored list specifies. The sentence states the
/// documented rule for `UpdateAllowListRequest$criteria` ("You can change a
/// list's underlying criteria … However, you can't change the type from
/// s3WordsList to regex or the other way around"), so it interpolates nothing
/// from the request.
pub const CRITERIA_TYPE_IS_FIXED_MESSAGE: &str =
    "Input validation failed. You can't change the type of an allow list's criteria from \
     s3WordsList to regex or the other way around.";

/// The ONE criterion an allow list specifies.
///
/// [`AllowListCriterion::from_modeled`] is where a request becomes one of
/// these, or is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowListCriterion {
    /// A regular expression that defines the text pattern to ignore.
    Regex(String),
    /// The location and name of the S3 object that lists specific text to
    /// ignore.
    WordsList {
        /// The full name of the S3 bucket that contains the object.
        bucket_name: String,
        /// The full name (key) of the object.
        object_key: String,
    },
}

impl AllowListCriterion {
    /// The one criterion `criteria` specifies, or the message the service
    /// refuses it with when it specifies both or neither.
    pub fn from_modeled(criteria: &AllowListCriteria) -> Result<Self, String> {
        match (criteria.regex.as_deref(), criteria.s3_words_list.as_ref()) {
            (Some(regex), None) => Ok(Self::Regex(regex.to_string())),
            (None, Some(words)) => Ok(Self::WordsList {
                bucket_name: words.bucket_name.clone(),
                object_key: words.object_key.clone(),
            }),
            _ => Err(CRITERIA_MUST_SPECIFY_ONE_MESSAGE.to_string()),
        }
    }

    /// Whether `other` specifies the same KIND of criterion as this one — a
    /// regular expression, or an S3 object. An edit may change what a criterion
    /// says but not which of the two it is.
    pub fn is_same_kind_as(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Regex(_), Self::Regex(_))
                | (Self::WordsList { .. }, Self::WordsList { .. })
        )
    }

    /// The same criterion as the modelled shape a read reports, which carries
    /// the member this criterion is and leaves the other absent.
    pub fn to_modeled(&self) -> AllowListCriteria {
        match self {
            Self::Regex(regex) => AllowListCriteria {
                regex: Some(regex.clone()),
                s3_words_list: None,
            },
            Self::WordsList {
                bucket_name,
                object_key,
            } => AllowListCriteria {
                regex: None,
                s3_words_list: Some(S3WordsList {
                    bucket_name: bucket_name.clone(),
                    object_key: object_key.clone(),
                }),
            },
        }
    }
}
