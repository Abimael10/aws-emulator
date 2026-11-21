// SPDX-License-Identifier: CC0-1.0

//! The syntax every name this service admits has to satisfy: cluster, parameter
//! group, subnet group, snapshot, grant and subscription identifiers, database
//! and user names, and the credential and token-scope values a session request
//! carries.

use super::*;

/// The syntax rule Redshift documents for one identifier member.
#[derive(Debug, Clone, Copy)]
pub struct IdentifierRule {
    /// The input member the rule belongs to, named in the refusal.
    pub member: &'static str,
    /// The documented maximum length, in characters.
    pub max_len: usize,
    /// Whether the documented rule forbids uppercase letters.
    pub lowercase_only: bool,
}

/// The rule for a cluster identifier (`ClusterIdentifier`, `NewClusterIdentifier`).
///
/// Source: `smithy_query(query="doc", shape="CreateClusterMessage",
/// member="ClusterIdentifier")` — "Must contain from 1 to 63 alphanumeric
/// characters or hyphens. Alphabetic characters must be lowercase. First
/// character must be a letter. Cannot end with a hyphen or contain two
/// consecutive hyphens."
pub const CLUSTER_IDENTIFIER_RULE: IdentifierRule = IdentifierRule {
    member: "ClusterIdentifier",
    max_len: 63,
    lowercase_only: true,
};

/// The rule for a cluster parameter group name (`ParameterGroupName`).
///
/// Source: `smithy_query(query="doc", shape="CreateClusterParameterGroupMessage",
/// member="ParameterGroupName")` — "Must be 1 to 255 alphanumeric characters or
/// hyphens First character must be a letter. Cannot end with a hyphen or contain
/// two consecutive hyphens." Mixed case is ACCEPTED and folded by
/// [`normalized_parameter_group_name`] rather than refused, because the same doc
/// says the value "is stored as a lower-case string".
pub const PARAMETER_GROUP_NAME_RULE: IdentifierRule = IdentifierRule {
    member: "ParameterGroupName",
    max_len: 255,
    lowercase_only: false,
};

/// The form a cluster parameter group name is STORED and reported in.
///
/// Source: the same doc sentence as [`PARAMETER_GROUP_NAME_RULE`] — "This value
/// is stored as a lower-case string." The rule admits only ASCII alphanumerics
/// and hyphens, so ASCII folding is the whole of it.
pub fn normalized_parameter_group_name(requested: &str) -> String {
    requested.to_ascii_lowercase()
}

/// The rule for a cluster subnet group name (`ClusterSubnetGroupName`).
///
/// Source: `smithy_query(query="doc", shape="CreateClusterSubnetGroupMessage",
/// member="ClusterSubnetGroupName")` — "Must contain no more than 255
/// alphanumeric characters or hyphens. Must not be "Default". Must be unique for
/// all subnet groups that are created by your Amazon Web Services account."
/// Mixed case is ACCEPTED and folded by [`normalized_subnet_group_name`] rather
/// than refused, because the same doc says "Amazon Redshift stores the value as a
/// lowercase string".
pub const SUBNET_GROUP_NAME_RULE: IdentifierRule = IdentifierRule {
    member: "ClusterSubnetGroupName",
    max_len: 255,
    lowercase_only: false,
};

/// The one name a cluster subnet group may not take, compared in the stored
/// (folded) form.
///
/// Source: the same doc sentence as [`SUBNET_GROUP_NAME_RULE`] — "Must not be
/// "Default"".
pub const RESERVED_SUBNET_GROUP_NAME: &str = "default";

/// The form a cluster subnet group name is STORED and reported in.
///
/// Source: `smithy_query(query="doc", shape="CreateClusterSubnetGroupMessage",
/// member="ClusterSubnetGroupName")` — "Amazon Redshift stores the value as a
/// lowercase string." The rule admits only ASCII alphanumerics and hyphens, so
/// ASCII folding is the whole of it.
pub fn normalized_subnet_group_name(requested: &str) -> String {
    requested.to_ascii_lowercase()
}

/// The rule for a snapshot identifier (`SnapshotIdentifier`,
/// `FinalClusterSnapshotIdentifier`).
///
/// Source: `smithy_query(query="doc", shape="CreateClusterSnapshotMessage",
/// member="SnapshotIdentifier")` — "Must contain from 1 to 255 alphanumeric
/// characters or hyphens. First character must be a letter. Cannot end with a
/// hyphen or contain two consecutive hyphens." (No lowercase restriction.)
pub const SNAPSHOT_IDENTIFIER_RULE: IdentifierRule = IdentifierRule {
    member: "SnapshotIdentifier",
    max_len: 255,
    lowercase_only: false,
};

/// The rule for a snapshot copy grant name (`SnapshotCopyGrantName`).
///
/// Source: `smithy_query(query="doc", shape="CreateSnapshotCopyGrantMessage",
/// member="SnapshotCopyGrantName")` — "Must contain from 1 to 63 alphanumeric
/// characters or hyphens. Alphabetic characters must be lowercase. First
/// character must be a letter. Cannot end with a hyphen or contain two
/// consecutive hyphens."
pub const SNAPSHOT_COPY_GRANT_NAME_RULE: IdentifierRule = IdentifierRule {
    member: "SnapshotCopyGrantName",
    max_len: 63,
    lowercase_only: true,
};

/// The rule for an event notification subscription name (`SubscriptionName`).
///
/// Source: `smithy_query(query="doc", shape="CreateEventSubscriptionMessage",
/// member="SubscriptionName")` — "Cannot be null, empty, or blank. Must contain
/// from 1 to 255 alphanumeric characters or hyphens. First character must be a
/// letter. Cannot end with a hyphen or contain two consecutive hyphens." (No
/// lowercase restriction.)
pub const EVENT_SUBSCRIPTION_NAME_RULE: IdentifierRule = IdentifierRule {
    member: "SubscriptionName",
    max_len: 255,
    lowercase_only: false,
};

/// Check `value` against `rule`, naming the member and the constraint it broke.
///
/// `member` overrides the rule's own member name, so `NewClusterIdentifier` and
/// `FinalClusterSnapshotIdentifier` are refused under the same rule as the
/// member it was documented on, while still naming themselves.
pub fn check_identifier(rule: IdentifierRule, member: &str, value: &str) -> Result<(), String> {
    let refuse = |why: &str| {
        Err(format!(
            "Invalid value for {member}: '{value}'. {why} (allowed: 1 to {} {}alphanumeric characters or hyphens, first character a letter, no trailing hyphen and no two consecutive hyphens)",
            rule.max_len,
            if rule.lowercase_only { "lowercase " } else { "" },
        ))
    };
    if value.is_empty() {
        return refuse("The identifier cannot be null, empty, or blank.");
    }
    if value.chars().count() > rule.max_len {
        return refuse("The identifier is longer than allowed.");
    }
    if !holds_only_the_identifier_alphabet(value) {
        return refuse("The identifier contains a character that is neither alphanumeric nor a hyphen.");
    }
    if rule.lowercase_only && value.chars().any(|c| c.is_ascii_uppercase()) {
        return refuse("Alphabetic characters must be lowercase.");
    }
    if !begins_with_a_letter(value) {
        return refuse("The first character must be a letter.");
    }
    if value.ends_with('-') {
        return refuse("The identifier cannot end with a hyphen.");
    }
    if value.contains("--") {
        return refuse("The identifier cannot contain two consecutive hyphens.");
    }
    Ok(())
}

/// Whether `value` opens with an ASCII letter, as every identifier of this
/// service must.
///
/// One of the constraints the identifier grammar is made of that carries a rule
/// of its own rather than reading off `str`, written ONCE here: the two refusals
/// that judge an identifier — the create path's per-constraint
/// [`check_identifier`] and the read path's [`check_identifier_syntax`] — word
/// their sentences differently but must decide the same values.
fn begins_with_a_letter(value: &str) -> bool {
    value.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

/// Whether every character of `value` is in the identifier alphabet: ASCII
/// letters, digits and the hyphen. See [`begins_with_a_letter`].
fn holds_only_the_identifier_alphabet(value: &str) -> bool {
    value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Which NAME the identifier validator reports for the cluster identifier an
/// operation was given.
///
/// Recorded behaviour: the grammar check that runs before a cluster lookup is a
/// validator shared with the rest of the RDS family, and the name it reports the
/// offending parameter under differs by the path that judged it — the member's
/// own name on the cluster-scoped reads, and the RDS-family name on
/// `DescribeClusters`. The message is what a caller matches on, so the name is a
/// per-operation attribute rather than one the member decides, which is why each
/// call site names the one its own recording carries.
#[derive(Debug, Clone, Copy)]
pub enum ClusterIdentifierParameter {
    /// The input member's own name, `ClusterIdentifier`.
    AsNamed,
    /// `DBInstanceIdentifier`, the name the shared RDS-family validator carries
    /// a cluster identifier under.
    RdsFamily,
}

impl ClusterIdentifierParameter {
    /// The name the refusal reports.
    pub fn reported(self) -> &'static str {
        match self {
            Self::AsNamed => CLUSTER_IDENTIFIER_RULE.member,
            Self::RdsFamily => "DBInstanceIdentifier",
        }
    }
}

/// Why `parameter` does not hold an identifier this service can resolve.
///
/// Amazon Redshift's own wording for this error class, and entirely fixed text
/// apart from the parameter the validator names: the offending value is NOT
/// echoed, so a caller matching the sentence sees the same one whatever it sent.
pub fn invalid_identifier_message(parameter: &str) -> String {
    format!(
        "The parameter {parameter} is not a valid identifier. Identifiers must begin with a \
         letter; must contain only ASCII letters, digits, and hyphens; and must not end with a \
         hyphen or contain two consecutive hyphens."
    )
}

/// Why `parameter` holds more characters than an identifier may.
///
/// The validator judges the LENGTH before the alphabet, so a value that breaks
/// both is refused with this sentence rather than
/// [`invalid_identifier_message`]'s. Fixed text apart from the parameter named
/// and the documented maximum, both of which the request did not supply.
pub fn identifier_too_long_message(parameter: &str, max_len: usize) -> String {
    format!(
        "The parameter {parameter} is not a valid identifier because it is longer than {max_len} \
         characters."
    )
}

/// Whether `value` satisfies the identifier GRAMMAR an operation that resolves an
/// identifier checks before it looks anything up: it must begin with a letter,
/// hold only ASCII letters, digits and hyphens, and neither end with a hyphen nor
/// carry two consecutive ones.
///
/// Those four constraints are the whole of the grammar
/// [`invalid_identifier_message`] states, so nothing else — the length, which
/// [`check_identifier_syntax`] judges first, or the case of a letter, which the
/// sentence does not mention — is judged here: a name that satisfies the grammar
/// and names nothing is a LOOKUP failure, answered with the operation's own
/// not-found fault, while one that breaks it could never name anything and is a
/// request failure.
pub fn identifier_syntax_holds(value: &str) -> bool {
    begins_with_a_letter(value)
        && holds_only_the_identifier_alphabet(value)
        && !value.ends_with('-')
        && !value.contains("--")
}

/// Refuse `value` under the cluster-identifier syntax, naming the parameter the
/// way the validator of the operation that judged it does.
///
/// Reached by an operation that RESOLVES the cluster identifier it is given,
/// ahead of the lookup. Two refusals, in the order the validator reaches them:
/// the documented maximum length of a cluster identifier
/// ([`CLUSTER_IDENTIFIER_RULE`]) first, then the grammar
/// ([`identifier_syntax_holds`]) — so a value that breaks both is refused for
/// being too long.
pub fn check_identifier_syntax(
    parameter: ClusterIdentifierParameter,
    value: &str,
) -> Result<(), String> {
    let max_len = CLUSTER_IDENTIFIER_RULE.max_len;
    if value.chars().count() > max_len {
        return Err(identifier_too_long_message(parameter.reported(), max_len));
    }
    if !identifier_syntax_holds(value) {
        return Err(invalid_identifier_message(parameter.reported()));
    }
    Ok(())
}

/// The NAME the shared validator that judges a parameter group's name reports it
/// under.
///
/// Recorded behaviour, and the same split [`ClusterIdentifierParameter`] states
/// for a cluster identifier: the grammar check a parameter-group name is judged by
/// is a validator shared with the rest of the RDS family, so the sentence it
/// answers names the member under THAT family's own name rather than under the
/// Redshift member's. The message is what a caller matches on, so the name is
/// stated here once instead of at each call site.
pub const PARAMETER_GROUP_NAME_PARAMETER: &str = "DBParameterGroupName";

/// Refuse a `ParameterGroupName` a create has to MINT under the identifier
/// grammar, naming the parameter the shared validator reports it under
/// ([`PARAMETER_GROUP_NAME_PARAMETER`]).
///
/// The same two refusals as [`check_identifier_syntax`], in the order the
/// validator reaches them — the documented maximum length of a parameter group
/// name ([`PARAMETER_GROUP_NAME_RULE`]) first, then the grammar
/// ([`identifier_syntax_holds`]) — so a value that breaks both is refused for
/// being too long. Neither sentence echoes the value the request carried.
///
/// Only a name a CREATE mints is judged this way: a name an operation merely
/// dereferences is resolved rather than parsed, and the default parameter group of
/// a family carries a `.` in its name, which this grammar does not admit.
pub fn check_parameter_group_identifier(value: &str) -> Result<(), String> {
    let max_len = PARAMETER_GROUP_NAME_RULE.max_len;
    if value.chars().count() > max_len {
        return Err(identifier_too_long_message(
            PARAMETER_GROUP_NAME_PARAMETER,
            max_len,
        ));
    }
    if !identifier_syntax_holds(value) {
        return Err(invalid_identifier_message(PARAMETER_GROUP_NAME_PARAMETER));
    }
    Ok(())
}

/// The syntax rule Redshift documents for a DATABASE object name — a database
/// user, a database name or a database group name.
///
/// A different alphabet from [`IdentifierRule`]'s, which is why it is its own
/// rule: a control-plane identifier admits alphanumerics and hyphens, while a
/// database object name also admits underscore, plus sign, period, at symbol and
/// hyphen, and the three members differ only in whether an uppercase letter is
/// admitted.
#[derive(Debug, Clone, Copy)]
pub struct DatabaseNameRule {
    /// The input member the rule belongs to, named in the refusal.
    pub member: &'static str,
    /// Whether the documented rule forbids uppercase letters.
    pub lowercase_only: bool,
    /// Whether the documented rule forbids the value `PUBLIC`, the group every
    /// database user is in ([`PUBLIC_DATABASE_GROUP`]).
    pub forbids_public: bool,
}

/// The documented maximum length, in characters, of every database object name
/// below: "Must be 1 to 64 alphanumeric characters or hyphens".
pub const DATABASE_NAME_MAX_LEN: usize = 64;

/// The rule for the database user a credential is issued for (`DbUser`).
///
/// Source: `smithy_query(query="doc", shape="GetClusterCredentialsMessage",
/// member="DbUser")` — "Must be 1 to 64 alphanumeric characters or hyphens. The
/// user name can't be PUBLIC. Must contain uppercase or lowercase letters,
/// numbers, underscore, plus sign, period (dot), at symbol (@), or hyphen. First
/// character must be a letter. Must not contain a colon ( : ) or slash ( / )."
pub const DB_USER_RULE: DatabaseNameRule = DatabaseNameRule {
    member: "DbUser",
    lowercase_only: false,
    forbids_public: true,
};

/// The rule for the database a credential authorises logging on to (`DbName`).
///
/// Source: `smithy_query(query="doc", shape="GetClusterCredentialsMessage",
/// member="DbName")` — the same sentences as [`DB_USER_RULE`]'s, without the
/// `PUBLIC` exclusion: "Must be 1 to 64 alphanumeric characters or hyphens Must
/// contain uppercase or lowercase letters, numbers, underscore, plus sign,
/// period (dot), at symbol (@), or hyphen. First character must be a letter.
/// Must not contain a colon ( : ) or slash ( / )."
pub const DB_NAME_RULE: DatabaseNameRule = DatabaseNameRule {
    member: "DbName",
    lowercase_only: false,
    forbids_public: false,
};

/// The rule for one database group a session joins (`DbGroups`).
///
/// Source: `smithy_query(query="doc", shape="GetClusterCredentialsMessage",
/// member="DbGroups")` — "Must be 1 to 64 alphanumeric characters or hyphens
/// Must contain ONLY LOWERCASE letters, numbers, underscore, plus sign, period
/// (dot), at symbol (@), or hyphen. First character must be a letter. Must not
/// contain a colon ( : ) or slash ( / )."
pub const DB_GROUP_RULE: DatabaseNameRule = DatabaseNameRule {
    member: "DbGroups",
    lowercase_only: true,
    forbids_public: false,
};

/// Check one database object name against `rule`, naming the member and the
/// constraint it broke.
///
/// The documented alphabet is the whole of what is admitted, so the colon and
/// the slash the same documentation calls out separately cannot pass it either.
/// The documentation's remaining constraint, "cannot be a reserved word", is a
/// list published in the Amazon Redshift Database Developer Guide rather than
/// anything the API model carries, so it is not checked here.
pub fn check_database_name(rule: DatabaseNameRule, value: &str) -> Result<(), String> {
    let member = rule.member;
    let refuse = |why: &str| {
        Err(format!(
            "Invalid value for {member}: '{value}'. {why} (allowed: 1 to {DATABASE_NAME_MAX_LEN} {}letters, numbers, underscore, plus sign, period, at symbol or hyphen, first character a letter)",
            if rule.lowercase_only { "lowercase " } else { "" },
        ))
    };
    if value.is_empty() {
        return refuse("The name cannot be null, empty, or blank.");
    }
    if value.chars().count() > DATABASE_NAME_MAX_LEN {
        return refuse("The name is longer than allowed.");
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '.' | '@' | '-'))
    {
        return refuse("The name contains a character that is not allowed in it.");
    }
    if rule.lowercase_only && value.chars().any(|c| c.is_ascii_uppercase()) {
        return refuse("Alphabetic characters must be lowercase.");
    }
    if !value.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return refuse("The first character must be a letter.");
    }
    // Redshift folds an unquoted database identifier to lower case, so the
    // excluded name is matched without regard to the case it arrived in.
    if rule.forbids_public && value.eq_ignore_ascii_case(PUBLIC_DATABASE_GROUP) {
        return refuse("The name cannot be PUBLIC.");
    }
    Ok(())
}

/// The window `GetClusterCredentials` documents for the lifetime of the
/// temporary database credentials it issues: the lifetime a request that names
/// none gets, and the inclusive bounds a request may ask for.
///
/// Source: `smithy_query(query="doc", shape="GetClusterCredentialsMessage",
/// member="DurationSeconds")` — "The number of seconds until the returned
/// temporary password expires. Constraint: minimum 900, maximum 3600. Default:
/// 900". A documented duration, so these are Amazon Redshift's numbers rather
/// than this emulator's choice.
pub const CREDENTIAL_DURATION_DEFAULT_SECONDS: i32 = 900;
/// The smallest `DurationSeconds` a request may name. See
/// [`CREDENTIAL_DURATION_DEFAULT_SECONDS`].
pub const CREDENTIAL_DURATION_MIN_SECONDS: i32 = 900;
/// The largest `DurationSeconds` a request may name. See
/// [`CREDENTIAL_DURATION_DEFAULT_SECONDS`].
pub const CREDENTIAL_DURATION_MAX_SECONDS: i32 = 3600;

/// How long the credentials a request asks for last, or why the request's
/// `DurationSeconds` is outside the documented window.
///
/// The refusal is Amazon Redshift's own recorded wording for this error class —
/// `Token duration must be between <min> and <max> seconds` — whose only
/// variable parts are the two documented bounds, both constants: the refusal
/// does not echo the value the request asked for, so a caller matching on the
/// sentence sees the same one whatever it sent.
pub fn credential_duration(requested: Option<i32>) -> Result<Duration, String> {
    let seconds = requested.unwrap_or(CREDENTIAL_DURATION_DEFAULT_SECONDS);
    if !(CREDENTIAL_DURATION_MIN_SECONDS..=CREDENTIAL_DURATION_MAX_SECONDS).contains(&seconds) {
        return Err(credential_duration_out_of_range_message());
    }
    Ok(Duration::from_secs(seconds.unsigned_abs().into()))
}

/// Why a `DurationSeconds` outside the documented window is refused. See
/// [`credential_duration`].
pub fn credential_duration_out_of_range_message() -> String {
    format!(
        "Token duration must be between {CREDENTIAL_DURATION_MIN_SECONDS} and \
         {CREDENTIAL_DURATION_MAX_SECONDS} seconds"
    )
}

/// The `DbUser` a `GetClusterCredentials` response reports for the user named in
/// its request.
///
/// Source: `smithy_query(query="doc", operation="GetClusterCredentials")` — "The
/// action returns the database user name prefixed with `IAM:` if `AutoCreate` is
/// `False` or `IAMA:` if `AutoCreate` is `True`" — so the prefix is decided by
/// the request's own `AutoCreate` and by nothing else.
pub fn credential_db_user(db_user: &str, auto_create: bool) -> String {
    let prefix = if auto_create {
        AUTO_CREATED_DB_USER_PREFIX
    } else {
        ::service_common::redshift_database_users::IAM_IDENTITY_DB_USER_PREFIX
    };
    format!("{prefix}{db_user}")
}

/// The prefix the database user of an identity `AutoCreate` creates on the way in
/// carries. See [`credential_db_user`].
const AUTO_CREATED_DB_USER_PREFIX: &str = "IAMA:";

/// The prefix the database user of an IAM ROLE identity carries, beside the
/// `IAM:` prefix [`credential_db_user`] states for every other IAM identity.
///
/// Source: `smithy_query(query="doc", operation="GetClusterCredentials")` states
/// the `IAM:`/`IAMA:` prefixes an identity logs on under, and the Amazon Redshift
/// identity-to-database-user mapping gives a ROLE its own prefix — so the prefix
/// says which KIND of IAM identity the database user stands for, and the name
/// after it is that identity's own name. Stated through the ONE definition of that
/// mapping, which the Data API reports the user a statement ran as from.
pub const IAM_ROLE_DB_USER_PREFIX: &str =
    ::service_common::redshift_database_users::IAM_ROLE_DB_USER_PREFIX;

/// The `DbUser` a `GetClusterCredentialsWithIAM` response reports for the IAM
/// identity the request was signed with, given that identity's ARN.
///
/// Source: `smithy_query(query="doc", shape="ClusterExtendedCredentials",
/// member="DbUser")` — "A database user name that you provide when you connect to
/// a database. The database user is mapped 1:1 to the SOURCE IAM IDENTITY" —
/// paired with the prefix `GetClusterCredentials` documents for a user an IAM
/// identity logs on as ([`credential_db_user`]). So the name is the IDENTITY's own
/// name, and nothing in the request decides it: unlike `GetClusterCredentials`,
/// this operation takes no `DbUser` at all.
///
/// The identity's own name is the name field of its ARN — the user name of
/// `user/<path…><name>` or of `federated-user/<name>`, the ROLE name of
/// `assumed-role/<role>/<session>` — so the reported name always appears in the
/// ARN `sts:GetCallerIdentity` answers for the same credential. A role identity
/// carries [`IAM_ROLE_DB_USER_PREFIX`]; every other IAM identity carries the
/// plain `IAM:` prefix (never the `IAMA:` one, which `GetClusterCredentials`
/// documents for an auto-created user and this operation models no member for).
///
/// It is a function of the identity alone, so two calls signed with two
/// credentials of ONE identity map to one database user, which is what makes the
/// mapping 1:1 — and a value that is not a well-formed ARN is read as the name it
/// stands for rather than replaced.
///
/// Stated through the ONE definition of that mapping
/// ([`service_common::redshift_database_users::iam_identity_db_user`]) rather than
/// built here, so the user this service mints a credential for is the user the
/// Data API reports a statement of the same identity as having run as.
pub fn iam_identity_db_user(caller_identity_arn: &str) -> String {
    ::service_common::redshift_database_users::iam_identity_db_user(caller_identity_arn)
}

// The clusters a token request may scope its token to are documented on the
// `ClusterIds` member itself — "`ClusterIds` must contain at least 1 cluster
// identifier. `ClusterIds` can hold a maximum of 20 cluster identifiers",
// "cluster identifiers must be 1 to 63 characters in length", "must start with
// a letter" and "can't end with a hyphen or contain two consecutive hyphens"
// (`smithy_query(query="doc", shape="GetIdentityCenterAuthTokenRequest",
// member="ClusterIds")`) — and NO rule of this store enforces them,
// deliberately: the operation they belong to gates on the CALLER's credential
// type first and unconditionally, so no request this emulator serves is ever
// judged on its scope (see `crate::ops::get_identity_center_auth_token`). The
// minimum is in any case the member's own `@required` trait, which the boundary
// validator refuses a request without before any handler runs.
