// SPDX-License-Identifier: CC0-1.0

//! The SERVER-SIDE ENCRYPTION this service's own Amazon S3 writes are made under: the customer
//! managed key the output configuration of the work declared, applied to every object that work
//! leaves behind.
//!
//! A SageMaker output configuration carries a `KmsKeyId` — `ProcessingOutputConfig$KmsKeyId`
//! ("the Amazon Web Services Key Management Service (Amazon Web Services KMS) key that Amazon
//! SageMaker uses to encrypt the processing job output … The `KmsKeyId` is applied to all
//! outputs"), `OutputDataConfig$KmsKeyId` and `TransformOutput$KmsKeyId` ("the … KMS key that
//! SageMaker uses to encrypt the model artifacts at rest using Amazon S3 server-side
//! encryption"), `AutoMLOutputDataConfig$KmsKeyId` and `DataCaptureConfig$KmsKeyId` ("the …
//! key that SageMaker AI uses to encrypt the captured data at rest using Amazon S3 server-side
//! encryption"). So the WRITE that materialises such an output is an SSE-KMS write under that
//! key, and a reader of the object — `HeadObject`, `GetObject` — is answered `aws:kms` and the
//! key it names; an output configuration that declares no key is written under whatever
//! encryption the destination bucket applies by default ("if you don't provide a KMS key ID,
//! SageMaker uses the default KMS key for Amazon S3 for your role's account",
//! `OutputDataConfig$KmsKeyId`), which is Amazon S3's own question and not this service's.
//!
//! This is the ONE place that reading is made, so no two kinds of output can disagree about
//! what the key a request declared protects: every one of this service's writes goes through
//! [`super::write_object`], which applies it here.

/// Apply the server-side encryption `declared_kms_key_id` asks for to one `PutObject`.
///
/// A key the configuration declared makes this an SSE-KMS write under that key, whichever
/// spelling the reference took — `KmsKeyId` "can be an ID of a KMS key, ARN of a KMS key, or
/// alias of a KMS key" (`ProcessingOutputConfig$KmsKeyId`), each of which Amazon S3 resolves
/// itself, so the reference travels as the request wrote it.
///
/// A configuration that declared NO key, or that declared an empty one — which is how this
/// service reports an output configuration that named none (`OutputDataConfig$KmsKeyId` is
/// modelled with a zero minimum length) — asks for no encryption of its own: the write names
/// no algorithm, so the destination bucket's own default applies.
#[must_use]
pub fn with_output_encryption(
    write: ::aws_sdk_s3::operation::put_object::builders::PutObjectFluentBuilder,
    declared_kms_key_id: Option<&str>,
) -> ::aws_sdk_s3::operation::put_object::builders::PutObjectFluentBuilder {
    match declared_output_key(declared_kms_key_id) {
        Some(key) => write
            .server_side_encryption(::aws_sdk_s3::types::ServerSideEncryption::AwsKms)
            .ssekms_key_id(key),
        None => write,
    }
}

/// The key an output configuration DECLARED, and nothing for one that declared none.
///
/// A `KmsKeyId` of no characters names no key: the member's modelled length starts at zero and
/// this service reports an output configuration that named no key with an empty one, so the
/// absent and the empty reference are the same declaration — neither selects a customer managed
/// key.
#[must_use]
pub fn declared_output_key(declared_kms_key_id: Option<&str>) -> Option<&str> {
    declared_kms_key_id
        .map(str::trim)
        .filter(|reference| !reference.is_empty())
}

#[cfg(test)]
mod tests {
    use super::declared_output_key;

    /// RULE: an output configuration declares a customer managed key exactly when its
    /// `KmsKeyId` carries a reference, and the reference travels as the request wrote it.
    ///
    /// The rows differ along the form the member took: absent, empty, blank, a key id, a key
    /// ARN, an alias name and an alias ARN — the four spellings `ProcessingOutputConfig$KmsKeyId`
    /// and `OutputDataConfig$KmsKeyId` document.
    #[test]
    fn a_declared_output_key_is_the_reference_the_configuration_carries() {
        // (case, the member as the request carried it, the key the write is made under)
        let rows: [(&str, Option<&str>, Option<&str>); 7] = [
            ("no member at all", None, None),
            ("an empty member", Some(""), None),
            ("a blank member", Some("   "), None),
            (
                "a key id",
                Some("1234abcd-12ab-34cd-56ef-1234567890ab"),
                Some("1234abcd-12ab-34cd-56ef-1234567890ab"),
            ),
            (
                "a key ARN",
                Some("arn:aws:kms:us-west-2:111122223333:key/1234abcd-12ab-34cd-56ef-1234567890ab"),
                Some("arn:aws:kms:us-west-2:111122223333:key/1234abcd-12ab-34cd-56ef-1234567890ab"),
            ),
            (
                "an alias name",
                Some("alias/ExampleAlias"),
                Some("alias/ExampleAlias"),
            ),
            (
                "an alias ARN",
                Some("arn:aws:kms:us-west-2:111122223333:alias/ExampleAlias"),
                Some("arn:aws:kms:us-west-2:111122223333:alias/ExampleAlias"),
            ),
        ];

        for (case, declared, expected) in rows {
            let resolved = declared_output_key(declared);
            assert_eq!(
                resolved, expected,
                "case {case:?}: expected the write made under {expected:?}, got {resolved:?}"
            );
        }
    }
}
