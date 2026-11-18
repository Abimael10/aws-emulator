Issue-Source: trace-sweep-divergence
Issue-Trace: controlcatalog-catalog-list-controls-aws-config-configrule
Issue-Step: task#155
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: controlcatalog-catalog-list-controls-aws-config-configrule
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/9vnj728czsv4m6pflfqt21bw2"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/9vnj728czsv4m6pflfqt21bw2"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/9vnj728czsv4m6pflfqt21bw2"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/87qo8rsoettjrxjevmjqcw1tu"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/87qo8rsoettjrxjevmjqcw1tu"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/aazzef7lptl5umta6690p6u5c"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/aazzef7lptl5umta6690p6u5c"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/aazzef7lptl5umta6690p6u5c"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/byrg7iwyl4azhecyhqtq8ocw1"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/byrg7iwyl4azhecyhqtq8ocw1"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/byrg7iwyl4azhecyhqtq8ocw1"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/84hd48gyrb6e5ribkmsoub44c"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/84hd48gyrb6e5ribkmsoub44c"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/1oxkwnc4hwhi2ndv6ekwy7np7"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/1oxkwnc4hwhi2ndv6ekwy7np7"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/1oxkwnc4hwhi2ndv6ekwy7np7"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/df2ta5ytg2zatj1q7y5e09u32"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/df2ta5ytg2zatj1q7y5e09u32"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/227ph3vim64tdwv5mfagjd22r"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/227ph3vim64tdwv5mfagjd22r"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/66gfl06uj1v999z53szvu0exa"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/66gfl06uj1v999z53szvu0exa"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/66gfl06uj1v999z53szvu0exa"].Name
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/4tf01viblttjxqhu35yz90vw1"].CreateTime
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/4tf01viblttjxqhu35yz90vw1"].Description
Issue-Key: controlcatalog|ListControls|2xx->2xx|$.Controls[Arn="arn:aws:controlcatalog:::control/4tf01viblttjxqhu35yz90vw1"].Name
Issue-Commit: 1490d5d01f48dd097a9435a06996bc23d586834e

# controlcatalog ListControls rewrites AWS Config rule control Name/Description and synthesizes CreateTime (10 catalog records)

> _Note: this issue was identified against the code base at commit `1490d5d01f48dd097a9435a06996bc23d586834e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListControls

## Description
ListControls answers 200 with the correct wire shape, but for AWS Config rule (AWS::Config::ConfigRule) catalog records the served ControlSummary carries emulator-authored Name/Description prose instead of the catalog's published text, and a CreateTime that is not the record's catalog publication timestamp. Ten distinct catalog records diverge this way in one replayed listing; they share one root cause (the seeded Config-rule catalog rows hold re-written text and invented timestamps), so they are bundled here as 10 causes.

SHARED STATEMENT OF THE FAULT
AWS: Smithy (controlcatalog): ListControls -> POST /list-controls, 200, paginated over ListControlsResponse$Controls : list<ControlSummary>. ControlSummary$Name and ControlSummary$Description are required members documented as the control's name and description "as it may appear in the console"; ControlSummary$CreateTime is the time the control was released/created in the catalog. All three are AWS-owned catalog content for a given control ARN — the exact published string and the record's own publication instant — not values a caller supplies or a server may re-phrase or mint. The recording supplies the published Name/Description and CreateTime for each ARN below (recorded behavior is the authority for the per-record values; the model is the authority for the fields being AWS-owned catalog content).
Emulator: for each ARN below it serves a differently worded Name and/or Description with the same general meaning (clauses re-ordered, 'AWS' expanded to 'Amazon Web Services', qualifying conditions added or dropped, 'Checks if' turned into 'Checks whether', input-parameter caveats invented) and, for nine of the ten, a CreateTime unrelated to the recorded one.
Why: callers treat the ControlCatalog Name/Description as published AWS content and CreateTime as the catalog record's identity in time. Compliance-evidence exports, console/portal renderings, catalog-diffing and documentation-generation tools, and 'controls released since <date>' selections all read these fields. A paraphrase that adds or drops a qualifying clause misstates what the control evaluates (e.g. the CloudTrail record gains a 'last delivery older than the age the rule's input parameter allows' condition the published text does not have); a wrong CreateTime silently moves a control in or out of any date-scoped selection.
Fix: the seeded catalog data behind the ListControls handler in the controlcatalog crate — the stored ControlSummary rows for AWS::Config::ConfigRule implementations must hold the published Name/Description strings verbatim and the catalog's own CreateTime, so handler and serializer pass them through unchanged. This is a state/data-fidelity defect, not a serialization defect.

Cause 1 of 10 — control arn:aws:controlcatalog:::control/9vnj728czsv4m6pflfqt21bw2 (CloudTrail -> CloudWatch Logs).
AWS (recorded): Name 'Checks if AWS CloudTrail trails are configured to send logs to CloudWatch logs'; Description 'Checks if AWS CloudTrail trails are configured to send logs to CloudWatch logs. The trail is NON_COMPLIANT if the CloudWatchLogsLogGroupArn property of the trail is empty.'; CreateTime 1540166400.
Emulator: Name 'Checks whether AWS CloudTrail trails are configured to send logs to Amazon CloudWatch Logs'; Description re-worded and extended with "or if its last delivery is older than the age the rule's input parameter allows"; CreateTime 1567296000.
Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#155.

Cause 2 of 10 — control arn:aws:controlcatalog:::control/87qo8rsoettjrxjevmjqcw1tu (public EBS snapshots).
AWS (recorded): Description 'Detects whether all AWS accounts have access to restore Amazon EBS snapshots. The rule is NON_COMPLIANT if any snapshots have the RestorableByUserIds field set to the value All. In that case, the Amazon EBS snapshots are public.'; CreateTime 1638316800.
Emulator: Description 'Checks whether Amazon Elastic Block Store (Amazon EBS) snapshots are not publicly restorable. The rule is NON_COMPLIANT if an Amazon EBS snapshot is restorable by all Amazon Web Services accounts.' (the RestorableByUserIds/All criterion is replaced by prose); CreateTime 1530403200.
Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#235.

Cause 3 of 10 — control arn:aws:controlcatalog:::control/aazzef7lptl5umta6690p6u5c (ALB/NLB ACM certificates).
AWS (recorded): Name 'Checks if Application Load Balancers and Network Load Balancers are configured to use certificates from Amazon Certificate Manager (ACM)'; Description 'Checks if Application Load Balancers and Network Load Balancers are configured to use certificates from Amazon Certificate Manager (ACM). This rule is NON_COMPLIANT if at least 1 load balancer is configured without a certificate from ACM.'; CreateTime 1594771200.
Emulator: Name/Description re-written to 'have listeners that are configured to use certificates from AWS Certificate Manager' with the NON_COMPLIANT criterion restated per listener; CreateTime 1545350400.
Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#345.

Cause 4 of 10 — control arn:aws:controlcatalog:::control/byrg7iwyl4azhecyhqtq8ocw1 (IAM policy no admin access).
AWS (recorded): Name 'Checks whether the default version of AWS Identity and Access Management (IAM) policies do not have administrator access'; Description ends 'If any statement has "Effect": "Allow" with "Action": "*" over "Resource": "*", the rule is non-compliant.'; CreateTime 1537228800.
Emulator: 'AWS' expanded to 'Amazon Web Services' in Name and Description, and the explicit Effect/Action/Resource criterion replaced with "The rule is NON_COMPLIANT if the policy's default version allows every action on every resource."; CreateTime 1675555200.
Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#409.

Cause 5 of 10 — control arn:aws:controlcatalog:::control/84hd48gyrb6e5ribkmsoub44c (RDS snapshot encryption).
AWS (recorded): Description 'Checks if Amazon Relational Database Service (Amazon RDS) DB snapshots are encrypted. The rule is NON_COMPLIANT if the Amazon RDS DB snapshots are not encrypted.'; CreateTime 1590364800.
Emulator: second sentence re-worded to 'The rule is NON_COMPLIANT if the snapshot is not encrypted.'; CreateTime 1578009600.
Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#568.

Cause 6 of 10 — control arn:aws:controlcatalog:::control/1oxkwnc4hwhi2ndv6ekwy7np7 (Redshift public access).
AWS (recorded): Name 'Detect whether Amazon Redshift clusters are blocked from public access'; Description 'Detects whether Amazon Redshift clusters are blocked from public access. The rule is NON_COMPLIANT if the publiclyAccessible field is true in the cluster configuration item.'; CreateTime 1638230400.
Emulator: Name 'Checks whether Amazon Redshift clusters are not publicly accessible' and the matching Description rewrite; CreateTime 1542499200.
Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#577.

Cause 7 of 10 — control arn:aws:controlcatalog:::control/df2ta5ytg2zatj1q7y5e09u32 (restricted common ports / remote console exposure).
AWS (recorded): Description "Reduces a server's exposure to risk by removing unfettered connectivity to remote console services such as RDP."; CreateTime 1542240000.
Emulator: an entirely different sentence about security groups allowing unrestricted incoming TCP traffic to parameter-named ports; CreateTime 1699833600.
Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#593.

Cause 8 of 10 — control arn:aws:controlcatalog:::control/227ph3vim64tdwv5mfagjd22r (root account hardware MFA).
AWS (recorded): Name 'Checks if your AWS account is enabled to use multi-factor authentication (MFA) hardware device to sign in with root credentials'; Description adds 'The rule is NON_COMPLIANT if any virtual MFA devices are permitted for signing in with root credentials.'
Emulator: 'AWS account' expanded to 'Amazon Web Services account' in both members and the NON_COMPLIANT criterion inverted to 'if no hardware MFA device is enabled for the root user'. CreateTime matches here; only the text diverges.
Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#594.

Cause 9 of 10 — control arn:aws:controlcatalog:::control/66gfl06uj1v999z53szvu0exa (SageMaker notebook direct internet access).
AWS (recorded): Name 'Detect whether an Amazon SageMaker notebook instance allows direct internet access'; Description 'Detects whether direct internet access is allowed for an Amazon SageMaker notebook instance. The rule is NON_COMPLIANT if Amazon SageMaker notebook instances allow direct internet access.'; CreateTime 1638230400.
Emulator: Name/Description re-written to the 'direct internet access is disabled' form with 'The rule is NON_COMPLIANT if the notebook instance reaches the internet directly.'; CreateTime 1529452800.
Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#664.

Cause 10 of 10 — control arn:aws:controlcatalog:::control/4tf01viblttjxqhu35yz90vw1 (Secrets Manager rotation enabled).
AWS (recorded): Name 'Checks whether AWS Secret Manager secret has rotation enabled'; Description 'Checks whether AWS Secret Manager secret has rotation enabled. If the maximumAllowedRotationFrequency or the maximumAllowedRotationFrequencyInHours parameter is specified, the rotation frequency of the secret is compared with the maximum allowed frequency.'; CreateTime 1779840000.
Emulator: Name 'Checks if AWS Secrets Manager secrets have rotation enabled'; Description drops the parameter-comparison sentence entirely; CreateTime 1664496000.
Trace: controlcatalog-catalog-list-controls-aws-config-configrule#task#665.

Relation to open issues: 20261115T014344Z-controlcatalog-listcontrols-pages-omit-catalog-config-controls-and-rewrite.md and 20261116T001437Z-controlcatalog-listcontrols-rewrites-published-security-hub-control-description.md describe the same class of rewrite for other catalog records; the replay reports no owner for the ten records above, so they are net-new and filed here. A fixer should expect the published-text and publication-timestamp correction to apply to the whole seeded AWS Config rule catalogue, not only these ten rows.

## Evidence
Replay of fixture controlcatalog-catalog-list-controls-aws-config-configrule (trace set simparity-wire-aws-v1): expected status 200, actual status 200, verdict mismatch on ten steps. Each cluster's divergence key is service=controlcatalog|ListControls|status=200->200|keys=$.Controls[Arn="..."].CreateTime,$.Controls[Arn="..."].Description(,.Name).

task#155, control 9vnj728czsv4m6pflfqt21bw2:
  CreateTime expected 1540166400.0, got 1567296000
  Description expected 'Checks if AWS CloudTrail trails are configured to send logs to CloudWatch logs. The trail is NON_COMPLIANT if the CloudWatchLogsLogGroupArn property of the trail is empty.'
  Description got "Checks whether AWS CloudTrail trails are configured to send logs to Amazon CloudWatch Logs. The rule is NON_COMPLIANT if a trail does not deliver to a CloudWatch Logs log group, or if its last delivery is older than the age the rule's input parameter allows."
  Name expected 'Checks if AWS CloudTrail trails are configured to send logs to CloudWatch logs', got 'Checks whether AWS CloudTrail trails are configured to send logs to Amazon CloudWatch Logs'

task#235, control 87qo8rsoettjrxjevmjqcw1tu:
  CreateTime expected 1638316800.0, got 1530403200
  Description expected 'Detects whether all AWS accounts have access to restore Amazon EBS snapshots. The rule is NON_COMPLIANT if any snapshots have the RestorableByUserIds field set to the value All. In that case, the Amazon EBS snapshots are public.'
  Description got 'Checks whether Amazon Elastic Block Store (Amazon EBS) snapshots are not publicly restorable. The rule is NON_COMPLIANT if an Amazon EBS snapshot is restorable by all Amazon Web Services accounts.'

task#345, control aazzef7lptl5umta6690p6u5c:
  CreateTime expected 1594771200.0, got 1545350400
  Description expected 'Checks if Application Load Balancers and Network Load Balancers are configured to use certificates from Amazon Certificate Manager (ACM). This rule is NON_COMPLIANT if at least 1 load balancer is configured without a certificate from ACM.'
  Description got 'Checks if Application Load Balancers and Network Load Balancers have listeners that are configured to use certificates from AWS Certificate Manager. The rule is NON_COMPLIANT if a listener uses a certificate that AWS Certificate Manager does not issue.'
  Name expected 'Checks if Application Load Balancers and Network Load Balancers are configured to use certificates from Amazon Certificate Manager (ACM)', got 'Checks if Application Load Balancers and Network Load Balancers have listeners that are configured to use certificates from AWS Certificate Manager'

task#409, control byrg7iwyl4azhecyhqtq8ocw1:
  CreateTime expected 1537228800.0, got 1675555200
  Description expected 'Checks whether the default version of AWS Identity and Access Management (IAM) policies do not have administrator access. If any statement has "Effect": "Allow" with "Action": "*" over "Resource": "*", the rule is non-compliant.'
  Description got "Checks whether the default version of Amazon Web Services Identity and Access Management (IAM) policies do not have administrator access. The rule is NON_COMPLIANT if the policy's default version allows every action on every resource."
  Name expected 'Checks whether the default version of AWS Identity and Access Management (IAM) policies do not have administrator access', got 'Checks whether the default version of Amazon Web Services Identity and Access Management (IAM) policies do not have administrator access'

task#568, control 84hd48gyrb6e5ribkmsoub44c:
  CreateTime expected 1590364800.0, got 1578009600
  Description expected 'Checks if Amazon Relational Database Service (Amazon RDS) DB snapshots are encrypted. The rule is NON_COMPLIANT if the Amazon RDS DB snapshots are not encrypted.'
  Description got 'Checks if Amazon Relational Database Service (Amazon RDS) DB snapshots are encrypted. The rule is NON_COMPLIANT if the snapshot is not encrypted.'

task#577, control 1oxkwnc4hwhi2ndv6ekwy7np7:
  CreateTime expected 1638230400.0, got 1542499200
  Description expected 'Detects whether Amazon Redshift clusters are blocked from public access. The rule is NON_COMPLIANT if the publiclyAccessible field is true in the cluster configuration item.'
  Description got 'Checks whether Amazon Redshift clusters are not publicly accessible. The rule is NON_COMPLIANT if the publiclyAccessible field of the cluster configuration is true.'
  Name expected 'Detect whether Amazon Redshift clusters are blocked from public access', got 'Checks whether Amazon Redshift clusters are not publicly accessible'

task#593, control df2ta5ytg2zatj1q7y5e09u32:
  CreateTime expected 1542240000.0, got 1699833600
  Description expected "Reduces a server's exposure to risk by removing unfettered connectivity to remote console services such as RDP."
  Description got "Checks whether the security groups in use do not allow unrestricted incoming TCP traffic to the specified ports. The rule is NON_COMPLIANT if a security group allows inbound traffic from any address to a port the rule's input parameters name."

task#594, control 227ph3vim64tdwv5mfagjd22r:
  Description expected 'Checks if your AWS account is enabled to use multi-factor authentication (MFA) hardware device to sign in with root credentials. The rule is NON_COMPLIANT if any virtual MFA devices are permitted for signing in with root credentials.'
  Description got 'Checks whether your Amazon Web Services account is enabled to use a hardware multi-factor authentication (MFA) device to sign in with root credentials. The rule is NON_COMPLIANT if no hardware MFA device is enabled for the root user.'
  Name expected 'Checks if your AWS account is enabled to use multi-factor authentication (MFA) hardware device to sign in with root credentials', got 'Checks whether your Amazon Web Services account is enabled to use a hardware multi-factor authentication (MFA) device to sign in with root credentials'

task#664, control 66gfl06uj1v999z53szvu0exa:
  CreateTime expected 1638230400.0, got 1529452800
  Description expected 'Detects whether direct internet access is allowed for an Amazon SageMaker notebook instance. The rule is NON_COMPLIANT if Amazon SageMaker notebook instances allow direct internet access.'
  Description got 'Checks whether direct internet access is disabled for an Amazon SageMaker notebook instance. The rule is NON_COMPLIANT if the notebook instance reaches the internet directly.'
  Name expected 'Detect whether an Amazon SageMaker notebook instance allows direct internet access', got 'Checks whether direct internet access is disabled for an Amazon SageMaker notebook instance'

task#665, control 4tf01viblttjxqhu35yz90vw1:
  CreateTime expected 1779840000.0, got 1664496000
  Description expected 'Checks whether AWS Secret Manager secret has rotation enabled. If the maximumAllowedRotationFrequency or the maximumAllowedRotationFrequencyInHours parameter is specified, the rotation frequency of the secret is compared with the maximum allowed frequency.'
  Description got 'Checks if AWS Secrets Manager secrets have rotation enabled. The rule is NON_COMPLIANT if the secret does not have rotation enabled.'
  Name expected 'Checks whether AWS Secret Manager secret has rotation enabled', got 'Checks if AWS Secrets Manager secrets have rotation enabled'

Model citation: Smithy controlcatalog — ListControls POST /list-controls, 200, paginated (inputToken NextToken / items Controls); ControlSummary$Name, $Description, $CreateTime are AWS-owned catalog members of the required Controls list.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay wire fixture controlcatalog-catalog-list-controls-aws-config-configrule from trace set simparity-wire-aws-v1.
2. POST /list-controls with the fixture's Filter selecting AWS::Config::ConfigRule implementations and page to the step under test (task#155, then task#235, task#345, task#409, task#568, task#577, task#593, task#594, task#664, task#665).
3. In each 200 response locate the ControlSummary whose Arn is the one named for that step and compare its Name, Description and CreateTime with the recorded response's values for the same Arn.
4. Each comparison differs: status and Arn match, while Description (and Name for seven of them) is re-worded and CreateTime (for nine of them) is a different epoch instant. Re-replay all ten steps to confirm a fix.
