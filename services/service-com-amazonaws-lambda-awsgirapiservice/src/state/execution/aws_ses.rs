// SPDX-License-Identifier: CC0-1.0

//! The Amazon SES half of the SDK seam a RUNNING handler calls another service through: the
//! messages it sends, the identities it verifies and the templates, configuration sets and
//! sending figures it reads, built the way Amazon SES's own protocol binds them.
//!
//! MAILING is one of the shapes a function exists for: a scheduled report, a sign-up
//! confirmation or an alert is composed by a handler and handed to this service, and every
//! downstream signal of such a pipeline (the message id the send answers, the success line the
//! handler logs, the metric a filter over that line feeds) exists only once the send itself is
//! served. Those calls are only reachable from a handler if this seam binds them.
//!
//! Most services a handler reaches bind their requests with an AWS JSON protocol, where the
//! request body IS the operation's input document, so one code path serves every operation of
//! every one of them ([`super::aws_api`]). Amazon SES does not: its model declares `awsQuery`
//! (`smithy_query(query="service_info")`), so a request to it is a form-encoded `Action` with
//! the input flattened into its parameters, and the answer is an XML document. That binding is
//! not written out here: the request is built, signed, dispatched and parsed by the Amazon SES
//! CLIENT this crate holds (`aws_sdk_ses`), over the same in-process callback transport every
//! other call of this seam uses, so the wire details are the client's and the answer is the
//! emulated Amazon SES service's own.
//!
//! This is the service whose SDK id is `SES` (API version 2010-12-01), which an SDK client is
//! built for as `ses`; the SAME account's v2 surface is a service of its own, bound by the
//! `restJson1` path of [`super::aws_api`] under the token `sesv2`. A handler reaches whichever
//! of the two it names, as it does on a managed runtime.
//!
//! What a guest can and cannot do here:
//!
//! * The destination is ALWAYS this emulator: the client is given the callback's own transport
//!   and Amazon SES's own regional endpoint, so no host name is resolved and no socket is
//!   opened.
//! * The call is signed as the identity the invocation was given — the session of the
//!   function's execution role ([`super::execution_role_session`]) — so Amazon SES judges it
//!   against the policies in force on that role exactly as it judges any other signed request,
//!   and a role without `ses:SendEmail` is answered that service's own authorization refusal
//!   rather than a success.
//! * Only the operations in [`BOUND_OPERATIONS`] are bound, and only the input members each
//!   entry names. An operation or a member this seam does not bind is reported as a call that
//!   was NOT MADE, naming it, rather than sent with a value dropped — a dropped member would
//!   make a call that did something other than what the handler asked for.
//! * The answer's own status and document are what Amazon SES answered: a success is that
//!   operation's output members, and a refusal is its own error code, message and status, which
//!   the SDK the guest calls raises as `ClientError` ([`super::aws_sdk_guest`]).
//!
//! How one call's input is read, what the answer carried and how a refusal is handed back are
//! the same for every service reached through an SDK client, so they are [`super::sdk_call`]'s
//! rather than this module's.

use ::aws_sdk_ses::Client;
use ::serde_json::{Map, Value};
use ::smithy_server_runtime::EmbeddedEmulator;

use super::aws_api::{Answer, CallerIdentity, Refusal};
use super::sdk_call::{
    insert_double, insert_flag, insert_number, insert_text, insert_timestamp, Input, Observed,
    PROVIDER_NAME,
};

/// The service a refusal of one of these calls names, as the AWS SDKs name it.
const SERVICE: &str = "Amazon SES";

/// The leading label of the host this service answers on (`awsService.endpointPrefix`, which
/// for this service is not its signing name).
const ENDPOINT_PREFIX: &str = "email";

/// The operations of Amazon SES this seam binds, in the order a refusal lists them.
///
/// One entry per operation [`call`] builds a request for: the SENDING surface a function that
/// mails uses (a composed message, a raw MIME one and a templated one), the IDENTITIES a send
/// is only served from (verifying one, reading what verification it reached, listing and
/// deleting), the account's own sending state and figures, and the templates and configuration
/// sets a send names. A handler naming any other operation of the service is told the call was
/// not made and why, because a request this module cannot bind is one it would otherwise be
/// guessing at.
///
/// `DescribeConfigurationSet` is deliberately not among them: its answer carries the set's
/// event destinations, tracking, delivery and reputation options, each a structure of its own
/// that this seam does not bind, and an answer handed over without an output member the
/// handler reads would be worse than a call reported as not made.
pub const BOUND_OPERATIONS: &[&str] = &[
    "SendEmail",
    "SendRawEmail",
    "SendTemplatedEmail",
    "VerifyEmailIdentity",
    "VerifyDomainIdentity",
    "ListIdentities",
    "GetIdentityVerificationAttributes",
    "DeleteIdentity",
    "GetAccountSendingEnabled",
    "GetSendQuota",
    "GetSendStatistics",
    "CreateTemplate",
    "UpdateTemplate",
    "GetTemplate",
    "ListTemplates",
    "DeleteTemplate",
    "CreateConfigurationSet",
    "ListConfigurationSets",
    "DeleteConfigurationSet",
];

/// Make ONE Amazon SES call: `operation`, with `body` as the operation's input document (the
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
        "SendEmail" => send_email(&client, &input, &observed).await,
        "SendRawEmail" => send_raw_email(&client, &input, &observed).await,
        "SendTemplatedEmail" => send_templated_email(&client, &input, &observed).await,
        "VerifyEmailIdentity" => verify_email_identity(&client, &input, &observed).await,
        "VerifyDomainIdentity" => verify_domain_identity(&client, &input, &observed).await,
        "ListIdentities" => list_identities(&client, &input, &observed).await,
        "GetIdentityVerificationAttributes" => {
            get_identity_verification_attributes(&client, &input, &observed).await
        }
        "DeleteIdentity" => delete_identity(&client, &input, &observed).await,
        "GetAccountSendingEnabled" => {
            get_account_sending_enabled(&client, &input, &observed).await
        }
        "GetSendQuota" => get_send_quota(&client, &input, &observed).await,
        "GetSendStatistics" => get_send_statistics(&client, &input, &observed).await,
        "CreateTemplate" => create_template(&client, &input, &observed).await,
        "UpdateTemplate" => update_template(&client, &input, &observed).await,
        "GetTemplate" => get_template(&client, &input, &observed).await,
        "ListTemplates" => list_templates(&client, &input, &observed).await,
        "DeleteTemplate" => delete_template(&client, &input, &observed).await,
        "CreateConfigurationSet" => create_configuration_set(&client, &input, &observed).await,
        "ListConfigurationSets" => list_configuration_sets(&client, &input, &observed).await,
        "DeleteConfigurationSet" => delete_configuration_set(&client, &input, &observed).await,
        other => Err(Refusal::NotMade(format!(
            "the AWS SDK this runtime supplies binds no {other} call on {SERVICE}; the \
             operations it binds are {}",
            BOUND_OPERATIONS.join(", ")
        ))),
    }
}

/// An Amazon SES client over the in-process callback into this same emulator, signing as the
/// identity this invocation was given.
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
    let config = ::aws_sdk_ses::Config::builder()
        .behavior_version(::aws_sdk_ses::config::BehaviorVersion::latest())
        .region(::aws_sdk_ses::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://{ENDPOINT_PREFIX}.{region}.amazonaws.com"))
        .http_client(callback.http_client())
        .interceptor(observed)
        .retry_config(::aws_sdk_ses::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ses::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ses::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_ses::config::StalledStreamProtectionConfig::disabled(),
        );
    let config = match identity {
        Some(identity) => {
            config.credentials_provider(::aws_sdk_ses::config::Credentials::new(
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

/// `SendEmail`: one message composed from the parts the call carries, queued for sending.
async fn send_email(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "Source",
        "Destination",
        "Message",
        "ReplyToAddresses",
        "ReturnPath",
        "SourceArn",
        "ReturnPathArn",
        "Tags",
        "ConfigurationSetName",
    ])?;
    let sent = client
        .send_email()
        .source(input.required_text("Source")?)
        .destination(destination(input, "Destination")?.ok_or_else(|| required("Destination"))?)
        .message(message(input, "Message")?.ok_or_else(|| required("Message"))?)
        .set_reply_to_addresses(optional_list(input.text_list("ReplyToAddresses")?))
        .set_return_path(input.text("ReturnPath")?)
        .set_source_arn(input.text("SourceArn")?)
        .set_return_path_arn(input.text("ReturnPathArn")?)
        .set_tags(message_tags(input)?)
        .set_configuration_set_name(input.text("ConfigurationSetName")?)
        .send()
        .await;
    match sent {
        Ok(sent) => observed.answer(message_id(sent.message_id())),
        Err(error) => observed.refused(&error),
    }
}

/// `SendRawEmail`: the same, for a message the handler composed itself.
async fn send_raw_email(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "Source",
        "Destinations",
        "RawMessage",
        "FromArn",
        "SourceArn",
        "ReturnPathArn",
        "Tags",
        "ConfigurationSetName",
    ])?;
    let sent = client
        .send_raw_email()
        .set_source(input.text("Source")?)
        .set_destinations(optional_list(input.text_list("Destinations")?))
        .raw_message(raw_message(input)?)
        .set_from_arn(input.text("FromArn")?)
        .set_source_arn(input.text("SourceArn")?)
        .set_return_path_arn(input.text("ReturnPathArn")?)
        .set_tags(message_tags(input)?)
        .set_configuration_set_name(input.text("ConfigurationSetName")?)
        .send()
        .await;
    match sent {
        Ok(sent) => observed.answer(message_id(sent.message_id())),
        Err(error) => observed.refused(&error),
    }
}

/// `SendTemplatedEmail`: the same, for a message rendered from a stored template.
async fn send_templated_email(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[
        "Source",
        "Destination",
        "ReplyToAddresses",
        "ReturnPath",
        "SourceArn",
        "ReturnPathArn",
        "Tags",
        "ConfigurationSetName",
        "Template",
        "TemplateArn",
        "TemplateData",
    ])?;
    let sent = client
        .send_templated_email()
        .source(input.required_text("Source")?)
        .destination(destination(input, "Destination")?.ok_or_else(|| required("Destination"))?)
        .set_reply_to_addresses(optional_list(input.text_list("ReplyToAddresses")?))
        .set_return_path(input.text("ReturnPath")?)
        .set_source_arn(input.text("SourceArn")?)
        .set_return_path_arn(input.text("ReturnPathArn")?)
        .set_tags(message_tags(input)?)
        .set_configuration_set_name(input.text("ConfigurationSetName")?)
        .template(input.required_text("Template")?)
        .set_template_arn(input.text("TemplateArn")?)
        .template_data(input.required_text("TemplateData")?)
        .send()
        .await;
    match sent {
        Ok(sent) => observed.answer(message_id(sent.message_id())),
        Err(error) => observed.refused(&error),
    }
}

/// `VerifyEmailIdentity`: an address added to this account's identities, which a send from it
/// is only served once its verification is through.
async fn verify_email_identity(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["EmailAddress"])?;
    let sent = client
        .verify_email_identity()
        .email_address(input.required_text("EmailAddress")?)
        .send()
        .await;
    match sent {
        Ok(_verified) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `VerifyDomainIdentity`: a domain added the same way, with the token its DNS record carries.
async fn verify_domain_identity(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["Domain"])?;
    let sent = client
        .verify_domain_identity()
        .domain(input.required_text("Domain")?)
        .send()
        .await;
    let verified = match sent {
        Ok(verified) => verified,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_text(
        &mut document,
        "VerificationToken",
        Some(verified.verification_token()),
    );
    observed.answer(document)
}

/// `ListIdentities`: the identities this account holds, one page at a time.
async fn list_identities(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["IdentityType", "NextToken", "MaxItems"])?;
    let sent = client
        .list_identities()
        .set_identity_type(
            input
                .text("IdentityType")?
                .map(|named| ::aws_sdk_ses::types::IdentityType::from(named.as_str())),
        )
        .set_next_token(input.text("NextToken")?)
        .set_max_items(input.integer("MaxItems")?)
        .send()
        .await;
    let listed = match sent {
        Ok(listed) => listed,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    document.insert(
        "Identities".to_string(),
        Value::Array(
            listed
                .identities()
                .iter()
                .map(|identity| Value::String(identity.clone()))
                .collect(),
        ),
    );
    insert_text(&mut document, "NextToken", listed.next_token());
    observed.answer(document)
}

/// `GetIdentityVerificationAttributes`: what verification each named identity reached, which is
/// what says whether a send from it will be served.
async fn get_identity_verification_attributes(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["Identities"])?;
    let sent = client
        .get_identity_verification_attributes()
        .set_identities(Some(input.required_text_list("Identities")?))
        .send()
        .await;
    let read = match sent {
        Ok(read) => read,
        Err(error) => return observed.refused(&error),
    };
    let mut attributes = Map::new();
    for (identity, held) in read.verification_attributes() {
        let mut entry = Map::new();
        insert_text(
            &mut entry,
            "VerificationStatus",
            Some(held.verification_status().as_str()),
        );
        insert_text(&mut entry, "VerificationToken", held.verification_token());
        attributes.insert(identity.clone(), Value::Object(entry));
    }
    let mut document = Map::new();
    document.insert(
        "VerificationAttributes".to_string(),
        Value::Object(attributes),
    );
    observed.answer(document)
}

/// `DeleteIdentity`: an identity taken off the account again.
async fn delete_identity(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["Identity"])?;
    let sent = client
        .delete_identity()
        .identity(input.required_text("Identity")?)
        .send()
        .await;
    match sent {
        Ok(_deleted) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `GetAccountSendingEnabled`: whether sending is on for the whole account, which is what a
/// handler checks before it reports a send as possible at all.
async fn get_account_sending_enabled(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[])?;
    let read = match client.get_account_sending_enabled().send().await {
        Ok(read) => read,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_flag(&mut document, "Enabled", Some(read.enabled()));
    observed.answer(document)
}

/// `GetSendQuota`: the account's 24-hour ceiling, its rate and what it has already sent.
async fn get_send_quota(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[])?;
    let read = match client.get_send_quota().send().await {
        Ok(read) => read,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    insert_double(&mut document, "Max24HourSend", Some(read.max24_hour_send()));
    insert_double(&mut document, "MaxSendRate", Some(read.max_send_rate()));
    insert_double(
        &mut document,
        "SentLast24Hours",
        Some(read.sent_last24_hours()),
    );
    observed.answer(document)
}

/// `GetSendStatistics`: the per-window figures the account's sending produced.
async fn get_send_statistics(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&[])?;
    let read = match client.get_send_statistics().send().await {
        Ok(read) => read,
        Err(error) => return observed.refused(&error),
    };
    let points: Vec<Value> = read
        .send_data_points()
        .iter()
        .map(|point| {
            let mut held = Map::new();
            insert_timestamp(&mut held, "Timestamp", point.timestamp());
            insert_number(&mut held, "DeliveryAttempts", Some(point.delivery_attempts()));
            insert_number(&mut held, "Bounces", Some(point.bounces()));
            insert_number(&mut held, "Complaints", Some(point.complaints()));
            insert_number(&mut held, "Rejects", Some(point.rejects()));
            Value::Object(held)
        })
        .collect();
    let mut document = Map::new();
    document.insert("SendDataPoints".to_string(), Value::Array(points));
    observed.answer(document)
}

/// `CreateTemplate`: the template a templated send renders from.
async fn create_template(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["Template"])?;
    let sent = client
        .create_template()
        .template(template(input)?)
        .send()
        .await;
    match sent {
        Ok(_created) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `UpdateTemplate`: that template's own content replaced.
async fn update_template(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["Template"])?;
    let sent = client
        .update_template()
        .template(template(input)?)
        .send()
        .await;
    match sent {
        Ok(_updated) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `GetTemplate`: the template the account holds under that name.
async fn get_template(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["TemplateName"])?;
    let sent = client
        .get_template()
        .template_name(input.required_text("TemplateName")?)
        .send()
        .await;
    let read = match sent {
        Ok(read) => read,
        Err(error) => return observed.refused(&error),
    };
    let mut document = Map::new();
    if let Some(held) = read.template() {
        let mut entry = Map::new();
        insert_text(&mut entry, "TemplateName", Some(held.template_name()));
        insert_text(&mut entry, "SubjectPart", held.subject_part());
        insert_text(&mut entry, "TextPart", held.text_part());
        insert_text(&mut entry, "HtmlPart", held.html_part());
        document.insert("Template".to_string(), Value::Object(entry));
    }
    observed.answer(document)
}

/// `ListTemplates`: the templates the account holds, one page at a time.
async fn list_templates(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["NextToken", "MaxItems"])?;
    let sent = client
        .list_templates()
        .set_next_token(input.text("NextToken")?)
        .set_max_items(input.integer("MaxItems")?)
        .send()
        .await;
    let listed = match sent {
        Ok(listed) => listed,
        Err(error) => return observed.refused(&error),
    };
    let held: Vec<Value> = listed
        .templates_metadata()
        .iter()
        .map(|metadata| {
            let mut entry = Map::new();
            insert_text(&mut entry, "Name", metadata.name());
            insert_timestamp(&mut entry, "CreatedTimestamp", metadata.created_timestamp());
            Value::Object(entry)
        })
        .collect();
    let mut document = Map::new();
    document.insert("TemplatesMetadata".to_string(), Value::Array(held));
    insert_text(&mut document, "NextToken", listed.next_token());
    observed.answer(document)
}

/// `DeleteTemplate`: that template taken off the account again.
async fn delete_template(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["TemplateName"])?;
    let sent = client
        .delete_template()
        .template_name(input.required_text("TemplateName")?)
        .send()
        .await;
    match sent {
        Ok(_deleted) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `CreateConfigurationSet`: the set a send names to have its sending events published.
async fn create_configuration_set(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["ConfigurationSet"])?;
    let Some(held) = input.structure("ConfigurationSet")? else {
        return Err(required("ConfigurationSet"));
    };
    held.only(&["Name"])?;
    let set = ::aws_sdk_ses::types::ConfigurationSet::builder()
        .name(held.required_text("Name")?)
        .build()
        .map_err(|error| not_built("the configuration set", &error.to_string()))?;
    let sent = client
        .create_configuration_set()
        .configuration_set(set)
        .send()
        .await;
    match sent {
        Ok(_created) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// `ListConfigurationSets`: the sets the account holds, one page at a time.
async fn list_configuration_sets(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["NextToken", "MaxItems"])?;
    let sent = client
        .list_configuration_sets()
        .set_next_token(input.text("NextToken")?)
        .set_max_items(input.integer("MaxItems")?)
        .send()
        .await;
    let listed = match sent {
        Ok(listed) => listed,
        Err(error) => return observed.refused(&error),
    };
    let held: Vec<Value> = listed
        .configuration_sets()
        .iter()
        .map(|set| {
            let mut entry = Map::new();
            insert_text(&mut entry, "Name", Some(set.name()));
            Value::Object(entry)
        })
        .collect();
    let mut document = Map::new();
    document.insert("ConfigurationSets".to_string(), Value::Array(held));
    insert_text(&mut document, "NextToken", listed.next_token());
    observed.answer(document)
}

/// `DeleteConfigurationSet`: that set taken off the account again.
async fn delete_configuration_set(
    client: &Client,
    input: &Input,
    observed: &Observed,
) -> Result<Answer, Refusal> {
    input.only(&["ConfigurationSetName"])?;
    let sent = client
        .delete_configuration_set()
        .configuration_set_name(input.required_text("ConfigurationSetName")?)
        .send()
        .await;
    match sent {
        Ok(_deleted) => observed.answer(Map::new()),
        Err(error) => observed.refused(&error),
    }
}

/// The `MessageId` an accepted send answered, as the answer's document carries it.
fn message_id(minted: &str) -> Map<String, Value> {
    let mut document = Map::new();
    insert_text(&mut document, "MessageId", Some(minted));
    document
}

/// The refusal for a member the operation requires and the call carries none of: the call is
/// not made, rather than sent without what it needs.
fn required(name: &str) -> Refusal {
    Refusal::NotMade(format!(
        "the call carries no {name}, which this {SERVICE} operation requires"
    ))
}

/// The refusal for a nested structure the client would not build from what the call carried.
fn not_built(what: &str, why: &str) -> Refusal {
    Refusal::NotMade(format!(
        "{what} of the call is not one {SERVICE} accepts: {why}"
    ))
}

/// A list member as the client takes it: `None` where the call carried the member not at all,
/// so a member it left out is not sent as an emptied one.
fn optional_list(values: Vec<String>) -> Option<Vec<String>> {
    (!values.is_empty()).then_some(values)
}

/// The `Destination` a send addresses: the To:, CC: and BCC: lines it carries.
fn destination(
    input: &Input,
    name: &str,
) -> Result<Option<::aws_sdk_ses::types::Destination>, Refusal> {
    let Some(held) = input.structure(name)? else {
        return Ok(None);
    };
    held.only(&["ToAddresses", "CcAddresses", "BccAddresses"])?;
    Ok(Some(
        ::aws_sdk_ses::types::Destination::builder()
            .set_to_addresses(optional_list(held.text_list("ToAddresses")?))
            .set_cc_addresses(optional_list(held.text_list("CcAddresses")?))
            .set_bcc_addresses(optional_list(held.text_list("BccAddresses")?))
            .build(),
    ))
}

/// The `Message` a composed send carries: a subject and a body, each its own content part.
fn message(input: &Input, name: &str) -> Result<Option<::aws_sdk_ses::types::Message>, Refusal> {
    let Some(held) = input.structure(name)? else {
        return Ok(None);
    };
    held.only(&["Subject", "Body"])?;
    let subject = content(&held, "Subject")?.ok_or_else(|| required("Message.Subject"))?;
    let Some(parts) = held.structure("Body")? else {
        return Err(required("Message.Body"));
    };
    parts.only(&["Text", "Html"])?;
    let body = ::aws_sdk_ses::types::Body::builder()
        .set_text(content(&parts, "Text")?)
        .set_html(content(&parts, "Html")?)
        .build();
    Ok(Some(
        ::aws_sdk_ses::types::Message::builder()
            .subject(subject)
            .body(body)
            .build(),
    ))
}

/// One `Content` part — the subject, or one representation of the body — with the character
/// set the call stated for it.
fn content(held: &Input, name: &str) -> Result<Option<::aws_sdk_ses::types::Content>, Refusal> {
    let Some(part) = held.structure(name)? else {
        return Ok(None);
    };
    part.only(&["Data", "Charset"])?;
    Ok(Some(
        ::aws_sdk_ses::types::Content::builder()
            .data(part.required_text("Data")?)
            .set_charset(part.text("Charset")?)
            .build()
            .map_err(|error| not_built("a content part", &error.to_string()))?,
    ))
}

/// The `RawMessage` a raw send carries: the message's own bytes, which travel to this seam as
/// the base64 text a JSON protocol carries a blob as.
fn raw_message(input: &Input) -> Result<::aws_sdk_ses::types::RawMessage, Refusal> {
    let Some(held) = input.structure("RawMessage")? else {
        return Err(required("RawMessage"));
    };
    held.only(&["Data"])?;
    let data = held.blob("Data")?.ok_or_else(|| required("RawMessage.Data"))?;
    ::aws_sdk_ses::types::RawMessage::builder()
        .data(::aws_sdk_ses::primitives::Blob::new(data))
        .build()
        .map_err(|error| not_built("the raw message", &error.to_string()))
}

/// The `MessageTag` list a send carries, or `None` where it carries the member not at all.
fn message_tags(input: &Input) -> Result<Option<Vec<::aws_sdk_ses::types::MessageTag>>, Refusal> {
    let entries = input.structures("Tags")?;
    if entries.is_empty() {
        return Ok(None);
    }
    let mut built = Vec::with_capacity(entries.len());
    for entry in entries {
        entry.only(&["Name", "Value"])?;
        built.push(
            ::aws_sdk_ses::types::MessageTag::builder()
                .name(entry.required_text("Name")?)
                .value(entry.required_text("Value")?)
                .build()
                .map_err(|error| not_built("a message tag", &error.to_string()))?,
        );
    }
    Ok(Some(built))
}

/// The `Template` a template create or update carries.
fn template(input: &Input) -> Result<::aws_sdk_ses::types::Template, Refusal> {
    let Some(held) = input.structure("Template")? else {
        return Err(required("Template"));
    };
    held.only(&["TemplateName", "SubjectPart", "TextPart", "HtmlPart"])?;
    ::aws_sdk_ses::types::Template::builder()
        .template_name(held.required_text("TemplateName")?)
        .set_subject_part(held.text("SubjectPart")?)
        .set_text_part(held.text("TextPart")?)
        .set_html_part(held.text("HtmlPart")?)
        .build()
        .map_err(|error| not_built("the template", &error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::BOUND_OPERATIONS;

    /// Every operation this seam binds is named once, so the catalog a refusal lists is a list
    /// of operations rather than of spellings.
    #[test]
    fn every_bound_operation_is_named_once() {
        let mut names = BOUND_OPERATIONS.to_vec();
        names.sort_unstable();
        let total = names.len();
        names.dedup();
        assert_eq!(
            names.len(),
            total,
            "case \"the bound operations\": expected each operation to be named once, got \
             {BOUND_OPERATIONS:?}"
        );
    }
}
