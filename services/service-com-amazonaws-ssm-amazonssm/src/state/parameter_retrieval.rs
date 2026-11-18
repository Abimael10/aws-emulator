// SPDX-License-Identifier: CC0-1.0

//! Serving the RETRIEVAL of a parameter this service is asked for by another AWS service, on
//! a ROLE's behalf.
//!
//! A compute service that exposes a parameter's value to the workload it runs does not sign a
//! request of its own: it retrieves the value AS a role of the caller's account — an Amazon
//! ECS task execution role, say — before the workload exists. That retrieval is still this
//! service's to answer, because every question it turns on is a fact about this service's own
//! resource and the key its value is sealed under:
//!
//! 1. **may this principal read it** — `ssm:GetParameters` ("Get information about one or
//!    more parameters by specifying multiple parameter names", `GetParameters`) on the
//!    parameter the reference names, decided over the documents in force on the role and the
//!    parameter's own resource-based policy, by the same shared evaluation this service's
//!    signed requests go through ([`super::identity_authorization`]);
//! 2. **is there a value to read** — this endpoint holds a parameter of that name, the
//!    version the reference's selector names is one it retains, and a `SecureString`'s key
//!    can run the decrypt the read needs. A batch read REPORTS a name it produces no value
//!    for among its `InvalidParameters` rather than refusing the call, which is why that
//!    answer is a refusal of its own class here ([`RetrievalRefusal::Unresolved`]) and the
//!    retrieving service words it the way its own client words such an answer;
//! 3. **may this principal open it** — a `SecureString`'s value is sealed under a KMS key and
//!    read with decryption, so that key's policy together with the documents in force on the
//!    role decide the retrieval exactly as they decide a caller's `GetParameter` with
//!    `WithDecryption` ([`super::secure_string::may_open_as_role`]), and the key's own key
//!    state decides it after that. A `String` or `StringList` is not encrypted at all, so no
//!    key decides it.
//!
//! The VALUE is not answered here: a caller that may read the parameter reads it through
//! `GetParameter`, which selects the version and opens it under that key. Nothing here opens
//! a value, so no plaintext passes through this module.
//!
//! Nothing is withheld on evidence this emulator does not have. An engine carrying no store
//! of this service, and a role whose documents the IAM store does not hold, each show nothing
//! about the retrieval, so it proceeds — the same rule every other gate over a role's
//! documents in this tree keeps. A retrieval made with no principal at all is not decided
//! against any policy either: a caller that names no role states nothing for a policy to be
//! read against, though the KEY STATE still decides it, because KMS runs no cryptographic
//! operation under an unusable key for anybody.
//!
//! A READ throughout: the decision consults the IAM store, KMS's and this one and writes to
//! none, so deciding a retrieval changes nothing a later caller of any of them sees.
//!
//! LOCK ORDER: this service's guard is taken and released before the IAM and KMS stores are
//! read, and each guard hands back owned values — the contract that keeps a lock cycle
//! between the stores unreachable. A caller reaches this with no lock of its own held.

use super::*;

/// The action a retrieval of a parameter's value takes, as IAM publishes it ("Actions defined
/// by AWS Systems Manager", IAM Service Authorization Reference): the operation the agent of
/// a compute service reads a batch of parameters through.
pub const GET_PARAMETERS_ACTION: &str = "GetParameters";

/// The principal a retrieval is made as.
#[derive(Debug, Clone, Copy)]
pub struct RetrievalPrincipal<'a> {
    /// The IAM ROLE whose identity-based documents are in force on the retrieval, and which a
    /// resource-based policy grants the read by naming.
    pub role_arn: &'a str,
    /// What a refusal NAMES as the principal: the ARN of the SESSION the retrieving service
    /// assumed that role in, which is what AWS reports for a call made under one. `None`
    /// names the role itself, for a caller that holds no session of its own.
    pub session_arn: Option<&'a str>,
}

/// Why this service did not serve a retrieval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetrievalRefusal {
    /// No policy in force on the principal permits the read: the action that was withheld,
    /// and the sentence AWS refuses it with.
    NotAuthorized {
        /// The fully qualified IAM action (`ssm:<OperationName>`) that was withheld.
        action: String,
        /// The authorization refusal itself.
        message: String,
    },
    /// This endpoint produces no value for the reference at all — nothing of that name here,
    /// a selector the parameter does not carry, or a `SecureString` whose key cannot run the
    /// decrypt the read needs. Carries the name AS THE REFERENCE WROTE IT, which is what a
    /// batch read reports among its `InvalidParameters`.
    Unresolved {
        /// The name the retrieval asked with.
        name: String,
    },
    /// The value is a `SecureString` whose key does not permit this principal the decrypt,
    /// carrying the sentence KMS words for the refusal — the `AccessDeniedException` a read
    /// with decryption is answered with.
    NotAuthorizedByKey {
        /// The refusal KMS answered.
        message: String,
    },
}

/// Serve the retrieval of the parameter `name` names in `region`, made on behalf of
/// `principal`.
///
/// `name` is a reference as the retrieving service's caller wrote it — a parameter name or a
/// full ARN, either optionally carrying a `:selector` — read through this service's own
/// reading of one ([`super::parse_parameter_query`]).
///
/// `Ok(())` for a retrieval this service would serve, and the refusal otherwise, which the
/// retrieving service reports in its own vocabulary. The VALUE is not answered here: the
/// caller reads it through `GetParameter`, so what this answers is whether that read would be
/// served at all, which is what decides whether the workload the value is for can start.
pub fn serve_parameter_retrieval<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    name: &str,
    principal: Option<RetrievalPrincipal<'_>>,
) -> Result<(), RetrievalRefusal> {
    let unresolved = || RetrievalRefusal::Unresolved {
        name: name.to_string(),
    };
    // A reference this service's own reading resolves to no parameter query, and one naming
    // another account's or another Region's endpoint, names nothing reachable here.
    let query = super::parse_parameter_query(name).ok_or_else(unresolved)?;
    if query.selector == Some("") || !query.names_this_endpoint(region) {
        return Err(unresolved());
    }
    // The authorization half comes FIRST, the order AWS decides a signed call in: a principal
    // holding no grant is refused rather than told whether the account holds the parameter.
    if let Some(principal) = principal {
        if let Some(message) = identity_authorization::parameter_refusal_as_role(
            engine,
            principal.role_arn,
            principal.session_arn,
            GET_PARAMETERS_ACTION,
            region,
            query.name,
        ) {
            let action = identity_authorization::action_for(GET_PARAMETERS_ACTION);
            // The refusal is the security-relevant event of this retrieval, so it is
            // attributable in the emulator's audit log the way this service's own gate logs a
            // refused request: who the retrieval was made as, what it asked for, and the
            // outcome. Neither the policy documents nor any value is logged.
            ::tracing::info!(
                target: "emulator::audit",
                audit = true,
                operation = "ServeParameterRetrieval",
                outcome = "refused_access_denied",
                principal = %principal.session_arn.unwrap_or(principal.role_arn),
                action = %action,
                resource = %super::parameter_arn(region, query.name),
                "no policy in force permits this role to retrieve this parameter"
            );
            return Err(RetrievalRefusal::NotAuthorized { action, message });
        }
    }
    // What the version the reference selects needs OPENED, copied out under a guard taken and
    // released here: the key's decision is KMS's and is made with no store of this service
    // held. A parameter AWS itself publishes is resolved beside the account's own, the way
    // every read of this service resolves one.
    let Some(state) = super::service_state(engine) else {
        // No store of this service to ask: nothing is known about the parameter, so the
        // retrieval is not refused on it.
        return Ok(());
    };
    let published = public_parameters::published_among(engine, region, &[query.name]);
    let sealed = {
        let store = state.lock();
        let record = store
            .parameter(region, query.name)
            .or_else(|| published.get(query.name))
            .ok_or_else(unresolved)?;
        let version = record.select(query.selector).ok_or_else(unresolved)?;
        // The agent of a compute service reads the value WITH DECRYPTION — a container is
        // given the parameter's value, not its ciphertext — so a `SecureString` version has
        // its key decide the retrieval and every other type has nothing to open.
        super::secure_string::value_to_open(Some(true), record, version)
    };
    let Some(sealed) = sealed else {
        return Ok(());
    };
    let role = principal.map(|principal| super::secure_string::OpeningRole {
        role_arn: principal.role_arn,
        session_arn: principal.session_arn,
    });
    super::secure_string::may_open_as_role(engine, region, role, &sealed).map_err(|refusal| {
        match refusal {
            super::secure_string::OpenRefusal::Denied(message) => {
                RetrievalRefusal::NotAuthorizedByKey { message }
            }
            // A value the KEY cannot open is a value this read produces nothing for, which a
            // batch read reports among its `InvalidParameters` rather than refusing the call
            // with. The key's own account of it is not part of that answer, so it is TRACED
            // for an operator rather than carried: the retrieving service would otherwise
            // report a sentence AWS does not give it.
            super::secure_string::OpenRefusal::Key(message) => {
                ::tracing::info!(
                    parameter = %sealed.parameter_arn,
                    key_id = %sealed.key_id,
                    refusal = %message,
                    "the key a parameter's value is sealed under cannot open it, so the \
                     retrieval produces no value for it"
                );
                RetrievalRefusal::Unresolved {
                    name: name.to_string(),
                }
            }
        }
    })
}
