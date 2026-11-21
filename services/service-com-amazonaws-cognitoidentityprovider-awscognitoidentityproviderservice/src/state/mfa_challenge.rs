// SPDX-License-Identifier: CC0-1.0

//! The SECOND factor of a sign-in: which challenge a user that has passed its
//! first factor still owes, what answers that challenge, and what satisfying it
//! leaves behind.
//!
//! One concern of [`super`]'s store, in its own file: the records and helpers
//! here are reached as `crate::state::…` because the parent re-exports them, so
//! every sign-in and every challenge response asks the SAME rule.

use super::*;

/// The `ChallengeNameType` value a user presents a registered TOTP
/// authenticator's one-time password through.
pub const SOFTWARE_TOKEN_MFA_CHALLENGE: &str = "SOFTWARE_TOKEN_MFA";

/// The `ChallengeNameType` value a user presents a text-messaged one-time
/// password through.
pub const SMS_MFA_CHALLENGE: &str = "SMS_MFA";

/// The `ChallengeNameType` value a user presents an emailed one-time password
/// through.
pub const EMAIL_OTP_CHALLENGE: &str = "EMAIL_OTP";

/// The `ChallengeNameType` value that asks a user holding SEVERAL activated
/// factors which of them it wants to present:
/// `"ChallengeName": "SELECT_MFA_TYPE", "ChallengeResponses": {"USERNAME":
/// "[username]", "ANSWER": "[SMS_MFA|EMAIL_MFA|SOFTWARE_TOKEN_MFA]"}`
/// (`ChallengeResponses` in this service's model).
pub const SELECT_MFA_TYPE_CHALLENGE: &str = "SELECT_MFA_TYPE";

/// The `ChallengeNameType` value a user that has to REGISTER a factor before it
/// can sign in is answered with.
pub const MFA_SETUP_CHALLENGE: &str = "MFA_SETUP";

/// The `ChallengeResponses` entry a `SELECT_MFA_TYPE` response names the factor
/// it chose in, and a `CUSTOM_CHALLENGE` response its answer (this service's
/// model).
pub const ANSWER_RESPONSE: &str = "ANSWER";

/// The `ChallengeParameters` entry of a `SELECT_MFA_TYPE` challenge that lists
/// the activated factors the caller may choose between, as the
/// [`MFAS_CAN_SETUP`](super) entry of an `MFA_SETUP` challenge lists the ones it
/// may set up.
const MFAS_CAN_CHOOSE: &str = "MFAS_CAN_CHOOSE";

/// The `ChallengeParameters` entry reporting the medium a challenge's code
/// travelled over, and the one reporting the masked address it was sent to.
pub const CODE_DELIVERY_DELIVERY_MEDIUM: &str = "CODE_DELIVERY_DELIVERY_MEDIUM";
/// See [`CODE_DELIVERY_DELIVERY_MEDIUM`].
pub const CODE_DELIVERY_DESTINATION: &str = "CODE_DELIVERY_DESTINATION";

/// The `UserMFASettingList` settings, in the order that member documents its
/// values in: "the possible values in this list are `SMS_MFA`, `EMAIL_OTP`, and
/// `SOFTWARE_TOKEN_MFA`" (this service's model). The order a listing of several
/// reports them in, so a caller reads the same order whichever user it asks
/// about.
pub const MFA_SETTING_ORDER: [&str; 3] = [
    SMS_MFA_SETTING,
    EMAIL_OTP_MFA_SETTING,
    SOFTWARE_TOKEN_MFA_SETTING,
];

/// The `ChallengeNameType` value the `UserMFASettingList` setting `setting` is
/// PRESENTED through, or `None` for a value that names no MFA factor.
///
/// Each of the three activated settings has a challenge of its own in
/// `ChallengeNameType` under the same name (this service's model), which is what
/// a sign-in answers with when that factor is the one to present.
#[must_use]
pub fn mfa_challenge_of(setting: &str) -> Option<&'static str> {
    match setting {
        SMS_MFA_SETTING => Some(SMS_MFA_CHALLENGE),
        EMAIL_OTP_MFA_SETTING => Some(EMAIL_OTP_CHALLENGE),
        SOFTWARE_TOKEN_MFA_SETTING => Some(SOFTWARE_TOKEN_MFA_CHALLENGE),
        _ => None,
    }
}

/// The `UserMFASettingList` setting a `SELECT_MFA_TYPE` response's `ANSWER`
/// names, or `None` for a value that names no factor.
///
/// The documented answers are `[SMS_MFA|EMAIL_MFA|SOFTWARE_TOKEN_MFA]`
/// (`ChallengeResponses` in this service's model), so the email factor is
/// accepted under the name that response documents beside the name the
/// `UserMFASettingList` reports it under.
#[must_use]
pub fn mfa_setting_chosen(answer: &str) -> Option<&'static str> {
    match answer {
        SMS_MFA_SETTING => Some(SMS_MFA_SETTING),
        EMAIL_OTP_MFA_SETTING | EMAIL_MFA_ANSWER => Some(EMAIL_OTP_MFA_SETTING),
        SOFTWARE_TOKEN_MFA_SETTING => Some(SOFTWARE_TOKEN_MFA_SETTING),
        _ => None,
    }
}

/// The name the `SELECT_MFA_TYPE` response documents the email factor under
/// (see [`mfa_setting_chosen`]).
const EMAIL_MFA_ANSWER: &str = "EMAIL_MFA";

/// The `ChallengeResponses` entry an `MFA_SETUP` response names the SETUP it completes in:
/// "the session ID from `VerifySoftwareToken`" (`ChallengeResponses` in this service's
/// model), which is the one entry that challenge asks for beside the `USERNAME` every
/// challenge requires.
pub const SETUP_SESSION_RESPONSE: &str = "SESSION";

/// Whether a response to an `MFA_SETUP` challenge CARRIES the setup it completes: the
/// session a `VerifySoftwareToken` handed out, which "satisfies an `MFA_SETUP` challenge"
/// (`VerifySoftwareToken.Session` in this service's model).
///
/// Two ways a response carries it, both the model's: the request's own `Session` IS that
/// session — the one the verification handed out, which `presented` is — or the response
/// names it in its [`SETUP_SESSION_RESPONSE`] entry (`named`). A response that carries
/// neither names no setup at all, which is a parameter it did not supply rather than
/// anything about the user's stored factors.
///
/// Decided from the SESSION and the response map alone, so it can be answered before any
/// user or factor is read.
#[must_use]
pub fn mfa_setup_session_presented(presented: &AuthSession, named: Option<&String>) -> bool {
    presented.issued_by == SessionIssuer::SoftwareTokenVerification
        || named.is_some_and(|session| !session.is_empty())
}

/// The `ChallengeResponses` entry the response to the MFA challenge
/// `challenge_name` carries the presented code in, or `None` for a challenge
/// that is answered with something other than a code.
///
/// `ChallengeResponses` documents one entry per challenge:
/// `{"SOFTWARE_TOKEN_MFA_CODE": [authenticator_code]}`, `{"SMS_MFA_CODE":
/// "[code]"}` and `{"EMAIL_OTP_CODE": "[code]"}` (this service's model), each
/// beside the `USERNAME` every challenge requires.
#[must_use]
pub fn mfa_code_response_key(challenge_name: &str) -> Option<&'static str> {
    match challenge_name {
        SOFTWARE_TOKEN_MFA_CHALLENGE => Some("SOFTWARE_TOKEN_MFA_CODE"),
        SMS_MFA_CHALLENGE => Some("SMS_MFA_CODE"),
        EMAIL_OTP_CHALLENGE => Some("EMAIL_OTP_CODE"),
        _ => None,
    }
}

/// What a sign-in that has already passed its FIRST factor still owes before it
/// can complete ([`UserPool::second_factor`]).
#[derive(Debug, Clone)]
pub enum SecondFactor {
    /// Nothing: no factor stands between this sign-in and its tokens.
    Complete,
    /// The pool requires a factor this user has none of, so the sign-in is
    /// answered with the challenge to SET one up, naming the methods the pool
    /// offers for it.
    Setup { settings_can_setup: Vec<String> },
    /// The one activated factor to present, as the challenge that presents it.
    Present(MfaChallenge),
    /// Several activated factors and no preference among them, so the caller
    /// chooses which to present.
    Select { settings: Vec<String> },
}

/// One started MFA challenge: the `ChallengeNameType` it is outstanding under,
/// and the one-time password it DELIVERED, for a factor that is delivered as a
/// message. A factor the user reads a code off itself delivers nothing.
#[derive(Debug, Clone)]
pub struct MfaChallenge {
    pub challenge_name: String,
    pub code: Option<VerificationCode>,
}

impl User {
    /// The `UserMFASettingList` settings this user has activated AND still holds
    /// the delivery configuration of, in [`MFA_SETTING_ORDER`].
    ///
    /// `UserMFASettingList` reports "the MFA options that are activated for the
    /// user" (this service's model), and each factor reaches the user through
    /// something it has to hold ([`User::holds_mfa_delivery_config`]) — a
    /// verified number or address, or a registered authenticator — so a setting
    /// whose delivery configuration is gone is no factor this user can present.
    pub fn presentable_mfa_settings(&self) -> Vec<String> {
        MFA_SETTING_ORDER
            .iter()
            .filter(|setting| {
                self.mfa_setting_list.iter().any(|held| held == *setting)
                    && self.holds_mfa_delivery_config(setting)
            })
            .map(|setting| (*setting).to_string())
            .collect()
    }
}

impl UserPool {
    /// What the sign-in of `user` still owes after its first factor, for a
    /// request that presented `presented_device_key`, as of `now`.
    ///
    /// A factor the user has ACTIVATED is a factor the sign-in presents: the
    /// preferred one where the user has expressed a preference — "only one
    /// factor can be set as preferred" (`SetUserMFAPreference` in this service's
    /// model) — the single activated one where it has not, and otherwise the
    /// choice between them. A user that has activated none owes the setup
    /// challenge where the POOL requires a factor from every sign-in
    /// ([`UserPool::requires_mfa`]) and owes nothing where it does not.
    ///
    /// A sign-in from a device the user has confirmed and REMEMBERS presents no
    /// second factor at all: `AuthParameters` is where a caller "add[s]
    /// `DEVICE_KEY` if you want to bypass multi-factor authentication with a
    /// remembered device" (this service's model), so a key naming such a device
    /// of this user is what bypasses it — and a key naming any other device,
    /// one this user does not hold or one it no longer remembers, bypasses
    /// nothing.
    pub fn second_factor(
        &self,
        user: &User,
        presented_device_key: Option<&str>,
        now: SystemTime,
    ) -> SecondFactor {
        let activated = user.presentable_mfa_settings();
        if activated.is_empty() {
            return if self.requires_mfa() {
                SecondFactor::Setup {
                    settings_can_setup: self.mfa_settings_available_for_setup(user),
                }
            } else {
                SecondFactor::Complete
            };
        }
        if presented_device_key
            .and_then(|device_key| user.remembered_device(device_key))
            .is_some()
        {
            return SecondFactor::Complete;
        }
        let preferred = user
            .preferred_mfa_setting
            .as_deref()
            .filter(|preferred| activated.iter().any(|held| held == preferred));
        let present = match (preferred, activated.as_slice()) {
            (Some(preferred), _) => preferred.to_string(),
            (None, [only]) => only.clone(),
            (None, _) => return SecondFactor::Select { settings: activated },
        };
        match self.start_mfa_challenge(user, &present, now) {
            Some(challenge) => SecondFactor::Present(challenge),
            // The setting is one the user holds the delivery configuration of,
            // so this is unreachable; a factor nothing can start is answered as
            // a factor to set up rather than as a sign-in nothing checked.
            None => SecondFactor::Setup {
                settings_can_setup: self.mfa_settings_available_for_setup(user),
            },
        }
    }

    /// Start the challenge that presents `setting` for `user` as of `now`, or
    /// `None` when the user holds no such factor.
    ///
    /// The two message factors are presented by DELIVERING a one-time password
    /// and asking for it back, so starting one of those challenges mints the
    /// code it is answered with; the software token is read off the user's own
    /// authenticator, so starting it delivers nothing.
    pub fn start_mfa_challenge(
        &self,
        user: &User,
        setting: &str,
        now: SystemTime,
    ) -> Option<MfaChallenge> {
        if !user.holds_mfa_delivery_config(setting) {
            return None;
        }
        let challenge_name = mfa_challenge_of(setting)?.to_string();
        let code = match setting {
            SMS_MFA_SETTING => Some(VerificationCode::issued(
                MFA_OPTION_ATTRIBUTE,
                &user.verified_destination_of(MFA_OPTION_ATTRIBUTE)?,
                now,
            )),
            EMAIL_OTP_MFA_SETTING => Some(VerificationCode::issued_email_otp(
                EMAIL_ATTRIBUTE,
                &user.verified_destination_of(EMAIL_ATTRIBUTE)?,
                now,
            )),
            _ => None,
        };
        Some(MfaChallenge {
            challenge_name,
            code,
        })
    }
}

/// The attribute an email message is addressed to, which is the destination the
/// email MFA factor is delivered to.
pub const EMAIL_ATTRIBUTE: &str = "email";

/// The `ChallengeParameters` of the challenge `factor` asks for, beside the
/// `ChallengeName` it is outstanding under and the code it delivered — or `None`
/// for a sign-in that owes no second factor at all.
///
/// Every sign-in operation reads it here, so `InitiateAuth` and
/// `AdminInitiateAuth` cannot report the same outstanding factor differently.
#[must_use]
pub fn second_factor_challenge(
    username: &str,
    factor: &SecondFactor,
) -> Option<(String, HashMap<String, String>, Option<VerificationCode>)> {
    match factor {
        SecondFactor::Complete => None,
        SecondFactor::Setup { settings_can_setup } => Some((
            MFA_SETUP_CHALLENGE.to_string(),
            mfa_setup_challenge_parameters(username, settings_can_setup),
            None,
        )),
        SecondFactor::Select { settings } => Some((
            SELECT_MFA_TYPE_CHALLENGE.to_string(),
            select_mfa_type_challenge_parameters(username, settings),
            None,
        )),
        SecondFactor::Present(challenge) => Some((
            challenge.challenge_name.clone(),
            mfa_challenge_parameters(username, challenge),
            challenge.code.clone(),
        )),
    }
}

/// The `ChallengeParameters` of a `SELECT_MFA_TYPE` challenge for `username`:
/// the user it is outstanding for, and the activated factors that user may
/// choose between, as the JSON array of `UserMFASettingList` values an
/// `MFA_SETUP` challenge reports the methods to set up in
/// ([`mfa_setup_challenge_parameters`]).
#[must_use]
pub fn select_mfa_type_challenge_parameters(
    username: &str,
    settings: &[String],
) -> HashMap<String, String> {
    challenge_parameters_listing(username, MFAS_CAN_CHOOSE, settings)
}

/// The `ChallengeParameters` of a challenge that names the user it is
/// outstanding for and lists a set of `UserMFASettingList` settings under
/// `entry`: the shape both the `MFA_SETUP` and the `SELECT_MFA_TYPE` challenge
/// report, which differ only in which entry carries the list, so the two cannot
/// drift apart.
#[must_use]
pub fn challenge_parameters_listing(
    username: &str,
    entry: &str,
    settings: &[String],
) -> HashMap<String, String> {
    let mut parameters = HashMap::new();
    parameters.insert(super::USER_ID_FOR_SRP.to_string(), username.to_string());
    parameters.insert(
        entry.to_string(),
        ::serde_json::Value::Array(
            settings
                .iter()
                .map(|setting| ::serde_json::Value::String(setting.clone()))
                .collect(),
        )
        .to_string(),
    );
    parameters
}

/// The `ChallengeParameters` of the MFA challenge `challenge`: the user it is
/// outstanding for, and — for a factor that was delivered as a message — the
/// medium it travelled over and the masked address it was sent to, as every
/// sent code of this API is reported.
#[must_use]
pub fn mfa_challenge_parameters(
    username: &str,
    challenge: &MfaChallenge,
) -> HashMap<String, String> {
    let mut parameters = HashMap::new();
    parameters.insert(super::USER_ID_FOR_SRP.to_string(), username.to_string());
    if let Some(delivery) = challenge
        .code
        .as_ref()
        .and_then(VerificationCode::delivery_details)
    {
        if let Some(medium) = delivery.delivery_medium {
            parameters.insert(CODE_DELIVERY_DELIVERY_MEDIUM.to_string(), medium);
        }
        if let Some(destination) = delivery.destination {
            parameters.insert(CODE_DELIVERY_DESTINATION.to_string(), destination);
        }
    }
    parameters
}

/// What the response to an outstanding MFA challenge presented
/// ([`ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState::mfa_challenge_answer`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MfaAnswer {
    /// The presented value is the code this factor is answered with, so the
    /// sign-in is complete.
    Accepted,
    /// A code IS there to present and the presented value is not it.
    CodeMismatch,
    /// The presented value is a one-time password this user's software token
    /// already authenticated a sign-in with, so it authenticates nothing more
    /// ([`User::software_token_authenticated_step`]).
    CodeAlreadyUsed,
    /// The factor the challenge asked for is not one the user holds any more —
    /// its authenticator or its delivered code is gone — so there is nothing to
    /// check the presented value against.
    FactorGone,
}

/// The message a `SELECT_MFA_TYPE` response whose `ANSWER` names no MFA factor
/// is refused with: the entry that is wrong and the factors it may name, with no
/// value from the request. Both challenge-response operations read it here, so
/// neither can word the refusal differently. The wording is this emulator's: no
/// recording of this refusal is available, so a later one can correct it.
#[must_use]
pub fn chosen_factor_refusal_message() -> String {
    format!(
        "Invalid {ANSWER_RESPONSE} for {SELECT_MFA_TYPE_CHALLENGE}. Valid values are {}",
        MFA_SETTING_ORDER.join(", ")
    )
}

/// The message the response to the MFA challenge `challenge_name` owes a
/// presented value that is not the code that factor is answered with.
///
/// Amazon Cognito words the refusal of a one-time password the user's own
/// authenticator did not emit as its own sentence, while a code the challenge
/// DELIVERED is refused with the wording of the authentication state the
/// response did not match ([`INVALID_CHALLENGE_STATE_MESSAGE`](super)) — the
/// sentence a response naming another challenge gets as well. Neither names a
/// value from the request.
#[must_use]
pub fn mfa_code_mismatch_message(challenge_name: &str) -> &'static str {
    if challenge_name == SOFTWARE_TOKEN_MFA_CHALLENGE {
        "Invalid code received for user"
    } else {
        super::INVALID_CHALLENGE_STATE_MESSAGE
    }
}

/// The message a one-time password this user's software token already
/// authenticated a sign-in with is refused with: one time step's password
/// authenticates once ([`User::software_token_authenticated_step`]), which is
/// what has been used up rather than anything about the request.
pub const SOFTWARE_TOKEN_CODE_USED_MESSAGE: &str =
    "Your software token has already been used once.";

/// What satisfying an `MFA_SETUP` challenge left behind
/// ([`ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState::complete_mfa_setup`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MfaSetupCompletion {
    /// The factor the user registered during the challenge, now ACTIVATED for
    /// it.
    Activated(String),
    /// The user registered no factor, so the challenge is unanswered.
    NothingRegistered,
}

impl ComAmazonawsCognitoidentityproviderAwscognitoidentityproviderserviceState {
    /// Check `presented` against the factor the challenge `session` is waiting
    /// on, as of `now`, and record what an ACCEPTED one spends.
    ///
    /// A refused answer writes nothing and leaves the challenge outstanding, so
    /// the caller can present the right code while the session lasts. The
    /// software token is checked against the key the user's authenticator was
    /// provisioned with ([`User::software_token_step_of`]) and the time step of
    /// an accepted code is recorded, because one step's password authenticates
    /// once ([`User::software_token_authenticated_step`]); a message factor is
    /// checked against the code its own challenge delivered, which is held on
    /// the session because the challenge is answerable exactly while that
    /// session lasts.
    pub fn answer_mfa_challenge(
        &mut self,
        session: &AuthSession,
        presented: &str,
        now: SystemTime,
    ) -> MfaAnswer {
        if session.challenge_name == SOFTWARE_TOKEN_MFA_CHALLENGE {
            let held = self
                .pool_mut(&session.user_pool_id)
                .and_then(|pool| pool.user_mut(&session.username))
                .filter(|user| user.software_token_registered);
            let Some(user) = held else {
                return MfaAnswer::FactorGone;
            };
            let Some(step) = user.software_token_step_of(presented, now) else {
                return MfaAnswer::CodeMismatch;
            };
            if user
                .software_token_authenticated_step
                .is_some_and(|authenticated| step <= authenticated)
            {
                return MfaAnswer::CodeAlreadyUsed;
            }
            user.software_token_authenticated_step = Some(step);
            return MfaAnswer::Accepted;
        }
        match session.code.as_ref() {
            Some(code) if code.code == presented => MfaAnswer::Accepted,
            Some(_) => MfaAnswer::CodeMismatch,
            None => MfaAnswer::FactorGone,
        }
    }

    /// Complete the `MFA_SETUP` challenge `session` is waiting on, as of `now`:
    /// ACTIVATE the factor the user registered while the challenge stood.
    ///
    /// The challenge is answered by registering a factor and then responding
    /// with "the session ID from `VerifySoftwareToken`"
    /// (`ChallengeResponses` in this service's model), so what satisfies it is a
    /// registered authenticator ([`User::software_token_registered`]) — and a
    /// factor the user set up to satisfy the pool's MFA requirement is activated
    /// for it from then on, which is what stops the next sign-in being asked to
    /// set the same factor up again. It becomes the user's preferred factor
    /// where it has expressed no other preference, since it is the factor it
    /// just chose to sign in with.
    pub fn complete_mfa_setup(
        &mut self,
        session: &AuthSession,
        now: SystemTime,
    ) -> MfaSetupCompletion {
        let held = self
            .pool_mut(&session.user_pool_id)
            .and_then(|pool| pool.user_mut(&session.username))
            .filter(|user| user.software_token_registered);
        let Some(user) = held else {
            return MfaSetupCompletion::NothingRegistered;
        };
        if !user
            .mfa_setting_list
            .iter()
            .any(|held| held == SOFTWARE_TOKEN_MFA_SETTING)
        {
            user.mfa_setting_list
                .push(SOFTWARE_TOKEN_MFA_SETTING.to_string());
            user.user_last_modified_date = now;
        }
        if user.preferred_mfa_setting.is_none() {
            user.preferred_mfa_setting = Some(SOFTWARE_TOKEN_MFA_SETTING.to_string());
            user.user_last_modified_date = now;
        }
        MfaSetupCompletion::Activated(SOFTWARE_TOKEN_MFA_SETTING.to_string())
    }
}
