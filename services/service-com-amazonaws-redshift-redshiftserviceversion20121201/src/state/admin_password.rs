// SPDX-License-Identifier: CC0-1.0

//! The admin password a cluster write carries, and the content rules Amazon
//! Redshift checks it against.
//!
//! The password is a value of the request like any other, so it is checked
//! against the constraints the service publishes for it BEFORE the operation
//! touches the store — which is also why the refusal says which constraint was
//! broken and never quotes the password itself: the member is sensitive, and the
//! caller only needs the rule.
//!
//! The constraints come from `smithy_query(query="doc",
//! shape="CreateClusterMessage", member="MasterUserPassword")`, which publishes
//! all of them in one list; `ModifyClusterMessage$MasterUserPassword` publishes
//! the same list, so the one rule here serves every operation that sets an admin
//! password.

/// The input member every sentence below is about.
const MASTER_PASSWORD_MEMBER: &str = "MasterUserPassword";

/// How long an admin password may be, in characters, at the least and at the
/// most.
///
/// Source: `smithy_query(query="doc", shape="CreateClusterMessage",
/// member="MasterUserPassword")` — "Must be between 8 and 64 characters in
/// length."
pub const MASTER_PASSWORD_LENGTH_BOUNDS: (usize, usize) = (8, 64);

/// The characters an admin password may NOT carry, in the order the refusal
/// names them.
///
/// Source: `smithy_query(query="doc", shape="CreateClusterMessage",
/// member="MasterUserPassword")` — "Can be any printable ASCII character (ASCII
/// code 33-126) except `'` (single quote), `"` (double quote), `\`, `/`, or
/// `@`". The space is in the list because the refusal names it: it is below the
/// printable band ([`PRINTABLE_ASCII_BAND`]) and so is excluded twice over.
pub const MASTER_PASSWORD_EXCLUDED_CHARACTERS: [char; 6] =
    ['/', '@', '"', ' ', '\\', '\''];

/// The lowest and highest ASCII codes a printable character has.
///
/// Source: the same doc sentence — "any printable ASCII character (ASCII code
/// 33-126)". A character outside this band (a control character, a space, or
/// anything non-ASCII) is not printable ASCII and so is not one the password may
/// carry.
const PRINTABLE_ASCII_BAND: (u8, u8) = (33, 126);

/// One content class an admin password has to carry a character of, as the
/// refusal names it.
///
/// Source: the same doc list — "Must contain at least one uppercase letter",
/// "Must contain at least one lowercase letter", "Must contain one number" —
/// with Amazon Redshift's own name for the last class, which the recorded
/// refusal words as `decimal digit` rather than as the doc's "number".
#[derive(Debug, Clone, Copy)]
struct ContentClass {
    /// How the refusal names the class.
    named: &'static str,
    /// Whether a character belongs to the class.
    holds: fn(char) -> bool,
}

/// The classes an admin password must carry a character of each, in the order
/// they are checked.
///
/// The decimal digit is checked first: it is the class whose refusal Amazon
/// Redshift's recorded answer to a password that satisfies the length rule
/// reports, so it is the one that answers first when more than one class is
/// missing.
const MASTER_PASSWORD_CONTENT_CLASSES: [ContentClass; 3] = [
    ContentClass {
        named: "decimal digit",
        holds: |character| character.is_ascii_digit(),
    },
    ContentClass {
        named: "uppercase letter",
        holds: |character| character.is_ascii_uppercase(),
    },
    ContentClass {
        named: "lowercase letter",
        holds: |character| character.is_ascii_lowercase(),
    },
];

/// What a password that carries no character of `class` is refused with.
///
/// Recorded Amazon Redshift wording, with the modelled member name and the class
/// as its only variable part: fixed text around `The parameter
/// MasterUserPassword must contain at least 1 ` and the class's own name.
fn missing_content_class_message(class: &ContentClass) -> String {
    format!(
        "The parameter {MASTER_PASSWORD_MEMBER} must contain at least 1 {}.",
        class.named
    )
}

/// What a password shorter than the published floor is refused with.
///
/// Recorded Amazon Redshift wording, including the service's own spelling of
/// "atleast"; the number is the published bound, so it is formatted from
/// [`MASTER_PASSWORD_LENGTH_BOUNDS`] rather than spelled out.
fn short_master_password_message() -> String {
    format!(
        "Invalid master password. Password should be atleast {} characters long.",
        MASTER_PASSWORD_LENGTH_BOUNDS.0
    )
}

/// What a password longer than the published ceiling is refused with — the same
/// sentence the floor is refused with, about the other end of the published
/// band.
fn long_master_password_message() -> String {
    format!(
        "Invalid master password. Password should be atmost {} characters long.",
        MASTER_PASSWORD_LENGTH_BOUNDS.1
    )
}

/// What a password carrying a character the member does not admit is refused
/// with: the sentence names the admitted set rather than the password, which is
/// sensitive and is never echoed.
///
/// Amazon Redshift wording for an admin password whose CHARACTERS are the
/// defect: fixed text around the set it names, which is
/// [`MASTER_PASSWORD_EXCLUDED_CHARACTERS`] in the order that sentence lists it.
fn master_password_character_message() -> String {
    let excluded: Vec<String> = MASTER_PASSWORD_EXCLUDED_CHARACTERS
        .iter()
        .map(|character| format!("'{character}'"))
        .collect();
    format!(
        "The parameter {MASTER_PASSWORD_MEMBER} is not a valid password. Only printable ASCII \
         characters except for {} may be used.",
        excluded.join(", ")
    )
}

/// Whether `character` is one an admin password may carry.
fn admitted_password_character(character: char) -> bool {
    let code = u32::from(character);
    (u32::from(PRINTABLE_ASCII_BAND.0)..=u32::from(PRINTABLE_ASCII_BAND.1)).contains(&code)
        && !MASTER_PASSWORD_EXCLUDED_CHARACTERS.contains(&character)
}

/// What a cluster write that OPTS OUT of Redshift-managed admin credentials
/// without setting an admin password of its own is refused with.
///
/// Recorded Amazon Redshift wording: fixed text with no variable part. The two
/// members are the two ways a cluster can have admin credentials at all —
/// `ManageMasterPassword` hands them to AWS Secrets Manager, `MasterUserPassword`
/// sets them on the request ("If `ManageMasterPassword` is false or not set,
/// Amazon Redshift uses `MasterUserPassword` for the admin user account's
/// password", `smithy_query(query="doc", shape="CreateClusterMessage",
/// member="ManageMasterPassword")`) — so a request that turns the first off and
/// names nothing for the second leaves the cluster no admin credential to be
/// created with.
pub const ADMIN_CREDENTIAL_REQUIRED: &str =
    "You're trying to opt out of having Amazon Redshift manage your admin credentials with AWS \
     Secrets Manager without setting your own admin password. Set an admin password, then try \
     again.";

/// Check an admin password against every constraint the service publishes for
/// it, or say which one it breaks.
///
/// The length is decided first, because the length rule is about the password as
/// a whole and a password too short to satisfy it cannot satisfy the content
/// rules either; then the content classes, then the character set.
///
/// Shared by `CreateCluster` and `ModifyCluster` so the rule cannot drift
/// between the operation that sets the password and the one that changes it.
pub fn check_master_user_password(password: &str) -> Result<(), String> {
    let length = password.chars().count();
    if length < MASTER_PASSWORD_LENGTH_BOUNDS.0 {
        return Err(short_master_password_message());
    }
    if length > MASTER_PASSWORD_LENGTH_BOUNDS.1 {
        return Err(long_master_password_message());
    }
    for class in &MASTER_PASSWORD_CONTENT_CLASSES {
        if !password.chars().any(class.holds) {
            return Err(missing_content_class_message(class));
        }
    }
    if !password.chars().all(admitted_password_character) {
        return Err(master_password_character_message());
    }
    Ok(())
}
