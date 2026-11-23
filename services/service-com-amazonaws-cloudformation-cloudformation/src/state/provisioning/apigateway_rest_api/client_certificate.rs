// SPDX-License-Identifier: CC0-1.0

//! `AWS::ApiGateway::ClientCertificate`: the certificate API Gateway presents to a backend
//! that authenticates the caller with mutual TLS, minted in Amazon API Gateway itself.
//!
//! A child module of [`super`], which owns the api and the resources it is built out of: the
//! same seam, the same calls and the same helpers (a declared property is read with
//! [`super::stated`], the identifier API Gateway assigned with [`super::own_id`], the tags a
//! resource carries with [`super::gateway_tags`], a property no call here carries is refused
//! with [`super::refuse_unpublished`]), so no two arms of this control plane word a refusal or
//! place a member differently.
//!
//! ## Why it is really created
//!
//! A client certificate is the material API Gateway presents to the integration endpoint
//! behind a stage, named by that stage's `clientCertificateId` ("The identifier of a client
//! certificate for an API stage", `Stage$clientCertificateId`), and only API Gateway can issue
//! one: `GenerateClientCertificate` takes no identifier and answers the `clientCertificateId`
//! it minted together with the PEM-encoded certificate and the window that material is valid
//! over, none of which a template can state or a naming rule compute. So a stack whose
//! certificate exists nowhere publishes a value nothing resolves: `GetClientCertificates` lists
//! no such certificate, `GetClientCertificate` answers `NotFoundException` for the identifier
//! the stack published, a stage naming it is refused, and nothing can be deleted through that
//! service either.
//!
//! "Ref returns the client certificate ID"
//! ([`crate::state::template::identity_is_service_assigned`] names this type for it), so the id
//! that create answers is the resource's physical id and the `ClientCertificateId` attribute
//! the type publishes.
//!
//! ## The calls
//!
//! The type is the calls that owning service publishes for a client certificate, each on the
//! URI its own operation's `@http` binding declares:
//!
//! * the CREATE is `GenerateClientCertificate` (`POST /clientcertificates`), whose
//!   `description` and `tags` the declaration's properties are;
//! * the DELETE is `DeleteClientCertificate` (`DELETE /clientcertificates/{clientCertificateId}`),
//!   which is what makes the identifier the stack published stop resolving once the stack is
//!   gone;
//! * the drift read is `GetClientCertificate`
//!   (`GET /clientcertificates/{clientCertificateId}`).

use super::*;

/// The collection every client certificate of the account lives in, which is what
/// `GenerateClientCertificate` is bound to (`POST /clientcertificates`) and what one
/// certificate hangs off as `/clientcertificates/{clientCertificateId}`.
const CLIENT_CERTIFICATES_URI: &str = "/clientcertificates";

/// Every property `AWS::ApiGateway::ClientCertificate` publishes. A declared property this
/// list does not carry is refused rather than dropped.
///
/// Source: the type's own property set, against `GenerateClientCertificate`'s members
/// (`smithy_query(query="operation",operation="GenerateClientCertificate")`: `description` and
/// `tags`). The identifier the type publishes is read-only — that call takes none — so there is
/// no property for it.
///
/// Visible to [`super::in_place`] too, which judges the document a stack UPDATE states against
/// the same list, so the create and the update cannot differ about what the type publishes.
pub(super) const CLIENT_CERTIFICATE_PROPERTIES: &[&str] = &[DESCRIPTION, "Tags"];

/// The property carrying what the certificate is for, which that create takes as its
/// `description` ("The description of the ClientCertificate").
const DESCRIPTION: &str = "Description";

/// The properties that ARE `GenerateClientCertificate`'s own scalar body members, as
/// `(property, member)` pairs: the description, with a lower-cased initial. The tags are added
/// beside them, since those are the resource's rather than the declaration's alone.
const CLIENT_CERTIFICATE_MEMBERS: &[(&str, &str)] = &[(DESCRIPTION, "description")];

/// The attribute this type publishes, which is the identifier API Gateway minted: "Ref returns
/// the client certificate ID", and `Fn::GetAtt` reads the same value as `ClientCertificateId`.
const CLIENT_CERTIFICATE_ID_ATTRIBUTE: &str = "ClientCertificateId";

/// The member that create and every read of a certificate answer the minted identifier under
/// (`ClientCertificate$clientCertificateId`).
const CLIENT_CERTIFICATE_ID_MEMBER: &str = "clientCertificateId";

/// The properties API Gateway's own read of a certificate reports, as `(property, member)`
/// pairs, for drift detection.
///
/// The DESCRIPTION alone: the tags are left out for the reason [`super::REST_API_REPORTED`]
/// records — the tags a resource carries change independently of its declaration — and
/// everything else that read answers (the issued material and the window it is valid over) is
/// API Gateway's own, which no declaration states, so a difference in one would be a change
/// nobody made.
const CLIENT_CERTIFICATE_REPORTED: &[(&str, &str)] = CLIENT_CERTIFICATE_MEMBERS;

/// Mint the certificate an `AWS::ApiGateway::ClientCertificate` declares, so a client that
/// deployed the stack finds it listed by `GetClientCertificates` — and so a stage naming the
/// identifier the stack published really presents that certificate to its backend — rather than
/// an identifier that resolves to nothing.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unpublished(resource, CLIENT_CERTIFICATE_PROPERTIES)?;
    let mut request = Map::new();
    for (property, member) in CLIENT_CERTIFICATE_MEMBERS {
        if let Some(value) = stated(resource, property) {
            request.insert((*member).to_string(), member_value(property, value)?);
        }
    }
    let tags = gateway_tags(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert("tags".to_string(), Value::Object(tags));
    }
    let answered = call_rest_json(
        ctx,
        API_GATEWAY_SIGNING_NAME,
        region,
        "POST",
        CLIENT_CERTIFICATES_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let id = api_gateway_answered(
        &answered,
        CLIENT_CERTIFICATE_ID_MEMBER,
        "GenerateClientCertificate",
    )?;
    let mut attributes = BTreeMap::new();
    attributes.insert(CLIENT_CERTIFICATE_ID_ATTRIBUTE.to_string(), id.clone());
    Ok(Created {
        physical_resource_id: Some(id),
        attributes,
    })
}

/// The URI that addresses ONE client certificate: the `@httpLabel` every call on it binds.
pub(super) fn client_certificate_uri(client_certificate_id: &str) -> String {
    format!(
        "{CLIENT_CERTIFICATES_URI}/{}",
        percent_encoded(client_certificate_id)
    )
}

/// Delete the certificate from API Gateway; see [`super::delete`].
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    api_gateway_deleted(ctx, region, &client_certificate_uri(own_id(resource)?)).await
}

/// The configuration the certificate has as API Gateway reports it, for drift detection; see
/// [`super::actual_configuration`]. One that service no longer holds (`NotFoundException`,
/// judged on the CODE and never on a message) was deleted.
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let uri = client_certificate_uri(own_id(resource)?);
    match call_rest_json(ctx, API_GATEWAY_SIGNING_NAME, region, "GET", &uri, None).await {
        Ok(answered) => {
            let mut held = Map::new();
            for (property, member) in CLIENT_CERTIFICATE_REPORTED {
                if let Some(value) = answered.get(*member) {
                    held.insert((*property).to_string(), value.clone());
                }
            }
            Ok(Some(Value::Object(held)))
        }
        Err(refusal) if refusal.code == API_GATEWAY_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    use crate::state::template::APIGATEWAY_CLIENT_CERTIFICATE;

    /// A resource record as a deployment hands one to these arms.
    fn declared(properties: Value) -> StoredResource {
        StoredResource {
            logical_id: "Declared".to_string(),
            resource_type: APIGATEWAY_CLIENT_CERTIFICATE.to_string(),
            physical_resource_id: "ab12cd".to_string(),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// Every published property of the type is placed somewhere: in the create's body or as the
    /// resource's tags. A property no arm places would be dropped from a create reported as a
    /// success.
    #[test]
    fn every_published_property_is_placed() {
        let placed: Vec<&str> = CLIENT_CERTIFICATE_MEMBERS
            .iter()
            .map(|(property, _)| *property)
            .chain(["Tags"])
            .collect();
        for property in CLIENT_CERTIFICATE_PROPERTIES {
            assert!(
                placed.contains(property),
                "case {property:?}: expected the property of {APIGATEWAY_CLIENT_CERTIFICATE} to \
                 be placed by some arm, got {placed:?}"
            );
        }
    }

    /// A certificate is addressed by the URI its own operations bind, built from the identifier
    /// API Gateway minted for it.
    #[test]
    fn a_certificate_is_addressed_by_the_uri_its_operations_bind() {
        let rows: &[(&str, &str, &str)] = &[
            ("a minted id", "ab12cd", "/clientcertificates/ab12cd"),
            // A value that is no identifier of that service's is still ONE path segment: an id
            // carrying URI structure would address a different resource.
            ("an id carrying a separator", "a/b", "/clientcertificates/a%2Fb"),
        ];
        for (case, id, expected) in rows {
            let uri = client_certificate_uri(id);
            assert_eq!(
                uri, *expected,
                "case {case:?}: expected the URI {expected:?}, got {uri:?}"
            );
        }
    }

    /// A record carrying no identifier is refused rather than addressing the account's whole
    /// COLLECTION: a read of that URI answers a page and a delete addresses nothing.
    #[test]
    fn a_record_with_no_minted_identifier_is_refused() {
        let mut resource = declared(json!({}));
        resource.physical_resource_id = String::new();
        let refused = own_id(&resource);
        let Err(refusal) = refused else {
            panic!("case \"no minted identifier\": expected a refusal, got {refused:?}");
        };
        assert!(
            refusal.message.contains(APIGATEWAY_CLIENT_CERTIFICATE),
            "case \"no minted identifier\": expected the refusal to name the type, got \
             {refusal:?}"
        );
    }

    /// A declaration states its properties as `GenerateClientCertificate`'s own members: the
    /// description with a lower-cased initial, and nothing for a declaration that states none.
    /// The tags are the RESOURCE's, added beside those members.
    #[test]
    fn a_declaration_travels_as_the_members_the_create_takes() {
        let rows: &[(&str, Value, Value)] = &[
            (
                "a stated description",
                json!({ DESCRIPTION: "the orders backend certificate" }),
                json!({ "description": "the orders backend certificate" }),
            ),
            ("no property at all", json!({}), json!({})),
            (
                "a null description",
                json!({ DESCRIPTION: Value::Null }),
                json!({}),
            ),
            (
                "tags beside the members",
                json!({
                    DESCRIPTION: "described",
                    "Tags": [{ "Key": "estate", "Value": "orders" }],
                }),
                json!({ "description": "described" }),
            ),
        ];
        for (case, properties, expected) in rows {
            let resource = declared(properties.clone());
            let mut request = Map::new();
            for (property, member) in CLIENT_CERTIFICATE_MEMBERS {
                if let Some(value) = stated(&resource, property) {
                    let mapped = member_value(property, value).unwrap_or_else(|refusal| {
                        panic!("case {case:?}: expected a member, got {refusal:?}")
                    });
                    request.insert((*member).to_string(), mapped);
                }
            }
            assert_eq!(
                Value::Object(request.clone()),
                *expected,
                "case {case:?}: expected the members GenerateClientCertificate takes, got \
                 {request:?}"
            );
        }
    }

    /// A declaration stating a property no call here carries is refused naming that property,
    /// rather than a certificate being minted with less than the declaration asked for.
    #[test]
    fn a_property_no_call_here_carries_is_refused() {
        let refused = refuse_unpublished(
            &declared(json!({ DESCRIPTION: "described", "Unpublished": "x" })),
            CLIENT_CERTIFICATE_PROPERTIES,
        );
        let Err(refusal) = refused else {
            panic!("case \"an unpublished property\": expected a refusal, got {refused:?}");
        };
        assert_eq!(
            refusal.code, NOT_IMPLEMENTED,
            "case \"an unpublished property\": expected the refusal to carry \
             {NOT_IMPLEMENTED:?}, got {refusal:?}"
        );
        assert!(
            refusal.message.contains("Unpublished")
                && refusal.message.contains(APIGATEWAY_CLIENT_CERTIFICATE),
            "case \"an unpublished property\": expected the refusal to name the property and the \
             type, got {refusal:?}"
        );
    }
}
