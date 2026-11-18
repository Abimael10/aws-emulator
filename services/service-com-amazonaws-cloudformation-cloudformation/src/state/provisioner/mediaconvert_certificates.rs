// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arm of AWS Elemental MediaConvert's CERTIFICATE ASSOCIATION:
//! the trust an `AWS::MediaConvert::CertificateAssociation` declares between an AWS
//! Certificate Manager certificate and the account's MediaConvert service. One file of
//! [`super`]'s per-type arms, dispatched to by its `Provisioner`.
//!
//! The resource IS the certificate it trusts. `AssociateCertificate`
//! (`smithy_query(query="operation", operation="AssociateCertificate")` of the
//! `mediaconvert` model: `POST /2017-08-29/certificates`, required input member `Arn` with
//! `jsonName: arn`, success 201) takes that ARN and no identifier of its own, and
//! `DisassociateCertificate` (`DELETE /2017-08-29/certificates/{Arn}`) addresses the
//! association by the same ARN — so the type's one property is the whole request, the ARN is
//! the resource's physical id ([`super::template::declared_name_property`] reads it off the
//! declaration) and that is what the type's `Ref` returns.
//!
//! MediaConvert is also what JUDGES the ARN: a value that is no ACM certificate ARN is its
//! `BadRequestException` and an ARN Certificate Manager issued no certificate under is its
//! `NotFoundException`, so a declaration naming a certificate that is not there fails the
//! resource with that service's own refusal — reported through the `NotFound` handler-error
//! class the code falls in ([`super::provisioning::handler_error_code`]) — rather than being
//! recorded as a trust nothing established.
//!
//! There is no UPDATE arm: an association is the pair it binds, and the only property of the
//! type is that pair's certificate, so a declaration naming another certificate replaces the
//! resource ([`super::template::replacement_properties`]) instead of changing one.

use super::*;

/// AWS Elemental MediaConvert's signing name, from its model (`sigv4Name: mediaconvert`),
/// which speaks `restJson1` — so its calls ride on [`call_rest_json_named`], each on the URI
/// its own operation's `@http` binding declares, naming the operation so a refusal is
/// reported the way that service's own SDK raises it.
///
/// It is also that model's `awsService.arnNamespace`, which is the vendor field of every ARN
/// of that service — the field [`mediaconvert_is_using`] reads to tell a resource of this
/// service from any other consumer a certificate's report names.
pub(super) const MEDIACONVERT_SIGNING_NAME: &str = "mediaconvert";

/// The collection `AssociateCertificate` is bound to (`POST /2017-08-29/certificates`), under
/// which one association is addressed by the certificate's own ARN
/// (`DELETE /2017-08-29/certificates/{Arn}`).
const CERTIFICATES_URI: &str = "/2017-08-29/certificates";

/// The operations this module calls, as that service's own model names them.
const ASSOCIATE_CERTIFICATE: &str = "AssociateCertificate";
const DISASSOCIATE_CERTIFICATE: &str = "DisassociateCertificate";

/// The error code MediaConvert answers for a certificate it cannot resolve
/// (`NotFoundException`, the modelled 404 of both calls): the association's delete reads it as
/// the state the delete asks for.
const MEDIACONVERT_NOT_FOUND: &str = "NotFoundException";

/// The type's ONE property, and the member `AssociateCertificate` takes it as on the wire
/// (`jsonName: arn`): "the ARN of the ACM certificate that you want to associate with your
/// MediaConvert resource".
pub(super) const CERTIFICATE_ARN_PROPERTY: &str = "Arn";
const CERTIFICATE_ARN_MEMBER: &str = "arn";

/// The member Certificate Manager's own `DescribeCertificate` answers the certificate's detail
/// in, and the member of that detail listing the resources using it
/// (`CertificateDetail$InUseBy`) — which is where the association this type declares is read
/// back from.
const REPORTED_CERTIFICATE: &str = "Certificate";
const REPORTED_IN_USE_BY: &str = "InUseBy";

/// Associate the certificate an `AWS::MediaConvert::CertificateAssociation` declares with the
/// account's MediaConvert service, so a client reading the stack's output finds the
/// certificate reported as in use by that service and MediaConvert really accepts it.
///
/// The declared `Arn` IS the request: it travels as the one member that call takes, under the
/// name that member carries on the wire. The identity is the certificate's own ARN, which the
/// naming rules already computed for the resource, so nothing here replaces it.
pub(super) async fn associate_mediaconvert_certificate(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let mut request = Map::new();
    request.insert(
        CERTIFICATE_ARN_MEMBER.to_string(),
        Value::String(required_property(resource, CERTIFICATE_ARN_PROPERTY)?),
    );
    call_rest_json_named(
        ctx,
        MEDIACONVERT_SIGNING_NAME,
        region,
        ASSOCIATE_CERTIFICATE,
        "POST",
        CERTIFICATES_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    Ok(Created::attributes(BTreeMap::new()))
}

/// Take the association a stack made away again, by the certificate ARN it was made under —
/// this resource's physical id, which is the `@httpLabel` that call addresses one association
/// by ([`percent_encoded`] writes it as a single path segment, so an ARN's own `:` and `/`
/// address one association rather than another path).
///
/// An association MediaConvert no longer holds (`NotFoundException`, judged on the CODE and
/// never on a message that carries the request's own values) is already in the state the
/// delete asks for.
pub(super) async fn disassociate_mediaconvert_certificate(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        MEDIACONVERT_SIGNING_NAME,
        region,
        DISASSOCIATE_CERTIFICATE,
        "DELETE",
        &format!("{CERTIFICATES_URI}/{}", percent_encoded(physical_resource_id)),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == MEDIACONVERT_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The association's configuration as the owning services report it, for drift detection: the
/// declared `Arn` where a resource of MediaConvert is still using the certificate, and `None`
/// where the association is gone.
///
/// MediaConvert publishes no read of its certificate associations at all — the account's
/// trusted set is write-only on that API (`smithy_query(query="list_operations")` of the
/// `mediaconvert` model lists the associate and the disassociate and nothing else about
/// certificates) — so the association is read from the other side of it: Certificate Manager
/// reports "the list of ARNs for the AWS resources that are used by the certificate"
/// (`CertificateDetail$InUseBy`), and this trust is exactly what puts a MediaConvert resource
/// in that list. A certificate that service no longer holds is a certificate nothing can be
/// using, which is the same answer.
pub(super) async fn describe_mediaconvert_certificate_association(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ "CertificateArn": physical_resource_id });
    let reported = match call_json_rpc(
        ctx,
        ACM_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{ACM_TARGET_PREFIX}.DescribeCertificate"),
        &request,
    )
    .await
    {
        Ok(reported) => reported,
        Err(refusal) if refusal.code == RESOURCE_NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    if !mediaconvert_is_using(&reported) {
        return Ok(None);
    }
    let mut properties = Map::new();
    properties.insert(
        CERTIFICATE_ARN_PROPERTY.to_string(),
        Value::String(physical_resource_id.to_string()),
    );
    Ok(Some(Value::Object(properties)))
}

/// Whether the certificate Certificate Manager reported is in use by a resource of AWS
/// Elemental MediaConvert.
///
/// Judged on the VENDOR field of each reported ARN ([`::service_common::arn::parse`]), which
/// is the field that says whose resource an ARN names: the account's MediaConvert service is
/// the consumer this trust creates, and no other service's resource in that list says anything
/// about this association.
fn mediaconvert_is_using(reported: &Value) -> bool {
    let Some(Value::Array(in_use_by)) = reported
        .get(REPORTED_CERTIFICATE)
        .and_then(|detail| detail.get(REPORTED_IN_USE_BY))
    else {
        return false;
    };
    in_use_by
        .iter()
        .filter_map(Value::as_str)
        .filter_map(::service_common::arn::parse)
        .any(|arn| arn.vendor == MEDIACONVERT_SIGNING_NAME)
}
