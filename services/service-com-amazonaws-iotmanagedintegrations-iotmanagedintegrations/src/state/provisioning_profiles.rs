// SPDX-License-Identifier: CC0-1.0

//! The provisioning-profile half of this service's state: the record a create writes, the
//! claim certificate material minted with it, and the removal a delete performs.
//!
//! A child module of [`super`] rather than more of `state.rs`: a provisioning profile is
//! one concern, and it is the only resource of this service that holds certificate material
//! at all — the claim certificate and its private half are minted here, when the profile is
//! created, and stored on the record so every later read reports the same material instead
//! of a freshly invented one.

use super::*;

/// The summary shape a list projects a stored profile onto, which no other item of
/// `crate::state` reaches, so it is imported here rather than at the module root.
use crate::ops::types::ProvisioningProfileSummary;

/// The `ResourceType` this service reports a provisioning profile under — the Smithy
/// resource name, as [`MANAGED_THING_RESOURCE_TYPE`] is for a managed thing. It is also the
/// resource kind an [`IdempotencyKey`] of a `CreateProvisioningProfile` carries.
pub const PROVISIONING_PROFILE_RESOURCE_TYPE: &str = "ProvisioningProfile";

/// The one sentence this service states for a provisioning profile it holds none of under
/// the identifier a request named, whatever operation resolved it.
///
/// It is FIXED text: it quotes no identifier and no other request value, so every absent
/// profile — never created, deleted, or created in another region — is stated the same way,
/// the way the recorded 404 of a get after a delete words it. The sentence names the KIND of
/// resource that was not found rather than which one, as a managed thing's read 404 does
/// ([`resource_not_found_message`]); this resource's backend ends it without a full stop, as
/// a connector destination's does ([`resource_name_not_found_message`]). The identifier the
/// caller sent is still reported, on the `ResourceId` member the shape declares for it.
pub const PROVISIONING_PROFILE_ABSENT_REASON: &str = "Provisioning Profile resource not found";

/// The resource-type segment a provisioning profile's ARN carries, which is both what the
/// `ProvisioningProfileArn` pattern
/// (`^arn:aws:iotmanagedintegrations:[0-9a-zA-Z-]+:[0-9]+:provisioning-profile/[0-9a-zA-Z]+$`)
/// requires and how the `ResourceType` of an event log configuration names that kind.
pub const PROVISIONING_PROFILE_ARN_SEGMENT: &str = "provisioning-profile";

/// The `ProvisioningProfileStatus` a created provisioning profile carries.
///
/// A create answers a profile that is already in this TERMINAL status, so nothing is owed to
/// the clock: the claim certificate is minted and the record stored inside the call, and
/// `CREATE_IN_PROGRESS` and `CREATE_FAILED` are statuses no creation of this emulator enters.
pub const PROVISIONING_PROFILE_STATUS_CREATED: &str = "CREATED";

/// The most provisioning profiles one store holds, for the same reason and on the same terms
/// as [`MAX_MANAGED_THINGS`]: a bound CHOSEN for the emulator, not a published AWS quota.
pub const MAX_PROVISIONING_PROFILES: usize = 1_000;

/// The most `ProvisioningProfileSummary` items one `ListProvisioningProfiles` page carries,
/// and the page size an absent `MaxResults` gets, for the same reason and on the same terms
/// as [`MANAGED_THING_PAGE_LIMIT`]: a caller reaches the rest of the store through the
/// `NextToken`, so one response stays bounded however many records are stored.
pub const PROVISIONING_PROFILE_PAGE_LIMIT: usize = 100;

/// One stored provisioning profile: the authoritative record every provisioning-profile
/// operation reads. `CreateProvisioningProfile` inserts it and `GetProvisioningProfile`
/// projects it onto its output shape.
#[derive(Clone)]
pub struct ProvisioningProfile {
    /// The generated `ProvisioningProfileId` this record is keyed by.
    pub id: String,
    /// `ProvisioningProfileArn`, as [`provisioning_profile_arn`] builds it.
    pub arn: String,
    /// The region the creating request was signed for. A provisioning profile is a regional
    /// resource, as every resource of this service is: only that region's endpoint resolves
    /// or lists it.
    pub region: String,
    /// `ProvisioningProfileName`, absent on a profile the create named none for, since the
    /// member is optional and the model publishes no default.
    pub name: Option<String>,
    /// `ProvisioningType` enum value (`FLEET_PROVISIONING` | `JITR`), which the create
    /// `@required`s.
    pub provisioning_type: String,
    /// The PEM-encoded claim certificate this profile provisions devices with: the one the
    /// create supplied, else the one minted for it ([`mint_claim_certificate`]). Reported by
    /// the create and by every read, so the material is issued once and never re-issued.
    pub claim_certificate: String,
    /// The private half of [`Self::claim_certificate`], held only where this service minted
    /// the certificate itself — a create that supplied its own claim certificate kept the
    /// key, so there is none to report for it.
    ///
    /// `ClaimCertificatePrivateKey` is a member of the CREATE's output alone: no read of
    /// this service declares one, so it leaves here only on the answer to the create that
    /// minted it and on a `ClientToken` replay of that same call.
    pub claim_certificate_private_key: Option<String>,
    /// The `CaCertificate` the create supplied, if any — `@sensitive`, and no output shape
    /// of this service declares a member for it, so it is stored and never reported.
    pub ca_certificate: Option<String>,
    pub tags: HashMap<String, String>,
    pub created_at: SystemTime,
    /// `ProvisioningProfileStatus` enum value, one of
    /// [`crate::ops::types::PROVISIONING_PROFILE_STATUS_VALUES`]: what the create that
    /// minted this profile stored for it ([`PROVISIONING_PROFILE_STATUS_CREATED`]), which is
    /// what every operation that reports the profile reports. It is a FIELD rather than a
    /// value each reader names, so one record is the single source of the status.
    ///
    /// `CREATE_IN_PROGRESS`, `CREATE_FAILED`, `DELETE_IN_PROGRESS` and `DELETE_FAILED` are
    /// the other values the model declares, and no profile of this emulator is ever in one: a
    /// create mints the material and stores the record inside the call, so no creation is
    /// left in flight or fails, and an accepted delete removes the record inside its own
    /// call, so no profile is ever reported on its way out either.
    pub status: String,
    /// Monotonic insertion sequence. `ListProvisioningProfiles` pages by it, so a page token
    /// keeps resuming at the right place even when records are deleted in between.
    pub sequence: u64,
}

/// Written by hand rather than derived: the record holds the claim certificate's PRIVATE key,
/// and a derived rendering would put it in every log line that prints a value this record is
/// reachable from — the whole store among them. What is diagnosable is which profile it is and
/// what state it is in, so the certificate material is left out and only its presence is
/// reported.
impl ::std::fmt::Debug for ProvisioningProfile {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("ProvisioningProfile")
            .field("id", &self.id)
            .field("arn", &self.arn)
            .field("region", &self.region)
            .field("provisioning_type", &self.provisioning_type)
            .field("status", &self.status)
            .field("created_at", &self.created_at)
            .field("sequence", &self.sequence)
            .field(
                "claim_certificate_private_key",
                &self.claim_certificate_private_key.is_some(),
            )
            .finish_non_exhaustive()
    }
}

/// The ARN of a provisioning profile in `region`, as the `ProvisioningProfileArn` pattern
/// requires.
pub fn provisioning_profile_arn(region: &str, id: &str) -> String {
    ::engine::emulator_arn(
        SERVICE_ARN_NAMESPACE,
        region,
        &format!("{PROVISIONING_PROFILE_ARN_SEGMENT}/{id}"),
    )
}

/// The modelled `ResourceNotFoundException` every operation of this service answers for a
/// provisioning profile no reader resolves under the identifier a request named: the one
/// sentence this resource's backend states its absence with
/// ([`PROVISIONING_PROFILE_ABSENT_REASON`]), plus the `ResourceId` and `ResourceType` members
/// its 404s report the identifier and the kind on.
///
/// ONE reason, read and mutation alike, as a cloud connector's and an account association's
/// 404s carry ([`cloud_connector_not_found`], [`account_association_not_found`]): the read
/// 404s of this service are not one sentence — each resource's backend words its own — and
/// this is the sentence recorded for a profile a delete had just removed, from a get of it and
/// from the repeated delete of it alike.
pub fn provisioning_profile_not_found(identifier: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(PROVISIONING_PROFILE_ABSENT_REASON.to_string()),
        ..super::resource_not_found(PROVISIONING_PROFILE_RESOURCE_TYPE, identifier)
    }
}

/// The `ProvisioningProfileSummary` a list reports for the stored profile `profile`.
///
/// Every member comes from the record, the status included, so a profile reported in a list
/// and the same profile read on its own cannot disagree.
pub fn provisioning_profile_summary(profile: &ProvisioningProfile) -> ProvisioningProfileSummary {
    ProvisioningProfileSummary {
        id: Some(profile.id.clone()),
        arn: Some(profile.arn.clone()),
        name: profile.name.clone(),
        provisioning_type: Some(profile.provisioning_type.clone()),
        status: Some(profile.status.clone()),
    }
}

/// The claim certificate a provisioning profile provisions devices with, and the private
/// half a device validates the connection endpoint with.
#[derive(Clone)]
pub struct ClaimCertificateMaterial {
    /// The PEM-encoded X.509 certificate, as `ClaimCertificate` carries it.
    pub certificate: String,
    /// The PEM-encoded PKCS#8 private key of that certificate, as
    /// `ClaimCertificatePrivateKey` carries it.
    pub private_key: String,
}

/// Written by hand rather than derived: the struct holds a private key, and a derived
/// rendering would put it in every log line that prints the value it is held on. What is
/// diagnosable is that material is there at all.
impl ::std::fmt::Debug for ClaimCertificateMaterial {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("ClaimCertificateMaterial")
            .finish_non_exhaustive()
    }
}

/// The subject common name a minted claim certificate is issued under.
///
/// A claim certificate is the identity a device presents while it is being provisioned,
/// before it has one of its own, so it is issued to the provisioning profile that mints it:
/// the subject names the profile's own identifier, which is what distinguishes one claim
/// certificate of an account from another.
fn claim_certificate_subject(id: &str) -> String {
    format!("{PROVISIONING_PROFILE_ARN_SEGMENT}/{id}")
}

/// How long a minted claim certificate is valid from the instant the profile is created.
///
/// A claim certificate outlives the provisioning of every device that uses it, and AWS
/// publishes no validity for the one a provisioning profile mints, so the window is CHOSEN:
/// long enough that a device provisioned with a profile created in this emulator's lifetime
/// presents a certificate inside its validity period.
const CLAIM_CERTIFICATE_VALIDITY: Duration = Duration::from_secs(10 * 365 * 24 * 60 * 60);

/// Mint the claim certificate of the profile `id`, valid from `not_before`.
///
/// A REAL self-signed X.509 certificate and its private key, issued through the maintained
/// generator this tree already uses for certificate material: `ClaimCertificate` is "the
/// body of the PEM-encoded claim certificate" and
/// `ClaimCertificatePrivateKey` "may be stored securely on the device for validating the
/// connection endpoint with IoT managed integrations using the public key"
/// (`smithy_query(query="doc",shape="CreateProvisioningProfileResponse",member="ClaimCertificatePrivateKey")`),
/// so what those members carry has to be material a device can decode, parse and present —
/// not text in the shape of it.
///
/// `None` when the material does not issue, which the operation that asked for it reports
/// rather than storing a provisioning profile with nothing to provision a device with.
pub fn mint_claim_certificate(id: &str, not_before: SystemTime) -> Option<ClaimCertificateMaterial> {
    let key = ::rcgen::KeyPair::generate().ok()?;
    // No subject alternative name: a claim certificate identifies the device presenting it
    // to the provisioning endpoint, it does not serve a name itself.
    let mut params = ::rcgen::CertificateParams::new(Vec::<String>::new()).ok()?;
    let mut subject = ::rcgen::DistinguishedName::new();
    subject.push(::rcgen::DnType::CommonName, claim_certificate_subject(id));
    params.distinguished_name = subject;
    params.not_before = offset_date_time(not_before)?;
    params.not_after = offset_date_time(not_before + CLAIM_CERTIFICATE_VALIDITY)?;
    params.is_ca = ::rcgen::IsCa::NoCa;
    params.key_usages = vec![
        ::rcgen::KeyUsagePurpose::DigitalSignature,
        ::rcgen::KeyUsagePurpose::KeyEncipherment,
    ];
    // The certificate is presented BY the device being provisioned, which is the purpose a
    // verifying endpoint checks it was issued for.
    params.extended_key_usages = vec![::rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    let certificate = params.self_signed(&key).ok()?;
    Some(ClaimCertificateMaterial {
        certificate: certificate.pem(),
        private_key: key.serialize_pem(),
    })
}

/// `at` as the date-time type the certificate generator takes a validity bound in, at the
/// one-second resolution an X.509 validity period is stated at.
///
/// `None` for an instant outside the range that type represents, which is the same answer
/// the caller gives for material that does not issue.
fn offset_date_time(at: SystemTime) -> Option<::time::OffsetDateTime> {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since_epoch| i64::try_from(since_epoch.as_secs()).ok())?;
    ::time::OffsetDateTime::from_unix_timestamp(seconds).ok()
}

impl ComAmazonawsIotmanagedintegrationsIotmanagedintegrationsState {
    /// The provisioning profile `region`'s endpoint resolves under `id`, if any.
    ///
    /// A profile created in another region is not visible here, as for every regional
    /// resource, and neither is one a delete has removed: the record is gone from the store
    /// with the call that asked for its removal ([`Self::remove_provisioning_profile_in`]), so
    /// every operation that resolves a profile stops resolving it from that call on.
    pub fn provisioning_profile_in(&self, region: &str, id: &str) -> Option<&ProvisioningProfile> {
        self.provisioning_profiles
            .get(id)
            .filter(|profile| profile.region == region)
    }

    /// [`Self::provisioning_profile_in`], mutably: what a tagging call writes to, so the write
    /// side resolves a profile by exactly the rule the read side resolves it by.
    pub fn provisioning_profile_in_mut(
        &mut self,
        region: &str,
        id: &str,
    ) -> Option<&mut ProvisioningProfile> {
        self.provisioning_profiles
            .get_mut(id)
            .filter(|profile| profile.region == region)
    }

    /// Every provisioning profile `region`'s endpoint resolves, in the order they were
    /// created: what a list reports, and what bounds its paging.
    pub fn provisioning_profiles_in<'a>(
        &'a self,
        region: &'a str,
    ) -> impl Iterator<Item = &'a ProvisioningProfile> {
        self.provisioning_profile_order
            .iter()
            .filter_map(|id| self.provisioning_profiles.get(id))
            .filter(move |profile| profile.region == region)
    }

    /// Store `profile`, keyed by its own id and appended to the order every operation that
    /// reports several profiles reports them in.
    pub fn insert_provisioning_profile(&mut self, profile: ProvisioningProfile) {
        self.provisioning_profile_order.push(profile.id.clone());
        self.provisioning_profiles
            .insert(profile.id.clone(), profile);
    }

    /// Remove the provisioning profile `region`'s endpoint resolves under `id`, with its
    /// certificate material and the `ClientToken` records that replayed into it. `true` when a
    /// profile was removed, `false` when this region holds none under that id — which is what
    /// makes a delete of an identifier nothing names the modelled 404.
    ///
    /// The removal is performed INSIDE the call that asks for it, so the next request of any
    /// caller resolves nothing under that id: the record, not a mark on it, is what the
    /// readers resolve.
    pub fn remove_provisioning_profile_in(&mut self, region: &str, id: &str) -> bool {
        if self.provisioning_profile_in(region, id).is_none() {
            return false;
        }
        self.provisioning_profiles.remove(id);
        self.provisioning_profile_order.retain(|held| held != id);
        // A `ClientToken` whose profile is gone must not replay into it: a create presenting
        // that token again mints a new profile rather than answering one nothing can
        // provision with any more.
        self.forget_idempotency_tokens_for(PROVISIONING_PROFILE_RESOURCE_TYPE, id);
        true
    }
}
