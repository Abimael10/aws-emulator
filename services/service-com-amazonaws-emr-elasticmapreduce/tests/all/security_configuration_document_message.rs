// SPDX-License-Identifier: CC0-1.0

//! What `CreateSecurityConfiguration` says when it will not store the DOCUMENT it was
//! handed, end to end over the wire.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.CreateSecurityConfiguration` and an `awsJson1_1`
//! body — through the router's `dispatch`, using the harness
//! `tests/all/job_flow_lifecycle.rs` owns, so these are the same bytes an SDK client
//! sends. Nothing reaches into the service's state struct: what was stored is read back
//! through `DescribeSecurityConfiguration`.
//!
//! THE RULE: every document-level rejection of a `SecurityConfiguration` is stated under
//! Amazon EMR's one prefix for that class, `Invalid security configuration; `, and the
//! clause that follows it is EMR's own for what is wrong with the document — for a field
//! whose value is outside the set that field defines, the rejected value in parentheses
//! and the values the field allows in a bracketed list, which is what the document is
//! judged against; for a document that is not JSON at all, the constant malformed-document
//! clause. The prefix is how a caller groups the whole class, so no clause is ever stated
//! without it.
//!
//! The rows span what the rule generalises over: every field of the document that defines
//! a value set (not only the encryption mode a recording covers), a value outside each —
//! including a value the field DOES define written in another case — and the other
//! document-level failures, which must carry the same prefix. Each field's accepted values
//! are the control: a document built from them is stored and reads back.

use serde_json::json;

use crate::job_flow_lifecycle::{assert_refused, Emr};
use crate::refusal_messages::message;

/// The modelled client-input error this operation declares
/// (`smithy_query(query="operation", operation="CreateSecurityConfiguration")`:
/// `InvalidRequestException`, HTTP 400).
const REFUSED_CODE: &str = "InvalidRequestException";

/// The prefix Amazon EMR states every document-level rejection of a security
/// configuration with.
const PREFIX: &str = "Invalid security configuration; ";

/// Every value `S3EncryptionConfiguration.EncryptionMode` defines, in the order the
/// document's own reference lists them. (Amazon EMR Management Guide, "Encryption
/// options" / the security configuration JSON reference — the same catalog the handler
/// enforces.)
const S3_ENCRYPTION_MODES: [&str; 4] = ["SSE-S3", "SSE-KMS", "CSE-KMS", "CSE-Custom"];

/// Every value `LocalDiskEncryptionConfiguration.EncryptionKeyProviderType` defines
/// (same reference).
const LOCAL_DISK_KEY_PROVIDER_TYPES: [&str; 2] = ["AwsKms", "Custom"];

/// Every value `TLSCertificateConfiguration.CertificateProviderType` defines (same
/// reference).
const CERTIFICATE_PROVIDER_TYPES: [&str; 2] = ["PEM", "Custom"];

/// A KMS key the rows that need one name.
const KMS_KEY: &str = "arn:aws:kms:us-east-1:123456789012:key/1a1b-2c2d";

/// The clause Amazon EMR states a value outside the set a document field defines with:
/// the rejected value, then the values the field allows.
fn not_allowed(actual: &str, allowed: &[&str]) -> String {
    format!(
        "instance value ({actual}) is not allowed; allowed values: [{}].",
        allowed.join(", ")
    )
}

/// A document whose S3 encryption configuration names `mode`, with the key material that
/// mode needs.
fn s3_document(mode: &str) -> String {
    let mut s3 = json!({ "EncryptionMode": mode });
    match mode {
        "SSE-KMS" | "CSE-KMS" => s3["AwsKmsKey"] = json!(KMS_KEY),
        "CSE-Custom" => {
            s3["EncryptionKeyProviderClass"] = json!("com.example.KeyProvider");
            s3["S3Object"] = json!("s3://my-bucket/my-key-provider.jar");
        }
        _ => {}
    }
    json!({
        "EncryptionConfiguration": {
            "EnableAtRestEncryption": true,
            "AtRestEncryptionConfiguration": { "S3EncryptionConfiguration": s3 },
        }
    })
    .to_string()
}

/// A document whose local-disk encryption configuration names `provider`, with what that
/// provider needs.
fn local_disk_document(provider: &str) -> String {
    let mut local = json!({ "EncryptionKeyProviderType": provider });
    if provider == "AwsKms" {
        local["AwsKmsKey"] = json!(KMS_KEY);
    } else {
        local["EncryptionKeyProviderClass"] = json!("com.example.KeyProvider");
        local["S3Object"] = json!("s3://my-bucket/my-key-provider.jar");
    }
    json!({
        "EncryptionConfiguration": {
            "EnableAtRestEncryption": true,
            "AtRestEncryptionConfiguration": { "LocalDiskEncryptionConfiguration": local },
        }
    })
    .to_string()
}

/// A document whose TLS certificate configuration names `provider`, with what that
/// provider needs.
fn tls_document(provider: &str) -> String {
    let mut tls = json!({
        "CertificateProviderType": provider,
        "S3Object": "s3://my-bucket/my-certs.zip",
    });
    if provider == "Custom" {
        tls["CertificateProviderClass"] = json!("com.example.CertProvider");
    }
    json!({
        "EncryptionConfiguration": {
            "EnableInTransitEncryption": true,
            "InTransitEncryptionConfiguration": { "TLSCertificateConfiguration": tls },
        }
    })
    .to_string()
}

/// A document whose at-rest encryption is switched on and carries `section`.
fn at_rest(section: serde_json::Value) -> String {
    json!({
        "EncryptionConfiguration": {
            "EnableAtRestEncryption": true,
            "AtRestEncryptionConfiguration": section,
        }
    })
    .to_string()
}

/// A document whose in-transit encryption is switched on and carries `tls` as its
/// certificate configuration.
fn in_transit(tls: serde_json::Value) -> String {
    json!({
        "EncryptionConfiguration": {
            "EnableInTransitEncryption": true,
            "InTransitEncryptionConfiguration": { "TLSCertificateConfiguration": tls },
        }
    })
    .to_string()
}

/// Each document the service will not store, with the clause it must be refused with.
fn refused_documents() -> Vec<(String, String, String)> {
    let mut rows: Vec<(String, String, String)> = Vec::new();

    // Each field that defines a value set, with a value outside it and with one of its
    // own values written in another case — enum-style catalogs are case-sensitive, so
    // that is outside the set too.
    for rejected in ["SSE-XYZ", "AES256", "sse-s3"] {
        rows.push((
            format!("an S3 EncryptionMode of {rejected:?}"),
            s3_document(rejected),
            not_allowed(rejected, &S3_ENCRYPTION_MODES),
        ));
    }
    for rejected in ["AwsKmsCustom", "awskms"] {
        rows.push((
            format!("a local-disk EncryptionKeyProviderType of {rejected:?}"),
            local_disk_document(rejected),
            not_allowed(rejected, &LOCAL_DISK_KEY_PROVIDER_TYPES),
        ));
    }
    for rejected in ["JKS", "pem"] {
        rows.push((
            format!("a CertificateProviderType of {rejected:?}"),
            tls_document(rejected),
            not_allowed(rejected, &CERTIFICATE_PROVIDER_TYPES),
        ));
    }

    // The companion class: a value of one field makes another field mandatory, and the
    // clause names the member to add and the values of the deciding field that ask for
    // it.
    for (label, document, clause) in [
        (
            "SSE-KMS with no AwsKmsKey",
            at_rest(json!({
                "S3EncryptionConfiguration": { "EncryptionMode": "SSE-KMS" }
            })),
            "AwsKmsKey are required for 'SSE-KMS' or 'CSE-KMS' EncryptionMode.",
        ),
        (
            "CSE-KMS with no AwsKmsKey",
            at_rest(json!({
                "S3EncryptionConfiguration": { "EncryptionMode": "CSE-KMS" }
            })),
            "AwsKmsKey are required for 'SSE-KMS' or 'CSE-KMS' EncryptionMode.",
        ),
        (
            "CSE-Custom with no EncryptionKeyProviderClass",
            at_rest(json!({
                "S3EncryptionConfiguration": {
                    "EncryptionMode": "CSE-Custom",
                    "S3Object": "s3://my-bucket/my-key-provider.jar",
                }
            })),
            "EncryptionKeyProviderClass are required for 'CSE-Custom' EncryptionMode.",
        ),
        (
            "CSE-Custom with no S3Object",
            at_rest(json!({
                "S3EncryptionConfiguration": {
                    "EncryptionMode": "CSE-Custom",
                    "EncryptionKeyProviderClass": "com.example.KeyProvider",
                }
            })),
            "S3Object are required for 'CSE-Custom' EncryptionMode.",
        ),
        (
            "a local disk AwsKms provider with no AwsKmsKey",
            at_rest(json!({
                "LocalDiskEncryptionConfiguration": { "EncryptionKeyProviderType": "AwsKms" }
            })),
            "AwsKmsKey are required for 'AwsKms' EncryptionKeyProviderType.",
        ),
        (
            "a local disk Custom provider with no EncryptionKeyProviderClass",
            at_rest(json!({
                "LocalDiskEncryptionConfiguration": {
                    "EncryptionKeyProviderType": "Custom",
                    "S3Object": "s3://my-bucket/my-key-provider.jar",
                }
            })),
            "EncryptionKeyProviderClass are required for 'Custom' EncryptionKeyProviderType.",
        ),
        (
            "a PEM certificate provider with no S3Object",
            in_transit(json!({ "CertificateProviderType": "PEM" })),
            "S3Object are required for 'PEM' or 'Custom' CertificateProviderType.",
        ),
        (
            "a Custom certificate provider with no CertificateProviderClass",
            in_transit(json!({
                "CertificateProviderType": "Custom",
                "S3Object": "s3://my-bucket/my-certs.zip",
            })),
            "CertificateProviderClass are required for 'Custom' CertificateProviderType.",
        ),
    ] {
        rows.push((label.to_string(), document, clause.to_string()));
    }

    // The malformed-document class: one constant clause, under the same prefix.
    for (label, document) in [
        ("bare text", "this-is-not-json"),
        ("an unterminated object", "{\"EncryptionConfiguration\":"),
        ("an empty document", ""),
    ] {
        rows.push((
            format!("a document that is not JSON: {label}"),
            document.to_string(),
            "Malformed Json string.".to_string(),
        ));
    }

    rows
}

/// THE RULE: every document-level rejection carries EMR's prefix, and a value outside the
/// set a document field defines is stated as that field's judgement words it.
#[tokio::test]
async fn a_document_level_rejection_is_stated_under_amazon_emrs_prefix() {
    let emr = Emr::new();
    for (index, (label, document, clause)) in refused_documents().into_iter().enumerate() {
        let name = format!("refused-{index}");
        let (status, payload) = emr
            .call(
                "CreateSecurityConfiguration",
                json!({ "Name": &name, "SecurityConfiguration": document }),
            )
            .await;
        assert_refused(&label, status, &payload, REFUSED_CODE);
        let said = message(&label, &payload);
        let expected = format!("{PREFIX}{clause}");
        assert_eq!(
            said, expected,
            "case {label:?}: expected {expected:?}, got {said:?}"
        );

        // A refused create stores nothing, so the name is still free of a configuration.
        let (status, payload) = emr
            .call("DescribeSecurityConfiguration", json!({ "Name": &name }))
            .await;
        assert_refused(
            &format!("{label} (nothing persisted)"),
            status,
            &payload,
            REFUSED_CODE,
        );
    }
}

/// THE RULE's other half: a document whose fields all name values they define is stored,
/// and reads back verbatim — so the clauses above are about the value and not about the
/// field carrying one.
#[tokio::test]
async fn every_value_a_document_field_defines_is_stored() {
    let emr = Emr::new();
    let mut accepted: Vec<(String, String)> = Vec::new();
    for mode in S3_ENCRYPTION_MODES {
        accepted.push((format!("s3-{mode}"), s3_document(mode)));
    }
    for provider in LOCAL_DISK_KEY_PROVIDER_TYPES {
        accepted.push((format!("local-{provider}"), local_disk_document(provider)));
    }
    for provider in CERTIFICATE_PROVIDER_TYPES {
        accepted.push((format!("tls-{provider}"), tls_document(provider)));
    }

    for (name, document) in accepted {
        let label = format!("the document {name}");
        let (status, payload) = emr
            .call(
                "CreateSecurityConfiguration",
                json!({ "Name": &name, "SecurityConfiguration": &document }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected HTTP 200, got {status} with {payload}"
        );
        let described = emr
            .ok("DescribeSecurityConfiguration", json!({ "Name": &name }))
            .await;
        assert_eq!(
            described["SecurityConfiguration"].as_str(),
            Some(document.as_str()),
            "case {label:?}: expected the stored document back verbatim, got {described}"
        );
    }
}

/// THE RULE's prefix on the document-level failures that are NOT about a value set: a
/// section a switched-on flag requires and is missing, and a section that is not an
/// object. Each is grouped by the same prefix, whatever the clause.
#[tokio::test]
async fn the_other_document_level_failures_carry_the_same_prefix() {
    let emr = Emr::new();
    let documents = [
        (
            "at rest enabled with no AtRestEncryptionConfiguration",
            json!({ "EncryptionConfiguration": { "EnableAtRestEncryption": true } }).to_string(),
        ),
        (
            "in transit enabled with no InTransitEncryptionConfiguration",
            json!({ "EncryptionConfiguration": { "EnableInTransitEncryption": true } })
                .to_string(),
        ),
        (
            "EncryptionConfiguration that is not an object",
            json!({ "EncryptionConfiguration": "all of it" }).to_string(),
        ),
        (
            "a flag that is not a flag",
            json!({ "EncryptionConfiguration": { "EnableAtRestEncryption": "true" } }).to_string(),
        ),
    ];

    for (index, (label, document)) in documents.into_iter().enumerate() {
        let name = format!("other-{index}");
        let (status, payload) = emr
            .call(
                "CreateSecurityConfiguration",
                json!({ "Name": &name, "SecurityConfiguration": document }),
            )
            .await;
        assert_refused(label, status, &payload, REFUSED_CODE);
        let said = message(label, &payload);
        assert!(
            said.starts_with(PREFIX),
            "case {label:?}: expected a message under {PREFIX:?}, got {said:?}"
        );
        assert!(
            said.len() > PREFIX.len(),
            "case {label:?}: the prefix is followed by the clause, got {said:?}"
        );
    }
}
