// SPDX-License-Identifier: CC0-1.0

//! The requirement content the base pack (`ASA Base Pack`) ships with.
//!
//! The base pack holds the service's own default rules — the ones it evaluates against
//! when a caller has defined no pack of its own — so each entry is a best-practice area
//! of the service's own review, named `<area>-best-practices`, and its `description` is
//! the one-sentence rule statement that area is published with. That is the shape of this
//! pack's content, which is why its descriptions are single statements where the three
//! framework packs carry a framework requirement's published prose.
//!
//! WHICH TEXT IS WHICH: the pack's requirement NAMES, the count of them and the STATEMENT
//! of each of the ten rules are recorded — they are what a fresh account's
//! `ListSecurityRequirements` reports for this pack — and every statement below is written
//! as the corpus states it, in full and word for word, because the whole string is the
//! rule: it carries no name, id, limit or timestamp from the request or from state, so a
//! re-wording of it is a different rule rather than a different rendering of the same one.
//! The `domain`, `evaluation` and `remediation` beside each statement are written to that
//! same one-statement shape rather than recorded, and a later recording of their exact
//! wording replaces them rather than being judged equal.
//!
//! TEN, not twenty-eight: the report that filed the short tables reads the recording as
//! holding 28 entries for this pack, but the recorded step reports these ten. Its own
//! divergence keys for that step are eighteen, which is the nine published names the table
//! lacked plus the nine names it held that the pack does not publish — a 28-entry pack
//! whose ten seeded entries all matched would have produced eighteen keys none of which
//! named a seeded entry, and the keys name all nine of them. Every one of those eighteen
//! stops diverging once the pack holds exactly the ten below, and the earlier report of the
//! same fixture step quotes its expectation as `securityRequirementSummaries[0..9+]`.

use super::AwsManagedRequirement;

/// The requirements the base pack ships with, complete: the ten best-practice areas the
/// pack defines, in name order.
pub const BASE_PACK_REQUIREMENTS: [AwsManagedRequirement; 10] = [
    AwsManagedRequirement {
        name: "audit-logging-best-practices",
        description: "Ensure the system supports security monitoring.",
        domain: "Logging",
        evaluation: "Check that security-relevant events are written to a durable audit \
                     log that is retained and queryable.",
        remediation: Some(
            "Enable audit logging for every component and deliver the records to a \
             retained, access-controlled log destination.",
        ),
    },
    AwsManagedRequirement {
        name: "authentication-best-practices",
        description: "Ensure only legitimate users can access the system.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check that every entry point authenticates its caller against a \
                     trusted identity source before serving the request.",
        remediation: Some(
            "Authenticate every entry point against a trusted identity source and reject \
             unauthenticated requests.",
        ),
    },
    AwsManagedRequirement {
        name: "authorization-best-practices",
        description: "Ensure systems follow authorization best practices.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check that each request is authorized against the caller's \
                     permissions for the resource it names, and not only authenticated.",
        remediation: Some(
            "Authorize every request against the caller's permissions for the resource it \
             names, and deny what is not explicitly allowed.",
        ),
    },
    AwsManagedRequirement {
        name: "information-protection-best-practices",
        description: "Ensure sensitive data remains confidential and unaltered.",
        domain: "DataProtection",
        evaluation: "Check that sensitive information is classified, encrypted in transit \
                     and at rest, and reachable only by the identities that need it.",
        remediation: Some(
            "Classify the information the system holds and encrypt and restrict each \
             store to the identities that need it.",
        ),
    },
    AwsManagedRequirement {
        name: "log-protection-best-practices",
        description: "Protect the integrity and confidentiality of system logs.",
        domain: "Logging",
        evaluation: "Check that log records carry no credentials or sensitive data and \
                     that the log destination cannot be altered by what it records.",
        remediation: Some(
            "Redact sensitive values before they are logged and write the records to a \
             destination the logged component cannot modify.",
        ),
    },
    AwsManagedRequirement {
        name: "privileged-access-best-practices",
        description: "Ensure there are adequate guardrails for privileged functions.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check that privileged access is held by the fewest identities \
                     possible, granted for a bounded time and recorded when used.",
        remediation: Some(
            "Reduce privileged access to the identities that need it, grant it for a \
             bounded time and record each use.",
        ),
    },
    AwsManagedRequirement {
        name: "secret-protection-best-practices",
        description: "Ensure that secrets like credentials remain confidential.",
        domain: "DataProtection",
        evaluation: "Check the repository, build configuration and images for credential \
                     material, and that each secret is read from a managed secret store \
                     at run time.",
        remediation: Some(
            "Move every secret into a managed secret store, read it at run time and \
             revoke whatever was disclosed.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-by-default-best-practices",
        description: "Ensure the system's default configuration is secure.",
        domain: "SecureConfiguration",
        evaluation: "Check that a component deployed with no configuration denies access, \
                     encrypts its data and exposes no optional interface.",
        remediation: Some(
            "Make the default configuration the restrictive one, so a component is secure \
             before anything is configured on it.",
        ),
    },
    AwsManagedRequirement {
        name: "tenant-isolation-best-practices",
        description: "Ensure appropriate separation between system tenants.",
        domain: "MultiTenancy",
        evaluation: "Check that every request carries the tenant it acts for and that \
                     each data access is scoped to that tenant at the store rather than \
                     in the caller.",
        remediation: Some(
            "Carry the tenant on every request and scope each data access to it at the \
             store, so a missing scope fails rather than widens.",
        ),
    },
    AwsManagedRequirement {
        name: "trusted-cryptography-best-practices",
        description: "Ensure only trustworthy cryptographic implementations are used.",
        domain: "DataProtection",
        evaluation: "Check that the algorithms, key sizes and protocol versions in use \
                     are the ones currently trusted, and that no component implements its \
                     own cryptography.",
        remediation: Some(
            "Replace deprecated algorithms and protocol versions with currently trusted \
             ones and use a vetted library rather than a local implementation.",
        ),
    },
];
