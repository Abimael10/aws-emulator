// SPDX-License-Identifier: CC0-1.0

//! The other half of the certificate rule the parent suite asserts: what a distribution on
//! a CUSTOM certificate reports.
//!
//! The rule: the security policy CloudFront applies automatically belongs to its OWN
//! certificate — "If the distribution uses the CloudFront domain name such as
//! `d111111abcdef8.cloudfront.net` (you set `CloudFrontDefaultCertificate` to `true`),
//! CloudFront automatically sets the security policy to `TLSv1` regardless of the value that
//! you set here" (`ViewerCertificate$MinimumProtocolVersion`) — so a distribution served on
//! a certificate of the caller's keeps both the security policy and the SSL support method
//! it declared, and the deprecated pair reports which certificate is in use and where it is
//! kept: `CertificateSource` names the store and `Certificate` the certificate itself. The
//! parent suite's table covers the default-certificate side of the same rule.
//!
//! The rows span what the rule generalises over: which store holds the certificate (ACM,
//! IAM) and which members of the two enums the configuration declares — the declared values
//! differ in every row, and each is a member of this service's own enums
//! (`smithy_query(query="shape",shape="SSLSupportMethod,MinimumProtocolVersion")`). Each row
//! is asserted on the create answer, on `GetDistribution` and on `GetDistributionConfig`, so
//! the three paths agree on the record the create stored.

use super::*;

/// An ACM certificate ARN, in the one Region CloudFront supports ACM certificates from
/// ("CloudFront only supports ACM certificates in the US East (N. Virginia) Region").
const ACM_ARN: &str = "arn:aws:acm:us-east-1:000000000000:certificate/\
     11111111-2222-3333-4444-555555555555";

/// An IAM server certificate id, within the 32-character maximum
/// `ViewerCertificate$IAMCertificateId` carries.
const IAM_ID: &str = "ASCAEXAMPLE1234567890";

/// One row: the certificate a configuration names and the settings the distribution it
/// describes runs under.
struct CustomCertificateRow {
    case: &'static str,
    /// The alternate domain name this row's distribution serves, which is what makes a
    /// certificate of the caller's the one that validates it.
    alias: &'static str,
    /// The `ViewerCertificate` the configuration states.
    certificate: &'static str,
    /// The `SSLSupportMethod` the stored configuration reports: the declared one.
    ssl_support_method: &'static str,
    /// The `MinimumProtocolVersion` the stored configuration reports: the declared one.
    minimum_protocol_version: &'static str,
    /// The `CertificateSource` the stored configuration reports.
    certificate_source: &'static str,
    /// The deprecated `Certificate` member, which names the certificate in use.
    reported_certificate: &'static str,
}

/// The rows: one per store a custom certificate is kept in, each declaring a different pair
/// of enum members.
fn custom_certificate_rows() -> Vec<CustomCertificateRow> {
    vec![
        CustomCertificateRow {
            case: "an ACM certificate, SNI-only over TLSv1.2_2021",
            alias: "acm.example.com",
            certificate: "<ViewerCertificate>\
                          <ACMCertificateArn>arn:aws:acm:us-east-1:000000000000:certificate/\
                          11111111-2222-3333-4444-555555555555</ACMCertificateArn>\
                          <SSLSupportMethod>sni-only</SSLSupportMethod>\
                          <MinimumProtocolVersion>TLSv1.2_2021</MinimumProtocolVersion>\
                          </ViewerCertificate>",
            ssl_support_method: "sni-only",
            minimum_protocol_version: "TLSv1.2_2021",
            certificate_source: CERTIFICATE_SOURCE_ACM,
            reported_certificate: ACM_ARN,
        },
        CustomCertificateRow {
            case: "an ACM certificate, dedicated-IP over TLSv1.3_2025",
            alias: "acm-vip.example.com",
            certificate: "<ViewerCertificate>\
                          <ACMCertificateArn>arn:aws:acm:us-east-1:000000000000:certificate/\
                          11111111-2222-3333-4444-555555555555</ACMCertificateArn>\
                          <SSLSupportMethod>vip</SSLSupportMethod>\
                          <MinimumProtocolVersion>TLSv1.3_2025</MinimumProtocolVersion>\
                          </ViewerCertificate>",
            ssl_support_method: "vip",
            minimum_protocol_version: "TLSv1.3_2025",
            certificate_source: CERTIFICATE_SOURCE_ACM,
            reported_certificate: ACM_ARN,
        },
        CustomCertificateRow {
            case: "an IAM server certificate, SNI-only over TLSv1.2_2018",
            alias: "iam.example.com",
            certificate: "<ViewerCertificate>\
                          <IAMCertificateId>ASCAEXAMPLE1234567890</IAMCertificateId>\
                          <SSLSupportMethod>sni-only</SSLSupportMethod>\
                          <MinimumProtocolVersion>TLSv1.2_2018</MinimumProtocolVersion>\
                          </ViewerCertificate>",
            ssl_support_method: "sni-only",
            minimum_protocol_version: "TLSv1.2_2018",
            certificate_source: CERTIFICATE_SOURCE_IAM,
            reported_certificate: IAM_ID,
        },
    ]
}

/// Every row, on the create answer and on both reads: a distribution on a certificate of
/// the caller's keeps the method and the security policy it declared — the automatic policy
/// is CloudFront's own certificate's — and reports which certificate is in use and where it
/// is kept, while claiming CloudFront's own certificate nowhere.
#[tokio::test]
async fn a_custom_certificate_keeps_the_settings_the_configuration_declared() {
    let router = emulator();

    for (index, row) in custom_certificate_rows().into_iter().enumerate() {
        let case = row.case;
        let document = config_xml(
            &format!("custom-certificate-{index}"),
            &format!(
                "<Aliases><Quantity>1</Quantity><Items><CNAME>{}</CNAME></Items></Aliases>\
                 {MINIMAL_CUSTOM_ORIGIN}{}<Comment>{case}</Comment><Enabled>true</Enabled>{}",
                row.alias,
                minimal_behavior(""),
                row.certificate
            ),
        );

        let created = create_distribution(&router, &document).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected the create to be accepted, got {} ({})",
            created.status, created.raw
        );
        let id = created.root().text_at("Id").unwrap_or_default().to_string();

        for (context, answer) in [
            ("CreateDistribution", created),
            ("GetDistribution", get_distribution(&router, &id).await),
            (
                "GetDistributionConfig",
                get_distribution_config(&router, &id).await,
            ),
        ] {
            // `GetDistributionConfig` answers with the configuration itself, while a
            // `Distribution` payload nests it, so the element is walked for rather than
            // taken as a child of the root.
            let config = answer
                .root()
                .descendant("DistributionConfig")
                .unwrap_or_else(|| {
                    panic!(
                        "case {case} ({context}): the answer carries no DistributionConfig: {}",
                        answer.raw
                    )
                });
            for (member, expected) in [
                ("SSLSupportMethod", row.ssl_support_method),
                ("MinimumProtocolVersion", row.minimum_protocol_version),
                ("CertificateSource", row.certificate_source),
                ("Certificate", row.reported_certificate),
            ] {
                let actual = config.text_at(&format!("ViewerCertificate/{member}"));
                assert_eq!(
                    actual,
                    Some(expected),
                    "case {case} ({context}): expected ViewerCertificate/{member} \
                     {expected:?}, got {actual:?} in {}",
                    answer.raw
                );
            }
            let default = config.text_at("ViewerCertificate/CloudFrontDefaultCertificate");
            assert_eq!(
                default, None,
                "case {case} ({context}): CloudFront's own certificate is not the one \
                 serving this distribution, got {default:?} in {}",
                answer.raw
            );
        }
    }
}
