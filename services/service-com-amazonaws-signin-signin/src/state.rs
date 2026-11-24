// SPDX-License-Identifier: CC0-1.0

//! Engine state for the com-amazonaws-signin-signin service.
//!
//! Three resources live here, and every implemented handler of this service
//! reads and writes exactly these records:
//!
//! * the **resource-permission statement** — a statement of the account's SignIn
//!   resource-based policy: `ListResourcePermissionStatements` enumerates them,
//!   `GetResourcePolicy` renders them as that policy document and
//!   `DeleteResourcePermissionStatement` removes one.
//!   `PutResourcePermissionStatement`, the only operation of this service that
//!   would write one, is an action AWS grants no caller (see that handler), so it
//!   writes nothing here;
//! * the **console-authorization configuration** —
//!   `PutConsoleAuthorizationConfiguration` persists one per target,
//!   `GetConsoleAuthorizationConfiguration` reports it back and
//!   `DeleteConsoleAuthorizationConfiguration` releases it;
//! * the **OAuth 2.0 token** — `CreateOAuth2TokenWithIAM` mints an access
//!   token, `IntrospectOAuth2TokenWithIAM` reports the claims of an access or
//!   refresh token the service issued and `RevokeOAuth2TokenWithIAM` revokes a
//!   refresh token together with the whole chain minted from it.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::ops::types::{
    AccessDeniedException, ConflictException, IntrospectOAuth2TokenWithIAMOutput,
    PermissionStatementSummary, PolicyStatement, ResourceNotFoundException,
    SigninResourceBasedPolicy, TooManyRequestsError, ValidationException,
};

/// The `Version` of the rendered `SigninResourceBasedPolicy`.
///
/// `2012-10-17` is the current IAM policy-language version every AWS
/// resource-based policy document declares; the model types `version` as a plain
/// string and no operation of this service accepts one, so the service's own
/// policy language version is the only value it can report.
pub const POLICY_VERSION: &str = "2012-10-17";

/// The largest page `ListResourcePermissionStatements` serves, and the page size
/// it uses when the request names no `maxResults`:
/// `ListResourcePermissionStatementsInput$maxResults` is modelled
/// `@range(min: 1, max: 100)`.
pub const STATEMENT_PAGE_LIMIT: i32 = 100;

/// How many permission statements this emulator's store is sized for.
///
/// This is the emulator's OWN bound, not a claim about any account's limit: AWS
/// bounds a resource-based policy by its document size and publishes no statement
/// count, and this emulator models neither Service Quotas nor an account's
/// adjustments. It is what keeps the state bounded: it caps the delete-retry
/// record ([`ComAmazonawsSigninSigninState::remember_deletion`]), which mirrors the
/// statement store, so repeated deletes cannot grow the state without end.
pub const PERMISSION_STATEMENT_QUOTA: usize = 100;

/// How many DISTINCT targets the console-authorization store holds before
/// `PutConsoleAuthorizationConfiguration` refuses to take on another one.
///
/// Like [`PERMISSION_STATEMENT_QUOTA`] this is the emulator's OWN bound on the
/// store, not a claim about any account's limit: AWS publishes no maximum number
/// of console-authorization targets and this emulator models neither Service
/// Quotas nor an account's adjustments. It is what stops the map growing without
/// end, since `TargetId`'s key space is effectively unlimited: a put naming a NEW
/// target past this bound is refused. Every record it holds is released by
/// `DeleteConsoleAuthorizationConfiguration`.
pub const CONSOLE_AUTHORIZATION_TARGET_QUOTA: usize = 100;

/// The `error` code the model documents for `ValidationException`: "Will be
/// INVALID_REQUEST for validation errors".
pub const VALIDATION_ERROR_CODE: &str = "INVALID_REQUEST";

/// The `error` code the model documents for `ResourceNotFoundException`: "Will be
/// RESOURCE_NOT_FOUND".
pub const NOT_FOUND_ERROR_CODE: &str = "RESOURCE_NOT_FOUND";

/// The `error` code the model documents for `ConflictException`: "Will be CONFLICT".
pub const CONFLICT_ERROR_CODE: &str = "CONFLICT";

/// The `error` code `AccessDeniedException` carries when the caller is not permitted to
/// perform the operation: `OAuth2ErrorCode`'s `INSUFFICIENT_PERMISSIONS`, documented
/// "Insufficient permissions to perform this operation".
pub const INSUFFICIENT_PERMISSIONS_ERROR_CODE: &str = "INSUFFICIENT_PERMISSIONS";

/// The `error` code `AccessDeniedException` carries when the subject principal's
/// credentials have changed since the token was issued: `OAuth2ErrorCode`'s
/// `USER_CREDENTIALS_CHANGED`, documented "User credentials have been changed".
///
/// Nothing in this service answers it yet; it is named because
/// `AccessDeniedException`'s documented status mapping places it
/// (`crate::service::oauth2_token_endpoint`), so the mapping states the code rather than
/// a literal.
pub const USER_CREDENTIALS_CHANGED_ERROR_CODE: &str = "USER_CREDENTIALS_CHANGED";

/// The `error` code the model documents for `ServiceQuotaExceededException`:
/// "Will be SERVICE_QUOTA_EXCEEDED".
pub const QUOTA_ERROR_CODE: &str = "SERVICE_QUOTA_EXCEEDED";

/// The `error` code the model documents for `TooManyRequestsError`: "Will be
/// INVALID_REQUEST for rate limiting scenarios".
pub const RATE_LIMIT_ERROR_CODE: &str = "INVALID_REQUEST";

/// The `error` code the model documents for `InternalServerException`: "Will be
/// SERVER_ERROR for internal server errors" — the enum member `SERVER_ERROR`
/// carries the wire value `server_error`.
pub const SERVER_ERROR_CODE: &str = "server_error";

/// The description `ValidationException` carries when an OAuth 2.0 request is
/// malformed.
///
/// Not chosen wording: it is what the model documents for the `INVALID_REQUEST`
/// member of `OAuth2ErrorCode` — "The request is missing a required parameter,
/// includes an invalid parameter value, or is otherwise malformed" — which is
/// also RFC 6749 §5.2's description of the `invalid_request` error.
pub const INVALID_REQUEST_MESSAGE: &str = "The request is missing a required parameter, \
     includes an invalid parameter value, or is otherwise malformed.";

/// `IntrospectOAuth2TokenWithIAM`'s own refusal of a request that carries no
/// token value at all.
pub const TOKEN_REQUIRED_MESSAGE: &str = "token is required";

/// `IntrospectOAuth2TokenWithIAM`'s own refusal of a token value that is not
/// shaped like a Sign-In token.
pub const TOKEN_INVALID_FORMAT_MESSAGE: &str = "token has invalid format";

/// The longest token value any Sign-In token member admits:
/// `IntrospectionToken` and `RevocationToken` are both `@length(min: 1, max:
/// 4096)`.
pub const TOKEN_MAX_LENGTH: usize = 4096;

/// Whether `value` is shaped like a token `IntrospectOAuth2TokenWithIAM`
/// accepts: `IntrospectionToken`'s `^ASO[AR][A-Za-z0-9+/=_\-]+$` within its
/// `@length(min: 1, max: 4096)`.
///
/// Spelled out here because this operation owns its own acceptance: the
/// handler, not the generic constraint walk, is what answers a value outside
/// it, so the handler needs the predicate.
pub fn is_introspection_token(value: &str) -> bool {
    let Some(rest) = value
        .strip_prefix(ACCESS_TOKEN_PREFIX)
        .or_else(|| value.strip_prefix(REFRESH_TOKEN_PREFIX))
    else {
        return false;
    };
    value.len() <= TOKEN_MAX_LENGTH && !rest.is_empty() && rest.chars().all(is_token_character)
}

/// Whether `value` is shaped like a token `RevokeOAuth2TokenWithIAM` revokes:
/// `RevocationToken`'s `^ASOR[A-Za-z0-9+/=_\-]+$` within its `@length(min: 1,
/// max: 4096)`. Anything else is a value revocation leaves alone — including an
/// access-token-shaped `ASOA…` value, which the model documents as not accepted
/// for revocation.
pub fn is_revocation_token(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(REFRESH_TOKEN_PREFIX) else {
        return false;
    };
    value.len() <= TOKEN_MAX_LENGTH && !rest.is_empty() && rest.chars().all(is_token_character)
}

/// One character of the token alphabet both token shapes share:
/// `[A-Za-z0-9+/=_\-]`.
fn is_token_character(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-')
}

/// The region a request is attributed to when it carries no SigV4 credential
/// scope to read one from. `engine::emulator_arn`'s own documented default.
pub const DEFAULT_REGION: &str = "us-east-1";

/// The `iss` claim of every token this service issues:
/// `IntrospectOAuth2TokenWithIAMResponse$iss` — "Always `signin.amazonaws.com`
/// for AWS Sign-In".
pub const TOKEN_ISSUER: &str = "signin.amazonaws.com";

/// The ARN namespace of this service (`aws.api#service` `arnNamespace`), used
/// for the sign-in session ARN a token is bound to.
pub const ARN_SERVICE: &str = "signin";

/// The `access_token` alternative of `IntrospectedTokenType` /`TokenTypeHint`,
/// both modelled `^(access_token|refresh_token)$`.
pub const TOKEN_TYPE_ACCESS: &str = "access_token";

/// The `refresh_token` alternative of the same two shapes.
pub const TOKEN_TYPE_REFRESH: &str = "refresh_token";

/// The prefix an access token carries.
///
/// `IntrospectionToken` is modelled `^ASO[AR][A-Za-z0-9+/=_\-]+$` — two token
/// families — and `RevocationToken`, which takes only a refresh token ("Only
/// the refresh_token type is accepted", `RevokeOAuth2TokenWithIAM`), narrows
/// that to `^ASOR[A-Za-z0-9+/=_\-]+$`. So `ASOR` is the refresh-token family
/// and the other alternative, `ASOA`, is the access-token one.
pub const ACCESS_TOKEN_PREFIX: &str = "ASOA";

/// The prefix a refresh token carries — `RevocationToken`'s own pattern.
pub const REFRESH_TOKEN_PREFIX: &str = "ASOR";

/// The `tokenType` `CreateOAuth2TokenWithIAM` answers with: `BearerTokenType`
/// is modelled `^Bearer$` — "Always "Bearer" per OAuth 2.1 specification".
pub const BEARER_TOKEN_TYPE: &str = "Bearer";

/// The `tokenType` the anonymous `/v1/token` endpoint answers with: `TokenType`
/// is modelled `^aws_sigv4$`, documented 'Value is "aws_sigv4" (instead of
/// typical "Bearer" for other OAuth systems) to indicate that the client must
/// de-serialize the token and use it to generate a signature'.
pub const SIGV4_TOKEN_TYPE: &str = "aws_sigv4";

/// The `error` code `AccessDeniedException` carries for an authorization code
/// that cannot be redeemed: `OAuth2ErrorCode`'s `AUTHCODE_EXPIRED`.
pub const AUTHCODE_EXPIRED_ERROR_CODE: &str = "AUTHCODE_EXPIRED";

/// What that refusal states — the description the model gives that member,
/// "Authorization code has expired". Fixed text: it quotes no part of the code
/// the request presented.
pub const AUTHCODE_EXPIRED_MESSAGE: &str = "Authorization code has expired";

/// The `error` code `AccessDeniedException` carries for a token the service
/// cannot act on any more: `OAuth2ErrorCode`'s `TOKEN_EXPIRED`.
pub const TOKEN_EXPIRED_ERROR_CODE: &str = "TOKEN_EXPIRED";

/// What that refusal states — the description the model gives that member,
/// "Token has expired and needs to be refreshed". Fixed text: it quotes no part
/// of the token the request presented.
pub const TOKEN_EXPIRED_MESSAGE: &str = "Token has expired and needs to be refreshed";

/// The only grant `CreateOAuth2TokenWithIAMRequest$grantType` admits:
/// `ClientCredentialsGrantType` is modelled `^client_credentials$`, documented
/// 'OAuth 2.0 grant type. Must be "client_credentials"'.
///
/// `CreateOAuth2TokenWithIAM` judges this member itself (see its handler), so
/// the value it accepts is named here rather than left to the generic
/// constraint walk.
pub const GRANT_TYPE_CLIENT_CREDENTIALS: &str = "client_credentials";

/// The authorization-code grant of the anonymous `/v1/token` endpoint:
/// `GrantType` is modelled `^(authorization_code|refresh_token)$` and
/// `CreateOAuth2Token` documents this alternative as "Authorization code
/// redemption (grant_type=authorization_code)".
pub const GRANT_TYPE_AUTHORIZATION_CODE: &str = "authorization_code";

/// The refresh-token grant of the same endpoint — the other alternative of
/// `GrantType`, documented "Token refresh (grant_type=refresh_token)".
pub const GRANT_TYPE_REFRESH_TOKEN: &str = "refresh_token";

/// The OAuth 2.0 clients the anonymous `/v1/token` endpoint knows.
///
/// `ClientId` is modelled `^arn:aws:signin:::devtools/(cross-device|same-device)$`
/// and documents exactly these two values — the public AWS Sign-In devtools CLI
/// clients, for which `CreateOAuth2Token` documents that "authn/authz [is]
/// skipped based on client_id & grant_type", so naming one IS how such a client
/// authenticates. The ARNs carry no account and no region: they identify the
/// client program, not a resource of any account.
pub const DEVTOOLS_CLIENT_IDS: [&str; 2] = [
    "arn:aws:signin:::devtools/cross-device",
    "arn:aws:signin:::devtools/same-device",
];

/// The OAuth 2.0 client of the IAM client-credentials grant
/// (`CreateOAuth2TokenWithIAM`), reported as the `clientId` claim of every token
/// that grant mints.
///
/// Like [`DEVTOOLS_CLIENT_IDS`] it carries neither account nor region: it
/// identifies the grant itself — the one whose client is authenticated by the
/// caller's SigV4 signature rather than by a client secret — so it is a constant
/// of that grant and the same for every token minted through it.
pub const IAM_CLIENT_CREDENTIALS_CLIENT_ID: &str = "arn:aws:signin:::client-credentials/sigv4";

/// Whether `client_id` names an OAuth 2.0 client the anonymous `/v1/token`
/// endpoint knows — one of [`DEVTOOLS_CLIENT_IDS`].
pub fn is_known_oauth_client(client_id: &str) -> bool {
    DEVTOOLS_CLIENT_IDS.contains(&client_id)
}

/// `CreateOAuth2TokenWithIAMRequest$resource`'s modelled `@length(min: 1, max:
/// 2048)` ceiling, likewise judged by that operation's own handler. Smithy bounds a
/// string's length in CHARACTERS, so the handler counts characters, not bytes.
pub const MAX_RESOURCE_LENGTH: usize = 2048;

/// The lifetime, in seconds, of an access token minted by
/// `CreateOAuth2TokenWithIAM`.
///
/// `CreateOAuth2TokenWithIAMResponse$expiresIn` documents "Token lifetime in
/// seconds. Value is the minimum of session validity and 1 hour", and
/// `TokenExpiresIn` is `@range(min: 1, max: 3600)`. This emulator's sign-in
/// sessions do not expire sooner than an hour, so the minimum is the hour.
pub const ACCESS_TOKEN_EXPIRES_IN: i64 = 3600;

/// The lifetime, in seconds, of the scoped-down AWS credentials the anonymous
/// `/v1/token` endpoint vends.
///
/// `CreateOAuth2TokenResponseBody$expiresIn` is "Time to expiry in seconds
/// (maximum 900)" over `ExpiresIn`'s `@range(min: 1, max: 900)`, and
/// `AccessToken` documents "Scoped, temporary AWS credentials with a 15-minute
/// duration" — the same 900 seconds, which is therefore the lifetime and not a
/// chosen one.
pub const SCOPED_CREDENTIALS_EXPIRES_IN: u64 = 900;

/// How many OAuth 2.0 tokens the store holds before
/// `CreateOAuth2TokenWithIAM` refuses to mint another one.
///
/// Like [`PERMISSION_STATEMENT_QUOTA`] this is the emulator's OWN bound on the
/// store, not a claim about any account's limit: it is what stops the token map
/// growing without end. Every expired record is reclaimed before the bound is
/// tested, so the bound is only ever reached by that many tokens that are all
/// still live.
pub const OAUTH_TOKEN_QUOTA: usize = 100;

/// The IAM user name of the ambient caller — the identity this emulator
/// attributes a request to.
///
/// The same name the emulator's STS service reports from `GetCallerIdentity`
/// for a request whose access key names no session, so the `sub` and `userId`
/// claims of a token agree with `sts:GetCallerIdentity` as
/// `IntrospectOAuth2TokenWithIAMResponse$sub`/`$userId` document.
const AMBIENT_USER_NAME: &str = "emulator";

/// The alphabet an AWS unique id (`AIDAEXAMPLE`-style) is drawn from, and the
/// number of characters after its prefix — as the emulator's STS service mints
/// them, so a `userId` claim has the shape `sts:GetCallerIdentity` reports.
const UNIQUE_ID_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// How many characters follow a unique id's four-character prefix.
const UNIQUE_ID_SUFFIX_LEN: usize = 17;

/// `ConditionBlock`: the condition operator to the condition keys it tests, each
/// key holding the values it is tested against.
///
/// The model types it `map<ConditionType, map<String, list<String>>>`, which is
/// the IAM policy-document condition grammar:
/// `"Condition": { "<operator>": { "<condition key>": [values] } }`.
pub type ConditionBlock = HashMap<String, HashMap<String, Vec<String>>>;

/// The IAM condition operator an exact-match condition key is tested under.
const STRING_EQUALS: &str = "StringEquals";

/// The IAM condition operator an address or CIDR condition key is tested under.
const IP_ADDRESS: &str = "IpAddress";

/// The IAM condition operator the principal a statement does NOT apply to is
/// tested under.
const ARN_NOT_EQUALS: &str = "ArnNotEquals";

/// The AWS global condition key a VPC identifier is tested against.
const SOURCE_VPC_KEY: &str = "aws:SourceVpc";

/// The AWS global condition key a VPC endpoint identifier is tested against.
/// Both the SignIn and the console endpoint of one statement are values of this
/// one key.
const SOURCE_VPCE_KEY: &str = "aws:SourceVpce";

/// The condition key an address inside the VPC is tested against.
const VPC_SOURCE_IP_KEY: &str = "aws:VpcSourceIp";

/// The AWS global condition key the requester's address is tested against.
const SOURCE_IP_KEY: &str = "aws:SourceIp";

/// The AWS global condition key the region of the request is tested against.
const REQUESTED_REGION_KEY: &str = "aws:RequestedRegion";

/// The AWS global condition key the calling principal's ARN is tested against.
const PRINCIPAL_ARN_KEY: &str = "aws:PrincipalArn";

/// The authorization scope of a 12-digit account id target.
pub const ACCOUNT_SCOPE: &str = "ACCOUNT";

/// The authorization scope of an `o-` AWS Organizations organization target.
pub const ORGANIZATION_SCOPE: &str = "ORGANIZATION";

/// The authorization scope of an `r-` AWS Organizations root target.
pub const ROOT_SCOPE: &str = "ROOT";

/// One permission statement of the account's SignIn resource-based policy, as
/// this emulator persists it: the id it was minted with and the condition-key
/// members the request supplied.
#[derive(Debug, Clone)]
pub struct PermissionStatement {
    /// `StatementId`'s `^[A-Za-z0-9+/]{64}=?$`, minted by
    /// [`ComAmazonawsSigninSigninState::mint_statement_id`] and stored here: it
    /// is what `ListResourcePermissionStatements` reports as a statement's `sid`
    /// and what `DeleteResourcePermissionStatement` names.
    pub statement_id: String,
    pub source_vpc: Option<String>,
    pub signin_source_vpce: Option<String>,
    pub console_source_vpce: Option<String>,
    pub vpc_source_ip: Option<String>,
    pub source_ip: Option<String>,
    pub requested_region: Option<String>,
    pub excluded_principal: Option<String>,
}

impl PermissionStatement {
    /// The populated condition members, projected into the `ConditionBlock` the
    /// list summary and the policy document both carry.
    ///
    /// Each member becomes a value of the AWS global condition key that names
    /// it, under the operator its type is tested with: identifiers and the
    /// region by string equality, addresses by `IpAddress`, and the excluded
    /// principal by `ArnNotEquals` — the statement is the one that does NOT
    /// apply to it. A member the request did not carry contributes nothing, so
    /// a statement created with no condition member at all has no condition
    /// block.
    pub fn conditions(&self) -> Option<ConditionBlock> {
        let mut block = ConditionBlock::new();
        let mut add = |operator: &str, key: &str, value: &Option<String>| {
            if let Some(value) = value {
                block
                    .entry(operator.to_string())
                    .or_default()
                    .entry(key.to_string())
                    .or_default()
                    .push(value.clone());
            }
        };

        add(STRING_EQUALS, SOURCE_VPC_KEY, &self.source_vpc);
        add(STRING_EQUALS, SOURCE_VPCE_KEY, &self.signin_source_vpce);
        add(STRING_EQUALS, SOURCE_VPCE_KEY, &self.console_source_vpce);
        add(STRING_EQUALS, REQUESTED_REGION_KEY, &self.requested_region);
        add(IP_ADDRESS, VPC_SOURCE_IP_KEY, &self.vpc_source_ip);
        add(IP_ADDRESS, SOURCE_IP_KEY, &self.source_ip);
        add(ARN_NOT_EQUALS, PRINCIPAL_ARN_KEY, &self.excluded_principal);

        (!block.is_empty()).then_some(block)
    }

    /// The `PermissionStatementSummary` projection
    /// `ListResourcePermissionStatements` returns.
    pub fn to_summary(&self) -> PermissionStatementSummary {
        PermissionStatementSummary {
            sid: self.statement_id.clone(),
            condition: self.conditions(),
        }
    }

    /// This statement as one `PolicyStatement` of the rendered policy document.
    ///
    /// Only `Condition` is reported: the model's `PolicyStatement` carries
    /// `Effect`, `Principal`, `Action` and `Resource` as OPTIONAL members and
    /// this service's surface has no input for any of them, so the emulator
    /// reports the conditions it stored rather than inventing a frame around
    /// them. `PolicyStatement` models no `Sid` member either — the statement id
    /// is reported by `PutResourcePermissionStatement` and by
    /// `ListResourcePermissionStatements`, which is where the model puts it.
    pub fn to_policy_statement(&self) -> PolicyStatement {
        PolicyStatement {
            effect: None,
            principal: None,
            action: None,
            resource: None,
            condition: self.conditions(),
        }
    }
}

/// The console-authorization configuration of one target, as
/// `PutConsoleAuthorizationConfiguration` persisted it.
#[derive(Debug, Clone)]
pub struct ConsoleAuthorizationConfiguration {
    /// `TargetId`: a 12-digit account id, an `o-` organization id or an `r-`
    /// root id.
    pub target_id: String,
    /// The scope the target's form implies — see [`scope_for_target`].
    pub scope: String,
    /// Whether console authorization is configured for this target.
    pub console_authorization_enabled: bool,
}

impl ConsoleAuthorizationConfiguration {
    /// The three members `Put`/`Get`/`DeleteConsoleAuthorizationConfiguration`
    /// all report, read off the stored record.
    pub fn to_members(&self) -> (String, String, bool) {
        (
            self.target_id.clone(),
            self.scope.clone(),
            self.console_authorization_enabled,
        )
    }
}

/// One OAuth 2.0 token this service issued, as it is persisted: the opaque
/// value it was handed out as, the claims `IntrospectOAuth2TokenWithIAM`
/// reports for it, and whether `RevokeOAuth2TokenWithIAM` has revoked it.
#[derive(Clone)]
pub struct OAuth2Token {
    /// The opaque token value the caller presents. SENSITIVE: `token` is
    /// `@sensitive` on every operation that takes one, so [`std::fmt::Debug`]
    /// below leaves it out.
    pub token: String,
    /// [`TOKEN_TYPE_ACCESS`] or [`TOKEN_TYPE_REFRESH`] — which kind of token
    /// this is, reported as the `tokenType` claim.
    pub token_type: String,
    /// The OAuth 2.0 client that requested the token, for the `clientId` claim:
    /// one of [`DEVTOOLS_CLIENT_IDS`] for a token the anonymous `/v1/token`
    /// endpoint minted, and [`IAM_CLIENT_CREDENTIALS_CLIENT_ID`] for one
    /// `CreateOAuth2TokenWithIAM` minted — every grant this service implements
    /// has a client, so the claim identifies which grant a token came from.
    pub client_id: Option<String>,
    /// The OAuth resource the token is scoped to: the `aud` claim of an access
    /// token and the `resource` claim of a refresh token.
    pub resource: String,
    /// The account of the token's subject principal, for the `accountId` claim.
    /// A token belonging to another account is INACTIVE to this caller
    /// (RFC 7662 §2.2).
    pub account_id: String,
    /// The IAM principal ARN the token speaks for, for the `sub` claim.
    pub subject: String,
    /// The subject principal's AWS unique id, for the `userId` claim.
    pub user_id: String,
    /// The sign-in session ARN the token is bound to, for the `signinSession`
    /// claim.
    pub session_arn: String,
    /// This token's own unique identifier, for the `jti` claim.
    pub jti: String,
    /// When the token was issued, for the `iat` and `nbf` claims.
    pub issued_at: SystemTime,
    /// The token's lifetime in seconds: `iat + expires_in` is the `exp` claim.
    pub expires_in: i64,
    /// The token chain this token belongs to: the `jti` of the refresh token it
    /// was minted from, or its own when it heads its chain. Revoking a refresh
    /// token invalidates "the entire token chain"
    /// (`RevokeOAuth2TokenWithIAM`), which is every token sharing this value.
    pub chain: String,
    /// The scoped-down AWS credentials the anonymous `/v1/token` endpoint last
    /// vended for this token, while they are still live.
    ///
    /// Only a REFRESH token carries any: `CreateOAuth2Token` documents its
    /// refresh flow as "Idempotent within token validity window — multiple calls
    /// with same refresh_token return consistent results", so the credentials a
    /// refresh vended are held on the token they were vended for and reported
    /// again while they last, rather than a second set being minted per call.
    pub credentials: Option<ScopedCredentials>,
    /// Whether a revocation has invalidated this token.
    pub revoked: bool,
}

/// Prints the token's claims but never the token value itself.
impl std::fmt::Debug for OAuth2Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuth2Token")
            .field("token_type", &self.token_type)
            .field("client_id", &self.client_id)
            .field("resource", &self.resource)
            .field("account_id", &self.account_id)
            .field("subject", &self.subject)
            .field("jti", &self.jti)
            .field("issued_at", &self.issued_at)
            .field("expires_in", &self.expires_in)
            .field("revoked", &self.revoked)
            .finish_non_exhaustive()
    }
}

impl OAuth2Token {
    /// The instant the token stops being usable.
    pub fn expires_at(&self) -> SystemTime {
        self.issued_at + Duration::from_secs(u64::try_from(self.expires_in.max(0)).unwrap_or(0))
    }

    /// Whether the token's lifetime has run out at `now`.
    pub fn is_expired(&self, now: SystemTime) -> bool {
        now >= self.expires_at()
    }

    /// Whether this is a refresh token.
    pub fn is_refresh_token(&self) -> bool {
        self.token_type == TOKEN_TYPE_REFRESH
    }

    /// Whether the token is active for a caller in `caller_account`:
    /// `IntrospectOAuth2TokenWithIAMResponse$active` is "`true` only when the
    /// token is valid, has not expired, has not been revoked, and belongs to
    /// the caller's account".
    pub fn is_active(&self, now: SystemTime, caller_account: &str) -> bool {
        !self.revoked && !self.is_expired(now) && self.account_id == caller_account
    }

    /// The introspection response for this token: every claim the model
    /// declares, read off the stored record.
    ///
    /// `aud` and `resource` carry the same stored value but belong to different
    /// token kinds: `aud` is "Omitted for refresh tokens" and `resource` is
    /// "Only present for refresh token introspection".
    pub fn to_claims(&self) -> IntrospectOAuth2TokenWithIAMOutput {
        let issued_at = epoch_seconds(self.issued_at);
        let refresh = self.is_refresh_token();
        IntrospectOAuth2TokenWithIAMOutput {
            active: true,
            client_id: self.client_id.clone(),
            user_id: Some(self.user_id.clone()),
            token_type: Some(self.token_type.clone()),
            exp: Some(epoch_seconds(self.expires_at())),
            iat: Some(issued_at),
            // The token is usable from the moment it was issued, so "not
            // before" is its issuance time.
            nbf: Some(issued_at),
            sub: Some(self.subject.clone()),
            aud: (!refresh).then(|| self.resource.clone()),
            iss: Some(TOKEN_ISSUER.to_string()),
            jti: Some(self.jti.clone()),
            account_id: Some(self.account_id.clone()),
            signin_session: Some(self.session_arn.clone()),
            resource: refresh.then(|| self.resource.clone()),
        }
    }
}

/// The scoped-down, temporary AWS credentials the anonymous `/v1/token`
/// endpoint vends as its access token.
///
/// `CreateOAuth2TokenResponseBody$accessToken` targets `AccessToken`, a
/// STRUCTURE of `accessKeyId`/`secretAccessKey`/`sessionToken` documented
/// "Scoped, temporary AWS credentials with a 15-minute duration" — so this
/// endpoint's access token is an AWS SigV4 credential (its `tokenType` is
/// `aws_sigv4`), not a bearer token. The id and both secrets are MINTED BY THE
/// EMULATOR's credential store, never constructed here, so credentials a caller
/// is handed are ones this emulator accepts for signing.
#[derive(Clone)]
pub struct ScopedCredentials {
    /// The access key id the credentials were minted under — the credential
    /// store's own id, which is also what deletes them.
    pub access_key_id: String,
    /// The signing secret. SENSITIVE: left out of [`std::fmt::Debug`] below.
    pub secret_access_key: String,
    /// The session token the credentials are presented with. SENSITIVE.
    pub session_token: String,
    /// When the credentials stop being usable.
    pub expires_at: SystemTime,
}

/// Prints which credential this is but neither of its secrets.
impl std::fmt::Debug for ScopedCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopedCredentials")
            .field("access_key_id", &self.access_key_id)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl ScopedCredentials {
    /// Whether the credentials are still usable at `now`.
    pub fn is_live(&self, now: SystemTime) -> bool {
        now < self.expires_at
    }

    /// How many whole seconds of life the credentials have left at `now`, as
    /// `CreateOAuth2TokenResponseBody$expiresIn` reports it: `ExpiresIn` is
    /// `@range(min: 1, max: 900)`, so the remaining lifetime is reported inside
    /// those published bounds.
    pub fn expires_in(&self, now: SystemTime) -> i32 {
        let remaining = self
            .expires_at
            .duration_since(now)
            .map(|left| left.as_secs())
            .unwrap_or_default();
        i32::try_from(remaining.clamp(1, SCOPED_CREDENTIALS_EXPIRES_IN)).unwrap_or(i32::MAX)
    }
}

/// What a token is minted with — everything about a new token that is not
/// drawn from the random source or read off the clock.
#[derive(Debug, Clone)]
pub struct NewToken {
    /// [`TOKEN_TYPE_ACCESS`] or [`TOKEN_TYPE_REFRESH`].
    pub token_type: &'static str,
    /// The OAuth resource the token is scoped to.
    pub resource: String,
    /// The OAuth 2.0 client that requested it, if any.
    pub client_id: Option<String>,
    /// The region whose sign-in session the token is bound to.
    pub region: String,
    /// The token's lifetime in seconds.
    pub expires_in: i64,
    /// The chain to join — the `jti` of the refresh token this token was minted
    /// from. `None` heads a new chain, which is what a token minted straight
    /// from an IAM principal does.
    pub chain: Option<String>,
    /// The sign-in session ARN to bind to, when the token joins the session of
    /// the chain it was minted from. `None` mints a fresh session.
    pub session_arn: Option<String>,
}

/// The inactive-token response, which RFC 7662 §2.2 — quoted by
/// `IntrospectOAuth2TokenWithIAM` — fixes exactly: "the response body is
/// exactly { "active": false } with all other claims omitted".
pub fn inactive_token() -> IntrospectOAuth2TokenWithIAMOutput {
    IntrospectOAuth2TokenWithIAMOutput {
        active: false,
        ..IntrospectOAuth2TokenWithIAMOutput::default()
    }
}

/// Per-service state for com-amazonaws-signin-signin. A handler reaches it through [`service_state`].
#[derive(Default)]
pub struct ComAmazonawsSigninSigninState {
    /// Every permission statement, keyed by its id. Ids sort in mint order in a
    /// `BTreeMap` — which is what gives `ListResourcePermissionStatements` the
    /// stable total order its pagination token indexes into, and
    /// `GetResourcePolicy` a deterministic document.
    pub permission_statements: BTreeMap<String, PermissionStatement>,
    /// A `PutResourcePermissionStatement` `clientToken` to the statement that
    /// token created. Nothing writes it: `PutResourcePermissionStatement` is an
    /// action AWS grants no caller, so no statement is ever created under a token.
    /// It is kept because it is what a delete forgets the tokens of.
    pub statement_idempotency: HashMap<String, String>,
    /// A `DeleteResourcePermissionStatement` `clientToken` to the statement it
    /// removed, so a retried delete answers the same success rather than a 404.
    /// Written only through [`ComAmazonawsSigninSigninState::remember_deletion`],
    /// which keeps it to the [`PERMISSION_STATEMENT_QUOTA`] most recent deletes.
    pub deletion_idempotency: HashMap<String, String>,
    /// The tokens of [`Self::deletion_idempotency`] in the order they were
    /// recorded, so the oldest is the one evicted when the bound is reached.
    pub deletion_token_order: VecDeque<String>,
    /// The console-authorization configuration of each configured target.
    /// Bounded by [`CONSOLE_AUTHORIZATION_TARGET_QUOTA`]: a put naming a new
    /// target past that count is refused rather than stored.
    pub console_auth_configs: HashMap<String, ConsoleAuthorizationConfiguration>,
    /// Every OAuth 2.0 token this service issued, keyed by the opaque token
    /// value the caller presents — which is what both
    /// `IntrospectOAuth2TokenWithIAM` and `RevokeOAuth2TokenWithIAM` look a
    /// token up by. Bounded by [`OAUTH_TOKEN_QUOTA`], with every expired record
    /// reclaimed before a new token is minted.
    pub oauth_tokens: HashMap<String, OAuth2Token>,
    /// The counter statement ids are minted from.
    pub statement_id_seq: u64,
}

/// Prints the store, but reports the token map by SIZE only: its keys are the
/// opaque token values themselves, which are `@sensitive` on the wire and must
/// not reach a log through a formatted store.
impl std::fmt::Debug for ComAmazonawsSigninSigninState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComAmazonawsSigninSigninState")
            .field("permission_statements", &self.permission_statements)
            .field("statement_idempotency", &self.statement_idempotency)
            .field("deletion_idempotency", &self.deletion_idempotency)
            .field("deletion_token_order", &self.deletion_token_order)
            .field("console_auth_configs", &self.console_auth_configs)
            .field("oauth_tokens", &self.oauth_tokens.len())
            .field("statement_id_seq", &self.statement_id_seq)
            .finish()
    }
}

impl ComAmazonawsSigninSigninState {
    /// Mint an unused statement id matching `StatementId`'s
    /// `^[A-Za-z0-9+/]{64}=?$`.
    ///
    /// The counter is rendered as 64 lowercase hex digits — an alphabet that
    /// pattern admits and whose ASCII order is its numeric order, so ids sort in
    /// mint order and one of them is usable as a `nextToken` (whose own pattern
    /// `^[-a-zA-Z0-9+=/_]*$` admits it too). The loop skips an id already taken,
    /// which is what makes the counter's wrap safe.
    pub fn mint_statement_id(&mut self) -> String {
        loop {
            self.statement_id_seq = self.statement_id_seq.wrapping_add(1);
            let id = format!("{:064x}", self.statement_id_seq);
            if !self.permission_statements.contains_key(&id) {
                return id;
            }
        }
    }

    /// Record that `token` deleted `statement_id`, keeping at most
    /// [`PERMISSION_STATEMENT_QUOTA`] records: once that many deletes are
    /// remembered the OLDEST is forgotten to make room, so the map stays bounded
    /// without the idempotency guarantee decaying as the store ages — a retry of
    /// a recent delete still answers its original success.
    pub fn remember_deletion(&mut self, token: String, statement_id: String) {
        if self
            .deletion_idempotency
            .insert(token.clone(), statement_id)
            .is_none()
        {
            self.deletion_token_order.push_back(token);
        }
        while self.deletion_token_order.len() > PERMISSION_STATEMENT_QUOTA {
            if let Some(evicted) = self.deletion_token_order.pop_front() {
                self.deletion_idempotency.remove(&evicted);
            }
        }
    }

    /// True when `token` is a pagination token this service could have issued:
    /// one of the statement ids [`Self::mint_statement_id`] has already minted.
    /// `ListResourcePermissionStatements`' `nextToken` is modelled
    /// `^[-a-zA-Z0-9+=/_]*$`, far wider than the ids this service mints, so this
    /// is what separates a token the caller got from a page of this operation
    /// from one it invented. A minted id is 64 lowercase hex digits rendering a
    /// counter value the mint has reached, so a token is issuable exactly when it
    /// reads as such a value; the id of a statement deleted meanwhile stays
    /// issuable, which is what lets a page still be served after the statement
    /// its token named is gone.
    pub fn is_issued_pagination_token(&self, token: &str) -> bool {
        token.len() == 64
            && u128::from_str_radix(token, 16)
                .is_ok_and(|value| (1..=u128::from(self.statement_id_seq)).contains(&value))
    }

    /// Whether the account HAS a SignIn resource-based policy at all.
    ///
    /// The policy is not a resource of its own on this service's surface: it is the
    /// document the account's permission statements make up, so the account has one
    /// exactly while at least one statement is stored. With nothing stored the policy
    /// is MISSING, not empty, and every reader of it answers the modelled
    /// `ResourceNotFoundException` rather than a document nothing created.
    pub fn has_resource_policy(&self) -> bool {
        !self.permission_statements.is_empty()
    }

    /// Render the account's SignIn resource-based policy from the stored
    /// statements: one policy statement per stored permission statement, in
    /// mint order — or `None` when the account has no policy at all.
    pub fn resource_policy(&self) -> Option<SigninResourceBasedPolicy> {
        self.has_resource_policy()
            .then(|| SigninResourceBasedPolicy {
                version: Some(POLICY_VERSION.to_string()),
                statement: Some(
                    self.permission_statements
                        .values()
                        .map(PermissionStatement::to_policy_statement)
                        .collect(),
                ),
            })
    }

    /// The token record `value` names, whatever its kind: both operations that
    /// take a token resolve it by the value the caller presented, so a
    /// `tokenTypeHint` only ever saves a lookup — "the server uses this hint to
    /// optimize lookup, but still falls back to the other token type on miss"
    /// (`IntrospectOAuth2TokenWithIAMRequest$tokenTypeHint`).
    pub fn token(&self, value: &str) -> Option<&OAuth2Token> {
        self.oauth_tokens.get(value)
    }

    /// Reclaims every token past its own lifetime. An expired token is
    /// INACTIVE to `IntrospectOAuth2TokenWithIAM` and is no longer usable for
    /// anything else either, so dropping it changes no answer — which is what
    /// lets the clock, rather than a caller, bound this store.
    pub fn purge_expired_tokens(&mut self, now: SystemTime) {
        self.oauth_tokens.retain(|_, token| !token.is_expired(now));
    }

    /// Whether another token fits under [`OAUTH_TOKEN_QUOTA`].
    pub fn has_room_for_token(&self) -> bool {
        self.oauth_tokens.len() < OAUTH_TOKEN_QUOTA
    }

    /// Mints and persists a token, and hands back the record as stored.
    ///
    /// The token value and the identifiers are drawn from the system random
    /// source, never from the request, so a value is unguessable and no two
    /// tokens collide; the subject principal is the identity this emulator
    /// attributes the request to ([`ambient_subject`]).
    pub fn issue_token(&mut self, new: NewToken, now: SystemTime) -> OAuth2Token {
        let prefix = if new.token_type == TOKEN_TYPE_REFRESH {
            REFRESH_TOKEN_PREFIX
        } else {
            ACCESS_TOKEN_PREFIX
        };
        let jti = random_uuid();
        let token = OAuth2Token {
            token: self.fresh_token_value(prefix),
            token_type: new.token_type.to_string(),
            client_id: new.client_id,
            resource: new.resource,
            account_id: ::engine::emulator_account_id().to_string(),
            user_id: ambient_user_id(),
            subject: ambient_subject(),
            session_arn: new
                .session_arn
                .unwrap_or_else(|| signin_session_arn(&new.region, &random_uuid())),
            chain: new.chain.unwrap_or_else(|| jti.clone()),
            jti,
            issued_at: now,
            expires_in: new.expires_in,
            // Credentials are vended against a token, never minted with it: a
            // refresh token carries the set the anonymous endpoint last vended
            // for it (see [`Self::refreshed_credentials`]).
            credentials: None,
            revoked: false,
        };
        self.oauth_tokens.insert(token.token.clone(), token.clone());
        token
    }

    /// The scoped-down AWS credentials the refresh token `refresh_token` is good
    /// for at `now`, and the credential the caller must DELETE afterwards.
    ///
    /// `CreateOAuth2Token`'s refresh flow is "Idempotent within token validity
    /// window", so the credentials already vended for this refresh token are
    /// reported again while they are still live and `minted` — the set the caller
    /// minted for this call — is handed back as the one to delete, unused. Once
    /// they have run out, `minted` becomes the token's credentials and the set it
    /// replaced is the one to delete, so one refresh token holds at most one
    /// credential however often it is presented.
    ///
    /// `None` when `refresh_token` names no refresh token this store holds that
    /// is active for `caller_account` — the caller then deletes the credential it
    /// minted itself, since nothing was vended.
    ///
    /// The refresh token is NOT consumed: that is what lets a repeated refresh
    /// answer consistently, where an authorization code "can only be used ONCE".
    pub fn refreshed_credentials(
        &mut self,
        refresh_token: &str,
        caller_account: &str,
        now: SystemTime,
        minted: ScopedCredentials,
    ) -> Option<(ScopedCredentials, Option<String>)> {
        let record = self
            .oauth_tokens
            .get_mut(refresh_token)
            .filter(|token| token.is_refresh_token() && token.is_active(now, caller_account))?;
        if let Some(live) = record
            .credentials
            .as_ref()
            .filter(|credentials| credentials.is_live(now))
        {
            return Some((live.clone(), Some(minted.access_key_id)));
        }
        let superseded = record
            .credentials
            .replace(minted.clone())
            .map(|credentials| credentials.access_key_id);
        Some((minted, superseded))
    }

    /// Revokes the refresh token `value` names and the whole chain minted from
    /// it, and answers whether any record was revoked.
    ///
    /// `RevokeOAuth2TokenWithIAM` "invalidat[es] the entire token chain so that
    /// the refresh_token can no longer be used to mint new access_tokens": the
    /// access tokens already minted from it share its chain, so they are
    /// revoked with it. The records are KEPT and flagged rather than dropped,
    /// so a revoked token stays distinguishable until its lifetime runs out.
    pub fn revoke_token_chain(&mut self, value: &str) -> bool {
        let Some(chain) = self
            .oauth_tokens
            .get(value)
            .filter(|token| token.is_refresh_token())
            .map(|token| token.chain.clone())
        else {
            return false;
        };
        let mut revoked = false;
        for token in self.oauth_tokens.values_mut() {
            if token.chain == chain && !token.revoked {
                token.revoked = true;
                revoked = true;
            }
        }
        revoked
    }

    /// A token value of `prefix` the store does not already hold.
    fn fresh_token_value(&self, prefix: &str) -> String {
        loop {
            let candidate = format!("{prefix}{}", mint_opaque_value());
            if !self.oauth_tokens.contains_key(&candidate) {
                return candidate;
            }
        }
    }
}

/// The authorization scope `TargetId`'s form implies.
///
/// `TargetId` is modelled `^(\d{12}|o-[a-z0-9]{10}|r-[0-9a-z]{4,32})$` — an
/// account id, an AWS Organizations organization id or an organizational root
/// id — so the scope is a function of which alternative the value matches.
/// `scope` is modelled as a plain string with no enum, so these three names are
/// this emulator's, named after the Organizations entity each target form
/// identifies. `None` is unreachable through a request the boundary validator
/// admitted, and the handlers refuse it as a validation failure rather than
/// guessing a scope.
pub fn scope_for_target(target_id: &str) -> Option<&'static str> {
    if target_id.len() == 12 && target_id.bytes().all(|b| b.is_ascii_digit()) {
        return Some(ACCOUNT_SCOPE);
    }
    if target_id.starts_with("o-") {
        return Some(ORGANIZATION_SCOPE);
    }
    if target_id.starts_with("r-") {
        return Some(ROOT_SCOPE);
    }
    None
}

/// What AWS states for a resource of the account's SignIn resource-based policy the
/// account does not have — the policy itself, or a statement of it.
///
/// Fixed text in full: the sentence names the resource class and interpolates no part
/// of the request, so the id a caller asked for is reported by the status and the error
/// code alone.
pub const RESOURCE_NOT_FOUND_MESSAGE: &str = "Requested resource not found";

/// What AWS states for a target that has no console-authorization configuration.
/// Fixed text in full, with no part of the request in it.
pub const CONFIGURATION_NOT_FOUND_MESSAGE: &str = "Console authorization configuration not found";

/// What AWS states when console authorization is already configured for the target a
/// put names. Fixed text in full, with no part of the request in it.
pub const CONFIGURATION_EXISTS_MESSAGE: &str = "Configuration already exists";

/// What AWS states for a call scoped to an AWS Organizations entity the caller has no
/// authority over. Fixed text in full, with no part of the request in it.
pub const ORGANIZATION_NOT_AUTHORIZED_MESSAGE: &str =
    "Caller is not authorized for this organization";

/// The `ResourceNotFoundException` payload for a resource-policy resource the account
/// does not have: the policy document itself, or a statement id that names no stored
/// statement.
pub fn resource_not_found() -> ResourceNotFoundException {
    ResourceNotFoundException {
        error: NOT_FOUND_ERROR_CODE.to_string(),
        message: RESOURCE_NOT_FOUND_MESSAGE.to_string(),
    }
}

/// The `ResourceNotFoundException` payload for a target whose console
/// authorization is not configured.
pub fn configuration_not_found() -> ResourceNotFoundException {
    ResourceNotFoundException {
        error: NOT_FOUND_ERROR_CODE.to_string(),
        message: CONFIGURATION_NOT_FOUND_MESSAGE.to_string(),
    }
}

/// The `ConflictException` payload for a put naming a target whose console
/// authorization is already enabled: the request conflicts with the state the target is
/// already in, which is what the model documents this error for ("request conflicts
/// with current state").
pub fn configuration_already_exists() -> ConflictException {
    ConflictException {
        error: CONFLICT_ERROR_CODE.to_string(),
        message: CONFIGURATION_EXISTS_MESSAGE.to_string(),
    }
}

/// The `AccessDeniedException` payload for a call scoped to an AWS Organizations entity
/// the caller is not authorized for — see [`caller_is_authorized_for_target`].
pub fn not_authorized_for_organization() -> AccessDeniedException {
    AccessDeniedException {
        error: INSUFFICIENT_PERMISSIONS_ERROR_CODE.to_string(),
        message: ORGANIZATION_NOT_AUTHORIZED_MESSAGE.to_string(),
    }
}

/// The `AccessDeniedException` payload for an API no caller of this service is granted:
/// the action's own name, which is the one variable part of the sentence.
pub fn no_access_to_api(api: &str) -> AccessDeniedException {
    AccessDeniedException {
        error: INSUFFICIENT_PERMISSIONS_ERROR_CODE.to_string(),
        message: format!("No Access to {api} API"),
    }
}

/// Whether the caller may act on the console-authorization configuration of
/// `target_id`.
///
/// An AWS Organizations entity — an organization (`o-`) or an organizational root
/// (`r-`) — belongs to exactly one organization, and console authorization for it is
/// that organization's management account's to configure, so a target naming an entity
/// the emulator's Organizations control plane does not record for the caller's own
/// organization is one the caller has no authority over. A 12-digit account target
/// names no Organizations entity and carries no such requirement.
///
/// Fails CLOSED: an engine that records no organization — one the Organizations control
/// plane is not mounted on — makes no entity the caller's, and an authorization check
/// that cannot run must not admit the request.
///
/// A one-directional, guard-free READ of the Organizations store: it takes that store's
/// lock, holds no other while it does, hands back an owned decision and writes nothing,
/// so it is called with NO guard of this service's own store held.
pub fn caller_is_authorized_for_target<Eng: Send + Sync + 'static>(
    engine: &Eng,
    target_id: &str,
) -> bool {
    if !matches!(
        scope_for_target(target_id),
        Some(ORGANIZATION_SCOPE | ROOT_SCOPE)
    ) {
        return true;
    }
    let Some(organizations) =
        ::service_com_amazonaws_organizations_awsorganizationsv20161128::state::service_state(
            engine,
        )
    else {
        return false;
    };
    let organizations = organizations.lock();
    let Some(organization) = organizations
        .organization
        .as_ref()
        .filter(|organization| organization.master_account_id == ::engine::emulator_account_id())
    else {
        return false;
    };
    organization.id == target_id || organizations.roots.iter().any(|root| root.id == target_id)
}

/// The `TooManyRequestsError` payload for a put that would take the
/// console-authorization store past [`CONSOLE_AUTHORIZATION_TARGET_QUOTA`].
///
/// `PutConsoleAuthorizationConfiguration` does not declare
/// `ServiceQuotaExceededException`, so the store's bound is reported with the
/// modelled refusal it does declare for exactly this shape of request: the one
/// the model documents for "abuse prevention" and "service protection against
/// excessive" use.
pub fn console_target_quota_exceeded(target_id: &str) -> TooManyRequestsError {
    TooManyRequestsError {
        error: RATE_LIMIT_ERROR_CODE.to_string(),
        message: format!(
            "console authorization is already configured for the maximum of {} targets; \
             configuring targetId {target_id} as well would exceed it",
            CONSOLE_AUTHORIZATION_TARGET_QUOTA
        ),
    }
}

/// The modelled `ValidationException` payload carrying `message`.
pub fn validation_failed(message: impl Into<String>) -> ValidationException {
    ValidationException {
        error: VALIDATION_ERROR_CODE.to_string(),
        message: message.into(),
    }
}

/// The target a console-authorization call addresses: the `targetId` it carries,
/// or the caller's own account when it carries none.
pub fn requested_target(target_id: Option<String>) -> String {
    target_id.unwrap_or_else(|| ::engine::emulator_account_id().to_string())
}

/// This service's store in the shared engine: call it with a handler's `&*self.engine`.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn service_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ComAmazonawsSigninSigninState>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ComAmazonawsSigninSigninState>)
}

/// The region this request was signed for, read from its SigV4 credential
/// scope — the only region a request carries — or [`DEFAULT_REGION`] when it
/// carries none.
pub fn request_region(parts: &::http::request::Parts) -> String {
    ::emulator_server::sigv4_credential_scope(&parts.headers)
        .map_or_else(|| DEFAULT_REGION.to_string(), |scope| scope.region)
}

/// A sign-in session ARN:
/// `IntrospectOAuth2TokenWithIAMResponse$signinSession` documents the form
/// `arn:aws:signin:{region}:{account}:session/{uuid}`. The account segment
/// comes from the engine, never from a literal.
pub fn signin_session_arn(region: &str, session_uuid: &str) -> String {
    ::engine::emulator_arn(ARN_SERVICE, region, &format!("session/{session_uuid}"))
}

/// The IAM principal ARN this emulator attributes a request to.
///
/// A request carries no identity beyond its SigV4 access key id, and this
/// service holds no session registry to resolve one through, so the subject is
/// the ambient identity the emulator's STS service reports for exactly that
/// case: the IAM user of the emulator's own account. Both are built from
/// `engine`'s account contract, so a token's `sub` claim is the `Arn`
/// `sts:GetCallerIdentity` answers with, as the model says it must be.
pub fn ambient_subject() -> String {
    ::engine::emulator_arn("iam", "", &format!("user/{AMBIENT_USER_NAME}"))
}

/// The subject principal's AWS unique id, for the `userId` claim.
pub fn ambient_user_id() -> String {
    unique_id("AIDA", &ambient_subject())
}

/// An AWS unique id: `prefix` plus [`UNIQUE_ID_SUFFIX_LEN`] characters derived
/// from `seed`.
///
/// The characters are a SHA-256 digest of `seed` mapped onto the id alphabet,
/// so the id is a function of the principal it names — the same principal
/// always yields the same id, two different ones practically never collide, and
/// nothing about it depends on when it was asked for. This is the derivation
/// the emulator's STS service mints `AIDA`/`AROA` ids with, so a `userId` claim
/// matches the `UserId` `sts:GetCallerIdentity` reports. It is NOT base32: the
/// mapping only has to be stable and well distributed, never decodable.
pub fn unique_id(prefix: &str, seed: &str) -> String {
    let digest = Sha256::digest(seed.as_bytes());
    let suffix = digest
        .iter()
        .take(UNIQUE_ID_SUFFIX_LEN)
        .map(|byte| char::from(UNIQUE_ID_ALPHABET[usize::from(*byte) % UNIQUE_ID_ALPHABET.len()]));
    prefix.chars().chain(suffix).collect()
}

/// `at` as whole seconds since the Unix epoch — the NumericDate the `exp`,
/// `iat` and `nbf` claims are modelled as (`Long`, "Unix epoch seconds").
pub fn epoch_seconds(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// A fresh random (version 4) UUID in its canonical hyphenated form, from the
/// system random source.
///
/// Drawn through `emulator_telemetry`'s own generator, a crate this service
/// already depends on, so the opaque values below add no dependency.
fn random_uuid() -> String {
    ::emulator_telemetry::SessionId::random().as_str().to_string()
}

/// A fresh opaque token body: 244 bits from the system random source, rendered
/// as the hex digits of two UUIDs, so a token value can be resolved only by the
/// caller it was handed to. Every character is one `IntrospectionToken`'s
/// `^ASO[AR][A-Za-z0-9+/=_\-]+$` admits.
pub fn mint_opaque_value() -> String {
    let mut value = String::with_capacity(64);
    for _ in 0..2 {
        value.extend(random_uuid().chars().filter(|c| *c != '-'));
    }
    value
}

/// The `TooManyRequestsError` payload for a mint that would take the token
/// store past [`OAUTH_TOKEN_QUOTA`].
///
/// `CreateOAuth2TokenWithIAM` declares no quota error; this is the refusal its
/// model documents for exactly this shape of request — "abuse prevention
/// mechanisms triggered", "service protection against excessive token
/// generation".
pub fn token_quota_exceeded() -> TooManyRequestsError {
    TooManyRequestsError {
        error: RATE_LIMIT_ERROR_CODE.to_string(),
        message: format!(
            "this account already holds the maximum of {OAUTH_TOKEN_QUOTA} live OAuth 2.0 \
             tokens; revoke or let one expire before requesting another"
        ),
    }
}

