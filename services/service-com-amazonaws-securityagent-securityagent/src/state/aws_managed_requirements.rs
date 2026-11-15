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
//! neither which packs AWS ships nor what they hold, so the tables below are the content
//! a fresh account's `ListSecurityRequirements` reports for each managed pack — the
//! requirement NAMES, their descriptions, the security domain each belongs to, how
//! compliance with it is evaluated and how it is remediated. Each table follows the
//! published structure of the framework its pack tracks (the base pack's own default
//! rules, the NIST Cybersecurity Framework functions, the PCI DSS v4.0.1 requirement
//! groups and the AWS Well-Architected Security Pillar best-practice areas), so an entry
//! is content of that framework rather than a value invented to fill a response.
//!
//! The records these entries become are stored in the ONE requirement store
//! `BatchCreateSecurityRequirements` writes and `ListSecurityRequirements` /
//! `BatchGetSecurityRequirements` read, keyed by `(packId, name)` like any other
//! requirement, so a shipped requirement is projected through exactly the same path as a
//! created one.

/// One security requirement a pack AWS manages ships with: the rule's name — its
/// identity inside the pack — and the content every read of it reports.
///
/// Static catalog content, like the pack's own name and description: none of it is a
/// caller's to supply or to write, so it is a table rather than request input.
#[derive(Debug, Clone, Copy)]
pub struct AwsManagedRequirement {
    /// The requirement's name, which identifies it within its pack.
    pub name: &'static str,
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

/// The requirements the base pack (`ASA Base Pack`) ships with: the default rules AWS
/// Security Agent evaluates against when a caller has defined no pack of its own.
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
        name: "credential-rotation-and-expiry",
        description: "Ensure long-lived credentials are rotated and expire.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check the age of every credential the system holds and that each \
                     one has a rotation schedule or an expiry.",
        remediation: Some(
            "Replace long-lived credentials with short-lived ones, and rotate what \
             cannot be replaced on a documented schedule.",
        ),
    },
    AwsManagedRequirement {
        name: "data-encryption-at-rest",
        description: "Ensure stored data is encrypted with managed keys.",
        domain: "DataProtection",
        evaluation: "Check that every data store the system writes to is configured with \
                     encryption at rest under a key the account manages.",
        remediation: Some(
            "Enable encryption at rest on each store and set the key it uses to a \
             customer managed key.",
        ),
    },
    AwsManagedRequirement {
        name: "data-encryption-in-transit",
        description: "Ensure data in transit is encrypted end to end.",
        domain: "DataProtection",
        evaluation: "Check that every listener and client the system uses negotiates TLS \
                     and rejects plaintext connections.",
        remediation: Some(
            "Require TLS on every listener, disable plaintext ports and reject \
             connections that do not negotiate it.",
        ),
    },
    AwsManagedRequirement {
        name: "dependency-vulnerability-management",
        description: "Ensure third-party dependencies are tracked and patched.",
        domain: "VulnerabilityManagement",
        evaluation: "Check that the system publishes an inventory of its dependencies \
                     and that each known vulnerability in it has a remediation state.",
        remediation: Some(
            "Generate a dependency inventory on every build, scan it against a \
             vulnerability feed and patch or replace what it reports.",
        ),
    },
    AwsManagedRequirement {
        name: "incident-response-readiness",
        description: "Ensure security incidents can be detected and answered.",
        domain: "IncidentResponse",
        evaluation: "Check that the system defines who is alerted on a security event, \
                     through what channel, and what the first response step is.",
        remediation: Some(
            "Document an incident response plan, wire its alerts to an on-call channel \
             and exercise it against a simulated event.",
        ),
    },
    AwsManagedRequirement {
        name: "input-validation-and-output-encoding",
        description: "Ensure untrusted input is validated before it is acted on.",
        domain: "ApplicationSecurity",
        evaluation: "Check that every entry point validates its input against a declared \
                     schema and encodes what it renders or passes on.",
        remediation: Some(
            "Validate each request against the interface's declared schema, reject what \
             does not match and encode every value on the way out.",
        ),
    },
    AwsManagedRequirement {
        name: "least-privilege-access-control",
        description: "Ensure identities hold only the permissions they use.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check each policy the system grants against the actions the \
                     identity actually calls, and that no grant is unscoped.",
        remediation: Some(
            "Scope every policy to the actions and resources in use and remove grants no \
             identity exercises.",
        ),
    },
    AwsManagedRequirement {
        name: "network-exposure-minimization",
        description: "Ensure only the network paths the system needs are reachable.",
        domain: "NetworkSecurity",
        evaluation: "Check every ingress rule and public endpoint against the paths the \
                     system documents as required.",
        remediation: Some(
            "Remove ingress rules and public endpoints no documented path uses, and \
             place the rest behind a private network boundary.",
        ),
    },
    AwsManagedRequirement {
        name: "secrets-management-outside-source",
        description: "Ensure secrets are never stored in source or configuration.",
        domain: "DataProtection",
        evaluation: "Check the repository, build configuration and container images for \
                     credential material, and that each secret is read from a managed \
                     secret store at run time.",
        remediation: Some(
            "Move every secret into a managed secret store, read it at run time and \
             revoke whatever was committed.",
        ),
    },
];

/// The requirements the NIST CSF pack ships with: one per function and category of the
/// NIST Cybersecurity Framework the pack tracks, so the `domain` of each entry is the
/// framework's own function name.
pub const NIST_CSF_PACK_REQUIREMENTS: [AwsManagedRequirement; 12] = [
    AwsManagedRequirement {
        name: "adverse-event-analysis-and-threat-intelligence-integration",
        description: "Analyze adverse events against threat intelligence to \
                      characterize incidents.",
        domain: "Detect",
        evaluation: "Check that detected adverse events are correlated with a threat \
                     intelligence source and that each correlation reaches an analyst or \
                     an automated responder.",
        remediation: Some(
            "Subscribe the detection pipeline to a threat intelligence feed and record \
             the characterization each adverse event received.",
        ),
    },
    AwsManagedRequirement {
        name: "ai-component-risk-assessment-and-security-integration",
        description: "Assess the risk of AI components and integrate them into the \
                      security program.",
        domain: "Identify",
        evaluation: "Check that every AI component the system uses is inventoried with \
                     its data sources, its risk assessment and the controls applied to \
                     it.",
        remediation: Some(
            "Inventory each AI component, assess the risk of its data and outputs, and \
             bring it under the same controls as the rest of the system.",
        ),
    },
    AwsManagedRequirement {
        name: "asset-inventory-and-classification-management",
        description: "Maintain an inventory of assets and classify them by criticality.",
        domain: "Identify",
        evaluation: "Check that every hardware, software, service and data asset is in \
                     the inventory and carries a classification and an owner.",
        remediation: Some(
            "Discover the assets in use, record each one with an owner and a \
             classification, and reconcile the inventory on a schedule.",
        ),
    },
    AwsManagedRequirement {
        name: "continuous-security-monitoring-coverage",
        description: "Monitor assets continuously to find anomalies and indicators of \
                      compromise.",
        domain: "Detect",
        evaluation: "Check that every inventoried asset is covered by a monitoring \
                     source and that gaps in coverage are reported.",
        remediation: Some(
            "Extend monitoring to each uncovered asset and alert on the absence of \
             telemetry as well as on its content.",
        ),
    },
    AwsManagedRequirement {
        name: "cybersecurity-risk-management-strategy",
        description: "Establish and communicate a cybersecurity risk management \
                      strategy.",
        domain: "Govern",
        evaluation: "Check that the risk strategy states the organization's risk \
                     appetite, names the roles accountable for it and is reviewed.",
        remediation: Some(
            "Publish a risk management strategy with named owners and review it on a \
             documented cadence.",
        ),
    },
    AwsManagedRequirement {
        name: "identity-authentication-and-access-management",
        description: "Authenticate identities and authorize access proportionate to \
                      risk.",
        domain: "Protect",
        evaluation: "Check that every identity authenticates with a factor proportionate \
                     to the risk of what it can reach, and that its authorizations are \
                     reviewed.",
        remediation: Some(
            "Require multi-factor authentication for privileged identities and review \
             each identity's authorizations on a schedule.",
        ),
    },
    AwsManagedRequirement {
        name: "incident-response-execution-and-reporting",
        description: "Execute the incident response plan and report incidents to \
                      stakeholders.",
        domain: "Respond",
        evaluation: "Check that declared incidents have a recorded response, a \
                     containment action and a report to the stakeholders the plan names.",
        remediation: Some(
            "Record the response taken for each incident and deliver the report the plan \
             requires to its named recipients.",
        ),
    },
    AwsManagedRequirement {
        name: "platform-and-technology-infrastructure-resilience",
        description: "Protect technology infrastructure so it resists and withstands \
                      adverse events.",
        domain: "Protect",
        evaluation: "Check that platforms are hardened to a documented baseline and that \
                     the system tolerates the loss of any single component.",
        remediation: Some(
            "Apply the hardening baseline to every platform and remove single points of \
             failure from the architecture.",
        ),
    },
    AwsManagedRequirement {
        name: "recovery-plan-execution-and-validation",
        description: "Execute and validate recovery plans to restore assets after an \
                      incident.",
        domain: "Recover",
        evaluation: "Check that a recovery plan exists for each critical asset, that it \
                     has been exercised and that the restored state was verified.",
        remediation: Some(
            "Exercise the recovery plan of each critical asset and record the \
             verification of what the exercise restored.",
        ),
    },
    AwsManagedRequirement {
        name: "supply-chain-risk-management-requirements",
        description: "Manage cybersecurity risk arising from suppliers and third \
                      parties.",
        domain: "Govern",
        evaluation: "Check that each supplier with access to systems or data has a risk \
                     assessment and contractual security requirements.",
        remediation: Some(
            "Assess every supplier's access, record the security requirements it is held \
             to and review them when the relationship changes.",
        ),
    },
    AwsManagedRequirement {
        name: "technology-asset-vulnerability-identification",
        description: "Identify vulnerabilities in assets and record their risk.",
        domain: "Identify",
        evaluation: "Check that assets are scanned for vulnerabilities on a schedule and \
                     that each finding carries a risk rating and an owner.",
        remediation: Some(
            "Scan every asset on a schedule, rate each finding and assign it to the \
             owner the inventory names.",
        ),
    },
    AwsManagedRequirement {
        name: "workforce-cybersecurity-awareness-and-training",
        description: "Train personnel so they perform their cybersecurity \
                      responsibilities.",
        domain: "Protect",
        evaluation: "Check that every role with security responsibilities has completed \
                     training for them and that the training is repeated.",
        remediation: Some(
            "Assign role-appropriate security training, track completion and repeat it \
             on a documented cadence.",
        ),
    },
];

/// The requirements the PCI DSS pack ships with: one per requirement group of PCI DSS
/// v4.0.1, so the `domain` of each entry is the group that requirement belongs to.
pub const PCI_DSS_PACK_REQUIREMENTS: [AwsManagedRequirement; 12] = [
    AwsManagedRequirement {
        name: "account-data-classification-and-retention-controls",
        description: "Protect stored account data and keep it no longer than needed.",
        domain: "ProtectAccountData",
        evaluation: "Check that stored account data is classified, encrypted and deleted \
                     once its documented retention period ends.",
        remediation: Some(
            "Classify every store holding account data, encrypt it and delete the data \
             the retention policy no longer covers.",
        ),
    },
    AwsManagedRequirement {
        name: "anti-malware-protection-for-workload-components",
        description: "Protect workload components against malicious software.",
        domain: "MaintainVulnerabilityManagement",
        evaluation: "Check that components at risk of malware run an anti-malware \
                     mechanism that is current and cannot be disabled by a user.",
        remediation: Some(
            "Deploy an anti-malware mechanism to each component at risk, keep its \
             definitions current and prevent users from disabling it.",
        ),
    },
    AwsManagedRequirement {
        name: "cardholder-data-transmission-encryption",
        description: "Encrypt account data transmitted across open public networks.",
        domain: "ProtectAccountData",
        evaluation: "Check that every path carrying account data over a public network \
                     negotiates strong cryptography and trusted certificates.",
        remediation: Some(
            "Require strong TLS with trusted certificates on every path that carries \
             account data, and reject the rest.",
        ),
    },
    AwsManagedRequirement {
        name: "information-security-policy-and-program-governance",
        description: "Maintain an information security policy and the program that \
                      enforces it.",
        domain: "InformationSecurityPolicy",
        evaluation: "Check that the security policy is published, assigned to a named \
                     owner, acknowledged by personnel and reviewed at least annually.",
        remediation: Some(
            "Publish the security policy with a named owner, record personnel \
             acknowledgement and review it annually.",
        ),
    },
    AwsManagedRequirement {
        name: "logging-and-monitoring-of-all-access",
        description: "Log and monitor all access to system components and account data.",
        domain: "MonitorAndTestNetworks",
        evaluation: "Check that access to system components and account data produces an \
                     audit record, that the records are time-synchronized and that they \
                     are reviewed.",
        remediation: Some(
            "Enable audit logging on every component, synchronize its clock to a trusted \
             source and review the records daily.",
        ),
    },
    AwsManagedRequirement {
        name: "multi-factor-authentication-for-administrative-access",
        description: "Authenticate users and require multiple factors for \
                      administrative access.",
        domain: "ImplementStrongAccessControl",
        evaluation: "Check that each user is uniquely identified and that every \
                     administrative or remote session requires more than one \
                     authentication factor.",
        remediation: Some(
            "Give each user a unique identity and require multi-factor authentication \
             for administrative and remote access.",
        ),
    },
    AwsManagedRequirement {
        name: "network-security-controls-between-trust-zones",
        description: "Install network security controls between trust zones.",
        domain: "BuildAndMaintainSecureNetworks",
        evaluation: "Check that traffic between the account data environment and any \
                     other network passes a control whose rules are documented and \
                     reviewed.",
        remediation: Some(
            "Place a network security control on every boundary of the account data \
             environment and review its rules on a schedule.",
        ),
    },
    AwsManagedRequirement {
        name: "physical-and-logical-media-access-restriction",
        description: "Restrict physical and logical access to media holding account \
                      data.",
        domain: "ImplementStrongAccessControl",
        evaluation: "Check that media holding account data is inventoried, access to it \
                     is authorized and its destruction is recorded.",
        remediation: Some(
            "Inventory media holding account data, authorize each access to it and \
             record the destruction of what is retired.",
        ),
    },
    AwsManagedRequirement {
        name: "restrict-access-to-system-components-by-business-need",
        description: "Restrict access to system components and account data by business \
                      need to know.",
        domain: "ImplementStrongAccessControl",
        evaluation: "Check that each role's access is defined by its business need, \
                     denied by default and reviewed at least every six months.",
        remediation: Some(
            "Define access per role from its business need, default the rest to deny and \
             review the grants semi-annually.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-configuration-of-system-components",
        description: "Apply secure configurations to all system components.",
        domain: "BuildAndMaintainSecureNetworks",
        evaluation: "Check that components are deployed from a hardening baseline, with \
                     vendor defaults and unnecessary services removed.",
        remediation: Some(
            "Deploy each component from the hardening baseline, change vendor default \
             credentials and disable the services it does not need.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-software-development-lifecycle-controls",
        description: "Develop and maintain secure systems and software.",
        domain: "MaintainVulnerabilityManagement",
        evaluation: "Check that changes pass code review and security testing before \
                     release and that known vulnerabilities are patched within their \
                     documented window.",
        remediation: Some(
            "Require review and security testing on every change, and patch known \
             vulnerabilities within the documented window.",
        ),
    },
    AwsManagedRequirement {
        name: "security-testing-and-vulnerability-scanning",
        description: "Test the security of systems and networks regularly.",
        domain: "MonitorAndTestNetworks",
        evaluation: "Check that internal and external vulnerability scans and \
                     penetration tests run on schedule and that their findings are \
                     resolved.",
        remediation: Some(
            "Schedule internal and external scans and penetration tests, and track each \
             finding to resolution.",
        ),
    },
];

/// The requirements the Well-Architected pack ships with: one per best-practice area of
/// the AWS Well-Architected Framework's Security Pillar, so the `domain` of each entry is
/// that pillar area.
pub const WELL_ARCHITECTED_PACK_REQUIREMENTS: [AwsManagedRequirement; 10] = [
    AwsManagedRequirement {
        name: "authenticated-network-communication-architecture",
        description: "Authenticate network communications between workload components.",
        domain: "InfrastructureProtection",
        evaluation: "Check that traffic between components is mutually authenticated or \
                     carries a service identity the receiver verifies.",
        remediation: Some(
            "Terminate component-to-component traffic on an authenticated channel and \
             verify the caller's service identity before serving it.",
        ),
    },
    AwsManagedRequirement {
        name: "automated-certificate-management-architecture",
        description: "Issue, distribute and rotate certificates automatically.",
        domain: "DataProtection",
        evaluation: "Check that every certificate the workload serves is issued by a \
                     managed authority and renewed without manual action before it \
                     expires.",
        remediation: Some(
            "Issue workload certificates from a managed certificate authority and \
             automate their renewal and distribution.",
        ),
    },
    AwsManagedRequirement {
        name: "centralized-identity-provider-federation",
        description: "Federate human access to a single centralized identity provider.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check that human access is federated from one identity provider and \
                     that no account holds a local, long-lived human credential.",
        remediation: Some(
            "Federate human access through the central identity provider and delete \
             local human credentials.",
        ),
    },
    AwsManagedRequirement {
        name: "data-classification-and-lifecycle-architecture",
        description: "Classify data and apply controls matched to each classification.",
        domain: "DataProtection",
        evaluation: "Check that each data store carries a classification and that its \
                     retention, access and encryption controls match that \
                     classification.",
        remediation: Some(
            "Classify every data store and align its encryption, access and retention \
             controls with that classification.",
        ),
    },
    AwsManagedRequirement {
        name: "defense-in-depth-network-layer-segmentation",
        description: "Segment the network into layers so one breach does not reach the \
                      whole workload.",
        domain: "InfrastructureProtection",
        evaluation: "Check that components are grouped into network layers whose traffic \
                     is controlled between them, not only at the workload's edge.",
        remediation: Some(
            "Group components into network layers and control the traffic between \
             layers, not only at the edge.",
        ),
    },
    AwsManagedRequirement {
        name: "detective-control-coverage-and-alerting",
        description: "Configure detective controls and route their findings to an \
                      investigator.",
        domain: "Detection",
        evaluation: "Check that detective controls are enabled in every account and \
                     region in use and that their findings reach an owner who acts on \
                     them.",
        remediation: Some(
            "Enable the detective controls in each account and region in use and route \
             their findings to a monitored destination.",
        ),
    },
    AwsManagedRequirement {
        name: "incident-response-automation-and-runbooks",
        description: "Prepare runbooks and automation so incidents are answered \
                      predictably.",
        domain: "IncidentResponse",
        evaluation: "Check that each anticipated incident type has a runbook, the access \
                     its responders need and an automation that performs its \
                     containment step.",
        remediation: Some(
            "Write a runbook per incident type, pre-provision responder access and \
             automate the containment step.",
        ),
    },
    AwsManagedRequirement {
        name: "key-management-and-envelope-encryption-design",
        description: "Manage cryptographic keys and encrypt data with envelope \
                      encryption.",
        domain: "DataProtection",
        evaluation: "Check that data keys are wrapped by a key the account manages, that \
                     key policies scope their use and that rotation is enabled.",
        remediation: Some(
            "Wrap data keys with a customer managed key, scope the key policy to the \
             principals that use it and enable rotation.",
        ),
    },
    AwsManagedRequirement {
        name: "least-privilege-permission-boundaries",
        description: "Grant least privilege and enforce it with permission boundaries.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check that identities are granted only the actions they use and \
                     that a boundary or guardrail prevents a grant beyond it.",
        remediation: Some(
            "Reduce each grant to the actions in use and attach a permission boundary \
             that caps what any grant can add.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-software-delivery-pipeline-controls",
        description: "Secure the pipeline that builds and deploys the workload.",
        domain: "ApplicationSecurity",
        evaluation: "Check that the pipeline builds from reviewed source, scans its \
                     artifacts and deploys with a role scoped to that deployment.",
        remediation: Some(
            "Require review on source, scan every artifact in the pipeline and scope the \
             deployment role to the resources it changes.",
        ),
    },
];
