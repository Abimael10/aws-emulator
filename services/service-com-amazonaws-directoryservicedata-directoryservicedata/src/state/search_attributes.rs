// SPDX-License-Identifier: CC0-1.0

//! Which attribute names a SEARCH may name.
//!
//! `SearchUsers` and `SearchGroups` carry `SearchAttributes`, the attributes the directory
//! is asked to match the `SearchString` against
//! (`smithy_query(query="doc", shape="SearchUsersRequest", member="SearchAttributes")`:
//! "One or more data attributes that are used to search for a user. For a list of supported
//! attributes, see Directory Service Data Attributes"). That attribute list is what decides
//! the member: it states, for every attribute of a user or a group object, whether the
//! directory INDEXES it for search, and a directory can only match a search against an
//! attribute it indexes. So naming an attribute of the object that the list does not mark
//! searchable is not a search that finds nothing — it is a request the directory refuses,
//! with `ValidationException` (HTTP 400) and the modelled `Reason`
//! [`INVALID_ATTRIBUTE_FOR_SEARCH`] (`ValidationExceptionReason` —
//! `crate::ops::types::VALIDATION_EXCEPTION_REASON_VALUES`).
//!
//! [`UNSEARCHABLE`] holds those attributes, as far as this plane carries them: an attribute
//! the list holds for a user or a group object and marks NOT searchable. An attribute this
//! plane does hold and the list marks searchable is matched the way every searched object
//! is matched — the value it holds for a named attribute IS the search string — and a name
//! the list has no entry for is left to that rule too: this plane holds an attribute whose
//! name its own schema catalogue does not carry ([`super::attribute_schema`]) and reports it
//! as written, so a value it holds stays searchable under the name it holds it under.
//!
//! Why the set is small: what this plane HOLDS of an object is its modelled members and the
//! DATA attributes a caller writes into `OtherAttributes` — the attributes the schema
//! catalogue carries, which are the list's own data attributes, searchable as the data they
//! are. The object's NAMING attributes are the exception: the directory derives them rather
//! than holding them as data, and the one of those this API models no member for is the
//! common name — so that is the entry here. An attribute of either object class that the
//! list marks unsearchable belongs in this table beside it; nothing else about the gate
//! changes.

use crate::ops::types::ValidationException;

/// The `ValidationExceptionReason` for an attribute the directory does not index for
/// search. One of `ValidationExceptionReason`'s modelled values — the one that names the
/// attribute's unfitness for a SEARCH rather than its name, its value or its object.
pub const INVALID_ATTRIBUTE_FOR_SEARCH: &str = "INVALID_ATTRIBUTE_FOR_SEARCH";

/// Every attribute of a user or a group object that the service's attribute list holds but
/// does NOT mark searchable, [`super::fold`]ed because an AD attribute name is case
/// insensitive.
///
/// `cn` is the object's common name — the directory names the objects this API creates
/// `CN=<SAMAccountName>` ([`super::Directory::distinguished_name`]), so the value it holds
/// is the account name over again — and the list marks it a valid attribute of both object
/// classes that is not searchable. An attribute reachable through a member of its own
/// (`SAMAccountName`, `SID`, `DistinguishedName`, `UserPrincipalName`, `EmailAddress`,
/// `GivenName`, `Surname`, `GroupType`, `GroupScope`) is searchable under that member's name
/// and under the directory's own name for it, and a DATA attribute of the schema catalogue
/// ([`super::attribute_schema::CATALOG`]) is searchable as the data it is, which is why
/// neither kind is here.
const UNSEARCHABLE: &[&str] = &["cn"];

/// How a search naming an attribute the directory does not index for search is refused, or
/// `None` when every name in `names` is one it can search.
///
/// The offender named is the FIRST such name the request listed, so one request is refused
/// with one sentence; the name is quoted exactly as the request spelled it, because the
/// name is the caller's own value and the refusal is about the attribute it named.
#[must_use]
pub fn unsearchable_attribute(names: &[String]) -> Option<ValidationException> {
    let offender = names
        .iter()
        .find(|name| UNSEARCHABLE.contains(&super::fold(name).as_str()))?;
    Some(ValidationException {
        message: Some(format!("Attribute '{offender}' cannot be searched.")),
        reason: Some(INVALID_ATTRIBUTE_FOR_SEARCH.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::{unsearchable_attribute, INVALID_ATTRIBUTE_FOR_SEARCH, UNSEARCHABLE};

    fn named(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    /// RULE: an attribute the directory does not index for search is refused whatever case
    /// the request spelled it in, and the refusal quotes that spelling.
    #[test]
    fn an_unsearchable_attribute_is_refused_in_every_spelling() {
        for held in UNSEARCHABLE {
            for spelling in [
                (*held).to_string(),
                held.to_uppercase(),
                held.to_lowercase(),
                // One letter in the other case, so nothing passes on a whole-word fold.
                {
                    let mut characters = held.chars();
                    match characters.next() {
                        Some(first) => {
                            first.to_uppercase().collect::<String>() + characters.as_str()
                        }
                        None => String::new(),
                    }
                },
            ] {
                let refusal = unsearchable_attribute(&named(&[&spelling]))
                    .unwrap_or_else(|| panic!("case {spelling:?}: expected a refusal, got none"));
                assert_eq!(
                    refusal.message.as_deref(),
                    Some(format!("Attribute '{spelling}' cannot be searched.").as_str()),
                    "case {spelling:?}: the sentence must quote the name as written, got {:?}",
                    refusal.message
                );
                assert_eq!(
                    refusal.reason.as_deref(),
                    Some(INVALID_ATTRIBUTE_FOR_SEARCH),
                    "case {spelling:?}: expected the modelled Reason, got {:?}",
                    refusal.reason
                );
            }
        }
    }

    /// RULE: an attribute set this plane can search refuses nothing, and a set mixing one
    /// unsearchable name among them is refused on the first of them.
    #[test]
    fn a_searchable_set_is_taken_and_a_mixed_one_is_refused_on_the_first_offender() {
        // (case, the names, the name the refusal quotes)
        let rows: [(&str, &[&str], Option<&str>); 6] = [
            ("the account name alone", &["SAMAccountName"], None),
            (
                "several attributes with members of their own",
                &["GivenName", "sn", "mail", "userPrincipalName", "objectSid"],
                None,
            ),
            (
                "an attribute of the schema catalogue",
                &["Description", "Title"],
                None,
            ),
            ("one offender alone", &["Cn"], Some("Cn")),
            (
                "one offender after a searchable name",
                &["SAMAccountName", "Cn"],
                Some("Cn"),
            ),
            (
                "two spellings of the offender: the first the request listed",
                &["cN", "Cn"],
                Some("cN"),
            ),
        ];
        for (case, names, expected) in rows {
            let refusal = unsearchable_attribute(&named(names));
            let quoted = refusal
                .as_ref()
                .and_then(|refusal| refusal.message.clone())
                .map(|message| message.replace("Attribute '", "").replace("' cannot be searched.", ""));
            assert_eq!(
                quoted.as_deref(),
                expected,
                "case {case:?}: expected {expected:?} refused, got {quoted:?}"
            );
        }
    }
}
