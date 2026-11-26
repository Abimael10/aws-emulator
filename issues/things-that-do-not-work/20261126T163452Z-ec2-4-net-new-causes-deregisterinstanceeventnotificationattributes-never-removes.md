Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.op-contract-deregister-instance-event-notification-attributes ec2.disassociate-security-group-vpc-dependency-rejected ec2.get-instance-tpm-ek-pub-returns-stable-blob ec2.op-contract-get-network-insights-access-scope-analysis-findings
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.op-contract-deregister-instance-event-notification-attributes|_poll_until#18|AssertionError|raise AssertionError(f"timed out after {timeout_s}s waiting for predicate; last value was {last!r}")
Issue-Key: ec2.disassociate-security-group-vpc-dependency-rejected|run#21|AssertionError|assert message == (
Issue-Key: ec2.get-instance-tpm-ek-pub-returns-stable-blob|run#6|AssertionError|assert code == "InvalidInstanceID.Malformed", first
Issue-Key: ec2.op-contract-get-network-insights-access-scope-analysis-findings|run#15|AssertionError|assert code == "InvalidParameterValue", f"expected InvalidParameterValue, got {code!r}"
Issue-Commit: 34fefd7aecead9141157dc0abcdfe1d4823d27aa

# ec2: 4 net-new causes — DeregisterInstanceEventNotificationAttributes never removes the tag key, and three refusal classifications/messages diverge (DisassociateSecurityGroupVpc, GetInstanceTpmEkPub, GetNetworkInsightsAccessScopeAnalysisFindings)

> _Note: this issue was identified against the code base at commit `34fefd7aecead9141157dc0abcdfe1d4823d27aa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeregisterInstanceEventNotificationAttributes

## Description
Four independent net-new root causes in the ec2 emulator, all grounded in failing behavioral checks. A fixer must close ALL FOUR before submitting.

Note on neighbouring issues: an open issue `20261022T235108Z-ec2-getinstancetpmekpub-stub.md` ("Implement ec2 GetInstanceTpmEkPub: unimplemented scaffold stub") and a resolved issue `20261029T175114Z-ec2-disassociatesecuritygroupvpc-stub.md` exist for two of these operations. Neither covers the divergences below: both operations now answer, they answer with the wrong refusal classification/message. The ec2 Smithy model is ec2Query and declares NO modelled errors for any of these four operations, so every error code/message below is grounded in recorded behavior, not in the model.

=== Cause 1 of 4: DeregisterInstanceEventNotificationAttributes does not remove the tag key from the account allow list ===
Failing check: ec2.op-contract-deregister-instance-event-notification-attributes
AWS:      recorded behavior only (Smithy: DeregisterInstanceEventNotificationAttributes input InstanceTagAttribute (DeregisterInstanceTagAttributeRequest) required; output InstanceTagAttribute (InstanceTagNotificationAttribute); no modelled errors). The call removes the named tag keys from the Region's scheduled-event-notification allow list and the response's InstanceTagAttribute reflects the list AFTER removal, i.e. the deregistered key is absent from InstanceTagKeys; a subsequent DescribeInstanceEventNotificationAttributes no longer lists it. Deregistering an already-absent key is then refused with code InvalidNotificationAttributeInstanceTagNotFound and NO Message element at all.
Emulator: the call returns 200 but the allow list is never mutated. After RegisterInstanceEventNotificationAttributes of one unique key, the deregister response keeps returning {'InstanceTagKeys': ['<the registered key>'], 'IncludeAllTagsOfInstance': False} — the key is still present — for the whole 30 s retry window, so the check times out waiting for the key to disappear. The response shape is right; the removal is a no-op.
Why:      a client that deregisters a tag key to stop receiving scheduled-event notifications for it is told nothing changed and, on a read-back, still sees the key registered; the allow list grows monotonically and can never be narrowed. The documented "already removed" refusal (InvalidNotificationAttributeInstanceTagNotFound, no Message) is also unreachable, since the key never becomes absent.
Fix:      the DeregisterInstanceEventNotificationAttributes handler and its state write — it must delete the requested keys from the per-Region `instance_event_tag_registrations` record that Register/Describe/Deregister share, then serialize the post-removal list, and refuse an already-absent key with InvalidNotificationAttributeInstanceTagNotFound carrying no Message element.
Traces:   ec2.op-contract-deregister-instance-event-notification-attributes

=== Cause 2 of 4: DisassociateSecurityGroupVpc's in-use refusal message is emulator-authored prose instead of AWS's templated sentence ===
Failing check: ec2.disassociate-security-group-vpc-dependency-rejected
AWS:      recorded behavior only (Smithy: DisassociateSecurityGroupVpc input GroupId (required), VpcId (required), DryRun; no modelled errors). While a network interface in the associated VPC still uses the security group, the call is refused with code SecurityGroupVpcAssociationInUse and the message "The Security Group VPC association between Security Group '<group-id>' and VPC '<vpc-id>' is in use." — the fixed text is "The Security Group VPC association between Security Group '", "' and VPC '", "' is in use."; the two variable parts are the GroupId and VpcId taken verbatim from the request (the check's sg-… and vpc-… values are one run's fillers, not part of the template).
Emulator: the code is right (SecurityGroupVpcAssociationInUse) but the message is "You cannot disassociate the security group if any Elastic network interfaces in the associated VPC are still associated with the security group" — emulator-authored prose that names neither the group nor the VPC and ends without a period.
Why:      a client that surfaces or matches on this message (operators and log scrapers routinely pattern-match the quoted ids to decide WHICH association is blocked) gets a sentence AWS never emits and cannot recover the group/VPC pair from it; with a fan-out of associations there is no way to tell which one is in use.
Fix:      the DisassociateSecurityGroupVpc handler's in-use validation — it must build the SecurityGroupVpcAssociationInUse message from the request's GroupId and VpcId using AWS's template.
Traces:   ec2.disassociate-security-group-vpc-dependency-rejected

=== Cause 3 of 4: GetInstanceTpmEkPub classifies a well-formed-but-never-issued instance id as NotFound instead of Malformed ===
Failing check: ec2.get-instance-tpm-ek-pub-returns-stable-blob
AWS:      recorded behavior only (Smithy: GetInstanceTpmEkPub input InstanceId (InstanceId) required, KeyType (EkPubKeyType) required, KeyFormat (EkPubKeyFormat) required, DryRun; no modelled errors). For an instance id this operation cannot resolve — both a well-formed 17-hex-char id that was never issued AND a structurally invalid id — the call is refused with code InvalidInstanceID.Malformed and the message "The instance ID '<id>' is malformed": fixed text "The instance ID '" … "' is malformed", with the id taken verbatim from the request. The verdict is independent of the requested KeyType/KeyFormat and identical on a repeat.
Emulator: for the well-formed, never-issued id it answers code InvalidInstanceID.NotFound with message "The instance ID 'i-0123456789abcdef0' does not exist" — wrong code and wrong message template (that id is one run's filler for the variable part).
Why:      a client branches on the error code: Malformed is a permanent request defect to surface to the caller, NotFound is a transient/eventual-consistency condition most SDK-level retry and reconciliation code will poll on. Mis-stamping a permanent rejection as NotFound makes callers retry forever and hides the real diagnosis.
Fix:      the GetInstanceTpmEkPub handler's instance-id validation — it must stamp InvalidInstanceID.Malformed with AWS's message for every id it cannot resolve, before and independent of any KeyType/KeyFormat handling.
Traces:   ec2.get-instance-tpm-ek-pub-returns-stable-blob

=== Cause 4 of 4: GetNetworkInsightsAccessScopeAnalysisFindings answers a resource-specific NotFound where AWS answers InvalidParameterValue ===
Failing check: ec2.op-contract-get-network-insights-access-scope-analysis-findings
AWS:      recorded behavior only (Smithy: GetNetworkInsightsAccessScopeAnalysisFindings input NetworkInsightsAccessScopeAnalysisId required, MaxResults range 1–1000, NextToken, DryRun; output echoes NetworkInsightsAccessScopeAnalysisId, AnalysisStatus, AnalysisFindings; no modelled errors). A placeholder-style analysis id that does not match the real analysis-id shape closely enough to reach the id-specific validator is refused generically: code InvalidParameterValue, message "NetworkInsightsAccessScopeAnalysisId (<value>) is not valid" — fixed text "NetworkInsightsAccessScopeAnalysisId (" … ") is not valid", with the rejected value echoed from the request.
Emulator: it answers code InvalidNetworkInsightsAccessScopeAnalysisId.NotFound — a resource-specific not-found classification AWS does not use for this input class (the preceding success-path assertions on the real analysis passed, so only the refusal branch diverges).
Why:      the same code-branching problem as Cause 3: a caller reads *.NotFound as "the resource may appear later" and retries, where AWS states the parameter value itself is invalid and the request can never succeed as written. Clients that assert on the generic InvalidParameterValue contract for malformed-ish ids also break.
Fix:      the GetNetworkInsightsAccessScopeAnalysisFindings handler's id validation — it must classify an analysis id that fails the shape check as InvalidParameterValue with AWS's templated message, reserving the resource-specific not-found code for well-shaped ids.
Traces:   ec2.op-contract-get-network-insights-access-scope-analysis-findings

## Evidence
Cause 1 — ec2.op-contract-deregister-instance-event-notification-attributes:
  Expected: the deregister response (and a following DescribeInstanceEventNotificationAttributes) no longer lists the registered tag key; a repeat deregister of the absent key is refused InvalidNotificationAttributeInstanceTagNotFound with no Message element.
  Actual:   AssertionError: timed out after 30s waiting for predicate; last value was {'InstanceTagKeys': ['sim2real-behav-notify-tag-749631cdba2a'], 'IncludeAllTagsOfInstance': False}
            (the registered key is still present in every deregister response)

Cause 2 — ec2.disassociate-security-group-vpc-dependency-rejected:
  Expected: code SecurityGroupVpcAssociationInUse, message "The Security Group VPC association between Security Group '<group-id>' and VPC '<vpc-id>' is in use."
  Actual:   code SecurityGroupVpcAssociationInUse, message 'You cannot disassociate the security group if any Elastic network interfaces in the associated VPC are still associated with the security group'
            AssertionError: unexpected SecurityGroupVpcAssociationInUse message: 'You cannot disassociate the security group if any Elastic network interfaces in the associated VPC are still associated with the security group'

Cause 3 — ec2.get-instance-tpm-ek-pub-returns-stable-blob:
  Expected: ('InvalidInstanceID.Malformed', "The instance ID '<id>' is malformed")
  Actual:   ('InvalidInstanceID.NotFound', "The instance ID 'i-0123456789abcdef0' does not exist")
            AssertionError at `assert code == "InvalidInstanceID.Malformed"`

Cause 4 — ec2.op-contract-get-network-insights-access-scope-analysis-findings:
  Expected: code InvalidParameterValue, message "NetworkInsightsAccessScopeAnalysisId (<value>) is not valid"
  Actual:   code 'InvalidNetworkInsightsAccessScopeAnalysisId.NotFound'
            AssertionError: expected InvalidParameterValue, got 'InvalidNetworkInsightsAccessScopeAnalysisId.NotFound'

Handlers implicated (file citations only):
  services/service-com-amazonaws-ec2-amazonec2/src/ops/deregister_instance_event_notification_attributes.rs
  services/service-com-amazonaws-ec2-amazonec2/src/ops/disassociate_security_group_vpc.rs
  services/service-com-amazonaws-ec2-amazonec2/src/ops/get_instance_tpm_ek_pub.rs
  services/service-com-amazonaws-ec2-amazonec2/src/ops/get_network_insights_access_scope_analysis_findings.rs

## Steps to Reproduce
Cause 1: DescribeInstanceEventNotificationAttributes to confirm a fresh tag key is absent; RegisterInstanceEventNotificationAttributes with InstanceTagAttribute={InstanceTagKeys:[<key>]}; DescribeInstanceEventNotificationAttributes shows it; then DeregisterInstanceEventNotificationAttributes with the same InstanceTagKeys and read the returned InstanceTagAttribute — the key is still listed, and Describe keeps listing it. Repeat the deregister: it never reaches the InvalidNotificationAttributeInstanceTagNotFound state.

Cause 2: create VPC A and VPC B (B with a subnet); create a security group in A; AssociateSecurityGroupVpc(GroupId, VpcId=B) and wait for state 'associated'; CreateNetworkInterface in B's subnet with Groups=[the group]; call DisassociateSecurityGroupVpc(GroupId, VpcId=B) and inspect the error Message.

Cause 3: GetInstanceTpmEkPub(InstanceId='i-0123456789abcdef0', KeyType='rsa-2048', KeyFormat='der') against an account where that id was never issued; inspect Error.Code and Error.Message. Repeat with KeyType='ecc-sec-p384', KeyFormat='tpmt' and with a structurally invalid id such as 'i-not-a-valid-id'.

Cause 4: CreateNetworkInsightsAccessScope, StartNetworkInsightsAccessScopeAnalysis, poll DescribeNetworkInsightsAccessScopeAnalyses to 'succeeded', then call GetNetworkInsightsAccessScopeAnalysisFindings(NetworkInsightsAccessScopeAnalysisId='nisa-00000000000000000') and inspect Error.Code and Error.Message.
