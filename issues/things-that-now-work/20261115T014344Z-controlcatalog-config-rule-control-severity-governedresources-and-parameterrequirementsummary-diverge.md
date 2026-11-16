Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-list-controls-aws-config-configrule
Issue-Step: task#235
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-list-controls-aws-config-configrule
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/87qo8rsoettjrxjevmjqcw1tu"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/b012d555ea80072760b03e204"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/9vnj728czsv4m6pflfqt21bw2"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/c541c3fac8ebcf2cf52bdb1a3"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/86c13f1fc857518acd6d9bdf9"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/aazzef7lptl5umta6690p6u5c"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/byrg7iwyl4azhecyhqtq8ocw1"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/ce9470ef9d085fd7f70df5e0a"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/84hd48gyrb6e5ribkmsoub44c"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/8f4441a5d17418e0d3dd43211"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/1oxkwnc4hwhi2ndv6ekwy7np7"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/b937c0bab1b4c2abe59ff11a4"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/8baa7e33d305724e63bdd4fde"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/df2ta5ytg2zatj1q7y5e09u32"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/227ph3vim64tdwv5mfagjd22r"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/3319fb81634f391764631d447"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/66gfl06uj1v999z53szvu0exa"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/8e3a09267df655b2a759a1042"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/4tf01viblttjxqhu35yz90vw1"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/fe033da4eda2320720632a2c9"]
Issue-Commit: ab88f803a4e379013b8a9f7f3c950c61c3772e52

# Config rule control Severity GovernedResources and ParameterRequirementSummary diverge from catalog

> _Note: this issue was identified against the code base at commit `ab88f803a4e379013b8a9f7f3c950c61c3772e52`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListControls

## Description
ListControls answers 200 but the AWS::Config::ConfigRule controls it serves carry classification fields the catalog does not publish: Severity is downgraded or upgraded, GovernedResources is invented or dropped, and ParameterRequirementSummary is forced to NONE where the catalog says OPTIONAL. In every cause the catalog's control is reported missing and an emulator-minted ARN is reported extra under the same Config rule identifier, so the classification error travels with a fabricated identity. TEN divergence clusters are bundled here, numbered as causes below; ALL TEN must be closed and re-replayed before this issue is done.

Shared AWS basis (cited once; Smithy model services/service-com-amazonaws-controlcatalog-controlcatalog/smithy-model/com-amazonaws-controlcatalog-controlcatalog.json): ListControls is POST /list-controls, @readonly, 200, paginated (NextToken/MaxResults @range 1..100, items Controls), output Controls @required. ControlSummary.Severity is an enum (LOW|MEDIUM|HIGH|CRITICAL), ParameterRequirementSummary an enum (REQUIRED|OPTIONAL|NONE) that states whether the control requires, accepts or does not support parameters, GovernedResources the resource types the control governs, and Arn is @required with @pattern ^arn:(aws(?:[-a-z]*)?):(controlcatalog|controltower):[a-zA-Z0-9-]*::control/[0-9a-zA-Z_\-]+$. The catalog is an AWS-published, account-independent read-only dataset, so these values must be the catalog's own; the per-control values below are recorded behavior.

Shared Emulator basis: status matches (200); in each cause the catalog's summary is absent and an emulator-authored one is present with the classification field changed.

Shared Why: Severity ranks the control in every risk view and in the policy a client builds on top of the catalog, so a CRITICAL control reported HIGH (or a LOW one reported MEDIUM) silently re-prioritises remediation. ParameterRequirementSummary NONE tells a client the control takes no parameters, so it never offers or sends them and parameterised enablement is impossible. A GovernedResources list the catalog does not publish changes which controls a client finds when it filters by governed resource type.

Shared Fix: the ControlCatalog control seed dataset read by crate::state and the ControlSummary projection in the ListControls handler (services/service-com-amazonaws-controlcatalog-controlcatalog/src/ops/list_controls.rs) — Severity, ParameterRequirementSummary, GovernedResources (and the control's Arn/Aliases) must be read from the catalog record, never defaulted or derived.

Cause 1 of 10 — alias AWS-GR_EBS_SNAPSHOT_PUBLIC_RESTORABLE_CHECK / rule EBS_SNAPSHOT_PUBLIC_RESTORABLE_CHECK (step task#235). AWS: control/87qo8rsoettjrxjevmjqcw1tu, Severity CRITICAL, GovernedResources ['AWS::EC2::Volume'], Name 'Detect whether Amazon EBS snapshots are restorable by all AWS accounts'. Emulator: absent; an emulator-minted ARN appears with Severity HIGH and GovernedResources [] — a severity downgrade of a public-snapshot control plus a dropped governed resource type.

Cause 2 of 10 — alias CONFIG.CLOUDTRAIL.DT.8 / rule CLOUD_TRAIL_CLOUD_WATCH_LOGS_ENABLED (step task#155). AWS: control/9vnj728czsv4m6pflfqt21bw2, Severity LOW. Emulator: absent; minted ARN with Severity MEDIUM and a rewritten Description.

Cause 3 of 10 — alias CONFIG.ELASTICLOADBALANCING.DT.3 / rule ELBV2_ACM_CERTIFICATE_REQUIRED (step task#345). AWS: control/aazzef7lptl5umta6690p6u5c, ParameterRequirementSummary OPTIONAL. Emulator: absent; minted ARN with ParameterRequirementSummary NONE.

Cause 4 of 10 — alias CONFIG.IAM.DT.7 / rule IAM_POLICY_NO_STATEMENTS_WITH_ADMIN_ACCESS (step task#409). AWS: control/byrg7iwyl4azhecyhqtq8ocw1, ParameterRequirementSummary OPTIONAL, Severity HIGH. Emulator: absent; minted ARN with ParameterRequirementSummary NONE.

Cause 5 of 10 — alias CONFIG.RDS.DT.39 / rule RDS_SNAPSHOT_ENCRYPTED (step task#568). AWS: control/84hd48gyrb6e5ribkmsoub44c, GovernedResources [] (the catalog publishes no representable governed resource type for this control). Emulator: absent; minted ARN with GovernedResources invented as ['AWS::RDS::DBSnapshot'].

Cause 6 of 10 — alias AWS-GR_REDSHIFT_CLUSTER_PUBLIC_ACCESS_CHECK / rule REDSHIFT_CLUSTER_PUBLIC_ACCESS_CHECK (step task#577). AWS: control/1oxkwnc4hwhi2ndv6ekwy7np7, Severity CRITICAL. Emulator: absent; minted ARN with Severity HIGH.

Cause 7 of 10 — alias AWS-GR_RESTRICTED_COMMON_PORTS / rule RESTRICTED_INCOMING_TRAFFIC (step task#593). AWS: control/df2ta5ytg2zatj1q7y5e09u32, Severity CRITICAL, Name 'Disallow Internet connection via RDP', ParameterRequirementSummary OPTIONAL. Emulator: absent; minted ARN with Severity HIGH and the emulator's own Name/Description.

Cause 8 of 10 — alias CONFIG.ACCOUNT.DT.2 / rule ROOT_ACCOUNT_HARDWARE_MFA_ENABLED (step task#594). AWS: control/227ph3vim64tdwv5mfagjd22r, Aliases ['CONFIG.ACCOUNT.DT.2'], Severity CRITICAL, GovernedResources []. Emulator: absent; minted ARN with Aliases [] and a synthesized CreateTime (1663113600 against the catalog's 1535587200).

Cause 9 of 10 — alias AWS-GR_SAGEMAKER_NOTEBOOK_NO_DIRECT_INTERNET_ACCESS / rule SAGEMAKER_NOTEBOOK_NO_DIRECT_INTERNET_ACCESS (step task#664). AWS: control/66gfl06uj1v999z53szvu0exa, Severity HIGH. Emulator: absent; minted ARN with Severity MEDIUM.

Cause 10 of 10 — alias CONFIG.SECRETSMANAGER.DT.5 / rule SECRETSMANAGER_ROTATION_ENABLED_CHECK (step task#665). AWS: control/4tf01viblttjxqhu35yz90vw1, ParameterRequirementSummary OPTIONAL (the control is parameterised by the maximum allowed rotation frequency). Emulator: absent; minted ARN with ParameterRequirementSummary NONE and a Description that drops the frequency parameters entirely.

Traces: controlcatalog-catalog-list-controls-aws-config-configrule steps task#235, task#155, task#345, task#409, task#568, task#577, task#593, task#594, task#664, task#665.

Related open issue which does NOT carry the replay key of any cause above (owned_by empty and stub_path empty for all ten clusters): 20261106T180948Z-controlcatalog-listcontrols-replaces-aws-config-catalog-controls-with-emulator.md covers ten OTHER steps of this fixture. All ARNs, names and timestamps quoted above are the recording's values filling the ControlSummary template; the requirement is that the catalog's own values are served.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true, 1408 steps, 13 passed, 1395 mismatched, 89 divergence clusters. All ten clusters: service controlcatalog, operation ListControls, fixture controlcatalog-catalog-list-controls-aws-config-configrule, verdict mismatch, expected_status 200, actual_status 200, stub_path empty, owned_by empty.

Cause 1 (#task#235): $.Controls[Arn="arn:aws:controlcatalog:::control/87qo8rsoettjrxjevmjqcw1tu"] missing (expected 'Severity': 'CRITICAL', 'GovernedResources': ['AWS::EC2::Volume']); $.Controls[Arn="arn:aws:controlcatalog:::control/b012d555ea80072760b03e204"] extra ('Severity': 'HIGH', 'GovernedResources': [], 'Aliases': []).
Cause 2 (#task#155): expected control/9vnj728czsv4m6pflfqt21bw2 'Severity': 'LOW' missing; extra control/c541c3fac8ebcf2cf52bdb1a3 'Severity': 'MEDIUM'.
Cause 3 (#task#345): expected control/aazzef7lptl5umta6690p6u5c 'ParameterRequirementSummary': 'OPTIONAL' missing; extra control/86c13f1fc857518acd6d9bdf9 'ParameterRequirementSummary': 'NONE'.
Cause 4 (#task#409): expected control/byrg7iwyl4azhecyhqtq8ocw1 PRS 'OPTIONAL' missing; extra control/ce9470ef9d085fd7f70df5e0a PRS 'NONE', 'Aliases': [].
Cause 5 (#task#568): expected control/84hd48gyrb6e5ribkmsoub44c 'GovernedResources': [] missing; extra control/8f4441a5d17418e0d3dd43211 'GovernedResources': ['AWS::RDS::DBSnapshot'].
Cause 6 (#task#577): expected control/1oxkwnc4hwhi2ndv6ekwy7np7 'Severity': 'CRITICAL' missing; extra control/b937c0bab1b4c2abe59ff11a4 'Severity': 'HIGH'.
Cause 7 (#task#593): expected control/df2ta5ytg2zatj1q7y5e09u32 'Severity': 'CRITICAL' missing; extra control/8baa7e33d305724e63bdd4fde 'Severity': 'HIGH'.
Cause 8 (#task#594): expected control/227ph3vim64tdwv5mfagjd22r (Aliases ['CONFIG.ACCOUNT.DT.2'], Severity CRITICAL, CreateTime 1535587200.0) missing; extra control/3319fb81634f391764631d447 'Aliases': [], CreateTime 1663113600.
Cause 9 (#task#664): expected control/66gfl06uj1v999z53szvu0exa 'Severity': 'HIGH' missing; extra control/8e3a09267df655b2a759a1042 'Severity': 'MEDIUM'.
Cause 10 (#task#665): expected control/4tf01viblttjxqhu35yz90vw1 PRS 'OPTIONAL' missing; extra control/fe033da4eda2320720632a2c9 PRS 'NONE'.

No status or error-code divergence in any cause: all return the modeled 200 and the defect is the content of the @required Controls list.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus.
2. Replay fixture controlcatalog-catalog-list-controls-aws-config-configrule at step task#235 — POST /list-controls filtered to AWS::Config::ConfigRule. The page omits control/87qo8rsoettjrxjevmjqcw1tu and carries a minted ARN for EBS_SNAPSHOT_PUBLIC_RESTORABLE_CHECK with Severity HIGH and empty GovernedResources.
3. Repeat for the other nine representatives: task#155, task#345, task#409, task#568, task#577, task#593, task#594, task#664, task#665 — each shows the named Severity, ParameterRequirementSummary or GovernedResources divergence on a minted ARN.
4. Confirm the fix by re-replaying ALL TEN representatives: each control must appear under the catalog's ARN with the catalog's Severity, ParameterRequirementSummary and GovernedResources.
