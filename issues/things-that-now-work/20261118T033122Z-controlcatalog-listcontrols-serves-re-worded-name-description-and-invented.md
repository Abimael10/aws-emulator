Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-list-controls-aws-config-configrule
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-list-controls-aws-config-configrule
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/4x11tie7h25sws4g7ilakhcb7"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/497wrm2xnk1wxlf4obrdo7mej"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/497wrm2xnk1wxlf4obrdo7mej"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/497wrm2xnk1wxlf4obrdo7mej"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/1ln29qglgkigm9ajfka6u2p2u"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/1ln29qglgkigm9ajfka6u2p2u"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/1ln29qglgkigm9ajfka6u2p2u"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/8sdhbqfe2fsmh3b1i2b24xaic"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/8sdhbqfe2fsmh3b1i2b24xaic"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/8sdhbqfe2fsmh3b1i2b24xaic"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/85z4kpyxbo5nepzfwbfwwxjx7"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/85z4kpyxbo5nepzfwbfwwxjx7"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/85z4kpyxbo5nepzfwbfwwxjx7"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/503uicglhjkokaajywfpt6ros"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/503uicglhjkokaajywfpt6ros"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/9xv17a25orapqa36e19o5b8lp"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/9xv17a25orapqa36e19o5b8lp"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/9xv17a25orapqa36e19o5b8lp"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/dmb52g7tw004w9iede1pgb1fj"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/dmb52g7tw004w9iede1pgb1fj"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/dmb52g7tw004w9iede1pgb1fj"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/9utgo6tl86bg3e9jaq073vmqw"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/9utgo6tl86bg3e9jaq073vmqw"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/7z1uzm6s8qk7ym7m0sm56cq0u"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/7z1uzm6s8qk7ym7m0sm56cq0u"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/6rilu41n0gb9w6mxrkyewoer4"].CreateTime
Issue-Commit: 027d9e9288dfd341f60d75b6ca76c3164fcd4263

# ListControls serves re-worded Name/Description and invented CreateTime for ten further Config rule records

> _Note: this issue was identified against the code base at commit `027d9e9288dfd341f60d75b6ca76c3164fcd4263`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListControls

## Description
ListControls answers 200 with the correct wire shape, but for ten FURTHER AWS::Config::ConfigRule catalog records (none of them owned by any open issue, per the replay report) the served ControlSummary carries emulator-authored Name/Description prose in place of the catalog's published text, and a CreateTime that is not the record's catalog publication instant. All ten share one root cause - the seeded Config-rule catalog rows hold re-written text and minted timestamps - so they are bundled here as 10 causes. A fixer must close all ten.

SHARED STATEMENT OF THE FAULT

AWS: Smithy (com.amazonaws.controlcatalog#ControlCatalog): ListControls -> POST /list-controls, HTTP 200, paginated (inputToken NextToken / outputToken NextToken / pageSize MaxResults / items Controls); output member Controls is @required list<ControlSummary>. ControlSummary$Name is @required, documented "The display name of the control"; ControlSummary$Description is @required, documented "A description of the control, as it may appear in the console. Describes the functionality of the control"; ControlSummary$CreateTime is documented "A timestamp that notes the time when the control was released (start of its life) as a governance capability in Amazon Web Services". All three are AWS-owned catalog content keyed by the control ARN - the exact published string and that record's own release instant - not values a caller supplies and not values a server may re-phrase or mint. For the per-record values the recording is the authority (recorded behavior only for the specific strings and epochs); the model is the authority for these members being AWS-owned catalog content of the ARN.

Emulator: for each ARN named below the emulator returns the right ARN inside a 200 ListControls page, but with a differently worded Description (clauses re-ordered, qualifying conditions added or dropped, "Checks if" turned into "Checks whether" and vice versa, "AWS" expanded or contracted, the rule's evaluation criterion restated in different terms), frequently a correspondingly re-worded Name, and in nine of the ten cases a CreateTime that is a completely different epoch instant from the catalog's.

Why: callers treat ControlCatalog Name/Description as published AWS content and CreateTime as the catalog record's identity in time. Compliance-evidence exports, console and portal renderings, catalog-diffing, control-documentation generation and "controls released since <date>" selections all read exactly these fields. A paraphrase that adds or drops a qualifying clause misstates what the control actually evaluates (e.g. the CloudWatch Logs encryption record below is re-described as requiring a KMS CMK rather than "any AWS KMS key or a specified AWS KMS key Id", which changes which resources a reader believes the control fails); a wrong CreateTime silently moves a control into or out of any date-scoped selection.

Fix: the seeded catalog data behind the ListControls handler in the controlcatalog service crate - the stored ControlSummary rows for AWS::Config::ConfigRule implementations must hold the published Name and Description strings verbatim and the catalog's own CreateTime, so the handler and serializer pass them through unchanged. This is a state/data-fidelity defect, not a serialization defect: status, ARN, paging and shape are all already correct.

Cause 1 of 10 - control arn:aws:controlcatalog:::control/4x11tie7h25sws4g7ilakhcb7 (account is part of AWS Organizations). Description only. AWS (recorded): 'Rule checks whether AWS account is part of AWS Organizations. The rule is NON_COMPLIANT if the AWS account is not part of AWS Organizations or AWS Organizations master account ID does not match rule parameter MasterAccountId.' Emulator: 'Checks if an AWS account is part of AWS Organizations. The rule is NON_COMPLIANT if an AWS account is not part of AWS Organizations or AWS Organizations master account ID does not match rule parameter MasterAccountId.' Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#1.

Cause 2 of 10 - control arn:aws:controlcatalog:::control/497wrm2xnk1wxlf4obrdo7mej (CloudWatch log group encryption). Name, Description and CreateTime. AWS (recorded): Name 'Checks if Amazon CloudWatch Log Groups are encrypted with any AWS KMS key or a specified AWS KMS key Id'; Description 'Checks if Amazon CloudWatch Log Groups are encrypted with any AWS KMS key or a specified AWS KMS key Id. The rule is NON_COMPLIANT if a CloudWatch Log Group is not encrypted with a KMS key or is encrypted with a KMS key not supplied in the rule parameter.'; CreateTime 1563408000. Emulator: Name 'Checks if a log group in Amazon CloudWatch Logs is encrypted with a AWS Key Management Service (KMS) managed Customer Master Keys (CMK)'; Description restated around a "KMS managed Customer Master Key" with 'The rule is NON_COMPLIANT if no AWS KMS CMK is configured on the log groups.'; CreateTime 1687478400. Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#153.

Cause 3 of 10 - two CloudTrail-enablement records in the same page, arn:aws:controlcatalog:::control/8sdhbqfe2fsmh3b1i2b24xaic and arn:aws:controlcatalog:::control/1ln29qglgkigm9ajfka6u2p2u. Name, Description and CreateTime on both. AWS (recorded), 8sdhbq...: Name 'Detect whether an AWS account has at least one AWS CloudTrail trail enabled for a governed AWS Region.'; Description 'Detects whether an AWS account has AWS CloudTrail enabled has at least one AWS CloudTrail trail enabled for a governed AWS Region. This rule is NON_COMPLIANT if no CloudTrail trails are enabled in an AWS account for a governed AWS Region.'; CreateTime 1657584000. Emulator: Name 'Detect whether CloudTrail is enabled on member accounts'; Description re-cast around organization member accounts and management events; CreateTime 1672444800. AWS (recorded), 1ln29q...: Description 'This control detects whether a shared AWS account in the Security organizational unit has at least one AWS CloudTrail trail enabled for a governed AWS Region. This rule is NON_COMPLIANT if no CloudTrail trails are enabled in an AWS account for a governed AWS Region.'; CreateTime 1657584000. Emulator: the shared-account text re-cast around "records no management events through a CloudTrail trail" with a matching Name rewrite; CreateTime 1651363200. Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#156.

Cause 4 of 10 - control arn:aws:controlcatalog:::control/85z4kpyxbo5nepzfwbfwwxjx7 (CloudTrail log file validation). Name, Description and CreateTime. AWS (recorded): Name 'Checks if AWS CloudTrail creates a signed digest file with logs'; Description 'Checks if AWS CloudTrail creates a signed digest file with logs. AWS recommends that the file validation must be enabled on all trails. The rule is NON_COMPLIANT if the validation is not enabled.'; CreateTime 1534982400. Emulator: 'Checks if' becomes 'Checks whether' in both members and NON_COMPLIANT is lower-cased to 'noncompliant'; CreateTime 1684022400. Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#158.

Cause 5 of 10 - control arn:aws:controlcatalog:::control/503uicglhjkokaajywfpt6ros (EBS volume encryption). Description and CreateTime. AWS (recorded): Description 'Secure EBS volumes attached to EC2 instances through encryption.'; CreateTime 1542240000. Emulator: Description 'Secure your data by encrypting the EBS volumes attached to your EC2 instances.'; CreateTime 1700092800. Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#362.

Cause 6 of 10 - control arn:aws:controlcatalog:::control/9xv17a25orapqa36e19o5b8lp (GuardDuty enabled). Name, Description and CreateTime. AWS (recorded): Name 'Checks if Amazon GuardDuty is enabled in your AWS account and AWS Region'; Description 'Checks if Amazon GuardDuty is enabled in your AWS account and AWS Region. If you provide an AWS account for centralization, the rule evaluates the GuardDuty results in the centralized account. The rule is COMPLIANT when GuardDuty is enabled.'; CreateTime 1534809600. Emulator: 'Checks if' -> 'Checks whether', 'AWS Region' -> 'region', 'GuardDuty' -> 'Amazon GuardDuty' throughout both members; CreateTime 1675641600. Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#395.

Cause 7 of 10 - control arn:aws:controlcatalog:::control/dmb52g7tw004w9iede1pgb1fj (IAM account password policy). Name, Description and CreateTime. AWS (recorded): Name 'Checks if the account password policy for AWS Identity and Access Management (IAM) users meets the specified requirements indicated in the parameters'; Description 'Checks if the account password policy for AWS Identity and Access Management (IAM) users meets the specified requirements indicated in the parameters. The rule is NON_COMPLIANT if the account password policy does not meet the specified requirements.'; CreateTime 1474416000. Emulator: Name truncated to 'Checks if the account password policy for IAM users meets the specified requirements' and the 'AWS Identity and Access Management' expansion dropped from the Description; CreateTime 1582848000. Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#407.

Cause 8 of 10 - control arn:aws:controlcatalog:::control/9utgo6tl86bg3e9jaq073vmqw (root access key check). Description and CreateTime. AWS (recorded): Description 'Checks if the root user access key is available. The rule is COMPLIANT if the user access key does not exist. Otherwise, NON_COMPLIANT.'; CreateTime 1534377600. Emulator: the final 'Otherwise, NON_COMPLIANT.' sentence is dropped; CreateTime 1653696000. Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#411.

Cause 9 of 10 - control arn:aws:controlcatalog:::control/7z1uzm6s8qk7ym7m0sm56cq0u (IAM user MFA). Description and CreateTime. AWS (recorded): Description 'Protect your account by requiring multi-factor authentication (MFA) for any AWS Identity and Access Management (IAM) user using the account. MFA adds an extra authentication code on top of a user name and password.'; CreateTime 1564444800. Emulator: an entirely different sentence, 'Reduces the risk of unauthorized access by disallowing access to IAM users that have no multi-factor authentication (MFA) device.'; CreateTime 1627171200. Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#416.

Cause 10 of 10 - control arn:aws:controlcatalog:::control/6rilu41n0gb9w6mxrkyewoer4. CreateTime only; Name and Description match. AWS (recorded): CreateTime 1542240000. Emulator: 1557792000. This cause isolates the timestamp half of the fault: the seeded row's CreateTime is minted independently of the catalog even when the text is right.
Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#423.

Relation to open issues: 20261116T095506Z-controlcatalog-listcontrols-rewrites-aws-config-rule-control-name.md, 20261115T014344Z-controlcatalog-listcontrols-pages-omit-catalog-config-controls-and-rewrite.md, 20261116T001437Z-controlcatalog-listcontrols-rewrites-published-security-hub-control-description.md and 20261113T140928Z-controlcatalog-taxonomy-and-control-text-are-emulator-authored.md describe the same class of rewrite for OTHER catalog records; the replay reports no owner for the ten records above, so they are net-new and filed here. The correction should be applied to the whole seeded AWS Config rule catalogue, not only these ten rows.

## Evidence
Replay of fixture controlcatalog-catalog-list-controls-aws-config-configrule (trace set simparity-wire-aws-v1, emulator up, /__admin/health 200): expected status 200, actual status 200, verdict mismatch on ten unowned clusters (owned_by empty, stub_path empty for all ten). Each cluster's divergence key has the form service=controlcatalog|ListControls|status=200->200|keys=$.Controls[Arn="..."].CreateTime,$.Controls[Arn="..."].Description(,.Name).

task#1, control 4x11tie7h25sws4g7ilakhcb7:
  $.Controls[...].Description: value_mismatch
    expected 'Rule checks whether AWS account is part of AWS Organizations. The rule is NON_COMPLIANT if the AWS account is not part of AWS Organizations or AWS Organizations master account ID does not match rule parameter MasterAccountId.'
    got 'Checks if an AWS account is part of AWS Organizations. The rule is NON_COMPLIANT if an AWS account is not part of AWS Organizations or AWS Organizations master account ID does not match rule parameter MasterAccountId.'

task#153, control 497wrm2xnk1wxlf4obrdo7mej:
  CreateTime: expected 1563408000.0, got 1687478400
  Description: expected 'Checks if Amazon CloudWatch Log Groups are encrypted with any AWS KMS key or a specified AWS KMS key Id. The rule is NON_COMPLIANT if a CloudWatch Log Group is not encrypted with a KMS key or is encrypted with a KMS key not supplied in the rule parameter.'
    got 'Checks if a log group in Amazon CloudWatch Logs is encrypted with a AWS Key Management Service (KMS) managed Customer Master Keys (CMK). The rule is NON_COMPLIANT if no AWS KMS CMK is configured on the log groups.'
  Name: expected 'Checks if Amazon CloudWatch Log Groups are encrypted with any AWS KMS key or a specified AWS KMS key Id'
    got 'Checks if a log group in Amazon CloudWatch Logs is encrypted with a AWS Key Management Service (KMS) managed Customer Master Keys (CMK)'

task#156, controls 8sdhbqfe2fsmh3b1i2b24xaic and 1ln29qglgkigm9ajfka6u2p2u:
  8sdhbq... CreateTime: expected 1657584000.0, got 1672444800
  8sdhbq... Description: expected 'Detects whether an AWS account has AWS CloudTrail enabled has at least one AWS CloudTrail trail enabled for a governed AWS Region. This rule is NON_COMPLIANT if no CloudTrail trails are enabled in an AWS account for a governed AWS Region.'
    got 'Detects whether CloudTrail is enabled on member accounts. The rule is NON_COMPLIANT if a member account of the organization records no management events through a CloudTrail trail.'
  8sdhbq... Name: expected 'Detect whether an AWS account has at least one AWS CloudTrail trail enabled for a governed AWS Region.', got 'Detect whether CloudTrail is enabled on member accounts'
  1ln29q... CreateTime: expected 1657584000.0, got 1651363200
  1ln29q... Description: expected 'This control detects whether a shared AWS account in the Security organizational unit has at least one AWS CloudTrail trail enabled for a governed AWS Region. This rule is NON_COMPLIANT if no CloudTrail trails are enabled in an AWS account for a governed AWS Region.'
    got "Detects whether CloudTrail is enabled on shared accounts. The rule is NON_COMPLIANT if a shared account of the organization's security organizational unit records no management events through a CloudTrail trail."
  1ln29q... Name: expected 'Detect whether a shared AWS account in the Security organizational unit has at least one AWS CloudTrail trail enabled for a governed AWS Region.', got a 'Detect whether CloudTrail ...' rewrite

task#158, control 85z4kpyxbo5nepzfwbfwwxjx7:
  CreateTime: expected 1534982400.0, got 1684022400
  Description: expected 'Checks if AWS CloudTrail creates a signed digest file with logs. AWS recommends that the file validation must be enabled on all trails. The rule is NON_COMPLIANT if the validation is not enabled.'
    got 'Checks whether AWS CloudTrail creates a signed digest file with logs. AWS recommends that the file validation must be enabled on all trails. The rule is noncompliant if the validation is not enabled.'
  Name: expected 'Checks if AWS CloudTrail creates a signed digest file with logs', got 'Checks whether AWS CloudTrail creates a signed digest file with logs'

task#362, control 503uicglhjkokaajywfpt6ros:
  CreateTime: expected 1542240000.0, got 1700092800
  Description: expected 'Secure EBS volumes attached to EC2 instances through encryption.', got 'Secure your data by encrypting the EBS volumes attached to your EC2 instances.'

task#395, control 9xv17a25orapqa36e19o5b8lp:
  CreateTime: expected 1534809600.0, got 1675641600
  Description: expected 'Checks if Amazon GuardDuty is enabled in your AWS account and AWS Region. If you provide an AWS account for centralization, the rule evaluates the GuardDuty results in the centralized account. The rule is COMPLIANT when GuardDuty is enabled.'
    got 'Checks whether Amazon GuardDuty is enabled in your AWS account and region. If you provide an AWS account for centralization, the rule evaluates the Amazon GuardDuty results in the centralized account. The rule is COMPLIANT when Amazon GuardDuty is enabled.'
  Name: expected 'Checks if Amazon GuardDuty is enabled in your AWS account and AWS Region', got 'Checks whether Amazon GuardDuty is enabled in your AWS account and region'

task#407, control dmb52g7tw004w9iede1pgb1fj:
  CreateTime: expected 1474416000.0, got 1582848000
  Description: expected 'Checks if the account password policy for AWS Identity and Access Management (IAM) users meets the specified requirements indicated in the parameters. The rule is NON_COMPLIANT if the account password policy does not meet the specified requirements.'
    got 'Checks if the account password policy for IAM users meets the specified requirements indicated in the parameters. The rule is NON_COMPLIANT if the account password policy does not meet the specified requirements.'
  Name: expected 'Checks if the account password policy for AWS Identity and Access Management (IAM) users meets the specified requirements indicated in the parameters', got 'Checks if the account password policy for IAM users meets the specified requirements'

task#411, control 9utgo6tl86bg3e9jaq073vmqw:
  CreateTime: expected 1534377600.0, got 1653696000
  Description: expected 'Checks if the root user access key is available. The rule is COMPLIANT if the user access key does not exist. Otherwise, NON_COMPLIANT.'
    got 'Checks if the root user access key is available. The rule is COMPLIANT if the user access key does not exist.'

task#416, control 7z1uzm6s8qk7ym7m0sm56cq0u:
  CreateTime: expected 1564444800.0, got 1627171200
  Description: expected 'Protect your account by requiring multi-factor authentication (MFA) for any AWS Identity and Access Management (IAM) user using the account. MFA adds an extra authentication code on top of a user name and password.'
    got 'Reduces the risk of unauthorized access by disallowing access to IAM users that have no multi-factor authentication (MFA) device.'

task#423, control 6rilu41n0gb9w6mxrkyewoer4:
  CreateTime: expected 1542240000.0, got 1557792000 (sole diverging key in this cluster)

Model citation: Smithy com.amazonaws.controlcatalog#ControlCatalog - ListControls POST /list-controls, 200, paginated (inputToken NextToken, items Controls); output Controls is @required list<ControlSummary>; ControlSummary$Name @required "The display name of the control", ControlSummary$Description @required "A description of the control, as it may appear in the console", ControlSummary$CreateTime "the time when the control was released (start of its life) as a governance capability". Errors modeled: AccessDeniedException 403, InternalServerException 500, ThrottlingException 429, ValidationException 400 - none of them apply here; the status is 200 in both the recording and the emulator, so this is purely a payload-content divergence.

## Steps to Reproduce
1. Clone this checkout and boot the emulator from it (the replay harness confirmed /__admin/health 200 and an accepting data listener).
2. Replay wire fixture controlcatalog-catalog-list-controls-aws-config-configrule from trace set simparity-wire-aws-v1.
3. POST /list-controls with the fixture's Filter selecting AWS::Config::ConfigRule implementations and page to each step under test: task#1, task#153, task#156, task#158, task#362, task#395, task#407, task#411, task#416, task#423.
4. In each 200 response, locate the ControlSummary whose Arn is the one named for that step in the Description section above and compare its Name, Description and CreateTime against the recorded response's values for the SAME Arn.
5. Every comparison differs: status, Arn, paging and shape match, while Description (and Name for six of the clusters) is re-worded and CreateTime (for nine of the ten) is a different epoch instant. task#423 isolates the timestamp half on its own.
6. Re-replay all ten steps to confirm a fix - the issue is closed only when every one of the ten passes.
