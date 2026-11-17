// SPDX-License-Identifier: CC0-1.0

//! Answering the `NEW_PASSWORD_REQUIRED` challenge: the ONE place the password a
//! user chooses for that challenge, and the attributes the challenge asked for
//! alongside it, are checked and stored.
//!
//! The challenge is started by whichever operation began the sign-in
//! (`InitiateAuth` or `AdminInitiateAuth`) and answered by the matching response
//! operation (`RespondToAuthChallenge` or `AdminRespondToAuthChallenge`). Which
//! API answers it changes who is authorized to call and what the refusal is
//! worded as, not what the challenge requires — so the requirement lives here and
//! each operation only words the refusal it reports
//! ([`NewPasswordChallengeRefusal`]).

use super::*;

/// Why a `NEW_PASSWORD_REQUIRED` response does not answer the challenge
/// ([`answer_new_password_challenge`]). Each operation reports it as the modelled
/// error of its own, carrying [`NewPasswordChallengeRefusal::message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewPasswordChallengeRefusal {
    /// The response map carries no entry for the chosen password.
    NewPasswordMissing,
    /// The response map names no user, which every challenge response has to.
    UsernameMissing,
    /// The response does not belong to the challenge it presents: it names
    /// another user than the session was started for.
    SessionNotApplicable,
    /// The chosen password misses one of the pool's policy requirements, which
    /// the pool's own wording reports ([`UserPool::check_password`]).
    PasswordRejected(String),
    /// The response submits a value for an attribute the pool's schema does not
    /// declare.
    UnknownAttribute(String),
    /// The response submits a value outside a bound the pool's schema declares
    /// for its attribute, carrying the report that states a line per such value
    /// ([`declared_bound_report`]).
    AttributeOutsideDeclaredBound(String),
    /// A required attribute the challenge asked for is still without a value
    /// after the response's own entries are applied.
    RequiredAttributeUnset(String),
    /// The user the session was started for is gone, or the pool is.
    UserNotFound,
    /// That user is disabled, so no sign-in of it completes.
    UserDisabled,
    /// The chosen password is one this pool's history policy restricts the user
    /// from reusing.
    PasswordReused,
}

impl NewPasswordChallengeRefusal {
    /// What the refusal reports, which is the same sentence whichever operation
    /// carried the response: the challenge's requirements are the pool's, so a
    /// response failing one fails it identically for an administrator and for the
    /// user itself.
    pub fn message(&self) -> String {
        match self {
            Self::NewPasswordMissing => format!(
                "Invalid attributes given, new password should be given with key {NEW_PASSWORD}"
            ),
            Self::UsernameMissing => format!("Missing required parameter {CHALLENGE_USERNAME}"),
            Self::SessionNotApplicable => SESSION_NOT_FOR_THIS_REQUEST_MESSAGE.to_string(),
            Self::PasswordRejected(message) => message.clone(),
            Self::UnknownAttribute(name) => format!(
                "Invalid attributes given, {name} is not a valid attribute of this user pool"
            ),
            Self::AttributeOutsideDeclaredBound(report) => report.clone(),
            Self::RequiredAttributeUnset(name) => {
                format!("Invalid attributes given, {name} is missing")
            }
            Self::UserNotFound => USER_NOT_FOUND_MESSAGE.to_string(),
            Self::UserDisabled => "User is disabled.".to_string(),
            Self::PasswordReused => PASSWORD_REUSED_MESSAGE.to_string(),
        }
    }
}

/// The `ChallengeResponses` entry a `NEW_PASSWORD_REQUIRED` response carries the
/// chosen password in: `"ChallengeResponses": {"NEW_PASSWORD":
/// "[new_password]", "USERNAME": "[username]"}` (`ChallengeResponses` in this
/// service's model).
pub const NEW_PASSWORD: &str = "NEW_PASSWORD";

/// The `ChallengeResponses` entry naming the user a response is for: "all
/// challenges require `USERNAME`" (`ChallengeResponses` in this service's model).
pub const CHALLENGE_USERNAME: &str = "USERNAME";

/// Answer the `NEW_PASSWORD_REQUIRED` challenge that `session_username`'s session
/// of the pool `user_pool_id` is waiting on, with the response map `responses`, as
/// of `now` — storing the chosen password and the attributes the response carried,
/// and answering the STORED username the sign-in completes for.
///
/// Everything the response would leave behind is computed from the stored user
/// FIRST, so a response that does not satisfy the challenge writes nothing at all;
/// a successful one leaves the user holding the password it chose, owing no further
/// change, and confirmed.
pub fn answer_new_password_challenge(
    store: &mut ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState,
    user_pool_id: &str,
    session_username: &str,
    responses: &HashMap<String, String>,
    now: SystemTime,
) -> Result<String, NewPasswordChallengeRefusal> {
    let new_password = responses
        .get(NEW_PASSWORD)
        .cloned()
        .ok_or(NewPasswordChallengeRefusal::NewPasswordMissing)?;
    // "All challenges require USERNAME" (`ChallengeResponses` in this service's
    // model), so a response that names no user answers none.
    if !responses.contains_key(CHALLENGE_USERNAME) {
        return Err(NewPasswordChallengeRefusal::UsernameMissing);
    }
    // `"userAttributes.[attribute_name]": "[attribute_value]"` for the attributes
    // the challenge asked for (`ChallengeResponses` in this service's model). The
    // challenge that ASKS for them names them under the same prefix, so both sides
    // read it from the one place.
    let requested: BTreeMap<String, String> = responses
        .iter()
        .filter_map(|(key, value)| {
            key.strip_prefix(USER_ATTRIBUTES_PREFIX)
                .map(|name| (name.to_string(), value.clone()))
        })
        .collect();

    let pool = store
        .pool_mut(user_pool_id)
        .ok_or(NewPasswordChallengeRefusal::UserNotFound)?;
    // A response that names a user is answered only for the user its own session
    // was started for.
    if let Some(named) = responses.get(CHALLENGE_USERNAME) {
        if pool.user_storage_key(named) != pool.user_storage_key(session_username) {
            return Err(NewPasswordChallengeRefusal::SessionNotApplicable);
        }
    }
    if let Some(message) = pool.check_password(&new_password, PasswordPolicyWording::AnswersChallenge)
    {
        return Err(NewPasswordChallengeRefusal::PasswordRejected(message));
    }
    let requested_names: Vec<String> = requested.keys().cloned().collect();
    if let Some(unknown) = pool.unknown_attribute(&requested_names) {
        return Err(NewPasswordChallengeRefusal::UnknownAttribute(unknown));
    }
    // Every value written has to be within the bounds the pool's schema declares
    // for its attribute, the same rule every other writer of a user attribute
    // applies ([`declared_bound_report`]).
    if let Some(report) = declared_bound_report(
        pool,
        requested
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    ) {
        return Err(NewPasswordChallengeRefusal::AttributeOutsideDeclaredBound(
            report,
        ));
    }

    let stored = pool
        .user(session_username)
        .ok_or(NewPasswordChallengeRefusal::UserNotFound)?;
    if !stored.enabled {
        return Err(NewPasswordChallengeRefusal::UserDisabled);
    }
    // The OUTSTANDING session is the state the server holds for this challenge: a
    // `Session` is "the session identifier that maintains the state of
    // authentication requests and challenge responses" (`RespondToAuthChallenge`
    // in this service's model), and answering it is what spends it — so a session
    // the store still holds answers its challenge whatever the stored user's
    // status has become since the challenge was raised, a user that already chose
    // a password among them. The only user refused here is one that cannot sign in
    // at all, which is the disabled one checked above.
    //
    // A password this pool's policy restricts the user from reusing is refused
    // before anything is written, as on every other path where the user itself
    // chooses the password it will sign in with.
    if pool.restricts_password_reuse(stored, &new_password) {
        return Err(NewPasswordChallengeRefusal::PasswordReused);
    }
    let username = stored.username.clone();
    let mut attributes = stored.attributes.clone();
    for (name, value) in requested {
        // "In a NEW_PASSWORD_REQUIRED challenge response, you can't modify a
        // required attribute that already has a value", and
        // `SchemaAttributeType.Mutable` decides whether any other stored value may
        // change at all — so an entry for either writes only the value the user
        // does not have yet.
        if attributes.contains_key(&name)
            && (pool.attribute_required(&name) || !pool.attribute_mutable(&name))
        {
            continue;
        }
        // An empty value is no value of the attribute, so it is stored for nobody
        // and takes a held value away ([`stored_text`]); a required attribute left
        // without one then leaves the challenge unanswered below.
        match stored_text(Some(value)) {
            Some(value) => attributes.insert(name, value),
            None => attributes.remove(&name),
        };
    }
    // The challenge asks for every required attribute the user has no value for,
    // so one still unset leaves it unanswered.
    if let Some(missing) = pool.unset_required_attributes(&attributes).first() {
        return Err(NewPasswordChallengeRefusal::RequiredAttributeUnset(
            missing.clone(),
        ));
    }

    let user = pool
        .user_mut(&username)
        .ok_or(NewPasswordChallengeRefusal::UserNotFound)?;
    user.attributes = attributes;
    // The challenge is answered, so the password the user signs in with from now
    // on is the one it just chose, and it owes no further change; the one it
    // replaces joins this user's password history.
    user.set_password(new_password, true, now);
    user.status = UserStatus::Confirmed;
    Ok(username)
}
