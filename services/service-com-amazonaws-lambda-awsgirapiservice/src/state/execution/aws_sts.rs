// SPDX-License-Identifier: CC0-1.0

//! The security-token half of the SDK seam a RUNNING handler calls another service through:
//! the role it assumes, the identity it reports and the session credentials it is vended,
//! built the way that service's own protocol binds them.
//!
//! ROLE CHAINING is a shape AWS documents for a function: the handler calls `AssumeRole` on
//! a second role with the identity of its own execution role, and then calls a service with
//! the session credentials it was vended — which is how a function reaches a resource of
//! another account, or narrows what one of its own calls may do. Both halves are only
//! reachable from a handler if this seam binds the first one, so a handler that cannot reach
//! this service can neither assume a role nor learn which identity it is running as.
//!
//! Most services a handler reaches bind their requests with an AWS JSON protocol, where the
//! request body IS the operation's input document, so one code path serves every operation of
//! every one of them ([`super::aws_api`]). This service does not: its model declares
//! `awsQuery` (`smithy_query(query="service_info")`), so a request to it is a form-encoded
//! `Action` with the input flattened into the parameters beside it, and the answer is an XML
//! document. That binding is not written out here: the request is built, signed, dispatched
//! and parsed by the CLIENT this crate holds (`aws_sdk_sts`), over the same in-process
//! callback transport every other call of this seam uses, so the wire details are the
//! client's and the answer is the emulated token service's own.
//!
//! What a guest can and cannot do here:
//!
//! * The destination is ALWAYS this emulator: the client is given the callback's own transport
//!   and the service's own regional endpoint, so no host name is resolved and no socket is
//!   opened.
//! * The call is signed as the identity the client was built with — the session of the
//!   function's execution role, or the credentials the handler passed that client
//!   ([`super::aws_api::AwsAccess::signing_as`]) — so the token service judges the assume
//!   against the trust policy in force on the role being assumed exactly as it judges any
//!   other signed request, and a trust policy that does not name the calling identity is
//!   answered that service's own authorization refusal rather than a success.
//! * Only the operations in [`BOUND_OPERATIONS`] are bound, and only the input members each
//!   entry names. An operation or a member this seam does not bind is reported as a call that
//!   was NOT MADE, naming it, rather than sent with a value dropped — a dropped member would
//!   make a call that did something other than what the handler asked for.
//! * The answer's own status and document are what the token service answered: a success is
//!   that operation's output members, and a refusal is its own error code, message and status,
//!   which the SDK the guest calls raises as `ClientError` ([`super::aws_sdk_guest`]).
//!
//! How one call's input is read, what the answer carried and how a refusal is handed back are
//! the same for every service reached through an SDK client, so they are [`super::sdk_call`]'s
//! rather than this module's.

use ::aws_sdk_sts::Client;
use ::serde_json::{Map, Value};
use ::smithy_server_runtime::EmbeddedEmulator;

use super::aws_api::{Answer, CallerIdentity, Refusal};
use super::sdk_call::{insert_number, insert_text, insert_timestamp, Input, Observed, PROVIDER_NAME};

/// The service a refusal of one of these calls names, as the AWS SDKs name it.
const SERVICE: &str = "AWS STS";

/// The operations of the token service this seam binds, in the order a refusal lists them.
///
/// One entry per operation [`call`] builds a request for: the surface a handler that chains
/// roles or reports its own identity uses — assuming a role with its own identity or with a
/// web identity token, reading back who it is running as, the session it is vended for a
/// long-term credential or a federated user, which account an access key id belongs to, and
/// the authorization message a refusal encoded. A handler naming any other operation of the
/// service is told the call was not made and why, because a request this module cannot bind is
/// one it would otherwise be guessing at.
pub const BOUND_OPERATIONS: &[&str] = &[
    "AssumeRole",
    "AssumeRoleWithWebIdentity",
    "GetCallerIdentity",
    "GetSessionToken",
    "GetFederationToken",
    "GetAccessKeyInfo",
    "DecodeAuthorizationMessage",
];

/// Make ONE token-service call: `operation`, with `body` as the operation's input document (the
/// keyword arguments the handler passed its client method), in `region`.
pub async fn call(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    region: &str,
    operation: &str,
    body: &str,
    chain: Option<&str>,
) -> Result<Answer, Refusal> {
    let input = Input::read(SERVICE, body)?;
    let observed = Observed::of(SERVICE);
    let client = client(callback, identity, region, observed.clone(), chain);
    match operation {
        "AssumeRole" => assume_role(&client, &input, &observed).await,
        "AssumeRoleWithWebIdentity" => {
            assume_role_with_web_identity(&client, &input, &observed).await
        }
        "GetCallerIdentity" => get_caller_identity(&client, &input, &observed).await,
        "GetSessionToken" => get_session_token(&client, &input, &observed).await,
        "GetFederationToken" => get_federation_token(&client, &input, &observed).await,
        "GetAccessKeyInfo" => get_access_key_info(&client, &input, &observed).await,
        "DecodeAuthorizationMessage" => {
            decode_authorization_message(&client, &input, &observed).await
        }
        other => Err(Refusal::NotMade(format!(
            "the AWS SDK this runtime supplies binds no {other} call on {SERVICE}; the \
             operations it binds are {}",
            BOUND_OPERATIONS.join(", ")
        ))),
    }
}

/// A token-service client over the in-process callback into this same emulator, signing as the
/// identity the SDK client the guest built was given.
///
/// The transport is in-process, so everything a network client needs and this one cannot have —
/// retries, timeouts, identity caching and stalled-stream protection, each of which needs a
/// sleep implementation the callback transport is not given — is switched off.
///
/// Every request it makes carries the tracing header of the chain this invocation belongs to,
/// where the invocation has one ([`::service_common::xray_trace::ChainHop`]): a call the
/// handler makes is one HOP of the chain that reached the handler.
fn client(
    callback: &EmbeddedEmulator,
    identity: Option<&CallerIdentity>,
    region: &str,
    observed: Observed,
    chain: Option<&str>,
) -> Client {
    let config = ::aws_sdk_sts::Config::builder()
        .behavior_version(::aws_sdk_sts::config::BehaviorVersion::latest())
        .region(::aws_sdk_sts::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://sts.{region}.amazonaws.com"))
        .http_client(callback.http_client())
        .interceptor(observed)
        .retry_config(::aws_sdk_sts::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_sts::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_sts::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_sts::config::StalledStreamProtectionConfig::disabled(),
        );
    let config = match identity {
        Some(identity) => {
            config.credentials_provider(::aws_sdk_sts::config::Credentials::new(
                identity.access_key_id.clone(),
                identity.secret_access_key.clone(),
                identity.token().map(str::to_string),
                None,
                PROVIDER_NAME,
            ))
        }
        None => config.credentials_provider(callback.aws_credentials_provider()),
    };
    // A call the handler makes is one HOP of the chain that reached the handler.
    let config = match chain {
        Some(chain) => config.interceptor(::service_common::xray_trace::ChainHop::carrying(chain)),
        None => config,
    };
    Client::from_conf(config.build())
}

/// `AssumeRole`: the session of another role, vended to the identity the call is signed as.
async fn assume_role(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "RoleArn",
        "RoleSessionName",
        "PolicyArns",
        "Policy",
        "DurationSeconds",
        "Tags",
        "TransitiveTagKeys",
        "ExternalId",
        "SerialNumber",
        "TokenCode",
        "SourceIdentity",
    ])?;
    let assumed = client
        .assume_role()
        .role_arn(input.required_text("RoleArn")?)
        .role_session_name(input.required_text("RoleSessionName")?)
        .set_policy_arns(policy_arns(input)?)
        .set_policy(input.text("Policy")?)
        .set_duration_seconds(input.integer("DurationSeconds")?)
        .set_tags(tags(input)?)
        .set_transitive_tag_keys(optional_list(input.text_list("TransitiveTagKeys")?))
        .set_external_id(input.text("ExternalId")?)
        .set_serial_number(input.text("SerialNumber")?)
        .set_token_code(input.text("TokenCode")?)
        .set_source_identity(input.text("SourceIdentity")?)
        .send()
        .await;
    let assumed = match assumed {
        Ok(assumed) => assumed,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_credentials(&mut document, assumed.credentials());
    insert_assumed_role_user(&mut document, assumed.assumed_role_user());
    // The member the model marks deprecated is still ANSWERED by the service and still read
    // back by every SDK client, so a handler reading it reads the service's own value.
    #[allow(deprecated)]
    insert_number(
        &mut document,
        "PackedPolicySize",
        assumed.packed_policy_size().map(i64::from),
    );
    insert_text(&mut document, "SourceIdentity", assumed.source_identity());
    observed.answer(document)
}

/// `AssumeRoleWithWebIdentity`: the same session, vended against a web identity token rather
/// than against the caller's own signature.
async fn assume_role_with_web_identity(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "RoleArn",
        "RoleSessionName",
        "WebIdentityToken",
        "ProviderId",
        "PolicyArns",
        "Policy",
        "DurationSeconds",
    ])?;
    let assumed = client
        .assume_role_with_web_identity()
        .role_arn(input.required_text("RoleArn")?)
        .role_session_name(input.required_text("RoleSessionName")?)
        .web_identity_token(input.required_text("WebIdentityToken")?)
        .set_provider_id(input.text("ProviderId")?)
        .set_policy_arns(policy_arns(input)?)
        .set_policy(input.text("Policy")?)
        .set_duration_seconds(input.integer("DurationSeconds")?)
        .send()
        .await;
    let assumed = match assumed {
        Ok(assumed) => assumed,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_credentials(&mut document, assumed.credentials());
    insert_text(
        &mut document,
        "SubjectFromWebIdentityToken",
        assumed.subject_from_web_identity_token(),
    );
    insert_assumed_role_user(&mut document, assumed.assumed_role_user());
    #[allow(deprecated)]
    insert_number(
        &mut document,
        "PackedPolicySize",
        assumed.packed_policy_size().map(i64::from),
    );
    insert_text(&mut document, "Provider", assumed.provider());
    insert_text(&mut document, "Audience", assumed.audience());
    insert_text(&mut document, "SourceIdentity", assumed.source_identity());
    observed.answer(document)
}

/// `GetCallerIdentity`: who the call is signed as, which for a handler's own client is the
/// session of the function's execution role.
async fn get_caller_identity(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[])?;
    let reported = match client.get_caller_identity().send().await {
        Ok(reported) => reported,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "UserId", reported.user_id());
    insert_text(&mut document, "Account", reported.account());
    insert_text(&mut document, "Arn", reported.arn());
    observed.answer(document)
}

/// `GetSessionToken`: the session a long-term credential is vended for itself.
async fn get_session_token(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["DurationSeconds", "SerialNumber", "TokenCode"])?;
    let vended = client
        .get_session_token()
        .set_duration_seconds(input.integer("DurationSeconds")?)
        .set_serial_number(input.text("SerialNumber")?)
        .set_token_code(input.text("TokenCode")?)
        .send()
        .await;
    let vended = match vended {
        Ok(vended) => vended,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_credentials(&mut document, vended.credentials());
    observed.answer(document)
}

/// `GetFederationToken`: the session vended for a federated user the caller names.
async fn get_federation_token(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["Name", "Policy", "PolicyArns", "DurationSeconds", "Tags"])?;
    let vended = client
        .get_federation_token()
        .name(input.required_text("Name")?)
        .set_policy(input.text("Policy")?)
        .set_policy_arns(policy_arns(input)?)
        .set_duration_seconds(input.integer("DurationSeconds")?)
        .set_tags(tags(input)?)
        .send()
        .await;
    let vended = match vended {
        Ok(vended) => vended,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_credentials(&mut document, vended.credentials());
    if let Some(user) = vended.federated_user() {
        let mut held = Map::new();
        insert_text(&mut held, "FederatedUserId", Some(user.federated_user_id()));
        insert_text(&mut held, "Arn", Some(user.arn()));
        document.insert("FederatedUser".to_string(), Value::Object(held));
    }
    #[allow(deprecated)]
    insert_number(
        &mut document,
        "PackedPolicySize",
        vended.packed_policy_size().map(i64::from),
    );
    observed.answer(document)
}

/// `GetAccessKeyInfo`: which account an access key id belongs to.
async fn get_access_key_info(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["AccessKeyId"])?;
    let reported = client
        .get_access_key_info()
        .access_key_id(input.required_text("AccessKeyId")?)
        .send()
        .await;
    let reported = match reported {
        Ok(reported) => reported,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "Account", reported.account());
    observed.answer(document)
}

/// `DecodeAuthorizationMessage`: the message an authorization refusal encoded, decoded.
async fn decode_authorization_message(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["EncodedMessage"])?;
    let decoded = client
        .decode_authorization_message()
        .encoded_message(input.required_text("EncodedMessage")?)
        .send()
        .await;
    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(&mut document, "DecodedMessage", decoded.decoded_message());
    observed.answer(document)
}

/// The `PolicyArns` of a call, read as the policy descriptors the service takes; `None` where
/// the call carries none, so the member is left off the request rather than sent emptied.
fn policy_arns(
    input: &Input,
) -> Result<Option<Vec<::aws_sdk_sts::types::PolicyDescriptorType>>, Refusal> {
    let mut held = Vec::new();
    for descriptor in input.structures("PolicyArns")? {
        descriptor.only(&["arn"])?;
        held.push(
            ::aws_sdk_sts::types::PolicyDescriptorType::builder()
                .set_arn(descriptor.text("arn")?)
                .build(),
        );
    }
    Ok(optional_list(held))
}

/// The session `Tags` of a call, read as the key/value pairs the service takes.
fn tags(input: &Input) -> Result<Option<Vec<::aws_sdk_sts::types::Tag>>, Refusal> {
    let mut held = Vec::new();
    for tag in input.structures("Tags")? {
        tag.only(&["Key", "Value"])?;
        held.push(
            ::aws_sdk_sts::types::Tag::builder()
                .key(tag.required_text("Key")?)
                .value(tag.required_text("Value")?)
                .build()
                .map_err(|error| {
                    Refusal::NotMade(format!(
                        "a session tag of the call is not one {SERVICE} accepts: {error}"
                    ))
                })?,
        );
    }
    Ok(optional_list(held))
}

/// A list member as the request carries it: absent where the call named none, so a member the
/// handler left out is left out of the request too.
fn optional_list<T>(held: Vec<T>) -> Option<Vec<T>> {
    (!held.is_empty()).then_some(held)
}

/// Put the session CREDENTIALS an answer vended on its document, as the members the operation's
/// own model names them by.
///
/// They are what the caller asked for: a handler that assumed a role calls the next service
/// with them, which is the whole point of the call. The expiry travels as the epoch seconds
/// this seam hands every other service's timestamps over in.
fn insert_credentials(
    document: &mut Map<String, Value>,
    credentials: Option<&::aws_sdk_sts::types::Credentials>,
) {
    let Some(credentials) = credentials else {
        return;
    };
    let mut held = Map::new();
    insert_text(&mut held, "AccessKeyId", Some(credentials.access_key_id()));
    insert_text(
        &mut held,
        "SecretAccessKey",
        Some(credentials.secret_access_key()),
    );
    insert_text(&mut held, "SessionToken", Some(credentials.session_token()));
    insert_timestamp(&mut held, "Expiration", Some(credentials.expiration()));
    document.insert("Credentials".to_string(), Value::Object(held));
}

/// Put the ASSUMED ROLE USER an answer reports on its document: the session's own id and ARN,
/// which is the identity the vended credentials speak as.
fn insert_assumed_role_user(
    document: &mut Map<String, Value>,
    user: Option<&::aws_sdk_sts::types::AssumedRoleUser>,
) {
    let Some(user) = user else {
        return;
    };
    let mut held = Map::new();
    insert_text(&mut held, "AssumedRoleId", Some(user.assumed_role_id()));
    insert_text(&mut held, "Arn", Some(user.arn()));
    document.insert("AssumedRoleUser".to_string(), Value::Object(held));
}
