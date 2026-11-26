// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `GetInstanceTpmEkPub`, driven over the wire the service actually
//! speaks: `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test are:
//!
//! 1. the operation reports the PUBLIC ENDORSEMENT KEY of the instance's NitroTPM: real key
//!    material of the type the request asked for, in the format it asked for — the X.509
//!    `SubjectPublicKeyInfo` of that key for `der`, and the TPM 2.0 `TPMT_PUBLIC` structure of
//!    the same key for `tpmt` — base64 encoded, with the request's key type and format reported
//!    back beside the instance the service resolved;
//! 2. the key belongs to the INSTANCE: two reads of one instance answer the same key, both
//!    formats of one read carry the same key material, and no two instances share one;
//! 3. an instance whose type or AMI offers no NitroTPM holds no endorsement key, so such a
//!    request is refused rather than answered, and so is an id the account holds no instance
//!    under.
//!
//! The rows differ along what the rules generalise over: both published key types, both
//! published formats, and the two ways an instance can lack a NitroTPM — so no row is one
//! recorded request.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code};
use super::wire::param;

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// An instance type the published catalog reports NitroTPM support for.
const TPM_INSTANCE_TYPE: &str = "m5.large";

/// A bare metal type, which has no hypervisor to present the virtual device: the published
/// catalog reports NitroTPM unsupported for one.
const BARE_METAL_INSTANCE_TYPE: &str = "m5.metal";

/// The name prefix of the published NitroTPM-preconfigured Windows AMIs, which is how a client
/// finds one ("TPM-Windows_Server-…", AWS's published Windows AMI names).
const TPM_IMAGE_NAME_PREFIX: &str = "TPM-Windows_Server-";

/// The image id of the published AMI whose name starts with `prefix`, and of one that carries
/// no NitroTPM support at all.
async fn published_images(case: &str, router: &MultiServiceRouter) -> (String, String) {
    let (status, body) = call(
        router,
        REGION,
        "DescribeImages",
        &[param("Owner.1", "amazon")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeImages must succeed, got {status} with body {body}"
    );
    let parsed = super::wire::document(case, &body);
    let mut with_tpm = None;
    let mut without_tpm = None;
    for item in parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
    {
        let Some(image_id) = super::wire::child_text(item, "imageId") else {
            continue;
        };
        let name = super::wire::child_text(item, "name").unwrap_or_default();
        let tpm = super::wire::child_text(item, "tpmSupport");
        if name.starts_with(TPM_IMAGE_NAME_PREFIX) && tpm.is_some() {
            with_tpm = Some(image_id);
        } else if tpm.is_none() && without_tpm.is_none() {
            without_tpm = Some(image_id);
        }
    }
    (
        with_tpm.unwrap_or_else(|| {
            panic!("case {case:?}: the published catalog must hold a {TPM_IMAGE_NAME_PREFIX}* AMI")
        }),
        without_tpm
            .unwrap_or_else(|| panic!("case {case:?}: the catalog must hold an AMI without one")),
    )
}

/// Launch one instance of `instance_type` from `image_id`, answering its id.
async fn launch(
    case: &str,
    router: &MultiServiceRouter,
    image_id: &str,
    instance_type: &str,
) -> String {
    let (status, body) = call(
        router,
        REGION,
        "RunInstances",
        &[
            param("ImageId", image_id),
            param("InstanceType", instance_type),
            param("MinCount", "1"),
            param("MaxCount", "1"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RunInstances must succeed, got {status} with body {body}"
    );
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: no instanceId in {body}"))
}

/// The `KeyValue` `GetInstanceTpmEkPub` reports for `instance_id`, decoded from base64 —
/// having pinned that the call succeeded and that it reports back the instance, the key type
/// and the format.
async fn endorsement_key(
    case: &str,
    router: &MultiServiceRouter,
    instance_id: &str,
    key_type: &str,
    key_format: &str,
) -> Vec<u8> {
    let (status, body) = call(
        router,
        REGION,
        "GetInstanceTpmEkPub",
        &[
            param("InstanceId", instance_id),
            param("KeyType", key_type),
            param("KeyFormat", key_format),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    assert_eq!(
        element(&body, "instanceId").as_deref(),
        Some(instance_id),
        "case {case:?}: the response names the instance the service resolved, got {body}"
    );
    assert_eq!(
        element(&body, "keyType").as_deref(),
        Some(key_type),
        "case {case:?}: the response reports the key type asked for, got {body}"
    );
    assert_eq!(
        element(&body, "keyFormat").as_deref(),
        Some(key_format),
        "case {case:?}: the response reports the format asked for, got {body}"
    );
    let encoded = element(&body, "keyValue")
        .unwrap_or_else(|| panic!("case {case:?}: no keyValue in {body}"));
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &encoded)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the key material must be base64 ({error}): {encoded}")
        })
}

/// RULE 1 and RULE 2: each published key type is reported in each published format, the
/// formats carry the same key, and the key is the instance's own.
#[tokio::test]
async fn each_key_type_is_reported_in_each_published_format() {
    let case = "a NitroTPM instance";
    let (router, _engine) = super::ec2_query::emulator();
    let (tpm_image, _plain_image) = published_images(case, &router).await;
    let first = launch(case, &router, &tpm_image, TPM_INSTANCE_TYPE).await;
    let second = launch(case, &router, &tpm_image, TPM_INSTANCE_TYPE).await;

    // (the key type, the algorithm id its TPMT_PUBLIC opens with, the OID its DER names)
    let cases: [(&str, [u8; 2], &str); 2] = [
        // The algorithm ids are the TPM 2.0 `TPM_ALG_ID` values of an RSA and an ECC key
        // (TPM 2.0 Library Part 2): `TPM_ALG_RSA` and `TPM_ALG_ECC`.
        (ec2::state::EK_PUB_KEY_TYPE_RSA_2048, [0x00, 0x01], "1.2.840.113549.1.1.1"),
        (ec2::state::EK_PUB_KEY_TYPE_ECC_SEC_P384, [0x00, 0x23], "1.2.840.10045.2.1"),
    ];

    for (key_type, tpm_algorithm, _oid) in cases {
        let der = endorsement_key(
            case,
            &router,
            &first,
            key_type,
            ec2::state::EK_PUB_KEY_FORMAT_DER,
        )
        .await;
        // A `SubjectPublicKeyInfo` is a DER SEQUENCE, and the one this operation reports
        // parses as a public key of the type that was asked for.
        assert_eq!(
            der.first(),
            Some(&0x30),
            "case {case:?} / {key_type}: the der format is a DER SEQUENCE, got {der:?}"
        );
        assert!(
            der.len() > 64,
            "case {case:?} / {key_type}: a public key is more than a header, got {} bytes",
            der.len()
        );

        let tpmt = endorsement_key(
            case,
            &router,
            &first,
            key_type,
            ec2::state::EK_PUB_KEY_FORMAT_TPMT,
        )
        .await;
        assert_eq!(
            tpmt.get(..2),
            Some(&tpm_algorithm[..]),
            "case {case:?} / {key_type}: the tpmt format opens with the key's TPM algorithm \
             id, got {tpmt:?}"
        );
        // `nameAlg` is `TPM_ALG_SHA256` and the `authPolicy` that follows the object
        // attributes is the 32-byte digest the published endorsement key template carries.
        assert_eq!(
            tpmt.get(2..4),
            Some(&[0x00, 0x0B][..]),
            "case {case:?} / {key_type}: the tpmt format names SHA-256 as its nameAlg, got \
             {tpmt:?}"
        );
        assert_eq!(
            tpmt.get(8..10),
            Some(&[0x00, 0x20][..]),
            "case {case:?} / {key_type}: the authPolicy is a 32-byte digest, got {tpmt:?}"
        );

        // The same instance answers the same key on a second read, in both formats.
        let again = endorsement_key(
            case,
            &router,
            &first,
            key_type,
            ec2::state::EK_PUB_KEY_FORMAT_DER,
        )
        .await;
        assert_eq!(
            again, der,
            "case {case:?} / {key_type}: an instance's endorsement key never changes"
        );

        // And another instance has a key of its own.
        let other = endorsement_key(
            case,
            &router,
            &second,
            key_type,
            ec2::state::EK_PUB_KEY_FORMAT_DER,
        )
        .await;
        assert_ne!(
            other, der,
            "case {case:?} / {key_type}: two instances must not share one endorsement key"
        );
    }
}

/// RULE 3: an instance with no NitroTPM, and an id the account holds no instance under, are
/// refused rather than answered with key material.
///
/// The rows are the two ways an instance can lack the device — an AMI that is not configured
/// for NitroTPM support, and an instance type that offers none — and the unresolvable id.
#[tokio::test]
async fn an_instance_without_a_nitro_tpm_is_refused() {
    let case = "no NitroTPM";
    let (router, _engine) = super::ec2_query::emulator();
    let (tpm_image, plain_image) = published_images(case, &router).await;
    let without_support = launch(case, &router, &plain_image, TPM_INSTANCE_TYPE).await;
    let bare_metal = launch(case, &router, &tpm_image, BARE_METAL_INSTANCE_TYPE).await;

    let cases: [(&str, &str, &str); 3] = [
        (
            "an AMI that is not configured for NitroTPM support",
            without_support.as_str(),
            ec2::state::INVALID_PARAMETER_VALUE,
        ),
        (
            "an instance type that offers no NitroTPM",
            bare_metal.as_str(),
            ec2::state::INVALID_PARAMETER_VALUE,
        ),
        (
            "an id the account holds no instance under",
            "i-0123456789abcdef0",
            "InvalidInstanceID.NotFound",
        ),
    ];

    for (row, instance_id, expected_code) in cases {
        let (status, body) = call(
            &router,
            REGION,
            "GetInstanceTpmEkPub",
            &[
                param("InstanceId", instance_id),
                param("KeyType", ec2::state::EK_PUB_KEY_TYPE_RSA_2048),
                param("KeyFormat", ec2::state::EK_PUB_KEY_FORMAT_DER),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {row:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {row:?}: expected {expected_code}, got {body}"
        );
        assert!(
            element(&body, "keyValue").is_none(),
            "case {row:?}: a refusal reports no key material, got {body}"
        );
    }
}

/// RULE 1: a key type or format outside the published catalogs is a parameter value the
/// service cannot use, which EC2 answers its own `InvalidParameterValue` for rather than as a
/// validation clause.
#[tokio::test]
async fn a_key_type_or_format_outside_the_catalog_is_refused_with_the_ec2_code() {
    let case = "an unpublished key type or format";
    let (router, _engine) = super::ec2_query::emulator();
    let (tpm_image, _plain) = published_images(case, &router).await;
    let instance_id = launch(case, &router, &tpm_image, TPM_INSTANCE_TYPE).await;

    // (case, the key type sent, the format sent)
    let cases: [(&str, &str, &str); 3] = [
        ("a key type nothing publishes", "rsa-4096", "der"),
        ("a format nothing publishes", "rsa-2048", "pem"),
        ("neither published", "dsa-1024", "openssh"),
    ];
    for (row, key_type, key_format) in cases {
        let (status, body) = call(
            &router,
            REGION,
            "GetInstanceTpmEkPub",
            &[
                param("InstanceId", &instance_id),
                param("KeyType", key_type),
                param("KeyFormat", key_format),
            ],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {row:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            ec2::state::INVALID_PARAMETER_VALUE,
            "case {row:?}: expected {}, got {body}",
            ec2::state::INVALID_PARAMETER_VALUE
        );
        assert_eq!(
            super::ec2_query::error_message(&body),
            ec2::state::EK_PUB_KEY_REFUSAL_MESSAGE,
            "case {row:?}: unexpected sentence, got {body}"
        );
    }
}
