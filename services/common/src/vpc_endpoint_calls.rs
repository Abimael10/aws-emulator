// SPDX-License-Identifier: CC0-1.0

//! What a VPC ENDPOINT POLICY decides about an AWS API call made from inside a VPC.
//!
//! # The rule this module implements
//!
//! "A VPC endpoint policy is an IAM resource policy that you attach to a VPC endpoint … it
//! controls access to the service that the endpoint is for" (`CreateVpcEndpointRequest$PolicyDocument`),
//! and a request that travels through the endpoint has to be allowed by BOTH the caller's own
//! IAM policies and that endpoint policy — so a call the endpoint policy does not allow is
//! refused however the caller's identity-based policies read. That is the whole point of an
//! endpoint policy: a machine whose network reaches a service only through an endpoint is held
//! to the endpoint's terms, which is how an account narrows what its own instances may do with
//! a service they are otherwise permitted to call.
//!
//! The decision is ONE rule, so it lives here once rather than in each component that runs
//! caller-supplied code inside a VPC: two of them must not answer differently about the same
//! document. What a caller supplies is the endpoint policies in force on the traffic
//! ([`ServiceEndpointPolicy`], collected by the service that OWNS VPC endpoints) and the
//! principal the call is signed as; what this module answers is whether the call travels and,
//! where it does not, the refusal the service answers it with ([`RefusedCall`]).
//!
//! # Which calls it decides
//!
//! A call is decided against the policies of the endpoints for the SERVICE it addresses: an
//! endpoint's `ServiceName` is `com.amazonaws.<region>.<service>` ("the name of the endpoint
//! service", `CreateVpcEndpointRequest$ServiceName`), whose last segment is the service's own
//! endpoint token ([`endpoint_service_token`]). An account whose VPC holds no endpoint for the
//! called service has no endpoint policy in force on the call at all, and nothing here refuses
//! one.
//!
//! WHAT the call asks for is the IAM action and the resource ARN the request addresses
//! ([`addressed_call`]): the action is the called service's namespace and the operation's own
//! name ([`crate::iam_authorization::iam_action`]), and the resource is the one the request's
//! own members name. The two services AWS offers GATEWAY endpoints for — "you can use gateway
//! endpoints to connect to Amazon S3 and DynamoDB" (Amazon VPC User Guide, "Gateway endpoints")
//! — are the ones whose requests are read here, because those are the endpoints whose traffic
//! is routed by a route table rather than resolved by private DNS. A call of any other service
//! addresses no resource this module can name, so it is admitted rather than refused on a
//! resource nothing resolved.

use crate::iam_authorization::{
    self as authorization, iam_action, no_vpc_endpoint_policy_allows, not_authorized_to_perform,
    Decision, VPC_ENDPOINT_POLICY_EXPLICIT_DENY,
};

/// How a `ServiceName` names the service an endpoint is for: `com.amazonaws.<region>.<token>`,
/// whose segments before the token are the prefix every AWS-owned endpoint service carries.
const AWS_ENDPOINT_SERVICE_PREFIX: &str = "com.amazonaws.";

/// The endpoint-service token Amazon S3's own endpoints carry, which is also its IAM action
/// namespace and the name an SDK client for it is built with.
pub const S3_SERVICE_TOKEN: &str = "s3";

/// The same for Amazon DynamoDB.
pub const DYNAMODB_SERVICE_TOKEN: &str = "dynamodb";

/// The error code Amazon S3 refuses an unauthorized request with, and the status it answers:
/// "AccessDenied … Access denied … 403 Forbidden" (Amazon S3 API Reference, "Error responses").
const S3_ACCESS_DENIED: &str = "AccessDenied";

/// The error code every AWS JSON service refuses one with, and the status such a refusal
/// carries: the protocol answers a modelled client error with HTTP 400, and
/// `AccessDeniedException` is the shape DynamoDB's own model declares for it.
const JSON_ACCESS_DENIED: &str = "AccessDeniedException";

/// See [`S3_ACCESS_DENIED`].
const FORBIDDEN_STATUS: u16 = 403;

/// See [`JSON_ACCESS_DENIED`].
const CLIENT_ERROR_STATUS: u16 = 400;

/// ONE VPC endpoint's policy, as the service that owns VPC endpoints holds it.
#[derive(Debug, Clone)]
pub struct ServiceEndpointPolicy {
    /// The endpoint's own id (`VpcEndpoint$VpcEndpointId`), which is what a refusal is a fact
    /// about.
    pub endpoint_id: String,
    /// The endpoint-service token the endpoint is for ([`endpoint_service_token`]), which is
    /// what decides WHICH calls it is in force on.
    pub service_token: String,
    /// The document attached to it (`VpcEndpoint$PolicyDocument`) — the default full-access
    /// policy for an endpoint created without one, because that is what EC2 stores for it.
    pub policy_document: String,
}

/// What one call ASKS FOR, as an endpoint policy decides it.
#[derive(Debug, Clone)]
pub struct AddressedCall {
    /// The fully qualified IAM action (`s3:PutObject`).
    pub action: String,
    /// The ARN of the resource the request addresses.
    pub resource: String,
    /// The error code the called service refuses an unauthorized request with.
    pub code: &'static str,
    /// The HTTP status that refusal carries.
    pub status: u16,
}

/// A call the network did NOT carry: the refusal the called service answers it with.
#[derive(Debug, Clone)]
pub struct RefusedCall {
    /// The HTTP status the answer carries.
    pub status: u16,
    /// The error code it names.
    pub code: String,
    /// Its message.
    pub message: String,
}

/// The endpoint-service token `service_name` names: the last segment of
/// `com.amazonaws.<region>.<token>`, which is the called service's own token.
///
/// `None` for a value that is not an AWS-owned endpoint service name at all — the
/// `com.amazonaws.vpce.<region>.<id>` name of another account's service among them, which names
/// no AWS service and so is in force on no call this module decides.
#[must_use]
pub fn endpoint_service_token(service_name: &str) -> Option<&str> {
    let named = service_name.strip_prefix(AWS_ENDPOINT_SERVICE_PREFIX)?;
    let (region, token) = named.split_once('.')?;
    // `com.amazonaws.vpce.<region>.<id>` is the name of a service another account published,
    // and the segment after the prefix is then the literal `vpce` rather than a Region.
    if region.is_empty() || token.is_empty() || token.contains('.') {
        return None;
    }
    Some(token)
}

/// What the call `operation` of the service `signing_name` asks for, read off the request's own
/// input document `body` — `None` for a service whose requests this module does not read (see
/// the module docs).
///
/// `region` and `account` are the Region the call is made in and the account it is made by,
/// which the ARN of a regional resource carries; an Amazon S3 ARN carries neither ("the ARN for
/// an S3 bucket … `arn:aws:s3:::bucket_name`", Amazon S3 User Guide).
#[must_use]
pub fn addressed_call(
    signing_name: &str,
    operation: &str,
    region: &str,
    account: &str,
    body: &::serde_json::Value,
) -> Option<AddressedCall> {
    let text = |member: &str| {
        body.get(member)
            .and_then(::serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
    };
    match signing_name {
        S3_SERVICE_TOKEN => {
            let bucket = text("Bucket")?;
            // "You specify a resource … for object operations, `arn:aws:s3:::bucket_name/key`;
            // for bucket operations, `arn:aws:s3:::bucket_name`" (Amazon S3 User Guide, "Policy
            // resources for Amazon S3"), so a request that names an OBJECT is authorized on
            // that object and one that names only the bucket on the bucket.
            let resource = match text("Key") {
                Some(key) => format!("arn:aws:s3:::{bucket}/{key}"),
                None => format!("arn:aws:s3:::{bucket}"),
            };
            Some(AddressedCall {
                action: iam_action(S3_SERVICE_TOKEN, operation),
                resource,
                code: S3_ACCESS_DENIED,
                status: FORBIDDEN_STATUS,
            })
        }
        DYNAMODB_SERVICE_TOKEN => {
            let table = text("TableName")?;
            Some(AddressedCall {
                action: iam_action(DYNAMODB_SERVICE_TOKEN, operation),
                // "arn:aws:dynamodb:region:account-id:table/table-name" (Amazon DynamoDB
                // Developer Guide, "Amazon DynamoDB resources and operations").
                resource: format!("arn:aws:dynamodb:{region}:{account}:table/{table}"),
                code: JSON_ACCESS_DENIED,
                status: CLIENT_ERROR_STATUS,
            })
        }
        _ => None,
    }
}

/// Whether the endpoints `policies` describe carry the call `addressed` names, made by
/// `principal` — and the refusal the called service answers it with when they do not.
///
/// The endpoints in force on the call are the ones for the called service, which is the service
/// the action's own namespace names; an account whose VPC holds none has no endpoint policy on
/// the call and `None` is the answer. Each of those documents is read as the RESOURCE POLICY it
/// is ([`authorization::resource_decision`]), in IAM's own order: an explicit `Deny` refuses
/// whatever else is written, and otherwise the call needs an `Allow` from the endpoint — a
/// document that states neither grants nothing, which is the position a request through an
/// endpoint whose policy does not cover it is in.
///
/// `principal` is the ARN the call is signed as — the assumed-role session an instance's own
/// calls are made under. The spellings a document may name it by are read off it the way every
/// other resource-policy decision of this crate reads them
/// ([`crate::s3_object_authorization::bucket_policy_decision`]): the session and its role grant,
/// and the ACCOUNT's own spellings are added to the deny side alone, because a statement that
/// allows an account delegates to that account's identity-based policies while one that denies it
/// refuses each of its principals.
#[must_use]
pub fn refusal_through_endpoints(
    policies: &[ServiceEndpointPolicy],
    principal: &str,
    addressed: &AddressedCall,
) -> Option<RefusedCall> {
    let namespace = addressed.action.split_once(':')?.0;
    let granting = principal_spellings(principal);
    let mut denying = granting.clone();
    denying.extend(authorization::account_principals(principal));
    let decided = |document: &str, principals: &[String]| {
        authorization::resource_decision(
            document,
            &authorization::Request::new(&addressed.action, &addressed.resource, principals),
        )
    };
    let mut carried = false;
    let mut allowed = false;
    for policy in policies {
        if policy.service_token != namespace {
            continue;
        }
        carried = true;
        // "An explicit deny in any of these policies overrides the allow" (IAM User Guide,
        // "Policy evaluation logic"): the deny is a fact about the endpoint's own document, so
        // the refusal names that document and not the caller's.
        if decided(&policy.policy_document, &denying) == Decision::Denied {
            return Some(refused(
                addressed,
                principal,
                VPC_ENDPOINT_POLICY_EXPLICIT_DENY,
            ));
        }
        if decided(&policy.policy_document, &granting) == Decision::Allowed {
            allowed = true;
        }
    }
    if !carried || allowed {
        return None;
    }
    Some(refused(
        addressed,
        principal,
        &no_vpc_endpoint_policy_allows(&addressed.action),
    ))
}

/// Every spelling an endpoint policy may name the calling principal by: the ARN the call is
/// signed as and, for an assumed-role session, the ROLE behind it — "the permissions in force on
/// such a session are the role's" ([`crate::iam_role::assumed_role_session_role_name`]), which is
/// why a document written about the role covers the session's calls.
fn principal_spellings(principal: &str) -> Vec<String> {
    let mut spellings = vec![principal.to_string()];
    let role = crate::iam_role::assumed_role_session_role_name(principal);
    let parsed = crate::arn::parse(principal);
    if let (Some(role), Some(parsed)) = (role, parsed) {
        spellings.push(format!(
            "arn:{}:iam::{}:role/{role}",
            parsed.partition, parsed.account
        ));
    }
    spellings
}

/// The refusal one call is answered with: the called service's own code and status, and the
/// sentence AWS words such a refusal in — the principal, the action and the resource the
/// request named, and WHICH of the endpoint policy's readings withheld it.
fn refused(addressed: &AddressedCall, principal: &str, reason: &str) -> RefusedCall {
    RefusedCall {
        status: addressed.status,
        code: addressed.code.to_string(),
        message: format!(
            "{} {reason}",
            not_authorized_to_perform(principal, &addressed.action, &addressed.resource)
        ),
    }
}

/// What decides whether a call made by caller-supplied code LEAVES the network it runs in.
///
/// The in-process AWS API seam a running program calls through consults this before it
/// dispatches, so a network control the account wrote is enforced on the traffic rather than
/// only stored. A seam given none is one with no such control in force on it, which is where
/// every seam was before this.
pub trait CallEgress: ::std::fmt::Debug + Send + Sync {
    /// The refusal the called service answers the call `operation` of `signing_name` with,
    /// carrying `body` as its input document and made in `region` — `None` for a call the
    /// network carries.
    fn refusal(
        &self,
        signing_name: &str,
        operation: &str,
        region: &str,
        body: &str,
    ) -> Option<RefusedCall>;
}

/// The VPC ENDPOINTS a machine's traffic leaves through, as the gate a call is decided by.
///
/// Built from the endpoints the service that owns them holds for the machine's own VPC and the
/// identity the machine's calls are signed as, both read ONCE — before the program runs — so
/// nothing of the run reaches a store.
#[derive(Debug, Clone)]
pub struct VpcEndpointEgress {
    /// The policies in force on this machine's traffic, one per endpoint.
    policies: Vec<ServiceEndpointPolicy>,
    /// The ARN the machine's calls are signed as, which a refusal names.
    principal: String,
    /// The account the machine's resources are in, which the ARN of a regional resource
    /// carries.
    account: String,
}

impl VpcEndpointEgress {
    /// The gate the endpoints `policies` describe make for a machine whose calls are signed as
    /// `principal`, in `account` — `None` where there is nothing to decide, which is a machine
    /// whose VPC holds no endpoint at all.
    #[must_use]
    pub fn of(policies: Vec<ServiceEndpointPolicy>, principal: &str, account: &str) -> Option<Self> {
        (!policies.is_empty() && !principal.is_empty()).then(|| Self {
            policies,
            principal: principal.to_string(),
            account: account.to_string(),
        })
    }
}

impl CallEgress for VpcEndpointEgress {
    fn refusal(
        &self,
        signing_name: &str,
        operation: &str,
        region: &str,
        body: &str,
    ) -> Option<RefusedCall> {
        let document = ::serde_json::from_str(body).unwrap_or(::serde_json::Value::Null);
        let addressed = addressed_call(
            signing_name,
            operation,
            region,
            &self.account,
            &document,
        )?;
        refusal_through_endpoints(&self.policies, &self.principal, &addressed)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        addressed_call, endpoint_service_token, refusal_through_endpoints, ServiceEndpointPolicy,
    };

    /// The full-access document EC2 stores for an endpoint created without one.
    const FULL_ACCESS: &str =
        r#"{"Statement":[{"Effect":"Allow","Principal":"*","Action":"*","Resource":"*"}]}"#;

    /// The session an instance's calls are made as.
    const PRINCIPAL: &str = "arn:aws:sts::000000000000:assumed-role/node/i-0123456789abcdef0";

    fn policy(service_token: &str, document: &str) -> ServiceEndpointPolicy {
        ServiceEndpointPolicy {
            endpoint_id: "vpce-0123456789abcdef0".to_string(),
            service_token: service_token.to_string(),
            policy_document: document.to_string(),
        }
    }

    /// RULE: the token is the LAST segment of an AWS-owned endpoint service name, and a value
    /// that names no such service has none. The rows differ along the FORM of the name.
    #[test]
    fn the_token_is_the_service_segment_of_an_endpoint_service_name() {
        let rows: [(&str, &str, Option<&str>); 6] = [
            ("a gateway service", "com.amazonaws.us-east-1.s3", Some("s3")),
            (
                "another Region's",
                "com.amazonaws.eu-west-2.dynamodb",
                Some("dynamodb"),
            ),
            (
                "an interface service",
                "com.amazonaws.us-east-1.ssm",
                Some("ssm"),
            ),
            (
                "another account's service",
                "com.amazonaws.vpce.us-east-1.vpce-svc-0123456789abcdef0",
                None,
            ),
            ("a name with no service", "com.amazonaws.us-east-1", None),
            ("a name of no AWS service", "my.own.service", None),
        ];
        for (case, name, expected) in rows {
            let read = endpoint_service_token(name);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// RULE: a request that names an OBJECT is authorized on that object's ARN and one that
    /// names only a bucket on the bucket's; a service whose requests this module does not read
    /// addresses nothing. The rows differ along which members the request carries and which
    /// service it addresses.
    #[test]
    fn a_request_is_authorized_on_the_resource_its_members_name() {
        let rows: [(&str, &str, &str, ::serde_json::Value, Option<&str>); 5] = [
            (
                "an object write",
                "s3",
                "PutObject",
                ::serde_json::json!({"Bucket": "pail", "Key": "under/it"}),
                Some("arn:aws:s3:::pail/under/it"),
            ),
            (
                "a bucket listing",
                "s3",
                "ListObjectsV2",
                ::serde_json::json!({"Bucket": "pail"}),
                Some("arn:aws:s3:::pail"),
            ),
            (
                "a request naming no bucket",
                "s3",
                "ListBuckets",
                ::serde_json::json!({}),
                None,
            ),
            (
                "a table write",
                "dynamodb",
                "PutItem",
                ::serde_json::json!({"TableName": "rows"}),
                Some("arn:aws:dynamodb:us-east-1:000000000000:table/rows"),
            ),
            (
                "a service this module does not read",
                "sqs",
                "SendMessage",
                ::serde_json::json!({"QueueUrl": "https://example.invalid/q"}),
                None,
            ),
        ];
        for (case, service, operation, body, expected) in rows {
            let read = addressed_call(service, operation, "us-east-1", "000000000000", &body);
            let resource = read.as_ref().map(|call| call.resource.as_str());
            assert_eq!(
                resource, expected,
                "case {case:?}: expected {expected:?}, got {resource:?}"
            );
        }
    }

    /// RULE: a call travels only where an endpoint for the called service allows it — an
    /// explicit deny refuses it, a document that states nothing about it grants nothing, and an
    /// account whose VPC holds no endpoint for the service has no endpoint policy on the call.
    ///
    /// The rows differ along WHAT the endpoint's document says and WHICH service it is for.
    #[test]
    fn an_endpoint_policy_decides_a_call_of_the_service_it_is_for() {
        let addressed = addressed_call(
            "s3",
            "PutObject",
            "us-east-1",
            "000000000000",
            &::serde_json::json!({"Bucket": "pail", "Key": "under/it"}),
        )
        .expect("an object write addresses the object");
        let deny = r#"{"Statement":[{"Effect":"Allow","Principal":"*","Action":"*",
             "Resource":"*"},{"Effect":"Deny","Principal":"*","Action":"s3:PutObject",
             "Resource":"arn:aws:s3:::pail/*"}]}"#;
        let reads_only = r#"{"Statement":[{"Effect":"Allow","Principal":"*",
             "Action":"s3:GetObject","Resource":"arn:aws:s3:::pail/*"}]}"#;
        let rows: [(&str, Vec<ServiceEndpointPolicy>, bool); 6] = [
            ("no endpoint at all", Vec::new(), true),
            (
                "an endpoint for another service",
                vec![policy("dynamodb", reads_only)],
                true,
            ),
            (
                "the default full-access policy",
                vec![policy("s3", FULL_ACCESS)],
                true,
            ),
            ("a policy denying the write", vec![policy("s3", deny)], false),
            (
                "a policy allowing only reads",
                vec![policy("s3", reads_only)],
                false,
            ),
            (
                "a denying endpoint beside a permitting one",
                vec![policy("s3", FULL_ACCESS), policy("s3", deny)],
                false,
            ),
        ];
        for (case, policies, travels) in rows {
            let refusal = refusal_through_endpoints(&policies, PRINCIPAL, &addressed);
            assert_eq!(
                refusal.is_none(),
                travels,
                "case {case:?}: expected the call to travel={travels}, got {refusal:?}"
            );
            let Some(refusal) = refusal else {
                continue;
            };
            assert!(
                refusal.message.contains("s3:PutObject")
                    && refusal.message.contains("arn:aws:s3:::pail/under/it")
                    && refusal.message.contains(PRINCIPAL),
                "case {case:?}: expected the refusal to name the principal, the action and the \
                 resource, got {:?}",
                refusal.message
            );
        }
    }
}
