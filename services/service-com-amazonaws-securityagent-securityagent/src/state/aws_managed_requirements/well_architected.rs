// SPDX-License-Identifier: CC0-1.0

//! The requirement content the AWS Well-Architected pack ships with: the workload
//! requirements the pack derives from the Security Pillar of the AWS Well-Architected
//! Framework, one table entry per requirement the pack defines, with the pillar's
//! best-practice AREA as the entry's `domain`.
//!
//! Each `description` is the requirement's published statement — what the architecture must
//! provide and what its documentation has to demonstrate — because a reviewer judges the
//! architecture from that text, and a summary line would not say what the requirement
//! demands.
//!
//! WHICH TEXT IS WHICH: the pack's requirement NAMES and the count of them are recorded —
//! they are what a fresh account's `ListSecurityRequirements` reports for this pack. Of the
//! statement text, the part the corpus is known to state is written here as it states it
//! (the east-west / north-south identity clause of the authenticated-network requirement
//! and the certificate lifecycle clause), and the rest is written to the published
//! structure of the pillar area each entry belongs to. The `domain`, `evaluation` and
//! `remediation` beside each statement are the same: written to that structure, not
//! recorded. A later recording of the exact wording replaces what is here rather than
//! being judged equal to it.

use super::AwsManagedRequirement;

/// The requirements the Well-Architected pack ships with, complete: the thirty-two
/// requirements the pack defines, in name order.
pub const WELL_ARCHITECTED_PACK_REQUIREMENTS: [AwsManagedRequirement; 32] = [
    AwsManagedRequirement {
        name: "authenticated-network-communication-architecture",
        description: "Network communication within and into the workload must be \
                      authenticated rather than trusted on the basis of its origin. The \
                      design must show how east-west traffic between workload components \
                      and north-south traffic from clients establishes a verified \
                      identity — mutual TLS (mTLS), IPsec, or request signing with a \
                      service identity such as SigV4 — how the certificates or credentials \
                      behind that identity are issued and rotated, and how a connection \
                      that presents no verified identity is rejected rather than served.",
        domain: "InfrastructureProtection",
        evaluation: "Check that component-to-component and client traffic each carry a \
                     verified identity, that the credentials behind it are rotated, and \
                     that an unauthenticated connection is refused.",
        remediation: Some(
            "Terminate component and client traffic on an authenticated channel, verify \
             the caller's identity before serving it, and rotate the credentials that \
             identity rests on.",
        ),
    },
    AwsManagedRequirement {
        name: "automated-certificate-management-architecture",
        description: "Certificates the workload relies on must be issued, distributed, \
                      renewed and revoked automatically. The design must name the \
                      certificate authority each certificate comes from, show how a \
                      certificate reaches the component that serves it without a manual \
                      step, how renewal happens well before expiry, how expiry and \
                      misissuance are monitored, and how a compromised certificate is \
                      revoked and replaced.",
        domain: "DataProtection",
        evaluation: "Check that certificates are issued by a managed authority, \
                     distributed and renewed without manual action, monitored for expiry, \
                     and revocable on compromise.",
        remediation: Some(
            "Issue workload certificates from a managed authority, automate their \
             distribution and renewal ahead of expiry, and monitor and revoke them from \
             one place.",
        ),
    },
    AwsManagedRequirement {
        name: "automated-credential-governance-architecture",
        description: "Credentials must be governed by automation rather than by periodic \
                      human review. The design must show how credentials are inventoried \
                      as they are created, how their age, usage and scope are evaluated \
                      continuously, how an unused or over-aged credential is rotated or \
                      removed without manual intervention, and how an exception is \
                      recorded and made to expire.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check that credentials are inventoried on creation, evaluated \
                     continuously for age, use and scope, and rotated or removed \
                     automatically, with every exception expiring.",
        remediation: Some(
            "Inventory credentials as they are created, evaluate their age, use and scope \
             continuously, and automate the rotation or removal of the ones that fail.",
        ),
    },
    AwsManagedRequirement {
        name: "automated-data-classification-architecture",
        description: "Data must be classified automatically as it enters and moves through \
                      the workload. The design must show the mechanism that inspects or \
                      infers the sensitivity of incoming data, the classification metadata \
                      it records, how that classification follows the data into downstream \
                      stores, and how a store holding data above its permitted \
                      classification is detected.",
        domain: "DataProtection",
        evaluation: "Check that incoming data is classified automatically, that the \
                     classification follows it downstream, and that a store holding data \
                     above its permitted level is detected.",
        remediation: Some(
            "Classify data automatically on entry, propagate the classification to \
             downstream stores, and alert on a store holding data above its level.",
        ),
    },
    AwsManagedRequirement {
        name: "automated-remediation-with-oversight-architecture",
        description: "Remediation of security findings should be automated, with human \
                      oversight where the action carries risk. The design must state which \
                      findings are remediated automatically, the action each one triggers, \
                      the guardrails that prevent an automated action from causing harm, \
                      the findings that require approval before acting, and the record each \
                      automated action leaves.",
        domain: "IncidentResponse",
        evaluation: "Check that automated remediations are enumerated with their actions \
                     and guardrails, that risky actions require approval, and that each \
                     action is recorded.",
        remediation: Some(
            "Automate the remediation of the findings whose action is safe, guard it, \
             require approval for the rest, and record every action taken.",
        ),
    },
    AwsManagedRequirement {
        name: "automated-security-testing-architecture",
        description: "Security testing must be automated and run as part of the workload's \
                      delivery process. The design must show the static, dependency, \
                      secret, configuration and dynamic tests it runs, where in the \
                      pipeline each runs, the findings that stop a release, and how the \
                      test suite is kept current with the threats the workload faces.",
        domain: "ApplicationSecurity",
        evaluation: "Check that static, dependency, secret, configuration and dynamic \
                     tests run in the pipeline, that defined findings block the release, \
                     and that the suite is kept current.",
        remediation: Some(
            "Add the static, dependency, secret, configuration and dynamic security tests \
             to the pipeline, and block a release on the findings that matter.",
        ),
    },
    AwsManagedRequirement {
        name: "automated-threat-intelligence-integration",
        description: "Threat intelligence must be integrated into the workload's detection \
                      and response automatically. The design must name the intelligence \
                      sources consumed, how indicators are matched against the workload's \
                      telemetry and configuration, how a match raises a finding or triggers \
                      a protective action, and how stale intelligence is retired.",
        domain: "Detection",
        evaluation: "Check that threat intelligence sources are consumed automatically, \
                     matched against telemetry and configuration, acted on, and retired \
                     when stale.",
        remediation: Some(
            "Consume threat intelligence automatically, match it against the workload's \
             telemetry and configuration, act on a match, and retire stale indicators.",
        ),
    },
    AwsManagedRequirement {
        name: "automated-vulnerability-management-architecture",
        description: "Vulnerability management must be continuous and automated across the \
                      workload's compute, images, dependencies and infrastructure. The \
                      design must show how components are discovered and scanned without \
                      being enrolled by hand, how findings are deduplicated and assigned, \
                      the remediation windows by severity, and how patching or \
                      redeployment is driven automatically.",
        domain: "InfrastructureProtection",
        evaluation: "Check that components are discovered and scanned automatically, that \
                     findings are deduplicated and assigned, and that patching or \
                     redeployment runs inside the severity windows.",
        remediation: Some(
            "Discover and scan every component automatically, assign the findings, and \
             drive patching or redeployment inside the window for each severity.",
        ),
    },
    AwsManagedRequirement {
        name: "automation-first-operational-access-architecture",
        description: "Operational tasks must be performed through automation rather than \
                      interactive access to running components. The design must show the \
                      automated paths operators use, demonstrate that interactive access is \
                      not required for routine work, and state the break-glass path that \
                      remains, the approval it needs, the time it is limited to, and the \
                      record it produces.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check that routine operations run through automation, that interactive \
                     access is not needed for them, and that the break-glass path is \
                     approved, time-bound and recorded.",
        remediation: Some(
            "Move routine operations into automation, remove standing interactive access, \
             and leave a break-glass path that is approved, time-bound and recorded.",
        ),
    },
    AwsManagedRequirement {
        name: "code-review-process-architecture",
        description: "Changes to the workload's code must be reviewed before they are \
                      merged, with security in the scope of the review. The design must \
                      state who may review, what the review must consider, the automated \
                      checks that run alongside it, the changes that require more than one \
                      reviewer, and how a change is prevented from reaching a protected \
                      branch without review.",
        domain: "ApplicationSecurity",
        evaluation: "Check that merges require review by a permitted reviewer with \
                     security in scope, that automated checks run with it, and that an \
                     unreviewed change cannot reach a protected branch.",
        remediation: Some(
            "Require review with security in scope before a merge, run the automated \
             checks alongside it, and protect the branch against an unreviewed change.",
        ),
    },
    AwsManagedRequirement {
        name: "compliance-framework-control-mapping",
        description: "The workload's controls must be mapped to the compliance frameworks \
                      it is held to, so that an obligation can be traced to the mechanism \
                      that satisfies it. The design must record the mapping, name the \
                      evidence each control produces, keep the mapping current as the \
                      architecture changes, and identify the obligations no control yet \
                      satisfies.",
        domain: "SecurityFoundations",
        evaluation: "Check that a current mapping from obligations to controls and their \
                     evidence exists and that unsatisfied obligations are listed.",
        remediation: Some(
            "Map each compliance obligation to the control and evidence that satisfy it, \
             keep the mapping current, and list what is still unsatisfied.",
        ),
    },
    AwsManagedRequirement {
        name: "comprehensive-security-logging-architecture",
        description: "The workload must log security-relevant activity from every layer \
                      into a place it can be analysed from. The design must enumerate the \
                      sources — API activity, identity, network flow, application and data \
                      access — and show how the records are centralized, protected from the \
                      accounts that produce them, retained for their stated period, and \
                      queryable with the fields an investigation needs.",
        domain: "Detection",
        evaluation: "Check that each layer's log sources are centralized, protected from \
                     the producing accounts, retained as stated, and queryable on the \
                     fields an investigation needs.",
        remediation: Some(
            "Centralize the log sources of every layer into a destination the producing \
             accounts cannot alter, with the retention and the query fields an \
             investigation needs.",
        ),
    },
    AwsManagedRequirement {
        name: "controlled-cross-account-access-architecture",
        description: "Access that crosses an account boundary must be explicit, scoped and \
                      observable. The design must state each cross-account path, the role \
                      assumed and the trust policy that permits it, the conditions \
                      constraining it, the external identifiers or confused-deputy \
                      protections applied, and the logging that shows each use.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check that each cross-account path names its role, trust policy and \
                     conditions, carries confused-deputy protection, and is logged on \
                     use.",
        remediation: Some(
            "Make each cross-account path an explicit role with a scoped trust policy and \
             conditions, add confused-deputy protection, and log every assumption.",
        ),
    },
    AwsManagedRequirement {
        name: "data-at-rest-access-control-architecture",
        description: "Access to data at rest must be controlled by the identity reaching it \
                      as well as by the encryption protecting it. The design must show the \
                      resource and identity policies governing each store, the conditions \
                      they enforce, the separation between the principals that may read the \
                      data and those that may administer the store, and the detection of a \
                      grant that widens access unexpectedly.",
        domain: "DataProtection",
        evaluation: "Check that each store's resource and identity policies scope access by \
                     condition, that reading and administering are separated, and that a \
                     widening grant is detected.",
        remediation: Some(
            "Scope each store's resource and identity policies with conditions, separate \
             the read and administer principals, and alert on a grant that widens access.",
        ),
    },
    AwsManagedRequirement {
        name: "data-classification-and-tagging-architecture",
        description: "Data and the resources holding it must carry a classification that \
                      automation can act on. The design must define the classification \
                      levels, the tags or metadata that express them, the requirement that \
                      every data-bearing resource carries one, and the controls — access, \
                      encryption, retention, monitoring — that are selected from the tag.",
        domain: "DataProtection",
        evaluation: "Check that the classification levels are defined, that every \
                     data-bearing resource is tagged with one, and that the tag selects \
                     the controls applied to it.",
        remediation: Some(
            "Define the classification levels, tag every data-bearing resource, and drive \
             the access, encryption, retention and monitoring controls from the tag.",
        ),
    },
    AwsManagedRequirement {
        name: "data-lifecycle-management-architecture",
        description: "Data must be managed through a defined lifecycle from creation to \
                      deletion. The design must state the stages data passes through, the \
                      storage class and protection applied at each, the retention and \
                      expiry rules by classification, and the mechanism that moves or \
                      deletes data when a rule is met rather than relying on a manual \
                      task.",
        domain: "DataProtection",
        evaluation: "Check that the lifecycle stages, their protection and the retention \
                     rules by classification are defined, and that transitions and \
                     deletions run automatically.",
        remediation: Some(
            "Define the data lifecycle stages with their protection and retention by \
             classification, and automate the transitions and deletions it requires.",
        ),
    },
    AwsManagedRequirement {
        name: "encryption-at-rest-enforcement-architecture",
        description: "Encryption at rest must be enforced by the architecture, not left to \
                      each resource's default. The design must show the keys used for each \
                      store, the policies or guardrails that prevent an unencrypted store \
                      being created, the detection of one that already exists, and the \
                      separation between the principals using the key and those \
                      administering it.",
        domain: "DataProtection",
        evaluation: "Check that a guardrail prevents an unencrypted store being created, \
                     that existing ones are detected, and that key use and key \
                     administration are held by different principals.",
        remediation: Some(
            "Add a guardrail that refuses an unencrypted store, remediate the ones that \
             exist, and separate the principals that use a key from those that administer \
             it.",
        ),
    },
    AwsManagedRequirement {
        name: "encryption-in-transit-enforcement-architecture",
        description: "Encryption in transit must be enforced everywhere data moves, \
                      including between internal components. The design must state the \
                      protocol versions and cipher suites permitted, how plaintext paths \
                      are prevented rather than merely discouraged, how certificates are \
                      validated by both ends where that applies, and how a non-conforming \
                      connection is detected and refused.",
        domain: "DataProtection",
        evaluation: "Check that internal and external traffic negotiates a permitted \
                     protocol and cipher, that plaintext paths are refused rather than \
                     discouraged, and that certificates are validated.",
        remediation: Some(
            "Require the permitted protocol versions and ciphers on every path, including \
             internal ones, validate certificates, and refuse what cannot negotiate them.",
        ),
    },
    AwsManagedRequirement {
        name: "fine-grained-traffic-flow-control-architecture",
        description: "Traffic between the workload's components must be controlled at the \
                      granularity of the flows they actually need. The design must document \
                      each permitted flow with its port, protocol and direction, show the \
                      mechanism enforcing it close to the component rather than only at the \
                      perimeter, default the rest to deny, and state how the rules are \
                      derived and kept current.",
        domain: "InfrastructureProtection",
        evaluation: "Check that each permitted flow is documented by port, protocol and \
                     direction, enforced close to the component, and that everything else \
                     is denied.",
        remediation: Some(
            "Document the flows each component needs and enforce them next to the \
             component, defaulting everything else to deny.",
        ),
    },
    AwsManagedRequirement {
        name: "incident-management-plan-architecture",
        description: "The workload must have an incident management plan that its \
                      architecture supports. The design must state the incident types \
                      anticipated, the roles and escalation path, the tooling and access \
                      responders use, the communication plan, and the architectural \
                      features — isolation points, forensic copies, deployable fixes — that \
                      each response step relies on.",
        domain: "IncidentResponse",
        evaluation: "Check that the plan names the incident types, roles, escalation, \
                     tooling and communications, and that the architecture provides the \
                     isolation, forensic and deployment features each step needs.",
        remediation: Some(
            "Write the incident management plan and build the isolation points, forensic \
             copies and deployment paths its steps depend on.",
        ),
    },
    AwsManagedRequirement {
        name: "layered-network-segmentation-architecture",
        description: "The network must be segmented into layers so that reaching one \
                      component does not grant reach to the rest. The design must show the \
                      segments and their trust relationships, the controls between them, \
                      the placement of each component in a segment appropriate to its \
                      exposure, and the absence of a path from the most exposed layer to \
                      the most sensitive one.",
        domain: "InfrastructureProtection",
        evaluation: "Check that components sit in segments matching their exposure, that \
                     controls sit between segments, and that no path runs from the most \
                     exposed to the most sensitive layer.",
        remediation: Some(
            "Group components into segments by exposure, control the traffic between them, \
             and remove any direct path from the exposed layer to the sensitive one.",
        ),
    },
    AwsManagedRequirement {
        name: "least-privilege-permission-architecture",
        description: "Permissions must be designed to the least privilege each identity \
                      needs, and the design must make that reviewable. The design must show \
                      how policies are scoped to actions, resources and conditions, how \
                      they are generated or validated from observed usage, how unused \
                      permissions are found and removed, and how a guardrail caps what any \
                      grant can reach.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check that policies are scoped by action, resource and condition, \
                     validated against observed usage, pruned of unused permissions, and \
                     capped by a guardrail.",
        remediation: Some(
            "Scope every policy by action, resource and condition, prune the permissions \
             nothing exercises, and cap the whole with a permission guardrail.",
        ),
    },
    AwsManagedRequirement {
        name: "secrets-management-architecture",
        description: "Secrets the workload needs must be held in a managed secret store and \
                      never in source, images or configuration. The design must show how a \
                      secret is created, who and what may read it, how it is retrieved at \
                      run time, how it is rotated without an outage, how its use is logged, \
                      and how a committed or leaked secret is detected and revoked.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check that secrets live in a managed store, are read at run time by \
                     scoped identities, are rotated without an outage, are logged on use, \
                     and that a leaked one is detected and revoked.",
        remediation: Some(
            "Move every secret into a managed store, read it at run time under a scoped \
             identity, rotate it without an outage, and scan for and revoke leaked ones.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-key-management-architecture",
        description: "Cryptographic keys must be managed by a dedicated key management \
                      service with policies that constrain their use. The design must state \
                      the keys the workload holds and the purpose and scope of each, the \
                      policy limiting which principals may use and which may administer it, \
                      the rotation applied, and the separation that prevents one identity \
                      both using a key and changing its policy.",
        domain: "DataProtection",
        evaluation: "Check that each key has a stated purpose, a policy scoping its users \
                     and administrators separately, and a rotation schedule.",
        remediation: Some(
            "Hold the workload's keys in a key management service, scope each key's policy \
             to its users and administrators separately, and enable rotation.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-third-party-access-architecture",
        description: "Access granted to third parties must be scoped, time-bound and \
                      observable. The design must state each third party, the resources it \
                      reaches, the identity and credentials it uses, the conditions and the \
                      duration applied, the approval behind the grant, and the logging and \
                      review that show how the access is used and when it should end.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check that each third-party grant names its resources, identity, \
                     conditions, duration and approval, and that its use is logged and \
                     reviewed.",
        remediation: Some(
            "Give each third party a scoped, time-bound identity with recorded approval, \
             and log and review how the access is used.",
        ),
    },
    AwsManagedRequirement {
        name: "security-design-principles-integration",
        description: "The workload's architecture must demonstrably apply the security \
                      design principles the framework sets out — a strong identity \
                      foundation, traceability, security at every layer, automation of \
                      security best practice, protection of data in transit and at rest, \
                      keeping people away from data, and preparation for security events. \
                      The design must show, principle by principle, the mechanisms that \
                      implement it and the decisions where it was traded off, with the \
                      reason for each trade-off.",
        domain: "SecurityFoundations",
        evaluation: "Check that the design names the mechanisms implementing each security \
                     design principle and records the reason for every trade-off against \
                     one.",
        remediation: Some(
            "Walk the architecture against each security design principle, name the \
             mechanism that implements it, and record the reason for any trade-off.",
        ),
    },
    AwsManagedRequirement {
        name: "sensitivity-based-data-protection-architecture",
        description: "The protection applied to data must follow its sensitivity. The \
                      design must state the sensitivity levels in use, the encryption, \
                      access control, masking or tokenization, logging and retention \
                      required at each, and the mechanism that applies the right set to a \
                      store or a flow from its recorded classification rather than by \
                      convention.",
        domain: "DataProtection",
        evaluation: "Check that each sensitivity level names the protection it requires and \
                     that a store or flow receives that set from its recorded \
                     classification.",
        remediation: Some(
            "Define the protection each sensitivity level requires and apply it to a store \
             or flow from its recorded classification.",
        ),
    },
    AwsManagedRequirement {
        name: "shared-responsibility-model-documentation",
        description: "The split of security responsibility between the cloud provider, the \
                      platform teams and the workload team must be documented and agreed. \
                      The design must state, for each layer the workload is built on, which \
                      party operates the control, which party configures it and which party \
                      verifies it, and must identify the responsibilities no party has yet \
                      accepted.",
        domain: "SecurityFoundations",
        evaluation: "Check that each layer records who operates, configures and verifies \
                     its controls, and that unaccepted responsibilities are listed.",
        remediation: Some(
            "Document, per layer, who operates, configures and verifies each control, and \
             assign the responsibilities nobody has accepted.",
        ),
    },
    AwsManagedRequirement {
        name: "software-integrity-validation-architecture",
        description: "The integrity of the software the workload runs must be validated \
                      before it is executed and verifiable afterwards. The design must show \
                      how artifacts are signed, how signatures and provenance are verified \
                      at deployment, how an unsigned or unverified artifact is refused, and \
                      how the inventory of what is running can be checked against what was \
                      built.",
        domain: "ApplicationSecurity",
        evaluation: "Check that artifacts are signed, that deployment verifies signature \
                     and provenance and refuses what fails, and that running software can \
                     be reconciled with the build.",
        remediation: Some(
            "Sign build artifacts, verify signature and provenance at deployment, refuse \
             what fails, and reconcile the running inventory with the build record.",
        ),
    },
    AwsManagedRequirement {
        name: "systematic-threat-modeling-in-design",
        description: "Threat modelling must be a systematic part of designing and changing \
                      the workload. The design must state the method used, who takes part, \
                      the assets, entry points and trust boundaries considered, the threats \
                      identified with the mitigation chosen for each, and the trigger that \
                      causes the model to be revisited when the workload changes.",
        domain: "SecurityFoundations",
        evaluation: "Check that a threat model exists with its method, participants, \
                     assets, entry points and trust boundaries, that each threat carries a \
                     mitigation, and that change revisits it.",
        remediation: Some(
            "Threat model the workload with a stated method, record the threats and their \
             mitigations, and revisit the model when the design changes.",
        ),
    },
    AwsManagedRequirement {
        name: "temporary-credentials-architecture",
        description: "Identities should use temporary credentials rather than long-lived \
                      ones. The design must show how human and workload identities obtain \
                      short-lived credentials, the lifetime and scope each is issued with, \
                      the elimination of static keys wherever it is possible, and the \
                      controls and monitoring over any long-lived credential that remains.",
        domain: "IdentityAndAccessManagement",
        evaluation: "Check that human and workload identities obtain short-lived \
                     credentials with a stated lifetime and scope, and that any remaining \
                     static key is justified and monitored.",
        remediation: Some(
            "Issue short-lived credentials to human and workload identities, remove the \
             static keys that are no longer needed, and monitor the ones that remain.",
        ),
    },
    AwsManagedRequirement {
        name: "traffic-inspection-architecture",
        description: "Traffic entering, leaving and crossing the workload must be \
                      inspectable. The design must state where inspection occurs, the \
                      protocols and content it can see, how encrypted traffic is handled \
                      where inspection is required, the detections the inspection feeds, \
                      and the controls protecting the inspection points and the data they \
                      hold.",
        domain: "InfrastructureProtection",
        evaluation: "Check that inspection points cover ingress, egress and internal \
                     traffic, that encrypted traffic is handled deliberately, and that the \
                     inspection points and their data are protected.",
        remediation: Some(
            "Place inspection on the ingress, egress and internal paths, decide how \
             encrypted traffic is handled, and protect the inspection points and what they \
             capture.",
        ),
    },
];
