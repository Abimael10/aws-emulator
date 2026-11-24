// SPDX-License-Identifier: CC0-1.0

//! What the code-signing configuration a function is attached to does to a DEPLOYMENT.
//!
//! A code-signing configuration is a deployment CONTROL, and its policy says what happens
//! to a deployment whose signature checks fail:
//! `CodeSigningPolicies$UntrustedArtifactOnDeployment` is "Code signing configuration
//! policy for deployment validation failure. If you set the policy to `Enforce`, Lambda
//! blocks the deployment request if signature validation checks fail. If you set the policy
//! to `Warn`, Lambda allows the deployment and issues a new Amazon CloudWatch metric
//! (`SignatureValidationErrors`) and also stores the warning in the CloudTrail log."
//!
//! What the checks are AGAINST is the configuration's `AllowedPublishers`: "The Amazon
//! Resource Name (ARN) for each of the signing profiles. A signing profile defines a
//! trusted user who can sign a code package." So a deployment passes only when the artifact
//! it carries bears a signature attesting to one of those signing profile versions.
//!
//! Every operation that DEPLOYS to a function holding an association puts that deployment to
//! [`untrusted_deployment_refusal`] before anything is stored, and answers the modelled
//! `CodeVerificationFailedException` (400, "The code signature failed one or more of the
//! validation checks for signature mismatch or expiry, and the code signing policy is set to
//! ENFORCE. Lambda blocks the deployment.") with the message it hands back: `CreateFunction`
//! and `UpdateFunctionCode` for the function's own deployment package, and
//! `UpdateFunctionConfiguration` for the layer packages a configuration change mounts into
//! the function's execution environment ([`DeployedArtifact`]). Those three are exactly the
//! operations the model binds that error to. The policy is therefore read in ONE place, and
//! a configuration whose policy is `Warn` lets the same deployment land.
//!
//! WHAT AN ARTIFACT ATTESTS TO is carried by the artifact itself: the signing service writes
//! the signature into the .zip package it signs, so the package a deployment hands over is
//! what names the signing profile version it was signed under, and no member of any of these
//! operations names a publisher. That signature is read and VERIFIED in one place
//! ([`super::package_signature`]) — against the key pair the signing service holds for that
//! profile version, so an envelope anyone can write attributes nothing on its own — and a
//! function reports the publisher of the package it is serving as
//! `SigningProfileVersionArn`, beside the `SigningJobArn` of the job that signed it.
//! A package bearing no signature this emulator can verify is attributed to no publisher,
//! and is the deployment an `Enforce` policy blocks.

use super::*;

/// The `CodeSigningPolicy` value that BLOCKS a deployment whose signature checks failed:
/// "If you set the policy to `Enforce`, Lambda blocks the deployment request if signature
/// validation checks fail."
///
/// The enum's other value, `Warn`, is the documented default and the one that lets such a
/// deployment land ([`DEFAULT_CODE_SIGNING_POLICY`]).
pub const CODE_SIGNING_POLICY_ENFORCE: &str = "Enforce";

/// The refusal Lambda words for a deployment its code-signing checks rejected under an
/// `Enforce` policy: two fixed sentences, and the function the deployment addressed as the
/// variable part of the third.
///
/// `resource` is the function the deployment is of, as the operation making it addresses
/// one: the ARN of the function an update changes, and the name a create was given for a
/// function that does not exist yet.
#[must_use]
pub fn code_verification_failed_message(resource: &str) -> String {
    format!(
        "Lambda cannot deploy the function. The function or layer might be signed using a \
         signature that the client is not configured to accept. Check the provided signature \
         for {resource}."
    )
}

/// WHAT a deployment puts on a function: the artifact that bears the signature Lambda's
/// code-signing checks are made against.
#[derive(Debug, Clone, Copy)]
pub enum DeployedArtifact<'a> {
    /// The function's own deployment package, as the create or the code update resolved it,
    /// with the publisher its own signature was verified as coming from.
    ///
    /// The signature is read and verified where the package is resolved, which is before the
    /// store is reached ([`super::verified_package_signature`]) — the same attribution the
    /// stored configuration then reports as `SigningProfileVersionArn`, so what admitted the
    /// deployment and what the function reports for it cannot differ.
    Package(&'a CodeRecord, Option<&'a DeploymentSignature>),
    /// The layer versions a configuration change mounts: "A list of function layers to add
    /// to the function's execution environment" — each its own package, signed the same way
    /// and verified alongside the function's, which is what the refusal means by "The
    /// function or layer might be signed using a signature that the client is not
    /// configured to accept."
    ///
    /// The versions carried here are the ones the request RESOLVED to
    /// ([`super::resolve_function_layers`]), because each of them is the package whose
    /// signature is verified and whose own ARN the refusal names
    /// ([`DeployedArtifact::untrusted_subject`]). Each carries the publisher its own archive
    /// was verified as coming from, settled when that version was published
    /// ([`FunctionLayer::signing_profile_version_arn`]).
    Layers(&'a [FunctionLayer]),
}

impl<'a> DeployedArtifact<'a> {
    /// Whether code signing VERIFIES this artifact at all.
    ///
    /// A container image is outside the control: the signature a check is made against is
    /// borne by a .zip package, while an image is a reference into a registry that pins the
    /// function to the manifest digest that registry minted for it
    /// ([`super::image_source`]), so there is no package signature to verify and nothing is
    /// refused for one.
    #[must_use]
    fn is_verified(&self) -> bool {
        match self {
            Self::Package(code, _) => !code.is_image(),
            Self::Layers(_) => true,
        }
    }

    /// WHICH artifact of this deployment `config` counts as coming from no trusted
    /// publisher, named as a refusal names it, or `None` when every one of them does.
    ///
    /// A deployment of the function's own package is named by the function it deploys to,
    /// as the operation making it addresses one (`resource`) — the ARN of the function an
    /// update changes, the name a create was given for a function that does not exist yet.
    /// A LAYER attach deploys the layer version's own package instead: "A list of function
    /// layers to add to the function's execution environment", each a package of its own
    /// signed on its own, so each is verified on its own and the artifact in question is the
    /// FIRST one whose signature no allowed publisher made — which is the one the refusal
    /// names, by its version ARN. A change mounting nothing deploys nothing and falls back
    /// to the function it addressed.
    #[must_use]
    fn untrusted_subject(
        self,
        config: &CodeSigningConfigRecord,
        resource: &'a str,
    ) -> Option<String> {
        match self {
            Self::Package(_, signature) => (!allowed_publisher_signed(
                config,
                signature.map(|signature| signature.profile_version_arn.as_str()),
            ))
            .then(|| resource.to_string()),
            Self::Layers(layers) => layers
                .iter()
                .find(|layer| {
                    !allowed_publisher_signed(
                        config,
                        layer.signing_profile_version_arn.as_deref(),
                    )
                })
                .map(|layer| layer.arn.clone()),
        }
    }
}

/// Whether `config` counts a package signed by the publisher `signed_by` as coming from a
/// TRUSTED one: whether that publisher is one of the signing profile versions the
/// configuration allows.
///
/// `AllowedPublishers$SigningProfileVersionArns` is "The Amazon Resource Name (ARN) for each
/// of the signing profiles. A signing profile defines a trusted user who can sign a code
/// package" — the ARN of a signing profile VERSION, which is what a signature attests to — so
/// an entry covers the one version it names and nothing else, whatever it shares with another
/// ARN. `None` is a package attributed to nobody — one bearing no signature this emulator
/// could verify ([`super::verified_package_signature`]) — which no entry can cover.
#[must_use]
fn allowed_publisher_signed(
    config: &CodeSigningConfigRecord,
    signed_by: Option<&str>,
) -> bool {
    signed_by.is_some_and(|signed_by| {
        config
            .signing_profile_version_arns
            .iter()
            .any(|allowed| allowed == signed_by)
    })
}

/// The refusal the code-signing configuration `config` owes the deployment of `artifact` to
/// the function `resource` names, or `None` when the deployment may land.
///
/// The one reading of the policy, made by every operation that deploys to a function the
/// configuration is attached to:
///
/// * an artifact whose own signature verifies under one of the configuration's allowed
///   publishers passes every check there is to make and lands;
/// * an artifact that does not, under [`CODE_SIGNING_POLICY_ENFORCE`], is the deployment
///   Lambda BLOCKS — the caller gets [`code_verification_failed_message`] naming the
///   artifact whose signature was checked ([`DeployedArtifact::untrusted_subject`]) and the
///   store is left as it was;
/// * an artifact that does not, under any other policy — `Warn`, the documented default —
///   lands, because that policy "allows the deployment".
///
/// Every artifact arrives with the publisher its own signature was verified as coming from
/// ([`super::verified_package_signature`]), read before this service's store was reached: the
/// function's own package by the call deploying it, and a layer version's archive by the
/// publish that stored it. So what is decided here is WHO signed, never whether a signature
/// verifies.
#[must_use]
pub fn untrusted_deployment_refusal(
    config: &CodeSigningConfigRecord,
    artifact: DeployedArtifact<'_>,
    resource: &str,
) -> Option<String> {
    if !artifact.is_verified() {
        return None;
    }
    let untrusted = artifact.untrusted_subject(config, resource)?;
    if config.untrusted_artifact_on_deployment != CODE_SIGNING_POLICY_ENFORCE {
        return None;
    }
    Some(code_verification_failed_message(&untrusted))
}

/// Lambda's sentence for a request that asks for code signing on a function whose
/// deployment package is a CONTAINER IMAGE.
///
/// "You can use code signing if your deployment package is a .zip file archive" (the
/// model's own `CreateFunction` documentation): the signature the checks here are made
/// against is borne by a .zip package, while an image is a reference into a registry that
/// the function is pinned to the manifest digest of ([`super::image_source`]) — so there is
/// no package signature to verify and the combination names a control Lambda cannot apply.
/// It is the one fixed sentence every operation that would form that pairing answers, as a
/// `InvalidParameterValueException` (400): the create that asks for both at once, and the
/// association made against a function already deployed from an image.
pub const CODE_SIGNING_UNSUPPORTED_FOR_IMAGE_MESSAGE: &str =
    "Code signing is not supported for functions created with container images.";

impl ComAmazonawsLambdaAwsgirapiserviceState {
    /// The code-signing configuration the function `function_name` is attached to, when it
    /// holds an association and this store still holds the configuration it names.
    ///
    /// One function has ONE association ([`FunctionRecord::code_signing_config_arn`]),
    /// however it was set, so every deployment to that function is verified against the
    /// single configuration found here.
    #[must_use]
    pub fn attached_code_signing_config(
        &self,
        function_name: &str,
    ) -> Option<&CodeSigningConfigRecord> {
        let arn = self
            .functions
            .get(function_name)?
            .code_signing_config_arn
            .as_deref()?;
        self.code_signing_configs.get(arn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the layer-attach table: the case, the layer versions the change mounts,
    /// and the artifact the refusal must name (`None` for a change every allowed publisher
    /// signed).
    type AttachRow<'a> = (&'a str, Vec<FunctionLayer>, Option<String>);

    /// One row of the allowed-publisher table: the case, the entry the configuration carries,
    /// the publisher a package was signed by, and whether the entry covers it.
    type PublisherRow<'a> = (&'a str, String, Option<String>, bool);

    /// One stored configuration, under the policy `policy`, allowing one publisher.
    fn config(policy: &str) -> CodeSigningConfigRecord {
        CodeSigningConfigRecord {
            id: "csc-0123456789abcdefg".to_string(),
            arn: "arn:aws:lambda:us-east-1:000000000000:code-signing-config:csc-0123456789abcdefg"
                .to_string(),
            description: None,
            signing_profile_version_arns: vec![
                "arn:aws:signer:us-east-1:000000000000:/signing-profiles/Profile1/00000001"
                    .to_string(),
            ],
            untrusted_artifact_on_deployment: policy.to_string(),
            last_modified: "2026-01-01T00:00:00.000+0000".to_string(),
            tags: BTreeMap::new(),
        }
    }

    /// One resolved layer version, as `resolve_function_layers` hands one over — signed by
    /// `signed_by`, as the publish that stored it verified its archive's own signature.
    fn layer(version: u32, signed_by: Option<&str>) -> FunctionLayer {
        FunctionLayer {
            arn: format!("arn:aws:lambda:us-east-1:000000000000:layer:shared:{version}"),
            code_size: 64,
            uncompressed_code_size: 128,
            zip_bytes: None,
            signing_profile_version_arn: signed_by.map(str::to_string),
        }
    }

    /// The publisher the configuration above allows.
    const ALLOWED_PUBLISHER: &str =
        "arn:aws:signer:us-east-1:000000000000:/signing-profiles/Profile1/00000001";

    /// RULE: the refusal names the ARTIFACT whose signature was checked, so a LAYER ATTACH
    /// is named by the layer version it mounts rather than by the function it mounts it into
    /// — and a change mounting several is decided on the FIRST version no allowed publisher
    /// signed, whichever position it is in.
    #[test]
    fn a_blocked_layer_attach_names_the_layer_version_it_mounts() {
        let function = "arn:aws:lambda:us-east-1:000000000000:function:probe";
        let unsigned = [layer(1, None), layer(2, None), layer(3, None)];
        let signed = layer(4, Some(ALLOWED_PUBLISHER));
        let elsewhere = layer(5, Some(
            "arn:aws:signer:us-east-1:000000000000:/signing-profiles/Other/00000001",
        ));
        // (case, the layers the change mounts, the artifact the sentence must name, or
        // `None` for a change every allowed publisher signed)
        let rows: Vec<AttachRow<'_>> = vec![
            (
                "one unsigned layer version",
                vec![unsigned[0].clone()],
                Some(unsigned[0].arn.clone()),
            ),
            (
                "every layer a change may mount, none of them signed",
                unsigned.to_vec(),
                Some(unsigned[0].arn.clone()),
            ),
            (
                "a signed version before an unsigned one",
                vec![signed.clone(), unsigned[1].clone()],
                Some(unsigned[1].arn.clone()),
            ),
            (
                "a version signed by a publisher the configuration does not allow",
                vec![signed.clone(), elsewhere.clone()],
                Some(elsewhere.arn.clone()),
            ),
            (
                "every version signed by the allowed publisher",
                vec![signed.clone(), layer(6, Some(ALLOWED_PUBLISHER))],
                None,
            ),
            // A change that mounts nothing deploys nothing, so there is no artifact whose
            // signature could fail a check and nothing to refuse.
            ("no layer at all", Vec::new(), None),
        ];
        for (case, layers, named) in rows {
            let stated = untrusted_deployment_refusal(
                &config(CODE_SIGNING_POLICY_ENFORCE),
                DeployedArtifact::Layers(&layers),
                function,
            );
            let expected = named.as_deref().map(code_verification_failed_message);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected the refusal {expected:?}, got {stated:?}"
            );
        }
    }

    /// RULE: the `Warn` policy "allows the deployment", so the same attach lands under it —
    /// the artifact named above is the Enforce path's, not every path's.
    #[test]
    fn the_same_attach_lands_under_the_admitting_policy() {
        let stated = untrusted_deployment_refusal(
            &config("Warn"),
            DeployedArtifact::Layers(&[layer(1, None)]),
            "arn:aws:lambda:us-east-1:000000000000:function:probe",
        );
        assert_eq!(
            stated, None,
            "expected the Warn policy to admit the attach, got {stated:?}"
        );
    }

    /// RULE: an allowed publisher entry covers the one signing profile VERSION it names and
    /// nothing else, whatever that version shares with the entry.
    #[test]
    fn an_allowed_publisher_covers_the_profile_version_it_names() {
        const PROFILE: &str = "arn:aws:signer:us-east-1:000000000000:/signing-profiles/Release";
        // (case, the allowed entry, the profile version a signature was made under, whether
        // the entry covers it)
        let rows: Vec<PublisherRow<'_>> = vec![
            (
                "the very version the entry names",
                format!("{PROFILE}/00000001"),
                Some(format!("{PROFILE}/00000001")),
                true,
            ),
            (
                "another version of the profile the entry names a version of",
                format!("{PROFILE}/00000001"),
                Some(format!("{PROFILE}/00000002")),
                false,
            ),
            (
                "the profile itself, naming no version",
                PROFILE.to_string(),
                Some(format!("{PROFILE}/00000002")),
                false,
            ),
            (
                "another profile of the same account",
                format!("{PROFILE}Candidate/00000001"),
                Some(format!("{PROFILE}/00000001")),
                false,
            ),
            (
                "the same profile and version in another region",
                format!("{PROFILE}/00000001").replace("us-east-1", "eu-west-1"),
                Some(format!("{PROFILE}/00000001")),
                false,
            ),
            (
                "a package no publisher's signature covers",
                format!("{PROFILE}/00000001"),
                None,
                false,
            ),
        ];
        for (case, allowed, signed_under, covered) in rows {
            let mut record = config(CODE_SIGNING_POLICY_ENFORCE);
            record.signing_profile_version_arns = vec![allowed.clone()];
            let decided = allowed_publisher_signed(&record, signed_under.as_deref());
            assert_eq!(
                decided, covered,
                "case {case:?}: expected {allowed:?} to {} {signed_under:?}, got {decided}",
                if covered { "cover" } else { "not cover" }
            );
        }
    }
}
