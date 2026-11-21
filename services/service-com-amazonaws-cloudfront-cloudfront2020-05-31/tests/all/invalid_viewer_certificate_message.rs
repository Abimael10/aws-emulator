// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE a distribution configuration whose alternate domain names no certificate it
//! names can serve is refused with, end to end over HTTP.
//!
//! The sibling suite `alias_viewer_certificate` pins WHICH configurations are refused; this
//! one pins what the refusal SAYS. CloudFront answers every such configuration with one
//! fixed sentence — it points the caller at the alternate-domain-name requirements and
//! carries no part of the request, so the alternate domain names submitted, the certificate
//! members stated and the call that carried them cannot change a character of it.
//!
//! What the rows span: one, two and three alternate domain names; a name that is a wildcard
//! and names of different lengths; and every certificate form that names no single trusted
//! certificate — the default certificate stated explicitly, the `ViewerCertificate` element
//! left out entirely, the default certificate stated beside a custom one, both custom
//! members at once, and a custom member left empty. Each row is submitted on all three
//! paths that store a configuration: `CreateDistribution`, `CreateDistributionWithTags` and
//! `UpdateDistribution`.

use crate::distribution_lifecycle::{
    call, create_distribution, emulator, get_distribution, update_distribution, Answer,
    API_VERSION,
};

/// The sentence CloudFront refuses this class with, written out as the service emits it:
/// fixed text with no variable parts.
const EXPECTED_MESSAGE: &str = "To add an alternate domain name (CNAME) to a CloudFront \
     distribution, you must attach a trusted certificate that validates your authorization \
     to use the domain name. For more details, see: \
     https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/CNAMEs.html#alternate-domain-names-requirements";

/// An ACM certificate ARN, which `ViewerCertificate$ACMCertificateArn` names ("the ARN … in
/// Certificate Manager (ACM) … in the US East (N. Virginia) Region").
const ACM_ARN: &str =
    "arn:aws:acm:us-east-1:000000000000:certificate/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

/// An IAM certificate id, which `ViewerCertificate$IAMCertificateId` names ("the ID of the
/// certificate … stored in IAM").
const IAM_CERTIFICATE_ID: &str = "ASCACKCEVSQ6CEXAMPLE2";

/// The certificate members a row states, or `None` for a configuration that leaves the
/// `ViewerCertificate` element out altogether. The three members are the ones the shape
/// declares for the choice: `CloudFrontDefaultCertificate`, `IAMCertificateId` and
/// `ACMCertificateArn` (smithy_query shape com.amazonaws.cloudfront#ViewerCertificate).
#[derive(Debug, Clone, Copy)]
struct Certificate {
    default: Option<bool>,
    acm: Option<&'static str>,
    iam: Option<&'static str>,
}

impl Certificate {
    /// The element as XML, in the member order the shape declares.
    fn xml(&self) -> String {
        let mut out = String::from("<ViewerCertificate>");
        if let Some(default) = self.default {
            out.push_str(&format!(
                "<CloudFrontDefaultCertificate>{default}</CloudFrontDefaultCertificate>"
            ));
        }
        if let Some(iam) = self.iam {
            out.push_str(&format!("<IAMCertificateId>{iam}</IAMCertificateId>"));
        }
        if let Some(acm) = self.acm {
            out.push_str(&format!("<ACMCertificateArn>{acm}</ACMCertificateArn>"));
        }
        if self.acm.is_some() || self.iam.is_some() {
            out.push_str("<SSLSupportMethod>sni-only</SSLSupportMethod>");
            out.push_str("<MinimumProtocolVersion>TLSv1.2_2021</MinimumProtocolVersion>");
        }
        out.push_str("</ViewerCertificate>");
        out
    }
}

/// A certificate kept in ACM: the one form that DOES serve an alternate domain name, used to
/// stand up the distribution the update rows then reconfigure.
const ACM_CERTIFICATE: Certificate = Certificate {
    default: None,
    acm: Some(ACM_ARN),
    iam: None,
};

/// The `DistributionConfig` body for `reference`, with `aliases` and `certificate`.
fn config_xml(reference: &str, aliases: &[&str], certificate: Option<Certificate>) -> String {
    let mut out = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>"
    );
    if !aliases.is_empty() {
        out.push_str(&format!(
            "<Aliases><Quantity>{}</Quantity><Items>",
            aliases.len()
        ));
        for alias in aliases {
            out.push_str(&format!("<CNAME>{alias}</CNAME>"));
        }
        out.push_str("</Items></Aliases>");
    }
    out.push_str(
        "<Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>origin.example.com</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         </CustomOriginConfig></Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>redirect-to-https</ViewerProtocolPolicy>\
         </DefaultCacheBehavior>",
    );
    out.push_str(&format!("<Comment>for {reference}</Comment>"));
    out.push_str("<PriceClass>PriceClass_All</PriceClass>");
    out.push_str("<Enabled>true</Enabled>");
    if let Some(certificate) = certificate {
        out.push_str(&certificate.xml());
    }
    out.push_str("</DistributionConfig>");
    out
}

/// `POST /2020-05-31/distribution?WithTags`, the second create path.
async fn create_distribution_with_tags(
    router: &smithy_server_runtime::MultiServiceRouter,
    config: &str,
) -> Answer {
    let body = format!(
        "<DistributionConfigWithTags \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">{config}<Tags/>\
         </DistributionConfigWithTags>"
    );
    call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution?WithTags"),
        Some(&body),
        None,
    )
    .await
}

/// One row: the alternate domain names and the certificate form a refused configuration
/// states.
struct Case {
    case: &'static str,
    aliases: &'static [&'static str],
    certificate: Option<Certificate>,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "one-alias-default-certificate",
            aliases: &["msg-default.example.com"],
            certificate: Some(Certificate {
                default: Some(true),
                acm: None,
                iam: None,
            }),
        },
        Case {
            case: "one-short-alias-no-certificate-element",
            aliases: &["a.io"],
            certificate: None,
        },
        Case {
            case: "wildcard-alias-default-certificate",
            aliases: &["*.msg-wildcard.example.com"],
            certificate: Some(Certificate {
                default: Some(true),
                acm: None,
                iam: None,
            }),
        },
        Case {
            case: "two-aliases-default-and-acm-certificate",
            aliases: &["msg-two-a.example.com", "msg-two-b.example.com"],
            certificate: Some(Certificate {
                default: Some(true),
                acm: Some(ACM_ARN),
                iam: None,
            }),
        },
        Case {
            case: "three-aliases-acm-and-iam-certificate",
            aliases: &[
                "msg-three-a.example.com",
                "msg-three-b.example.com",
                "msg-three-c.example.com",
            ],
            certificate: Some(Certificate {
                default: None,
                acm: Some(ACM_ARN),
                iam: Some(IAM_CERTIFICATE_ID),
            }),
        },
        Case {
            case: "one-alias-empty-acm-certificate",
            aliases: &["msg-empty.example.com"],
            certificate: Some(Certificate {
                default: None,
                acm: Some(""),
                iam: None,
            }),
        },
        Case {
            case: "one-alias-default-and-iam-certificate",
            aliases: &["msg-default-iam.example.com"],
            certificate: Some(Certificate {
                default: Some(true),
                acm: None,
                iam: Some(IAM_CERTIFICATE_ID),
            }),
        },
    ]
}

/// The `<Message>` of the restXml error envelope, or `""`.
fn message(answer: &Answer) -> &str {
    answer.root().find_text("Message").unwrap_or_default()
}

/// Assert `answer` is the refusal CloudFront states for the class, word for word, and that it
/// repeats nothing the request carried.
fn assert_class_refusal(case: &str, path: &str, aliases: &[&str], answer: &Answer) {
    assert_eq!(
        answer.status, 400,
        "case {case} ({path}): expected HTTP 400, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        "InvalidViewerCertificate",
        "case {case} ({path}): expected InvalidViewerCertificate, got {:?} with {}",
        answer.error_code(),
        answer.raw
    );
    assert_eq!(
        message(answer),
        EXPECTED_MESSAGE,
        "case {case} ({path}): expected the class sentence, got {:?}",
        message(answer)
    );
    for alias in aliases {
        assert!(
            !message(answer).contains(alias),
            "case {case} ({path}): the sentence carries no part of the request, but it \
             names {alias:?}: {:?}",
            message(answer)
        );
    }
}

#[tokio::test]
async fn a_refused_viewer_certificate_states_the_class_sentence_on_every_create_path() {
    let router = emulator();

    for (index, case) in cases().into_iter().enumerate() {
        for (path, answer) in [
            (
                "CreateDistribution",
                create_distribution(
                    &router,
                    &config_xml(
                        &format!("msg-{index}-plain"),
                        case.aliases,
                        case.certificate,
                    ),
                )
                .await,
            ),
            (
                "CreateDistributionWithTags",
                create_distribution_with_tags(
                    &router,
                    &config_xml(
                        &format!("msg-{index}-tagged"),
                        case.aliases,
                        case.certificate,
                    ),
                )
                .await,
            ),
        ] {
            assert_class_refusal(case.case, path, case.aliases, &answer);
        }
    }
}

#[tokio::test]
async fn a_refused_update_states_the_same_sentence_and_changes_nothing() {
    let router = emulator();

    for (index, case) in cases().into_iter().enumerate() {
        // A distribution that IS created: one alternate domain name under a certificate
        // kept in ACM. The update then submits the row's certificate form for the row's
        // alternate domain names.
        let reference = format!("msg-update-{index}");
        let created = create_distribution(
            &router,
            &config_xml(
                &reference,
                &[&format!("msg-update-{index}.example.com")],
                Some(ACM_CERTIFICATE),
            ),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {} (setup): expected HTTP 201, got {} with {}",
            case.case, created.status, created.raw
        );
        let id = created.root().text_at("Id").unwrap_or_default().to_string();
        let etag = created.header("etag").to_string();

        let refused = update_distribution(
            &router,
            &id,
            &config_xml(&reference, case.aliases, case.certificate),
            Some(&etag),
        )
        .await;
        assert_class_refusal(case.case, "UpdateDistribution", case.aliases, &refused);

        let read = get_distribution(&router, &id).await;
        assert_eq!(
            read.status, 200,
            "case {} (read-back): expected HTTP 200, got {} with {}",
            case.case, read.status, read.raw
        );
        assert_eq!(
            read.root()
                .text_at("DistributionConfig/ViewerCertificate/ACMCertificateArn"),
            Some(ACM_ARN),
            "case {} (read-back): the refused update leaves the stored certificate in \
             place, got {}",
            case.case,
            read.raw
        );
        assert_eq!(
            read.header("etag"),
            etag,
            "case {} (read-back): a refused update mints no new version, got {:?}",
            case.case,
            read.header("etag")
        );
    }
}
