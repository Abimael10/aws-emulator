// SPDX-License-Identifier: CC0-1.0

//! The Scope entity: the record `CreateScope` persists and `GetScope` reads back.
//!
//! Kept beside [`super`]'s policy record rather than inside any one `src/ops/*.rs`, so
//! every scope operation agrees on one copy of the record, the id minting and the
//! `scopeIdentifier` resolution.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::ops::types::ScopeConfiguration;

use super::{
    ComAmazonawsNetworksecuritymanagerPiccolocustomerapiserviceState as State, mint_entity_id,
    to_date_time,
};

/// `ServiceResourceType` naming a scope, for the `resourceType` member the modelled
/// `ConflictException` / `ResourceNotFoundException` / `ServiceQuotaExceededException`
/// shapes carry. One of `ServiceResourceType`'s modelled values.
pub const RESOURCE_TYPE_SCOPE: &str = "Scope";

/// The most scopes this emulator stores, after which `CreateScope` answers with the
/// modelled `ServiceQuotaExceededException` (HTTP 402) instead of growing [`State::scopes`]
/// for as long as a caller keeps asking. A CHOSEN emulator capacity, not a quota this
/// service documents: what matters is that the store has a ceiling and that reaching it is
/// refused through a modelled error rather than by exhausting the host.
pub const MAX_SCOPES: usize = 100;

/// One persisted scope: every member `GetScope` answers with, plus the bookkeeping the
/// lifecycle needs (`tags`, `created_at`).
#[derive(Debug, Clone)]
pub struct Scope {
    pub scope_id: String,
    pub scope_arn: String,
    pub scope_name: String,
    pub scope_description: Option<String>,
    /// The accounts-and-resources selection the caller submitted, stored as given so a
    /// later read answers with the same selection the create echoed.
    pub scope_configuration: ScopeConfiguration,
    /// `EntityStatus` — [`super::STATUS_ACTIVE`] or [`super::STATUS_DRAFT`], recomputed
    /// from `isPublished` on every create and update.
    pub status: String,
    /// Rendered as the modelled `EntityVersion` (`^[1-9][0-9]*$`) by
    /// [`Scope::version_string`]; starts at 1 and is bumped by every update.
    pub version: u32,
    /// `UpdateToken` — a UUID rotated on every mutation, so a caller holding a stale one
    /// is refused with `ConflictException`.
    pub update_token: String,
    pub is_snapshot: bool,
    pub has_published_version: bool,
    pub tags: BTreeMap<String, String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

impl Scope {
    /// The modelled `EntityVersion` rendering of [`Scope::version`].
    pub fn version_string(&self) -> String {
        self.version.to_string()
    }

    /// `updatedAt` as the modelled `DateTimestamp`.
    pub fn updated_at_timestamp(&self) -> ::aws_smithy_types::DateTime {
        to_date_time(self.updated_at)
    }
}

impl State {
    /// A fresh `ScopeId`, minted exactly as a `PolicyId` is (see [`mint_entity_id`]): it
    /// satisfies the modelled `^[a-z0-9]{1,64}$` and cannot collide with an id this store
    /// already handed out.
    pub fn mint_scope_id(&mut self) -> String {
        mint_entity_id(&mut self.next_scope_seq)
    }

    /// The `scopeId` a `scopeIdentifier` names — the scope's ARN, as the `@references`
    /// binding on `scopeArn` documents it, or the bare id — or `None` when no stored scope
    /// answers to it.
    pub fn resolve_scope_id(&self, identifier: &str) -> Option<String> {
        if self.scopes.contains_key(identifier) {
            return Some(identifier.to_string());
        }
        self.scopes
            .values()
            .find(|scope| scope.scope_arn == identifier)
            .map(|scope| scope.scope_id.clone())
    }

    /// The scope a `scopeIdentifier` names.
    pub fn scope_by_identifier(&self, identifier: &str) -> Option<&Scope> {
        let id = self.resolve_scope_id(identifier)?;
        self.scopes.get(&id)
    }
}
