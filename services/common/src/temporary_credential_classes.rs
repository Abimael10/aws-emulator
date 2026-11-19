// SPDX-License-Identifier: CC0-1.0

//! The CLASS of a temporary credential: which kind of credential an access key id names,
//! for the services that decide a request signed with one.
//!
//! A temporary credential is minted by one service (STS, or a service that mints a session
//! on a caller's behalf) and presented to every other, and two of the rules AWS states
//! about such a credential are about its CLASS rather than about the identity it speaks
//! for or the documents in force on it:
//!
//! * what the global condition key `aws:PrincipalType` answers for a request it signs —
//!   [`PRINCIPAL_TYPE_ASSUMED_ROLE`] for a role session, [`PRINCIPAL_TYPE_FEDERATED_USER`]
//!   for a federated-user session, [`PRINCIPAL_TYPE_USER`] for an IAM user's own
//!   credential — which is what a statement conditioned on that key selects on; and
//! * whether a credential of that class may call IAM at all
//!   ([`TemporaryCredentialClass::may_call_iam`]).
//!
//! Neither is a document, so neither can travel with the session policies a mint passed,
//! and neither is a fact about the identity the credential speaks for. So the
//! classification lives here, in one place: the service that MINTS a credential classifies
//! it, the gate that decides a request signed with one answers the condition key from the
//! same classification, and the ban is stated once rather than per service.

/// The condition key naming the class of the principal that signed the request:
/// "`aws:PrincipalType` — the type of principal (user, account, service, or other) that is
/// making the request" (AWS General Reference, *Global condition context keys*).
pub const PRINCIPAL_TYPE_CONDITION_KEY: &str = "aws:PrincipalType";

/// What [`PRINCIPAL_TYPE_CONDITION_KEY`] answers for a credential vended for an assumed
/// ROLE.
pub const PRINCIPAL_TYPE_ASSUMED_ROLE: &str = "AssumedRole";

/// What [`PRINCIPAL_TYPE_CONDITION_KEY`] answers for a credential vended for a FEDERATED
/// user (`sts:GetFederationToken`).
pub const PRINCIPAL_TYPE_FEDERATED_USER: &str = "FederatedUser";

/// What [`PRINCIPAL_TYPE_CONDITION_KEY`] answers for an IAM user's own credential: its
/// long-term access key, and the session `sts:GetSessionToken` vends for that same user,
/// whose credentials "have the same permissions as the IAM user" that asked for them.
pub const PRINCIPAL_TYPE_USER: &str = "User";

/// Which class of credential an access key id names.
///
/// The three classes STS vends, which are not interchangeable: each answers
/// [`PRINCIPAL_TYPE_CONDITION_KEY`] with its own value, and each is documented with the
/// calls it may not make ([`TemporaryCredentialClass::may_call_iam`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporaryCredentialClass {
    /// A session of an IAM role, whose permissions are the role's.
    AssumedRole,
    /// A federated-user session, whose permissions are the intersection of the minting IAM
    /// user's documents and the session policies the mint passed.
    FederatedUser,
    /// A session vended for the IAM user that asked for it (`sts:GetSessionToken`).
    SessionToken,
}

impl TemporaryCredentialClass {
    /// What [`PRINCIPAL_TYPE_CONDITION_KEY`] answers for a request a credential of this
    /// class signs.
    #[must_use]
    pub fn principal_type(self) -> &'static str {
        match self {
            Self::AssumedRole => PRINCIPAL_TYPE_ASSUMED_ROLE,
            Self::FederatedUser => PRINCIPAL_TYPE_FEDERATED_USER,
            Self::SessionToken => PRINCIPAL_TYPE_USER,
        }
    }

    /// Whether a request signed with a credential of this class may call IAM at all.
    ///
    /// Two of the classes are documented with that ban, and they differ in it:
    ///
    /// * a federated-user session cannot call IAM under any circumstances — "you cannot
    ///   call any IAM operations using the Amazon Web Services CLI or the Amazon Web
    ///   Services API" (`GetFederationToken`, the STS model); and
    /// * a `GetSessionToken` session cannot "unless MFA authentication information is
    ///   included in the request" (`GetSessionToken`), which is `mfa_authenticated`: the
    ///   mint that vended the credential answered a challenge of the user's own device.
    ///
    /// A role session is under no such ban: it is judged on the role's own documents.
    #[must_use]
    pub fn may_call_iam(self, mfa_authenticated: bool) -> bool {
        match self {
            Self::AssumedRole => true,
            Self::FederatedUser => false,
            Self::SessionToken => mfa_authenticated,
        }
    }
}

/// What [`PRINCIPAL_TYPE_CONDITION_KEY`] answers for a request signed by a credential of
/// `class` — and [`PRINCIPAL_TYPE_USER`] for `None`, a credential of no class at all: an IAM
/// user's own long-term access key, which is a `User` exactly as the session vended for that
/// user is.
///
/// The ONE rendering of the key, so the service that MINTS a credential, the gate that
/// decides a request signed with it and the trust policy a further mint is judged against
/// cannot name one class differently.
#[must_use]
pub fn principal_type(class: Option<TemporaryCredentialClass>) -> &'static str {
    match class {
        Some(class) => class.principal_type(),
        None => PRINCIPAL_TYPE_USER,
    }
}

/// The condition context entry [`PRINCIPAL_TYPE_CONDITION_KEY`] contributes for a request
/// signed by a credential whose class renders as `principal_type` ([`principal_type`]).
///
/// A gate that resolved the signing credential can answer the key for every request, so it
/// answers it from here and declares the key published; one that resolved no credential at
/// all — an expired or withdrawn one, which speaks for nobody — answers nothing, exactly as
/// it answers no principal ARN.
#[must_use]
pub fn principal_type_conditions(principal_type: Option<&str>) -> Vec<(String, String)> {
    principal_type
        .map(|class| (PRINCIPAL_TYPE_CONDITION_KEY.to_string(), class.to_string()))
        .into_iter()
        .collect()
}


