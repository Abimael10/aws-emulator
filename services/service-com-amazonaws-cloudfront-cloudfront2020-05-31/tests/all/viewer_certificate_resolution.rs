// SPDX-License-Identifier: CC0-1.0

//! WHICH certificate a distribution may attach an alternate domain name under, end to end over
//! HTTP.
//!
//! Identity and Access Management and Certificate Manager are MOUNTED beside CloudFront on
//! the same engine, because the certificate a `ViewerCertificate` names is another service's
//! resource: the server certificates these cases name are ones this suite really uploaded
//! through `UploadServerCertificate`, and the managed one is a certificate it really brought
//! in through `ImportCertificate`, both built from real X.509 material (`rcgen`) — so the facts
//! the handler reads (who signed the certificate and which hosts it covers) come from those
//! services' own records rather than from anything this suite told CloudFront.
//!
//! RULE under test: an alternate domain name is served only under a certificate that
//! "validates your authorization to use the domain name"
//! (`smithy_query(query="shape",shape="ViewerCertificate")`), so a configuration that claims
//! one is refused — `InvalidViewerCertificate`, HTTP 400, with the one sentence CloudFront
//! states for a certificate it will not present — unless the certificate it names can serve
//! every name claimed:
//!
//! * a certificate the account holds in Identity and Access Management has to have an
//!   authority above it (one that signed ITSELF is verifiable by no viewer and travels with no
//!   chain that could make it so) and has to COVER every alternate domain name, by the match a
//!   TLS client applies to a certificate's names — a leading `*` label covers one label in
//!   that position and not the bare name beneath it;
//! * a certificate in Certificate Manager has to be in the one Region CloudFront presents one
//!   from: "CloudFront only supports ACM certificates in the US East (N. Virginia) Region
//!   (`us-east-1`)" (`ViewerCertificate$ACMCertificateArn`);
//! * a value NEITHER service resolves anything for is evidence of nothing and is left alone.
//!
//! What the rows span: the two directions of the authority rule (a CA-signed certificate and a
//! self-signed one), coverage exactly, by wildcard, past a wildcard and not at all, one alias
//! and two, and — for the Region rule — the supported Region, three others and values of no
//! ACM resource at all. Every row is submitted on both create paths, and the stored
//! distribution of an accepted row is read back; one refusal is also driven through
//! `UpdateDistribution`, which leaves the stored certificate where it was.

use std::sync::Arc;

use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

use crate::distribution_lifecycle::{call, get_distribution, Answer, API_VERSION};

/// The Region the clients below sign for.
const REGION: &str = "us-east-1";

/// The sentence CloudFront refuses a certificate it will not present with, written out here as
/// the service emits it: fixed text with no variable parts, so neither the alternate domain
/// name nor the certificate reaches it.
const UNUSABLE_CERTIFICATE: &str = "The specified SSL certificate doesn't exist, isn't in \
                                    us-east-1 region, isn't valid, or doesn't include a valid \
                                    certificate chain.";

// ---------------------------------------------------------------------------
// The emulator: CloudFront plus the service that holds the certificates
// ---------------------------------------------------------------------------

/// CloudFront, Identity and Access Management and Certificate Manager over ONE shared engine,
/// with a callback into the same router so this suite brings its certificates in through those
/// services' own APIs — the very records the handler reads them from.
struct Emulator {
    router: MultiServiceRouter,
    callback: EmbeddedEmulator,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_acm_certificatemanager::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        let callback = EmbeddedEmulator::from_router(router.clone());
        Self { router, callback }
    }

    /// The Identity and Access Management client, over the in-process callback.
    fn iam(&self) -> aws_sdk_iam::Client {
        service_common::iam_client::iam_client_through(&self.callback, REGION)
    }

    /// The Certificate Manager client, over the same callback.
    fn acm(&self) -> aws_sdk_acm::Client {
        let config = aws_sdk_acm::Config::builder()
            .behavior_version(aws_sdk_acm::config::BehaviorVersion::latest())
            .region(aws_sdk_acm::config::Region::new(REGION))
            .endpoint_url(format!("https://acm.{REGION}.amazonaws.com"))
            .credentials_provider(self.callback.aws_credentials_provider())
            .http_client(self.callback.http_client())
            .retry_config(aws_sdk_acm::config::retry::RetryConfig::disabled())
            .timeout_config(aws_sdk_acm::config::timeout::TimeoutConfig::disabled())
            .identity_cache(aws_sdk_acm::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                aws_sdk_acm::config::StalledStreamProtectionConfig::disabled(),
            )
            .build();
        aws_sdk_acm::Client::from_conf(config)
    }

    /// Bring `minted` into Certificate Manager through its OWN `ImportCertificate` and report
    /// the ARN it stored the certificate under — the value `ACMCertificateArn` names it by.
    async fn import(&self, case: &str, minted: &Minted) -> String {
        let mut request = self
            .acm()
            .import_certificate()
            .certificate(aws_smithy_types::Blob::new(
                minted.certificate_pem.clone().into_bytes(),
            ))
            .private_key(aws_smithy_types::Blob::new(
                minted.private_key_pem.clone().into_bytes(),
            ));
        if let Some(chain) = &minted.chain_pem {
            request = request.certificate_chain(aws_smithy_types::Blob::new(
                chain.clone().into_bytes(),
            ));
        }
        let imported = request
            .send()
            .await
            .unwrap_or_else(|error| panic!("case {case}: importing the certificate: {error:?}"));
        imported
            .certificate_arn()
            .unwrap_or_else(|| panic!("case {case}: the import reports the ARN it stored"))
            .to_string()
    }

    /// Upload `minted` under `name` and report the `ServerCertificateId` that service minted
    /// for it — the value `ViewerCertificate$IAMCertificateId` names a certificate by.
    async fn upload(&self, case: &str, name: &str, minted: &Minted) -> String {
        let mut request = self
            .iam()
            .upload_server_certificate()
            .server_certificate_name(name)
            .certificate_body(minted.certificate_pem.clone())
            .private_key(minted.private_key_pem.clone());
        if let Some(chain) = &minted.chain_pem {
            request = request.certificate_chain(chain.clone());
        }
        let uploaded = request.send().await.unwrap_or_else(|error| {
            panic!("case {case}: uploading the server certificate {name}: {error}")
        });
        uploaded
            .server_certificate_metadata()
            .map(|metadata| metadata.server_certificate_id().to_string())
            .unwrap_or_else(|| panic!("case {case}: the upload reports the certificate's id"))
    }
}

// ---------------------------------------------------------------------------
// Real certificate material
// ---------------------------------------------------------------------------

/// A certificate authority a case signs a leaf with, so a case's material can carry a CHAIN as
/// well as a leaf.
struct Authority {
    certificate: rcgen::Certificate,
    key: rcgen::KeyPair,
}

/// A real CA certificate, generated here.
fn authority() -> Authority {
    let key = rcgen::KeyPair::generate().expect("a key pair");
    let mut params =
        rcgen::CertificateParams::new(vec!["viewer-certificate-ca.example.com".to_string()])
            .expect("certificate parameters");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    let certificate = params.self_signed(&key).expect("a self-signed CA");
    Authority { certificate, key }
}

/// The material one case uploads: the leaf, the key that matches it and the chain, which is
/// `None` for a certificate that signed itself.
struct Minted {
    certificate_pem: String,
    private_key_pem: String,
    chain_pem: Option<String>,
}

/// A real leaf covering `names`, signed by `signer` when there is one and self-signed
/// otherwise.
fn mint(names: &[String], signer: Option<&Authority>) -> Minted {
    let key = rcgen::KeyPair::generate().expect("a key pair");
    let mut params =
        rcgen::CertificateParams::new(names.to_vec()).expect("certificate parameters");
    let mut subject = rcgen::DistinguishedName::new();
    subject.push(
        rcgen::DnType::CommonName,
        names.first().expect("a certificate covers a name").clone(),
    );
    params.distinguished_name = subject;
    let (certificate, chain_pem) = match signer {
        None => (params.self_signed(&key).expect("a self-signed leaf"), None),
        Some(signer) => (
            params
                .signed_by(&key, &signer.certificate, &signer.key)
                .expect("a leaf signed by the authority"),
            Some(signer.certificate.pem()),
        ),
    };
    Minted {
        certificate_pem: certificate.pem(),
        private_key_pem: key.serialize_pem(),
        chain_pem,
    }
}

// ---------------------------------------------------------------------------
// The configurations under test
// ---------------------------------------------------------------------------

/// A host of a row's own zone: `"@"` is the zone itself, `"*"` the wildcard one label above it,
/// and anything else that label of the zone.
fn host(zone: &str, label: &str) -> String {
    match label {
        "@" => zone.to_string(),
        "*" => format!("*.{zone}"),
        label => format!("{label}.{zone}"),
    }
}

/// The `DistributionConfig` for `reference` claiming `aliases` under the certificate element
/// `certificate`.
fn config_xml(reference: &str, aliases: &[String], certificate: &str) -> String {
    let items: String = aliases
        .iter()
        .map(|alias| format!("<CNAME>{alias}</CNAME>"))
        .collect();
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Aliases><Quantity>{}</Quantity><Items>{items}</Items></Aliases>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>origin.example.com</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         </CustomOriginConfig></Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>redirect-to-https</ViewerProtocolPolicy>\
         </DefaultCacheBehavior>\
         <Comment>for {reference}</Comment>\
         <PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled>{certificate}</DistributionConfig>",
        aliases.len()
    )
}

/// The `ViewerCertificate` element naming the certificate `ServerCertificateId`
/// `certificate_id` holds in Identity and Access Management.
fn iam_certificate(certificate_id: &str) -> String {
    format!(
        "<ViewerCertificate><IAMCertificateId>{certificate_id}</IAMCertificateId>\
         <SSLSupportMethod>sni-only</SSLSupportMethod>\
         <MinimumProtocolVersion>TLSv1.2_2021</MinimumProtocolVersion></ViewerCertificate>"
    )
}

/// The `ViewerCertificate` element naming the Certificate Manager certificate `arn`.
fn acm_certificate(arn: &str) -> String {
    format!(
        "<ViewerCertificate><ACMCertificateArn>{arn}</ACMCertificateArn>\
         <SSLSupportMethod>sni-only</SSLSupportMethod>\
         <MinimumProtocolVersion>TLSv1.2_2021</MinimumProtocolVersion></ViewerCertificate>"
    )
}

/// `POST /2020-05-31/distribution` with the configuration `config`.
async fn create(router: &MultiServiceRouter, config: &str) -> Answer {
    call(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(config),
        None,
    )
    .await
}

/// `POST /2020-05-31/distribution?WithTags` with the configuration `config`, the second create
/// path.
async fn create_with_tags(router: &MultiServiceRouter, config: &str) -> Answer {
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

/// `PUT /2020-05-31/distribution/{Id}/config` under the version token `if_match`.
async fn update(router: &MultiServiceRouter, id: &str, config: &str, if_match: &str) -> Answer {
    call(
        router,
        "PUT",
        &format!("/{API_VERSION}/distribution/{id}/config"),
        Some(config),
        Some(if_match),
    )
    .await
}

/// One row: the hosts the configuration claims, the hosts the certificate covers, whether that
/// certificate signed itself, and whether the write has to be refused.
struct Row {
    case: &'static str,
    /// The aliases claimed, as labels of the row's own zone ([`host`]).
    alias_labels: &'static [&'static str],
    /// The names the uploaded certificate carries, as labels of that same zone.
    certificate_labels: &'static [&'static str],
    /// Whether the certificate signed itself, so nothing stands above it.
    self_signed: bool,
    refused: bool,
}

/// The table: both directions of the authority rule, and coverage exactly, by wildcard, past a
/// wildcard, not at all, over one alias and two.
const ROWS: &[Row] = &[
    Row {
        case: "a CA-signed certificate covering the one alias",
        alias_labels: &["@"],
        certificate_labels: &["@"],
        self_signed: false,
        refused: false,
    },
    Row {
        case: "a self-signed certificate covering the one alias",
        alias_labels: &["@"],
        certificate_labels: &["@"],
        self_signed: true,
        refused: true,
    },
    Row {
        case: "a CA-signed certificate for another host of the zone",
        alias_labels: &["@"],
        certificate_labels: &["other"],
        self_signed: false,
        refused: true,
    },
    Row {
        case: "a CA-signed wildcard covering the alias's label",
        alias_labels: &["host"],
        certificate_labels: &["*"],
        self_signed: false,
        refused: false,
    },
    Row {
        case: "a wildcard does not cover the zone beneath it",
        alias_labels: &["@"],
        certificate_labels: &["*"],
        self_signed: false,
        refused: true,
    },
    Row {
        case: "two aliases, the certificate covering only one of them",
        alias_labels: &["@", "host"],
        certificate_labels: &["@"],
        self_signed: false,
        refused: true,
    },
    Row {
        case: "two aliases, the certificate covering both",
        alias_labels: &["@", "host"],
        certificate_labels: &["@", "host"],
        self_signed: false,
        refused: false,
    },
];

/// Assert `answer` is the refusal CloudFront states for a certificate it will not present, and
/// that it created nothing.
fn assert_refused(case: &str, path: &str, aliases: &[String], answer: &Answer) {
    assert_eq!(
        answer.status, 400,
        "case {case:?} ({path}): expected HTTP 400, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        "InvalidViewerCertificate",
        "case {case:?} ({path}): expected InvalidViewerCertificate, got {:?} with {}",
        answer.error_code(),
        answer.raw
    );
    let message = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        message, UNUSABLE_CERTIFICATE,
        "case {case:?} ({path}): expected {UNUSABLE_CERTIFICATE:?}, got {message:?}"
    );
    for alias in aliases {
        assert!(
            !message.contains(alias.as_str()),
            "case {case:?} ({path}): the sentence carries no part of the request, but it names \
             {alias:?}: {message:?}"
        );
    }
    assert!(
        answer.root().find_text("Id").is_none(),
        "case {case:?} ({path}): a refused write creates no distribution, got {}",
        answer.raw
    );
}

/// Assert the accepted write stored the configuration it was given: the aliases and the
/// certificate are read back through `GetDistribution`.
async fn assert_stored(
    router: &MultiServiceRouter,
    case: &str,
    path: &str,
    aliases: &[String],
    certificate_id: &str,
    answer: &Answer,
) {
    assert_eq!(
        answer.status, 201,
        "case {case:?} ({path}): expected HTTP 201, got {} with {}",
        answer.status, answer.raw
    );
    let id = answer.root().text_at("Id").unwrap_or_default().to_string();
    assert!(
        !id.is_empty(),
        "case {case:?} ({path}): expected a minted distribution id, got {}",
        answer.raw
    );
    let read = get_distribution(router, &id).await;
    assert_eq!(
        read.status, 200,
        "case {case:?} ({path}): expected the distribution to read back, got {} with {}",
        read.status, read.raw
    );
    let stored: Vec<String> = read
        .root()
        .at("DistributionConfig/Aliases/Items")
        .map(|items| {
            items
                .children_named("CNAME")
                .iter()
                .map(|node| node.text().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        stored, aliases,
        "case {case:?} ({path}): expected the aliases stored as submitted, got {stored:?}"
    );
    assert_eq!(
        read.root()
            .text_at("DistributionConfig/ViewerCertificate/IAMCertificateId"),
        Some(certificate_id),
        "case {case:?} ({path}): expected the stored certificate to be the one named, got {}",
        read.raw
    );
}

/// RULE: every row is judged by what Identity and Access Management records about the
/// certificate it names — who signed it and which hosts it covers — on both create paths.
#[tokio::test]
async fn a_certificate_can_attach_an_alias_only_when_it_can_serve_that_name() {
    let emulator = Emulator::new();
    let signer = authority();

    for (index, row) in ROWS.iter().enumerate() {
        // Each row works in its own zone, and each PATH in its own zone under it, so no two
        // writes of this suite ever claim the same alternate domain name.
        for (position, path) in ["CreateDistribution", "CreateDistributionWithTags"]
            .into_iter()
            .enumerate()
        {
            let zone = format!("vc-{index}-{position}.example.com");
            let aliases: Vec<String> = row
                .alias_labels
                .iter()
                .map(|label| host(&zone, label))
                .collect();
            let covered: Vec<String> = row
                .certificate_labels
                .iter()
                .map(|label| host(&zone, label))
                .collect();
            let certificate_id = emulator
                .upload(
                    row.case,
                    &format!("viewer-certificate-{index}-{position}"),
                    &mint(
                        &covered,
                        if row.self_signed { None } else { Some(&signer) },
                    ),
                )
                .await;
            let reference = format!("viewer-certificate-{index}-{position}");
            let config = config_xml(&reference, &aliases, &iam_certificate(&certificate_id));
            let answer = match position {
                0 => create(&emulator.router, &config).await,
                _ => create_with_tags(&emulator.router, &config).await,
            };
            if row.refused {
                assert_refused(row.case, path, &aliases, &answer);
            } else {
                assert_stored(
                    &emulator.router,
                    row.case,
                    path,
                    &aliases,
                    &certificate_id,
                    &answer,
                )
                .await;
            }
        }
    }
}

/// RULE: a certificate id Identity and Access Management holds nothing under is evidence of
/// nothing, so the configuration is stored — and the Region rule for a Certificate Manager
/// certificate is read off the ARN itself.
#[tokio::test]
async fn an_acm_arn_is_judged_by_the_region_it_names() {
    let emulator = Emulator::new();

    // (case, the ACMCertificateArn submitted, whether the write is refused)
    let rows: &[(&str, &str, bool)] = &[
        (
            "an ACM certificate ARN in the one supported Region",
            "arn:aws:acm:us-east-1:000000000000:certificate/\
             aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            false,
        ),
        (
            "an ACM certificate ARN in another Region",
            "arn:aws:acm:us-west-2:000000000000:certificate/\
             aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            true,
        ),
        (
            "an ACM certificate ARN in a Region of another continent",
            "arn:aws:acm:eu-west-1:000000000000:certificate/\
             aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            true,
        ),
        (
            "an ACM certificate ARN in a Region of yet another partition's naming",
            "arn:aws-cn:acm:cn-north-1:000000000000:certificate/\
             aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            true,
        ),
        // Neither value names a Certificate Manager resource, so nothing of either service's
        // records says anything about the certificate behind it.
        (
            "an ARN of another service altogether",
            "arn:aws:iam::000000000000:server-certificate/legacy",
            false,
        ),
        ("a value that is no ARN at all", "not-an-arn", false),
    ];

    for (index, (case, arn, refused)) in rows.iter().enumerate() {
        let aliases = vec![format!("vc-acm-{index}.example.com")];
        let reference = format!("viewer-certificate-acm-{index}");
        let answer = create(
            &emulator.router,
            &config_xml(&reference, &aliases, &acm_certificate(arn)),
        )
        .await;
        if *refused {
            assert_refused(case, "CreateDistribution", &aliases, &answer);
        } else {
            assert_eq!(
                answer.status, 201,
                "case {case:?}: expected HTTP 201, got {} with {}",
                answer.status, answer.raw
            );
        }
    }
}

/// RULE: a certificate Certificate Manager HOLDS has to cover every alternate domain name the
/// configuration claims — the same half of the rule the uploaded certificates above are judged
/// by, asked of the other service that holds certificates.
#[tokio::test]
async fn a_held_acm_certificate_has_to_cover_the_alias() {
    let emulator = Emulator::new();
    let signer = authority();
    let zone = "vc-acm-store.example.com";

    // One certificate brought into Certificate Manager, covering the zone itself and one
    // label under it.
    let covered = vec![zone.to_string(), host(zone, "covered")];
    let arn = emulator
        .import(
            "a certificate imported into Certificate Manager",
            &mint(&covered, Some(&signer)),
        )
        .await;

    // (case, the alias claimed, whether the write is refused)
    let rows: &[(&str, &str, bool)] = &[
        ("the alias the certificate's own name covers", "@", false),
        ("an alias the certificate names beside it", "covered", false),
        ("a host of the zone the certificate does not name", "other", true),
    ];
    for (index, (case, label, refused)) in rows.iter().enumerate() {
        let aliases = vec![host(zone, label)];
        let reference = format!("viewer-certificate-acm-store-{index}");
        let answer = create(
            &emulator.router,
            &config_xml(&reference, &aliases, &acm_certificate(&arn)),
        )
        .await;
        if *refused {
            assert_refused(case, "CreateDistribution", &aliases, &answer);
        } else {
            assert_eq!(
                answer.status, 201,
                "case {case:?}: expected HTTP 201, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer
                    .root()
                    .text_at("Distribution/DistributionConfig/ViewerCertificate/ACMCertificateArn")
                    .or_else(|| answer
                        .root()
                        .text_at("DistributionConfig/ViewerCertificate/ACMCertificateArn")),
                Some(arn.as_str()),
                "case {case:?}: expected the stored certificate to be the one imported, got {}",
                answer.raw
            );
        }
    }
}

/// RULE: the same judgement is made on the way IN to a stored distribution, so an update
/// cannot leave one presenting a certificate CloudFront would not serve its alias under — and
/// the refused update changes nothing.
#[tokio::test]
async fn an_update_cannot_attach_a_certificate_that_cannot_serve_the_alias() {
    let emulator = Emulator::new();
    let signer = authority();
    let zone = "vc-update.example.com";
    let aliases = vec![host(zone, "@")];

    let served = emulator
        .upload(
            "the certificate the distribution is created under",
            "viewer-certificate-update-served",
            &mint(&aliases, Some(&signer)),
        )
        .await;
    let created = create(
        &emulator.router,
        &config_xml("viewer-certificate-update", &aliases, &iam_certificate(&served)),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: expected HTTP 201 from CreateDistribution, got {} with {}",
        created.status, created.raw
    );
    let id = created.root().text_at("Id").unwrap_or_default().to_string();
    let etag = created.header("etag").to_string();

    // Each row is a certificate the update tries to move the distribution onto.
    let rows: &[(&str, &[&str], bool)] = &[
        ("a self-signed certificate for the alias", &["@"], true),
        ("a CA-signed certificate for another host", &["other"], false),
    ];
    for (position, (case, labels, self_signed)) in rows.iter().enumerate() {
        let covered: Vec<String> = labels.iter().map(|label| host(zone, label)).collect();
        let certificate_id = emulator
            .upload(
                case,
                &format!("viewer-certificate-update-{position}"),
                &mint(&covered, if *self_signed { None } else { Some(&signer) }),
            )
            .await;
        let refused = update(
            &emulator.router,
            &id,
            &config_xml(
                "viewer-certificate-update",
                &aliases,
                &iam_certificate(&certificate_id),
            ),
            &etag,
        )
        .await;
        assert_refused(case, "UpdateDistribution", &aliases, &refused);

        let read = get_distribution(&emulator.router, &id).await;
        assert_eq!(
            read.root()
                .text_at("DistributionConfig/ViewerCertificate/IAMCertificateId"),
            Some(served.as_str()),
            "case {case:?}: a refused update leaves the stored certificate in place, got {}",
            read.raw
        );
        assert_eq!(
            read.header("etag"),
            etag,
            "case {case:?}: a refused update mints no new version, got {:?}",
            read.header("etag")
        );
    }
}
