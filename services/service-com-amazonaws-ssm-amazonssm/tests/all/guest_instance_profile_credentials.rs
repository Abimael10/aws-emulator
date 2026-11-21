// SPDX-License-Identifier: CC0-1.0

//! The CREDENTIALS the guest of a managed node is served for the role its instance profile
//! carries, read the way an agent reads them — a Run Command invocation that curls the
//! instance metadata service — with Systems Manager, Amazon EC2, IAM and AWS STS on one engine
//! ([`super::ec2_instance_managed_nodes`]).
//!
//! RULE under test: an instance that runs as an instance profile has temporary credentials FOR
//! the role that profile carries published to its guest under that role's name, as the document
//! AWS publishes there (`Code`, `LastUpdated`, `Type`, `AccessKeyId`, `SecretAccessKey`,
//! `Token`, `Expiration`); the credentials are a session of that role named for the instance,
//! they belong to the instance rather than to one read, and they belong to the role it runs as
//! NOW — so replacing the association serves a session of the new role instead.
//!
//! The rows differ along what the rule generalises over: whether the profile's name is the
//! role's name or another name entirely. The role SWAP is its own case, which is the one thing
//! a single row cannot carry.
//!
//! What the credentials are a session OF — the assumed-role identity a call signed with them
//! is made by — and what the metadata service answers for a name that is not the role's are
//! tabled in the EC2 crate's `tests/all/instance_guest_iam_role_credentials.rs`, where the
//! machine's own side of this rule lives. What this suite owns is the delivery: that a command
//! this service sends reaches a guest holding the credentials of its instance's role.

use serde_json::Value;

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, AL2023_NAME_PATTERN,
};
use super::guest_instance_profile_identity::{online, profile_carrying_role, read_in_guest};

/// The `Code` AWS's own credentials document carries for a role the metadata service resolved.
const DOCUMENT_CODE: &str = "Success";

/// The `Type` it carries.
const DOCUMENT_TYPE: &str = "AWS-HMAC";

/// The metadata service's own address, read from the service that publishes it to its guest
/// rather than written here, with the scheme a documented read names it under.
fn metadata() -> String {
    format!(
        "http://{}",
        service_com_amazonaws_ec2_amazonec2::state::metadata_address()
    )
}

/// The script an agent inside the guest runs to obtain the role's credentials: a session token,
/// the role listing, and the credentials published under the name that listing answered.
fn probe() -> Vec<String> {
    vec![
        format!(
            "TOKEN=$(curl -X PUT {}/latest/api/token \
             -H \"X-aws-ec2-metadata-token-ttl-seconds: 21600\")",
            metadata()
        ),
        format!(
            "ROLE=$(curl -s -H \"X-aws-ec2-metadata-token: $TOKEN\" \
             {}/latest/meta-data/iam/security-credentials/)",
            metadata()
        ),
        "echo \"$ROLE\"".to_string(),
        format!(
            "curl -s -H \"X-aws-ec2-metadata-token: $TOKEN\" \
             {}/latest/meta-data/iam/security-credentials/$ROLE",
            metadata()
        ),
    ]
}

/// What one run of [`probe`] read: the role the guest discovered, and the credentials document
/// published under that name.
struct Probed {
    /// The one line `iam/security-credentials/` answered.
    listed_role: String,
    /// The document published under it.
    document: Value,
}

/// Run [`probe`] in the node's guest and read back what it wrote.
async fn probed(emulator: &Emulator, case: &str, instance_id: &str) -> Probed {
    let read = read_in_guest(emulator, case, instance_id, &probe()).await;
    let mut lines = read.lines();
    let listed_role = lines.next().unwrap_or_default().trim().to_string();
    let document = lines.next().unwrap_or_default().trim().to_string();
    let parsed: Value = serde_json::from_str(&document).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: the credentials document must be JSON ({error}): {document:?} \
             (whole read {read:?})"
        )
    });
    Probed {
        listed_role,
        document: parsed,
    }
}

/// RULE: the guest is served the credentials of the role its instance profile carries.
#[tokio::test]
async fn a_guest_is_served_the_credentials_of_the_role_its_instance_profile_carries() {
    // (what the case is, the profile's name, the role's name)
    let rows: &[(&str, &str, &str)] = &[
        (
            "a profile and role named alike, as a client that names both once creates them",
            "imds-credentials-alike",
            "imds-credentials-alike",
        ),
        (
            "a profile whose role is named differently, as a template's generated names are",
            "stack-instance-profile-7A8B9C",
            "stack-instance-role-1D2E3F",
        ),
    ];
    for (case, profile_name, role_name) in rows {
        let emulator = Emulator::new();
        let image = emulator.image(case, AL2023_NAME_PATTERN).await;
        let profile = profile_carrying_role(&emulator, case, profile_name, role_name).await;
        let instance = emulator.launch(case, &image, Some(&profile)).await;
        emulator.launched_ago(&instance, launched_long_enough_ago());
        online(&emulator, case, &instance).await;

        let read = probed(&emulator, case, &instance).await;
        assert_eq!(
            read.listed_role, *role_name,
            "case {case:?}: expected the listing to name the role {role_name:?}, got {:?}",
            read.listed_role
        );
        let document = &read.document;
        assert_eq!(
            document["Code"].as_str(),
            Some(DOCUMENT_CODE),
            "case {case:?}: expected the document's own code, got {document}"
        );
        assert_eq!(
            document["Type"].as_str(),
            Some(DOCUMENT_TYPE),
            "case {case:?}: expected the document's own type, got {document}"
        );
        for member in ["AccessKeyId", "SecretAccessKey", "Token"] {
            let value = document[member].as_str().unwrap_or_default();
            assert!(
                !value.is_empty(),
                "case {case:?}: expected the document to carry {member}, got {document}"
            );
        }
        let issued = document["LastUpdated"].as_str().unwrap_or_default();
        let expires = document["Expiration"].as_str().unwrap_or_default();
        assert!(
            !issued.is_empty() && expires > issued,
            "case {case:?}: expected the credentials to expire after they were minted, got \
             {issued:?} and {expires:?}"
        );

        // They belong to the instance rather than to one read: a second read of the same
        // machine is served the same credential.
        let again = probed(&emulator, case, &instance).await;
        assert_eq!(
            again.document["AccessKeyId"], document["AccessKeyId"],
            "case {case:?}: expected the second read to be served the same credential, got \
             {} after {document}",
            again.document
        );
    }
}

/// RULE: the credentials are the role the instance runs as NOW — replacing the association
/// serves the guest a session of the new role.
#[tokio::test]
async fn swapping_the_instance_profile_changes_the_credentials_the_guest_is_served() {
    let case = "a role swap through ReplaceIamInstanceProfileAssociation";
    let emulator = Emulator::new();
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let first = profile_carrying_role(&emulator, case, "swap-first-profile", "swap-first-role")
        .await;
    let second =
        profile_carrying_role(&emulator, case, "swap-second-profile", "swap-second-role").await;
    let instance = emulator.launch(case, &image, Some(&first)).await;
    emulator.launched_ago(&instance, launched_long_enough_ago());
    online(&emulator, case, &instance).await;

    let before = probed(&emulator, case, &instance).await;
    assert_eq!(
        before.listed_role, "swap-first-role",
        "case {case:?}: expected the first role to be listed, got {:?}",
        before.listed_role
    );

    let associations = emulator
        .ec2(
            case,
            "DescribeIamInstanceProfileAssociations",
            &[
                ("Filter.1.Name", "instance-id"),
                ("Filter.1.Value.1", instance.as_str()),
            ],
        )
        .await;
    let association_id = Emulator::answered(&associations, "associationId").unwrap_or_else(|| {
        panic!("case {case:?}: no association over {instance}: {associations}")
    });
    emulator
        .ec2(
            case,
            "ReplaceIamInstanceProfileAssociation",
            &[
                ("AssociationId", association_id.as_str()),
                ("IamInstanceProfile.Name", second.as_str()),
            ],
        )
        .await;

    let after = probed(&emulator, case, &instance).await;
    assert_eq!(
        after.listed_role, "swap-second-role",
        "case {case:?}: expected the new role to be listed, got {:?}",
        after.listed_role
    );
    assert_ne!(
        after.document["AccessKeyId"], before.document["AccessKeyId"],
        "case {case:?}: expected the swap to change the credential served, got {} against {}",
        after.document, before.document
    );
}
