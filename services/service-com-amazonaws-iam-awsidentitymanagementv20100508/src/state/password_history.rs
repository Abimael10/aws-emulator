// SPDX-License-Identifier: CC0-1.0

//! What the account remembers of the console passwords a user NO LONGER has, and the rule
//! that remembering serves.
//!
//! The account's password policy bounds "the number of previous passwords that IAM users are
//! prevented from reusing" (`PasswordPolicy$PasswordReusePrevention`). A rule about PREVIOUS
//! passwords can only be decided by an account that still knows them, so a login profile
//! keeps the verifier of each password a write superseded ([`LoginProfileRecord::supersede`])
//! beside the verifier of the one in force, and the question "is this password one of the
//! last `n` this user had?" is answered from those ([`LoginProfileRecord::reuses_a_recent_password`]).
//!
//! What is kept is a [`PasswordVerifier`] and never a password: a superseded password is held
//! exactly as the one in force is, so the account can answer whether a password a later
//! request presents is one of them without keeping the secret, and nothing can report one
//! back. The list is bounded by the most any policy can ask for
//! ([`MOST_REMEMBERED_PASSWORDS`]), so a user changing its password without end does not grow
//! the store.

use super::{AccountPasswordPolicy, ComAmazonawsIamAwsidentitymanagementv20100508State,
    LoginProfileRecord, PasswordVerifier};

/// How many superseded passwords one login profile remembers.
///
/// `passwordReusePreventionType` (range 1..24) is the published bound on what a policy may
/// require, so 24 remembered passwords answer every policy the account can hold — the one in
/// force plus 23 superseded ones covers a window of 24 — and nothing beyond them can ever be
/// asked about. Older verifiers are dropped rather than kept for a question no policy can
/// pose.
pub const MOST_REMEMBERED_PASSWORDS: usize = 24;

/// The whole sentence AWS refuses a password change that returns to a password the reuse
/// window still covers with.
///
/// Fixed text: it interpolates nothing — not the user, not the window the policy set, and
/// certainly not the password — so the refusal names the rule and discloses nothing of the
/// account's credentials.
pub const PASSWORD_ALREADY_USED: &str = "Password cannot be the same as previously used password";

impl AccountPasswordPolicy {
    /// How many of a user's most recent passwords this policy bars it from choosing again,
    /// counting the one in force: the `PasswordReusePrevention` the account set, and `0` for
    /// a policy that set none, under which users "are not prevented from reusing previous
    /// passwords" (`UpdateAccountPasswordPolicy$PasswordReusePrevention`).
    ///
    /// The stored value is a `passwordReusePreventionType` (range 1..24), so a value outside
    /// that range is not a window this policy could have been written with: anything below
    /// the published minimum bars nothing — the position of the account that set no window at
    /// all — and anything above the published maximum is capped at what a profile remembers,
    /// so the rule enforced is one the model admits.
    pub fn barred_recent_passwords(&self) -> usize {
        self.password_reuse_prevention
            .map(|window| {
                usize::try_from(window)
                    .unwrap_or(0)
                    .min(MOST_REMEMBERED_PASSWORDS)
            })
            .unwrap_or(0)
    }
}

impl LoginProfileRecord {
    /// Replace the password in force with `replacement`, remembering the one it supersedes.
    ///
    /// Every write that REPLACES a console password goes through here — the administrative
    /// update and the user's own change — so the account's memory of a user's passwords is
    /// kept in one place and the reuse window is the same whichever write moved the password.
    pub fn supersede(&mut self, replacement: PasswordVerifier) {
        let retired = ::std::mem::replace(&mut self.password, replacement);
        self.superseded_passwords.insert(0, retired);
        self.superseded_passwords.truncate(MOST_REMEMBERED_PASSWORDS);
    }

    /// Whether `password` is one of the `barred` most recent passwords of this profile — the
    /// one in force among them, since a password a user still has is the first one it would
    /// be reusing.
    ///
    /// `barred` of `0` is the account that bars nothing, which no password can fall foul of.
    /// A READ: asking changes nothing a later caller sees.
    pub fn reuses_a_recent_password(&self, password: &str, barred: usize) -> bool {
        if barred == 0 {
            return false;
        }
        if self.password.verifies(password) {
            return true;
        }
        self.superseded_passwords
            .iter()
            .take(barred - 1)
            .any(|superseded| superseded.verifies(password))
    }
}

impl ComAmazonawsIamAwsidentitymanagementv20100508State {
    /// Whether the account's password policy bars the user `user_key` from returning to
    /// `password`, worded as AWS words that refusal ([`PASSWORD_ALREADY_USED`]), or `None`
    /// when it admits it.
    ///
    /// `None` for an account holding no policy and for one whose policy set no reuse window,
    /// which is the account this emulator starts out as, and `None` for a user with no login
    /// profile, which has no passwords to have reused.
    /// [`Self::password_policy_violation`](ComAmazonawsIamAwsidentitymanagementv20100508State::password_policy_violation)
    /// is the same question about the password's own CONTENT; this one is about the passwords
    /// the user had before it. A READ.
    pub fn password_reuse_violation(&self, user_key: &str, password: &str) -> Option<String> {
        let barred = self
            .account_password_policy
            .as_ref()
            .map_or(0, AccountPasswordPolicy::barred_recent_passwords);
        let profile = self.login_profiles.get(user_key)?;
        profile
            .reuses_a_recent_password(password, barred)
            .then(|| PASSWORD_ALREADY_USED.to_string())
    }
}
