// SPDX-License-Identifier: CC0-1.0

//! The IDENTIFIER a portfolio-principal association records for the principal it names.
//!
//! A portfolio association is addressed by the `PrincipalARN` the request carried — that
//! is the value `AssociatePrincipalWithPortfolio` and `DisassociatePrincipalFromPortfolio`
//! name an association by — but the value a READ of the association reports is the
//! identifier of the PRINCIPAL, not a copy of the request string: for a fully defined ARN
//! (`PrincipalType` `IAM`) that is the principal's own unique id in the account's identity
//! records, which is how a client correlates a portfolio's principals with the roles,
//! users and groups IAM reports.
//!
//! `Principal$PrincipalARN` is what draws the line: "This field allows for an ARN with no
//! accountID, with or without wildcard characters if the `PrincipalType` is an
//! `IAM_PATTERN`". An `IAM_PATTERN` therefore names a SET of principals rather than one —
//! it may omit the account and carry `*`/`?` — so there is no principal to resolve and the
//! pattern itself is what the association records and reports.
//!
//! The identity records are IAM's OWN state, reached through the SAME shared engine this
//! service's store lives in, so what is read is the very record `iam:CreateRole`,
//! `iam:CreateUser` or `iam:CreateGroup` wrote, and the id reported here is the one
//! `iam:GetRole` reports for that role. Those records are the AUTHORITY: nothing here
//! mints an identifier of its own, so a value they resolve to no principal of this account
//! is recorded as the request gave it.
//!
//! LOCK ORDER: the IAM store is taken on its own and its guard released before this
//! service's own store is locked, so no two stores are held at once.

use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_state;

use iam_state::EntityKind;

/// The identity records a concrete principal ARN is resolved against: the IAM service's
/// own state struct, reached through the shared engine.
pub type IamState = iam_state::ComAmazonawsIamAwsidentitymanagementv20100508State;

/// The `PrincipalType` whose ARN names a PATTERN rather than one principal
/// (`smithy_query query="shape" shape="PrincipalType"`: `IAM` | `IAM_PATTERN`).
pub const PRINCIPAL_TYPE_IAM_PATTERN: &str = "IAM_PATTERN";

/// The identifier an association of `principal_arn` as `principal_type` records, resolved
/// against the account's identity records in the shared engine.
///
/// Called by `AssociatePrincipalWithPortfolio` with the engine the handler holds, so the
/// identity records are reached the same way this service's own store is. The resolution
/// happens ONCE, when the association is stored, which is the value every later read of
/// that association reports.
///
/// `None` when this engine carries no identity records to resolve against at all — the
/// same condition `crate::state::service_state` answers `None` for, which is unreachable
/// in the assembled emulator and which the handler answers with the same modelled server
/// error rather than with a value nothing backs.
pub fn association_principal_identifier<Eng: Send + Sync + 'static>(
    engine: &Eng,
    principal_arn: &str,
    principal_type: &str,
) -> Option<String> {
    // A pattern names no single principal, so there is nothing to resolve: what was
    // associated is the pattern, and that is what is recorded.
    if principal_type == PRINCIPAL_TYPE_IAM_PATTERN {
        return Some(principal_arn.to_string());
    }
    let iam = iam_state::service_state(engine)?;
    let held = iam.lock();
    Some(principal_identifier(&held, principal_arn))
}

/// The ARN of the principal THIS REQUEST speaks for, as `sts:GetCallerIdentity` reports it
/// for the credential the request was signed with — what a launch records as the principal
/// that asked for it.
///
/// The question is IAM's, so IAM's own resolution answers it against the records in this
/// same shared engine: the user that holds the long-term access key, or the role session a
/// temporary credential was vended for. Every credential those records name no identity for
/// — the long-term pair this emulator publishes at start among them — resolves to the one
/// principal STS reports for all of them, which is the identity such a caller reads back for
/// itself. Nothing here spells an identity out of the access key id's text: a client that
/// reads its own identity and then reads what the launch recorded has to see one principal.
///
/// LOCK ORDER: the IAM store is taken on its own and released before this service's own
/// store is locked, so no two stores are held at once — call it BEFORE locking this
/// service's state.
pub fn request_principal_arn<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> String {
    match iam_state::service_state(engine) {
        Some(iam) => iam_state::authorization::caller_principal_arn(&iam.lock(), ctx),
        // No identity records on this engine: the only credential a request can present
        // there is the one this emulator publishes, which speaks for the account's own
        // principal.
        None => ::service_common::caller_principal::ambient_user_arn(),
    }
}

/// The principal a request speaks for, as every ARN a portfolio-principal association can
/// name that one caller by.
///
/// A grant is addressed by the ARN the association was made with, and ONE caller answers to
/// several of them: a request signed with a temporary credential speaks for a role SESSION,
/// whose permissions are the role's, and a request signed by a user is granted whatever the
/// GROUPS that user belongs to are granted ("you cannot identify a user group as a
/// `Principal`" in a policy, but `PrincipalARN` is "the principal (user, role, or group)",
/// so a group association names its members). So the question an association answers is not
/// "is this ARN the caller's" but "does this ARN name the caller", which is what
/// [`Self::names`] decides.
#[derive(Debug, Clone)]
pub struct CallerPrincipal {
    /// The ARN `sts:GetCallerIdentity` reports for this request's credential FIRST, then
    /// every other ARN the account's identity records let an association name the same
    /// caller by.
    arns: Vec<String>,
}

impl CallerPrincipal {
    /// The ARN `sts:GetCallerIdentity` reports for this request — what a record of who
    /// asked for a launch carries.
    pub fn arn(&self) -> &str {
        // The resolution always puts the reported principal first, and the vector is
        // never built empty.
        self.arns.first().map(String::as_str).unwrap_or_default()
    }

    /// Whether an association made with `association_arn` under `principal_type` names
    /// THIS caller.
    ///
    /// A fully defined `IAM` ARN names one principal, so it names the caller exactly when
    /// it is one of the ARNs the caller answers to. An `IAM_PATTERN` names a SET — the type
    /// whose ARN may omit the account and carry `*`/`?` wildcards
    /// (`AssociatePrincipalWithPortfolio$PrincipalARN`) — so the caller is in that set when
    /// the pattern matches one of its ARNs, with IAM's own wildcard reading
    /// ([`::service_common::iam_authorization::wildcard_match`], `*` for any run of
    /// characters and `?` for exactly one). An omitted account field reads as THIS
    /// account's, the same way every other account-less IAM ARN this tree resolves does.
    pub fn names(&self, association_arn: &str, principal_type: &str) -> bool {
        if principal_type == PRINCIPAL_TYPE_IAM_PATTERN {
            let pattern = account_filled_in(association_arn);
            return self.arns.iter().any(|arn| {
                // An identity ARN is compared case-sensitively, as every resource ARN is.
                ::service_common::iam_authorization::wildcard_match(&pattern, arn, true)
            });
        }
        self.arns.iter().any(|arn| arn == association_arn)
    }
}

/// `pattern` with an EMPTY account field filled in with this account's id, so an
/// account-less `IAM_PATTERN` is matched against the ARNs of the account whose portfolio
/// carries it. A value that is not a six-field ARN, or one that already names an account,
/// is its own pattern.
fn account_filled_in(pattern: &str) -> String {
    let fields: Vec<&str> = pattern.splitn(6, ':').collect();
    let [prefix, partition, service, region, account, resource] = fields.as_slice() else {
        return pattern.to_string();
    };
    if !account.is_empty() {
        return pattern.to_string();
    }
    let account = ::engine::emulator_account_id();
    format!("{prefix}:{partition}:{service}:{region}:{account}:{resource}")
}

/// The principal THIS REQUEST speaks for, as every ARN an association can name it by:
/// [`request_principal_arn`] plus the identities the account's records put BEHIND that
/// principal.
///
/// LOCK ORDER: the IAM store is taken on its own and released before this service's own
/// store is locked, so no two stores are held at once — call it BEFORE locking this
/// service's state.
pub fn request_caller_principal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> CallerPrincipal {
    let Some(iam) = iam_state::service_state(engine) else {
        // No identity records on this engine: the only credential a request can present
        // there is the one this emulator publishes, which speaks for the account's own
        // principal and belongs to no group this engine holds.
        return CallerPrincipal {
            arns: vec![::service_common::caller_principal::ambient_user_arn()],
        };
    };
    let held = iam.lock();
    let reported = iam_state::authorization::caller_principal_arn(&held, ctx);
    let behind = identities_behind(&held, &reported);
    let mut arns = vec![reported];
    arns.extend(behind);
    CallerPrincipal { arns }
}

/// The ARNs the identity records put BEHIND the principal `principal_arn` — the other
/// values an association can name that same caller by.
///
/// * A role SESSION is granted what its ROLE is granted: "the permissions in force on such
///   a session are the role's", so an association naming the role names every session of
///   it. The role is addressed the way IAM holds it, so a role under a path is named by the
///   ARN that carries the path rather than by one rebuilt from the session.
/// * A USER is granted what the groups it belongs to are granted, which is the membership
///   IAM itself reads to collect a user's policies.
///
/// Nothing is invented: a principal the records hold nothing for puts no further ARN
/// behind it.
fn identities_behind(iam: &IamState, principal_arn: &str) -> Vec<String> {
    if let Some(role_name) = ::service_common::iam_role::assumed_role_session_role_name(principal_arn)
    {
        return iam
            .roles
            .get(&iam_state::name_key(role_name))
            .map(|role| vec![role.arn.clone()])
            .unwrap_or_default();
    }
    if let Some(user_name) = ::service_common::iam_role::user_name(principal_arn) {
        let member = iam_state::name_key(user_name);
        return iam
            .group_keys_held_by(&member)
            .filter_map(|group_key| iam.groups.get(group_key))
            .map(|group| group.arn.clone())
            .collect();
    }
    Vec::new()
}

/// [`association_principal_identifier`] for a CONCRETE principal against identity records
/// already locked: the unique id those records hold for the principal the ARN names.
///
/// The records are the AUTHORITY on a principal's identifier, so a value they resolve to
/// no principal is recorded as given: a principal they hold no record of — provisioned out
/// of band, or deleted since — has no identifier to report, and neither has a value that
/// names no IAM user, role or group at all, which is a `PrincipalARN`
/// `AssociatePrincipalWithPortfolio` has already refused for an `IAM` association. Nothing
/// here mints an id, because an id IAM did not mint is one no `iam:GetRole` would report.
pub fn principal_identifier(iam: &IamState, principal_arn: &str) -> String {
    let Some(entity) = iam_state::parse_entity_arn(principal_arn) else {
        return principal_arn.to_string();
    };
    held_identifier(iam, &entity, principal_arn).unwrap_or_else(|| principal_arn.to_string())
}

/// The unique id the identity records HOLD for the principal `entity` names, or `None`
/// when they hold no such principal.
///
/// Each of the three kinds of principal is found the way IAM itself addresses it — by the
/// case-folded name its map is keyed on — and only a record whose OWN ARN is the one asked
/// for answers, so a role and a user of the same name cannot be mistaken for each other.
fn held_identifier(
    iam: &IamState,
    entity: &iam_state::EntityArn,
    principal_arn: &str,
) -> Option<String> {
    let key = iam_state::name_key(&entity.name);
    match entity.kind {
        EntityKind::User => iam
            .users
            .get(&key)
            .filter(|user| user.arn == principal_arn)
            .map(|user| user.user_id.clone()),
        EntityKind::Role => iam
            .roles
            .get(&key)
            .filter(|role| role.arn == principal_arn)
            .map(|role| role.role_id.clone()),
        EntityKind::Group => iam
            .groups
            .get(&key)
            .filter(|group| group.arn == principal_arn)
            .map(|group| group.group_id.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: an association names the caller when its ARN is one the caller answers to —
    /// exactly, for a fully defined `IAM` principal, and by IAM's `*`/`?` wildcards for an
    /// `IAM_PATTERN`, whose empty account field reads as this account's.
    #[test]
    fn an_association_names_the_caller_its_arn_addresses() {
        let account = ::engine::emulator_account_id();
        let session = format!("arn:aws:sts::{account}:assumed-role/Launcher/launch-session");
        let role = format!("arn:aws:iam::{account}:role/Launcher");
        let caller = CallerPrincipal {
            arns: vec![session.clone(), role.clone()],
        };

        // Rows differ along the TYPE of the association, the form of its ARN (a role, a
        // session, another principal, a path, a wildcard, an account-less pattern) and
        // which of the caller's ARNs it addresses.
        let rows: &[(&str, String, &str, bool)] = &[
            ("the role behind the session", role.clone(), "IAM", true),
            ("the session itself", session.clone(), "IAM", true),
            (
                "another role of the account",
                format!("arn:aws:iam::{account}:role/Other"),
                "IAM",
                false,
            ),
            (
                "the same name under a path",
                format!("arn:aws:iam::{account}:role/service-role/Launcher"),
                "IAM",
                false,
            ),
            (
                "a wildcard, which is no wildcard for an IAM principal",
                format!("arn:aws:iam::{account}:role/Launch*"),
                "IAM",
                false,
            ),
            (
                "a pattern over the role name",
                format!("arn:aws:iam::{account}:role/Launch*"),
                PRINCIPAL_TYPE_IAM_PATTERN,
                true,
            ),
            (
                "an account-less pattern of this account",
                "arn:aws:iam:::role/Launch?r".to_string(),
                PRINCIPAL_TYPE_IAM_PATTERN,
                true,
            ),
            (
                "a pattern over another account",
                "arn:aws:iam::999999999999:role/Launch*".to_string(),
                PRINCIPAL_TYPE_IAM_PATTERN,
                false,
            ),
            (
                "a pattern over the sessions of the role",
                format!("arn:aws:sts::{account}:assumed-role/Launcher/*"),
                PRINCIPAL_TYPE_IAM_PATTERN,
                true,
            ),
            (
                "a pattern over another role's sessions",
                format!("arn:aws:sts::{account}:assumed-role/Other/*"),
                PRINCIPAL_TYPE_IAM_PATTERN,
                false,
            ),
            (
                "a single-character wildcard one character short of the name",
                format!("arn:aws:iam::{account}:role/Launch?"),
                PRINCIPAL_TYPE_IAM_PATTERN,
                false,
            ),
            (
                "a value that is not an ARN at all",
                "Launcher".to_string(),
                PRINCIPAL_TYPE_IAM_PATTERN,
                false,
            ),
        ];
        for (case, association_arn, principal_type, named) in rows {
            let answered = caller.names(association_arn, principal_type);
            assert_eq!(
                answered, *named,
                "case {case:?}: expected {association_arn} as {principal_type} to name the \
                 caller = {named}, got {answered}"
            );
        }
    }

    /// RULE: a concrete principal the identity records HOLD reports the id those records
    /// hold for it; a value they resolve to no principal is recorded as given, and nothing
    /// mints an id of its own.
    #[test]
    fn a_concrete_principal_reports_the_identifier_the_records_hold() {
        let account = ::engine::emulator_account_id();
        let iam = IamState::default();

        // The user the account holds before any request is served, with the id the
        // identity records themselves hold for it.
        let ambient_arn = ::service_common::caller_principal::ambient_user_arn();
        let held = principal_identifier(&iam, &ambient_arn);
        assert_eq!(
            held,
            ::service_common::caller_principal::ambient_user_id(),
            "case \"a user the records hold\": expected the id IAM holds, got {held:?}"
        );
        assert_ne!(
            held, ambient_arn,
            "case \"a user the records hold\": the request ARN must not be what is recorded"
        );

        // Rows differ along the kind of principal and its path. The records hold none of
        // them, so each is recorded as given: an identifier is theirs to mint, and one
        // this service minted would be an id no `iam:GetRole` would ever report.
        let rows: &[(&str, String)] = &[
            (
                "a role the records do not hold",
                format!("arn:aws:iam::{account}:role/Admin"),
            ),
            (
                "a role under a path",
                format!("arn:aws:iam::{account}:role/service-role/Launcher"),
            ),
            (
                "a user the records do not hold",
                format!("arn:aws:iam::{account}:user/Builder"),
            ),
            (
                "a group the records do not hold",
                format!("arn:aws:iam::{account}:group/Builders"),
            ),
            (
                "a value naming no principal at all",
                format!("arn:aws:iam::{account}:policy/Admin"),
            ),
        ];
        for (case, arn) in rows {
            let answered = principal_identifier(&iam, arn);
            assert_eq!(
                &answered, arn,
                "case {case:?}: expected the value recorded as given, got {answered:?}"
            );
        }
    }

    /// RULE: an `IAM_PATTERN` association records the pattern it was given, while a
    /// concrete `IAM` principal the records hold records that principal's identifier.
    #[test]
    fn a_pattern_association_records_the_pattern_it_was_given() {
        let engine = ::std::sync::Arc::new(::engine::InMemoryEngine::new());
        let ambient_arn = ::service_common::caller_principal::ambient_user_arn();
        // (case, the ARN, the type it is associated under, whether the ARN is recorded)
        let rows: &[(&str, &str, &str, bool)] = &[
            (
                "an account-less wildcard pattern",
                "arn:aws:iam:::role/Team*",
                PRINCIPAL_TYPE_IAM_PATTERN,
                true,
            ),
            (
                "a fully defined ARN as a pattern",
                &ambient_arn,
                PRINCIPAL_TYPE_IAM_PATTERN,
                true,
            ),
            (
                "the same ARN as a concrete principal",
                &ambient_arn,
                "IAM",
                false,
            ),
        ];
        for (case, arn, principal_type, recorded_as_given) in rows {
            let answered = association_principal_identifier(&*engine, arn, principal_type)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the shared engine carries identity records")
                });
            assert_eq!(
                &answered == arn,
                *recorded_as_given,
                "case {case:?}: expected the ARN recorded as given = {recorded_as_given}, got \
                 {answered:?}"
            );
        }
    }
}
