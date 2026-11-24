// SPDX-License-Identifier: CC0-1.0

//! Fine-grained access control on a domain of this service: the records the domain's own
//! security API holds, and the ONE decision every request that reaches that domain is
//! taken by.
//!
//! A domain created with `AdvancedSecurityOptions$Enabled` set has fine-grained access
//! control switched on, and on such a domain the cluster — not IAM alone — decides who a
//! request speaks for and what that identity may do. Who: the cluster admits nobody
//! anonymously, and a request presenting HTTP Basic credentials is admitted only as a user
//! its INTERNAL USER DATABASE holds ([`authentication`]). What: the master user the create
//! named may do everything, and every other principal may do only what a ROLE MAPPING binds
//! it to and what the roles it is bound to permit ([`super::fgac_permissions`]). A principal
//! no mapping binds is refused, whatever its IAM policies allow, and the cluster states that
//! refusal as a `security_exception`.
//!
//! Both paths into a domain take the decision HERE so they cannot drift:
//!
//! * the domain's REST data plane ([`crate::service::domain_rest`]), which a client
//!   addresses at the endpoint `DescribeDomain` published, and
//! * the document write a producer outside this service performs ([`super::ingest`]) — a
//!   delivery stream whose destination is this domain, say — where the principal is the
//!   role that producer was configured with.
//!
//! Nothing here reaches the clock or the network: the decision is a pure function of the
//! stored domain and the principal, so the same request decides the same way twice.

use ::service_common::iam_authorization::wildcard_match;

use std::collections::BTreeMap;

use super::Domain;

/// The cluster action a request for the health of the cluster asks for, as the security
/// plugin's own action names spell it (`cluster:monitor/health`, the action the
/// `cluster_monitor` action group carries). A refusal names the action it refused.
pub const ACTION_CLUSTER_HEALTH: &str = "cluster:monitor/health";

/// The action a read of the cluster's own banner asks for.
pub const ACTION_CLUSTER_STATE: &str = "cluster:monitor/state";

/// The action a read of the cluster's nodes asks for.
pub const ACTION_NODES_INFO: &str = "cluster:monitor/nodes/info";

/// The action a read of the cluster's own settings asks for.
pub const ACTION_GET_CLUSTER_SETTINGS: &str = "cluster:monitor/settings";

/// The action updating those settings asks for.
pub const ACTION_UPDATE_CLUSTER_SETTINGS: &str = "cluster:admin/settings/update";

/// The action writing an ingest pipeline asks for.
pub const ACTION_PUT_PIPELINE: &str = "cluster:admin/ingest/pipeline/put";

/// The action reading one asks for.
pub const ACTION_GET_PIPELINE: &str = "cluster:admin/ingest/pipeline/get";

/// The action deleting one asks for.
pub const ACTION_DELETE_PIPELINE: &str = "cluster:admin/ingest/pipeline/delete";

/// The action running one over a document without storing it asks for.
pub const ACTION_SIMULATE_PIPELINE: &str = "cluster:admin/ingest/pipeline/simulate";

/// The action writing an index-management policy asks for.
pub const ACTION_PUT_ISM_POLICY: &str = "cluster:admin/opendistro/ism/policy/write";

/// The action reading one asks for.
pub const ACTION_GET_ISM_POLICY: &str = "cluster:admin/opendistro/ism/policy/get";

/// The action deleting one asks for.
pub const ACTION_DELETE_ISM_POLICY: &str = "cluster:admin/opendistro/ism/policy/delete";

/// The action writing an alerting MONITOR asks for, as the alerting plugin's own action
/// names spell it.
pub const ACTION_WRITE_MONITOR: &str = "cluster:admin/opendistro/alerting/monitor/write";

/// The action reading one asks for.
pub const ACTION_GET_MONITOR: &str = "cluster:admin/opendistro/alerting/monitor/get";

/// The action deleting one asks for.
pub const ACTION_DELETE_MONITOR: &str = "cluster:admin/opendistro/alerting/monitor/delete";

/// The action running one asks for.
pub const ACTION_EXECUTE_MONITOR: &str = "cluster:admin/opendistro/alerting/monitor/execute";

/// The action reading the alerts its triggers left asks for.
pub const ACTION_GET_ALERTS: &str = "cluster:admin/opendistro/alerting/alerts/get";

/// The action writing a legacy alerting DESTINATION asks for.
pub const ACTION_WRITE_DESTINATION: &str =
    "cluster:admin/opendistro/alerting/destination/write";

/// The action reading one asks for.
pub const ACTION_GET_DESTINATION: &str = "cluster:admin/opendistro/alerting/destination/get";

/// The action deleting one asks for.
pub const ACTION_DELETE_DESTINATION: &str =
    "cluster:admin/opendistro/alerting/destination/delete";

/// The action creating a NOTIFICATIONS channel configuration asks for, as the notifications
/// plugin's own action names spell it.
pub const ACTION_CREATE_NOTIFICATION_CONFIG: &str =
    "cluster:admin/opensearch/notifications/configs/create";

/// The action reading one asks for.
pub const ACTION_GET_NOTIFICATION_CONFIG: &str =
    "cluster:admin/opensearch/notifications/configs/get";

/// The action updating one asks for.
pub const ACTION_UPDATE_NOTIFICATION_CONFIG: &str =
    "cluster:admin/opensearch/notifications/configs/update";

/// The action deleting one asks for.
pub const ACTION_DELETE_NOTIFICATION_CONFIG: &str =
    "cluster:admin/opensearch/notifications/configs/delete";

/// The action a write of one document asks for.
pub const ACTION_INDEX_DOCUMENT: &str = "indices:data/write/index";

/// The action a PARTIAL-DOCUMENT update asks for — the update API's own action, beside the
/// write the merged document it produces is stored by.
pub const ACTION_UPDATE_DOCUMENT: &str = "indices:data/write/update";

/// The action a BULK write asks for — the one action the bulk API itself is registered
/// under, beside the per-item action each of its items asks for.
pub const ACTION_BULK: &str = "indices:data/write/bulk";

/// The action adding or removing an alias asks for.
pub const ACTION_ALIASES: &str = "indices:admin/aliases";

/// The action reading an index's aliases asks for.
pub const ACTION_GET_ALIASES: &str = "indices:admin/aliases/get";

/// The action reading an index's own settings asks for.
pub const ACTION_GET_SETTINGS: &str = "indices:monitor/settings/get";

/// The action updating an index's settings asks for.
pub const ACTION_UPDATE_SETTINGS: &str = "indices:admin/settings/update";

/// The action reading an index's field mappings asks for.
pub const ACTION_GET_MAPPING: &str = "indices:admin/mappings/get";

/// The action adding to those mappings asks for.
pub const ACTION_PUT_MAPPING: &str = "indices:admin/mapping/put";

/// The action the index listing of the `_cat` APIs asks for — the index statistics those
/// rows report.
pub const ACTION_INDICES_STATS: &str = "indices:monitor/stats";

/// The action writing a component template asks for.
pub const ACTION_PUT_COMPONENT_TEMPLATE: &str = "cluster:admin/component_template/put";

/// The action reading one asks for.
pub const ACTION_GET_COMPONENT_TEMPLATE: &str = "cluster:admin/component_template/get";

/// The action a search asks for.
pub const ACTION_SEARCH: &str = "indices:data/read/search";

/// The action a refresh asks for — making the writes already taken visible to search.
pub const ACTION_REFRESH: &str = "indices:admin/refresh";

/// The action a count asks for.
pub const ACTION_COUNT: &str = "indices:data/read/count";

/// The action reporting how a text is analyzed asks for — the index-administration action
/// the engine's own analyze API is registered under.
pub const ACTION_ANALYZE: &str = "indices:admin/analyze";

/// The action a read of one document by its id asks for.
pub const ACTION_GET_DOCUMENT: &str = "indices:data/read/get";

/// The action deleting one document asks for.
pub const ACTION_DELETE_DOCUMENT: &str = "indices:data/write/delete";

/// The action deleting every document a query selects asks for — the action the engine's own
/// delete-by-query API is registered under.
pub const ACTION_DELETE_BY_QUERY: &str = "indices:data/write/delete/byquery";

/// The action reading ONE task of the cluster asks for — the monitoring action the engine's
/// own task API is registered under, which is what a client polls a by-query write it
/// offloaded through.
pub const ACTION_GET_TASK: &str = "cluster:monitor/task/get";

/// The action creating an index asks for.
pub const ACTION_CREATE_INDEX: &str = "indices:admin/create";

/// The action deleting an index asks for.
pub const ACTION_DELETE_INDEX: &str = "indices:admin/delete";

/// The action reading an index's own settings and mappings asks for.
pub const ACTION_GET_INDEX: &str = "indices:admin/get";

/// The action reading an index's own settings asks for — the monitoring action the
/// engine's get-settings API is registered under.
pub const ACTION_GET_INDEX_SETTINGS: &str = "indices:monitor/settings/get";

/// The action updating those settings asks for.
pub const ACTION_UPDATE_INDEX_SETTINGS: &str = "indices:admin/settings/update";

/// The action writing an index template asks for.
pub const ACTION_PUT_TEMPLATE: &str = "indices:admin/index_template/put";

/// The action reading an index template asks for.
pub const ACTION_GET_TEMPLATE: &str = "indices:admin/index_template/get";

/// The action changing the security configuration itself asks for — the one only the
/// master user holds, which is why the security API is reachable by nobody else.
pub const ACTION_SECURITY_CONFIG: &str = "cluster:admin/security/config/update";

/// The action registering a snapshot REPOSITORY asks for, as the snapshot plugin's own
/// action names spell it.
pub const ACTION_PUT_REPOSITORY: &str = "cluster:admin/repository/put";

/// The action reading a registered repository asks for.
pub const ACTION_GET_REPOSITORY: &str = "cluster:admin/repository/get";

/// The action unregistering one asks for.
pub const ACTION_DELETE_REPOSITORY: &str = "cluster:admin/repository/delete";

/// The action taking a SNAPSHOT into a repository asks for.
pub const ACTION_CREATE_SNAPSHOT: &str = "cluster:admin/snapshot/create";

/// The action reading a snapshot of one asks for.
pub const ACTION_GET_SNAPSHOT: &str = "cluster:admin/snapshot/get";

/// The action deleting a snapshot asks for.
pub const ACTION_DELETE_SNAPSHOT: &str = "cluster:admin/snapshot/delete";

/// The action restoring the indexes of a snapshot asks for.
pub const ACTION_RESTORE_SNAPSHOT: &str = "cluster:admin/snapshot/restore";

/// The role the cluster grants every permission, from the security plugin's predefined
/// roles: `all_access`. It is the role the control plane binds a domain's MASTER USER to,
/// which is why the master is reported holding it.
pub const ALL_ACCESS_ROLE: &str = "all_access";

/// One user of a domain's INTERNAL USER DATABASE, as the security plugin's internal-user
/// API holds it.
///
/// A domain created with `AdvancedSecurityOptions$InternalUserDatabaseEnabled` keeps its
/// own users beside the IAM principals that reach it, and a request presenting HTTP Basic
/// credentials is authenticated against them ([`authentication`]). The master user the
/// create named is the first of them, held on the domain record itself because the control
/// plane — not this API — wrote it.
#[derive(Debug, Clone, Default)]
pub struct InternalUser {
    /// The user's name — unique in the database, so the name IS the key.
    pub name: String,
    /// The credential the user authenticates with ([`Secret`]).
    pub password: Secret,
    /// `backend_roles` — the external identities this user is also known by, which a role
    /// mapping may name it under.
    pub backend_roles: Vec<String>,
    /// `opendistro_security_roles` — the security roles the user holds DIRECTLY, without a
    /// role mapping naming it.
    pub security_roles: Vec<String>,
    /// `attributes` — kept so the API reports back what it was given.
    pub attributes: BTreeMap<String, String>,
}

/// A credential a domain holds only to COMPARE a presented one against.
///
/// A type of its own rather than a `String` for one reason: its `Debug` writes a fixed
/// placeholder, so a password cannot reach a log or a trace through the DERIVED `Debug` of
/// any record that holds it. The value is reachable from nowhere else — [`Self::matches`]
/// is the one thing anything does with it, and no read path reports it.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// The credential `value` is, as this domain will hold it.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Whether `presented` IS this credential.
    ///
    /// An EMPTY credential matches nothing, in either direction: a user the database holds
    /// no password for cannot be authenticated at all, and neither can one that presented
    /// no password.
    #[must_use]
    pub fn matches(&self, presented: &str) -> bool {
        !self.0.is_empty() && self.0 == presented
    }
}

impl ::std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// What the cluster's authentication layer makes of the credential ONE request presented.
///
/// Authentication comes before authorization: a request the layer refuses is answered
/// `401` and never reaches the decision [`refusal`] takes, and a request it authenticates
/// is decided for the user it authenticated AS rather than for any AWS principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authentication {
    /// The request presented no credential this layer reads, and the domain does not
    /// require one of it: it is decided for whatever principal the front door resolved.
    Unauthenticated,
    /// It authenticated as this user of the internal database.
    User(String),
    /// Refused: the request presented no credential at all to a cluster that admits none
    /// anonymously, or named a user the database does not hold, or not with that password.
    Refused,
}

/// Whether `domain` keeps an internal user database:
/// `AdvancedSecurityOptions$InternalUserDatabaseEnabled`, "true to enable the internal
/// user database".
#[must_use]
pub fn internal_user_database(domain: &Domain) -> bool {
    domain
        .advanced_security_options
        .as_ref()
        .and_then(|options| options.internal_user_database_enabled)
        == Some(true)
}

/// What the cluster makes of the credential a request to `domain` presented.
///
/// `presented` is the user name and password of an HTTP Basic credential, where the
/// request carried one, and `carries_aws_credential` whether it instead presented an AWS
/// one — which the front door has already authenticated by verifying its signature.
///
/// A domain without fine-grained access control runs no security plugin, so it
/// authenticates nobody and nothing is refused here: such a request is decided by IAM
/// exactly as it was before. On a domain that has the boundary switched on every request
/// must arrive as SOMEBODY — that is what a cluster behind fine-grained access control
/// requires — so one presenting neither credential is refused, and one presenting Basic
/// credentials is admitted only as a user the database holds, at that user's own password.
#[must_use]
pub fn authentication(
    domain: &Domain,
    presented: Option<(&str, &str)>,
    carries_aws_credential: bool,
) -> Authentication {
    if !enforced(domain) {
        return Authentication::Unauthenticated;
    }
    let Some((name, password)) = presented else {
        if carries_aws_credential {
            return Authentication::Unauthenticated;
        }
        return Authentication::Refused;
    };
    // An empty password authenticates nobody, and neither does a user the database holds
    // none for: the comparison [`Secret::matches`] performs refuses both.
    let master = domain
        .master_user_name
        .as_deref()
        .zip(domain.master_user_password.as_ref());
    if master.is_some_and(|(held, secret)| held == name && secret.matches(password)) {
        return Authentication::User(name.to_string());
    }
    match domain.internal_users.get(name) {
        Some(user) if user.password.matches(password) => Authentication::User(name.to_string()),
        _ => Authentication::Refused,
    }
}

/// One role of a domain's security configuration, as its security API holds it.
///
/// The definition is kept VERBATIM as the document that was written, because the API
/// reports back what it was given; what is READ out of it is what holding the role is worth
/// — the actions and index patterns it permits a MAPPED principal, and the documents and
/// fields of those indexes it leaves visible ([`super::fgac_permissions`]).
#[derive(Debug, Clone)]
pub struct SecurityRole {
    /// The role's name — unique in the configuration, so the name IS the key.
    pub name: String,
    /// The role document as it was written.
    pub definition: ::serde_json::Value,
}

/// One role MAPPING of a domain's security configuration: which principals hold a role.
#[derive(Debug, Clone, Default)]
pub struct SecurityRoleMapping {
    /// The role this mapping binds principals to.
    pub role: String,
    /// `backend_roles` — the external identities the authentication backend asserts,
    /// which for a request signed with AWS credentials are the IAM identities the
    /// credential speaks for. Each entry may carry the `*` and `?` wildcards.
    pub backend_roles: Vec<String>,
    /// `users` — the subject names the mapping binds directly.
    pub users: Vec<String>,
    /// `hosts` — kept so the API reports back what it was given.
    pub hosts: Vec<String>,
}

/// Whether fine-grained access control is the authorization boundary on `domain`:
/// `AdvancedSecurityOptions$Enabled`. A domain that carries no such options, or carries
/// them switched off, is decided by IAM alone.
#[must_use]
pub fn enforced(domain: &Domain) -> bool {
    domain
        .advanced_security_options
        .as_ref()
        .and_then(|options| options.enabled)
        == Some(true)
}

/// The identities a request signed as `principal_arn` is matched by: the principal itself,
/// and — for an assumed-role session — the IAM ROLE it is a session of, which is the
/// identity a `backend_roles` entry names.
///
/// A session ARN is `arn:<partition>:sts::<account>:assumed-role/<RoleName>/<SessionName>`
/// and the role it names is `arn:<partition>:iam::<account>:role/<RoleName>`, which is the
/// value a client puts in a role mapping because it is the value it created the role with.
#[must_use]
pub fn identities(principal_arn: &str) -> Vec<String> {
    let mut identities = vec![principal_arn.to_string()];
    if let Some(parsed) = ::service_common::arn::parse(principal_arn) {
        if parsed.vendor == "sts" {
            if let Some(role_name) = parsed
                .resource
                .strip_prefix("assumed-role/")
                .and_then(|rest| rest.split('/').next())
                .filter(|name| !name.is_empty())
            {
                identities.push(format!(
                    "arn:{}:iam::{}:role/{role_name}",
                    parsed.partition, parsed.account
                ));
            }
        }
    }
    identities
}

/// Whether `principal_arn` is the master user the domain was created with — the principal
/// fine-grained access control grants everything, and the only one the security API itself
/// is reachable by.
#[must_use]
pub fn is_master(domain: &Domain, principal_arn: &str) -> bool {
    let identities = identities(principal_arn);
    let named = |configured: &String| {
        identities
            .iter()
            .any(|identity| identity.eq_ignore_ascii_case(configured))
    };
    domain.master_user_arn.as_ref().is_some_and(named)
        || domain.master_user_name.as_ref().is_some_and(named)
}

/// The roles this domain's role mappings bind `principal_arn` to, in configuration order.
///
/// A user of the internal database may also hold roles DIRECTLY — the
/// `opendistro_security_roles` its own record names — so those are reported beside the
/// mapped ones: the plugin grants a user the union of both.
#[must_use]
pub fn mapped_roles(domain: &Domain, principal_arn: &str) -> Vec<String> {
    let identities = identities(principal_arn);
    let mut roles: Vec<String> = domain
        .security_role_mappings
        .values()
        .filter(|mapping| {
            mapping
                .backend_roles
                .iter()
                .chain(mapping.users.iter())
                .any(|pattern| {
                    identities
                        .iter()
                        .any(|identity| wildcard_match(pattern, identity, true))
                })
        })
        .map(|mapping| mapping.role.clone())
        .collect();
    if let Some(user) = domain.internal_users.get(principal_arn) {
        for role in &user.security_roles {
            if !roles.contains(role) {
                roles.push(role.clone());
            }
        }
    }
    roles
}

/// The reason fine-grained access control refuses `principal_arn` the `action` on the index
/// `index` names, or `None` when it permits it.
///
/// `None` for every domain the boundary is not switched on for, so a domain without
/// fine-grained access control is decided by IAM exactly as it was before.
///
/// Two narrowings, in the order the plugin takes them: a principal no role mapping binds
/// and that holds no role of its own may do nothing at all, and a principal its roles DO
/// bind may do only what one of those roles permits — the action groups and index patterns
/// [`super::fgac_permissions`] reads off the role documents the security API stored.
///
/// `index` is the index the request's own path addresses, and `None` for a request that
/// addresses the cluster rather than an index.
#[must_use]
pub fn refusal(
    domain: &Domain,
    principal_arn: &str,
    action: &str,
    index: Option<&str>,
) -> Option<String> {
    if !enforced(domain) || is_master(domain, principal_arn) {
        return None;
    }
    let roles = mapped_roles(domain, principal_arn);
    if roles.is_empty() {
        return Some(no_permissions(principal_arn, action));
    }
    if super::fgac_permissions::permitted(domain, &roles, action, index) {
        return None;
    }
    Some(no_permissions(principal_arn, action))
}

/// The reason only the master user may change the security configuration, or `None` when
/// `principal_arn` IS that master user.
#[must_use]
pub fn security_api_refusal(domain: &Domain, principal_arn: &str) -> Option<String> {
    (!is_master(domain, principal_arn))
        .then(|| no_permissions(principal_arn, ACTION_SECURITY_CONFIG))
}

/// How the security plugin states a request it does not permit: the action it refused and
/// the user it refused it for, with that user's own name and the backend roles it arrived
/// with. Every value comes from the request.
#[must_use]
pub fn no_permissions(principal_arn: &str, action: &str) -> String {
    format!(
        "no permissions for [{action}] and {}",
        user_description(principal_arn)
    )
}

/// How the security plugin DESCRIBES the user a request speaks for: its own name, the
/// backend roles it arrived with and the tenant it asked for.
///
/// One wording, because two answers carry it: the `security_exception` a refusal states
/// ([`no_permissions`]) and the `user` the plugin's own `authinfo` reports
/// ([`crate::service::domain_rest`]) — so what a caller is told it is cannot differ
/// between the two.
#[must_use]
pub fn user_description(principal_arn: &str) -> String {
    format!(
        "User [name={principal_arn}, backend_roles=[{}], requestedTenant=null]",
        backend_roles(principal_arn).join(", ")
    )
}

/// The backend roles a request signed as `principal_arn` arrives with: the identities the
/// authentication backend asserts for it BESIDE its own name ([`identities`]), which for
/// an AWS principal is the IAM role a session speaks for — the identity a role mapping is
/// written with.
#[must_use]
pub fn backend_roles(principal_arn: &str) -> Vec<String> {
    identities(principal_arn).into_iter().skip(1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a request signed as an assumed-role session is matched by its own ARN AND by
    /// the IAM role it is a session of, which is the identity a role mapping names.
    #[test]
    fn a_session_is_matched_by_the_role_it_is_a_session_of() {
        // (the case, the principal, the identities it is matched by)
        let rows: &[(&str, &str, &[&str])] = &[
            (
                "an assumed-role session",
                "arn:aws:sts::000000000000:assumed-role/firehose-delivery/session-1",
                &[
                    "arn:aws:sts::000000000000:assumed-role/firehose-delivery/session-1",
                    "arn:aws:iam::000000000000:role/firehose-delivery",
                ],
            ),
            (
                "an IAM role, as a producer writes as",
                "arn:aws:iam::000000000000:role/firehose-delivery",
                &["arn:aws:iam::000000000000:role/firehose-delivery"],
            ),
            (
                "an IAM user",
                "arn:aws:iam::000000000000:user/analyst",
                &["arn:aws:iam::000000000000:user/analyst"],
            ),
            ("a value that is no ARN", "master", &["master"]),
        ];
        for (case, principal, expected) in rows {
            let answered = identities(principal);
            assert_eq!(
                answered, *expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }

    /// RULE: the refusal names the action it refused and the user it refused it for, with
    /// the backend roles that user arrived with.
    #[test]
    fn the_refusal_names_the_action_and_the_user() {
        let message = no_permissions(
            "arn:aws:sts::000000000000:assumed-role/firehose-delivery/session-1",
            ACTION_INDEX_DOCUMENT,
        );
        for part in [
            "no permissions for [indices:data/write/index]",
            "name=arn:aws:sts::000000000000:assumed-role/firehose-delivery/session-1",
            "backend_roles=[arn:aws:iam::000000000000:role/firehose-delivery]",
            "requestedTenant=null",
        ] {
            assert!(
                message.contains(part),
                "case {part:?}: expected the refusal to carry it, got {message:?}"
            );
        }
    }

    /// RULE: the user the plugin describes carries the same name and backend roles a
    /// refusal names, so `authinfo` and a `security_exception` cannot disagree about who
    /// the caller is.
    #[test]
    fn the_user_description_is_the_one_a_refusal_names() {
        // (the case, the principal, the backend roles it arrives with)
        let rows: &[(&str, &str, &[&str])] = &[
            (
                "an assumed-role session",
                "arn:aws:sts::000000000000:assumed-role/search-reader/session-1",
                &["arn:aws:iam::000000000000:role/search-reader"],
            ),
            (
                "an IAM role",
                "arn:aws:iam::000000000000:role/search-reader",
                &[],
            ),
        ];
        for (case, principal, expected) in rows {
            let described = user_description(principal);
            assert!(
                no_permissions(principal, ACTION_SEARCH).contains(&described),
                "case {case:?}: expected the refusal to carry the description \
                 {described:?}"
            );
            let answered = backend_roles(principal);
            assert_eq!(
                answered, *expected,
                "case {case:?}: expected the backend roles {expected:?}, got {answered:?}"
            );
        }
    }
}
