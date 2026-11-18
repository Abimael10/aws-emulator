// SPDX-License-Identifier: CC0-1.0

//! The requirement content the PCI DSS pack ships with: the workload requirements the pack
//! derives from PCI DSS v4.0.1, one table entry per requirement the pack defines, with the
//! standard's requirement GROUP as the entry's `domain`.
//!
//! Each `description` is the requirement's published statement — what must be in place and
//! what the design has to demonstrate — because an assessment driven from this pack is read
//! from `description`, and a summary line in place of the control statement would pass a
//! scope it never checked.
//!
//! WHICH TEXT IS WHICH: the pack's requirement NAMES and the count of them are recorded —
//! they are what a fresh account's `ListSecurityRequirements` reports for this pack. Of the
//! statement text, the part the corpus is known to state is written here as it states it
//! (the classification, post-authorization retention and unreadable-PAN clauses of the
//! account-data requirement, and the anti-malware clauses), and the rest is written to the
//! published structure of the requirement group each entry belongs to. The `domain`,
//! `evaluation` and `remediation` beside each statement are the same: written to that
//! structure, not recorded. A later recording of the exact wording replaces what is here
//! rather than being judged equal to it.

use super::AwsManagedRequirement;

/// The requirements the PCI DSS pack ships with, complete: the fifty requirements the pack
/// defines, in name order.
pub const PCI_DSS_PACK_REQUIREMENTS: [AwsManagedRequirement; 50] = [
    AwsManagedRequirement {
        name: "account-data-classification-and-retention-controls",
        description: "All stored account data must be identified and classified into \
                      cardholder data (CHD) and sensitive authentication data (SAD) \
                      elements, and kept only for as long as a documented business need \
                      requires. Sensitive authentication data must not be retained after \
                      authorization completes, even when it is encrypted, and a stored \
                      primary account number must be rendered unreadable wherever it is \
                      held. The retention period for each element, the data it covers and \
                      the process that deletes or renders unrecoverable the data no longer \
                      needed must be documented and enforced.",
        domain: "ProtectAccountData",
        evaluation: "Check that stored account data is classified into CHD and SAD \
                     elements, that no SAD persists after authorization, that stored PANs \
                     are unreadable, and that the retention process deletes what is no \
                     longer needed.",
        remediation: Some(
            "Classify every store holding account data, remove sensitive authentication \
             data once authorization completes, render stored PANs unreadable, and run \
             the deletion the retention policy requires.",
        ),
    },
    AwsManagedRequirement {
        name: "anti-malware-protection-for-workload-components",
        description: "Workload components must be protected against malicious software by \
                      a mechanism matched to the risk each component carries. The design \
                      must identify the components at risk, the anti-malware or equivalent \
                      mechanism protecting each, how its detection content is kept \
                      current, how periodic and on-access scanning is performed, and how a \
                      user or process is prevented from disabling or altering it. A \
                      component judged not to be at risk must carry a documented \
                      justification that is reviewed periodically.",
        domain: "MaintainVulnerabilityManagement",
        evaluation: "Check that components at risk run a current anti-malware mechanism \
                     that scans periodically and on access and cannot be disabled, and \
                     that the components excluded carry a reviewed justification.",
        remediation: Some(
            "Deploy an anti-malware mechanism to each component at risk, keep its \
             detection content current, prevent it being disabled, and document and \
             review every exclusion.",
        ),
    },
    AwsManagedRequirement {
        name: "anti-phishing-controls-for-workload-communications",
        description: "Communications the workload sends and receives must be protected \
                      against phishing and impersonation. The design must show the \
                      mechanisms that authenticate the workload's own outbound messages \
                      and domains, the controls that detect and block phishing reaching \
                      its users and operators, and the reporting path a suspected phishing \
                      attempt follows.",
        domain: "MaintainVulnerabilityManagement",
        evaluation: "Check that outbound messages and domains are authenticated, that \
                     inbound phishing is detected and blocked, and that a reporting path \
                     for a suspected attempt exists.",
        remediation: Some(
            "Authenticate the workload's outbound mail and domains, deploy phishing \
             detection on inbound communications, and publish the reporting path.",
        ),
    },
    AwsManagedRequirement {
        name: "application-and-system-account-security-management",
        description: "Accounts used by applications and systems rather than by people must \
                      be managed with the same rigour as user accounts. The design must \
                      inventory each application or system account, the function it serves, \
                      the privileges it holds and the interactive use it is prevented \
                      from, and must state how its credentials are protected, rotated and \
                      reviewed.",
        domain: "ImplementStrongAccessControl",
        evaluation: "Check that every application and system account is inventoried with \
                     its function and privileges, is prevented from interactive use, and \
                     has its credentials protected and rotated.",
        remediation: Some(
            "Inventory the application and system accounts, scope each to its function, \
             block interactive use, and protect and rotate their credentials.",
        ),
    },
    AwsManagedRequirement {
        name: "audit-log-protection-integrity-and-retention",
        description: "Audit logs must be protected from alteration and retained for the \
                      period an investigation needs. The design must show that log records \
                      are write-protected against the identities whose activity they \
                      record, that any change to them is detectable, that they are \
                      forwarded or backed up to a destination outside the system producing \
                      them, and that both the full retention period and the portion kept \
                      immediately available are met.",
        domain: "MonitorAndTestNetworks",
        evaluation: "Check that audit records cannot be altered by the identities they \
                     record, that alteration is detectable, and that the retention period \
                     and the immediately available window are both met.",
        remediation: Some(
            "Forward audit records to a destination the logged system cannot modify, \
             enable change detection on them, and set the retention the investigation \
             window requires.",
        ),
    },
    AwsManagedRequirement {
        name: "automated-audit-log-review-and-anomaly-detection",
        description: "Audit logs must be reviewed by automated mechanisms rather than only \
                      by hand, so that anomalies and suspicious activity are surfaced \
                      promptly. The design must state which log sources are reviewed \
                      automatically, the activity the review looks for, how often it runs, \
                      and how a finding is escalated to a person for action.",
        domain: "MonitorAndTestNetworks",
        evaluation: "Check that log sources are reviewed by an automated mechanism on a \
                     stated frequency and that its findings are escalated to a person.",
        remediation: Some(
            "Put the audit log sources under automated review, define the activity it \
             looks for, and route its findings to an owner.",
        ),
    },
    AwsManagedRequirement {
        name: "cardholder-data-encryption-scope-isolation",
        description: "Where cryptography is used to render account data unreadable, the \
                      design must show that the scope it covers is complete and isolated \
                      from the keys that would reverse it. The design must identify every \
                      location account data is stored, the cryptography applied to each, \
                      and the separation between the stored ciphertext and the key \
                      management function, so that compromising the data store does not \
                      also yield the means to decrypt it.",
        domain: "ProtectAccountData",
        evaluation: "Check that every store of account data is covered by cryptography and \
                     that the keys reversing it are held under a separate access path \
                     from the data.",
        remediation: Some(
            "Encrypt every store of account data and move the keys to a key management \
             service whose access path is separate from the data store's.",
        ),
    },
    AwsManagedRequirement {
        name: "cde-network-segmentation-and-scope-minimization",
        description: "The cardholder data environment must be segmented from the rest of \
                      the network so that its assessment scope is as small as it can be. \
                      The design must document the segmentation controls, the systems \
                      inside the resulting boundary, the systems that connect to it, and \
                      the validation that confirms systems outside the boundary cannot \
                      reach the environment.",
        domain: "BuildAndMaintainSecureNetworks",
        evaluation: "Check that the segmentation controls and the systems inside and \
                     connected to the boundary are documented, and that the isolation of \
                     out-of-scope systems is validated.",
        remediation: Some(
            "Segment the cardholder data environment, document what is inside and \
             connected to it, and validate that out-of-scope systems cannot reach it.",
        ),
    },
    AwsManagedRequirement {
        name: "cde-network-segmentation-and-traffic-control",
        description: "Traffic crossing the cardholder data environment boundary must be \
                      controlled explicitly in both directions. The design must document \
                      the permitted flows with their business justification, show that \
                      inbound and outbound traffic is restricted to those flows and denied \
                      by default, and state how the rule sets are reviewed and kept in \
                      line with the documented flows.",
        domain: "BuildAndMaintainSecureNetworks",
        evaluation: "Check that inbound and outbound traffic at the boundary is limited to \
                     documented, justified flows, that the default is deny, and that the \
                     rules are reviewed against the documentation.",
        remediation: Some(
            "Restrict traffic at the boundary to the documented flows in both directions, \
             default the rest to deny, and review the rule sets against the \
             documentation.",
        ),
    },
    AwsManagedRequirement {
        name: "cde-scope-documentation-and-data-flow-validation",
        description: "The scope of the cardholder data environment must be documented and \
                      validated against how account data actually flows. The design must \
                      record the people, processes and technology in scope, the account \
                      data flows between them, and the periodic validation that confirms \
                      no undocumented flow or store exists.",
        domain: "InformationSecurityPolicy",
        evaluation: "Check that the scope record covers people, processes and technology \
                     with their account data flows, and that a periodic validation looks \
                     for undocumented flows and stores.",
        remediation: Some(
            "Document the scope and its account data flows, and run a periodic validation \
             that searches for flows and stores the documentation does not name.",
        ),
    },
    AwsManagedRequirement {
        name: "comprehensive-audit-logging-for-sensitive-data-access",
        description: "All access to system components and to account data must produce an \
                      audit record sufficient to reconstruct who did what. The design must \
                      state the events it records, and each record must identify the user \
                      or process, the type of event, the date and time, whether it \
                      succeeded, its origin, and the data or component affected.",
        domain: "MonitorAndTestNetworks",
        evaluation: "Check that access to components and account data is logged and that \
                     each record carries the identity, event type, timestamp, outcome, \
                     origin and affected resource.",
        remediation: Some(
            "Log every access to system components and account data with the identity, \
             event type, timestamp, outcome, origin and affected resource.",
        ),
    },
    AwsManagedRequirement {
        name: "continuous-security-control-monitoring-and-failure-response",
        description: "Critical security controls must be monitored so that their failure \
                      is detected and answered promptly. The design must list the controls \
                      under monitoring, how a failure of each is detected, who is alerted, \
                      and the actions taken while a control is failed — restoring it, \
                      identifying the cause, and recording what exposure occurred in the \
                      interim.",
        domain: "MonitorAndTestNetworks",
        evaluation: "Check that critical controls are monitored for failure, that failures \
                     alert an owner, and that the response records the cause and the \
                     exposure during the outage.",
        remediation: Some(
            "Monitor each critical control for failure, alert an owner when one fails, and \
             record the cause and the exposure until it is restored.",
        ),
    },
    AwsManagedRequirement {
        name: "cryptographic-agility-and-technology-lifecycle-risk",
        description: "The workload must be able to change the cryptography it relies on \
                      before that cryptography becomes untrusted. The design must inventory \
                      the algorithms, key sizes, protocols and certificate authorities in \
                      use, track industry guidance and vendor support for each, and show \
                      how a primitive can be replaced without redesigning the components \
                      that depend on it.",
        domain: "ProtectAccountData",
        evaluation: "Check that the cryptography in use is inventoried and tracked against \
                     current guidance, and that a primitive can be replaced without \
                     redesigning its dependants.",
        remediation: Some(
            "Inventory the cryptography in use, track it against current guidance, and \
             isolate it behind an interface so a primitive can be replaced.",
        ),
    },
    AwsManagedRequirement {
        name: "cryptographic-key-management-for-stored-account-data",
        description: "Keys protecting stored account data must be managed over their whole \
                      lifecycle with strict access control. The design must state how keys \
                      are generated, distributed, stored, rotated, retired and destroyed, \
                      how key-encrypting keys are kept separate from the data keys they \
                      protect, how access to clear-text keys is limited to the fewest \
                      custodians necessary, and how each custodian's responsibility is \
                      acknowledged.",
        domain: "ProtectAccountData",
        evaluation: "Check that the key lifecycle is defined end to end, that \
                     key-encrypting keys are stored apart from the data keys, and that \
                     clear-text key access is limited to acknowledged custodians.",
        remediation: Some(
            "Define the key lifecycle from generation to destruction, separate \
             key-encrypting keys from data keys, and limit clear-text key access to \
             acknowledged custodians.",
        ),
    },
    AwsManagedRequirement {
        name: "documented-security-control-rationale-and-objectives",
        description: "Each security control the workload implements must have a recorded \
                      objective and rationale, so that a reviewer can judge whether it \
                      meets its intent. The design must state what each control is intended \
                      to achieve, how its effectiveness is measured, and the evidence it \
                      produces to show that it is operating as intended.",
        domain: "InformationSecurityPolicy",
        evaluation: "Check that each control records its objective, its effectiveness \
                     measure and the evidence it produces.",
        remediation: Some(
            "Record the objective, the effectiveness measure and the evidence for every \
             control the workload implements.",
        ),
    },
    AwsManagedRequirement {
        name: "incident-response-and-recovery-workflow",
        description: "The workload must have an incident response and recovery workflow \
                      that is documented, assigned and exercised. The design must state \
                      the roles and responsibilities, the notification and escalation \
                      path, the containment and eradication steps, the recovery and \
                      verification steps, and the review that feeds the result back into \
                      the plan.",
        domain: "InformationSecurityPolicy",
        evaluation: "Check that the workflow names roles, notification and escalation, \
                     containment, recovery and verification steps, and that it is \
                     exercised and reviewed.",
        remediation: Some(
            "Document the incident response and recovery workflow with its roles and \
             steps, exercise it, and feed each review back into the plan.",
        ),
    },
    AwsManagedRequirement {
        name: "intrusion-detection-file-integrity-and-page-tamper",
        description: "The workload must detect intrusion and unauthorized modification of \
                      the components and the content it serves. The design must show \
                      network intrusion detection or prevention at the points traffic \
                      enters, change detection on critical files and configuration, and \
                      detection of unauthorized modification of the content delivered to a \
                      user's browser, with each detection routed to an owner for action.",
        domain: "MonitorAndTestNetworks",
        evaluation: "Check that intrusion detection covers the entry points, that critical \
                     file and configuration changes are detected, that payment page \
                     tampering is detected, and that each detection reaches an owner.",
        remediation: Some(
            "Deploy intrusion detection at the traffic entry points, change detection on \
             critical files, and tamper detection on delivered content, and route the \
             alerts to an owner.",
        ),
    },
    AwsManagedRequirement {
        name: "least-privilege-access-control-with-default-deny",
        description: "Access to system components and account data must be granted on \
                      least privilege with a default-deny position. The design must define \
                      access by role and business need, grant only the privileges that role \
                      exercises, deny everything not explicitly allowed, and show the \
                      approval each grant required and the review that confirms it is still \
                      needed.",
        domain: "ImplementStrongAccessControl",
        evaluation: "Check that access is defined per role from business need, that \
                     anything not allowed is denied, and that each grant carries an \
                     approval and a periodic review.",
        remediation: Some(
            "Define access per role from business need, default the rest to deny, and \
             record the approval and the review of every grant.",
        ),
    },
    AwsManagedRequirement {
        name: "mfa-for-cardholder-data-environment-access",
        description: "All access into the cardholder data environment must require more \
                      than one authentication factor. The design must show multi-factor \
                      authentication for administrative access, for remote access from \
                      outside the network and for personnel access into the environment, \
                      with the factors independent of each other and the mechanism \
                      resistant to replay.",
        domain: "ImplementStrongAccessControl",
        evaluation: "Check that administrative, remote and personnel access into the \
                     environment all require independent factors and that the mechanism \
                     resists replay.",
        remediation: Some(
            "Require multi-factor authentication on every path into the cardholder data \
             environment, with independent factors and replay-resistant verification.",
        ),
    },
    AwsManagedRequirement {
        name: "multi-tenant-isolation-and-per-tenant-security",
        description: "Where the workload serves multiple tenants, each tenant's data and \
                      processes must be isolated from the others and able to be assessed on \
                      their own. The design must state the isolation boundary, the \
                      mechanisms enforcing it at the compute, network, storage and identity \
                      layers, and the means by which a tenant obtains evidence about its \
                      own environment without reaching another tenant's.",
        domain: "BuildAndMaintainSecureNetworks",
        evaluation: "Check that the tenant isolation boundary is enforced at every layer \
                     and that a tenant can obtain evidence about its own environment \
                     only.",
        remediation: Some(
            "Enforce the tenant boundary at the compute, network, storage and identity \
             layers, and scope each tenant's evidence to its own environment.",
        ),
    },
    AwsManagedRequirement {
        name: "network-architecture-and-data-flow-documentation",
        description: "The workload must maintain current diagrams of its network \
                      architecture and of every account data flow. The design must show all \
                      connections between the cardholder data environment and other \
                      networks, identify each system in scope, and state how the diagrams \
                      are reviewed and updated when the environment changes.",
        domain: "BuildAndMaintainSecureNetworks",
        evaluation: "Check that network and data flow diagrams exist, cover every \
                     connection into the environment, and are updated when it changes.",
        remediation: Some(
            "Produce network and account data flow diagrams covering every connection into \
             the environment, and update them with each change.",
        ),
    },
    AwsManagedRequirement {
        name: "network-config-security-and-internal-address-protection",
        description: "Network configuration must be secured and internal addressing must \
                      not be disclosed to untrusted networks. The design must show that \
                      configuration files and rule sets are protected and change \
                      controlled, that internal addresses and routing information are not \
                      published to untrusted parties, and that any disclosure that is \
                      authorized is documented.",
        domain: "BuildAndMaintainSecureNetworks",
        evaluation: "Check that network configuration is access controlled and change \
                     controlled, and that internal addressing is not disclosed to \
                     untrusted networks without documented authorization.",
        remediation: Some(
            "Protect and change-control the network configuration, and stop internal \
             addressing and routing information reaching untrusted networks.",
        ),
    },
    AwsManagedRequirement {
        name: "network-security-controls-for-traffic-segmentation",
        description: "Network security controls must separate trusted from untrusted \
                      networks and restrict traffic between segments to what is required. \
                      The design must document the segments, the controls between them, the \
                      permitted ports, protocols and directions with their justification, \
                      and the periodic review of those rules.",
        domain: "BuildAndMaintainSecureNetworks",
        evaluation: "Check that segments are separated by a control whose permitted ports, \
                     protocols and directions are documented, justified and reviewed.",
        remediation: Some(
            "Place a network security control between each pair of segments and restrict \
             it to the documented, justified ports and directions.",
        ),
    },
    AwsManagedRequirement {
        name: "pan-display-masking-and-storage-protection",
        description: "The primary account number must be masked when displayed and \
                      rendered unreadable wherever it is stored. The design must state the \
                      maximum digits each role is permitted to see, the legitimate business \
                      need justifying any broader view, and the mechanism — strong one-way \
                      hashing with a salt, truncation, tokenization or strong cryptography \
                      — that protects the stored value.",
        domain: "ProtectAccountData",
        evaluation: "Check that displayed PANs are masked to the digits each role may see \
                     and that stored PANs are hashed, truncated, tokenized or encrypted.",
        remediation: Some(
            "Mask displayed PANs to the digits each role needs and render stored PANs \
             unreadable by hashing, truncation, tokenization or strong cryptography.",
        ),
    },
    AwsManagedRequirement {
        name: "pci-dss-scope-documentation-and-change-impact-assessment",
        description: "The scope of the environment subject to the standard must be \
                      documented, confirmed periodically and reassessed whenever it \
                      changes. The design must record the scoping decision and its basis, \
                      and must show that a change to systems, data flows or third parties \
                      triggers an assessment of its impact on the scope and on the \
                      controls inside it.",
        domain: "InformationSecurityPolicy",
        evaluation: "Check that the scope and its basis are recorded, confirmed \
                     periodically, and reassessed when systems, flows or third parties \
                     change.",
        remediation: Some(
            "Record the scope and its basis, confirm it periodically, and make a change to \
             systems, flows or third parties trigger a scope impact assessment.",
        ),
    },
    AwsManagedRequirement {
        name: "penetration-testing-readiness-and-remediation",
        description: "The workload must be penetration tested on a defined schedule and \
                      after significant change, and the findings must be remediated and \
                      retested. The design must state the scope and method of the test, the \
                      independence of the tester, the correction of exploitable findings, \
                      and the retest that confirms the correction.",
        domain: "MonitorAndTestNetworks",
        evaluation: "Check that penetration tests run on schedule and after significant \
                     change by an independent tester, and that exploitable findings are \
                     corrected and retested.",
        remediation: Some(
            "Schedule independent penetration tests, repeat them after significant change, \
             and correct and retest every exploitable finding.",
        ),
    },
    AwsManagedRequirement {
        name: "poi-device-inventory-and-integrity-management",
        description: "Devices that capture payment card data through physical interaction \
                      must be inventoried and protected from tampering and substitution. \
                      The design must record each device with its location and serial \
                      number, the periodic inspection that detects tampering, the training \
                      given to the personnel who perform it, and the response when a device \
                      fails inspection.",
        domain: "ImplementStrongAccessControl",
        evaluation: "Check that interaction devices are inventoried with location and \
                     serial number, inspected periodically by trained personnel, and \
                     removed from service when they fail inspection.",
        remediation: Some(
            "Inventory the interaction devices, inspect them periodically for tampering \
             and substitution, train the inspectors, and act on a failed inspection.",
        ),
    },
    AwsManagedRequirement {
        name: "programmatic-access-control-for-cardholder-data-stores",
        description: "Programmatic access to stores holding account data must be mediated \
                      and restricted. The design must show that applications and services \
                      reach account data through controlled interfaces under their own \
                      identities, that direct or administrative query access is limited to \
                      the fewest identities necessary, and that every such access is \
                      logged.",
        domain: "ImplementStrongAccessControl",
        evaluation: "Check that programmatic access to account data goes through \
                     controlled interfaces under per-service identities, that direct query \
                     access is minimal, and that all of it is logged.",
        remediation: Some(
            "Mediate programmatic access to account data through controlled interfaces \
             with per-service identities, minimise direct query access, and log every \
             access.",
        ),
    },
    AwsManagedRequirement {
        name: "protection-of-security-posture-artifacts",
        description: "The evidence and artifacts that demonstrate the workload's security \
                      posture must themselves be protected. The design must state where \
                      configuration baselines, scan results, logs, test reports and \
                      attestations are held, who may change them, how their integrity is \
                      assured, and how long they are retained.",
        domain: "MonitorAndTestNetworks",
        evaluation: "Check that posture artifacts are held in a controlled location with \
                     restricted write access, integrity assurance and a stated retention.",
        remediation: Some(
            "Store the posture artifacts in a controlled location, restrict who may change \
             them, assure their integrity, and set their retention.",
        ),
    },
    AwsManagedRequirement {
        name: "public-facing-web-application-attack-protection",
        description: "Public-facing web applications must be protected against the attacks \
                      they are exposed to, continuously and not only at release. The design \
                      must show either an automated technical solution that detects and \
                      prevents web-based attacks in front of the application, or a \
                      documented programme of review and correction, together with how its \
                      coverage and effectiveness are confirmed.",
        domain: "MaintainVulnerabilityManagement",
        evaluation: "Check that each public-facing web application is covered \
                     continuously by an automated attack-prevention solution or a \
                     documented review programme whose effectiveness is confirmed.",
        remediation: Some(
            "Place an automated web attack detection and prevention solution in front of \
             each public-facing application, or run and evidence the equivalent review \
             programme.",
        ),
    },
    AwsManagedRequirement {
        name: "regular-vulnerability-scanning-and-remediation",
        description: "The workload must be scanned for vulnerabilities on a defined \
                      schedule, internally and externally, and the findings resolved. The \
                      design must state the scan scope and frequency, the authentication \
                      the scans use, the risk ranking applied to findings, the remediation \
                      windows by rank, and the rescans that confirm resolution.",
        domain: "MaintainVulnerabilityManagement",
        evaluation: "Check that internal and external scans run on schedule with the \
                     stated authentication, that findings are ranked and remediated in \
                     their window, and that a rescan confirms resolution.",
        remediation: Some(
            "Run internal and external vulnerability scans on schedule, rank the findings, \
             remediate within the window for each rank, and rescan to confirm.",
        ),
    },
    AwsManagedRequirement {
        name: "sad-non-retention-after-authorization",
        description: "Sensitive authentication data must not be retained after \
                      authorization has completed, whether or not it is encrypted. The \
                      design must identify every place such data could persist — including \
                      logs, traces, caches, message queues, debugging output and backups — \
                      and show the mechanism that removes it or prevents it being written \
                      at all.",
        domain: "ProtectAccountData",
        evaluation: "Check every place sensitive authentication data could persist, \
                     including logs, traces, caches, queues and backups, and that a \
                     mechanism prevents or removes it after authorization.",
        remediation: Some(
            "Stop sensitive authentication data being written to logs, traces, caches, \
             queues and backups, and delete what authorization has already completed on.",
        ),
    },
    AwsManagedRequirement {
        name: "scheduled-security-activities-and-change-triggered-reviews",
        description: "Recurring security activities must be performed on their defined \
                      frequency, and a significant change must trigger the reviews that \
                      change affects. The design must list each activity with its frequency \
                      and owner, the evidence that it ran, and the changes that cause an \
                      activity to be performed ahead of its schedule.",
        domain: "InformationSecurityPolicy",
        evaluation: "Check that each recurring activity has a frequency, an owner and \
                     evidence that it ran, and that defined changes trigger it early.",
        remediation: Some(
            "List the recurring security activities with their frequency and owner, keep \
             the evidence each ran, and trigger them on the changes that affect them.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-change-management-and-environment-separation",
        description: "Changes must move to production through a controlled process, and \
                      production must be separated from the environments changes are \
                      developed and tested in. The design must show the approval, testing \
                      and rollback of each change, the separation of duties between those \
                      who develop and those who release, and that production account data \
                      is not used in test environments.",
        domain: "MaintainVulnerabilityManagement",
        evaluation: "Check that changes carry approval, testing and a rollback, that \
                     development and release duties are separated, and that production \
                     account data does not appear in test environments.",
        remediation: Some(
            "Require approval, testing and a rollback plan for each change, separate the \
             develop and release duties, and keep production account data out of test \
             environments.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-configuration-and-system-hardening",
        description: "System components must be configured and hardened to a defined \
                      standard before they carry traffic or data. The design must state the \
                      configuration standard applied, the removal or disabling of \
                      unnecessary services, functions, accounts and default credentials, \
                      the single primary function per component where that is required, and \
                      how drift from the standard is detected.",
        domain: "BuildAndMaintainSecureNetworks",
        evaluation: "Check that components are deployed from a hardening standard with \
                     defaults changed and unnecessary services removed, and that drift \
                     from the standard is detected.",
        remediation: Some(
            "Deploy each component from the hardening standard, change default \
             credentials, disable what it does not need, and detect drift from the \
             standard.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-data-channel-scoping-and-unintended-data-handling",
        description: "The channels the workload exchanges data over must be scoped so that \
                      account data is neither sent nor received where it was not intended. \
                      The design must identify the interfaces that may carry account data, \
                      the validation that rejects it on interfaces that must not, and the \
                      handling of data that nonetheless arrives, so that an unintended flow \
                      is detected and removed rather than silently stored.",
        domain: "ProtectAccountData",
        evaluation: "Check which interfaces may carry account data, that the others reject \
                     it, and that data arriving unintentionally is detected and removed.",
        remediation: Some(
            "Declare the interfaces permitted to carry account data, reject it elsewhere, \
             and detect and remove what arrives unintentionally.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-data-lifecycle-and-media-destruction",
        description: "Data and the media holding it must be secured through to \
                      destruction. The design must state how media is inventoried and \
                      classified, how it is protected in storage and in transfer, how data \
                      is rendered unrecoverable once it is no longer needed, and how the \
                      destruction of electronic and physical media is performed and \
                      evidenced.",
        domain: "ProtectAccountData",
        evaluation: "Check that media is inventoried and classified, protected in storage \
                     and transfer, and destroyed with evidence when its data is no longer \
                     needed.",
        remediation: Some(
            "Inventory and classify the media holding account data, protect it in storage \
             and transfer, and render its data unrecoverable on destruction with a record \
             of the act.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-development-lifecycle-and-vulnerability-prevention",
        description: "Software the workload is built from must be developed to prevent the \
                      vulnerability classes it is exposed to. The design must show the \
                      secure coding standards and the training behind them, the review and \
                      testing that detect injection, broken access control, insecure data \
                      handling and the other classes in scope, and that a change cannot be \
                      released while a known high-risk defect remains.",
        domain: "MaintainVulnerabilityManagement",
        evaluation: "Check that secure coding standards and training exist, that review \
                     and testing cover the vulnerability classes in scope, and that a \
                     high-risk defect blocks the release.",
        remediation: Some(
            "Adopt secure coding standards with training, test for the vulnerability \
             classes in scope, and block a release that carries a known high-risk defect.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-payment-software-lifecycle-and-configuration",
        description: "Payment software the workload uses must be kept at a supported, \
                      validated version and configured as its vendor's guidance requires. \
                      The design must record each payment component and its version, the \
                      vendor guidance it is configured against, the updates applied and \
                      tested, and how a component that loses validation or support is \
                      replaced.",
        domain: "MaintainVulnerabilityManagement",
        evaluation: "Check that each payment component is at a supported, validated \
                     version configured to vendor guidance, and that losing validation \
                     triggers its replacement.",
        remediation: Some(
            "Record each payment component's version and configuration against vendor \
             guidance, keep it updated, and replace one that loses validation or support.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-wireless-network-configuration-for-cde",
        description: "Wireless networks that are connected to, or can reach, the \
                      cardholder data environment must be secured and inventoried. The \
                      design must record the authorized wireless access points, replace \
                      vendor defaults, require strong authentication and encryption, detect \
                      unauthorized access points, and show that wireless traffic reaching \
                      the environment is restricted to the flows it needs.",
        domain: "BuildAndMaintainSecureNetworks",
        evaluation: "Check that authorized wireless access points are inventoried with \
                     defaults replaced and strong authentication and encryption, that \
                     rogue access points are detected, and that wireless traffic into the \
                     environment is restricted.",
        remediation: Some(
            "Inventory the authorized access points, replace their defaults, require strong \
             authentication and encryption, scan for rogue access points, and restrict \
             wireless traffic into the environment.",
        ),
    },
    AwsManagedRequirement {
        name: "session-idle-timeout-and-re-authentication",
        description: "Interactive sessions must not stay usable indefinitely. The design \
                      must state the idle period after which a session is terminated or \
                      locked and re-authentication is required, apply it to administrative \
                      and remote sessions, and show that session identifiers are \
                      invalidated server-side rather than only hidden from the user.",
        domain: "ImplementStrongAccessControl",
        evaluation: "Check that an idle timeout forces re-authentication on \
                     administrative and remote sessions and that the session identifier is \
                     invalidated server-side.",
        remediation: Some(
            "Apply an idle timeout to interactive sessions, require re-authentication \
             after it, and invalidate the session server-side.",
        ),
    },
    AwsManagedRequirement {
        name: "stored-account-data-protection-and-retention",
        description: "Account data that is stored must be protected by cryptography or an \
                      equivalent mechanism and held only while it is needed. The design \
                      must record each store, the protection applied to it, the retention \
                      period and the business justification for it, and the deletion \
                      process that runs when the period ends, including the copies held in \
                      backups and archives.",
        domain: "ProtectAccountData",
        evaluation: "Check that each store of account data records its protection, its \
                     retention period and justification, and that deletion covers backups \
                     and archives too.",
        remediation: Some(
            "Protect each store of account data, record its retention period and \
             justification, and delete the expired data from backups and archives as well.",
        ),
    },
    AwsManagedRequirement {
        name: "strong-authentication-and-credential-management",
        description: "Users and services must authenticate with strong credentials that \
                      are managed over their lifecycle. The design must state the \
                      authentication factors accepted, the strength and change requirements \
                      for secrets, how credentials are transmitted and stored protected, \
                      how initial and reset credentials are issued and forced to change, \
                      and how unused credentials are disabled.",
        domain: "ImplementStrongAccessControl",
        evaluation: "Check that the accepted factors and secret strength are stated, that \
                     credentials are protected in transit and at rest, and that initial, \
                     reset and unused credentials are handled.",
        remediation: Some(
            "Set the authentication factors and secret strength, protect credentials in \
             transit and at rest, force a change on issued credentials, and disable unused \
             ones.",
        ),
    },
    AwsManagedRequirement {
        name: "strong-cryptography-for-cardholder-data-in-transit",
        description: "Account data transmitted over open, public networks must be protected \
                      with strong cryptography and trusted keys and certificates. The \
                      design must identify each such transmission, the protocol version and \
                      cipher suites permitted, the certificate validation performed, and the \
                      rejection of connections that cannot negotiate the required \
                      protection.",
        domain: "ProtectAccountData",
        evaluation: "Check that every transmission of account data over a public network \
                     negotiates a permitted protocol and cipher with a validated \
                     certificate, and that weaker connections are refused.",
        remediation: Some(
            "Require a permitted protocol version and cipher suite with certificate \
             validation on every public transmission of account data, and refuse the \
             rest.",
        ),
    },
    AwsManagedRequirement {
        name: "strong-cryptography-for-pos-poi-terminal-connections",
        description: "Connections to point-of-sale and point-of-interaction terminals must \
                      be protected with strong cryptography appropriate to the terminal. \
                      The design must state how each terminal authenticates to the workload \
                      and the workload to it, the cryptography the channel negotiates, and \
                      the keys the terminal holds together with how they are injected, \
                      rotated and retired.",
        domain: "ProtectAccountData",
        evaluation: "Check that terminal connections are mutually authenticated with strong \
                     cryptography and that terminal keys have a defined injection, \
                     rotation and retirement path.",
        remediation: Some(
            "Mutually authenticate terminal connections with strong cryptography and define \
             how terminal keys are injected, rotated and retired.",
        ),
    },
    AwsManagedRequirement {
        name: "third-party-service-provider-security-accountability",
        description: "Service providers the workload depends on must be held accountable \
                      for the security of what they operate. The design must record each \
                      provider, the account data and access it is given, the requirements \
                      it has acknowledged in writing, the responsibilities split between it \
                      and the workload, and the monitoring of its compliance status.",
        domain: "InformationSecurityPolicy",
        evaluation: "Check that each service provider is recorded with its access, its \
                     written acknowledgement, the responsibility split and its monitored \
                     compliance status.",
        remediation: Some(
            "Record each service provider's access and written acknowledgement, agree the \
             responsibility split, and monitor its compliance status.",
        ),
    },
    AwsManagedRequirement {
        name: "time-synchronization",
        description: "System clocks across the workload must be synchronized to a common, \
                      trusted time source so that records can be correlated. The design \
                      must state the time sources used, the components permitted to change \
                      time settings, the protection of the time service from tampering, and \
                      the detection of a component whose clock has drifted outside \
                      tolerance.",
        domain: "MonitorAndTestNetworks",
        evaluation: "Check that components synchronize to a common trusted time source, \
                     that only authorized components may change time settings, and that \
                     drift outside tolerance is detected.",
        remediation: Some(
            "Synchronize every component to a common trusted time source, restrict who may \
             change time settings, and alert on clock drift.",
        ),
    },
    AwsManagedRequirement {
        name: "unauthorized-network-access-path-detection",
        description: "The workload must detect network access paths that were never \
                      authorized. The design must show how the actual connectivity of the \
                      environment is observed and compared with the documented permitted \
                      flows, how an unauthorized path, device or access point is detected, \
                      and the response when one is found.",
        domain: "MonitorAndTestNetworks",
        evaluation: "Check that observed connectivity is compared with the documented \
                     flows, that an unauthorized path or device is detected, and that the \
                     response is defined.",
        remediation: Some(
            "Observe the environment's actual connectivity, compare it with the documented \
             flows, and act on every path or device the documentation does not name.",
        ),
    },
    AwsManagedRequirement {
        name: "unique-user-identification-and-account-lifecycle",
        description: "Every user must be identified uniquely and their account managed \
                      from joining to leaving. The design must forbid shared or generic \
                      accounts for individual activity, state how an account is requested, \
                      approved, provisioned, reviewed and revoked, and show that access is \
                      removed promptly when a user's role ends.",
        domain: "ImplementStrongAccessControl",
        evaluation: "Check that each user has a unique account, that shared accounts are \
                     not used for individual activity, and that the account lifecycle ends \
                     with prompt revocation.",
        remediation: Some(
            "Give every user a unique account, remove shared accounts from individual use, \
             and revoke access promptly when a role ends.",
        ),
    },
    AwsManagedRequirement {
        name: "vulnerability-identification-and-patch-management",
        description: "Vulnerabilities in the workload's components must be identified from \
                      authoritative sources and patched within a window set by their risk. \
                      The design must state the sources monitored, how a vulnerability is \
                      matched to the deployed components, the risk ranking applied, the \
                      patch window for each rank, and how a component that cannot be \
                      patched is otherwise protected.",
        domain: "MaintainVulnerabilityManagement",
        evaluation: "Check that authoritative vulnerability sources are monitored and \
                     matched to deployed components, that findings are ranked, and that \
                     each is patched in its window or compensated for.",
        remediation: Some(
            "Monitor authoritative vulnerability sources, match them to the deployed \
             inventory, and patch within the window for each rank or compensate where a \
             patch is impossible.",
        ),
    },
];
