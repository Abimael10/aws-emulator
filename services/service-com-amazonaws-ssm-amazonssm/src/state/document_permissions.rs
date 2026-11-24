// SPDX-License-Identifier: CC0-1.0

//! Who an SSM document is shared with: what `ModifyDocumentPermission` records and
//! `DescribeDocumentPermission` reports.
//!
//! "If you created the document, you are the owner. If a document is shared, it can either
//! be shared privately (by specifying a user's Amazon Web Services account ID) or publicly
//! (All)" (`DescribeDocumentPermission`). A share is a fact of the DOCUMENT it is granted
//! on, so it is held on [`DocumentRecord`] beside that document's versions and tags: a
//! share survives an update, goes with the document when it is deleted, and is reached
//! through the one store every document read resolves against — so what a share call
//! persisted is what a later read reports.
//!
//! One account is shared the document at ONE version at a time ("The version of the current
//! document shared with that account", `AccountSharingInfo$SharedDocumentVersion`), which
//! is why the sharing is a map from account id to version rather than a list of grants: a
//! second share of the same account replaces the version the first one granted. The map is
//! ordered, so a paged read walks the accounts in one stable, total order and can resume at
//! the account id it stopped at rather than at an offset.
//!
//! Two modelled constraints bound such a page, and the SHORTER one wins: a read may ask for
//! up to two hundred items (`DescribeDocumentPermissionRequest$MaxResults`, range 1..200),
//! while the list it is answered in holds at most twenty
//! (`DescribeDocumentPermissionResponse$AccountIds`, len 0..20) — a longer page could not be
//! serialised at all, so [`permission_page_size`] bounds what `MaxResults` asks for by what
//! the response can carry.

use std::collections::BTreeMap;

use crate::ops::types::AccountSharingInfo;

use super::{ComAmazonawsSsmAmazonssmState, DocumentRecord, DOCUMENT_VERSION_DEFAULT};

/// The account id that names EVERY account: "If you share a document publicly, you must
/// specify All as the account ID" (`ModifyDocumentPermissionRequest$AccountIdsToAdd`).
///
/// The modelled `AccountId` pattern (`^(?i)all|[0-9]{12}$`) spells that value
/// case-insensitively, so every spelling of it names the one public share, and a read
/// reports it in this spelling.
pub const ALL_ACCOUNTS: &str = "all";

/// The one permission type this API has: "The permission type for the document. The
/// permission type can be Share" (`DocumentPermissionType`, whose value list is
/// `crate::ops::types::DOCUMENT_PERMISSION_TYPE_VALUES`).
pub const PERMISSION_TYPE_SHARE: &str = "Share";

/// The version a share that names none grants: "(Optional) The version of the document to
/// share. If it isn't specified, the system choose the Default version to share"
/// (`ModifyDocumentPermissionRequest$SharedDocumentVersion`).
pub const DEFAULT_SHARED_DOCUMENT_VERSION: &str = DOCUMENT_VERSION_DEFAULT;

/// How many accounts ONE document may be shared privately with: "By default, you can share
/// a private document with a maximum of 1,000 accounts" (`DocumentPermissionLimit`).
pub const MAX_SHARED_ACCOUNTS: usize = 1_000;

/// How many documents one account may share publicly: "and publicly share up to five
/// documents" (`DocumentPermissionLimit`, the same sentence).
pub const MAX_PUBLICLY_SHARED_DOCUMENTS: usize = 5;

/// The lower bound the modelled `MaxResults` of a permission read carries
/// (`DescribeDocumentPermissionRequest$MaxResults`, range 1..200).
pub const MIN_PERMISSION_PAGE_SIZE: i32 = 1;

/// The upper bound of that same member.
pub const MAX_PERMISSION_PAGE_SIZE: i32 = 200;

/// How many accounts ONE page of a permission read reports: the list it answers with holds
/// at most twenty (`DescribeDocumentPermissionResponse$AccountIds`, len 0..20), so a page
/// is never longer than that however many items `MaxResults` asks for.
pub const MAX_ACCOUNT_IDS_PER_PAGE: usize = 20;

/// The page size a permission read serves: what `MaxResults` asks for, bounded by the
/// longest account list the response can carry ([`MAX_ACCOUNT_IDS_PER_PAGE`]). A request
/// naming no `MaxResults` is served that same longest page.
pub fn permission_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(
        max_results,
        MIN_PERMISSION_PAGE_SIZE,
        MAX_PERMISSION_PAGE_SIZE,
    )
    .map(|requested| requested.min(MAX_ACCOUNT_IDS_PER_PAGE))
}

/// Whether `account_id` names every account rather than one of them.
pub fn is_all_accounts(account_id: &str) -> bool {
    account_id.eq_ignore_ascii_case(ALL_ACCOUNTS)
}

/// The spelling a share is held and reported under: the public value in its one spelling,
/// so sharing with `All` and stopping the share of `all` act on the same grant, and an
/// account number exactly as the call named it.
pub fn canonical_account_id(account_id: &str) -> String {
    if is_all_accounts(account_id) {
        ALL_ACCOUNTS.to_string()
    } else {
        account_id.to_string()
    }
}

/// Whether `value` is an account id at all: the public value, or the twelve digits of an
/// account number (the modelled `AccountId` pattern).
///
/// This is also the SHAPE a continuation token of a permission read has to carry, because
/// that token is the account id the next page starts at: a value outside it was not issued
/// by this read, which is what its invalid-token refusal reports.
pub fn is_account_id(value: &str) -> bool {
    is_all_accounts(value) || (value.len() == 12 && value.bytes().all(|byte| byte.is_ascii_digit()))
}

/// The version a share grants: the selector the call named, or the default version when it
/// named none.
pub fn shared_version(requested: Option<&str>) -> String {
    requested
        .filter(|selector| !selector.is_empty())
        .unwrap_or(DEFAULT_SHARED_DOCUMENT_VERSION)
        .to_string()
}

/// What a document's sharing becomes once a share call's two account lists are applied to
/// `current`.
///
/// Both lists are applied to the one map, removals last: "This action has a higher priority
/// than AccountIdsToAdd. If you specify an ID to add and the same ID to remove, the system
/// removes access to the document" (`AccountIdsToRemove`). An account already shared the
/// document and named again is shared it at the version THIS call names, because one
/// account holds the document at one version.
pub fn shared_after(
    current: &BTreeMap<String, String>,
    accounts_to_add: &[String],
    accounts_to_remove: &[String],
    shared_document_version: &str,
) -> BTreeMap<String, String> {
    let mut shared = current.clone();
    for account_id in accounts_to_add {
        shared.insert(
            canonical_account_id(account_id),
            shared_document_version.to_string(),
        );
    }
    for account_id in accounts_to_remove {
        shared.remove(&canonical_account_id(account_id));
    }
    shared
}

/// How many of `shared`'s grants are PRIVATE shares — the account numbers, which is what
/// the private-share quota counts. The public grant is not one of them: it is bounded by
/// how many documents an account shares publicly instead.
pub fn private_accounts(shared: &BTreeMap<String, String>) -> usize {
    shared
        .keys()
        .filter(|account_id| !is_all_accounts(account_id))
        .count()
}

/// Whether `shared` holds the grant that shares the document with every account.
pub fn shared_publicly(shared: &BTreeMap<String, String>) -> bool {
    shared.keys().any(|account_id| is_all_accounts(account_id))
}

/// What a permission call on a document this account cannot share is refused with: the
/// sentence `InvalidDocument` carries, in the model's own words — a document Systems Manager
/// publishes exists, and is not available to this account, which is the one answer the
/// reader and the writer both give for it.
pub const NOT_AVAILABLE_TO_SHARE: &str =
    "The SSM document doesn't exist or the document isn't available to the user";

/// How this service states a share that would put ONE document past the private-share
/// quota, in the words `DocumentPermissionLimit` states that quota in.
pub fn shared_account_limit_message() -> String {
    format!("You can share a private document with a maximum of {MAX_SHARED_ACCOUNTS} accounts")
}

/// How this service states a public share past the quota on publicly shared documents, in
/// the words `DocumentPermissionLimit` states that quota in.
pub fn public_share_limit_message() -> String {
    format!("You can publicly share up to {MAX_PUBLICLY_SHARED_DOCUMENTS} documents")
}

/// What a share call naming neither account list is refused with.
///
/// "You must specify a value for this parameter or the AccountIdsToRemove parameter"
/// (`AccountIdsToAdd`) is the rule; the sentence the service states it in is this one,
/// entirely fixed text naming both members — and the class it answers under is
/// `InvalidParameterException` ([`crate::scaffolded::Scaffolded::invalid_parameter`]) rather
/// than a constraint violation, because neither member on its own is outside anything the
/// model declares: the fault is the pair.
pub const NO_ACCOUNTS_TO_MODIFY: &str =
    "Either AccountIdsToAdd or AccountIdsToRemove must be specified for API operation";

/// What a delete of a document that is still shared is refused with: the sentence
/// `InvalidDocumentOperation` states that rule in.
pub const STILL_SHARED: &str = "You attempted to delete a document while it is still \
                                shared. You must stop sharing the document before you can \
                                delete it.";

/// What a call naming a permission type this API has not is refused with: the sentence
/// `InvalidPermissionType` carries, in the model's own words.
pub const INVALID_PERMISSION_TYPE: &str =
    "The permission type isn't supported. Share is the only supported permission type.";

/// ONE page of a document's sharing: the accounts it reports, each one's version, and the
/// account id the next page resumes at when one was left over.
#[derive(Debug, Clone, Default)]
pub struct DocumentSharingPage {
    /// "The account IDs that have permission to use this document"
    /// (`DescribeDocumentPermissionResponse$AccountIds`).
    pub account_ids: Vec<String>,
    /// The same accounts, each with "the version of the current document shared with that
    /// account" — the one list projected two ways, so the two cannot disagree.
    pub sharing: Vec<AccountSharingInfo>,
    /// "The token for the next set of items to return", or `None` when this page is the
    /// last one.
    pub next_token: Option<String>,
}

impl DocumentRecord {
    /// Whether this document is shared with any account at all, which is what a delete of
    /// a still-shared document is refused for.
    pub fn is_shared(&self) -> bool {
        !self.shared_with.is_empty()
    }

    /// Whether this document is shared with every account.
    pub fn is_shared_publicly(&self) -> bool {
        shared_publicly(&self.shared_with)
    }

    /// Whether this document is shared with the account `account_id` names, which is what a
    /// continuation token of a permission read has to resume at: that token is an account
    /// id this document's sharing held when the page before it was served.
    pub fn shares_with(&self, account_id: &str) -> bool {
        self.shared_with
            .contains_key(&canonical_account_id(account_id))
    }

    /// The page of this document's sharing that starts at the account `resume_from` names
    /// and holds at most `page_size` accounts.
    ///
    /// The accounts are walked in the one order the store holds them, so a page resumes
    /// exactly where the one before it stopped and no account is reported twice or skipped.
    /// The account the page resumes at is read in the spelling the store holds it under, so
    /// the walk's order never depends on how a caller spelled the public value.
    pub fn sharing_page(&self, resume_from: Option<&str>, page_size: usize) -> DocumentSharingPage {
        let resume_from = resume_from.map(canonical_account_id);
        let mut page = DocumentSharingPage::default();
        for (account_id, shared_version) in &self.shared_with {
            if resume_from
                .as_deref()
                .is_some_and(|from| account_id.as_str() < from)
            {
                continue;
            }
            if page.account_ids.len() >= page_size {
                page.next_token = Some(account_id.clone());
                break;
            }
            page.account_ids.push(account_id.clone());
            page.sharing.push(AccountSharingInfo {
                account_id: Some(account_id.clone()),
                shared_document_version: Some(shared_version.clone()),
            });
        }
        page
    }
}

impl ComAmazonawsSsmAmazonssmState {
    /// How many of `region`'s documents are shared with every account, which is what the
    /// quota on publicly shared documents counts.
    pub fn publicly_shared_documents(&self, region: &str) -> usize {
        self.documents
            .iter()
            .filter(|((held, _), record)| held == region && record.is_shared_publicly())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Removing an account has a higher priority than adding it, whichever order the two
    /// lists name it in, and a share of an account already shared the document moves it to
    /// the version this call names.
    #[test]
    fn a_removal_outranks_an_add_of_the_same_account() {
        let shared = shared_after(
            &BTreeMap::new(),
            &["111122223333".to_string(), "444455556666".to_string()],
            &["111122223333".to_string()],
            "$LATEST",
        );
        assert_eq!(
            shared.keys().collect::<Vec<_>>(),
            vec!["444455556666"],
            "case add two and remove one: got {shared:?}"
        );
        let moved = shared_after(&shared, &["444455556666".to_string()], &[], "$ALL");
        assert_eq!(
            moved.get("444455556666").map(String::as_str),
            Some("$ALL"),
            "case re-share at another version: got {moved:?}"
        );
    }

    /// Every spelling of the public value names the one public grant, and an account
    /// number is held exactly as it was named.
    #[test]
    fn the_public_grant_has_one_spelling() {
        for spelling in ["All", "all", "ALL", "aLl"] {
            let shared = shared_after(&BTreeMap::new(), &[spelling.to_string()], &[], "$DEFAULT");
            assert_eq!(
                shared.keys().collect::<Vec<_>>(),
                vec![ALL_ACCOUNTS],
                "case {spelling}: got {shared:?}"
            );
            assert!(
                shared_publicly(&shared) && private_accounts(&shared) == 0,
                "case {spelling}: must count as the public grant, got {shared:?}"
            );
            let stopped = shared_after(&shared, &[], &["ALL".to_string()], "$DEFAULT");
            assert!(
                stopped.is_empty(),
                "case {spelling}: must stop sharing, got {stopped:?}"
            );
        }
    }
}
