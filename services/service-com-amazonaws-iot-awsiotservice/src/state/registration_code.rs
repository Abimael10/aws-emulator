// SPDX-License-Identifier: CC0-1.0

//! The account's CA-CERTIFICATE REGISTRATION CODE: the value a caller proves it holds a
//! certificate authority's private key with when it registers that authority.
//!
//! One value for the account and not a map, for the reason
//! [`super::AccountAuditConfiguration`] is one record: `GetRegistrationCode` and
//! `DeleteRegistrationCode` name no resource
//! (`smithy_query(query="operation",operation="GetRegistrationCode")` binds
//! `GET /registrationcode` with an empty request, and the delete binds `DELETE` on the same
//! URI), so there is one registration code per account.
//!
//! It is an `Option` and not a plain field, unlike that configuration: an account nobody has
//! asked for a registration code HAS none, and the read is documented as the call that
//! creates one — "IoT will create a registration code as part of this API call if the
//! registration code doesn't exist or has been deleted. If you already have a registration
//! code, this API call will return the same registration code" — so the absent state is
//! what the first read turns into a code, and what the delete puts the account back into.
//!
//! The value is DRAWN AT RANDOM and then persisted rather than derived from the account:
//! the same account that deletes its code and asks for one again is handed a code it did not
//! have before, which is what "has been deleted" distinguishes from "you already have one".
//! Deriving it from the account or the region would hand back the deleted value instead.
//!
//! How a delete of a code the account does NOT hold is refused is worded with the rest of
//! this service's not-found sentences
//! ([`super::MISSING_REGISTRATION_CODE_MESSAGE`]), so this family's miss is stated in the
//! one place every other family's is.

use super::ComAmazonawsIotAwsiotserviceState;

/// How many characters a registration code carries.
///
/// `RegistrationCode` is modelled `length 64..64, pattern ^(0x)?[a-fA-F0-9]+$`
/// (`smithy_query(query="operation",operation="GetRegistrationCode")`): a code is exactly
/// this many hexadecimal digits, so one minted here is the 32 random bytes that encode to
/// them.
pub const REGISTRATION_CODE_LENGTH: usize = 64;

/// A fresh registration code: [`REGISTRATION_CODE_LENGTH`] hexadecimal digits, encoded from
/// bytes drawn from the system random source — the same source certificate material is
/// drawn from ([`super::certificates`]).
///
/// Each byte encodes to two digits, so half as many bytes as the code has characters fills
/// it exactly and every digit is uniform: no value has to be dropped or folded, unlike an
/// id over an alphabet a byte does not divide by ([`super::audit_tasks`]). `None` when the
/// random source cannot be read at all — a code is never completed from anything less,
/// which is the answer certificate material gives too.
pub fn fresh_registration_code() -> Option<String> {
    let mut bytes = [0u8; REGISTRATION_CODE_LENGTH / 2];
    ::aws_lc_rs::rand::fill(&mut bytes).ok()?;
    Some(::hex::encode(bytes))
}

impl ComAmazonawsIotAwsiotserviceState {
    /// The account's registration code, MINTING and persisting one when it holds none.
    ///
    /// The one write `GetRegistrationCode` makes, stated here rather than in the handler so
    /// that the read and the delete agree about what holding a code means. "IoT will create
    /// a registration code as part of this API call if the registration code doesn't exist
    /// or has been deleted. If you already have a registration code, this API call will
    /// return the same registration code": a code already held is answered unchanged, so
    /// repeating the call hands back one value and persists nothing new.
    ///
    /// `None` only when no code could be drawn at all ([`fresh_registration_code`]), in
    /// which case nothing is written and the account still holds no code.
    pub fn registration_code_or_create(&mut self) -> Option<String> {
        if let Some(held) = &self.registration_code {
            return Some(held.clone());
        }
        let minted = fresh_registration_code()?;
        self.registration_code = Some(minted.clone());
        Some(minted)
    }

    /// Delete the account's registration code, answering whether there was one to delete.
    ///
    /// `false` is the account that holds none: the delete models
    /// `ResourceNotFoundException` while the read models no not-found error at all, and the
    /// account's registration code is the only resource either of them addresses, so that
    /// is the condition the refusal is about. After a code IS deleted the account holds
    /// none again, which is the state the next read mints a new code from.
    pub fn delete_registration_code(&mut self) -> bool {
        self.registration_code.take().is_some()
    }
}
