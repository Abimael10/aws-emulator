Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-list-controls-aws-config-configrule
Issue-Step: task#35
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-list-controls-aws-config-configrule
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/aayte0p9vah5rp97z6yfbytl2"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/b4c045e5fa1e6405cec122e16"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/7305ygy72ra2amwqjl1bqrmrw"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/8f38c889ca59939fbd189f36c"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/a0ufrfbomg90qbskltuop262u"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/b5059d436929d334eb38b6bdf"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/43732cd47b605af8307808e0a"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/4v7xtm83uvvyulk1wwpm4qm3s"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/5813118860648d85cfd160b5b"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/8qmzz7l2841u6bvwpkiug38ta"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/aeellyghb27pbehyzua1nyena"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/e1746742fddeb610a84028873"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/b2gzofz99eb7nsuj5g8wcimse"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/cc0501e764f8d4beb5baec311"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/1fvktjhpo9wdpt3pjb2wsz7nt"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/8be40ac54d760235c4a40ece0"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/70u9gks728rcr33237rckvzbo"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/879d271b159d1c01c6b32479f"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/3efaa7f29f9c6c81b85753258"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/4jc77cq1lcr7g64xywwypykv8"]
Issue-Commit: ab88f803a4e379013b8a9f7f3c950c61c3772e52

# Config rule controls are served under emulator-minted ARNs with emptied Aliases

> _Note: this issue was identified against the code base at commit `ab88f803a4e379013b8a9f7f3c950c61c3772e52`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListControls

## Description
ListControls answers 200, but for each control the AWS Control Catalog publishes with an AWS::Config::ConfigRule implementation the page carries an emulator-authored ControlSummary instead of the catalog's: a different, emulator-minted control ARN, an EMPTY Aliases list where the catalog publishes the control's alternative identifier, and a rewritten Name/Description with a synthesized CreateTime. The replay reports the catalog's summary missing and the emulator's extra under the same Implementation.Identifier — the signature of a fabricated identity rather than a filter or ordering fault. TEN divergence clusters are bundled here, numbered as causes below; ALL TEN must be closed and re-replayed before this issue is done.

Shared AWS basis (cited once, applies to every cause; Smithy model services/service-com-amazonaws-controlcatalog-controlcatalog/smithy-model/com-amazonaws-controlcatalog-controlcatalog.json): ListControls is POST /list-controls, @readonly, success code 200, paginated (inputToken/outputToken NextToken @length max 1024, pageSize MaxResults @range 1..100, items Controls); output ListControlsResponse.Controls is @required. A ControlSummary carries Arn (@required, ControlArn @length 34..2048, @pattern ^arn:(aws(?:[-a-z]*)?):(controlcatalog|controltower):[a-zA-Z0-9-]*::control/[0-9a-zA-Z_\-]+$), Name and Description (@required), Aliases, Behavior, Severity, ParameterRequirementSummary, Implementation{Type,Identifier}, CreateTime, GovernedResources, GovernedProviders. The catalog is an AWS-published, account-independent read-only dataset, so each summary must reproduce the catalog's own identity and metadata for that control; the per-control values below are recorded behavior.

Shared Emulator basis: status matches (200); in every cause the catalog's control is absent from the page and an emulator-minted ARN appears in its place with Aliases [] and the emulator's own wording.

Shared Why: every control ARN a client harvests from the emulator's ListControls exists in no AWS account, so it is rejected by the real service — and it is the only id the emulator's own GetControl can resolve. An emptied Aliases list breaks the alias-to-control lookup (CONFIG.* / AWS-GR_* identifiers) that Control Tower and Security Hub clients use to find a control, and the paraphrased Name/Description defeats any client or test matching the published catalog text.

Shared Fix: the ControlCatalog control seed dataset read by crate::state and the ControlSummary projection in the ListControls handler (services/service-com-amazonaws-controlcatalog-controlcatalog/src/ops/list_controls.rs) — Arn, Aliases, Name, Description and CreateTime must be read from the catalog record instead of synthesized.

Cause 1 of 10 — alias CONFIG.APIGATEWAY.DT.10 / rule API_GW_EXECUTION_LOGGING_ENABLED (step task#35). AWS: control/aayte0p9vah5rp97z6yfbytl2, Aliases ['CONFIG.APIGATEWAY.DT.10'], Name 'Checks if all methods in Amazon API Gateway stages have logging enabled', CreateTime 1779840000. Emulator: that entry is absent; an emulator-minted ARN appears with Aliases [], a reworded Name/Description and CreateTime 1576195200.

Cause 2 of 10 — alias CONFIG.KMS.DT.3 / rule CMK_BACKING_KEY_ROTATION_ENABLED (step task#159). AWS: control/7305ygy72ra2amwqjl1bqrmrw, Aliases ['CONFIG.KMS.DT.3'], ParameterRequirementSummary NONE, CreateTime 1540339200. Emulator: absent; emulator-minted ARN with Aliases [], reworded text and CreateTime 1661040000.

Cause 3 of 10 — alias CONFIG.DYNAMODB.DT.4 / rule DYNAMODB_TABLE_ENCRYPTED_KMS (step task#226). AWS: control/a0ufrfbomg90qbskltuop262u, Aliases ['CONFIG.DYNAMODB.DT.4'], CreateTime 1590364800. Emulator: absent; emulator-minted ARN with Aliases [], reworded Description and CreateTime 1693785600.

Cause 4 of 10 — alias AWS-GR_EC2_INSTANCE_NO_PUBLIC_IP / rule EC2_INSTANCE_NO_PUBLIC_IP (step task#250). AWS: control/4v7xtm83uvvyulk1wwpm4qm3s, Aliases ['AWS-GR_EC2_INSTANCE_NO_PUBLIC_IP'], Name 'Detect whether any Amazon EC2 instance has an associated public IPv4 address'. Emulator: absent; emulator-minted ARN with Aliases [] and the emulator's own Name/Description.

Cause 5 of 10 — alias CONFIG.ELASTICFILESYSTEM.DT.8 / rule EFS_ENCRYPTED_CHECK (step task#312). AWS: control/8qmzz7l2841u6bvwpkiug38ta, Aliases ['CONFIG.ELASTICFILESYSTEM.DT.8'], CreateTime 1563408000. Emulator: absent; emulator-minted ARN with Aliases [] and CreateTime 1541635200.

Cause 6 of 10 — alias AWS-GR_EKS_ENDPOINT_NO_PUBLIC_ACCESS / rule EKS_ENDPOINT_NO_PUBLIC_ACCESS (step task#325). AWS: control/aeellyghb27pbehyzua1nyena, Aliases ['AWS-GR_EKS_ENDPOINT_NO_PUBLIC_ACCESS'], Name 'Detect whether an Amazon EKS endpoint is blocked from public access'. Emulator: absent; emulator-minted ARN with Aliases [] and reworded Name/Description.

Cause 7 of 10 — alias AWS-GR_LAMBDA_FUNCTION_PUBLIC_ACCESS_PROHIBITED / rule LAMBDA_FUNCTION_PUBLIC_ACCESS_PROHIBITED (step task#469). AWS: control/b2gzofz99eb7nsuj5g8wcimse, Aliases ['AWS-GR_LAMBDA_FUNCTION_PUBLIC_ACCESS_PROHIBITED'], Name 'Detect whether the AWS Lambda function policy attached to the Lambda resource blocks public access', Severity CRITICAL. Emulator: absent; emulator-minted ARN with Aliases [] and reworded text.

Cause 8 of 10 — alias AWS-GR_MFA_ENABLED_FOR_IAM_CONSOLE_ACCESS / rule MFA_ENABLED_FOR_IAM_CONSOLE_ACCESS (step task#484). AWS: control/1fvktjhpo9wdpt3pjb2wsz7nt, Aliases ['AWS-GR_MFA_ENABLED_FOR_IAM_CONSOLE_ACCESS'], Name 'Disallow console access to IAM users without MFA'. Emulator: absent; emulator-minted ARN with Aliases [] and the emulator's own Name/Description.

Cause 9 of 10 — alias CONFIG.OPENSEARCH.DT.9 / rule OPENSEARCH_ENCRYPTED_AT_REST (step task#528). AWS: control/70u9gks728rcr33237rckvzbo, Aliases ['CONFIG.OPENSEARCH.DT.9'], CreateTime 1637193600. Emulator: absent; emulator-minted ARN with Aliases [] and CreateTime 1660867200.

Cause 10 of 10 — alias AWS-GR_RDS_INSTANCE_PUBLIC_ACCESS_CHECK / rule RDS_INSTANCE_PUBLIC_ACCESS_CHECK (step task#552). AWS: control/4jc77cq1lcr7g64xywwypykv8, Aliases ['AWS-GR_RDS_INSTANCE_PUBLIC_ACCESS_CHECK'], Name 'Disallow public access to RDS database instances'. Emulator: absent; emulator-minted ARN with Aliases [] and reworded Name/Description.

Traces: controlcatalog-catalog-list-controls-aws-config-configrule steps task#35, task#159, task#226, task#250, task#312, task#325, task#469, task#484, task#528, task#552.

Related open issue which does NOT carry the replay key of any cause above (the replay reports owned_by empty and stub_path empty for all ten clusters): 20261106T180948Z-controlcatalog-listcontrols-replaces-aws-config-catalog-controls-with-emulator.md bundles ten OTHER steps of this same fixture. Fixing the seed will likely close these ten too, but they are separate clusters and each must be re-replayed. All ARNs, names and timestamps quoted above are the recording's values filling the ControlSummary template; the requirement is that the catalog's own values are served, not these literals.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true, 1408 steps, 13 passed, 1395 mismatched, 89 divergence clusters. All ten clusters: service controlcatalog, operation ListControls, fixture controlcatalog-catalog-list-controls-aws-config-configrule, verdict mismatch, expected_status 200, actual_status 200, stub_path empty, owned_by empty.

Cause 1 (#task#35): $.Controls[Arn="arn:aws:controlcatalog:::control/aayte0p9vah5rp97z6yfbytl2"] missing (expected Aliases ['CONFIG.APIGATEWAY.DT.10'], CreateTime 1779840000.0, Implementation API_GW_EXECUTION_LOGGING_ENABLED); $.Controls[Arn="arn:aws:controlcatalog:::control/b4c045e5fa1e6405cec122e16"] extra ('Aliases': [], CreateTime 1576195200, reworded Description).
Cause 2 (#task#159): expected control/7305ygy72ra2amwqjl1bqrmrw ('CONFIG.KMS.DT.3') missing; extra control/8f38c889ca59939fbd189f36c with 'Aliases': [] and CreateTime 1661040000.
Cause 3 (#task#226): expected control/a0ufrfbomg90qbskltuop262u ('CONFIG.DYNAMODB.DT.4') missing; extra control/b5059d436929d334eb38b6bdf with 'Aliases': [] and CreateTime 1693785600.
Cause 4 (#task#250): expected control/4v7xtm83uvvyulk1wwpm4qm3s ('AWS-GR_EC2_INSTANCE_NO_PUBLIC_IP') missing; extra control/43732cd47b605af8307808e0a with 'Aliases': [].
Cause 5 (#task#312): expected control/8qmzz7l2841u6bvwpkiug38ta ('CONFIG.ELASTICFILESYSTEM.DT.8') missing; extra control/5813118860648d85cfd160b5b with 'Aliases': [] and CreateTime 1541635200.
Cause 6 (#task#325): expected control/aeellyghb27pbehyzua1nyena ('AWS-GR_EKS_ENDPOINT_NO_PUBLIC_ACCESS') missing; extra control/e1746742fddeb610a84028873 with 'Aliases': [].
Cause 7 (#task#469): expected control/b2gzofz99eb7nsuj5g8wcimse ('AWS-GR_LAMBDA_FUNCTION_PUBLIC_ACCESS_PROHIBITED') missing; extra control/cc0501e764f8d4beb5baec311 with 'Aliases': [].
Cause 8 (#task#484): expected control/1fvktjhpo9wdpt3pjb2wsz7nt ('AWS-GR_MFA_ENABLED_FOR_IAM_CONSOLE_ACCESS') missing; extra control/8be40ac54d760235c4a40ece0 with 'Aliases': [].
Cause 9 (#task#528): expected control/70u9gks728rcr33237rckvzbo ('CONFIG.OPENSEARCH.DT.9') missing; extra control/879d271b159d1c01c6b32479f with 'Aliases': [] and CreateTime 1660867200.
Cause 10 (#task#552): expected control/4jc77cq1lcr7g64xywwypykv8 ('AWS-GR_RDS_INSTANCE_PUBLIC_ACCESS_CHECK') missing; extra control/3efaa7f29f9c6c81b85753258 with 'Aliases': [].

No status or error-code divergence in any cause: all return the modeled 200 and the defect is entirely the content of the @required Controls list.

## Steps to Reproduce
1. Boot the emulator from this checkout (controlcatalog is bootstrapped) and replay the simparity-wire-aws-v1 wire corpus.
2. Replay fixture controlcatalog-catalog-list-controls-aws-config-configrule at step task#35 — POST /list-controls with the implementation filter for AWS::Config::ConfigRule. The page omits control/aayte0p9vah5rp97z6yfbytl2 and carries an emulator-minted ARN for rule API_GW_EXECUTION_LOGGING_ENABLED with an empty Aliases list.
3. Repeat for the other nine representatives of the same fixture: task#159, task#226, task#250, task#312, task#325, task#469, task#484, task#528, task#552. In each the catalog's control is absent and an emulator-authored summary appears under a minted ARN with Aliases [].
4. Confirm the fix by re-replaying ALL TEN representatives: each page must carry the catalog's own control ARN, its published alias, name, description and create time.
