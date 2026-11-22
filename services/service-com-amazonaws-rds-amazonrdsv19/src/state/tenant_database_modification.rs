// SPDX-License-Identifier: CC0-1.0

//! What a MODIFICATION of a tenant database changes, and what the call that made it reports.
//!
//! `ModifyTenantDatabase` "modifies an existing tenant database in a DB instance. You can
//! change the tenant database name or the master user password", and the modelled result
//! carries `TenantDatabasePendingModifiedValues` — "a response element in the
//! `ModifyTenantDatabase` operation that describes changes that will be applied. Specific
//! changes are identified by subelements." So the call answers the tenant database as it
//! WAS, with what it changed named in those pending values, and the change is the tenant
//! database's own from then on: the next caller finds it under its new name, and a report of
//! the tenant database itself carries no pending values.
//!
//! The constraints the renaming member publishes live here too, because they are the rules
//! the name a modification offers is held to ([`check_new_tenant_db_name`]).

use super::*;
use crate::ops::types::TenantDatabasePendingModifiedValues;

/// How long a tenant database's name may be.
///
/// `ModifyTenantDatabaseMessage$NewTenantDBName` publishes the bound: "Can't be longer than
/// 8 characters"
/// (`smithy_query(query="doc",shape="ModifyTenantDatabaseMessage",member="NewTenantDBName")`).
pub const MAX_TENANT_DB_NAME_LEN: usize = 8;

/// The names `NewTenantDBName` documents as reserved: "Can't be the string null or any
/// other reserved word."
///
/// Only the word the member itself names is enforced. "Any other reserved word" is each
/// engine's own SQL keyword list, which this service does not hold — and refusing a name no
/// published list carries would refuse names AWS accepts, exactly as
/// [`super::check_master_username`] says of the same clause.
const RESERVED_TENANT_DB_NAMES: [&str; 1] = ["null"];

/// Refuse a `NewTenantDBName` outside the constraints its member documents: the length
/// bound ([`MAX_TENANT_DB_NAME_LEN`]) and the reserved words
/// ([`RESERVED_TENANT_DB_NAMES`]).
///
/// Returns the sentence to refuse the request with; the CALLER answers it with
/// `InvalidParameterValue`, the Query API common error for a value outside the constraints
/// its own member publishes.
pub fn check_new_tenant_db_name(value: &str) -> Result<(), String> {
    let length = value.chars().count();
    if value.is_empty() || length > MAX_TENANT_DB_NAME_LEN {
        return Err(format!(
            "The parameter NewTenantDBName must contain from 1 to {MAX_TENANT_DB_NAME_LEN} \
             characters."
        ));
    }
    if RESERVED_TENANT_DB_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(value))
    {
        return Err(format!(
            "NewTenantDBName {value} cannot be used as it is a reserved word used by the engine"
        ));
    }
    Ok(())
}

/// What one `ModifyTenantDatabase` call changed about a tenant database, as the modelled
/// `TenantDatabasePendingModifiedValues` reports it.
///
/// It carries exactly that structure's two subelements: the new name a rename asked for, and
/// whether the master user password was set. The password itself is NOT held — the member is
/// `@sensitive` and RDS reports no member carrying it — so what is reported is the masked
/// form every pending credential of this service is reported as
/// ([`super::MASKED_PENDING_MASTER_USER_PASSWORD`]).
#[derive(Debug, Clone, Default)]
pub struct TenantDatabaseModification {
    /// `TenantDatabasePendingModifiedValues$TenantDBName`: the name the tenant database is
    /// renamed to, for a request that renamed it.
    pub tenant_db_name: Option<String>,
    /// Whether the request set the master user password.
    pub master_user_password: bool,
}

impl TenantDatabaseModification {
    /// The change a request asked for: the name it renames the tenant database to, if any,
    /// and whether it sets the master user password.
    pub fn requested(tenant_db_name: Option<String>, master_user_password: bool) -> Self {
        Self {
            tenant_db_name,
            master_user_password,
        }
    }

    /// Whether this change does anything at all, which is what decides whether the call
    /// reports any pending values: a request that renames nothing and sets no password
    /// leaves the tenant database as it is and reports none.
    pub fn changes_anything(&self) -> bool {
        self.tenant_db_name.is_some() || self.master_user_password
    }

    /// Project the change onto the modelled `TenantDatabasePendingModifiedValues`.
    pub fn to_wire(&self) -> TenantDatabasePendingModifiedValues {
        TenantDatabasePendingModifiedValues {
            tenant_db_name: self.tenant_db_name.clone(),
            master_user_password: self
                .master_user_password
                .then(|| MASKED_PENDING_MASTER_USER_PASSWORD.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The constraints `NewTenantDBName` publishes, over the length bound it states and the
    /// one reserved word it names.
    #[test]
    fn a_new_tenant_db_name_is_held_to_its_published_constraints() {
        for case in ["a", "tenant1", "eightchr"] {
            assert_eq!(
                check_new_tenant_db_name(case),
                Ok(()),
                "case {case:?}: a name within the published bound is accepted"
            );
        }
        for case in ["", "ninechars"] {
            assert!(
                check_new_tenant_db_name(case).is_err(),
                "case {case:?}: expected a name outside 1 to {MAX_TENANT_DB_NAME_LEN} \
                 characters to be refused"
            );
        }
        for case in ["null", "NULL"] {
            let refused = check_new_tenant_db_name(case);
            assert!(
                refused
                    .as_ref()
                    .err()
                    .is_some_and(|message| message.contains("reserved word")),
                "case {case:?}: expected the reserved-word refusal, got {refused:?}"
            );
        }
    }

    /// What the call that changed a tenant database reports: the new name as it is, and the
    /// password MASKED rather than as itself.
    #[test]
    fn the_change_is_reported_with_the_password_masked() {
        let renamed = TenantDatabaseModification::requested(Some("renamed".to_string()), true);
        assert!(
            renamed.changes_anything(),
            "a rename with a password change is a change"
        );
        let wire = renamed.to_wire();
        assert_eq!(
            (
                wire.tenant_db_name.as_deref(),
                wire.master_user_password.as_deref()
            ),
            (Some("renamed"), Some(MASKED_PENDING_MASTER_USER_PASSWORD)),
            "expected the new name and the masked password, got {wire:?}"
        );
        let nothing = TenantDatabaseModification::default();
        assert!(
            !nothing.changes_anything(),
            "a request that renames nothing and sets no password changes nothing"
        );
    }
}
