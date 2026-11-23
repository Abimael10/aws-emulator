Issue-Severity: MEDIUM
Issue-Gap-Class: error-response
Issue-Commit: 724276c7155ec07ee34cf1e38d33e217c3831042

# CreateConnector's ResourceNotFoundException carries ResourceType "CertificateAuthorityArn", a value the model's ResourceType doc does not admit

_Filed: 2026-11-23T21:20:57Z_
_Operation: CreateConnector | Class: error-response_

> _Note: this issue was identified against the code base at commit `724276c7155ec07ee34cf1e38d33e217c3831042`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

When Amazon Web Services Private CA reports it holds no certificate authority under the `CertificateAuthorityArn` the request named, `CreateConnector` answers the modelled 404 `ResourceNotFoundException` with `ResourceType` set to the literal input-member NAME `"CertificateAuthorityArn"`, and builds the required `Message` from that same string ("CertificateAuthorityArn was not found: <arn>.").

The model documents the values that member carries, and `CertificateAuthorityArn` is not one of them. A client branching on `ResourceType` gets a value outside the documented set.

This is the same shared builder as the `CreateChallenge` case, reached from this operation with a second non-admitted string, so it is a separate operation's response that is wrong in the same way.

## Evidence

Smithy model, verbatim — `smithy_query(query="doc", shape="ResourceNotFoundException", member="ResourceType")`:

    "<p>The resource type, which can be either <code>Connector</code> or <code>Challenge</code>.</p>"

Handler, `services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/src/ops/create_connector.rs:524`:

    const MISSING_MEMBER_CERTIFICATE_AUTHORITY_ARN: &str = "CertificateAuthorityArn";

and `.../src/ops/create_connector.rs:529-535`:

    fn certificate_authority_not_found(certificate_authority_arn: &str) -> CreateConnectorError {
        CreateConnectorErrorModeled::ResourceNotFoundException(crate::state::resource_not_found(
            MISSING_MEMBER_CERTIFICATE_AUTHORITY_ARN,
            certificate_authority_arn,
        ))
        .into()
    }

That string lands in both the field and the sentence, `.../src/state.rs:880-886`:

    message: format!("{resource_type} was not found: {resource_id}."),
    resource_id: resource_id.to_string(),
    resource_type: resource_type.to_string(),

The crate already holds the two admitted values and uses them for the sibling errors of this very handler (`ConflictException` / `ServiceQuotaExceededException` at `.../src/ops/create_connector.rs:365` and `:389` both pass `crate::state::RESOURCE_TYPE_CONNECTOR`), so the 404 is the one place this operation leaves the documented vocabulary.

Test coverage that currently pins the divergence, `.../tests/all/missing_resource_status_class.rs:58-67`:

    /// `ResourceType`'s own doc sentence ("can be either `Connector` or `Challenge`") names the two
    /// resource KINDS this service owns ...
    const MEMBER_CERTIFICATE_AUTHORITY_ARN: &str = "CertificateAuthorityArn";

## Steps to Reproduce

1. Mount this service beside Private CA on one engine (as `tests/all/missing_resource_status_class.rs` does).
2. POST /connectors with {"CertificateAuthorityArn": "arn:aws:acm-pca:eu-west-2:000000000000:certificate-authority/11111111-2222-3333-4444-555555555555"} where Private CA holds no such CA, signed for eu-west-2.
3. Observe HTTP 404 ResourceNotFoundException.
4. Read the body: `ResourceType` is "CertificateAuthorityArn" and `Message` is "CertificateAuthorityArn was not found: arn:...".
5. Expected per the member's own doc: `ResourceType` is one of "Connector" / "Challenge".
