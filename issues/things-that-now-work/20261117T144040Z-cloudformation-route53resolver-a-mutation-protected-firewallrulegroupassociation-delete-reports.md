Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__route53resolver.deploy-firewall-rule-group-association-mutation-protection-blocks-delete
Issue-Key: cloudformation__route53resolver.deploy-firewall-rule-group-association-mutation-protection-blocks-delete|run#23|AssertionError|assert (message, code) == (want_message, _REJECTED_CODE), (
Issue-Commit: f1f3347138550af0668723fa38c8460b5bb9555e

# cloudformation__route53resolver: a mutation-protected FirewallRuleGroupAssociation delete reports Resolver's raw RSLVR-02307 ValidationException text instead of CloudFormation's "Invalid request provided: DisassociateFirewallRuleGroupRequest(...)" handler message

> _Note: this issue was identified against the code base at commit `f1f3347138550af0668723fa38c8460b5bb9555e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteStack

## Description
Cause 1 of 1: when CloudFormation deletes a stack containing an AWS::Route53Resolver::FirewallRuleGroupAssociation whose MutationProtection is ENABLED, the resource handler's DisassociateFirewallRuleGroup call is refused by Route 53 Resolver and the stack correctly ends DELETE_FAILED with HandlerErrorCode InvalidRequest — but the handler message carried in the resource's ResourceStatusReason is Resolver's own raw service text rather than CloudFormation's normalized rendering of an invalid request. Failing check id: cloudformation__route53resolver.deploy-firewall-rule-group-association-mutation-protection-blocks-delete.

AWS: Smithy (route53resolver): DisassociateFirewallRuleGroup takes a single required member FirewallRuleGroupAssociationId (ResourceId, @length 1..64) and models ValidationException (client, HTTP 400) among its errors; an association under mutation protection is refused with that error. Smithy (cloudformation) models no per-resource reason text, so for the CloudFormation-side rendering this is recorded behavior only: the AWS::Route53Resolver::FirewallRuleGroupAssociation handler converts the SDK's ValidationException into a resource failure whose ResourceStatusReason is `Resource handler returned message: "<message>" (RequestToken: <uuid>, HandlerErrorCode: InvalidRequest)` where <message> is `Invalid request provided: DisassociateFirewallRuleGroupRequest(FirewallRuleGroupAssociationId=<association-id>)`. In that message the fixed text is `Invalid request provided: DisassociateFirewallRuleGroupRequest(FirewallRuleGroupAssociationId=` plus the closing `)`; the only variable part is the association id taken from the delete request (the recording's rslvr-frgassoc-1d0b14e4db018fed is one such value, not part of the template). The stack status (DELETE_FAILED), the HandlerErrorCode (InvalidRequest) and the untouched association (Status COMPLETE, MutationProtection ENABLED, Priority unchanged) are all part of the recorded behavior and the emulator already matches those.

Emulator: the handler surfaces Route 53 Resolver's service-level message verbatim, so the reason parses out as message `[RSLVR-02307] The DNS Firewall rule group association is under mutation protection. Remove the mutation protection and try again. Trace Id: "<trace-id>"` with code `InvalidRequest` — i.e. fixed text `[RSLVR-02307] The DNS Firewall rule group association is under mutation protection. Remove the mutation protection and try again. Trace Id: "` + `"` and a per-run trace id as the variable part. The check's assertion on (message, code) therefore fails on the message only; the code already matches. Note the RSLVR-02307 wording is plausible for Route 53 Resolver's OWN DisassociateFirewallRuleGroup refusal — the divergence is that CloudFormation's resource handler passes it through instead of re-rendering it as an invalid-request message about the request it issued.

Why: a client or CI pipeline that reads the stack's ResourceStatusReason to classify why a delete failed sees text that never appears in AWS. Tooling that matches CloudFormation's documented `Resource handler returned message: "Invalid request provided: <RequestShape>(...)"` shape — to extract the request shape and the offending member/value, or merely to assert the failure class in a test — fails to match, and operators are shown a provider-internal service error code (RSLVR-02307) plus a per-run trace id instead of the stable, request-describing message AWS emits. The emulator's message is also non-deterministic across runs because of the embedded trace id.

Fix: the CloudFormation resource handler for AWS::Route53Resolver::FirewallRuleGroupAssociation — specifically its delete path's error mapping, where a refusal from the underlying DisassociateFirewallRuleGroup call is translated into the resource-failure reason. It must map the Resolver client error to HandlerErrorCode InvalidRequest (already correct) and build the message as `Invalid request provided: ` + the request shape name + `(` + the set members rendered as `Member=Value` + `)` for the request it actually issued, rather than forwarding the service's own message. The Resolver-side ValidationException text itself should be left alone; only the CloudFormation handler's rendering changes.

Traces: cloudformation__route53resolver.deploy-firewall-rule-group-association-mutation-protection-blocks-delete — the stack creates (CREATE_COMPLETE), the association reads back correctly through GetFirewallRuleGroupAssociation (Arn/Name/MutationProtection=ENABLED/Priority=101/VpcId/FirewallRuleGroupId all as templated), DeleteStack correctly ends DELETE_FAILED and the association survives untouched; only the DELETE_FAILED reason's message diverges. This is NOT covered by open issue 20261109T161636Z-cloudformation-route53resolver-no-aws-route53resolver-resource-type-is.md: that issue is about AWS::Route53Resolver::* types CreateStack does not recognize at all, whereas FirewallRuleGroupAssociation is registered and deploys here. Handler code lives in the cloudformation service crate (services/service-com-amazonaws-cloudformation-cloudformation/, see src/lib.rs:10-13 for the ops/state module split).

## Evidence
Check cloudformation__route53resolver.deploy-firewall-rule-group-association-mutation-protection-blocks-delete, assertion at line 400 of the check:

  assert (message, code) == (want_message, "InvalidRequest")

where (message, code) are parsed out of the Association resource's DELETE_FAILED ResourceStatusReason with the regex
  ^Resource handler returned message: "(?P<message>.*)" \(RequestToken: [0-9a-f-]+, HandlerErrorCode: (?P<code>[A-Za-z]+)\)$
and the trace id is scrubbed to "<trace-id>".

Expected (AWS):
  message = 'Invalid request provided: DisassociateFirewallRuleGroupRequest(FirewallRuleGroupAssociationId=<association-id>)'
  code    = 'InvalidRequest'

Actual (emulator):
  message = '[RSLVR-02307] The DNS Firewall rule group association is under mutation protection. Remove the mutation protection and try again. Trace Id: "<trace-id>"'
  code    = 'InvalidRequest'

Runner output:
  AssertionError: delete-refusal reason ('[RSLVR-02307] The DNS Firewall rule group association is under mutation protection. Remove the mutation protection and try again. Trace Id: "<trace-id>"', 'InvalidRequest'), expected ('Invalid request provided: DisassociateFirewallRuleGroupRequest(FirewallRuleGroupAssociationId=rslvr-frgassoc-1d0b14e4db018fed)', 'InvalidRequest')

Everything else in the check matched: stack CREATE_COMPLETE, Outputs/Ref/Fn::GetAtt Arn, GetFirewallRuleGroupAssociation reading MutationProtection=ENABLED and Priority=101, DeleteStack reaching DELETE_FAILED, HandlerErrorCode InvalidRequest, and the association surviving the refused delete (Status COMPLETE, MutationProtection ENABLED, Priority 101). Disabling MutationProtection then deleting the stack succeeds.

## Steps to Reproduce
1. Create a VPC (ec2:CreateVpc 10.0.0.0/16) and wait for it to be available.
2. cloudformation:CreateStack with a template declaring AWS::Route53Resolver::FirewallDomainList, AWS::Route53Resolver::FirewallRuleGroup and AWS::Route53Resolver::FirewallRuleGroupAssociation, where the association sets VpcId=<vpc>, Priority=101, MutationProtection=ENABLED and Name=<unique>, with Outputs exporting Ref and Fn::GetAtt Arn of the association. Wait for CREATE_COMPLETE.
3. route53resolver:GetFirewallRuleGroupAssociation(FirewallRuleGroupAssociationId=<AssocRef>) — Status COMPLETE, MutationProtection ENABLED, Priority 101 (matches).
4. cloudformation:DeleteStack(StackName=<stack-id>) and poll DescribeStacks until DELETE_FAILED.
5. cloudformation:DescribeStackEvents(StackName=<stack-id>) and read the Association resource's DELETE_FAILED ResourceStatusReason. The emulator reports handler message '[RSLVR-02307] The DNS Firewall rule group association is under mutation protection. Remove the mutation protection and try again. Trace Id: "<id>"'; AWS reports 'Invalid request provided: DisassociateFirewallRuleGroupRequest(FirewallRuleGroupAssociationId=<AssocRef>)'. Both carry HandlerErrorCode InvalidRequest.
6. Cleanup/positive control: route53resolver:UpdateFirewallRuleGroupAssociation(MutationProtection=DISABLED), wait for COMPLETE/DISABLED, then DeleteStack again — it reaches DELETE_COMPLETE.
7. Replay: run_behavioral_suite and re-run the check in trace_ids.
