Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-get-control
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-get-control
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|GetControl|2xx->4xx|$.Aliases
Issue-Key: controlcatalog|GetControl|2xx->4xx|$.Arn
Issue-Key: controlcatalog|GetControl|2xx->4xx|$.Behavior
Issue-Key: controlcatalog|GetControl|2xx->4xx|$.CreateTime
Issue-Key: controlcatalog|GetControl|2xx->4xx|$.Description
Issue-Key: controlcatalog|GetControl|2xx->4xx|$.GovernedProviders
Issue-Key: controlcatalog|GetControl|2xx->4xx|$.GovernedResources
Issue-Key: controlcatalog|GetControl|2xx->4xx|$.Implementation
Issue-Key: controlcatalog|GetControl|2xx->4xx|$.Name
Issue-Key: controlcatalog|GetControl|2xx->4xx|$.ParameterRequirementSummary
Issue-Key: controlcatalog|GetControl|2xx->4xx|$.Parameters
Issue-Key: controlcatalog|GetControl|2xx->4xx|$.RegionConfiguration
Issue-Key: controlcatalog|GetControl|2xx->4xx|$.Severity
Issue-Key: controlcatalog|GetControl|2xx->4xx|status
Issue-Key: controlcatalog|GetControl|2xx->2xx|$.CreateTime
Issue-Key: controlcatalog|GetControl|2xx->2xx|$.Description
Issue-Key: controlcatalog|GetControl|2xx->2xx|$.RegionConfiguration.DeployableRegions[*]
Issue-Commit: ab88f803a4e379013b8a9f7f3c950c61c3772e52

# GetControl answers 404 for catalogued control ARNs and serves rewritten metadata

> _Note: this issue was identified against the code base at commit `ab88f803a4e379013b8a9f7f3c950c61c3772e52`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetControl

## Description
GetControl cannot resolve the control ARNs the AWS Control Catalog publishes: for all but one recorded identifier it answers 404 ResourceNotFoundException instead of the control record, and for the one it does resolve the record it returns is emulator-authored rather than the catalog's. TWO divergence clusters are bundled here, numbered as causes below; BOTH must be closed and re-replayed before this issue is done.

Shared AWS basis (Smithy model services/service-com-amazonaws-controlcatalog-controlcatalog/smithy-model/com-amazonaws-controlcatalog-controlcatalog.json): GetControl is POST /get-control, @readonly, success code 200. Input ControlArn is @required (ControlArn, @length 34..2048, @pattern ^arn:(aws(?:[-a-z]*)?):(controlcatalog|controltower):[a-zA-Z0-9-]*::control/[0-9a-zA-Z_\-]+$). Output GetControlResponse: Arn, Name, Description, Behavior and RegionConfiguration are @required, plus Aliases, Severity, Implementation, ParameterRequirementSummary, Parameters, CreateTime, GovernedResources, GovernedProviders. Modeled errors are AccessDeniedException (403), InternalServerException (500), ResourceNotFoundException (404), ThrottlingException (429), ValidationException (400) — so 404/ResourceNotFoundException is the modeled answer ONLY for an identifier the catalog does not hold. The catalog is an AWS-published, account-independent read-only dataset (the ARNs carry no account or region), so every ARN the catalog publishes must resolve, and the record returned must be the catalog's own. The control contents below are recorded behavior (the model fixes only the field set).

Cause 1 of 2 — GetControl 404s a published control ARN (fixture controlcatalog-catalog-get-control, step task#2; this cluster also covers steps task#3 through task#20, i.e. 19 further recorded GetControl calls). AWS: the request names a control ARN of the modeled form and the recorded response is 200 carrying that control — for the representative step, control/1ks6hwrtiiia8t2djn1iys62x with Aliases ['SH.REDSHIFT-SERVERLESS.DT.3'], Behavior DETECTIVE, Severity HIGH, Implementation {Type AWS::SecurityHub::SecurityControl, Identifier RedshiftServerless.3}, Name 'Redshift Serverless workgroups should prohibit public access', ParameterRequirementSummary NONE, Parameters [], GovernedProviders ['AWS'], GovernedResources ['AWS::RedshiftServerless::Workgroup'] and a RegionConfiguration with Scope REGIONAL. Emulator: HTTP 404 with no response body members at all (every output member reported missing), i.e. ResourceNotFoundException for an ARN the catalog publishes. The error class is the requirement here, not the message text: ResourceNotFoundException must be raised only when the catalog holds no control for the requested ARN; the fixed text of AWS's message plus the requested ARN as its only variable part is what the emulator may serve in that case. Why: a client that discovers a control from the catalog (or hard-codes a documented control ARN, as Control Tower and Security Hub integrations do) cannot read it at all; the only ARNs the emulator resolves are the ones it minted itself, so nothing that works against AWS works here.

Cause 2 of 2 — the one control GetControl does resolve is served with rewritten text, a synthesized CreateTime and the wrong deployable-region set (step task#1). AWS: for the Cognito advanced-security control the recorded response gives Description 'Checks if an Amazon Cognito user pool has advanced security enabled. The rule is NON_COMPLIANT if advanced security is not enabled. Optionally, you can specify an advanced security mode for the rule to check.', CreateTime 1715212800, and RegionConfiguration.DeployableRegions as the catalog's own ordered list for that control (beginning ap-northeast-1, ap-northeast-2, ap-northeast-3, ap-south-1, ap-southeast-1, ...). Emulator: 200, but Description is the emulator's own paraphrase ("...if advanced security is not enabled for the user pool, or if its security mode is weaker than the mode the rule's input parameter names."), CreateTime is 1621728000, and DeployableRegions is a different, service-wide region list (beginning af-south-1, ap-east-1, ap-east-2, ...) rather than the regions the catalog publishes for this control. Why: CreateTime is documented as when the control was released as a governance capability, so a client auditing control availability over time reads a false date; DeployableRegions drives where a client may deploy the control, so an emulator-wide region list makes a client attempt deployments AWS rejects; and the rewritten Description breaks any client or test that matches the catalog's published text.

Fix: the ControlCatalog control seed dataset and the GetControl lookup in the controlcatalog service crate — the ARN index must resolve every control the seed publishes (services/service-com-amazonaws-controlcatalog-controlcatalog/src/ops/get_control.rs reads the record out of crate::state), and the record's Description, CreateTime and RegionConfiguration.DeployableRegions must be read from the catalog record instead of synthesized. The 404 path must remain reachable only for ARNs the seed does not hold.

Traces: controlcatalog-catalog-get-control#task#2 (cause 1, 19 further affected steps task#3..task#20), controlcatalog-catalog-get-control#task#1 (cause 2).

Related open issue which does NOT carry either cluster's replay key (the replay reports owned_by empty and stub_path empty for both): 20261101T194508Z-controlcatalog-serves-a-fabricated-placeholder-control-dataset-getcontrol.md describes the same fabricated-dataset root cause. Every ARN, id, name and timestamp quoted above is the recording's value filling the response template; the requirement is that the catalog's own values are served, not these literals.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true (/__admin/health 200 and the :45687 data listener accepts), 1408 steps, 13 passed, 1395 mismatched, 89 divergence clusters. Both clusters: service controlcatalog, operation GetControl, verdict mismatch, stub_path empty, owned_by empty.

Cause 1 — fixture controlcatalog-catalog-get-control step task#2, expected_status 200, actual_status 404, affected_count 19 (task#2 .. task#20): "status: expected 200, got 404"; $.Aliases missing (expected ['SH.REDSHIFT-SERVERLESS.DT.3'], got None); $.Arn missing (expected 'arn:aws:controlcatalog:::control/1ks6hwrtiiia8t2djn1iys62x'); $.Behavior missing (expected 'DETECTIVE'); $.CreateTime missing (expected 1765929600.0); $.Description, $.GovernedProviders (['AWS']), $.GovernedResources (['AWS::RedshiftServerless::Workgroup']), $.Implementation ({'Identifier': 'RedshiftServerless.3', 'Type': 'AWS::SecurityHub::SecurityControl'}), $.Name ('Redshift Serverless workgroups should prohibit public access'), $.ParameterRequirementSummary ('NONE'), $.Parameters ([]), $.RegionConfiguration (Scope 'REGIONAL' with 22 DeployableRegions), $.Severity ('HIGH') all missing — the emulator returned no body members because it answered 404.

Cause 2 — fixture controlcatalog-catalog-get-control step task#1, expected_status 200, actual_status 200: $.CreateTime value_mismatch (expected 1715212800.0, got 1621728000); $.Description value_mismatch (expected 'Checks if an Amazon Cognito user pool has advanced security enabled. The rule is NON_COMPLIANT if advanced security is not enabled. Optionally, you can specify an advanced security mode for the rule to check.', got "Checks if an Amazon Cognito user pool has advanced security enabled. The rule is NON_COMPLIANT if advanced security is not enabled for the user pool, or if its security mode is weaker than the mode the rule's input parameter names."); $.RegionConfiguration.DeployableRegions[0..32] value_mismatch across the whole list (expected[0] 'ap-northeast-1', got 'af-south-1'; expected[1] 'ap-northeast-2', got 'ap-east-1'; expected[2] 'ap-northeast-3', got 'ap-east-2'; and so on for 33 positions).

## Steps to Reproduce
1. Boot the emulator from this checkout (controlcatalog is bootstrapped; /__admin/health answers 200) and replay the simparity-wire-aws-v1 wire corpus.
2. Replay fixture controlcatalog-catalog-get-control step task#2 — POST /get-control with the recorded ControlArn. The emulator answers 404 (ResourceNotFoundException) where the recording has 200 with the control record. Steps task#3..task#20 behave the same for their own ARNs.
3. Replay step task#1 of the same fixture — the one ARN the emulator does resolve. It answers 200 but with a paraphrased Description, CreateTime 1621728000 instead of 1715212800, and a DeployableRegions list that is the emulator's region inventory rather than the catalog's list for that control.
4. Confirm the fix by re-replaying BOTH representatives (controlcatalog-catalog-get-control task#2 and task#1): every published control ARN must resolve to 200 with the catalog's own Name, Description, Behavior, Severity, Implementation, Aliases, CreateTime, Parameters, GovernedResources/Providers and RegionConfiguration.DeployableRegions, and 404/ResourceNotFoundException must be returned only for an ARN the catalog does not hold.
