Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 57fead3d76889f671b891b7ff2471bd21da9f263

# CreateConnector's ResourceNotFoundException reports ResourceType "CertificateAuthorityArn", a value the model's documented domain excludes

_Filed: 2026-11-16T05:24:30Z_
_Operation: CreateConnector | Class: error-response_

> _Note: this issue was identified against the code base at commit `57fead3d76889f671b891b7ff2471bd21da9f263`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

When Amazon Web Services Private CA reports it holds no certificate authority under the `CertificateAuthorityArn` a create named, `CreateConnector` answers the modelled 404 `ResourceNotFoundException` with its @required `ResourceType` member set to the input member name `"CertificateAuthorityArn"`, and builds the `Message` from the same string (`"CertificateAuthorityArn was not found: <arn>."`).

The model documents that member's value domain as exactly two values: it "can be either `Connector` or `Challenge`". `"CertificateAuthorityArn"` is neither, so the one machine-readable discriminator the error carries is a value no documented case covers.

As with `CreateChallenge`, this also contradicts the vocabulary this same crate declares in `src/state.rs` and uses everywhere else (`state::RESOURCE_TYPE_CONNECTOR` / `state::RESOURCE_TYPE_CHALLENGE`, and `state::resource_type_of()` which can only return those two).

## Evidence

Model (smithy_query query="doc" shape="ResourceNotFoundException" member="ResourceType"):
  "<p>The resource type, which can be either <code>Connector</code> or <code>Challenge</code>.</p>"

Handler, services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/ops/create_connector.rs:495:
  `const MISSING_MEMBER_CERTIFICATE_AUTHORITY_ARN: &str = "CertificateAuthorityArn";`
and :500-506:
```
fn certificate_authority_not_found(certificate_authority_arn: &str) -> CreateConnectorError {
    CreateConnectorErrorModeled::ResourceNotFoundException(crate::state::resource_not_found(
        MISSING_MEMBER_CERTIFICATE_AUTHORITY_ARN,
        certificate_authority_arn,
    ))
    .into()
}
```
reached from :245-249:
```
        if private_ca.is_absent() {
            return Err(certificate_authority_not_found(
                &input.certificate_authority_arn,
            ));
        }
```

The string lands in both the member and the sentence — src/state.rs:760-765 (`resource_type: resource_type.to_string()`).

The crate's own stated vocabulary, src/state.rs:66-70:
```
/// `ResourceId`/`ResourceType` vocabulary of this service's errors: the `ResourceType`
/// member's doc says it "can be either `Connector` or `Challenge`".
pub const RESOURCE_TYPE_CONNECTOR: &str = "Connector";
pub const RESOURCE_TYPE_CHALLENGE: &str = "Challenge";
```

Currently pinned with the crate's own reason rather than the model's — tests/all/missing_resource_status_class.rs:48-51:
```
/// The name of the `CreateConnector` input member that references the private CA, which is
/// what its `ResourceNotFoundException` carries as `ResourceType` (smithy_query
/// operation=CreateConnector).
const MEMBER_CERTIFICATE_AUTHORITY_ARN: &str = "CertificateAuthorityArn";
```
(smithy_query query="operation" operation="CreateConnector" states no such thing; the only doc for the member is the sentence quoted above.)

## Steps to Reproduce

1. Mount this service beside the acm-pca emulator on one engine (as tests/all/missing_resource_status_class.rs does).
2. POST /connectors with {"CertificateAuthorityArn": "arn:aws:acm-pca:eu-west-2:000000000000:certificate-authority/11111111-2222-3333-4444-555555555555"} where Private CA holds no CA under that ARN.
3. Observe HTTP 404 ResourceNotFoundException with ResourceType = "CertificateAuthorityArn" and Message = "CertificateAuthorityArn was not found: <arn>.".
4. Compare with the model sentence for ResourceNotFoundException$ResourceType ("can be either Connector or Challenge").
