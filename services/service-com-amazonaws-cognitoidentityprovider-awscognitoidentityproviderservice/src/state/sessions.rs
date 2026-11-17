// SPDX-License-Identifier: CC0-1.0

//! Which call of this service handed a stored challenge session out, and which
//! sessions an operation that takes one resolves.
//!
//! A `Session` is not one interchangeable credential: the model says, per
//! operation, which call the session it takes comes from — "the session ID from
//! an `AssociateSoftwareToken` request" (`VerifySoftwareToken.Session`) against
//! "the session ID from a successful sign-in"
//! (`AssociateSoftwareToken.Session`) — so a stored session records the call
//! that issued it ([`SessionIssuer`]) and each operation resolves only the
//! sessions its own member names
//! ([`super::ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState::software_token_caller`]).
//! A live session of a DIFFERENT exchange maintains state this call is not part
//! of, so it names no session of this request.

/// Which call issued a stored session
/// ([`super::AuthSession::issued_by`]).
///
/// Every session in one exchange descends from the sign-in that started it:
/// the sign-in hands the first one out, and each call of a setup the sign-in is
/// waiting on hands the next one out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIssuer {
    /// A sign-in, or a response to one of the challenges it is waiting on
    /// (`InitiateAuth`, `AdminInitiateAuth`, `RespondToAuthChallenge`,
    /// `AdminRespondToAuthChallenge`): the session that "maintains the state of
    /// authentication requests and challenge responses" (this service's model).
    SignIn,
    /// `AssociateSoftwareToken`: the session a started software-token setup
    /// hands out, which is the one "from an `AssociateSoftwareToken` request"
    /// that a verification of that setup takes
    /// (`VerifySoftwareToken.Session`).
    SoftwareTokenAssociation,
    /// `VerifySoftwareToken`: the session a completed software-token setup hands
    /// out, which "satisfies an `MFA_SETUP` challenge" and is supplied "in your
    /// challenge response" (`VerifySoftwareToken`'s output in this service's
    /// model) rather than to another verification.
    SoftwareTokenVerification,
}

/// The sessions a call whose `Session` is "the session ID from a successful
/// sign-in" (`AssociateSoftwareToken.Session` in this service's model)
/// resolves: every session of that sign-in's exchange, since each one is handed
/// out by a call of the sign-in the first one started and identifies the same
/// user.
pub const SESSIONS_OF_A_SIGN_IN: &[SessionIssuer] = &[
    SessionIssuer::SignIn,
    SessionIssuer::SoftwareTokenAssociation,
    SessionIssuer::SoftwareTokenVerification,
];

/// The sessions a call whose `Session` is "the session ID from an
/// `AssociateSoftwareToken` request" (`VerifySoftwareToken.Session` in this
/// service's model) resolves: only the session an association handed out, which
/// is what carries the private key the presented code is checked against.
pub const SESSIONS_OF_AN_ASSOCIATION: &[SessionIssuer] =
    &[SessionIssuer::SoftwareTokenAssociation];
