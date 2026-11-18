// SPDX-License-Identifier: CC0-1.0

//! The requirement content the NIST CSF pack ships with: the workload requirements the
//! pack derives from the NIST Cybersecurity Framework, one table entry per requirement the
//! pack defines, with the framework FUNCTION each belongs to (Govern, Identify, Protect,
//! Detect, Respond, Recover) as the entry's `domain`.
//!
//! Each `description` is the requirement's published statement — what the workload must
//! support and what its design has to demonstrate — because that is the text a client
//! renders or evaluates the requirement from; a summary line would read as a different,
//! weaker rule.
//!
//! WHICH TEXT IS WHICH: the pack's requirement NAMES and the count of them are recorded —
//! they are what a fresh account's `ListSecurityRequirements` reports for this pack. Of the
//! statement text, the part the corpus is known to state is written here as it states it
//! (the opening and the escalation clause of the adverse-event requirement below), and the
//! rest is written to the published structure of the framework function each requirement
//! belongs to. The `domain`, `evaluation` and `remediation` beside each statement are the
//! same: written to that structure, not recorded. A later recording of the exact wording
//! replaces what is here rather than being judged equal to it.

use super::AwsManagedRequirement;

/// The requirements the NIST CSF pack ships with, complete: the thirty-seven requirements
/// the pack defines, in name order.
pub const NIST_CSF_PACK_REQUIREMENTS: [AwsManagedRequirement; 37] = [
    AwsManagedRequirement {
        name: "adverse-event-analysis-and-threat-intelligence-integration",
        description: "Workloads should be designed to support the detection and analysis \
                      of anomalies, indicators of compromise (IoCs), and other \
                      potentially adverse events. This includes emitting telemetry \
                      sufficient for correlation across multiple sources, integrating \
                      cyber threat intelligence to contextualize anomalies against known \
                      adversary tactics, techniques and procedures (TTPs), recording the \
                      characterization each event received, and establishing defined \
                      criteria for escalating adverse events to formal incident \
                      declarations.",
        domain: "Detect",
        evaluation: "Check that adverse events are correlated across sources and against \
                     a threat intelligence source, that each correlation is recorded, and \
                     that the escalation criteria for declaring an incident are defined.",
        remediation: Some(
            "Subscribe the detection pipeline to a threat intelligence feed, record the \
             characterization of each adverse event and publish the criteria that \
             escalate one to an incident.",
        ),
    },
    AwsManagedRequirement {
        name: "ai-component-risk-assessment-and-security-integration",
        description: "Workloads that incorporate artificial intelligence or machine \
                      learning components must assess the cybersecurity risk those \
                      components introduce and manage them under the same security \
                      program as every other component. The design must inventory each \
                      model, the training and inference data it draws on and the \
                      identities permitted to invoke it, evaluate risks such as data \
                      poisoning, prompt injection, model extraction and unintended \
                      disclosure through inference, and demonstrate the controls applied \
                      to each. Residual AI risk must be reported through the same risk \
                      register as the rest of the workload.",
        domain: "Identify",
        evaluation: "Check that every AI component is inventoried with its data sources \
                     and invoking identities, that its risks are assessed, and that its \
                     residual risk appears in the workload's risk register.",
        remediation: Some(
            "Inventory each AI component, assess the risk of its data, invocation path \
             and outputs, and bring it under the controls and the risk register the rest \
             of the workload uses.",
        ),
    },
    AwsManagedRequirement {
        name: "compliance-driven-workload-security-design",
        description: "The workload's security design must be driven by the legal, \
                      regulatory and contractual obligations that apply to it rather than \
                      by technology preference. The design must identify the obligations \
                      in scope, trace each one to the controls that satisfy it, and \
                      demonstrate how the evidence of that satisfaction is produced and \
                      retained. A change to the obligations in scope must trigger a \
                      reassessment of the design.",
        domain: "Govern",
        evaluation: "Check that the obligations in scope are recorded, that each traces to \
                     the controls satisfying it with the evidence they produce, and that a \
                     change in scope triggers a design reassessment.",
        remediation: Some(
            "Record the obligations the workload is subject to, map each to the controls \
             and evidence that satisfy it, and reassess the design when the scope moves.",
        ),
    },
    AwsManagedRequirement {
        name: "comprehensive-logging-and-continuous-monitoring-coverage",
        description: "Workloads must produce a complete and continuous record of \
                      security-relevant activity across every layer they are built from. \
                      The design must enumerate the log sources of its compute, storage, \
                      network, identity and application layers, show that each source is \
                      collected without gaps, and state the retention each record is held \
                      for. Coverage must be monitored in its own right, so that the \
                      absence of expected telemetry is detected as readily as its \
                      content.",
        domain: "Detect",
        evaluation: "Check that every layer's log sources are enumerated and collected, \
                     that retention is stated for each, and that missing telemetry raises \
                     a finding of its own.",
        remediation: Some(
            "Enumerate and collect the log sources of each layer, set the retention each \
             requires, and alert on the absence of expected telemetry as well as on its \
             content.",
        ),
    },
    AwsManagedRequirement {
        name: "continuous-workload-cybersecurity-risk-management",
        description: "Cybersecurity risk for the workload must be managed continuously \
                      rather than assessed once at design time. The design must show how \
                      changes to the workload, its dependencies, its data and the threat \
                      landscape are fed back into the risk assessment, how the resulting \
                      risks are tracked to a decision, and how the current risk position \
                      is reported to the owner accountable for accepting it.",
        domain: "Identify",
        evaluation: "Check that changes to the workload, its dependencies and the threat \
                     landscape re-enter the risk assessment, and that each resulting risk \
                     is tracked to a recorded decision.",
        remediation: Some(
            "Feed workload, dependency and threat changes back into the risk assessment \
             on a defined cadence and track every resulting risk to an accountable \
             decision.",
        ),
    },
    AwsManagedRequirement {
        name: "forensic-readiness-and-evidence-integrity-for-workloads",
        description: "Workloads must be able to preserve the evidence an investigation \
                      needs without destroying it in the course of responding. The design \
                      must identify the artifacts an investigator would require, keep them \
                      where responders can reach them under incident conditions, protect \
                      them from modification or deletion by the identities the incident \
                      may have compromised, and record the chain of custody from \
                      collection onwards.",
        domain: "Respond",
        evaluation: "Check that the artifacts an investigation needs are identified and \
                     retained outside the control of the identities under investigation, \
                     and that their chain of custody is recorded.",
        remediation: Some(
            "Identify the forensic artifacts the workload must preserve, copy them to a \
             destination the compromised identities cannot alter, and record their chain \
             of custody.",
        ),
    },
    AwsManagedRequirement {
        name: "holistic-cybersecurity-coverage-across-all-risk-functions",
        description: "A workload's security design must address the whole cybersecurity \
                      risk lifecycle — governing, identifying, protecting, detecting, \
                      responding to and recovering from risk — rather than a subset of it. \
                      The design must demonstrate coverage of each function, name the \
                      mechanisms that serve it, and show that no function is left to an \
                      assumption about another team or another system.",
        domain: "Govern",
        evaluation: "Check that the design names the mechanisms covering each risk \
                     function and that no function is covered only by an assumption about \
                     another team or system.",
        remediation: Some(
            "Map the workload's mechanisms onto every risk function and close the \
             functions covered only by assumption with a named owner and control.",
        ),
    },
    AwsManagedRequirement {
        name: "incident-response-recovery-communication-planning",
        description: "Recovery from an incident must be planned and communicated, not \
                      improvised. The design must state who is informed of recovery \
                      progress, through which channel and at which points — internal \
                      stakeholders, customers, and any regulator or partner with a \
                      notification right — and must show that the information those \
                      recipients need is obtainable from the workload while the recovery \
                      is in progress.",
        domain: "Recover",
        evaluation: "Check that the recovery communication plan names its recipients, \
                     channels and trigger points, and that the workload can produce the \
                     information each recipient is owed during a recovery.",
        remediation: Some(
            "Write the recovery communication plan with its recipients, channels and \
             trigger points, and make the status it reports observable from the workload.",
        ),
    },
    AwsManagedRequirement {
        name: "least-privilege-and-separation-of-duties-for-workload-access",
        description: "Access to the workload must be granted at the minimum scope each \
                      task requires, and duties whose combination would let a single \
                      identity act unchecked must be separated. The design must enumerate \
                      the roles that reach the workload, the permissions each holds and \
                      the pairs of duties kept apart, and must demonstrate how a grant is \
                      reviewed and withdrawn once the task no longer needs it.",
        domain: "Protect",
        evaluation: "Check that each role's permissions match the task it performs, that \
                     conflicting duties are held by different identities, and that grants \
                     are reviewed and withdrawn when the task ends.",
        remediation: Some(
            "Scope each role to the task it performs, split the duties whose combination \
             would go unchecked, and withdraw grants the task no longer needs.",
        ),
    },
    AwsManagedRequirement {
        name: "platform-configuration-software-lifecycle-management",
        description: "The platforms and software a workload runs on must be configured \
                      from a managed baseline and kept inside their supported lifecycle. \
                      The design must state where the baseline comes from, how drift from \
                      it is detected and corrected, how versions approaching end of \
                      support are identified ahead of time, and how an update is tested \
                      and rolled out without leaving a component unpatched.",
        domain: "Protect",
        evaluation: "Check that components are deployed from a managed baseline, that \
                     drift and end-of-support versions are detected, and that updates \
                     reach every component.",
        remediation: Some(
            "Deploy from a managed baseline, detect and correct drift, and schedule \
             updates so no component is left on an unsupported or unpatched version.",
        ),
    },
    AwsManagedRequirement {
        name: "privacy-cybersecurity-coordinated-risk-management",
        description: "Where a workload processes personal data, privacy risk and \
                      cybersecurity risk must be managed in coordination rather than in \
                      separate programs. The design must identify the personal data it \
                      handles and the purposes it is processed for, name the controls that \
                      serve both the security and the privacy obligation, and show how a \
                      decision taken for one is assessed against the other.",
        domain: "Govern",
        evaluation: "Check that the personal data handled and its purposes are recorded, \
                     and that security decisions affecting it are assessed for their \
                     privacy consequence.",
        remediation: Some(
            "Record the personal data the workload processes and its purposes, and assess \
             each security decision touching it against the privacy obligation.",
        ),
    },
    AwsManagedRequirement {
        name: "risk-informed-security-control-selection-and-prioritization",
        description: "The controls a workload implements must be selected and sequenced \
                      from its assessed risk rather than applied uniformly. The design \
                      must link each control to the risk it reduces, justify the controls \
                      it leaves out, and show the remaining risk being accepted by an \
                      accountable owner.",
        domain: "Govern",
        evaluation: "Check that each implemented control names the risk it reduces, that \
                     omissions are justified, and that residual risk carries a recorded \
                     acceptance.",
        remediation: Some(
            "Link every control to the assessed risk it reduces, justify what is left \
             out, and record the acceptance of the risk that remains.",
        ),
    },
    AwsManagedRequirement {
        name: "secure-software-development-lifecycle-integration",
        description: "Security activities must be integrated into every phase of the \
                      workload's software development lifecycle rather than appended to \
                      its release. The design must show where threat modelling, secure \
                      coding standards, peer review, dependency analysis and security \
                      testing occur, and must demonstrate that a change cannot reach \
                      production without the activities its risk level requires.",
        domain: "Protect",
        evaluation: "Check that threat modelling, review, dependency analysis and security \
                     testing are placed in the lifecycle and that a release is blocked \
                     when the activities its risk requires have not run.",
        remediation: Some(
            "Place the security activities each phase needs into the delivery process and \
             block a release whose required activities have not run.",
        ),
    },
    AwsManagedRequirement {
        name: "security-control-mapping-to-recognized-frameworks",
        description: "The workload's controls must be mapped to the recognized \
                      cybersecurity framework or frameworks the organization has adopted, \
                      so that coverage and gaps can be stated in a common language. The \
                      design must record the mapping, keep it current as controls change, \
                      and identify every framework outcome that no implemented control \
                      satisfies.",
        domain: "Govern",
        evaluation: "Check that a current mapping from implemented controls to framework \
                     outcomes exists and that unsatisfied outcomes are listed.",
        remediation: Some(
            "Map each implemented control to the framework outcomes it serves, keep the \
             mapping current and list the outcomes still unsatisfied.",
        ),
    },
    AwsManagedRequirement {
        name: "security-design-consistency-and-repeatable-pattern-application",
        description: "Security must be applied through consistent, repeatable patterns \
                      rather than per-component decisions. The design must identify the \
                      approved patterns it builds on, show that components with the same \
                      security requirement use the same pattern, and justify every \
                      deviation, so that a reviewer can tell an exception from an \
                      oversight.",
        domain: "Protect",
        evaluation: "Check that components with the same security requirement use the same \
                     approved pattern and that each deviation carries a recorded \
                     justification.",
        remediation: Some(
            "Adopt the approved pattern for each security requirement, apply it uniformly \
             and record a justification for every deviation.",
        ),
    },
    AwsManagedRequirement {
        name: "sensitive-data-inventory-classification-and-flow-documentation",
        description: "The workload must know what sensitive data it holds, how that data \
                      is classified and where it flows. The design must inventory each \
                      store and interface handling sensitive data, record the \
                      classification applied to it, and document the flows between \
                      components and to third parties together with the controls each \
                      flow relies on.",
        domain: "Identify",
        evaluation: "Check that every store and interface handling sensitive data is \
                     inventoried with its classification and that each flow is documented \
                     with the controls protecting it.",
        remediation: Some(
            "Inventory and classify the stores and interfaces holding sensitive data and \
             document every flow between them with the controls it depends on.",
        ),
    },
    AwsManagedRequirement {
        name: "software-supply-chain-integrity-and-acquisition-verification",
        description: "Software the workload acquires must be verified before it is \
                      trusted, and its provenance must remain known after deployment. The \
                      design must state how components are obtained, how their integrity \
                      and origin are verified, how an inventory of what is deployed is \
                      maintained, and how a compromised or withdrawn component is \
                      identified and replaced.",
        domain: "Identify",
        evaluation: "Check that acquired components are integrity- and \
                     provenance-verified before use, that a deployed inventory is \
                     maintained, and that a withdrawn component can be located.",
        remediation: Some(
            "Verify the integrity and origin of every acquired component, keep an \
             inventory of what is deployed, and use it to find and replace a component \
             that becomes untrusted.",
        ),
    },
    AwsManagedRequirement {
        name: "structured-incident-triage-escalation-and-recovery",
        description: "Incidents affecting the workload must be triaged, escalated and \
                      recovered through a defined process rather than ad hoc. The design \
                      must define the severity levels, the evidence collected at triage, \
                      the escalation path and timing for each severity, and the handover \
                      from containment to recovery, so that the response to an incident \
                      does not depend on who answers it.",
        domain: "Respond",
        evaluation: "Check that severity levels, triage evidence, escalation paths and the \
                     containment-to-recovery handover are defined and exercised.",
        remediation: Some(
            "Define the severity levels, the triage evidence, the escalation path per \
             severity and the handover into recovery, and exercise the process.",
        ),
    },
    AwsManagedRequirement {
        name: "structured-post-incident-recovery-and-return-to-operations",
        description: "Returning a workload to normal operation after an incident must \
                      follow a structured, verified sequence. The design must state the \
                      order in which services are restored, the checks that confirm a \
                      restored component is free of the original cause, the validation \
                      that proves data integrity, and the criteria for declaring \
                      operations normal again.",
        domain: "Recover",
        evaluation: "Check that the restoration order, the checks for the original cause, \
                     the data integrity validation and the return-to-normal criteria are \
                     all defined.",
        remediation: Some(
            "Write the restoration sequence with the checks that confirm the cause is \
             gone, the data integrity validation and the criteria for declaring \
             operations normal.",
        ),
    },
    AwsManagedRequirement {
        name: "third-party-dependency-risk-assessment-and-governance",
        description: "Third parties and services the workload depends on must be assessed \
                      for cybersecurity risk and governed for the life of the \
                      relationship. The design must identify each dependency, the data and \
                      access it is granted, the security requirements it is held to, and \
                      the monitoring and review that confirm it continues to meet them.",
        domain: "Govern",
        evaluation: "Check that each third party is assessed before use, holds only the \
                     data and access it needs, and is reviewed against its security \
                     requirements for as long as the relationship lasts.",
        remediation: Some(
            "Assess every third party before granting access, record the security \
             requirements it is held to, and review its compliance for the life of the \
             relationship.",
        ),
    },
    AwsManagedRequirement {
        name: "user-third-party-activity-monitoring-for-anomaly-detection",
        description: "Activity by users and third parties inside the workload must be \
                      monitored for behaviour that deviates from what is expected. The \
                      design must record the authentication, authorization and data-access \
                      activity of each identity, establish what normal looks like for it, \
                      and raise an anomaly for review when a deviation could indicate \
                      misuse or a compromised credential.",
        domain: "Detect",
        evaluation: "Check that user and third-party activity is recorded per identity, \
                     compared against an expected baseline, and escalated for review when \
                     it deviates.",
        remediation: Some(
            "Record each identity's activity, baseline what is normal for it and raise \
             deviations for review rather than only storing the records.",
        ),
    },
    AwsManagedRequirement {
        name: "vulnerability-disclosure-and-change-risk-management",
        description: "Vulnerabilities in the workload must have a route to disclosure, and \
                      the changes made to remediate them must themselves be risk-assessed. \
                      The design must publish how a vulnerability is reported and \
                      acknowledged, how a report is triaged to an owner and a remediation \
                      window, and how an emergency change is assessed, approved and \
                      recorded without bypassing the controls it affects.",
        domain: "Identify",
        evaluation: "Check that a disclosure route is published and triaged to owners with \
                     remediation windows, and that emergency changes are assessed, \
                     approved and recorded.",
        remediation: Some(
            "Publish a vulnerability disclosure route, triage each report to an owner and \
             a window, and risk-assess and record the changes that remediate them.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-asset-inventory-and-lifecycle-management",
        description: "Every asset the workload is composed of must be inventoried and \
                      managed from provisioning through to decommissioning. The design \
                      must show how assets are discovered, how each is recorded with an \
                      owner and a classification, and how decommissioning removes its \
                      access, its data and its credentials rather than only its compute.",
        domain: "Identify",
        evaluation: "Check that assets are discovered rather than registered by hand, that \
                     each carries an owner and a classification, and that \
                     decommissioning removes its access, data and credentials.",
        remediation: Some(
            "Discover the workload's assets continuously, record an owner and \
             classification for each, and make decommissioning remove its access, data \
             and credentials.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-backup-integrity-and-recovery-verification",
        description: "Backups the workload relies on must be protected from the events \
                      they exist to survive, and their recoverability must be proven. The \
                      design must state what is backed up and how often, where the copies \
                      are held and under whose control, how they are protected from \
                      deletion or encryption by an attacker, and how a restore is \
                      exercised and its result verified.",
        domain: "Protect",
        evaluation: "Check that backups are held outside the control of the identities \
                     that could destroy the originals and that a restore has been \
                     exercised and verified.",
        remediation: Some(
            "Hold backup copies under separate control with deletion protection, and \
             exercise and verify a restore on a defined schedule.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-continuous-monitoring-and-adverse-event-detection",
        description: "The workload must be monitored continuously so that adverse events \
                      are detected while they can still be acted on. The design must \
                      identify the events it watches for, the sources each detection draws \
                      on, the latency between an event and its detection, and the \
                      destination each detection is delivered to for action.",
        domain: "Detect",
        evaluation: "Check that the events watched for are enumerated with their sources, \
                     that detection latency is stated, and that each detection reaches a \
                     destination that acts on it.",
        remediation: Some(
            "Enumerate the adverse events the workload must detect, wire each to a \
             monitoring source, and deliver the detections to an owner that acts on them.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-cybersecurity-risk-assessment",
        description: "The cybersecurity risk the workload carries must be assessed \
                      explicitly, against the assets it holds and the threats it faces. \
                      The design must state the assets in scope, the threats considered, \
                      the likelihood and impact assigned to each, and the resulting risk \
                      position that its control selection is derived from.",
        domain: "Identify",
        evaluation: "Check that the assessment names the assets and threats in scope with \
                     a likelihood and impact for each, and that the control selection \
                     follows from it.",
        remediation: Some(
            "Assess the workload's risk against its assets and threats, rate each for \
             likelihood and impact, and derive the control selection from the result.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-cybersecurity-risk-prioritization-action-planning",
        description: "Assessed risks must be prioritized and turned into a plan with \
                      owners and dates. The design must rank the risks it records, state \
                      the response chosen for each — mitigate, transfer, avoid or accept — \
                      and show the action, the owner and the target date that follow from \
                      that choice, together with how progress against the plan is \
                      tracked.",
        domain: "Identify",
        evaluation: "Check that recorded risks are ranked, that each carries a chosen \
                     response with an action, an owner and a date, and that progress is \
                     tracked.",
        remediation: Some(
            "Rank the assessed risks, choose and record a response for each with an \
             action, an owner and a target date, and track the plan to completion.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-data-lifecycle-cia-protection",
        description: "Data must be protected for its confidentiality, integrity and \
                      availability at every stage of its lifecycle in the workload. The \
                      design must show the controls applied to data in transit, in use, at \
                      rest and at destruction, and must demonstrate that integrity and \
                      availability are addressed alongside confidentiality rather than \
                      assumed from encryption.",
        domain: "Protect",
        evaluation: "Check that each lifecycle stage names the controls protecting \
                     confidentiality, integrity and availability, and that integrity and \
                     availability are not left to the encryption control.",
        remediation: Some(
            "State the controls protecting data in transit, in use, at rest and at \
             destruction, and add the integrity and availability controls encryption does \
             not provide.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-identity-authentication-and-access-control",
        description: "Every identity that reaches the workload — human, service or \
                      federated — must be authenticated and authorized proportionately to \
                      what it can reach. The design must enumerate the identity sources it \
                      trusts, the strength of authentication required of each, how \
                      authorizations are expressed and evaluated at the point of access, \
                      and how access is revoked promptly when an identity changes or \
                      leaves.",
        domain: "Protect",
        evaluation: "Check that every identity class authenticates against a trusted \
                     source at a proportionate strength, that authorization is evaluated \
                     at the point of access, and that revocation is prompt.",
        remediation: Some(
            "Authenticate each identity class against a trusted source at a proportionate \
             strength, evaluate authorization at the access point, and revoke access as \
             identities change.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-incident-containment-and-eradication-design",
        description: "The workload must be designed so that an incident can be contained \
                      and its cause eradicated without a full outage. The design must show \
                      how a compromised component, credential or network path is isolated, \
                      how that isolation is invoked, what function it costs while it is in \
                      place, and how the cause is removed before the component is returned \
                      to service.",
        domain: "Respond",
        evaluation: "Check that isolation of a component, credential or path is possible \
                     and invokable, that its cost in function is known, and that return \
                     to service requires the cause to be removed.",
        remediation: Some(
            "Build invokable isolation for components, credentials and network paths, \
             document what each costs, and gate return to service on eradicating the \
             cause.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-incident-recovery-architecture-and-communication",
        description: "Recovery must be a property of the architecture, not only of the \
                      runbook. The design must state the recovery point and recovery time \
                      the workload is built to meet, the components and data each \
                      restoration step depends on, and how recovery status is observed and \
                      communicated while the restoration is in progress.",
        domain: "Recover",
        evaluation: "Check that the recovery point and time objectives are stated and \
                     supported by the architecture, and that recovery status is observable \
                     while a restoration runs.",
        remediation: Some(
            "State the recovery point and time the workload must meet, build the \
             architecture that delivers them, and make recovery progress observable.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-incident-response-readiness",
        description: "The workload must be ready to be responded to before an incident \
                      occurs. The design must show that responders have the access, the \
                      telemetry and the runbooks they need, that those are exercised \
                      rather than only documented, and that the response plan names the \
                      roles accountable for each step.",
        domain: "Respond",
        evaluation: "Check that responder access, telemetry and runbooks exist, are \
                     exercised, and that the plan names an accountable role for each \
                     step.",
        remediation: Some(
            "Pre-provision responder access and telemetry, write the runbooks, name the \
             accountable roles, and exercise the plan against a simulated event.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-infrastructure-resilience-architecture",
        description: "The infrastructure the workload runs on must be architected to \
                      resist and withstand adverse events. The design must show the \
                      redundancy of each layer, the failure domains it spans, the \
                      behaviour of the workload when a domain is lost, and the capacity \
                      and configuration that make the surviving domains sufficient.",
        domain: "Protect",
        evaluation: "Check that each layer's redundancy and failure domains are stated, \
                     that the behaviour on losing a domain is known, and that the \
                     survivors carry the load.",
        remediation: Some(
            "Spread each layer across failure domains, size the survivors for the whole \
             load, and verify the workload's behaviour when a domain is lost.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-network-protection-and-continuous-monitoring",
        description: "The workload's network must be protected at its boundaries and \
                      between its internal zones, and that traffic must be monitored \
                      continuously. The design must document the zones and the paths \
                      permitted between them, the controls enforcing each path, and the \
                      telemetry that shows what traffic actually flowed, so that an \
                      unexpected path is detected rather than assumed impossible.",
        domain: "Protect",
        evaluation: "Check that zones and permitted paths are documented and enforced, and \
                     that flow telemetry is compared against them so an unexpected path \
                     is detected.",
        remediation: Some(
            "Document the network zones and permitted paths, enforce them between zones as \
             well as at the edge, and compare flow telemetry against the documented \
             paths.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-resilience-and-availability-architecture",
        description: "The workload must meet a stated availability objective under both \
                      failure and attack. The design must state the objective, show the \
                      architecture that delivers it, and demonstrate how load shedding, \
                      throttling and isolation keep the workload available when demand or \
                      an attack exceeds what it was provisioned for.",
        domain: "Protect",
        evaluation: "Check that an availability objective is stated, that the architecture \
                     meets it, and that shedding, throttling and isolation keep the \
                     workload serving under excess demand.",
        remediation: Some(
            "State the availability objective, build to it, and add shedding, throttling \
             and isolation so excess demand degrades the workload instead of failing it.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-risk-visibility-and-communication-design",
        description: "The workload's security risk position must be visible to the people \
                      accountable for it. The design must state what risk information is \
                      produced, how it is aggregated, who receives it and how often, and \
                      must show that the information is sufficient for an accountable \
                      owner to make a decision without asking the implementation team.",
        domain: "Govern",
        evaluation: "Check that risk information is produced, aggregated and delivered to \
                     named recipients on a stated cadence, and that it is sufficient to \
                     decide on without further enquiry.",
        remediation: Some(
            "Publish the workload's risk position to its accountable owners on a stated \
             cadence, in the form a decision can be taken from.",
        ),
    },
    AwsManagedRequirement {
        name: "workload-third-party-dependency-risk-management",
        description: "Risk arising from the third-party components and services the \
                      workload runs on must be managed throughout the workload's life. The \
                      design must record each dependency with its criticality, the failure \
                      and compromise modes it introduces, the mitigations in place for \
                      each, and the plan for replacing a dependency that becomes \
                      unsupported or untrusted.",
        domain: "Govern",
        evaluation: "Check that each third-party dependency is recorded with its \
                     criticality, failure and compromise modes, mitigations and a \
                     replacement plan.",
        remediation: Some(
            "Record every third-party dependency with its criticality and failure modes, \
             mitigate each, and keep a replacement plan for the ones that matter.",
        ),
    },
];
