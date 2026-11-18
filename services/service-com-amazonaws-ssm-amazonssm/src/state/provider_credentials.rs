// SPDX-License-Identifier: CC0-1.0

//! The provider credential a cloud connector acts with, and whether this account holds
//! what creating one for the tenant a configuration names takes.
//!
//! A cloud connector acts in the third-party cloud as ONE application registration of ONE
//! tenant (`AzureConfiguration$TenantId`, `AzureConfiguration$ApplicationId`), and the
//! credential it acts under is created by presenting the Amazon Web Services web identity
//! token to that application's federated identity credential. Two things an emulated
//! service CAN establish stand between a request and that credential, and the model names a
//! finding for each of them
//! (`crate::ops::types::VALIDATION_FINDING_CODE_VALUES`), after the two steps
//! [`super::cloud_connector_validation`] already resolves about the role:
//!
//! 1. the ACCOUNT has to have outbound web identity federation on, because that is what
//!    lets its principals obtain the token at all (IAM's
//!    `GetOutboundWebIdentityFederationInfo`: "When true, IAM principals in the account can
//!    call the `GetWebIdentityToken` API to obtain JSON Web Tokens (JWTs) for
//!    authentication with external services"). An account that has not enabled it is
//!    [`OUTBOUND_FEDERATION_DISABLED`], reported as `OutboundWebIdentityFederationDisabled`.
//! 2. the token is exchanged for the PROVIDER's own credential, which takes a credential
//!    this account already verified for that tenant and that application
//!    ([`AZURE_CREDENTIALS_UNOBTAINABLE`], reported as
//!    `ProviderCredentialCreationFailed`).
//!
//! This emulator makes no request outside itself, so it asks the provider nothing. What it
//! CAN establish for the second question is whether this ACCOUNT already holds a verified
//! credential for the pair, and the connector names where that credential lives: its
//! `ConfigConnectorArn` is "The ARN of the Amazon Web Services Config connector associated
//! with this cloud connector", and an Amazon Web Services Config connector IS the
//! credential Config connects to a third-party cloud service provider with, carrying the
//! tenant and the client the connector was created for
//! (`AzureConnectorConfiguration$tenantIdentifier`, `$clientIdentifier`).
//!
//! So each question is put to the service HOLDING the record — the feature to IAM, which
//! owns it, and the verified credential to Config — over the in-process hop every caller of
//! another emulated service makes ([`::service_common::in_process_sigv4`]), and each is
//! answered from what that service returned out of its own store.
//!
//! An answer from the emulator's own front door rather than from the service addressed —
//! nothing mounted to ask, or a call this process could not make — is no answer ABOUT the
//! account or the connector, so nothing is refused on it: the same grounding every question
//! this slice puts to another service takes ([`super::activation_role`],
//! [`super::cloud_connector_validation`]).

/// The signing name Amazon Web Services Config is addressed by, which is also its endpoint
/// prefix (`awsService$endpointPrefix`).
const CONFIG_SIGNING_NAME: &str = "config";

/// The `X-Amz-Target` prefix of that service's `awsJson1_1` wire: the service shape the
/// model binds its operations to (`com.amazonaws.configservice#StarlingDoveService`).
const CONFIG_TARGET_PREFIX: &str = "StarlingDoveService";

/// The operation that reads ONE connector of that service: "Returns details of a
/// connector", by the ARN the connector is named by.
const CONFIG_GET_CONNECTOR: &str = "GetConnector";

/// The media type an `awsJson1_1` request carries.
const AWS_JSON_1_1: &str = "application/x-amz-json-1.1";

/// The signing name IAM is addressed by, which owns the account's record of the feature.
const IAM_SIGNING_NAME: &str = "iam";

/// The operation reporting that record: "Retrieves the configuration information for the
/// outbound identity federation feature in your Amazon Web Services account."
const IAM_GET_OUTBOUND_FEDERATION_INFO: &str = "GetOutboundWebIdentityFederationInfo";

/// The API version that service's `awsQuery` wire names
/// (`com.amazonaws.iam#AWSIdentityManagementV20100508`, version `2010-05-08`).
const IAM_API_VERSION: &str = "2010-05-08";

/// The media type an `awsQuery` request carries.
const FORM_URLENCODED: &str = "application/x-www-form-urlencoded";

/// The member of that answer reporting whether the feature is on
/// (`GetOutboundWebIdentityFederationInfoResponse$JwtVendingEnabled`).
const JWT_VENDING_ENABLED: &str = "JwtVendingEnabled";

/// The element an `awsQuery` error document names the failing shape in, inside its `Error`.
const QUERY_ERROR_CODE: &str = "Code";

/// The element that document carries each refusal in.
const QUERY_ERROR: &str = "Error";

/// The element an `awsQuery` answer carries that operation's output members in
/// (`<GetOutboundWebIdentityFederationInfoResult>`).
const IAM_OUTBOUND_FEDERATION_RESULT: &str = "GetOutboundWebIdentityFederationInfoResult";

/// The `awsQuery` error code IAM words for an account that has the feature OFF
/// (`FeatureDisabledException`, awsQuery code `FeatureDisabled`): "The request failed because
/// outbound identity federation is already disabled for your Amazon Web Services account."
/// The shape's own name carries the same prefix, so one reading covers both spellings.
const FEATURE_DISABLED_CODE: &str = "FeatureDisabled";

/// How much of that service's answer is read.
///
/// The EMULATOR's own bound: the answer is one connector — a name, an ARN, a provider
/// configuration and an instant — so what a read of it costs is fixed here rather than by
/// whatever the answering service sends, and a longer answer is one this reader does not
/// read rather than an allocation the answer chose the size of.
const MAX_ANSWER_BYTES: usize = 64 * 1024;

/// The output member that answer carries the connector in (`GetConnectorResponse$Connector`).
const ANSWERED_CONNECTOR: &str = "Connector";

/// The member an `awsJson` refusal names its own shape in.
const JSON_ERROR_TYPE: &str = "__type";

/// The connector member carrying the provider configuration it was created with
/// (`Connector$connectorConfiguration`).
const CONNECTOR_CONFIGURATION: &str = "connectorConfiguration";

/// The provider member of that configuration (`ConnectorConfiguration$azure`), which is the
/// one provider the configuration declares.
const AZURE_CONFIGURATION: &str = "azure";

/// The tenant that configuration was verified for
/// (`AzureConnectorConfiguration$tenantIdentifier`).
const AZURE_TENANT_IDENTIFIER: &str = "tenantIdentifier";

/// The application it was verified for (`AzureConnectorConfiguration$clientIdentifier`),
/// which is the application registration a cloud connector's `ApplicationId` names.
const AZURE_CLIENT_IDENTIFIER: &str = "clientIdentifier";

/// The errors `GetConnector` declares, each of which answers that the `Arn` it was handed
/// resolves to no connector of this account
/// (`smithy_query(query="operation", operation="GetConnector")`: `ResourceNotFoundException |
/// ValidationException`) — that service refuses a value that is no connector ARN and a
/// well-formed one it holds no connector under the same way, so both of them say the same
/// thing about what the account holds.
///
/// Every other refusal — the call was not authorized, it was throttled, nothing served that
/// operation — is an answer about the CALL rather than about the connector, and settles
/// nothing.
const GET_CONNECTOR_UNRESOLVED_ERRORS: &[&str] =
    &["ResourceNotFoundException", "ValidationException"];

/// How a request whose tenant and application no verified credential of this account stands
/// behind is refused.
///
/// The sentence Systems Manager words for the condition the modelled
/// `ProviderCredentialCreationFailed` finding names: the provider credential could not be
/// created, and the three things that bear on creating one are the tenant, the application
/// and the federated identity credential configured for them. The whole sentence is FIXED
/// text for this condition: it interpolates nothing from the request — neither the tenant,
/// the application nor the connector appears in it.
pub const AZURE_CREDENTIALS_UNOBTAINABLE: &str =
    "Unable to create Azure credentials. Please verify the Azure tenant ID, application ID, \
     and federated identity credential configuration are correct.";

/// What this account holds about the tenant and the application a configuration names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderCredential {
    /// Config answered with a connector it holds for exactly that tenant and that
    /// application, so the credential the federation is exchanged for can be created.
    Verified,
    /// Config answered, and what it holds verifies no credential for them: it holds no
    /// connector under that ARN at all, or one created for another tenant or another
    /// application.
    Unverified,
    /// Nothing answered about the connector, so this says nothing about the credential.
    Unanswered,
}

/// How a request made for an account that has not enabled outbound web identity federation
/// is refused.
///
/// The whole sentence is FIXED text for this condition: it names the account-level feature
/// and what enabling it allows, and interpolates nothing from the request.
pub const OUTBOUND_FEDERATION_DISABLED: &str =
    "Outbound web identity federation is disabled for this AWS account. Enable it to use \
     cross-cloud authentication with Azure.";

/// Whether the account has outbound web identity federation on, as IAM reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutboundFederation {
    /// IAM answered with the account's configuration, reporting the feature on.
    Enabled,
    /// IAM answered that the account has it off.
    Disabled,
    /// Nothing answered about the account, so this says nothing about the feature.
    Unanswered,
}

/// Whether the ACCOUNT has not enabled outbound web identity federation, so none of its
/// principals can obtain the web identity token the cross-cloud federation presents — the
/// question resolved before the provider credential itself.
///
/// `false` — refuse nothing — for everything that is not IAM ANSWERING that the account has
/// the feature off, including an emulator with no IAM mounted to ask.
pub async fn outbound_federation_is_disabled(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> bool {
    let answered = match ask_iam_for_outbound_federation(ctx, region).await {
        None => OutboundFederation::Unanswered,
        Some((status, payload)) => outbound_federation(status, &payload),
    };
    answered == OutboundFederation::Disabled
}

/// What IAM's answer about the account says about the feature.
fn outbound_federation(status: u16, payload: &[u8]) -> OutboundFederation {
    // The emulator's own front door refusing a call nothing served is not IAM's answer: the
    // call reached no account record to be answered from.
    if ::service_common::in_process_refusal::is_transport_refusal(payload) {
        return OutboundFederation::Unanswered;
    }
    // A server fault is that service failing to answer rather than an answer about the
    // account.
    if status >= 500 {
        return OutboundFederation::Unanswered;
    }
    let Ok(text) = ::std::str::from_utf8(payload) else {
        return OutboundFederation::Unanswered;
    };
    let Ok(document) = ::roxmltree::Document::parse(text) else {
        return OutboundFederation::Unanswered;
    };
    if (200..300).contains(&status) {
        // The member reports "whether outbound identity federation is currently enabled",
        // so the answer is read off it rather than inferred from the status — and off the
        // result element the operation's own output members live in, rather than from
        // wherever in the document that name appears.
        return match member_text(&document, IAM_OUTBOUND_FEDERATION_RESULT, JWT_VENDING_ENABLED)
            .as_str()
        {
            "true" => OutboundFederation::Enabled,
            "false" => OutboundFederation::Disabled,
            _ => OutboundFederation::Unanswered,
        };
    }
    // Of IAM's own refusals, only the one it words for an account holding the feature off
    // says anything about the feature: every other refusal — the call was not authorized,
    // say — leaves the question unanswered.
    if member_text(&document, QUERY_ERROR, QUERY_ERROR_CODE)
        .rsplit(['#', '.'])
        .next()
        .unwrap_or_default()
        .starts_with(FEATURE_DISABLED_CODE)
    {
        OutboundFederation::Disabled
    } else {
        OutboundFederation::Unanswered
    }
}

/// The text of the `<member>` element held directly inside the first `<parent>` of an XML
/// document, empty when the document carries no such pair.
///
/// The member is resolved through the element it is declared in rather than by the first
/// match anywhere in the document, so a value carried somewhere else in the answer is not
/// read as that member. The document is another service's answer, so it is parsed by the
/// maintained XML reader, which reports a malformed one instead of guessing at it.
fn member_text(document: &::roxmltree::Document<'_>, parent: &str, member: &str) -> String {
    document
        .descendants()
        .find(|node| node.has_tag_name(parent))
        .and_then(|node| {
            node.children()
                .find(|child| child.has_tag_name(member))
        })
        .and_then(|node| node.text())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Whether the Azure credential a connector acting as `application_id` in `tenant_id` would
/// act with cannot be created — the question a create resolves before it mints a connector,
/// and the one a validation reports as a finding of the connector it is asked about.
///
/// `false` — refuse nothing — for everything that is not Config ANSWERING that it verifies
/// no such credential, including an emulator with no Config mounted to ask.
pub async fn azure_credentials_are_unobtainable(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    config_connector_arn: &str,
    tenant_id: &str,
    application_id: &str,
) -> bool {
    let answered = match ask_config_for_connector(ctx, region, config_connector_arn).await {
        None => ProviderCredential::Unanswered,
        Some((status, payload)) => {
            verified_credential(status, &payload, tenant_id, application_id)
        }
    };
    answered == ProviderCredential::Unverified
}

/// What Config's answer about one connector says about the credential for `tenant_id` and
/// `application_id`.
fn verified_credential(
    status: u16,
    payload: &[u8],
    tenant_id: &str,
    application_id: &str,
) -> ProviderCredential {
    // The emulator's own front door refusing a call nothing served is not Config's answer:
    // the call reached no store to be answered from.
    if ::service_common::in_process_refusal::is_transport_refusal(payload) {
        return ProviderCredential::Unanswered;
    }
    // A server fault is that service failing to answer rather than an answer about the
    // connector, so it settles nothing either.
    if status >= 500 {
        return ProviderCredential::Unanswered;
    }
    // Config's own refusal of the READ is an answer about what this account holds only
    // where it is one of the two that operation declares, each of which says the ARN
    // resolves to no connector of it; every other refusal is an answer about the CALL, which
    // settles nothing about the credential.
    if !(200..300).contains(&status) {
        return if GET_CONNECTOR_UNRESOLVED_ERRORS.contains(&refusal_code(payload).as_str()) {
            ProviderCredential::Unverified
        } else {
            ProviderCredential::Unanswered
        };
    }
    let Ok(document) = ::serde_json::from_slice::<::serde_json::Value>(payload) else {
        return ProviderCredential::Unanswered;
    };
    let azure = &document[ANSWERED_CONNECTOR][CONNECTOR_CONFIGURATION][AZURE_CONFIGURATION];
    let verified_tenant = azure[AZURE_TENANT_IDENTIFIER]
        .as_str()
        .unwrap_or_default();
    let verified_application = azure[AZURE_CLIENT_IDENTIFIER]
        .as_str()
        .unwrap_or_default();
    // A connector of another provider carries no Azure identifiers at all, so the
    // comparison is made only against identifiers the answer actually stated: two empty
    // values are not a match.
    let verified = !verified_tenant.is_empty()
        && !verified_application.is_empty()
        && verified_tenant == tenant_id
        && verified_application == application_id;
    if verified {
        ProviderCredential::Verified
    } else {
        ProviderCredential::Unverified
    }
}

/// The shape an `awsJson` refusal names itself by: the NAME of the error its `__type`
/// carries, which may be written as a shape id and followed by a trailer of the protocol's
/// own. Empty for an answer that names none.
fn refusal_code(payload: &[u8]) -> String {
    let Ok(document) = ::serde_json::from_slice::<::serde_json::Value>(payload) else {
        return String::new();
    };
    document[JSON_ERROR_TYPE]
        .as_str()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Ask Config for the connector `arn` names, in process, answering the status and the body
/// it answered with.
async fn ask_config_for_connector(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    arn: &str,
) -> Option<(u16, Vec<u8>)> {
    let host = format!("{CONFIG_SIGNING_NAME}.{region}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", AWS_JSON_1_1)
        .header(
            ::service_common::rpc_target::TARGET_HEADER,
            format!("{CONFIG_TARGET_PREFIX}.{CONFIG_GET_CONNECTOR}"),
        )
        .body(::serde_json::json!({ "Arn": arn }).to_string().into_bytes())
        .ok()?;
    dispatch_in_process(ctx, CONFIG_SIGNING_NAME, region, request).await
}

/// Ask IAM for this account's outbound web identity federation, in process, answering the
/// status and the body it answered with.
///
/// That service speaks `awsQuery`, so the operation travels as the `Action` of a form body,
/// and this operation takes no other parameter.
async fn ask_iam_for_outbound_federation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<(u16, Vec<u8>)> {
    let host = format!("{IAM_SIGNING_NAME}.{region}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", FORM_URLENCODED)
        .body(
            format!("Action={IAM_GET_OUTBOUND_FEDERATION_INFO}&Version={IAM_API_VERSION}")
                .into_bytes(),
        )
        .ok()?;
    dispatch_in_process(ctx, IAM_SIGNING_NAME, region, request).await
}

/// Sign `request` for `signing_name` in `region` and dispatch it in process, answering the
/// status and the body the called service answered with — or `None` when the call could not
/// be MADE at all (it could not be signed, or this process carries nothing that could
/// dispatch it), which is a fact about this process rather than about what was asked.
///
/// The emulator verifies every request signature, so the call carries one: the credential the
/// framework hands this service, signed through the tree's one in-process signer
/// ([`::service_common::in_process_sigv4`]), which is never reported or logged. The connector
/// is the emulator's own, so nothing is dialled and no name is resolved.
async fn dispatch_in_process(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    signing_name: &str,
    region: &str,
    mut request: ::http::Request<Vec<u8>>,
) -> Option<(u16, Vec<u8>)> {
    ::service_common::in_process_sigv4::sign_in_process_request(
        &mut request,
        signing_name,
        region,
        ctx,
    )
    .await
    .ok()?;
    let (parts, body) = request.into_parts();
    let request = ::aws_smithy_runtime_api::http::Request::try_from(::http::Request::from_parts(
        parts,
        ::aws_smithy_types::body::SdkBody::from(::bytes::Bytes::from(body)),
    ))
    .ok()?;
    let connector = ctx.callback().http_connector();
    let answer = {
        use ::aws_smithy_runtime_api::client::http::HttpConnector;
        HttpConnector::call(&connector, request).await.ok()?
    };
    let status = answer.status().as_u16();
    let payload: Vec<u8> = answer
        .body()
        .bytes()
        .unwrap_or_default()
        .iter()
        .take(MAX_ANSWER_BYTES)
        .copied()
        .collect();
    Some((status, payload))
}

#[cfg(test)]
mod tests {
    use super::{outbound_federation, verified_credential, OutboundFederation, ProviderCredential};

    /// The tenant and application a request names, shaped as the modelled hex UUIDs.
    const TENANT: &str = "11111111-2222-3333-4444-555555555555";

    /// See [`TENANT`].
    const APPLICATION: &str = "66666666-7777-8888-9999-aaaaaaaaaaaa";

    /// One connector answer, as Config's `GetConnector` words it.
    fn answered(tenant: &str, client: &str) -> String {
        ::serde_json::json!({
            "Connector": {
                "name": format!("AZURE-{tenant}"),
                "arn": format!("arn:aws:config:us-west-2:000000000000:connector/AZURE-{tenant}"),
                "connectorConfiguration": {
                    "azure": {
                        "tenantIdentifier": tenant,
                        "clientIdentifier": client,
                    },
                },
                "createdTime": 1.0,
            }
        })
        .to_string()
    }

    /// RULE: the credential is verified only where Config ANSWERED with a connector it
    /// holds for the same tenant and the same application; its own refusal verifies none,
    /// and an answer that is not Config's settles nothing.
    #[test]
    fn a_credential_is_verified_only_by_the_connector_config_holds_for_that_pair() {
        let matching = answered(TENANT, APPLICATION);
        let other_tenant = answered("99999999-9999-9999-9999-999999999999", APPLICATION);
        let other_application = answered(TENANT, "01234567-89ab-cdef-0123-456789abcdef");
        // (case, the status, the body, what it says about the credential)
        type Row<'a> = (&'a str, u16, &'a str, ProviderCredential);
        let rows: &[Row<'_>] = &[
            (
                "a connector held for that tenant and application",
                200,
                matching.as_str(),
                ProviderCredential::Verified,
            ),
            (
                "a connector held for another tenant",
                200,
                other_tenant.as_str(),
                ProviderCredential::Unverified,
            ),
            (
                "a connector held for another application",
                200,
                other_application.as_str(),
                ProviderCredential::Unverified,
            ),
            (
                "a connector of a provider carrying no Azure identifiers",
                200,
                r#"{"Connector":{"name":"n","arn":"a","connectorConfiguration":{}}}"#,
                ProviderCredential::Unverified,
            ),
            (
                "Config refusing the read because it holds no such connector",
                400,
                r#"{"__type":"ValidationException","Message":"The connector ARN provided is not valid."}"#,
                ProviderCredential::Unverified,
            ),
            (
                "Config refusing it as a resource it does not hold",
                400,
                r#"{"__type":"ResourceNotFoundException","Message":"absent"}"#,
                ProviderCredential::Unverified,
            ),
            (
                "the same refusal written as the shape id the model declares",
                400,
                r#"{"__type":"com.amazonaws.configservice#ValidationException","Message":"x"}"#,
                ProviderCredential::Unverified,
            ),
            (
                "a refusal about the CALL rather than the connector",
                403,
                r#"{"__type":"AccessDeniedException","Message":"not authorized"}"#,
                ProviderCredential::Unanswered,
            ),
            (
                "the call being throttled",
                400,
                r#"{"__type":"ThrottlingException","Message":"rate exceeded"}"#,
                ProviderCredential::Unanswered,
            ),
            (
                "nothing serving that operation",
                404,
                r#"{"__type":"UnknownOperationException"}"#,
                ProviderCredential::Unanswered,
            ),
            (
                "the front door refusing a call no mounted service claimed",
                404,
                r#"{"__type":"UnknownService","message":"no emulated service is registered"}"#,
                ProviderCredential::Unanswered,
            ),
            (
                "the front door refusing a call two services claimed",
                404,
                r#"{"__type":"AmbiguousService","message":"two services claim it"}"#,
                ProviderCredential::Unanswered,
            ),
            (
                "that service failing to answer at all",
                500,
                r#"{"__type":"InternalFailure"}"#,
                ProviderCredential::Unanswered,
            ),
            (
                "an answer this reader cannot read",
                200,
                "not a document",
                ProviderCredential::Unanswered,
            ),
        ];

        for (case, status, body, expected) in rows {
            let actual = verified_credential(*status, body.as_bytes(), TENANT, APPLICATION);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// One `GetOutboundWebIdentityFederationInfo` answer, as IAM's `awsQuery` wire words it.
    fn federation_reported(enabled: bool) -> String {
        format!(
            "<GetOutboundWebIdentityFederationInfoResponse>\
             <GetOutboundWebIdentityFederationInfoResult>\
             <IssuerIdentifier>https://issuer.example</IssuerIdentifier>\
             <JwtVendingEnabled>{enabled}</JwtVendingEnabled>\
             </GetOutboundWebIdentityFederationInfoResult>\
             </GetOutboundWebIdentityFederationInfoResponse>"
        )
    }

    /// One refusal of that call, as that wire words one.
    fn federation_refused(code: &str) -> String {
        format!(
            "<ErrorResponse><Error><Type>Sender</Type><Code>{code}</Code>\
             <Message>whatever it words</Message></Error></ErrorResponse>"
        )
    }

    /// RULE: the account's feature is read off the member IAM reports it in, and of IAM's
    /// own refusals only the one it words for an account holding the feature off says
    /// anything about it — every other answer leaves the question unanswered.
    #[test]
    fn the_accounts_feature_is_read_only_from_what_iam_answered_about_it() {
        let on = federation_reported(true);
        let off = federation_reported(false);
        let disabled = federation_refused("FeatureDisabled");
        let denied = federation_refused("AccessDenied");
        let throttled = federation_refused("Throttling");
        // (case, the status, the body, what it says about the feature)
        type Row<'a> = (&'a str, u16, &'a str, OutboundFederation);
        let rows: &[Row<'_>] = &[
            (
                "an account reporting the feature on",
                200,
                on.as_str(),
                OutboundFederation::Enabled,
            ),
            (
                "an account reporting it off",
                200,
                off.as_str(),
                OutboundFederation::Disabled,
            ),
            (
                "IAM refusing the read because the account has it off",
                404,
                disabled.as_str(),
                OutboundFederation::Disabled,
            ),
            (
                "a refusal about the CALL rather than the account",
                403,
                denied.as_str(),
                OutboundFederation::Unanswered,
            ),
            (
                "the call being throttled",
                400,
                throttled.as_str(),
                OutboundFederation::Unanswered,
            ),
            (
                "the front door refusing a call no mounted service claimed",
                404,
                r#"{"__type":"UnknownService","message":"no emulated service is registered"}"#,
                OutboundFederation::Unanswered,
            ),
            (
                "that service failing to answer at all",
                500,
                "<ErrorResponse><Error><Code>InternalFailure</Code></Error></ErrorResponse>",
                OutboundFederation::Unanswered,
            ),
            (
                "an answer this reader cannot read",
                200,
                "not a document",
                OutboundFederation::Unanswered,
            ),
            (
                "an answer carrying the member somewhere else entirely",
                200,
                "<Other><JwtVendingEnabled>true</JwtVendingEnabled></Other>",
                OutboundFederation::Unanswered,
            ),
        ];

        for (case, status, body, expected) in rows {
            let actual = outbound_federation(*status, body.as_bytes());
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
