// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS a refusal: the message each error class carries.
//!
//! An HTTP status, an error code and an error message are all part of the contract a client
//! branches on, so the wording of a refusal lives here — one constant per error class,
//! shared by every operation that answers it — rather than being spelled at each site that
//! raises it.
//!
//! Lake Formation words "the thing you addressed is not there" with a sentence that names
//! neither the name the request carried nor the kind of thing it addressed, so each of
//! those is wholly fixed text.
//!
//! A refusal that is NOT about the thing a request addressed but about a VALUE inside it —
//! an identifier that is not an ARN, a permission the service does not define — names that
//! value, because a request carrying a list of them is refused for one entry and the
//! caller cannot tell which without being told. Those are a fixed template plus the
//! rejected value, and the template is what lives here.

/// What a read or an update of a data cells filter the store does not hold answers.
///
/// Wholly fixed text: it names neither the filter nor the table, so the same sentence
/// answers every such call.
pub const FILTER_ENTITY_NOT_FOUND: &str = "Entity Not Found";

/// What a read of a data-lake location that is not registered answers.
///
/// Wholly fixed text, like [`FILTER_ENTITY_NOT_FOUND`] — and in its own casing, which is
/// the one this service words that resource's absence in.
pub const RESOURCE_ENTITY_NOT_FOUND: &str = "Entity not found";

/// What an operation addressing an LF-tag key the Data Catalog does not hold answers: the
/// readers, the update and the delete alike.
///
/// Wholly fixed text: the key the request carried is not echoed.
pub const TAG_KEY_DOES_NOT_EXIST: &str = "Tag key does not exist";

/// What a create of a data cells filter the store already holds answers.
///
/// Wholly fixed text, like the absence above: neither the filter nor the table it is on is
/// named.
pub const FILTER_ALREADY_EXISTS: &str = "Data cells filter already exists.";

/// What a register of a data-lake location that is already registered answers.
///
/// Wholly fixed text: the ARN the request carried is not echoed.
pub const RESOURCE_ALREADY_REGISTERED: &str = "Resource is already registered";

/// What an operation handed a `ResourceArn` that is not an Amazon Resource Name answers.
///
/// Wholly fixed text: the value the request carried is not echoed.
pub const UNSUPPORTED_RESOURCE_ARN: &str = "Un-supported resource arn format";

/// What a create of a data cells filter whose `RowFilter` names both of its alternatives
/// answers.
///
/// A PartiQL predicate is an expression OR the all-rows wildcard, never both, and the
/// refusal is this sentence whatever the expression was.
pub const ROW_FILTER_BOTH_SET: &str = "Filter expression and AllRowsWildcard cannot both be set.";

/// What a call whose `Resource.Table` names BOTH a table `Name` and a `TableWildcard`
/// answers.
///
/// `TableResource` carries both members and they are alternatives — one table, or every
/// table of the database — so naming both names no table. Wholly fixed text: neither the
/// table nor the database the request carried is echoed.
pub const TABLE_NAME_AND_WILDCARD: &str = "Table name and table wildcard cannot both be present.";

/// What a revoke that would take nothing away answers.
///
/// Wholly fixed text, in two sentences: the permissions the request named are NOT
/// interpolated, because the request already carries them.
pub const NOTHING_TO_REVOKE: &str =
    "No permissions revoked. Grantee has no permissions and no grantable permissions on \
     resource.";

/// What a create of an LF-tag whose key the Data Catalog already holds answers.
///
/// Wholly fixed text: neither the key nor the catalog is named.
pub const TAG_KEY_ALREADY_EXISTS: &str = "Tag key already exists";

/// The fixed text of the refusal a value that is not an Amazon Resource Name draws from
/// this service's ARN reader, which the rejected value follows.
///
/// This one is NOT wholly fixed: a list of principals is refused for one of its entries,
/// and the caller cannot tell which entry that was unless the refusal says, so the value
/// the request carried is the sentence's one variable part. It is the value as written,
/// unquoted.
pub const INVALID_ARN_PREFIX: &str = "Invalid ARN:ARNs must start with 'arn:': ";

/// [`INVALID_ARN_PREFIX`] followed by `identifier`: what an identifier that does not even
/// begin an ARN is refused with.
pub fn invalid_arn(identifier: &str) -> String {
    format!("{INVALID_ARN_PREFIX}{identifier}")
}

/// The first of the two fixed sentences a write naming a value that is not a Lake
/// Formation permission draws, up to the bracket the rejected values open.
///
/// Fixed to the character, the leading space included.
pub const INVALID_PERMISSIONS_PREFIX: &str =
    " Please grant only valid Lake Formation permissions. [";

/// The second of those two sentences, from the bracket the rejected values close.
///
/// Fixed to the character, the trailing space included.
pub const INVALID_PERMISSIONS_SUFFIX: &str =
    "] are not all valid Lake Formation permissions. ";

/// How the rejected values are listed between those two sentences.
const REJECTED_PERMISSION_SEPARATOR: &str = ", ";

/// What a grant or a revoke naming `rejected` — values the `Permission` enum does not
/// carry — is refused with: the two fixed sentences with those values bracketed between
/// them, in the order the request named them.
pub fn invalid_permissions(rejected: &[String]) -> String {
    format!(
        "{INVALID_PERMISSIONS_PREFIX}{}{INVALID_PERMISSIONS_SUFFIX}",
        rejected.join(REJECTED_PERMISSION_SEPARATOR)
    )
}
