// SPDX-License-Identifier: CC0-1.0

//! The account's DEFAULT AUTHORIZER: which of its custom authorizers a connection that
//! names none is authorized by, and what refuses a write of it.
//!
//! `SetDefaultAuthorizer` "sets the default authorizer. This will be used if a websocket
//! connection is made without specifying an authorizer", `DescribeDefaultAuthorizer`
//! "describes the default authorizer" and `ClearDefaultAuthorizer` "clears the default
//! authorizer" — none of the three names a resource beyond the single authorizer the
//! account has elected, so the account holds ONE default at a time, recorded as the NAME
//! of the authorizer holding it
//! ([`super::ComAmazonawsIotAwsiotserviceState::default_authorizer_name`]) and resolved
//! against the authorizer store on every read. Recording the name rather than a copy of
//! the record is what makes `DescribeDefaultAuthorizer` report the same
//! `AuthorizerDescription` `DescribeAuthorizer` does, with every later `UpdateAuthorizer`
//! of that authorizer already in it.
//!
//! The elected authorizer is an ATTACHMENT of the account to that record, which is what
//! the three refusals here are about:
//!
//! * an election of the authorizer that ALREADY holds the default is the modelled
//!   `ResourceAlreadyExistsException` (HTTP 409,
//!   [`duplicate_default_authorizer_message`]): the election it asks for is one the
//!   account already has, so it is a duplicate rather than a change. Electing a DIFFERENT
//!   authorizer is a change and is recorded, because the account's default is
//!   single-valued: the new election replaces the one in force;
//! * a read while the account holds none is the modelled `ResourceNotFoundException`
//!   (HTTP 404, [`MISSING_DEFAULT_AUTHORIZER_MESSAGE`]) rather than a 200 describing
//!   nothing, and a CLEAR while it holds none is refused the same way: the election is the
//!   resource both of them address;
//! * the delete of the authorizer that HOLDS the default is the modelled
//!   `DeleteConflictException` (HTTP 409, [`default_authorizer_deletion_message`]) — the
//!   record is attached to the account, so it is not torn out from under the connections
//!   it authorizes, and clearing the default is how a caller frees it. That refusal comes
//!   ahead of the status precondition the authorizer family keeps
//!   ([`super::undeletable_authorizer_message`]), because an authorizer is elected while
//!   it is in service and the attachment, not the status, is what the caller has to undo
//!   first.

/// How this service refuses the election of the authorizer that already holds the
/// account's default.
///
/// The fixed text is the class's — the election the request asks for is a DUPLICATE of the
/// one the account already has configured — and the variable part is the name of that
/// authorizer, which is both the name the request addressed and the name this account's
/// state holds the election under.
pub fn duplicate_default_authorizer_message(authorizer_name: &str) -> String {
    format!(
        "Duplicate default authorizer {authorizer_name} already configured for this account"
    )
}

/// How this service states that the account has elected no default authorizer.
///
/// The authorizer family words a miss `Authorizer <name> not found`
/// ([`super::missing_authorizer_message`]); the default authorizer is not addressed by
/// name, so the sentence names the resource the request asked about and carries no
/// variable part at all.
pub const MISSING_DEFAULT_AUTHORIZER_MESSAGE: &str = "Default authorizer not found";

/// How this service refuses the delete of the authorizer that holds the account's
/// default.
///
/// The fixed text is the class's, and the variable part is the name the request addressed,
/// which is also the name the account's default is set to. The sentence names the way out,
/// as the status precondition's sentence does: change the default, then retry the delete.
pub fn default_authorizer_deletion_message(authorizer_name: &str) -> String {
    format!("Cannot delete default authorizer {authorizer_name}. Change default and retry delete.")
}

/// Why a write of the account's default authorizer was refused.
///
/// Each variant is one modelled error class, so the two writers of the default cannot word
/// the same refusal two ways: the handler maps the variant and this module words it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultAuthorizerRefusal {
    /// The request named an authorizer the account holds no record under: the modelled
    /// `ResourceNotFoundException` (HTTP 404), worded by the authorizer family.
    MissingAuthorizer,
    /// The request named the authorizer that already holds the account's default, with
    /// the ARN of its record: the modelled `ResourceAlreadyExistsException` (HTTP 409).
    AlreadyDefault {
        /// The authorizer the account's default is already set to.
        authorizer_name: String,
        /// That authorizer's ARN, as its create minted it.
        authorizer_arn: String,
    },
}

impl super::ComAmazonawsIotAwsiotserviceState {
    /// The authorizer record the account's default is set to, or `None` while the account
    /// has elected none.
    ///
    /// Resolved against the authorizer store on every call, so a reader of the default
    /// reports the record as it stands now — including every `UpdateAuthorizer` since the
    /// election — rather than a copy taken when it was elected.
    pub fn default_authorizer(&self) -> Option<&super::RegisteredAuthorizer> {
        let authorizer_name = self.default_authorizer_name.as_deref()?;
        self.authorizers.get(authorizer_name)
    }

    /// Whether `authorizer_name` is the authorizer holding the account's default — what a
    /// delete of that record is refused for.
    pub fn is_default_authorizer(&self, authorizer_name: &str) -> bool {
        self.default_authorizer()
            .is_some_and(|authorizer| authorizer.authorizer_name == authorizer_name)
    }

    /// Elect `authorizer_name` as the account's default authorizer and answer the record
    /// elected, or state why the election is refused.
    ///
    /// The name must be one the account holds an authorizer under — the default is an
    /// attachment to a record and not a name on its own — and must not be the authorizer
    /// that already holds the election, which is the duplicate this refuses. Electing
    /// another authorizer replaces the election in force, because the account's default is
    /// single-valued.
    pub fn set_default_authorizer(
        &mut self,
        authorizer_name: &str,
    ) -> Result<&super::RegisteredAuthorizer, DefaultAuthorizerRefusal> {
        if !self.authorizers.contains_key(authorizer_name) {
            return Err(DefaultAuthorizerRefusal::MissingAuthorizer);
        }
        if let Some(existing) = self.default_authorizer() {
            if existing.authorizer_name == authorizer_name {
                return Err(DefaultAuthorizerRefusal::AlreadyDefault {
                    authorizer_name: existing.authorizer_name.clone(),
                    authorizer_arn: existing.authorizer_arn.clone(),
                });
            }
        }
        self.default_authorizer_name = Some(authorizer_name.to_string());
        self.default_authorizer()
            .ok_or(DefaultAuthorizerRefusal::MissingAuthorizer)
    }

    /// Clear the account's default authorizer, answering the name it was set to, or `None`
    /// when the account had elected none.
    ///
    /// Clearing removes the ELECTION and never the authorizer: the record stays in the
    /// store, free to be elected again or deleted.
    pub fn clear_default_authorizer(&mut self) -> Option<String> {
        self.default_authorizer_name.take()
    }
}
