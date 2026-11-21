// SPDX-License-Identifier: CC0-1.0

//! The INSTANCE METADATA SERVICE of one instance's guest, and the programs of that guest
//! which read it.
//!
//! # Why a guest needs one
//!
//! An instance's own facts are published TO its guest, at a link-local address only that
//! guest can reach, and a program inside the guest is how they are read: "this document can
//! be found by running the following command from the instance", followed by a `curl` of
//! that address, is how Amazon ECS's `RegisterContainerInstance` documents obtaining the
//! identity document an agent registers with, and the same address publishes the instance's
//! id, its image, its type, its addresses and the zone it is in. A guest with no metadata
//! service has no way to answer "which machine am I", so every agent inside it — the ECS
//! container agent, cloud-init's own scripts, anything a Run Command sends — is stuck.
//!
//! # What is real here
//!
//! Every value is read off the STORED instance ([`GuestMachine`], the one reading of what a
//! guest knows about its machine): the id, the image, the type, the architecture, the
//! addresses, the zone and the Region of that zone, the instant it was launched, the
//! metadata options its launch chose, and the IAM instance profile it runs as — the profile
//! this store holds for it, the name of the role that profile carries and the CREDENTIALS of
//! that role, which are the facts of the machine another service owns
//! ([`GuestMachine::running_as`]). Nothing is echoed from a request, and nothing is answered
//! for a fact the record does not carry — a path whose value the instance has none of is a
//! `404` rather than an invented value, which is why an instance running as no profile
//! publishes no `iam/` category at all. The credentials published UNDER a role's name are a
//! SESSION of that role minted by the token service ([`super::instance_role_credentials`]),
//! handed to the machine by the caller that reaches its guest: a credential is minted by the
//! credential store, never by a reader, so a machine nothing minted one for publishes none.
//!
//! The identity document is SIGNED: [`identity_document_signature`] is the RSASSA-PKCS1-v1_5
//! signature of the document's own bytes under a 2048-bit key this process generates once
//! (the shared signer in `service_common::jws_signing`, which is also what this workspace's
//! emulated OIDC issuers sign with), base64 as the metadata service publishes it. So the
//! signature really is a signature OF that document — two instances' documents carry
//! different ones, and a document that changes changes it — rather than a constant of the
//! right shape.
//!
//! # No network, no host
//!
//! The client below is a PROGRAM OF THE GUEST, performed by the same bounded, in-process
//! shell session the guest's own boot script runs in
//! ([`super::guest_console`]): it opens no socket, resolves no name and reads no host path.
//! NOTHING HERE DIALS ANYTHING — the emulator SERVES this address to its own emulated guest.
//! The wire itself — the address, the session token, the client's options and the statuses it
//! exits with — is the one every node of this emulator publishes
//! ([`::service_common::instance_metadata_service`]), so this module states only what THIS
//! machine publishes: the categories below, read off the stored instance.

use ::std::sync::OnceLock;

use ::service_common::instance_metadata_service::{
    metadata_client, PublishedInstanceMetadata, METADATA_PREFIX,
};
use ::service_common::jws_signing::{SigningKey, RS256};
use ::service_common::posix_shell_session::EnvironmentPrograms;

use super::guest_console::{guest_mac_address, GuestInstanceProfile, GuestMachine};
use super::{iso8601_utc, region_of_zone_name};

pub use ::service_common::instance_metadata_service::{
    metadata_address, METADATA_ADDRESS_OCTETS, METADATA_CLIENT, METADATA_TOKEN_HEADER,
    METADATA_TOKEN_PATH, METADATA_TOKEN_TTL_HEADER,
};

/// The path the instance profile the machine runs as is published at ("information about the
/// IAM instance profile associated with the instance", Amazon EC2 User Guide, "Instance
/// metadata categories").
pub const IAM_INFO_PATH: &str = "/latest/meta-data/iam/info";

/// The path the role whose credentials the guest's agents use is listed at: the category is a
/// LISTING, whose one entry is the name of that role, and the credentials themselves are
/// published under it, one path per role.
pub const IAM_SECURITY_CREDENTIALS_PATH: &str = "/latest/meta-data/iam/security-credentials";

/// `Code` of the `iam/info` document for an instance whose profile the metadata service
/// resolved: the word AWS's own document carries.
const IAM_INFO_SUCCESS: &str = "Success";

/// The path the signed identity document of the instance is published at
/// (`RegisterContainerInstanceRequest$instanceIdentityDocument`).
pub const IDENTITY_DOCUMENT_PATH: &str = "/latest/dynamic/instance-identity/document";

/// The path its signature is published at
/// (`RegisterContainerInstanceRequest$instanceIdentityDocumentSignature`).
pub const IDENTITY_SIGNATURE_PATH: &str = "/latest/dynamic/instance-identity/signature";

/// The prefix of the paths that publish this instance's identity.
const IDENTITY_PREFIX: &str = "/latest/dynamic/instance-identity";

/// `version` of the identity document this metadata service publishes: the schema version
/// AWS's own documents carry.
pub const IDENTITY_DOCUMENT_VERSION: &str = "2017-09-30";

/// What one instance's GUEST publishes at the metadata address: the machine's own facts, read
/// off the stored instance.
///
/// Holds the machine and nothing else — no store guard, no engine and no host handle is
/// reachable from a program.
#[derive(Debug)]
pub struct GuestPrograms {
    /// The machine whose metadata these programs publish.
    machine: GuestMachine,
}

/// The programs the guest of `machine` has installed in it, for a session that runs a
/// program of that guest.
///
/// The ONE construction of them, so the boot's own user-data program and a command something
/// later sends the guest read the same machine through the same client.
#[must_use]
pub fn guest_programs(machine: &GuestMachine) -> Box<dyn EnvironmentPrograms> {
    metadata_client(Box::new(GuestPrograms {
        machine: machine.clone(),
    }))
}

impl PublishedInstanceMetadata for GuestPrograms {
    /// "Indicates whether the HTTP metadata endpoint on your instances is enabled or
    /// disabled" (`InstanceMetadataOptionsResponse$HttpEndpoint`): nothing answers at the
    /// address on an instance whose launch turned it off.
    fn endpoint_enabled(&self) -> bool {
        self.machine.metadata_endpoint_enabled
    }

    /// "If the state is `required`, you must send a session token with any instance metadata
    /// retrieval requests" (`InstanceMetadataOptionsResponse$HttpTokens`).
    fn token_required(&self) -> bool {
        self.machine.metadata_token_required
    }

    /// The value this machine's metadata service publishes at `path`, as the lines it
    /// answers with, and `None` for a path it publishes nothing at.
    ///
    /// The paths are the instance-metadata categories AWS publishes (Amazon EC2 User Guide,
    /// "Instance metadata categories"), each answered off the stored instance; a category
    /// whose value this instance has none of is published by neither the listing nor the
    /// path itself.
    fn published(&self, path: &str) -> Option<Vec<String>> {
        let machine = &self.machine;
        let one = |value: String| Some(vec![value]);
        match path {
            METADATA_PREFIX => Some(
                METADATA_CATEGORIES
                    .iter()
                    .filter(|category| {
                        self.published(&format!("{METADATA_PREFIX}/{category}"))
                            .is_some()
                    })
                    .map(|category| (*category).to_string())
                    .collect(),
            ),
            IDENTITY_PREFIX => Some(
                [IDENTITY_DOCUMENT_PATH, IDENTITY_SIGNATURE_PATH]
                    .into_iter()
                    .filter(|published| self.published(published).is_some())
                    .filter_map(|published| published.rsplit('/').next())
                    .map(str::to_string)
                    .collect(),
            ),
            "/latest/meta-data/ami-id" => machine.image_id.clone().map(|id| vec![id]),
            "/latest/meta-data/instance-id" => one(machine.instance_id.clone()),
            "/latest/meta-data/instance-type" => one(machine.instance_type.clone()),
            "/latest/meta-data/local-ipv4" => one(machine.private_ip.clone()),
            "/latest/meta-data/local-hostname" | "/latest/meta-data/hostname" => {
                one(machine.private_dns_name.clone())
            }
            "/latest/meta-data/public-ipv4" => machine.public_ip.clone().map(|ip| vec![ip]),
            "/latest/meta-data/mac" => one(guest_mac_address(machine)),
            "/latest/meta-data/placement/availability-zone" => {
                one(machine.availability_zone.clone())
            }
            "/latest/meta-data/placement/region" => {
                one(region_of_zone_name(&machine.availability_zone).to_string())
            }
            // The profile the machine runs as, and the role it carries: an instance that runs
            // as no profile publishes neither, which is what the metadata service of such an
            // instance answers (the category is simply not there).
            IAM_INFO_PATH => machine.instance_profile.as_ref().map(iam_info),
            IAM_SECURITY_CREDENTIALS_PATH => machine
                .instance_profile
                .as_ref()
                .and_then(|profile| profile.role_name.clone())
                .map(|role_name| vec![role_name]),
            IDENTITY_DOCUMENT_PATH => one(identity_document(machine)),
            // The published signature is base64 text of the document's signature, which is
            // the form the operations that take one accept.
            IDENTITY_SIGNATURE_PATH => identity_document_signature(&identity_document(machine))
                .map(|signature| vec![signature]),
            // The CREDENTIALS of the role the machine runs as, published UNDER that role's
            // name: "applications on the instance retrieve the security credentials provided
            // by the role from the instance metadata item
            // `iam/security-credentials/<role-name>`" (Amazon EC2 User Guide, "Use an IAM
            // role to grant permissions to applications running on Amazon EC2 instances").
            // The credentials are the ones minted for this machine
            // ([`super::instance_role_credentials`]) — a credential is minted by the token
            // service, never by a reader — so a machine nothing minted one for publishes
            // nothing here, and so does a read under any other name than the role's.
            other => other
                .strip_prefix(IAM_SECURITY_CREDENTIALS_PATH)
                .and_then(|under| under.strip_prefix('/'))
                .and_then(|role_name| role_credentials(machine, role_name)),
        }
    }
}

/// The credentials document the machine publishes under the role name `role_name`, or `None`
/// when it publishes none there: a machine running as no profile, one whose role nothing
/// minted credentials for, and a name that is not the role the credentials belong to.
///
/// The members are the ones AWS's own document carries (Amazon EC2 User Guide, "Instance
/// metadata categories"), each read off the minted session: its `Code` and `Type` are the
/// words that document carries ([`super::ROLE_CREDENTIALS_SUCCESS`],
/// [`super::ROLE_CREDENTIALS_TYPE`]), `LastUpdated` is when the session was minted and
/// `Expiration` when it stops being valid, and the three credential members are the token
/// service's own — nothing here constructs an access key id, a secret or a token.
fn role_credentials(machine: &GuestMachine, role_name: &str) -> Option<Vec<String>> {
    let credentials = machine
        .instance_profile
        .as_ref()?
        .role_credentials
        .as_ref()
        .filter(|credentials| credentials.role_name == role_name)?;
    Some(vec![
        ::serde_json::json!({
            "Code": super::ROLE_CREDENTIALS_SUCCESS,
            "LastUpdated": iso8601_utc(credentials.issued_at),
            "Type": super::ROLE_CREDENTIALS_TYPE,
            "AccessKeyId": credentials.access_key_id,
            "SecretAccessKey": credentials.secret_access_key,
            "Token": credentials.session_token,
            "Expiration": iso8601_utc(credentials.expires_at),
        })
        .to_string(),
    ])
}

/// The metadata categories the listing of [`METADATA_PREFIX`] names, in the order the
/// service lists them (alphabetically, as a directory listing is). A category this instance
/// has no value for is left out of the listing by the reader above.
const METADATA_CATEGORIES: &[&str] = &[
    "ami-id",
    "hostname",
    "iam/info",
    "iam/security-credentials",
    "instance-id",
    "instance-type",
    "local-hostname",
    "local-ipv4",
    "mac",
    "placement/availability-zone",
    "placement/region",
    "public-ipv4",
];

/// The `iam/info` document of the instance profile a machine runs as: the members AWS's own
/// document carries (Amazon EC2 User Guide, "Instance metadata categories"), each read off
/// the profile this store holds for the instance. A member the profile has no value for is
/// left out rather than written as null, which is what the published document does.
fn iam_info(profile: &GuestInstanceProfile) -> Vec<String> {
    let mut document = ::serde_json::Map::new();
    document.insert(
        "Code".to_string(),
        ::serde_json::Value::String(IAM_INFO_SUCCESS.to_string()),
    );
    if let Some(arn) = profile.arn.clone() {
        document.insert(
            "InstanceProfileArn".to_string(),
            ::serde_json::Value::String(arn),
        );
    }
    if let Some(id) = profile.id.clone() {
        document.insert(
            "InstanceProfileId".to_string(),
            ::serde_json::Value::String(id),
        );
    }
    vec![::serde_json::Value::Object(document).to_string()]
}

/// The identity document of `machine`: the JSON document the metadata service publishes for
/// the instance, with every member read off the stored instance.
///
/// The members are the ones an AWS identity document carries (Amazon EC2 User Guide,
/// "Instance identity documents"); a member the instance has no value for is `null`, which
/// is what AWS writes for one too.
#[must_use]
pub fn identity_document(machine: &GuestMachine) -> String {
    ::serde_json::json!({
        "accountId": ::engine::emulator_account_id(),
        "architecture": machine.architecture,
        "availabilityZone": machine.availability_zone,
        "billingProducts": ::serde_json::Value::Null,
        "devpayProductCodes": ::serde_json::Value::Null,
        "imageId": machine.image_id,
        "instanceId": machine.instance_id,
        "instanceType": machine.instance_type,
        "kernelId": machine.kernel_id,
        "marketplaceProductCodes": ::serde_json::Value::Null,
        "pendingTime": iso8601_utc(machine.launch_time),
        "privateIp": machine.private_ip,
        "ramdiskId": machine.ramdisk_id,
        "region": region_of_zone_name(&machine.availability_zone),
        "version": IDENTITY_DOCUMENT_VERSION,
    })
    .to_string()
}

/// The signature the metadata service publishes for `document`: the RSASSA-PKCS1-v1_5
/// signature of its bytes under [`signing_key`], base64 — the form the published signature
/// takes, which is why the operations that accept one accept base64 text of a signature's
/// length.
///
/// `None` when this process could not produce a key or could not sign, in which case the
/// metadata service publishes no signature at all rather than bytes that are not one.
#[must_use]
pub fn identity_document_signature(document: &str) -> Option<String> {
    let signed = signing_key()?.sign(document.as_bytes())?;
    Some(::aws_smithy_types::base64::encode(&signed))
}

/// The key the identity documents of this emulator's instances are signed under, generated
/// ONCE for this process.
///
/// Its private half never leaves this module: the only thing anything can ask of it is a
/// signature ([`SigningKey::sign`]).
fn signing_key() -> Option<&'static SigningKey> {
    static KEY: OnceLock<Option<SigningKey>> = OnceLock::new();
    KEY.get_or_init(|| SigningKey::generate(RS256)).as_ref()
}
