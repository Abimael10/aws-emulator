// SPDX-License-Identifier: CC0-1.0

//! The security requirement CONTENT every AWS managed security requirement pack ships
//! with: the rules a fresh account can already read out of the packs AWS manages.
//!
//! `CreateSecurityRequirementPack` documents itself as creating a *customer managed*
//! pack, so the other `ManagementType` — `AWS_MANAGED` — is a pack AWS itself ships,
//! content included. A shipped pack therefore holds the requirements its framework
//! defines from the moment the account exists: nothing creates them, and no caller may
//! write them.
//!
//! RECORDED, not modelled: the model declares the requirement shapes but enumerates
//! neither which packs AWS ships nor what they hold, so the tables in the child modules
//! are the content a fresh account's `ListSecurityRequirements` reports for each managed
//! pack — the requirement NAMES, their descriptions, the security domain each belongs to,
//! how compliance with it is evaluated and how it is remediated. A pack's table is its
//! COMPLETE shipped set rather than a sample of it, and each entry's `description` is the
//! requirement's own published statement rather than a summary of it: a client reads
//! `description` as the rule, and selects, renders or hashes the set as a whole, so a
//! short table reads as a smaller framework and a paraphrase reads as a different rule.
//!
//! One child module per pack, because a pack's shipped content is one concern: the base
//! pack's own default rules ([`base`]), the NIST Cybersecurity Framework functions
//! ([`nist_csf`]), the PCI DSS v4.0.1 requirement groups ([`pci_dss`]) and the AWS
//! Well-Architected Security Pillar best-practice areas ([`well_architected`]). Each table
//! follows the published structure of the framework its pack tracks, so an entry is
//! content of that framework rather than a value invented to fill a response.
//!
//! The records these entries become are stored in the ONE requirement store
//! `BatchCreateSecurityRequirements` writes and `ListSecurityRequirements` /
//! `BatchGetSecurityRequirements` read, keyed by `(packId, name)` like any other
//! requirement, so a shipped requirement is projected through exactly the same path as a
//! created one.

mod base;
mod nist_csf;
mod pci_dss;
mod well_architected;

pub use base::BASE_PACK_REQUIREMENTS;
pub use nist_csf::NIST_CSF_PACK_REQUIREMENTS;
pub use pci_dss::PCI_DSS_PACK_REQUIREMENTS;
pub use well_architected::WELL_ARCHITECTED_PACK_REQUIREMENTS;

/// One security requirement a pack AWS manages ships with: the rule's name — its
/// identity inside the pack — and the content every read of it reports.
///
/// Static catalog content, like the pack's own name and description: none of it is a
/// caller's to supply or to write, so it is a table rather than request input.
#[derive(Debug, Clone, Copy)]
pub struct AwsManagedRequirement {
    /// The requirement's name, which identifies it within its pack.
    pub name: &'static str,
    /// The requirement's own published statement — what the workload must do and what
    /// its design has to demonstrate — because that is what a read of `description`
    /// reports and what a client evaluates or renders the rule from.
    pub description: &'static str,
    /// The security domain the requirement belongs to (source: the
    /// `CreateSecurityRequirementEntry$domain` member's doc, "The security domain the
    /// requirement belongs to").
    pub domain: &'static str,
    /// How compliance with the requirement is assessed (source: the `$evaluation`
    /// member's doc, "The evaluation criteria used to assess compliance with this
    /// requirement").
    pub evaluation: &'static str,
    /// The recommended remediation, which the member leaves optional — a shipped rule
    /// carries one.
    pub remediation: Option<&'static str>,
}

/// When the requirement content of the base pack was published, as seconds since the
/// Unix epoch: 2025-12-01T00:00:00Z, the catalog's own publication instant
/// ([`super::AWS_MANAGED_PACK_PUBLISHED_AT_EPOCH_SECONDS`]) — nothing has revised the
/// default rules since the catalog shipped them.
///
/// RECORDED, not modelled, for the same reason the catalog's instant is: the
/// `securityagent-catalog-list-security-requirements` fixture reports ONE instant as both
/// `createdAt` and `updatedAt` for every requirement of a pack, and that instant is a
/// property of the shipped content rather than of this process — a rule the account was
/// created with was not published when the emulator started, and a client that caches a
/// pack's content or asks "has it changed since X" must see the same instant across
/// restarts. So it is written here instead of read from the clock, and it moves only when
/// the content below it does.
pub const BASE_CONTENT_PUBLISHED_AT_EPOCH_SECONDS: u64 =
    super::AWS_MANAGED_PACK_PUBLISHED_AT_EPOCH_SECONDS;

/// The same, for the NIST CSF pack's content: 2026-05-11T21:16:07Z, when the framework
/// revision that pack tracks shipped. A pack's content carries its OWN revision instant,
/// which is why this is not the catalog's.
pub const NIST_CSF_CONTENT_PUBLISHED_AT_EPOCH_SECONDS: u64 = 1_778_534_167;

/// The same, for the PCI DSS pack's content: 2026-06-15T19:28:54Z.
pub const PCI_DSS_CONTENT_PUBLISHED_AT_EPOCH_SECONDS: u64 = 1_781_551_734;

/// The same, for the Well-Architected pack's content, which shipped in the revision the
/// NIST CSF content did: 2026-05-11T21:16:07Z.
pub const WELL_ARCHITECTED_CONTENT_PUBLISHED_AT_EPOCH_SECONDS: u64 =
    NIST_CSF_CONTENT_PUBLISHED_AT_EPOCH_SECONDS;
