// SPDX-License-Identifier: CC0-1.0

//! The AUTHORIZATION an `AWS::Serverless::Api` declares (`Auth`), written into the OpenAPI
//! definition the transform gives the `AWS::ApiGateway::RestApi` it expands to.
//!
//! ## The rule this module implements
//!
//! A REST API the transform expands is created by IMPORTING its definition, so the
//! authorization of its methods is part of that document: each authorizer the declaration
//! states becomes a SECURITY SCHEME of the definition, and the api's `DefaultAuthorizer`
//! becomes the SECURITY REQUIREMENT of every method the document leaves uncovered — "the
//! default authorizer applies to all the methods of the API that don't specify an
//! authorizer of their own". A method that states a requirement of its own keeps it, and one
//! that states an EMPTY requirement has said it needs none, so neither inherits the default.
//!
//! That is what makes a declaration's authorization reach the deployed api: API Gateway reads
//! the requirement of each imported operation and creates the method under the authorization
//! the scheme it names stands for, so a method the default covers is deployed with that
//! authorization rather than open. A default that is never propagated deploys every uncovered
//! route of the api to anonymous callers.
//!
//! ## What a scheme is written as
//!
//! `AWS_IAM` is the one authorization that is a SCHEME rather than a resource: it requires the
//! caller's SigV4 credential and nothing has to be created for it, so the definition declares
//! a scheme of that name carrying the credential in the `Authorization` header, under the
//! authentication type that names SigV4
//! (`crate::state::provisioning::apigateway_rest_api` imports the api through that service's
//! own `ImportRestApi`, which reads exactly that).
//!
//! An authorizer the declaration states is a resource of the owning service, declared in the
//! document as the `apiKey` scheme its authentication type names, carrying API Gateway's own
//! authorizer extension: a Lambda authorizer's `type` is the payload it is called with
//! (`token` or `request`) with the function to call and the identity it reads, and a user-pool
//! authorizer's is `cognito_user_pools` with the pools whose tokens it accepts. The function's
//! ARN and the pool's are not known until those resources exist, so each travels as the
//! expression the declaration stated, exactly as the integration URI of a route does.
//!
//! ## Bounds
//!
//! One pass over the definition's own `paths` — a document already bounded by the template
//! reader ([`crate::state::template::parse`]) — and one scheme per declared authorizer, so
//! nothing here is sized by anything the expansion did not already hold. The requirement a
//! covered method is given is built ONCE and bounded in its own right
//! ([`MAX_REQUIREMENT_SCOPES`]), so what this pass writes is one bounded value per method
//! rather than the product of two counts the submitted template chooses.
//!
//! What a declaration states BESIDE those two members — the api key it requires of every
//! method, the resource policy that decides who may reach it, the usage plan it is metered by
//! — is expanded by nothing here, as the rest of this transform's partiality is: those are
//! members of the api and of other resources rather than of its definition's methods, and a
//! method's own `Auth` is the method's.

use super::*;

/// The property an API declaration states its authorization under.
pub(super) const AUTH_PROPERTY: &str = "Auth";

/// The `Auth` members this module reads: the authorizer every uncovered method inherits, and
/// the authorizers the api declares.
pub(super) const DEFAULT_AUTHORIZER: &str = "DefaultAuthorizer";
pub(super) const AUTHORIZERS: &str = "Authorizers";

/// The `DefaultAuthorizer` value that is the CALLER's SigV4 credential rather than an
/// authorizer of the api: "AWS_IAM for using AWS IAM permissions"
/// (`Method$authorizationType`).
pub(super) const AWS_IAM: &str = "AWS_IAM";

/// The value that states NO authorization, which is how a method opts out of the default and
/// the one value of it that propagates nothing.
pub(super) const NO_AUTHORIZER: &str = "NONE";

/// The document member an OpenAPI 2.0 definition declares its security schemes under, and the
/// member one operation (or the document) states its requirement in.
const SECURITY_DEFINITIONS: &str = "securityDefinitions";
const SECURITY: &str = "security";

/// The scheme name the SigV4 requirement of an `AWS_IAM` method is declared under, which is
/// the value the declaration itself states for that authorization: the scheme stands for the
/// caller's own credential, so it is named by the authorization rather than after an
/// authorizer of the api.
const SIGV4_SCHEME: &str = AWS_IAM;

/// The member a security scheme states WHAT it carries under, and the one API Gateway's
/// extensions name the authentication type and the authorizer with.
const SCHEME_TYPE: &str = "type";
const SCHEME_NAME: &str = "name";
const SCHEME_IN: &str = "in";
const AUTHTYPE_EXTENSION: &str = "x-amazon-apigateway-authtype";
const AUTHORIZER_EXTENSION: &str = "x-amazon-apigateway-authorizer";

/// The scheme type an authorization carried in a request header is declared as, and the
/// location it is carried in.
const API_KEY_SCHEME: &str = "apiKey";
const IN_HEADER: &str = "header";

/// The header an `AWS_IAM` caller's credential travels in, and the one an authorizer reads
/// its identity from when the declaration names none.
const AUTHORIZATION_HEADER: &str = "Authorization";

/// The authentication types API Gateway names the three authorizations by: the caller's own
/// SigV4 credential, a Lambda authorizer and an Amazon Cognito user pool.
const AUTHTYPE_SIGV4: &str = "awsSigv4";
const AUTHTYPE_CUSTOM: &str = "custom";
const AUTHTYPE_COGNITO: &str = "cognito_user_pools";

/// The `x-amazon-apigateway-authorizer` types each of those is created as.
const AUTHORIZER_TYPE_TOKEN: &str = "token";
const AUTHORIZER_TYPE_REQUEST: &str = "request";
const AUTHORIZER_TYPE_COGNITO: &str = "cognito_user_pools";

/// The members one authorizer of an `AWS::Serverless::Api` declares.
pub(super) const FUNCTION_ARN: &str = "FunctionArn";
pub(super) const FUNCTION_PAYLOAD_TYPE: &str = "FunctionPayloadType";
pub(super) const USER_POOL_ARN: &str = "UserPoolArn";
pub(super) const IDENTITY: &str = "Identity";

/// The `FunctionPayloadType` of a Lambda authorizer called with the WHOLE request, which is
/// the one whose identity sources are the request parts it reads
/// ([`REQUEST_IDENTITY_SOURCES`]). The other payload type takes a single token from one
/// header, and is the documented default: "FunctionPayloadType … Default: TOKEN".
pub(super) const PAYLOAD_TYPE_REQUEST: &str = "REQUEST";

/// The `Identity` members a `REQUEST` authorizer names the request parts it authorizes on,
/// beside the mapping expression each one is read with: API Gateway names a method request
/// parameter `method.request.<section>.<name>`, which is the form an authorizer's
/// `identitySource` states.
pub(super) const REQUEST_IDENTITY_SOURCES: &[(&str, &str)] = &[
    ("Headers", "method.request.header"),
    ("QueryStrings", "method.request.querystring"),
    ("StageVariables", "stageVariables"),
    ("Context", "context"),
];

/// The `Identity` members every authorizer may state: the header a token is read from, how
/// long a result is cached for and the expression a token has to match.
const IDENTITY_HEADER: &str = "Header";
pub(super) const IDENTITY_REAUTHORIZE_EVERY: &str = "ReauthorizeEvery";
const IDENTITY_VALIDATION_EXPRESSION: &str = "ValidationExpression";

/// The member one authorizer's declaration names the SCOPES a method it covers is reached
/// with, which travel as the value of the requirement naming it rather than inside its
/// scheme.
const AUTHORIZATION_SCOPES: &str = "AuthorizationScopes";

/// The most scopes the requirement this expansion writes carries, which is the most a METHOD
/// can carry: API Gateway stores a method's `authorizationScopes` under its own guard on a
/// caller-sized list, so a definition stating more than that is one the import refuses
/// outright. Writing more would grow the document by the PRODUCT of two counts the submitted
/// template chooses — the methods its definition declares and the scopes its authorizer states
/// — and buy nothing, so the list is read up to this many entries and no further.
///
/// Source: that service's own `MAX_COLLECTION_ENTRIES`, the guard it bounds a method's
/// `authorizationScopes` by (`apigateway/src/ops/put_rest_api.rs`'s `bound_imported_method`).
const MAX_REQUIREMENT_SCOPES: usize = 50;

/// The members API Gateway's authorizer extension spells those as.
const AUTHORIZER_URI: &str = "authorizerUri";
const AUTHORIZER_CREDENTIALS: &str = "authorizerCredentials";
const AUTHORIZER_TTL: &str = "authorizerResultTtlInSeconds";
const AUTHORIZER_IDENTITY_SOURCE: &str = "identitySource";
const AUTHORIZER_VALIDATION_EXPRESSION: &str = "identityValidationExpression";
const AUTHORIZER_PROVIDER_ARNS: &str = "providerARNs";

/// The member a Lambda authorizer's declaration names the role the api assumes to call the
/// function under.
const FUNCTION_INVOKE_ROLE: &str = "FunctionInvokeRole";

/// The keys of an OpenAPI path item that name a METHOD of the path rather than something
/// about the path itself: the Path Item Object's own operation fields, plus API Gateway's
/// extension for the `ANY` catch-all. Source: the same table the import reads a definition's
/// operations with (`crate::ops::put_rest_api::OPERATION_KEYS` of the API Gateway crate).
const METHOD_KEYS: &[&str] = &[
    "get",
    "put",
    "post",
    "delete",
    "options",
    "head",
    "patch",
    "x-amazon-apigateway-any-method",
];

/// Write what `properties`' `Auth` states into the api's OpenAPI definition `body`: the
/// security scheme of each declared authorizer, and the default authorizer as the security
/// requirement of every method the document leaves uncovered.
///
/// A definition that is not an object — one left as an expression for CloudFormation to
/// resolve, or a document held in Amazon S3, which is not in the template at all — is left
/// exactly as it was written, as every other member the expansion states inside a definition
/// is ([`super::http_definition_members`]).
pub(super) fn apply(body: &mut Value, properties: &Value) {
    let auth = object(properties.get(AUTH_PROPERTY));
    if auth.is_empty() {
        return;
    }
    let authorizers = object(auth.get(AUTHORIZERS));
    let Some(document) = body.as_object_mut() else {
        return;
    };
    // Every declared authorizer becomes a scheme of the document, whether the default names
    // it or a method does: the authorizers an api declares are the api's, and a requirement
    // naming one resolves against the document that declares it.
    let mut schemes: Vec<(String, Value)> = authorizers
        .iter()
        .filter_map(|(name, declaration)| {
            security_scheme(declaration).map(|scheme| (name.clone(), scheme))
        })
        .collect();
    let declared_default = auth.get(DEFAULT_AUTHORIZER).and_then(Value::as_str);
    let requirement = match declared_default {
        // The caller's own credential: the scheme stands for the authorization, so it is
        // declared here rather than created anywhere, and it carries no scopes of its own.
        Some(AWS_IAM) => {
            schemes.push((SIGV4_SCHEME.to_string(), sigv4_scheme()));
            Some((SIGV4_SCHEME.to_string(), Vec::new()))
        }
        // A default that states no authorization covers nothing, and neither does one naming
        // an authorizer the api does not declare — which the transform refuses outright
        // ([`super::validation`]), so nothing is written for it here either.
        Some(NO_AUTHORIZER) | None => None,
        // ...and neither does one whose declaration this module can build no scheme FROM: a
        // requirement naming a scheme the document does not declare resolves to nothing, so
        // the default is written only where its own scheme is.
        Some(name) => schemes
            .iter()
            .any(|(declared, _)| declared == name)
            .then(|| {
                (
                    name.to_string(),
                    authorization_scopes(authorizers.get(name)),
                )
            }),
    };
    if !schemes.is_empty() {
        let definitions = document
            .entry(SECURITY_DEFINITIONS.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(definitions) = definitions.as_object_mut() {
            for (name, scheme) in schemes {
                definitions.entry(name).or_insert(scheme);
            }
        }
    }
    let Some((requirement, scopes)) = requirement else {
        return;
    };
    cover_uncovered_methods(document, &requirement, &scopes);
}

/// The scopes a declared authorizer states a method it covers is reached with
/// (`AuthorizationScopes`, "the authorization scopes to apply to the default authorizer"),
/// which travel as the VALUE of the security requirement naming it — and the empty list for a
/// declaration that states none, which is how a requirement with no scope constraint is
/// written.
///
/// Read up to [`MAX_REQUIREMENT_SCOPES`] entries, the most a method can carry: that is what
/// keeps what this expansion writes from being the product of two counts the submitted
/// template chooses.
fn authorization_scopes(declaration: Option<&Value>) -> Vec<Value> {
    declaration
        .and_then(|declaration| declaration.get(AUTHORIZATION_SCOPES))
        .and_then(Value::as_array)
        .map(|scopes| scopes.iter().take(MAX_REQUIREMENT_SCOPES).cloned().collect())
        .unwrap_or_default()
}

/// State `requirement` as the security requirement of every method of `document`'s paths
/// that states none, under the `scopes` that authorization is reached with.
///
/// A method that states one of its own — including the EMPTY requirement, which states that
/// the method needs no authorization — is left exactly as the document wrote it: the default
/// covers what the document leaves uncovered and overrides nothing.
///
/// The requirement is built ONCE and copied onto each uncovered method, so this pass costs one
/// bounded value per method rather than re-building one per method from the two counts the
/// submitted template chose ([`MAX_REQUIREMENT_SCOPES`] bounds the one of them this expansion
/// writes).
fn cover_uncovered_methods(
    document: &mut Map<String, Value>,
    requirement: &str,
    scopes: &[Value],
) {
    let mut named = Map::new();
    named.insert(requirement.to_string(), Value::Array(scopes.to_vec()));
    let stated = Value::Array(vec![Value::Object(named)]);
    let Some(paths) = document
        .get_mut(DEFINITION_PATHS)
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    for item in paths.values_mut() {
        let Some(item) = item.as_object_mut() else {
            continue;
        };
        for (key, operation) in item.iter_mut() {
            if !METHOD_KEYS.contains(&key.to_ascii_lowercase().as_str()) {
                continue;
            }
            let Some(operation) = operation.as_object_mut() else {
                continue;
            };
            if operation.contains_key(SECURITY) {
                continue;
            }
            operation.insert(SECURITY.to_string(), stated.clone());
        }
    }
}

/// The security scheme an `AWS_IAM` requirement resolves against: the caller's SigV4
/// credential, carried in the `Authorization` header, under the authentication type API
/// Gateway names SigV4 by.
fn sigv4_scheme() -> Value {
    json!({
        SCHEME_TYPE: API_KEY_SCHEME,
        SCHEME_NAME: AUTHORIZATION_HEADER,
        SCHEME_IN: IN_HEADER,
        AUTHTYPE_EXTENSION: AUTHTYPE_SIGV4,
    })
}

/// The security scheme ONE declared authorizer becomes, or `None` for a declaration that
/// names neither a function to call nor a user pool to authorize against — which is an
/// authorizer nothing could be created from, so no scheme is invented for it.
fn security_scheme(declaration: &Value) -> Option<Value> {
    let identity = declaration.get(IDENTITY);
    // The two `Identity` members EVERY kind of authorizer may state, read for both kinds: how
    // long its result is cached for and the expression the identity it reads has to match.
    let mut extension = Map::new();
    if let Some(ttl) = identity.and_then(|identity| identity.get(IDENTITY_REAUTHORIZE_EVERY)) {
        extension.insert(AUTHORIZER_TTL.to_string(), ttl.clone());
    }
    if let Some(expression) =
        identity.and_then(|identity| identity.get(IDENTITY_VALIDATION_EXPRESSION))
    {
        extension.insert(
            AUTHORIZER_VALIDATION_EXPRESSION.to_string(),
            expression.clone(),
        );
    }
    if let Some(pools) = declaration.get(USER_POOL_ARN) {
        extension.insert(
            SCHEME_TYPE.to_string(),
            json!(AUTHORIZER_TYPE_COGNITO),
        );
        // "A list of the Amazon Cognito user pool ARNs" is what that authorizer takes, so a
        // declaration naming one pool states a list of one.
        extension.insert(
            AUTHORIZER_PROVIDER_ARNS.to_string(),
            match pools {
                Value::Array(_) => pools.clone(),
                single => Value::Array(vec![single.clone()]),
            },
        );
        return Some(scheme(
            identity_header(identity),
            AUTHTYPE_COGNITO,
            extension,
        ));
    }
    let function = declaration.get(FUNCTION_ARN)?;
    let request_payload = declaration
        .get(FUNCTION_PAYLOAD_TYPE)
        .and_then(Value::as_str)
        == Some(PAYLOAD_TYPE_REQUEST);
    extension.insert(
        SCHEME_TYPE.to_string(),
        json!(if request_payload {
            AUTHORIZER_TYPE_REQUEST
        } else {
            AUTHORIZER_TYPE_TOKEN
        }),
    );
    extension.insert(AUTHORIZER_URI.to_string(), authorizer_uri(function));
    if let Some(role) = declaration.get(FUNCTION_INVOKE_ROLE) {
        extension.insert(AUTHORIZER_CREDENTIALS.to_string(), role.clone());
    }
    let sources = match request_payload {
        true => request_identity_sources(identity),
        // A `TOKEN` authorizer reads one token from one header, which is the identity source
        // that kind of authorizer states.
        false => vec![format!(
            "{}.{}",
            REQUEST_IDENTITY_SOURCES[0].1,
            identity_header(identity)
        )],
    };
    if !sources.is_empty() {
        extension.insert(
            AUTHORIZER_IDENTITY_SOURCE.to_string(),
            json!(sources.join(", ")),
        );
    }
    Some(scheme(
        identity_header(identity),
        AUTHTYPE_CUSTOM,
        extension,
    ))
}

/// One `apiKey` scheme: the header it is carried in, the authentication type it serves and
/// API Gateway's own authorizer extension.
fn scheme(header: String, authtype: &str, extension: Map<String, Value>) -> Value {
    json!({
        SCHEME_TYPE: API_KEY_SCHEME,
        SCHEME_NAME: header,
        SCHEME_IN: IN_HEADER,
        AUTHTYPE_EXTENSION: authtype,
        AUTHORIZER_EXTENSION: Value::Object(extension),
    })
}

/// The header an authorizer reads its identity from: the one its `Identity` names, or the
/// `Authorization` header, which is where a declaration that names none reads it.
fn identity_header(identity: Option<&Value>) -> String {
    identity
        .and_then(|identity| identity.get(IDENTITY_HEADER))
        .and_then(Value::as_str)
        .unwrap_or(AUTHORIZATION_HEADER)
        .to_string()
}

/// The identity sources a `REQUEST` authorizer's `Identity` states, as the mapping
/// expressions that authorizer's `identitySource` lists them in
/// ([`REQUEST_IDENTITY_SOURCES`]), in the order the property publishes them.
pub(super) fn request_identity_sources(identity: Option<&Value>) -> Vec<String> {
    let mut sources = Vec::new();
    for (property, expression) in REQUEST_IDENTITY_SOURCES {
        for name in identity
            .and_then(|identity| identity.get(*property))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            if let Some(name) = name.as_str() {
                sources.push(format!("{expression}.{name}"));
            }
        }
    }
    sources
}

/// The URI an authorizer's function is called on: the API Gateway path that invokes it, with
/// the function's ARN carried as the expression the declaration stated — the ARN is not known
/// until that function exists, exactly as a route's integration URI is
/// ([`super::lambda_integration_uri`]).
fn authorizer_uri(function: &Value) -> Value {
    let template = arn(
        "apigateway:${AWS::Region}:lambda:path/2015-03-31/functions/${__FunctionArn__}/\
         invocations",
    );
    json!({ "Fn::Sub": [template, { "__FunctionArn__": function.clone() }] })
}
