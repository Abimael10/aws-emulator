// SPDX-License-Identifier: CC0-1.0

//! The packages this account HOLDS, as stored: one record per package a
//! `CreatePackage` minted, carrying every revision of that package's content.
//!
//! A package exists for this service only once a create wrote its record, so
//! every answer about a package — the create's own `PackageDetails`, the
//! `DescribePackages` listing, the version history and an association's
//! `PackageVersion` — is a projection of the record below. There is no table of
//! packages beside the store: a listing reports the records of the Region the
//! call was signed for and nothing else, which is what keeps it from reporting a
//! package the account does not own and cannot read back.

use super::*;

/// `PackageStatus`'s `COPYING`: the package's content is still being copied out
/// of the customer's S3 location. The status a package holds from the instant
/// `CreatePackage` returns until [`active_deadline`] has passed.
pub const PACKAGE_STATUS_COPYING: &str = "COPYING";

/// `PackageStatus`'s `AVAILABLE`: the copy finished and the package can be
/// associated with a domain.
pub const PACKAGE_STATUS_AVAILABLE: &str = "AVAILABLE";

/// The label the `ordinal`-th revision of a package's content carries.
///
/// `PackageVersion` is an unconstrained string, and the versions one package
/// goes through are a GENERATED SEQUENCE rather than anything the caller
/// supplies: a create mints the first, and every `UpdatePackage` the next
/// counting number behind the same `v` prefix. One function is the only place
/// that shape is written, so `AvailablePackageVersion`, the version history and
/// an association's `PackageVersion` cannot name a package's version three
/// different ways.
pub fn package_version_label(ordinal: usize) -> String {
    format!("v{ordinal}")
}

/// The most revisions one package may hold.
///
/// `UpdatePackage` is a caller-driven growth site on a record that is never
/// otherwise trimmed, and this is the emulator's own bound on it, answered with
/// the modelled `LimitExceededException` ("trying to create more than allowed
/// resources or sub-resources"). It sits far above the number of revisions a
/// dictionary goes through in any one workflow.
pub const MAX_PACKAGE_VERSIONS: usize = 100;

/// A hard ceiling on every stored package, across every region.
///
/// Packages are the second caller-driven growth site in this store, and
/// `CreatePackage`'s only bound on it: the region comes from the request's own
/// credential scope, so a per-region allowance alone would not bound the total.
/// It sits far above the number of packages any domain workflow associates, so
/// no caller that stays inside the service's own quotas is clipped by it.
pub const MAX_PACKAGES_TOTAL: usize = 1_000;

/// The key one package is stored under: the region it was created in and the id
/// the service minted for it.
pub fn package_key(region: &str, package_id: &str) -> (String, String) {
    (region.to_string(), package_id.to_string())
}

/// How many hexadecimal digits a package id carries behind its prefix.
///
/// `PackageID` declares no constraint, so the FORM is read off the wire this API
/// is recorded on: every package id a recorded request of this API addresses a
/// package by is `pkg-` followed by 40 lowercase hex digits. An id is something a
/// client stores and presents back, so minting one of that width is what keeps
/// an id this emulator hands out indistinguishable in shape from one the service
/// hands out; the DIGITS are this store's own, derived below.
const PACKAGE_ID_HEX_DIGITS: usize = 40;

/// The id a package is addressed by.
///
/// What matters of the value is that it is opaque and that two packages never
/// share one: the digits are a digest of the store's monotonic package counter,
/// which is injective in practice for the counter values one store reaches, and
/// they carry no part of the caller's request — a package name is not recoverable
/// from the id. The width is [`PACKAGE_ID_HEX_DIGITS`].
pub fn package_id(seq: u64) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(format!("package/{seq}").as_bytes());
    let hex = hex::encode(digest);
    format!(
        "pkg-{token}",
        token = hex.get(..PACKAGE_ID_HEX_DIGITS).unwrap_or(hex.as_str())
    )
}

/// One revision of a package's content, as stored.
///
/// The sequence of these IS the package's version history: `CreatePackage`
/// appends the first and each `UpdatePackage` the next, so the history a read
/// reports is the stored sequence rather than anything recomputed per response.
#[derive(Debug, Clone)]
pub struct PackageVersionRecord {
    /// This revision's label — see [`package_version_label`].
    pub version: String,
    /// `CommitMessage`, "a message associated with the version", where the
    /// update that minted it carried one. A create has no such input member, so
    /// a package's first version has none.
    pub commit_message: Option<String>,
    /// When this revision was minted.
    pub created_at: SystemTime,
}

impl PackageVersionRecord {
    /// This record as the `PackageVersionHistory` entry a history read reports.
    pub fn to_history(&self) -> PackageVersionHistory {
        PackageVersionHistory {
            package_version: Some(self.version.clone()),
            commit_message: self.commit_message.clone(),
            created_at: Some(datetime_of(self.created_at)),
        }
    }
}

/// One package, as stored.
///
/// The record is the single source of truth for every answer about the package:
/// the create's response and every later read are both [`Self::to_details`] of
/// it, so the two cannot disagree. The package's status is COMPUTED from the
/// instant its copy falls due rather than stored as a mutable status, so no read
/// has to write.
#[derive(Debug, Clone)]
pub struct Package {
    /// `PackageID`, unique per package, so it is half the key the store holds
    /// the record under.
    pub package_id: String,
    /// `PackageName`, the name the caller gave the package. Unique among the
    /// packages of one region, which is the scope a create checks it in.
    pub package_name: String,
    /// `PackageType` — one of `PACKAGE_TYPE_VALUES`.
    pub package_type: String,
    /// `PackageDescription`, where the create supplied one.
    pub package_description: Option<String>,
    /// The S3 location the content was imported from, as the request gave it.
    pub source: PackageSource,
    /// When the package was created.
    pub created_at: SystemTime,
    /// When the package last changed.
    pub last_updated_at: SystemTime,
    /// When the clock finishes copying the current content, i.e. when the
    /// package becomes [`PACKAGE_STATUS_AVAILABLE`].
    pub available_at: SystemTime,
    /// Every revision of the package's content, oldest first — the package's
    /// version history, and the source of the version its current content
    /// carries.
    pub versions: Vec<PackageVersionRecord>,
}

impl Package {
    /// A newly created package, whose content is still being copied.
    pub fn create(
        package_id: String,
        package_name: String,
        package_type: String,
        package_description: Option<String>,
        source: PackageSource,
        now: SystemTime,
    ) -> Self {
        Self {
            package_id,
            package_name,
            package_type,
            package_description,
            source,
            created_at: now,
            last_updated_at: now,
            available_at: active_deadline(now),
            versions: vec![PackageVersionRecord {
                version: package_version_label(1),
                commit_message: None,
                created_at: now,
            }],
        }
    }

    /// The label of the package's newest revision — the version its current
    /// copy is of, and the one `AvailablePackageVersion` names once that copy
    /// has landed.
    pub fn current_version(&self) -> &str {
        self.versions
            .last()
            .map(|revision| revision.version.as_str())
            .unwrap_or_default()
    }

    /// Take new content: the package's source becomes `source`, the clock starts
    /// copying it again, and the history grows by the next version.
    ///
    /// `PackageDescription` is replaced only when the update supplied one, since
    /// the member is optional and an update that omits it changes nothing about
    /// the description.
    pub fn revise(
        &mut self,
        source: PackageSource,
        package_description: Option<String>,
        commit_message: Option<String>,
        now: SystemTime,
    ) {
        self.source = source;
        if package_description.is_some() {
            self.package_description = package_description;
        }
        self.versions.push(PackageVersionRecord {
            version: package_version_label(self.versions.len() + 1),
            commit_message,
            created_at: now,
        });
        self.last_updated_at = now;
        self.available_at = active_deadline(now);
    }

    /// The package's version history, NEWEST FIRST: a history read reports the
    /// most recent revision first, so the newest version is the head of the
    /// first page.
    pub fn history_newest_first(&self) -> Vec<&PackageVersionRecord> {
        self.versions.iter().rev().collect()
    }

    /// Whether the clock has not yet finished copying the package's content.
    pub fn copying(&self, now: SystemTime) -> bool {
        now < self.available_at
    }

    /// The package's modelled state — one of `PACKAGE_STATUS_VALUES`.
    pub fn status(&self, now: SystemTime) -> &'static str {
        if self.copying(now) {
            PACKAGE_STATUS_COPYING
        } else {
            PACKAGE_STATUS_AVAILABLE
        }
    }

    /// This record as the `PackageDetails` every package operation reports.
    ///
    /// `AvailablePackageVersion` names the version a domain can associate, so a
    /// package whose copy has not landed yet has none to report; `ErrorDetails`
    /// is "additional information if the package is in an error state. Null
    /// otherwise" (its own documentation), and a stored package is in no error
    /// state.
    pub fn to_details(&self, now: SystemTime) -> PackageDetails {
        PackageDetails {
            package_id: Some(self.package_id.clone()),
            package_name: Some(self.package_name.clone()),
            package_type: Some(self.package_type.clone()),
            package_description: self.package_description.clone(),
            package_status: Some(self.status(now).to_string()),
            created_at: Some(datetime_of(self.created_at)),
            last_updated_at: Some(datetime_of(self.last_updated_at)),
            available_package_version: (!self.copying(now))
                .then(|| self.current_version().to_string()),
            error_details: None,
        }
    }
}
