Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-list-controls-aws-config-configrule
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-list-controls-aws-config-configrule controlcatalog-catalog-list-controls-aws-securityhub-securitycontrol
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[*]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/6yl9xxuky13vzaxi3n6lm73k5"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/1038h7d21e6g8i10g6v3xsssi"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/1038h7d21e6g8i10g6v3xsssi"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/8abhdcvu10syqrdyfgr2300px"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/c749f7ec712f9d3553ad439ea"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/73a73485f029120904f01f428"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/7laz29268m0wkplaj7z2fxxl"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/1d7faba51e721fdefb9c5455a"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/dfbp3c8jerraflh2g68427p4k"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/a29435e1d67a6b6ffc37f0b8d"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/bd9d97ndoue5lss1zjjiecle4"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/29387c501b177585f21b178c0"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/bn8ygh3zsy33drdeojmu09a4z"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/3ucly6t3j33t5zhbzy45l619n"]
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/8c87c96ac6184df91633d87f1"]
Issue-Commit: ab88f803a4e379013b8a9f7f3c950c61c3772e52

# ListControls pages omit catalog Config controls and rewrite Description and CreateTime

> _Note: this issue was identified against the code base at commit `ab88f803a4e379013b8a9f7f3c950c61c3772e52`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListControls

## Description
On the AWS::Config::ConfigRule page of the catalog, ListControls answers 200 but the page content does not reproduce the catalog: the very first element of the page is not the catalog's first control (a cluster that recurs across 1245 recorded steps), and for the controls that are present the Description is the emulator's own paraphrase and CreateTime is synthesized. NINE divergence clusters are bundled here, numbered as causes below; ALL NINE must be closed and re-replayed before this issue is done.

Shared AWS basis (cited once; Smithy model services/service-com-amazonaws-controlcatalog-controlcatalog/smithy-model/com-amazonaws-controlcatalog-controlcatalog.json): ListControls is POST /list-controls, @readonly, success 200, paginated (inputToken/outputToken NextToken, pageSize MaxResults @range 1..100, items Controls); ListControlsResponse.Controls is @required. ControlSummary.Description and Name are @required, CreateTime is documented as when the control was released as a governance capability, and Arn is @required with @pattern ^arn:(aws(?:[-a-z]*)?):(controlcatalog|controltower):[a-zA-Z0-9-]*::control/[0-9a-zA-Z_\-]+$. The catalog is an AWS-published read-only dataset, so the page must contain the catalog's controls, in the catalog's own text, at the catalog's own positions; the per-control values below are recorded behavior.

Shared Emulator basis: status matches (200); the page is served from an emulator-authored control set, so recorded entries are reported missing (sometimes with an emulator-minted ARN reported extra) and present entries carry reworded text or a different CreateTime.

Shared Why: a client paging the catalog with MaxResults/NextToken reads a different control set than AWS returns, so an inventory, a diff against a previous snapshot, or a 'first N controls' assertion all come out wrong; a paraphrased Description defeats any client or test that matches the published text; and a synthesized CreateTime misdates when the control became available.

Shared Fix: the ControlCatalog control seed dataset read by crate::state and the ControlSummary projection and page assembly in the ListControls handler (services/service-com-amazonaws-controlcatalog-controlcatalog/src/ops/list_controls.rs) — the page must enumerate the catalog's own records in the catalog's order, with Description, Name and CreateTime read from the record.

Cause 1 of 9 — the first element of the filtered page is not the catalog's first control (step task#3; this cluster recurs across 1245 recorded steps of the fixture). AWS: Controls[0] is the ACM expiry control — control/a6kwsgw65ayxiqg7dhzbz2n7w, Aliases ['CONFIG.ACM.DT.3'], Implementation AWS::Config::ConfigRule/ACM_CERTIFICATE_EXPIRATION_CHECK, ParameterRequirementSummary OPTIONAL, Severity MEDIUM, CreateTime 1779840000. Emulator: that entry is not at position 0 of the page at all (reported missing), so the whole page enumeration diverges from the catalog's.

Cause 2 of 9 — Private CA tagging control Description rewritten (step task#2). AWS: control/6yl9xxuky13vzaxi3n6lm73k5 Description 'Checks if AWS Private CA certificate authorities have tags. Optionally, you can specify tag keys. The rule is NON_COMPLIANT if there are no tags or if the specified tag keys are not present. The rule does not check for tags starting with \'aws:\'.'. Emulator: same ARN, but the Description is the emulator's own sentence ("...if the certificate authority has no tags, or if it is missing a tag key or value the rule's input parameter names."), dropping the documented aws:-prefix exclusion.

Cause 3 of 9 — Cognito advanced-security control CreateTime and Description (step task#181). AWS: control/1038h7d21e6g8i10g6v3xsssi, CreateTime 1715212800, Description 'Checks if an Amazon Cognito user pool has advanced security enabled. The rule is NON_COMPLIANT if advanced security is not enabled. Optionally, you can specify an advanced security mode for the rule to check.'. Emulator: same ARN, CreateTime 1621728000 and a paraphrased Description.

Cause 4 of 9 — alias CONFIG.ELASTICLOADBALANCING.DT.12 / rule ELB_LOGGING_ENABLED (step task#354). AWS: control/8abhdcvu10syqrdyfgr2300px, Aliases ['CONFIG.ELASTICLOADBALANCING.DT.12'], CreateTime 1534896000. Emulator: absent; an emulator-minted ARN appears with Aliases [], a reworded Description and CreateTime 1569283200.

Cause 5 of 9 — alias CONFIG.ELASTICLOADBALANCING.DT.16 / rule ELB_TLS_HTTPS_LISTENERS_ONLY (step task#356). AWS: control/7laz29268m0wkplaj7z2fxxl, Aliases ['CONFIG.ELASTICLOADBALANCING.DT.16'], CreateTime 1594166400. Emulator: absent; minted ARN with identical wording but Aliases [] and CreateTime 1671235200 — the text matches, so only the identity and the date are fabricated.

Cause 6 of 9 — alias CONFIG.REDSHIFT.DT.9 / rule REDSHIFT_REQUIRE_TLS_SSL (step task#582). AWS: control/dfbp3c8jerraflh2g68427p4k, GovernedResources ['AWS::Redshift::Cluster', 'AWS::Redshift::ClusterParameterGroup'], CreateTime 1590364800. Emulator: absent; minted ARN with only ['AWS::Redshift::Cluster'] and CreateTime 1637625600.

Cause 7 of 9 — alias CONFIG.S3.DT.14 / rule S3_BUCKET_LOGGING_ENABLED (step task#620). AWS: control/bd9d97ndoue5lss1zjjiecle4, Aliases ['CONFIG.S3.DT.14'], CreateTime 1475280000. Emulator: absent; minted ARN with Aliases [], a reworded Description and CreateTime 1565654400.

Cause 8 of 9 — alias CONFIG.SNS.DT.2 / rule SNS_ENCRYPTED_KMS (step task#679). AWS: control/bn8ygh3zsy33drdeojmu09a4z, Aliases ['CONFIG.SNS.DT.2'], Description naming the optional key ARN/alias/key-id parameters, CreateTime 1590364800. Emulator: absent; minted ARN with Aliases [], a Description that drops the parameter sentence, and CreateTime 1678492800.

Cause 9 of 9 — alias CONFIG.EC2.DT.49 / rule VPC_DEFAULT_SECURITY_GROUP_CLOSED (step task#710). AWS: control/3ucly6t3j33t5zhbzy45l619n, Aliases ['CONFIG.EC2.DT.49'], CreateTime 1534291200. Emulator: absent; minted ARN with Aliases [] and CreateTime 1567555200.

Traces: controlcatalog-catalog-list-controls-aws-config-configrule steps task#3 (1245 affected steps), task#2, task#181, task#354, task#356, task#582, task#620, task#679, task#710.

Related open issues which do NOT carry the replay key of any cause above (owned_by empty and stub_path empty for all nine clusters): 20261106T180948Z-controlcatalog-listcontrols-replaces-aws-config-catalog-controls-with-emulator.md and 20261113T140928Z-controlcatalog-taxonomy-and-control-text-are-emulator-authored.md describe the same seeded-dataset root cause over other steps. Every ARN, name and timestamp quoted above is the recording's value filling the ControlSummary template; the requirement is that the catalog's own values are served.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true, 1408 steps, 13 passed, 1395 mismatched, 89 divergence clusters. All nine clusters: service controlcatalog, operation ListControls, fixture controlcatalog-catalog-list-controls-aws-config-configrule, verdict mismatch, expected_status 200, actual_status 200, stub_path empty, owned_by empty.

Cause 1 (#task#3, affected_count 1245): $.Controls[0] missing (expected {'Aliases': ['CONFIG.ACM.DT.3'], 'Arn': 'arn:aws:controlcatalog:::control/a6kwsgw65ayxiqg7dhzbz2n7w', 'Behavior': 'DETECTIVE', 'CreateTime': 1779840000.0, 'Implementation': {'Identifier': 'ACM_CERTIFICATE_EXPIRATION_CHECK', 'Type': 'AWS::Config::ConfigRule'}, 'ParameterRequirementSummary': 'OPTIONAL', 'Severity': 'MEDIUM'}, got None).
Cause 2 (#task#2): $.Controls[Arn="arn:aws:controlcatalog:::control/6yl9xxuky13vzaxi3n6lm73k5"].Description value_mismatch (expected the catalog text naming optional tag keys and excluding 'aws:' tags, got the emulator's paraphrase).
Cause 3 (#task#181): $.Controls[Arn="...control/1038h7d21e6g8i10g6v3xsssi"].CreateTime value_mismatch (expected 1715212800.0, got 1621728000) and .Description value_mismatch.
Cause 4 (#task#354): expected control/8abhdcvu10syqrdyfgr2300px missing; extra control/c749f7ec712f9d3553ad439ea with 'Aliases': [] and CreateTime 1569283200.
Cause 5 (#task#356): expected control/7laz29268m0wkplaj7z2fxxl missing; extra control/73a73485f029120904f01f428 with 'Aliases': [] and CreateTime 1671235200.
Cause 6 (#task#582): expected control/dfbp3c8jerraflh2g68427p4k ('GovernedResources': ['AWS::Redshift::Cluster', 'AWS::Redshift::ClusterParameterGroup']) missing; extra control/1d7faba51e721fdefb9c5455a ('GovernedResources': ['AWS::Redshift::Cluster'], CreateTime 1637625600).
Cause 7 (#task#620): expected control/bd9d97ndoue5lss1zjjiecle4 missing; extra control/a29435e1d67a6b6ffc37f0b8d with 'Aliases': [] and CreateTime 1565654400.
Cause 8 (#task#679): expected control/bn8ygh3zsy33drdeojmu09a4z missing; extra control/29387c501b177585f21b178c0 with 'Aliases': [] and CreateTime 1678492800.
Cause 9 (#task#710): expected control/3ucly6t3j33t5zhbzy45l619n missing; extra control/8c87c96ac6184df91633d87f1 with 'Aliases': [] and CreateTime 1567555200.

No status or error-code divergence in any cause: all return the modeled 200.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus.
2. Replay fixture controlcatalog-catalog-list-controls-aws-config-configrule at step task#3 — POST /list-controls filtered to AWS::Config::ConfigRule. Controls[0] is not the catalog's ACM_CERTIFICATE_EXPIRATION_CHECK control; the same divergence recurs on 1245 steps of this fixture.
3. Replay task#2 and task#181 — the controls are present under the catalog's ARN but with a rewritten Description and, for task#181, CreateTime 1621728000 instead of 1715212800.
4. Replay task#354, task#356, task#582, task#620, task#679 and task#710 — each catalog control is absent and a minted ARN appears with an emptied alias list and a different CreateTime (task#582 additionally drops AWS::Redshift::ClusterParameterGroup from GovernedResources).
5. Confirm the fix by re-replaying ALL NINE representatives (task#3, task#2, task#181, task#354, task#356, task#582, task#620, task#679, task#710): the page must enumerate the catalog's controls in the catalog's order with the catalog's text, create times and governed resources.
