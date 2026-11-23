// SPDX-License-Identifier: CC0-1.0

//! WHO a request to a cluster's Kubernetes API server speaks for.
//!
//! An IAM principal reaches a cluster's Kubernetes API with a bearer token it builds
//! out of its own AWS credential — the `k8s-aws-v1.` token a kubeconfig's
//! credential plugin produces, which carries a presigned
//! `sts:GetCallerIdentity` request — and the cluster maps that principal to a
//! Kubernetes identity through its ACCESS ENTRIES: `AccessEntry$username` is "the
//! name of a user that can authenticate to your cluster" and
//! `AccessEntry$kubernetesGroups` "the value of `kubernetesGroups` is a list of
//! Kubernetes groups that the principal is a member of". So the username and the
//! groups a request is authorized and audited under are the ENTRY's, never anything
//! derived from the request's own text.
//!
//! Which IAM principal the credential speaks for is the same question
//! `sts:GetCallerIdentity` answers, resolved from the same registries
//! ([`super::request_caller_principal_arn`] answers it for a control-plane call):
//! the session the access key id was minted for — a role session speaks for its
//! ROLE, the permanent principal an access entry can be filed under — else the IAM
//! user that holds the long-term key, else the credential this emulator publishes at
//! start.
//!
//! A Kubernetes API server AUTHENTICATES every request it serves, and the credential
//! of a request to this one is the bearer token it carries and nothing else: a
//! request with no `Authorization` header presents no credential, so it speaks for
//! NOBODY rather than for whichever principal an unsigned AWS call would be
//! attributed to. An anonymous request is answered the `401 Unauthorized` below
//! before anything is routed.
//!
//! A token is bound to ONE cluster: the plugin signs the cluster's name into the
//! presigned request under `x-k8s-aws-id` ([`CLUSTER_ID_HEADER`]), so a token built
//! for another cluster speaks for nobody at this one however good its credential is.
//!
//! A principal NEITHER mapping of the cluster names authenticates as nobody too, and
//! is answered that same `401 Unauthorized`.
//!
//! There are two mappings, and [`authenticate`] consults them in the order Amazon
//! EKS does: the cluster's ACCESS ENTRIES, then its `aws-auth` ConfigMap for a
//! cluster whose `authenticationMode` includes `CONFIG_MAP`
//! ([`super::kubernetes_aws_auth`]).

use super::kubernetes_objects::ApiRefusal;
use super::kubernetes_rbac::{AccessPolicyGrant, SubjectIdentity};
use super::ComAmazonawsEksAwswesleyfrontendState as ServiceStore;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::std::time::SystemTime;

/// The prefix the bearer token of an EKS kubeconfig carries, and the version of that
/// token this API server reads.
const EKS_BEARER_TOKEN_PREFIX: &str = "k8s-aws-v1.";

/// The scheme a bearer token is presented under in the `Authorization` header.
const BEARER_SCHEME: &str = "Bearer ";

/// The name the CLUSTER a bearer token was built for is bound into the token under.
///
/// A kubeconfig's credential plugin signs `x-k8s-aws-id` into the presigned
/// `sts:GetCallerIdentity` request the token carries, so one token is usable at one
/// cluster: that is what stops a token built for one cluster from being replayed at
/// another. A client that names the cluster beside the token sends it under the same
/// name.
pub const CLUSTER_ID_HEADER: &str = "x-k8s-aws-id";

/// What one presented bearer token names: the credential it was built from, and the
/// cluster it was built FOR when the token names one.
#[derive(Debug, Clone)]
pub struct PresentedToken {
    /// The access key id the token's presigned URL is scoped to.
    pub access_key_id: String,
    /// The cluster the token is bound to ([`CLUSTER_ID_HEADER`]), and `None` for a
    /// token whose URL names none.
    pub cluster_name: Option<String>,
}

/// The registries that answer which IAM principal an access key id speaks for, held
/// for as long as the cluster's API address is open.
///
/// They are the stores of the OTHER services in the same shared engine — the session
/// registry STS writes and the users and roles IAM holds — so the API server resolves
/// a credential against the very records `sts:AssumeRole` and `iam:CreateUser` wrote,
/// and an emulator that mounts neither resolves every credential to the published
/// one.
#[derive(Clone)]
pub struct ApiServerPrincipals {
    sessions: Option<::engine::ServiceState<sts_store::StsState>>,
    identities: Option<::engine::ServiceState<sts_store::IamState>>,
}

/// Written by hand rather than derived: what is diagnosable is whether the registries
/// are there at all, not what they hold.
impl ::std::fmt::Debug for ApiServerPrincipals {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("ApiServerPrincipals")
            .field("sessions", &self.sessions.is_some())
            .field("identities", &self.identities.is_some())
            .finish()
    }
}

impl ApiServerPrincipals {
    /// The registries in `engine`.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Self {
        Self {
            sessions: sts_store::service_state(engine),
            identities: sts_store::iam_state(engine),
        }
    }

    /// WHO the access key id `access_key_id` speaks for, and `None` for a
    /// credential that speaks for nobody — one naming a session whose window has
    /// run out.
    ///
    /// `access_key_id` is the credential a request PRESENTED
    /// ([`presented_access_key_id`]); a request that presented none never reaches
    /// here, because it authenticates as nobody.
    ///
    /// Reading only: no session is minted, moved or pruned here — a request to a
    /// cluster's API server is not what expires a credential. Call it with no store
    /// of THIS service held: it takes the other services' locks.
    #[must_use]
    pub fn caller(&self, access_key_id: &str, now: SystemTime) -> Option<ApiCaller> {
        if let Some(sessions) = &self.sessions {
            let store = sessions.lock();
            if let Some(session) = store.session(access_key_id) {
                if session.is_expired(now) {
                    // The credential named a session that has run out: it speaks for
                    // nobody rather than for the identity behind it.
                    return None;
                }
                // "The temporary security credentials … have the permissions of the
                // role": the permanent principal behind a role session is that role,
                // and the role is what an access entry is filed under. The session's
                // OWN principal is what the caller presents, which is the identity
                // the API server names it by.
                let presented = session.identity.arn.clone();
                return Some(ApiCaller::of(
                    session.role_arn.clone().unwrap_or_else(|| presented.clone()),
                    Some(presented),
                ));
            }
        }
        let identity = match &self.identities {
            Some(identities) => {
                sts_store::long_term_identity(Some(&identities.lock()), access_key_id)
            }
            None => sts_store::long_term_identity(None, access_key_id),
        };
        Some(ApiCaller::of(identity.arn, None))
    }
}

/// WHO one request to a cluster's Kubernetes API server speaks for.
///
/// Two ARNs, because an IAM principal and the identity it PRESENTS are not always
/// the same: a role session's permanent principal is the role — the principal an
/// access entry and an `aws-auth` mapping are filed under — while the identity it
/// presents is the session itself, which is the name the API server knows it by
/// ([`super::access_entry_usernames`]).
#[derive(Debug, Clone)]
pub struct ApiCaller {
    /// The permanent IAM principal the credential speaks for: a role, an IAM
    /// user, or whatever other principal the registries resolved it to.
    pub principal_arn: String,
    /// The name of the assumed-role session the credential was minted for, and
    /// `None` for a long-term credential, which names no session.
    pub session_name: Option<String>,
    /// The account that principal belongs to, as its ARN names it.
    pub account_id: String,
}

impl ApiCaller {
    /// The caller behind `principal_arn`, which `presented_arn` is the session
    /// principal of when the credential was a session's.
    fn of(principal_arn: String, presented_arn: Option<String>) -> Self {
        let session_name = presented_arn.as_deref().and_then(|presented| {
            super::access_entry_usernames::assumed_role_session_name(presented)
                .map(str::to_string)
        });
        Self {
            account_id: super::access_entry_usernames::arn_account_id(&principal_arn).to_string(),
            principal_arn,
            session_name,
        }
    }

    /// `template` as the username THIS caller authenticates under: the
    /// placeholders Amazon EKS resolves per request, filled in from the
    /// credential this request presented.
    fn username(&self, template: &str) -> String {
        super::access_entry_usernames::resolved_username(
            template,
            self.session_name.as_deref(),
            &self.account_id,
        )
    }
}

/// What the `Authorization` header of a Kubernetes API request PRESENTS, and `None`
/// for a header this server cannot read a credential out of at all.
///
/// The token is `k8s-aws-v1.` followed by the base64url form of a presigned
/// `sts:GetCallerIdentity` URL, so the credential it was built from is the one that
/// URL's own SigV4 credential scope names — read through the SAME readers every
/// service in this tree reads them with: `service_common::jwt::decode_base64url` for
/// the segment, the one base64url codec of this workspace, and
/// `service_common::request_access_key_id` for the scope, so a token and a presigned
/// call resolve to one credential. The CLUSTER the token was built for is read off
/// the same URL ([`CLUSTER_ID_HEADER`]).
///
/// A request carrying NO `Authorization` header presents no credential at all, and a
/// header that IS there but carries no credential this server can read — another
/// scheme, a token of another version, a segment that is not base64url, a URL with no
/// credential scope — presents nothing either. Both are answered `None`, which the
/// caller turns into the `401 Unauthorized` the API server answers a request it cannot
/// authenticate: the Kubernetes API is reached with a bearer token, so a request that
/// carries none speaks for nobody rather than for the principal an unsigned AWS call
/// would be attributed to.
#[must_use]
pub fn presented_token(authorization: Option<&str>) -> Option<PresentedToken> {
    let header = authorization?;
    let token = header
        .strip_prefix(BEARER_SCHEME)
        .and_then(|token| token.trim().strip_prefix(EKS_BEARER_TOKEN_PREFIX))?;
    let url = ::service_common::jwt::decode_base64url(token)
        .and_then(|bytes| String::from_utf8(bytes).ok())?;
    let (parts, ()) = ::http::Request::builder()
        .uri(url)
        .body(())
        .ok()?
        .into_parts();
    let access_key_id = ::service_common::request_access_key_id(&parts);
    // The scope the presigned URL carries, and nothing when it carries none: that URL
    // is the whole credential the token presents.
    if access_key_id == no_credential_key_id() {
        return None;
    }
    let cluster_name = parts.uri.query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            name.eq_ignore_ascii_case(CLUSTER_ID_HEADER)
                .then(|| value.to_string())
        })
    });
    Some(PresentedToken {
        access_key_id,
        cluster_name,
    })
}

/// The sentinel the one credential reader of this workspace answers for a request
/// that carries no credential scope, taken from that reader rather than restated
/// here: a presigned URL whose scope reads as this names no credential, so the token
/// built from it presents none.
fn no_credential_key_id() -> String {
    let (parts, ()) = ::http::Request::new(()).into_parts();
    ::service_common::request_access_key_id(&parts)
}

/// The Kubernetes identity the cluster keyed `key` maps the calling IAM principal
/// to, or the `401 Unauthorized` a principal the cluster maps to nobody is answered
/// with.
///
/// The cluster has two mappings, and they are consulted in the order Amazon EKS
/// consults them: the ACCESS ENTRY for the principal first, then — for a cluster
/// whose `authenticationMode` includes `CONFIG_MAP` — the `aws-auth` ConfigMap
/// ([`super::kubernetes_aws_auth`]). So an access entry for a principal takes
/// precedence over a ConfigMap mapping of the same principal, and deleting that
/// entry falls back to the ConfigMap; a cluster in `API` mode alone has no
/// ConfigMap mapping to fall back to.
///
/// Either way the username is the mapping's own, with the placeholders Amazon EKS
/// resolves per request filled in from the credential THIS request presented
/// ([`ApiCaller::username`]).
pub fn authenticate(
    store: &ServiceStore,
    key: &(String, String),
    caller: &ApiCaller,
) -> Result<SubjectIdentity, ApiRefusal> {
    let cluster = store.clusters.get(key).ok_or_else(ApiRefusal::unauthorized)?;
    if let Some(entry) = cluster.access_entries.get(&caller.principal_arn) {
        let grants = entry
            .associated_policies
            .values()
            .filter_map(|association| {
                Some(AccessPolicyGrant {
                    policy_name: super::access_policy_name(&association.policy_arn)?.to_string(),
                    namespaces: (association.scope_type == super::ACCESS_SCOPE_TYPE_NAMESPACE)
                        .then(|| association.namespaces.clone()),
                })
            })
            .collect();
        return Ok(SubjectIdentity {
            username: caller.username(&entry.username),
            groups: SubjectIdentity::authenticated_groups(&entry.kubernetes_groups),
            grants,
            principal_arn: caller.principal_arn.clone(),
            session_name: caller.session_name.clone(),
        });
    }
    if cluster.authenticates_through_config_map() {
        if let Some(mapping) = store
            .kubernetes_api
            .get(key)
            .and_then(|plane| plane.objects.aws_auth_mapping(&caller.principal_arn))
        {
            return Ok(SubjectIdentity {
                username: caller.username(&mapping.username),
                groups: SubjectIdentity::authenticated_groups(&mapping.groups),
                // The ConfigMap maps a principal to a username and groups and
                // nothing else: EKS access policies are associated to an access
                // ENTRY, so a principal mapped this way carries none and what it
                // may do is decided by the cluster's own RBAC objects alone.
                grants: Vec::new(),
                principal_arn: caller.principal_arn.clone(),
                session_name: caller.session_name.clone(),
            });
        }
    }
    Err(ApiRefusal::unauthorized())
}
