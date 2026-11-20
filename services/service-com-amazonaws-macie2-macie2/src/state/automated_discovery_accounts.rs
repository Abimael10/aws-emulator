// SPDX-License-Identifier: CC0-1.0

//! The per-ACCOUNT plane of automated sensitive data discovery: the status
//! Macie performs those activities under for each account an administrator
//! account administers in one region, which
//! `BatchUpdateAutomatedDiscoveryAccounts` changes and
//! `ListAutomatedDiscoveryAccounts` reports.
//!
//! This is a different fact from the per-region configuration in
//! [`crate::state::AutomatedDiscoveryConfiguration`], which is the
//! organization-wide settings `UpdateAutomatedDiscoveryConfiguration` writes —
//! except for ONE account, the administrator's own: the status of automated
//! sensitive data discovery for that account is the `status` of that same
//! configuration, so there is one fact in one place for it and both operations
//! that write it go through
//! [`super::ComAmazonawsMacie2Macie2State::configure_automated_discovery`].
//!
//! The other accounts this plane covers are the accounts the administrator
//! administers in the region — its member associations — because that is what
//! the auto-enable mode's doc says these two operations act on: "To enable or
//! disable it for individual member accounts, specify NEW or NONE, and then
//! enable or disable it for each account by using the
//! BatchUpdateAutomatedDiscoveryAccounts operation."
//!
//! A status here is always STORED, never derived at read time: the mode a
//! configuration names is what decides the status an account is given, and it
//! is given at the moment the configuration is written or the account is
//! associated, so a later mode change leaves it where it was. That is what the
//! same doc requires — "If you specify NEW or NONE, automated sensitive data
//! discovery continues to be enabled for any existing accounts that it's
//! currently enabled for" — and it is why `ListAutomatedDiscoveryAccounts` can
//! report every account's status without writing anything.

use std::time::SystemTime;

use super::{
    AUTOMATED_DISCOVERY_DISABLED, AUTOMATED_DISCOVERY_ENABLED,
    DEFAULT_AUTO_ENABLE_ORGANIZATION_MEMBERS, DEFAULT_AUTOMATED_DISCOVERY_STATUS,
    DEFAULT_MACIE_STATUS, Member,
};

/// The most accounts one `ListAutomatedDiscoveryAccounts` response carries,
/// which is also the page a request naming no `maxResults` gets: that
/// operation's `maxResults` carries a modelled range of 1 to 25, so 25 is the
/// page the service itself bounds this read to.
pub const MAX_AUTOMATED_DISCOVERY_ACCOUNT_PAGE_SIZE: usize = 25;

/// The most accounts one `ListAutomatedDiscoveryAccounts` request may name: the
/// `accountIds` member's doc states "The Amazon Web Services account ID for
/// each account, for as many as 50 accounts."
pub const MAX_LISTED_AUTOMATED_DISCOVERY_ACCOUNT_IDS: usize = 50;

/// The `AutoEnableMode` under which automated sensitive data discovery is
/// enabled for every account: the mode's doc states "ALL (default), enable it
/// for all existing accounts and new member accounts". An entry of
/// [`crate::ops::types::AUTO_ENABLE_MODE_VALUES`], and the mode a
/// configuration naming none takes
/// ([`DEFAULT_AUTO_ENABLE_ORGANIZATION_MEMBERS`]).
pub const AUTO_ENABLE_ALL: &str = DEFAULT_AUTO_ENABLE_ORGANIZATION_MEMBERS;

/// The `AutoEnableMode` under which it is enabled only for accounts associated
/// after the configuration was written: "NEW, enable it only for new member
/// accounts". An entry of [`crate::ops::types::AUTO_ENABLE_MODE_VALUES`].
pub const AUTO_ENABLE_NEW: &str = "NEW";

/// The `AutomatedDiscoveryAccountUpdateErrorCode` for an account this
/// administrator does not administer in the region the request addressed: the
/// member's doc states "ACCOUNT_NOT_FOUND, the account doesn't exist or you're
/// not the Amazon Macie administrator for the account". An entry of
/// [`crate::ops::types::AUTOMATED_DISCOVERY_ACCOUNT_UPDATE_ERROR_CODE_VALUES`].
pub const UPDATE_ERROR_ACCOUNT_NOT_FOUND: &str = "ACCOUNT_NOT_FOUND";

/// The `AutomatedDiscoveryAccountUpdateErrorCode` for an administered account
/// Macie is not running for: the member's doc states "ACCOUNT_PAUSED, Macie
/// isn't enabled for the account in the current Amazon Web Services Region". An
/// entry of
/// [`crate::ops::types::AUTOMATED_DISCOVERY_ACCOUNT_UPDATE_ERROR_CODE_VALUES`].
pub const UPDATE_ERROR_ACCOUNT_PAUSED: &str = "ACCOUNT_PAUSED";

/// The `RelationshipStatus` of a live membership whose account Macie is
/// SUSPENDED for, which is what [`UPDATE_ERROR_ACCOUNT_PAUSED`] reports: the
/// status means the membership is in force with Macie suspended for the
/// account. The other two statuses of a live membership
/// ([`super::CURRENT_MEMBER_RELATIONSHIP_STATUSES`]) leave Macie running for
/// the account, and a status that is no live membership at all is not
/// administered here, so it answers [`UPDATE_ERROR_ACCOUNT_NOT_FOUND`]
/// instead. An entry of the modelled `RelationshipStatus` enum.
pub const MACIE_SUSPENDED_RELATIONSHIP_STATUS: &str = "Paused";

/// The `MacieStatus` under which Macie IS enabled for this account in a region:
/// the `status` member of `EnableMacieRequest` documents "To enable Amazon
/// Macie and start all Macie activities for the account, set this value to
/// ENABLED", and the only other entry of
/// [`crate::ops::types::MACIE_STATUS_VALUES`] is `PAUSED`, under which those
/// activities are suspended. The same value as [`DEFAULT_MACIE_STATUS`], the
/// status an enable naming none takes.
pub const MACIE_RUNNING_STATUS: &str = DEFAULT_MACIE_STATUS;

/// How `ListAutomatedDiscoveryAccounts` refuses a request naming more accounts
/// than one call may ask about, which the `accountIds` member documents as 50.
pub const TOO_MANY_ACCOUNT_IDS_MESSAGE: &str =
    "The request is rejected because accountIds specifies more than 50 accounts.";

/// How `BatchUpdateAutomatedDiscoveryAccounts` refuses an account update that
/// names no account to change the status of.
pub const UPDATE_WITHOUT_ACCOUNT_ID_MESSAGE: &str =
    "The request is rejected because every account in accounts must specify an accountId.";

/// How it refuses an account update that names no new status.
pub const UPDATE_WITHOUT_STATUS_MESSAGE: &str =
    "The request is rejected because every account in accounts must specify a status.";

/// The status of automated sensitive data discovery for ONE account the
/// administrator administers in ONE region.
///
/// Both members of the modelled `AutomatedDiscoveryAccount` are this record or
/// the key it is filed under, so nothing a read reports about an account is
/// computed from the request that asked for it.
#[derive(Debug, Clone)]
pub struct AutomatedDiscoveryAccountSetting {
    /// `AutomatedDiscoveryAccountStatus` as the wire words it: an entry of
    /// [`crate::ops::types::AUTOMATED_DISCOVERY_ACCOUNT_STATUS_VALUES`].
    pub status: String,
    /// When this status was last written. The modelled
    /// `AutomatedDiscoveryAccount` carries no instant, so this is the store's
    /// own record of the change rather than a response member.
    pub updated_at: SystemTime,
}

/// Whether a batch update can change one account's status, and why it cannot
/// when it cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountAdministration {
    /// This administrator administers the account in this region and Macie is
    /// enabled for it there, so an update reaches it.
    Administered,
    /// Administered, but Macie is not enabled for the account in this region.
    MacieSuspended,
    /// No account this administrator administers in this region.
    NotAdministered,
}

impl super::ComAmazonawsMacie2Macie2State {
    /// Whether a member account's own Macie is enabled in the region its
    /// association belongs to, read off the status that association carries
    /// right now.
    fn macie_runs_for_member(member: &Member) -> bool {
        member.relationship_status != MACIE_SUSPENDED_RELATIONSHIP_STATUS
    }

    /// How `account_id` stands with this administrator in `region`, which is
    /// what a batch update's per-account error code reports.
    ///
    /// The administrator's OWN account is administered here while its Macie
    /// session runs; an account with no live membership here is not
    /// administered at all: the administrator may have a record of it but is
    /// not its Macie administrator, the condition `ACCOUNT_NOT_FOUND` names.
    pub fn automated_discovery_account_administration(
        &self,
        region: &str,
        account_id: &str,
    ) -> AccountAdministration {
        if account_id == ::engine::emulator_account_id() {
            return match self.session(region) {
                Some(session) if session.status == MACIE_RUNNING_STATUS => {
                    AccountAdministration::Administered
                }
                Some(_) => AccountAdministration::MacieSuspended,
                None => AccountAdministration::NotAdministered,
            };
        }
        match self.member(region, account_id) {
            Some(member) if !member.is_current_association() => {
                AccountAdministration::NotAdministered
            }
            Some(member) if Self::macie_runs_for_member(member) => {
                AccountAdministration::Administered
            }
            Some(_) => AccountAdministration::MacieSuspended,
            None => AccountAdministration::NotAdministered,
        }
    }

    /// The accounts `ListAutomatedDiscoveryAccounts` reports for `region`, in
    /// account-id order, so the total order a page boundary rests on is stable.
    ///
    /// These are the accounts this administrator administers there: its own
    /// account, whose status is its own configuration's, and every account it
    /// has a live membership with — the ones a batch update can reach, plus the
    /// ones it refuses with [`UPDATE_ERROR_ACCOUNT_PAUSED`], because Macie
    /// being suspended for an account does not stop the administrator
    /// administering it.
    pub fn automated_discovery_accounts_of(&self, region: &str) -> Vec<String> {
        let mut accounts: Vec<String> = self
            .members_of(region)
            .filter(|member| member.is_current_association())
            .map(|member| member.account_id.clone())
            .collect();
        accounts.push(::engine::emulator_account_id().to_string());
        // Sorting is what puts the administrator's own account and its member
        // associations in ONE ascending order, which is the order a page
        // boundary rests on; a duplicate id would make one page key name two
        // rows, so it is dropped.
        accounts.sort();
        accounts.dedup();
        accounts
    }

    /// The status of automated sensitive data discovery for `account_id` in
    /// `region`, as a read reports it.
    ///
    /// For the administrator's own account that is the status of its own
    /// configuration — the one fact [`Self::configure_automated_discovery`]
    /// writes — and for every other account the status last written for it, or
    /// [`AUTOMATED_DISCOVERY_DISABLED`] while nothing has enabled it:
    /// "DISABLED, don't perform automated sensitive data discovery
    /// activities".
    pub fn automated_discovery_account_status(&self, region: &str, account_id: &str) -> &str {
        if account_id == ::engine::emulator_account_id() {
            return self
                .automated_discovery_configuration(region)
                .map_or(DEFAULT_AUTOMATED_DISCOVERY_STATUS, |configuration| {
                    configuration.status.as_str()
                });
        }
        self.automated_discovery_accounts
            .get(&(region.to_string(), account_id.to_string()))
            .map_or(AUTOMATED_DISCOVERY_DISABLED, |setting| {
                setting.status.as_str()
            })
    }

    /// Write the status of automated sensitive data discovery for `account_id`
    /// in `region`, which is what a batch update does for each account it
    /// reaches.
    ///
    /// The administrator's own account goes to
    /// [`Self::configure_automated_discovery`], the one writer of that
    /// account's status, which keeps the settings the region already carries;
    /// every other account carries a record of its own.
    pub fn set_automated_discovery_account_status(
        &mut self,
        region: &str,
        account_id: &str,
        status: String,
        now: SystemTime,
    ) {
        if account_id == ::engine::emulator_account_id() {
            let settings = self.automated_discovery_configuration(region).map_or_else(
                || DEFAULT_AUTO_ENABLE_ORGANIZATION_MEMBERS.to_string(),
                |configuration| configuration.auto_enable_organization_members.clone(),
            );
            self.configure_automated_discovery(region, status, settings, now);
            return;
        }
        self.automated_discovery_accounts.insert(
            (region.to_string(), account_id.to_string()),
            AutomatedDiscoveryAccountSetting {
                status,
                updated_at: now,
            },
        );
    }

    /// Drop the per-account status of `account_id` in `region`. An account the
    /// administrator no longer administers has no status for it to report, so
    /// the record goes with the association.
    pub fn forget_automated_discovery_account(&mut self, region: &str, account_id: &str) {
        self.automated_discovery_accounts
            .remove(&(region.to_string(), account_id.to_string()));
    }

    /// Enable automated sensitive data discovery for every account this
    /// administrator administers in `region`, which is what a configuration
    /// written with [`AUTO_ENABLE_ALL`] asks for: "ALL (default), enable it for
    /// all existing accounts and new member accounts".
    ///
    /// Called by the write that leaves the region's configuration enabled under
    /// that mode — never by a read, and never under `NEW` or `NONE`, where the
    /// same doc states that the accounts it is currently enabled for keep it
    /// and the others are left to a batch update.
    pub fn auto_enable_automated_discovery_for_existing_accounts(
        &mut self,
        region: &str,
        now: SystemTime,
    ) {
        for account_id in self.automated_discovery_accounts_of(region) {
            // The administrator's own account is enabled by the write that
            // brought this call here, so only the accounts carrying a record of
            // their own are written.
            if account_id == ::engine::emulator_account_id() {
                continue;
            }
            self.set_automated_discovery_account_status(
                region,
                &account_id,
                AUTOMATED_DISCOVERY_ENABLED.to_string(),
                now,
            );
        }
    }

    /// Enable it for an account being associated with this administrator in
    /// `region`, when the region's configuration auto-enables new member
    /// accounts — the modes `ALL` ("enable it for all existing accounts and new
    /// member accounts") and `NEW` ("enable it only for new member accounts")
    /// — and leave the account alone otherwise.
    ///
    /// Called by the operation that associates the account, so the status an
    /// account carries is settled when it joins rather than derived by whoever
    /// reads it next.
    pub fn auto_enable_automated_discovery_for_new_account(
        &mut self,
        region: &str,
        account_id: &str,
        now: SystemTime,
    ) {
        let auto_enables_new_accounts = self
            .automated_discovery_configuration(region)
            .is_some_and(|configuration| {
                configuration.status == AUTOMATED_DISCOVERY_ENABLED
                    && matches!(
                        configuration.auto_enable_organization_members.as_str(),
                        AUTO_ENABLE_ALL | AUTO_ENABLE_NEW
                    )
            });
        if auto_enables_new_accounts {
            self.set_automated_discovery_account_status(
                region,
                account_id,
                AUTOMATED_DISCOVERY_ENABLED.to_string(),
                now,
            );
        }
    }
}
