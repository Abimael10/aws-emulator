// SPDX-License-Identifier: CC0-1.0

//! WHO a request to a GraphQL API's `GRAPHQL` endpoint speaks for, and what IAM lets that
//! principal do with the API's fields.
//!
//! Two things rest on the same answer, which is why they are read in one place:
//!
//! 1. **`$context.identity`** — "an object that contains information about the caller"
//!    (AppSync's resolver mapping-template context reference). For the `AWS_IAM`
//!    authorization mode it carries the account and the user the call signed as, and — for
//!    a call signed with credentials an identity pool vended — the pool, the identity and
//!    the authentication type that exchange was made under, which is what a resolver echoes
//!    to prove whose call it is resolving.
//! 2. **the authorization of each FIELD the call selects**. With `AWS_IAM`, "you can use
//!    IAM policies to restrict access to … individual fields" of an API: the action is
//!    `appsync:GraphQL` and the resource a field is addressed by is the API's own ARN
//!    extended with `/types/<typeName>/fields/<fieldName>`, so a policy may grant the whole
//!    API with a wildcard and `Deny` one field of it — and the explicit `Deny` wins, as it
//!    does everywhere in IAM.
//!
//! The three pieces of the decision are the ones this service's control-plane gate names
//! ([`crate::service::authorization`]): WHO the request's access key id speaks for, WHAT
//! the IAM store holds for that principal, and the DECISION itself, which is
//! [`::service_common::iam_authorization`] — the one shared evaluation, so a policy written
//! about this API's fields is read here exactly as a policy about its control-plane calls is
//! read there.
//!
//! The material is taken as PRESENTED: this emulator verifies request signatures at its
//! front door and a service never checks one itself, and this socket is not that door (the
//! same reading [`crate::service::graphql_api::authorized`] applies to every provider's
//! material). Nothing here WRITES: the session registry is read, never swept, so a call on
//! this endpoint changes nothing another caller would see.

use ::serde_json::{Map as JsonMap, Value as Json};
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;
use ::service_common::iam_authorization::{self as iam, Decision};

/// The authorization MODE this module decides for: "AWS_IAM" of `AuthenticationType`, where
/// "the request is signed with AWS Signature Version 4 and the caller's IAM policies decide
/// the call".
pub const IAM_AUTH_TYPE: &str = "AWS_IAM";

/// The action an IAM policy grants to let a call reach a GraphQL API's data plane:
/// `appsync:GraphQL`, the one action AppSync publishes for its data plane, under this
/// service's own action prefix.
pub const GRAPHQL_ACTION: &str = "appsync:GraphQL";

/// The scheme a SigV4-signed request presents its material under, which is what says the
/// call is one the `AWS_IAM` mode decides at all.
const SIGV4_SCHEME: &str = "AWS4-HMAC-SHA256";

/// The claim keys a Cognito Identity exchange publishes about the token it swapped for the
/// session a call is signed with: the identity POOL the session was vended for, the IDENTITY
/// it was vended to, and the authentication methods the exchange was made under. They are
/// the `cognito-identity.amazonaws.com` condition keys a role's trust policy is written
/// over, and they travel with the session, so the API's data plane reads the very values the
/// exchange published.
const IDENTITY_POOL_CLAIM: &str = "cognito-identity.amazonaws.com:aud";
const IDENTITY_CLAIM: &str = "cognito-identity.amazonaws.com:sub";
const AUTHENTICATION_METHODS_CLAIM: &str = "cognito-identity.amazonaws.com:amr";

/// The two authentication types an identity pool's exchange is made under, which is what
/// `cognitoIdentityAuthType` reports.
const AUTHENTICATED: &str = "authenticated";
const UNAUTHENTICATED: &str = "unauthenticated";

/// The issuer of the token an identity pool's exchange swaps, which every such exchange is
/// made through and which therefore identifies no PROVIDER of the user.
const TOKEN_ISSUER: &str = "cognito-identity.amazonaws.com";

/// Who a signed request speaks for, and what the account's policies let it do.
#[derive(Debug, Clone)]
pub struct SignedCaller {
    /// What a resolver reads as `$context.identity`.
    pub identity: Json,
    /// Every spelling of the principal a statement's `Principal` element may name.
    principals: Vec<String>,
    /// The identity-based policy documents in force on it.
    documents: Vec<String>,
    /// The permissions boundary that caps them, where it carries one.
    boundary: Option<String>,
    /// Set when nothing the identity-based decision can read bounds this caller: the
    /// account's own published credential, or a principal holding an attachment whose
    /// document this emulator does not carry (the two cases
    /// [`crate::service::authorization`] states in full).
    unbounded: bool,
}

impl SignedCaller {
    /// The caller a credential that speaks for NOBODY presents: no principal a statement can
    /// name, no document in force, and nothing that lifts IAM's default deny — so every
    /// action it asks for is refused, and `$context.identity` reports nothing of it.
    fn nobody() -> Self {
        Self {
            identity: Json::Null,
            principals: Vec::new(),
            documents: Vec::new(),
            boundary: None,
            unbounded: false,
        }
    }
}

/// The facts about the session an access key id names, copied out under one read so no store
/// guard is held while another is taken.
struct SessionFacts {
    identity: sts_store::Identity,
    role_arn: Option<String>,
    claims: Vec<(String, String)>,
}

/// Who the request `headers` present, or `None` for a request that presents no SigV4
/// material at all — which is every call made under another authorization mode, and which
/// `AWS_IAM` has nothing to decide about.
///
/// A credential that presents material but speaks for NOBODY — a session the clock has
/// already closed — is not that case: it is a caller carrying no principal and no policy, so
/// every action it asks for falls to IAM's default deny rather than going undecided.
#[must_use]
pub fn presented_caller(
    engine: &::engine::InMemoryEngine,
    headers: &::http::HeaderMap,
) -> Option<SignedCaller> {
    let presented = headers
        .get(::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;
    if !presented.starts_with(SIGV4_SCHEME) {
        return None;
    }
    let access_key_id = ::engine::extract_akid(headers);
    let now = ::std::time::SystemTime::now();
    let session = match sts_store::service_state(engine) {
        Some(registry) => {
            let store = registry.lock();
            match store.session(&access_key_id) {
                // The window this credential was minted for has closed, so it stands for no
                // principal: no spelling a statement can name, and no document in force.
                Some(session) if session.is_expired(now) => return Some(SignedCaller::nobody()),
                Some(session) => Some(SessionFacts {
                    identity: session.identity.clone(),
                    role_arn: session.role_arn.clone(),
                    claims: session.federated_claims.clone(),
                }),
                None => None,
            }
        }
        None => None,
    };
    let ambient = sts_store::ambient_identity();
    let mut caller = SignedCaller {
        identity: Json::Null,
        principals: Vec::new(),
        documents: Vec::new(),
        boundary: None,
        unbounded: false,
    };
    let mut identity = session
        .as_ref()
        .map_or_else(|| ambient.clone(), |facts| facts.identity.clone());
    let role_arn = session.as_ref().and_then(|facts| facts.role_arn.clone());
    let claims = session.map(|facts| facts.claims).unwrap_or_default();

    if let Some(iam) = iam_store::service_state(engine) {
        let store = iam.lock();
        // A LONG-TERM access key belongs to an IAM user, and IAM's store is what maps one to
        // its owner; a key no session and no user holds really is the account's own
        // published credential.
        if role_arn.is_none() && identity.arn == ambient.arn {
            if let Some(user) = store
                .access_keys
                .get(&access_key_id)
                .and_then(|record| store.users.get(&record.user_key))
            {
                identity = sts_store::Identity {
                    user_id: user.user_id.clone(),
                    account: ::engine::emulator_account_id().to_string(),
                    arn: user.arn.clone(),
                };
            }
        }
        caller.unbounded = identity.arn == ambient.arn && role_arn.is_none();
        caller.principals = vec![identity.arn.clone()];
        // An assumed-role session's permissions are the ROLE's, addressed by the ARN the
        // account holds the role under; an IAM user's are its own.
        let policies_arn = match &role_arn {
            Some(role_arn) => {
                caller.principals.push(role_arn.clone());
                Some(role_arn.clone())
            }
            None => ::service_common::iam_role::user_name(&identity.arn)
                .map(|_| identity.arn.clone()),
        };
        if let Some(policies_arn) = policies_arn {
            if let Some(policies) = store.principal_policies(&policies_arn) {
                caller.documents = policies.documents;
                // An attachment the collection resolved no document for is an entry of
                // AWS's own catalog this emulator does not carry: a grant it cannot read.
                if !policies.uncarried.is_empty() {
                    caller.unbounded = true;
                }
            }
            caller.boundary = store.permissions_boundary_document(&policies_arn);
        }
    } else {
        caller.principals = vec![identity.arn.clone()];
        caller.unbounded = identity.arn == ambient.arn && role_arn.is_none();
    }

    caller.identity = reported_identity(&identity, &claims, headers);
    Some(caller)
}

/// What a resolver reads as `$context.identity` for this caller.
///
/// Every member comes from the credential the call was signed with: the account and the
/// principal the access key id resolves to, and the pool, the identity and the
/// authentication type the exchange that vended it published. A call signed with a
/// credential no identity pool vended carries none of the Cognito members, because no
/// exchange published them.
fn reported_identity(
    identity: &sts_store::Identity,
    claims: &[(String, String)],
    headers: &::http::HeaderMap,
) -> Json {
    let claimed = |key: &str| {
        claims
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    let methods: Vec<&str> = claims
        .iter()
        .filter(|(name, _)| name == AUTHENTICATION_METHODS_CLAIM)
        .map(|(_, value)| value.as_str())
        .collect();
    let mut reported = JsonMap::new();
    reported.insert(
        "accountId".to_string(),
        Json::String(identity.account.clone()),
    );
    reported.insert(
        USER_ARN_MEMBER.to_string(),
        Json::String(identity.arn.clone()),
    );
    // "In the case of AWS_IAM authorization, the value of username is the value of the AWS
    // user principal", which is the caller's own user id.
    reported.insert(
        "username".to_string(),
        Json::String(identity.user_id.clone()),
    );
    if let Some(source_ip) = source_ip(headers) {
        reported.insert(
            "sourceIp".to_string(),
            Json::Array(vec![Json::String(source_ip)]),
        );
    }
    if let Some(pool) = claimed(IDENTITY_POOL_CLAIM) {
        reported.insert("cognitoIdentityPoolId".to_string(), Json::String(pool));
    }
    if let Some(identity_id) = claimed(IDENTITY_CLAIM) {
        reported.insert("cognitoIdentityId".to_string(), Json::String(identity_id));
    }
    if !methods.is_empty() {
        let authenticated = !methods.contains(&UNAUTHENTICATED);
        reported.insert(
            "cognitoIdentityAuthType".to_string(),
            Json::String(
                if authenticated {
                    AUTHENTICATED
                } else {
                    UNAUTHENTICATED
                }
                .to_string(),
            ),
        );
        // The providers the exchange was made under, beside the two authentication types
        // and the issuer every such exchange goes through: what is left is what
        // authenticated the user, and `cognitoIdentityAuthProvider` names them as the
        // authentication methods themselves are written — each quoted, in one
        // comma-separated value.
        let providers: Vec<String> = methods
            .iter()
            .copied()
            .filter(|method| ![AUTHENTICATED, UNAUTHENTICATED, TOKEN_ISSUER].contains(method))
            .map(|method| format!("\"{method}\""))
            .collect();
        if !providers.is_empty() {
            reported.insert(
                "cognitoIdentityAuthProvider".to_string(),
                Json::String(providers.join(",")),
            );
        }
    }
    Json::Object(reported)
}

/// The address the call came from, as `sourceIp` reports one: the address a proxy declares
/// for it, else the address the client dialled this socket at, which on a loopback listener
/// is the one it came from. `None` when the request states neither, which leaves the member
/// off rather than inventing one.
pub(super) fn source_ip(headers: &::http::HeaderMap) -> Option<String> {
    let field = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let forwarded = field("x-forwarded-for").and_then(|value| {
        value
            .split(',')
            .map(str::trim)
            .find(|address| address.parse::<::std::net::IpAddr>().is_ok())
            .map(str::to_string)
    });
    forwarded.or_else(|| {
        let host = field("host")?;
        let address = host.rsplit_once(':').map_or(host.clone(), |(head, _)| {
            head.trim_start_matches('[').trim_end_matches(']').to_string()
        });
        address
            .parse::<::std::net::IpAddr>()
            .ok()
            .map(|parsed| parsed.to_string())
    })
}

/// How one field of one type of an API is ADDRESSED by an IAM policy: the API's own ARN,
/// extended with the type and the field — `…:apis/<apiId>/types/<typeName>/fields/<fieldName>`.
///
/// Built from the ARN the API is held under, so the partition, the Region and the account of
/// the resource a policy is written about are the API's own.
#[must_use]
pub fn field_resource(api_arn: &str, type_name: &str, field_name: &str) -> String {
    format!("{api_arn}/types/{type_name}/fields/{field_name}")
}

/// Whether the account's policies let this caller perform `appsync:GraphQL` on `resource`.
///
/// The decision is the shared one: an explicit `Deny` refuses whatever else the principal
/// carries, an `Allow` about the request grants it, neither falls to IAM's default deny, and
/// a permissions boundary caps the grant — so a policy that allows the whole API and denies
/// one field of it denies that field.
#[must_use]
pub fn permits(caller: &SignedCaller, resource: &str) -> bool {
    withholding(caller, GRAPHQL_ACTION, resource).is_none()
}

/// Which of IAM's rules withholds `action` on `resource` from this caller, as the reason
/// clause an authorization refusal states — `None` when the account's policies permit it.
///
/// The ONE decision ladder this endpoint takes about the caller, over any action: the fields
/// of the API itself ([`permits`]) and the call a resolver's data source makes UNDER THE
/// CALLER's own credentials ([`super::dynamodb`]) are decided by it, so the two cannot
/// disagree about what a principal may do. An explicit `Deny` refuses whatever else the
/// principal carries, an `Allow` about the request grants it, neither falls to IAM's default
/// deny, and a permissions boundary caps the grant — "a permissions boundary sets the maximum
/// permissions that an identity-based policy can grant", so a grant the boundary does not
/// also allow is not a permission.
#[must_use]
pub fn withholding(caller: &SignedCaller, action: &str, resource: &str) -> Option<String> {
    let request = iam::Request::new(action, resource, &caller.principals);
    let identity_said = iam::identity_decision(&caller.documents, &request);
    if identity_said == Decision::Denied {
        return Some(iam::IDENTITY_POLICY_EXPLICIT_DENY.to_string());
    }
    if !(iam::is_allowed(identity_said, Decision::NotStated) || caller.unbounded) {
        return Some(iam::no_identity_policy_allows(action));
    }
    let boundary = caller.boundary.as_ref()?;
    match iam::identity_decision(::std::slice::from_ref(boundary), &request) {
        Decision::Allowed => None,
        Decision::Denied => Some(iam::PERMISSIONS_BOUNDARY_EXPLICIT_DENY.to_string()),
        _ => Some(iam::no_permissions_boundary_allows(action)),
    }
}

impl SignedCaller {
    /// The principal an authorization refusal of this caller NAMES: the ARN the credential it
    /// signed with speaks for, which is the value `$context.identity` reports as `userArn`.
    #[must_use]
    pub fn principal(&self) -> String {
        self.identity
            .get(USER_ARN_MEMBER)
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The member of `$context.identity` carrying the principal the call signed as.
const USER_ARN_MEMBER: &str = "userArn";

#[cfg(test)]
mod tests {
    use super::*;

    /// The material a case presents, as a request's own fields carry it.
    fn presented(authorization: Option<&str>) -> ::http::HeaderMap {
        let mut headers = ::http::HeaderMap::new();
        if let Some(value) = authorization {
            headers.insert(
                ::http::header::AUTHORIZATION,
                ::http::HeaderValue::from_str(value).expect("the case's own value"),
            );
        }
        headers
    }

    /// Only a call presenting SigV4 material is one the `AWS_IAM` mode decides: material of
    /// another provider, and none at all, name no principal for it to judge.
    #[test]
    fn only_a_signed_call_names_a_principal_to_judge() {
        let engine = ::engine::InMemoryEngine::new();
        let cases: &[(&str, Option<&str>, bool)] = &[
            ("no material at all", None, false),
            ("an OIDC or user-pool token", Some("eyJraWQiOiJrZXkifQ.e30.c2ln"), false),
            ("a bearer token", Some("Bearer a-token-of-another-provider"), false),
            (
                "a SigV4 signature",
                Some(
                    "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/us-east-1/\
                     appsync/aws4_request, SignedHeaders=host, Signature=0000",
                ),
                true,
            ),
        ];
        for (case, authorization, judged) in cases {
            let resolved = presented_caller(&engine, &presented(*authorization));
            assert_eq!(
                resolved.is_some(),
                *judged,
                "case {case:?}: expected a principal to judge to be {judged}, got \
                 {resolved:?}"
            );
        }
    }

    /// A credential that speaks for NOBODY is refused every field, whatever the account's
    /// policies say: it carries no principal a statement can name and no document in force,
    /// so the decision falls to IAM's default deny — and it reports no identity to a
    /// resolver either.
    #[test]
    fn a_credential_that_speaks_for_nobody_is_refused() {
        let nobody = SignedCaller::nobody();
        assert_eq!(
            nobody.identity,
            Json::Null,
            "case the identity of a credential that speaks for nobody: expected none, got \
             {:?}",
            nobody.identity
        );
        let api = ::engine::emulator_arn("appsync", "us-east-1", "apis/anapiidofitsown");
        for (case, resource) in [
            ("one field of the API", field_resource(&api, "Query", "allowed")),
            ("the API itself", api.clone()),
            ("every resource of this service", "*".to_string()),
        ] {
            assert!(
                !permits(&nobody, &resource),
                "case {case:?}: expected a credential that speaks for nobody to be refused"
            );
        }
        // And a caller the account's policies DO cover is permitted over the same resource,
        // so the refusal above is the credential's and not the resource's.
        let allowed = SignedCaller {
            identity: Json::Null,
            principals: vec!["arn:aws:iam::000000000000:role/reader".to_string()],
            documents: vec![
                r#"{"Statement":[{"Effect":"Allow","Action":"appsync:GraphQL","Resource":"*"}]}"#
                    .to_string(),
            ],
            boundary: None,
            unbounded: false,
        };
        assert!(
            permits(&allowed, &field_resource(&api, "Query", "allowed")),
            "case a caller the policies allow: expected the field to be permitted"
        );
    }
}
