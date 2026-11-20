// SPDX-License-Identifier: CC0-1.0

//! The allow-list plane of this service's state: the lists `CreateAllowList`
//! stores, the ONE criterion each one carries, and the status a read DERIVES
//! for a list by asking the object store about the S3 object its criteria name.
//!
//! A list's status is not a stored field. "Provides information about the
//! current status of an allow list, which indicates whether Amazon Macie can
//! access and use the list's criteria" — so it is a fact about the criteria AT
//! THE MOMENT OF THE READ, and a read derives it: the object an S3 criterion
//! names may be put, emptied or removed after the list was created, and the
//! status a client polls has to follow that. Deriving it also means nothing a
//! read reports is invented: the answer comes from this emulator's own Amazon
//! S3 ([`read_words_list`]), reached over the in-process callback.

use std::collections::BTreeMap;
use std::time::SystemTime;

use ::smithy_server_runtime::EmbeddedEmulator;

use crate::ops::types::AllowListStatus;

/// The ONE criterion a list specifies lives beside this module, because turning
/// a request's `criteria` into it is its own concern; it is re-exported here, so
/// every path that reads an allow list's criteria resolves through this module
/// as before.
pub use super::allow_list_criteria::{
    AllowListCriterion, CRITERIA_MUST_SPECIFY_ONE_MESSAGE, CRITERIA_TYPE_IS_FIXED_MESSAGE,
};

/// The resource-type segment of an allow list's ARN, read off the ARN grammar
/// the model pins for `CreateAllowListResponse$arn`
/// (`^arn:(aws|aws-cn|aws-us-gov):macie2:[a-z1-9-]{9,20}:\d{12}:allow-list\/[a-z0-9]{22}$`).
pub const ALLOW_LIST_RESOURCE_TYPE: &str = "allow-list";

/// The most allow lists one emulator process stores, across every region. The
/// EMULATOR's own memory bound, on the same footing as
/// [`crate::state::MAX_CUSTOM_DATA_IDENTIFIERS`] rather than an AWS quota:
/// without it any caller could grow one process's heap without limit. A create
/// past it answers the `ServiceQuotaExceededException` the operation models.
pub const MAX_ALLOW_LISTS: usize = 10_000;

/// The most allow lists one `ListAllowLists` response carries, which is also
/// the page a request naming no `maxResults` gets: that operation's
/// `maxResults` carries a modelled range of 1 to 25, so 25 is the page the
/// service itself bounds this read to.
pub const MAX_ALLOW_LIST_PAGE_SIZE: usize = 25;

/// The `AllowListStatusCode` of a list whose criteria Macie can access and use:
/// "OK - Macie can retrieve and parse the contents of the object." An entry of
/// [`crate::ops::types::ALLOW_LIST_STATUS_CODE_VALUES`], as is every constant
/// below.
pub const STATUS_OK: &str = "OK";

/// The code for an object the store holds nothing at: "S3_OBJECT_NOT_FOUND -
/// The object doesn't exist in Amazon S3."
pub const STATUS_S3_OBJECT_NOT_FOUND: &str = "S3_OBJECT_NOT_FOUND";

/// The code for an object the store will not serve: "S3_OBJECT_ACCESS_DENIED -
/// Macie isn't allowed to access the object".
pub const STATUS_S3_OBJECT_ACCESS_DENIED: &str = "S3_OBJECT_ACCESS_DENIED";

/// The code for an object that holds nothing: "S3_OBJECT_EMPTY - Macie can
/// retrieve the object but the object doesn't contain any content."
pub const STATUS_S3_OBJECT_EMPTY: &str = "S3_OBJECT_EMPTY";

/// The code for an object too large to be an allow list: "S3_OBJECT_OVERSIZE -
/// Macie can retrieve the object. However, the object contains too many entries
/// or its storage size exceeds the quota for an allow list."
pub const STATUS_S3_OBJECT_OVERSIZE: &str = "S3_OBJECT_OVERSIZE";

/// The code for everything else that stopped Macie using the criteria:
/// "UNKNOWN_ERROR - A transient or internal error occurred when Macie attempted
/// to retrieve or parse the object."
pub const STATUS_UNKNOWN_ERROR: &str = "UNKNOWN_ERROR";

/// The storage size past which an S3 object is too large to be used as an allow
/// list, which is what [`STATUS_S3_OBJECT_OVERSIZE`] reports. The EMULATOR's
/// own bound: the quota the status code's doc refers to ("its storage size
/// exceeds the quota for an allow list") is not stated in the model, so this is
/// a limit of this process rather than a published number, chosen well above
/// any word list a client stages in an emulator.
pub const MAX_WORDS_LIST_BYTES: i64 = 35 * 1024 * 1024;

/// A stored allow list.
///
/// Every member of `GetAllowListResponse` except `status` is a field here or
/// projected from one; `status` is derived at read time
/// ([`derive_status`]), and every member of `AllowListSummary` is a field here
/// too.
#[derive(Debug, Clone)]
pub struct AllowList {
    pub id: String,
    pub arn: String,
    /// The region the create that minted this list addressed. Macie is a
    /// regional service, so a list is only visible to a request naming the
    /// region it was created in — which is also the region in its ARN.
    pub region: String,
    pub name: String,
    pub description: Option<String>,
    /// The one criterion the list specifies.
    pub criteria: AllowListCriterion,
    /// Tags in a `BTreeMap` so a read reports them in a stable order.
    pub tags: BTreeMap<String, String>,
    pub created_at: SystemTime,
    /// When the list's settings were most recently changed, which is the
    /// instant it was created until something changes them.
    pub updated_at: SystemTime,
}

impl AllowList {
    /// This list's `tags`, as a read reports them: the modelled `TagMap` is a
    /// hash map, and absent rather than empty when it carries none.
    pub fn tags_output(&self) -> Option<std::collections::HashMap<String, String>> {
        if self.tags.is_empty() {
            return None;
        }
        Some(
            self.tags
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )
    }
}

/// How the service refuses a call addressing an allow list the addressed
/// region holds none of. The whole sentence is fixed text: the refusal names
/// the kind of resource the request addressed and nothing of the request.
pub const ALLOW_LIST_NOT_FOUND_MESSAGE: &str =
    "Input validation failed. The request specifies an allow list that does not exist.";

/// How the service refuses a delete of a list classification jobs are still
/// configured to use.
///
/// The rule is `DeleteAllowListRequest$ignoreJobChecks`'s own: "Amazon Macie
/// checks for classification jobs that use the list and have a status other
/// than COMPLETE or CANCELLED. By default, Macie rejects your request if any
/// jobs meet these criteria." The fixed text is the service's, as its own
/// answer to this refusal words it, and the only variable part is how many such
/// jobs the store holds: the sentence reads the same whatever that count is,
/// down to one job, so nothing here is singularised.
pub fn allow_list_in_use_message(jobs: usize) -> String {
    format!(
        "The allow list can't be deleted. {jobs} jobs are configured to use the list and \
         those jobs have a status other than Complete or Cancelled."
    )
}

/// How the service refuses a create whose `s3WordsList` names an object the
/// object store holds nothing at. The fixed text is the service's; the variable
/// parts are the bucket name and the object key the request named.
pub fn s3_object_absent_message(bucket_name: &str, object_key: &str) -> String {
    format!("The S3 object doesn't exist: {bucket_name}, {object_key}")
}

/// Mint the ARN of an allow list:
/// `arn:aws:macie2:<region>:<account>:allow-list/<id>`.
pub fn allow_list_arn(region: &str, id: &str) -> String {
    ::engine::emulator_arn(
        crate::state::ARN_SERVICE,
        region,
        &format!("{ALLOW_LIST_RESOURCE_TYPE}/{id}"),
    )
}

/// What the object store answered about the S3 object an allow list's criteria
/// name.
///
/// The reading of each answer is the tree's shared one
/// ([`::service_common::s3_object_read`]); what it MEANS for an allow list —
/// which `AllowListStatusCode` it derives — is this service's, and is
/// [`WordsListAnswer::status_code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordsListAnswer {
    /// The store holds the object and it carries content.
    Readable,
    /// The store holds the object and it carries nothing.
    Empty,
    /// The store holds the object and its storage size is past
    /// [`MAX_WORDS_LIST_BYTES`].
    Oversize,
    /// The store answered that it holds nothing at that address, or the address
    /// names no object anywhere.
    Absent,
    /// The store answered that the read is not permitted.
    Denied,
    /// There is an object store and this object cannot be read through it.
    Unreadable,
    /// Nothing answered the read: no object store is mounted to ask, or the
    /// call could not be dispatched. Nothing is known about the object.
    Unanswered,
}

impl WordsListAnswer {
    /// The `AllowListStatusCode` this answer derives.
    ///
    /// Each arm is the status code whose own documentation states this answer.
    /// An answer nothing gave ([`Self::Unanswered`]) is the same code as an
    /// answer no object can be read through: "a transient or internal error
    /// occurred when Macie attempted to retrieve or parse the object" is
    /// exactly what is known then.
    pub fn status_code(self) -> &'static str {
        match self {
            Self::Readable => STATUS_OK,
            Self::Empty => STATUS_S3_OBJECT_EMPTY,
            Self::Oversize => STATUS_S3_OBJECT_OVERSIZE,
            Self::Absent => STATUS_S3_OBJECT_NOT_FOUND,
            Self::Denied => STATUS_S3_OBJECT_ACCESS_DENIED,
            Self::Unreadable | Self::Unanswered => STATUS_UNKNOWN_ERROR,
        }
    }

    /// Whether the object store itself answered that it holds NO object at the
    /// address the criteria named — the one answer a create refuses on, since
    /// it is the store saying the list could never be built from that address.
    pub fn is_absent(self) -> bool {
        matches!(self, Self::Absent)
    }
}

/// Ask this emulator's own Amazon S3 about the object at `bucket_name`/`key` of
/// `region`.
///
/// The address is checked for being an address at all before anything is asked
/// of the store, through the tree's one reading of a bucket name
/// ([`::service_common::s3_bucket_name::is_well_formed`]): a blank bucket or
/// key, and a bucket name Amazon S3 holds no bucket under, name no object
/// anywhere. The object's METADATA is what is asked for — the status depends on
/// whether the object is there and how large it is, never on its contents — so
/// no caller-sized body is ever buffered here.
pub async fn read_words_list(
    callback: &EmbeddedEmulator,
    region: &str,
    bucket_name: &str,
    object_key: &str,
) -> WordsListAnswer {
    let (bucket_name, object_key) = (bucket_name.trim(), object_key.trim());
    if bucket_name.is_empty()
        || object_key.is_empty()
        || !::service_common::s3_bucket_name::is_well_formed(bucket_name)
    {
        return WordsListAnswer::Absent;
    }
    // Built by the one shared constructor, so this hop is configured exactly as
    // every other emulated service's hop into the object store is. The
    // transport is in-process: it opens no socket and reaches nothing outside
    // this process.
    let headed = ::service_common::s3_client::s3_client(callback, region)
        .head_object()
        .bucket(bucket_name)
        .key(object_key)
        .send()
        .await;
    let headed = match headed {
        Ok(headed) => headed,
        Err(error) => {
            return match ::service_common::s3_object_read::answered_refusal(&error) {
                ::service_common::s3_object_read::ObjectRead::Absent => WordsListAnswer::Absent,
                ::service_common::s3_object_read::ObjectRead::Denied => WordsListAnswer::Denied,
                ::service_common::s3_object_read::ObjectRead::Unanswered => {
                    WordsListAnswer::Unanswered
                }
                _ => WordsListAnswer::Unreadable,
            }
        }
    };
    // An object the store reports no length for carries nothing, which is the
    // same answer as one it reports zero bytes for.
    match headed.content_length().unwrap_or_default() {
        bytes if bytes <= 0 => WordsListAnswer::Empty,
        bytes if bytes > MAX_WORDS_LIST_BYTES => WordsListAnswer::Oversize,
        _ => WordsListAnswer::Readable,
    }
}

/// The status of a list whose criteria are `criterion`, as of now.
///
/// For an S3 criterion the answer comes from the object store
/// ([`read_words_list`]). For a regex criterion the status is what Macie can do
/// with the expression — its doc states "If the list's criteria specify a
/// regular expression (regex), this value is typically OK. Amazon Macie can
/// compile the expression" — so the expression is COMPILED and the status says
/// whether that succeeded. The compile is the crate's one bounded compile of a
/// caller's pattern ([`super::bounded_pattern`]); the pattern is never matched
/// against anything here.
pub async fn derive_status(
    callback: &EmbeddedEmulator,
    region: &str,
    criterion: &AllowListCriterion,
) -> AllowListStatus {
    let code = match criterion {
        AllowListCriterion::Regex(pattern) => {
            if pattern_is_runnable(pattern) {
                STATUS_OK
            } else {
                STATUS_UNKNOWN_ERROR
            }
        }
        AllowListCriterion::WordsList {
            bucket_name,
            object_key,
        } => read_words_list(callback, region, bucket_name, object_key)
            .await
            .status_code(),
    };
    AllowListStatus {
        code: code.to_string(),
        description: None,
    }
}

/// Whether `pattern` is an expression this service can compile and run.
fn pattern_is_runnable(pattern: &str) -> bool {
    super::bounded_pattern::pattern_compiles(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;
    // The criteria SHAPE and its S3 member are what these cases build a request's
    // criteria out of; the module itself now reads the criterion
    // ([`AllowListCriterion`]) rather than the shape.
    use crate::ops::types::{AllowListCriteria, S3WordsList};

    /// Every answer the object store can give derives the status code whose own
    /// documentation states that answer, and only the store's "no such object"
    /// answer is the one a create refuses on.
    #[test]
    fn each_object_store_answer_derives_its_documented_status_code() {
        let rows: &[(&str, WordsListAnswer, &str)] = &[
            ("the object is there", WordsListAnswer::Readable, STATUS_OK),
            (
                "the object holds nothing",
                WordsListAnswer::Empty,
                STATUS_S3_OBJECT_EMPTY,
            ),
            (
                "the object is too large",
                WordsListAnswer::Oversize,
                STATUS_S3_OBJECT_OVERSIZE,
            ),
            (
                "the store holds no such object",
                WordsListAnswer::Absent,
                STATUS_S3_OBJECT_NOT_FOUND,
            ),
            (
                "the read is not permitted",
                WordsListAnswer::Denied,
                STATUS_S3_OBJECT_ACCESS_DENIED,
            ),
            (
                "the store answered something no object can be read through",
                WordsListAnswer::Unreadable,
                STATUS_UNKNOWN_ERROR,
            ),
            (
                "nothing answered the read",
                WordsListAnswer::Unanswered,
                STATUS_UNKNOWN_ERROR,
            ),
        ];
        for (case, answer, expected) in rows {
            let actual = answer.status_code();
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
            assert!(
                crate::ops::types::ALLOW_LIST_STATUS_CODE_VALUES.contains(&actual),
                "case {case:?}: {actual:?} must be a modelled AllowListStatusCode"
            );
            assert_eq!(
                answer.is_absent(),
                *expected == STATUS_S3_OBJECT_NOT_FOUND,
                "case {case:?}: only the store's no-such-object answer is absence"
            );
        }
    }

    /// A criteria shape becomes the ONE criterion it specifies, and one that
    /// specifies both or neither is refused with the documented rule.
    #[test]
    fn criteria_specify_exactly_one_criterion() {
        let words = || S3WordsList {
            bucket_name: "word-lists".to_string(),
            object_key: "lists/allow.txt".to_string(),
        };
        let accepted: &[(&str, AllowListCriteria, AllowListCriterion)] = &[
            (
                "a regex alone",
                AllowListCriteria {
                    regex: Some("[0-9]{3}".to_string()),
                    s3_words_list: None,
                },
                AllowListCriterion::Regex("[0-9]{3}".to_string()),
            ),
            (
                "an S3 object alone",
                AllowListCriteria {
                    regex: None,
                    s3_words_list: Some(words()),
                },
                AllowListCriterion::WordsList {
                    bucket_name: "word-lists".to_string(),
                    object_key: "lists/allow.txt".to_string(),
                },
            ),
        ];
        for (case, criteria, expected) in accepted {
            let actual = AllowListCriterion::from_modeled(criteria);
            assert_eq!(
                actual.as_ref(),
                Ok(expected),
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
            // The criterion survives the shape a read reports it as.
            let round_tripped = AllowListCriterion::from_modeled(&expected.to_modeled());
            assert_eq!(
                round_tripped.as_ref(),
                Ok(expected),
                "case {case:?}: the criterion must survive its modelled form, \
                 got {round_tripped:?}"
            );
        }

        let refused: &[(&str, AllowListCriteria)] = &[
            (
                "both a regex and an S3 object",
                AllowListCriteria {
                    regex: Some("[0-9]{3}".to_string()),
                    s3_words_list: Some(words()),
                },
            ),
            (
                "neither",
                AllowListCriteria {
                    regex: None,
                    s3_words_list: None,
                },
            ),
        ];
        for (case, criteria) in refused {
            let actual = AllowListCriterion::from_modeled(criteria);
            assert_eq!(
                actual,
                Err(CRITERIA_MUST_SPECIFY_ONE_MESSAGE.to_string()),
                "case {case:?}: expected the documented refusal, got {actual:?}"
            );
        }
    }

    /// The refusal a create earns for an object the store holds nothing at
    /// names the bucket and the key the request carried, whatever they are.
    #[test]
    fn the_absent_object_refusal_names_the_requested_address() {
        for (case, bucket, key) in [
            ("a plain key", "word-lists", "does-not-exist.txt"),
            ("a key with a prefix", "another-bucket", "lists/nested/absent.csv"),
            ("a key with a space", "third-bucket", "my words.txt"),
        ] {
            let message = s3_object_absent_message(bucket, key);
            let expected = format!("The S3 object doesn't exist: {bucket}, {key}");
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }

    /// An allow list's ARN carries the service, the region the call addressed,
    /// the emulator account and the resource, and fits the modelled grammar's
    /// width.
    #[test]
    fn a_minted_arn_fits_the_modelled_grammar() {
        for region in ["us-east-1", "eu-central-1"] {
            let id = crate::state::new_resource_id(7, SystemTime::UNIX_EPOCH);
            let arn = allow_list_arn(region, &id);
            let expected = format!(
                "arn:aws:macie2:{region}:{}:allow-list/{id}",
                ::engine::emulator_account_id()
            );
            assert_eq!(
                arn, expected,
                "case {region:?}: expected {expected:?}, got {arn:?}"
            );
            let width = arn.chars().count();
            assert!(
                (71..=89).contains(&width),
                "case {region:?}: the modelled ARN width is 71..89, got {width} in {arn:?}"
            );
        }
    }

    /// An expression Macie can compile derives OK; one it cannot does not.
    #[test]
    fn a_regex_criterion_reports_whether_the_expression_compiles() {
        for (case, pattern, expected) in [
            ("a character class", "[0-9]{3}-[0-9]{4}", true),
            ("an anchored literal", "^allowed$", true),
            ("an unclosed group", "(unclosed", false),
            ("a dangling repetition", "*", false),
        ] {
            let actual = pattern_is_runnable(pattern);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?} for {pattern:?}, got {actual:?}"
            );
        }
    }
}
