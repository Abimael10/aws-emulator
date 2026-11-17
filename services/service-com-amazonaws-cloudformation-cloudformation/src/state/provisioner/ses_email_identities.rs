// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of the Amazon SES email IDENTITY an
//! `AWS::SES::EmailIdentity` declares. One file of [`super`]'s per-type arms, dispatched to
//! by its `Provisioner`.
//!
//! An email identity is the domain or the address an account sends mail from, and it is
//! addressed by that very text: the type's `EmailIdentity` property IS the resource's
//! identity ("Ref returns the email identity"), which is why the type is registered in
//! `super::template::declared_name_property` rather than taking a generated
//! `<stack>-<logical id>-<unique id>` — a name no SES operation would resolve.
//!
//! The identity is created in the service that owns it, through the API v2 that owns the
//! type's properties (`CreateEmailIdentity`, `POST /v2/email/identities`), so a client that
//! deployed the stack reads the identity back with `GetEmailIdentity`, lists it, configures
//! its DKIM signing and sends from it once it is verified. The attributes the declaration
//! states beside the identity's name are each the call of that API which writes them: the
//! configuration set and the BYODKIM/Easy DKIM signing attributes travel on the create
//! itself, and the DKIM signing switch, the feedback-forwarding switch and the custom MAIL
//! FROM domain are the three `PutEmailIdentity*Attributes` calls made on the identity the
//! create answered.
//!
//! The type publishes no `Tags` property, so CloudFormation propagates no tags to an
//! identity.

use super::*;

/// Amazon SES's signing name, which the v2 API signs with as the classic surface does
/// (`smithy_query(query="service_info")` for either model: `sigv4Name: ses`). The v2 model
/// speaks `restJson1` and binds each operation to a method and a URI under `/v2/email`, so
/// its calls ride on [`call_rest_json`] rather than on the classic surface's
/// [`call_query`].
const SESV2_SIGNING_NAME: &str = "ses";

/// The identity collection `CreateEmailIdentity` is bound to (`POST /v2/email/identities`);
/// one identity is addressed under it by its own text (`GET`/`DELETE
/// /v2/email/identities/{EmailIdentity}`), and its attributes under that
/// (`PUT …/{EmailIdentity}/dkim` and the siblings below).
const SESV2_IDENTITIES_URI: &str = "/v2/email/identities";

/// The error code Amazon SES v2 answers for an identity it does not hold
/// (`NotFoundException`, the modelled 404 of its reads, writes and deletes).
const SESV2_NOT_FOUND: &str = "NotFoundException";

/// The member `CreateEmailIdentity` takes the identity's own text in, which is this
/// resource's physical id.
const EMAIL_IDENTITY_MEMBER: &str = "EmailIdentity";

/// The `AWS::SES::EmailIdentity` property stating the configuration set the identity sends
/// with by default, and the member inside it: the type publishes the pair as a structure
/// while `CreateEmailIdentity` takes the set's name as a member of its own
/// (`ConfigurationSetName`).
const CONFIGURATION_SET_ATTRIBUTES: &str = "ConfigurationSetAttributes";
const CONFIGURATION_SET_NAME_MEMBER: &str = "ConfigurationSetName";

/// The property stating the signing attributes a domain's DKIM is configured with, which
/// `CreateEmailIdentity` takes under the same name and whose members that operation's own
/// `DkimSigningAttributes` shape spells exactly as the type publishes them
/// (`DomainSigningSelector`, `DomainSigningPrivateKey`, `NextSigningKeyLength`).
const DKIM_SIGNING_ATTRIBUTES: &str = "DkimSigningAttributes";

/// The three properties that are a `PutEmailIdentity*Attributes` call of their own, as
/// `(the property, the path under the identity the call is bound to)`: the write is the
/// declared structure itself, because each of those operations takes exactly the members
/// the property publishes (`SigningEnabled`; `EmailForwardingEnabled`; `MailFromDomain` and
/// `BehaviorOnMxFailure`).
const ATTRIBUTE_WRITES: [(&str, &str); 3] = [
    ("DkimAttributes", "dkim"),
    ("FeedbackAttributes", "feedback"),
    ("MailFromAttributes", "mail-from"),
];

/// The properties of the type this arm sends on the CREATE, beside the three writes above,
/// so one it sends nowhere is refused rather than dropped (see [`super`]'s header): a stack
/// never reports an identity configured differently from its declaration.
const CREATE_PROPERTIES: [&str; 3] = [
    EMAIL_IDENTITY_MEMBER,
    CONFIGURATION_SET_ATTRIBUTES,
    DKIM_SIGNING_ATTRIBUTES,
];

/// Create the email identity an `AWS::SES::EmailIdentity` declares, under the text the
/// declaration states — this resource's physical id, which is what `Ref` returns for the
/// type — and apply the attributes the declaration asks for.
///
/// The identity's own text is the only member CloudFormation decides; everything else the
/// request carries is the declaration's. A `MailFromAttributes` or either switch the owning
/// service refuses fails the resource, so the stack reports the refusal that service gave
/// rather than an identity carrying none of the attributes its declaration asked for.
pub(super) async fn create_ses_email_identity(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unsent_properties(resource)?;
    let identity = resource.physical_resource_id.clone();
    let mut request = Map::new();
    request.insert(
        EMAIL_IDENTITY_MEMBER.to_string(),
        Value::String(identity.clone()),
    );
    if let Some(name) = resource
        .properties
        .get(CONFIGURATION_SET_ATTRIBUTES)
        .and_then(|declared| declared.get(CONFIGURATION_SET_NAME_MEMBER))
    {
        request.insert(CONFIGURATION_SET_NAME_MEMBER.to_string(), name.clone());
    }
    if let Some(signing) = resource.properties.get(DKIM_SIGNING_ATTRIBUTES) {
        request.insert(DKIM_SIGNING_ATTRIBUTES.to_string(), signing.clone());
    }
    call_rest_json(
        ctx,
        SESV2_SIGNING_NAME,
        region,
        "POST",
        SESV2_IDENTITIES_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    for (property, path) in ATTRIBUTE_WRITES {
        let Some(declared) = resource.properties.get(property) else {
            continue;
        };
        if let Err(refusal) = call_rest_json(
            ctx,
            SESV2_SIGNING_NAME,
            region,
            "PUT",
            &identity_attribute_uri(&identity, path),
            Some(declared),
        )
        .await
        {
            // The identity this arm created a moment ago is removed before the refusal is
            // reported, the way the configuration-set arm removes a set whose tracking
            // options were refused: a failed resource leaves no identity behind that
            // carries none of the attributes its declaration asked for.
            let _ = delete_ses_email_identity(ctx, region, &identity).await;
            return Err(refusal);
        }
    }
    // "Ref returns the email identity", which is the physical id the identity was created
    // under, and the `Fn::GetAtt` attributes of the type are the Easy DKIM tokens this
    // emulator publishes no return value for.
    Ok(Created::default())
}

/// Delete the email identity a stack created, addressed by its own text — this resource's
/// physical id. One the service no longer holds (`NotFoundException`, judged on the CODE
/// and never on a message that carries the request's own values) is already in the state
/// the delete asks for.
pub(super) async fn delete_ses_email_identity(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json(
        ctx,
        SESV2_SIGNING_NAME,
        region,
        "DELETE",
        &identity_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == SESV2_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The identity's configuration as Amazon SES reports it, for drift detection:
/// `GetEmailIdentity`'s own answer, read by the text that is this resource's physical id.
/// One it no longer holds (`NotFoundException`) was deleted.
///
/// The one member its read reports under a name this type declares is the configuration set
/// the identity sends with, which the type publishes inside its `ConfigurationSetAttributes`
/// structure; a property the read does not report is not evidence of anything
/// ([`super::drift`]).
pub(super) async fn describe_ses_email_identity(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json(
        ctx,
        SESV2_SIGNING_NAME,
        region,
        "GET",
        &identity_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(answer) => {
            let mut reported = Map::new();
            reported.insert(
                EMAIL_IDENTITY_MEMBER.to_string(),
                Value::String(physical_resource_id.to_string()),
            );
            if let Some(name) = reported_text(&answer, CONFIGURATION_SET_NAME_MEMBER) {
                reported.insert(
                    CONFIGURATION_SET_ATTRIBUTES.to_string(),
                    ::serde_json::json!({ CONFIGURATION_SET_NAME_MEMBER: name }),
                );
            }
            Ok(Some(Value::Object(reported)))
        }
        Err(refusal) if refusal.code == SESV2_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The URI one identity is addressed at: the collection plus the identity's own text as the
/// `EmailIdentity` `@httpLabel`, escaped as a single path segment ([`percent_encoded`]) so
/// an address carrying URI structure addresses one identity rather than another path.
fn identity_uri(identity: &str) -> String {
    format!("{SESV2_IDENTITIES_URI}/{}", percent_encoded(identity))
}

/// The URI one of an identity's attribute writes is bound to: the identity's own URI plus
/// the segment that operation's `@http` binding declares.
fn identity_attribute_uri(identity: &str, attribute: &str) -> String {
    format!("{}/{attribute}", identity_uri(identity))
}

/// Refuse a declaration carrying a property this arm sends to no call of the owning
/// service, rather than creating an identity that does not carry it.
fn refuse_unsent_properties(resource: &StoredResource) -> Result<(), Refusal> {
    let Value::Object(declared) = &resource.properties else {
        return Ok(());
    };
    for property in declared.keys() {
        let sent = CREATE_PROPERTIES.contains(&property.as_str())
            || ATTRIBUTE_WRITES
                .iter()
                .any(|(mapped, _)| mapped == property);
        if !sent {
            return Err(Refusal {
                code: NOT_IMPLEMENTED.to_string(),
                message: format!(
                    "this emulator does not send {SES_EMAIL_IDENTITY}'s {property} property to \
                     the service that owns the type, so the resource was not created"
                ),
            });
        }
    }
    Ok(())
}
