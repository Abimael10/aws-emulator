Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: route53resolver.system-resolver-rule-and-policy
Issue-Shape-Id: com.amazonaws.route53resolver#Route53Resolver
Issue-Key: route53resolver.system-resolver-rule-and-policy|run#19|AssertionError|assert json.loads(got) == {}, f"self-share policy must store nothing, got {got}"
Issue-Commit: f1b556c6bf78e22ad46aec863809a9b606b2a702

# route53resolver: PutResolverRulePolicy stores a self-share policy instead of discarding it, so GetResolverRulePolicy echoes the document back where AWS returns an empty policy

> _Note: this issue was identified against the code base at commit `f1b556c6bf78e22ad46aec863809a9b606b2a702`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutResolverRulePolicy

## Description
Cause 1 of 1: a resolver-rule share whose only principal is the rule's OWN owner account is persisted verbatim instead of being dropped.

AWS: Smithy (com-amazonaws-route53resolver-route53resolver) models PutResolverRulePolicy with required input Arn (@length 1..255) and ResolverRulePolicy (@length 0..30000) and output ReturnValue (boolean, default false); GetResolverRulePolicy returns ResolverRulePolicy (@length 0..30000). The model states nothing about which principals are retained, so for the retention rule this is recorded behavior only: the recording shows real Route 53 Resolver ACCEPT the Put (HTTP 200, ReturnValue=true) for a policy whose single Principal.AWS entry is the calling account that owns the rule, and then shows GetResolverRulePolicy on the same Arn return a ResolverRulePolicy that parses to the EMPTY JSON object — sharing a rule with yourself is a no-op, so nothing is stored. Neither call is refused; no error code or message is involved on either side.

Emulator: PutResolverRulePolicy is accepted (ReturnValue=true, matching AWS), but the policy document is stored as submitted, and GetResolverRulePolicy on the same rule ARN returns that document back. The replayed response body was the submitted statement re-serialized — Version "2012-10-17", one Allow statement whose Principal.AWS is the owner account rendered as arn:aws:iam::<account-id>:root, Action the five route53resolver rule actions the request listed, Resource the rule's own ARN. (The concrete account id, rule id and region in that body are per-run values of the replay environment, not part of the requirement; the requirement is that the stored policy be empty whenever the only principal resolves to the rule's owner account.)

Why: a client that shares a rule and then reads the policy back to decide whether a share exists concludes the rule IS shared with the owner account. Teardown/idempotence logic that treats a non-empty GetResolverRulePolicy as "an outstanding share to revoke" loops or errors against the emulator where it exits immediately against AWS, and code that diffs the desired share set against the read-back policy sees a spurious self-principal statement it can never remove.

Fix: PutResolverRulePolicy's handler/state write for resolver-rule policies in the route53resolver service crate — when canonicalizing the submitted ResolverRulePolicy, resolve each statement's principals and drop the ones naming the rule's own owner account (both the bare account-id and the arn:<partition>:iam::<account-id>:root forms); a statement left with no principals, and a policy left with no statements, must persist as no stored policy so GetResolverRulePolicy answers an empty policy document. The Put itself must keep answering 200 with ReturnValue=true.

Traces: route53resolver.system-resolver-rule-and-policy (assertion at check line 129: "self-share policy must store nothing"). The same check's SYSTEM-rule creation, COMPLETE status poll and ListResolverRules assertions all passed, so the divergence is isolated to the rule-policy store/read path.

## Evidence
Check route53resolver.system-resolver-rule-and-policy, verdict mismatch.

Sequence: CreateResolverRule(RuleType=SYSTEM, DomainName=<unique>.example.internal.) -> rule reaches COMPLETE and appears in ListResolverRules (all passed) -> PutResolverRulePolicy(Arn=<rule arn>, ResolverRulePolicy={Version 2012-10-17, one Allow statement, Principal.AWS=[<own account id>], Action=[route53resolver:GetResolverRule, AssociateResolverRule, DisassociateResolverRule, ListResolverRules, ListResolverRuleAssociations], Resource=[<rule arn>]}) -> ReturnValue true (matches AWS) -> GetResolverRulePolicy(Arn=<rule arn>).

Expected (AWS, recorded): ResolverRulePolicy whose JSON parses to {} (empty policy; the self-share stored nothing). HTTP 200, no error.

Actual (emulator): ResolverRulePolicy is the submitted document echoed back —
{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::<account-id>:root"},"Action":["route53resolver:GetResolverRule","route53resolver:AssociateResolverRule","route53resolver:DisassociateResolverRule","route53resolver:ListResolverRules","route53resolver:ListResolverRuleAssociations"],"Resource":"arn:aws:route53resolver:<region>:<account-id>:resolver-rule/<rule-id>"}]}
raising: AssertionError: self-share policy must store nothing, got {...}

Fixed text of the divergence: a non-empty policy document where AWS serves an empty one. The account id, region and rslvr-rr-* rule id in the body are per-run values from the request/state, not requirements.

## Steps to Reproduce
1. CreateResolverRule(RuleType="SYSTEM", DomainName="<unique>.example.internal.", Name=<unique>, CreatorRequestId=<unique>); note ResolverRule.Id and ResolverRule.Arn.
2. Poll GetResolverRule until Status == COMPLETE.
3. PutResolverRulePolicy(Arn=<rule arn>, ResolverRulePolicy='{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"AWS":["<the calling account id>"]},"Action":["route53resolver:GetResolverRule","route53resolver:AssociateResolverRule","route53resolver:DisassociateResolverRule","route53resolver:ListResolverRules","route53resolver:ListResolverRuleAssociations"],"Resource":["<rule arn>"]}]}') and observe ReturnValue == true.
4. GetResolverRulePolicy(Arn=<rule arn>) and json-parse ResolverRulePolicy.
   AWS: {} . Emulator: the document submitted in step 3, with the principal rendered as arn:aws:iam::<account-id>:root.
5. Cleanup: DeleteResolverRule(ResolverRuleId=<rule id>).
