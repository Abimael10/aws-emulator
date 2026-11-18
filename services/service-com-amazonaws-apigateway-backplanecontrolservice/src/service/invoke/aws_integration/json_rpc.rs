// SPDX-License-Identifier: CC0-1.0

//! The `AWS` integration whose `uri` names an action of an AWS JSON-RPC service: how a method
//! request becomes ONE operation of that api, made in process.
//!
//! `Integration$uri` gives the form an AWS service action is named in —
//! `arn:aws:apigateway:{region}:{subdomain.service|service}:path|action/{service_api}` — and a
//! JSON-RPC api (the `awsJson1_0` and `awsJson1_1` protocols) names its operation in the
//! `X-Amz-Target` header and carries every input of it in a JSON object body. That makes ONE
//! rule serve every such api, and a service is served by stating the wire facts its own model
//! publishes ([`JsonRpcApi`]) rather than by a second copy of the rule:
//!
//! * the `action/{Action}` form of the `uri` names the OPERATION, which is the only thing the
//!   `uri` decides — such an api fixes no input in a query parameter, because its inputs all
//!   travel in the body, and it publishes no REST path for the `path` form to address;
//! * the integration request's own BODY — the request template this call selects, RENDERED
//!   (`Integration$requestTemplates`) — IS the operation's input document, which is why an
//!   integration to one of these apis carries a template that writes one;
//! * the RESOURCE the call addresses is the one THAT DOCUMENT names, by the identifier members
//!   the api publishes for it ([`JsonRpcApi::arn_members`] and [`JsonRpcApi::name_members`]),
//!   which is what an identity-based policy grants the action on. A document that names none —
//!   a batch or an account-wide action — addresses every resource of the account.
//!
//! The call is made THROUGH THE EMULATOR this service runs within, over the in-process
//! transport `OperationContext::callback` hands a handler: nothing is invented and no
//! connection is opened. What the addressed service answered in process is what the method
//! response carries — its status, its media type and its bytes, refusals included — for the
//! integration response to map, so a template written against the service's own answer
//! (`$input.path('$.ShardId')`) reads exactly what that service sent.
//!
//! Who the call is made as — the role `Integration$credentials` names — and what a role that
//! may not make it earns is the one rule the parent module states for every `AWS` integration;
//! this module only names the action a call is authorized as ([`Call::iam_action`]), the
//! resource it is authorized on ([`Call::resource_arn`]) and the refusal ([`access_denied`]).

use ::serde_json::Value;
use ::smithy_server_runtime::OperationContext;

use crate::ops::types::GetIntegrationOutput;

use super::super::{MethodRequest, UnservedIntegration};
use super::{
    integration_request_body, named_service, substituted, unanswered, Answered,
    ACTION_FORM_PREFIX, APIGATEWAY_VENDOR,
};

/// The wire facts of ONE AWS JSON-RPC api an `AWS` integration can address, each read from
/// that service's own model. Nothing else about a service is needed to place a call against
/// it, which is what lets one rule serve them all.
#[derive(Debug)]
pub(super) struct JsonRpcApi {
    /// The subdomain field of the integration ARN that names this service, which is also the
    /// namespace its IAM actions are published under and the name its calls are signed with
    /// (a model's `arnNamespace` and `sigv4Name`).
    pub(super) subdomain: &'static str,
    /// What the service is called in the sentence an integration that reached no answer
    /// carries.
    pub(super) service_name: &'static str,
    /// The service shape an `X-Amz-Target` names an operation of this api under.
    pub(super) target_prefix: &'static str,
    /// The media type a request to this api and its answer carry.
    pub(super) media_type: &'static str,
    /// The resource type a call that names no resource of its own addresses EVERY resource of
    /// — a batch or an account-wide action, which this api's primary resource type is the
    /// scope of.
    ///
    /// The type-scoped wildcard rather than a bare `*`: an identity-based policy for such an
    /// action is written either on `*`, which matches any resource ARN including this one, or
    /// on that type's own wildcard (`stream/*`, `table/*`), which matches this one and not a
    /// bare `*` — so naming the type keeps both spellings of the grant in force.
    pub(super) resource_type: &'static str,
    /// The members an input document names its resource in BY ARN, in the order a request
    /// narrows what it addresses: the first one the document carries names the resource
    /// outright, whichever of the api's resource types it is of.
    pub(super) arn_members: &'static [&'static str],
    /// The members an input document names its resource in BY NAME, each with the resource
    /// type that name is of — asked after the ARN members, because a request carrying both
    /// addresses the ARN's resource. A name becomes the `{resource-type}/{name}` path of an
    /// ARN in the call's own partition, Region and account.
    pub(super) name_members: &'static [(&'static str, &'static str)],
}

/// Every AWS JSON-RPC api this data plane places an `AWS` integration's call against. The
/// subdomain of the integration ARN picks one of them, so serving another service is one
/// more entry here and nothing else.
const SERVED: &[&JsonRpcApi] = &[&super::dynamodb::API, &super::kinesis::API];

/// The status one of these apis refuses a call the caller may not make with: each declares
/// `AccessDeniedException` a CLIENT error carrying no explicit `@httpError`, which the
/// protocol defaults to 400 (`smithy_query(query="operation", operation="PutRecord")` on the
/// stream api, `operation="GetItem"` on the table api).
const REFUSAL_STATUS: u16 = 400;

/// The shape each of these apis names an authorization refusal by, in its own namespace
/// (`com.amazonaws.{subdomain}#AccessDeniedException`).
const ACCESS_DENIED_TYPE: &str = "AccessDeniedException";

/// The header a JSON-RPC request names its operation in.
const TARGET_HEADER: &str = "x-amz-target";

/// What the input document of a call names as the resource it addresses.
#[derive(Debug, PartialEq, Eq)]
enum Addressed {
    /// An ARN the document carried, which names the resource outright.
    Arn(String),
    /// A name the document carried, with the resource type it is of: it names a resource in
    /// the call's own partition, Region and account.
    Named {
        resource_type: &'static str,
        name: String,
    },
    /// A document that names no resource of its own, which addresses every resource of that
    /// type in the account.
    Any,
}

/// One call of a JSON-RPC api an `AWS` integration addresses: the api, the operation, the
/// resource its input document names and the document itself.
#[derive(Debug)]
pub(super) struct Call {
    /// The api the call is placed against.
    api: &'static JsonRpcApi,
    /// The partition of the integration ARN, which the resource ARN is scoped to.
    partition: String,
    /// The Region the call is made at: the integration ARN's own, else the api's.
    region: String,
    /// The operation the `action` form names, e.g. `PutRecord`.
    action: String,
    /// The resource the integration request's own document addresses.
    addressed: Addressed,
    /// The integration request's body: the rendered request template, which IS the
    /// operation's input document.
    body: Vec<u8>,
}

impl Call {
    /// The Region of the service endpoint the call is made at.
    pub(super) fn region(&self) -> &str {
        &self.region
    }

    /// The IAM action a call of it is authorized by: `{namespace}:{Action}`, the operation the
    /// `uri` names under the addressed service's own action namespace.
    pub(super) fn iam_action(&self) -> String {
        format!("{}:{}", self.api.subdomain, self.action)
    }

    /// The ARN of the resource the call addresses, which is what an identity-based policy
    /// grants that action on.
    pub(super) fn resource_arn(&self) -> String {
        let (resource_type, name) = match &self.addressed {
            // An ARN the document carried names the resource outright, whatever type it is of:
            // an api that publishes several resource types is addressed by the ARN its own
            // input carries.
            Addressed::Arn(arn) => return arn.clone(),
            Addressed::Named {
                resource_type,
                name,
            } => (*resource_type, name.as_str()),
            Addressed::Any => (self.api.resource_type, "*"),
        };
        format!(
            "arn:{partition}:{namespace}:{region}:{account}:{resource_type}/{name}",
            partition = self.partition,
            namespace = self.api.subdomain,
            region = self.region,
            account = ::emulator_server::EMULATOR_ACCOUNT_ID,
        )
    }

    /// The integration request this call CARRIES to the service: the rendered request
    /// template, which IS the operation's input document on this wire — the bytes the call
    /// below posts, so the stage's execution log reports what was really sent.
    pub(super) fn sent(&self) -> &[u8] {
        &self.body
    }
}

/// What `integration`'s `uri` addresses in one of the JSON-RPC apis this module serves:
///
/// * `None` when the `uri` names none of them at all — another service's action, or no ARN —
///   which leaves it to the next reader;
/// * `Some(Err(unserved))` when it DOES name one and no call can be placed against it, which
///   is a configuration this stage cannot serve;
/// * `Some(Ok(call))` for the call to make.
pub(super) fn addressed(
    integration: &GetIntegrationOutput,
    default_region: &str,
    call: &MethodRequest,
) -> Option<Result<Call, UnservedIntegration>> {
    let uri = integration.uri.as_deref()?;
    let arn = ::service_common::arn::parse(uri)?;
    if arn.vendor != APIGATEWAY_VENDOR {
        return None;
    }
    let service = named_service(arn.account);
    let api = SERVED.iter().copied().find(|api| api.subdomain == service)?;
    Some(resolved(api, integration, &arn, default_region, call))
}

/// The call the integration ARN `arn` names against `api`, with every `{placeholder}` of its
/// resource resolved against this request and the integration request's own document
/// rendered.
fn resolved(
    api: &'static JsonRpcApi,
    integration: &GetIntegrationOutput,
    arn: &::service_common::arn::Arn<'_>,
    default_region: &str,
    call: &MethodRequest,
) -> Result<Call, UnservedIntegration> {
    let region = match arn.region.is_empty() {
        true => default_region,
        false => arn.region,
    };
    let resource =
        substituted(arn.resource, integration, &call.inputs).map_err(|reason| unanswered(&reason))?;
    // Only the ACTION form addresses these apis: the `path` form is a service's own REST path,
    // and a JSON-RPC api publishes none — every operation of it is reached at the service's
    // root.
    let Some(named) = resource.strip_prefix(ACTION_FORM_PREFIX) else {
        return Err(unanswered(&format!(
            "the integration's uri does not address the action form of a call to {service}, \
             which is the only form that api is reached by",
            service = api.service_name,
        )));
    };
    // Nothing of the resource past the action name is read: the `&{parameter}={value}` tail
    // names inputs of a QUERY api, and every input of one of these travels in the body.
    let action = named.split('&').next().unwrap_or_default();
    if action.is_empty() || !action.chars().all(char::is_alphanumeric) {
        return Err(unanswered(&format!(
            "the integration's uri names {action:?}, which is no {service} operation",
            service = api.service_name,
        )));
    }
    let body = integration_request_body(integration, call)?;
    Ok(Call {
        api,
        partition: arn.partition.to_string(),
        region: region.to_string(),
        action: action.to_string(),
        addressed: addressed_resource(api, &body),
        body,
    })
}

/// The resource the integration request's document addresses, by the identifier members `api`
/// publishes for one: an ARN member it carries, else a name member it carries, else no
/// resource of its own.
fn addressed_resource(api: &JsonRpcApi, body: &[u8]) -> Addressed {
    let document = ::serde_json::from_slice::<Value>(body).ok();
    let text = |member: &str| {
        document
            .as_ref()
            .and_then(|document| document.get(member))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    if let Some(arn) = api.arn_members.iter().copied().find_map(text) {
        return Addressed::Arn(arn);
    }
    if let Some((resource_type, name)) = api
        .name_members
        .iter()
        .find_map(|(member, resource_type)| text(member).map(|name| (*resource_type, name)))
    {
        return Addressed::Named {
            resource_type,
            name,
        };
    }
    Addressed::Any
}

/// What one of these apis answers a call the integration's role may not make: the refusal its
/// protocol words an unauthorized call in — the principal the call would have been made as,
/// the action it asked for, the resource it addressed and the clause IAM closes an implicit
/// deny with, through the tree's one formatter for that sentence
/// ([`::service_common::iam_authorization::not_authorized_by_identity_policy`]).
pub(super) fn access_denied(call: &Call, principal: &str) -> Answered {
    let action = call.iam_action();
    let body = ::serde_json::json!({
        "__type": format!(
            "com.amazonaws.{namespace}#{ACCESS_DENIED_TYPE}",
            namespace = call.api.subdomain,
        ),
        "message": ::service_common::iam_authorization::not_authorized_by_identity_policy(
            principal,
            &action,
            &call.resource_arn(),
            &::service_common::iam_authorization::no_identity_policy_allows(&action),
        ),
    });
    Answered {
        status: REFUSAL_STATUS,
        content_type: Some(call.api.media_type.to_string()),
        body: body.to_string().into_bytes(),
    }
}

/// Make the call `call` addresses against the api its `uri` named, in process, and report what
/// it answered.
///
/// The request is the one that api takes: its operation in the `X-Amz-Target` header, the
/// integration request's document as the body, signed for that service in the call's Region by
/// the tree's one in-process signer with the credential the callback hands this handler —
/// which is never reported or logged.
///
/// `Err` only where there is no answer FROM THE SERVICE to report at all — the request could
/// not be built or signed, it reached no such service in this emulator, or the answer is
/// larger than one method response may carry. Every answer the service DID give, refusals
/// included, is reported as [`Answered`] and mapped.
pub(super) async fn answered(
    ctx: &OperationContext<'_>,
    call: &Call,
) -> Result<Answered, UnservedIntegration> {
    let host = format!("{}.{}.amazonaws.com", call.api.subdomain, call.region);
    let built = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header(::http::header::HOST, &host)
        .header(::http::header::CONTENT_TYPE, call.api.media_type)
        .header(
            TARGET_HEADER,
            format!("{}.{}", call.api.target_prefix, call.action),
        )
        .body(call.body.clone());
    let service = call.api.service_name;
    let request = built.map_err(|error| {
        ::tracing::warn!(%error, service, "the integration's call does not render an HTTP request");
        unanswered(&format!(
            "the integration's uri does not render a request to {service}, so no call was made"
        ))
    })?;
    super::answered_in_process(
        ctx,
        request,
        call.api.subdomain,
        &call.region,
        service,
        Some(call.api.media_type),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Call` of `action` against `api`, addressing what `body` names, as [`resolved`] builds
    /// one.
    fn call(api: &'static JsonRpcApi, action: &str, body: &str) -> Call {
        Call {
            api,
            partition: "aws".to_string(),
            region: "eu-west-1".to_string(),
            action: action.to_string(),
            addressed: addressed_resource(api, body.as_bytes()),
            body: body.as_bytes().to_vec(),
        }
    }

    /// RULE: a call is authorized as the operation the `uri` names, under the addressed
    /// service's own action namespace, on the resource that service's input document names.
    #[test]
    fn the_call_is_authorized_as_its_operation_on_the_resource_the_document_names() {
        /// (case, the api, the action, the document, the IAM action, the resource)
        type Row<'a> = (
            &'a str,
            &'static JsonRpcApi,
            &'a str,
            &'a str,
            &'a str,
            &'a str,
        );
        let rows: &[Row<'_>] = &[
            (
                "a table action naming its table",
                &super::super::dynamodb::API,
                "GetItem",
                r#"{"TableName":"capacity","Key":{"id":{"S":"a"}}}"#,
                "dynamodb:GetItem",
                "arn:aws:dynamodb:eu-west-1:000000000000:table/capacity",
            ),
            (
                "a stream write naming its stream by name",
                &super::super::kinesis::API,
                "PutRecord",
                r#"{"StreamName":"orders","Data":"aGk=","PartitionKey":"k"}"#,
                "kinesis:PutRecord",
                "arn:aws:kinesis:eu-west-1:000000000000:stream/orders",
            ),
            (
                "a stream write naming its stream by ARN, which the name beside it does not \
                 override",
                &super::super::kinesis::API,
                "PutRecords",
                r#"{"StreamName":"orders","StreamARN":"arn:aws:kinesis:us-east-2:000000000000:stream/other","Records":[]}"#,
                "kinesis:PutRecords",
                "arn:aws:kinesis:us-east-2:000000000000:stream/other",
            ),
            (
                "a channel action naming its channel, a resource of another type of the same \
                 api",
                &super::super::kinesis::API,
                "DescribeChannel",
                r#"{"ChannelName":"ingest"}"#,
                "kinesis:DescribeChannel",
                "arn:aws:kinesis:eu-west-1:000000000000:channel/ingest",
            ),
            (
                "a table request naming no table",
                &super::super::dynamodb::API,
                "ListTables",
                r#"{"Limit":10}"#,
                "dynamodb:ListTables",
                "arn:aws:dynamodb:eu-west-1:000000000000:table/*",
            ),
            (
                "a stream request naming no stream",
                &super::super::kinesis::API,
                "ListStreams",
                r#"{"Limit":10}"#,
                "kinesis:ListStreams",
                "arn:aws:kinesis:eu-west-1:000000000000:stream/*",
            ),
        ];
        for (case, api, action, body, expected_action, expected_resource) in rows {
            let call = call(api, action, body);
            assert_eq!(
                call.iam_action(),
                *expected_action,
                "case {case:?}: expected the action {expected_action:?}, got {:?}",
                call.iam_action()
            );
            assert_eq!(
                call.resource_arn(),
                *expected_resource,
                "case {case:?}: expected the resource {expected_resource:?}, got {:?}",
                call.resource_arn()
            );
        }
    }

    /// RULE: the resource a call addresses is read off its OWN input document — the ARN
    /// members of that api first, then its name members — and a document that names none
    /// addresses every resource of the account.
    #[test]
    fn the_resource_is_the_one_the_integration_request_document_names() {
        /// (case, the api, the document, what it addresses)
        type Row<'a> = (&'a str, &'static JsonRpcApi, &'a str, Addressed);
        let rows: &[Row<'_>] = &[
            (
                "a table name",
                &super::super::dynamodb::API,
                r#"{"TableName":"other","Item":{}}"#,
                Addressed::Named {
                    resource_type: "table",
                    name: "other".to_string(),
                },
            ),
            (
                "a stream name",
                &super::super::kinesis::API,
                r#"{"StreamName":"orders"}"#,
                Addressed::Named {
                    resource_type: "stream",
                    name: "orders".to_string(),
                },
            ),
            (
                "a channel name, which is a resource of another type of the same api",
                &super::super::kinesis::API,
                r#"{"ChannelName":"ingest"}"#,
                Addressed::Named {
                    resource_type: "channel",
                    name: "ingest".to_string(),
                },
            ),
            (
                "a consumer ARN, which is its own resource type",
                &super::super::kinesis::API,
                r#"{"ConsumerARN":"arn:aws:kinesis:eu-west-1:000000000000:stream/orders/consumer/c:1"}"#,
                Addressed::Arn(
                    "arn:aws:kinesis:eu-west-1:000000000000:stream/orders/consumer/c:1".to_string(),
                ),
            ),
            (
                "a tagging call's resource ARN",
                &super::super::kinesis::API,
                r#"{"ResourceARN":"arn:aws:kinesis:eu-west-1:000000000000:stream/orders","Tags":{}}"#,
                Addressed::Arn(
                    "arn:aws:kinesis:eu-west-1:000000000000:stream/orders".to_string(),
                ),
            ),
            (
                "an empty identifier, which names nothing",
                &super::super::kinesis::API,
                r#"{"StreamName":""}"#,
                Addressed::Any,
            ),
            (
                "a document naming no resource",
                &super::super::kinesis::API,
                r#"{"Limit":10}"#,
                Addressed::Any,
            ),
            (
                "a body that is no document at all",
                &super::super::kinesis::API,
                "not json",
                Addressed::Any,
            ),
            ("no body at all", &super::super::kinesis::API, "", Addressed::Any),
        ];
        for (case, api, body, expected) in rows {
            let actual = addressed_resource(api, body.as_bytes());
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a refusal of the integration's role carries the class the addressed api words an
    /// unauthorized call in, naming the principal, the action and the resource.
    #[test]
    fn an_unauthorized_call_is_refused_the_way_the_addressed_api_words_it() {
        const ROLE: &str = "arn:aws:iam::000000000000:role/apigateway-integration";
        /// (case, the api, the action, the document, the `__type`, the resource the sentence
        /// names)
        type Row<'a> = (
            &'a str,
            &'static JsonRpcApi,
            &'a str,
            &'a str,
            &'a str,
            &'a str,
        );
        let rows: &[Row<'_>] = &[
            (
                "a table write",
                &super::super::dynamodb::API,
                "PutItem",
                r#"{"TableName":"capacity"}"#,
                "com.amazonaws.dynamodb#AccessDeniedException",
                "arn:aws:dynamodb:eu-west-1:000000000000:table/capacity",
            ),
            (
                "a stream write",
                &super::super::kinesis::API,
                "PutRecord",
                r#"{"StreamName":"orders","Data":"aGk=","PartitionKey":"k"}"#,
                "com.amazonaws.kinesis#AccessDeniedException",
                "arn:aws:kinesis:eu-west-1:000000000000:stream/orders",
            ),
        ];
        for (case, api, action, body, expected_type, expected_resource) in rows {
            let refused = access_denied(&call(api, action, body), ROLE);
            assert_eq!(
                refused.status, REFUSAL_STATUS,
                "case {case:?}: expected the status that api refuses a client-side failure \
                 with, got {}",
                refused.status
            );
            let document: Value = ::serde_json::from_slice(&refused.body)
                .expect("the refusal carries the service's own JSON envelope");
            assert_eq!(
                document["__type"],
                Value::String((*expected_type).to_string()),
                "case {case:?}: expected the class the service names it by, got {document:?}"
            );
            let message = document["message"].as_str().unwrap_or_default();
            let action = format!("{}:{action}", api.subdomain);
            assert_eq!(
                message,
                format!(
                    "User: {ROLE} is not authorized to perform: {action} on resource: \
                     {expected_resource} because no identity-based policy allows the {action} \
                     action"
                ),
                "case {case:?}: expected the sentence IAM words an authorization failure in, \
                 got {message:?}"
            );
        }
    }
}
