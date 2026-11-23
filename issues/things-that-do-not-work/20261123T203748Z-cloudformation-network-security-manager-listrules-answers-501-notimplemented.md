Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__network-security-manager.deploy-network-security-manager-policy-ambiguous-reference-rejected
Issue-Key: cloudformation__network-security-manager|ListRules|NotImplemented
Issue-Commit: 66c10af8263c3210e92c7ec64217ef7e1817c3dd

# cloudformation__network-security-manager: ListRules answers 501 NotImplemented, so CloudFormation rollback-absence of AWS::NetworkSecurityManager::Rule cannot be read back

> _Note: this issue was identified against the code base at commit `66c10af8263c3210e92c7ec64217ef7e1817c3dd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListRules

## Description
One cause.

Cause 1 of 1: Network Security Manager's ListRules is an unimplemented scaffold stub, so the cross-service CloudFormation check that confirms a rolled-back AWS::NetworkSecurityManager::Rule is absent from the service's own list cannot complete. Failing check id: cloudformation__network-security-manager.deploy-network-security-manager-policy-ambiguous-reference-rejected.

AWS: Smithy (com.amazonaws.networksecuritymanager#PiccoloCustomerApiService): ListRules is `GET /rules`, HTTP 200, @readonly and @paginated (inputToken nextToken, outputToken nextToken, pageSize maxResults, items rules). Input members are all optional query params — maxResults (@range min 1 max 100), nextToken (@length 1..4096, @pattern ^[0-9A-Za-z:/+=_-]+$), status (EntityStatusFilter enum). Output shape ListRulesOutput declares `rules` (RuleSummaryList, @required, @length max 100) plus an optional `nextToken`. The ONLY modeled errors are AccessDeniedException (403), ValidationException (400), ThrottlingException (429) and InternalServerException (500); there is no NotImplemented error in the model or in the service's common errors. So a well-formed, unfiltered ListRules on an account with no rules must answer 200 with `rules: []` (and no nextToken), and with rules present must answer 200 listing a RuleSummary per rule, page by page.

Emulator: the call is refused. botocore raises `ClientError: An error occurred (NotImplemented) when calling the ListRules operation: operation ListRules is scaffolded but not implemented in this emulator`. The error code `NotImplemented` and that message are emitted by the scaffold stub, not by any modeled error; the status is a non-2xx where AWS answers 200. Everything the check exercises BEFORE this point already matches: the ambiguous-reference stack reaches ROLLBACK_COMPLETE and the Policy resource's handler reason matches AWS's rejection exactly. The failure is purely the refusal of the read-back.

Why: a client cannot enumerate rules at all, so it cannot confirm that a failed/rolled-back CloudFormation deployment left no orphan rule behind, cannot reconcile stack state against service state, and cannot paginate rules for inventory or cleanup. `NotImplemented` is also not in the operation's modeled error set, so a generated SDK's typed error handling falls through to a generic ClientError instead of one of the four modeled exceptions — retry/backoff logic keyed on ThrottlingException/InternalServerException never engages, and a caller treating a 501 as fatal aborts a deploy that AWS would let proceed.

Fix: implement the ListRules operation handler (services/service-com-amazonaws-networksecuritymanager-piccolocustomerapiservice/src/ops/list_rules.rs) over the rule state the CreateRule/DeleteRule path already maintains: honour the optional `status` filter and the maxResults/nextToken paging contract, serialize `rules` as a RuleSummaryList that is always present (empty list, never omitted and never null) and only emit `nextToken` when a further page exists. Input validation for maxResults range and nextToken length/pattern belongs in the operation's input validation and must reject with ValidationException (400), not NotImplemented.

Note: open issue 20261118T113746Z-network-security-manager-listrules-stub.md names the same stub under the `network-security-manager` service; it does not carry this cross-service check's replay key, which the suite left unmarked, so this item records it under the cloudformation__network-security-manager heading. Closing the stub should close both.

Traces: cloudformation__network-security-manager.deploy-network-security-manager-policy-ambiguous-reference-rejected (errors at the list_rules rollback-absence read, after the ROLLBACK_COMPLETE and handler-reason assertions pass).

## Evidence
Expected (AWS, per Smithy ListRules: GET /rules -> 200, output `rules` @required): HTTP 200 with body {"rules": []} for the rolled-back rule name — the check filters the paginated `rules` items by ruleName and asserts the match list is empty.

Actual (emulator): the ListRules call is refused instead of answered:
  botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the ListRules operation: operation ListRules is scaffolded but not implemented in this emulator

Message anatomy: the fixed text is the scaffold's own "operation <Name> is scaffolded but not implemented in this emulator" with code `NotImplemented`; `ListRules` is the only request-derived part. Neither the code nor the message is modeled — ListRules models only AccessDeniedException/ValidationException/ThrottlingException/InternalServerException.

Preceding assertions in the same check that PASSED (so they are not part of this defect): stack status == ROLLBACK_COMPLETE for the Policy whose AssociatedTemplateAndRuleList entry names both a TemplateArn and a RuleArn, and the Policy resource's CREATE_FAILED handler reason matching Network Security Manager's rejection text. ListPolicies and ListTemplates were reached; ListRules is where the check errored.

## Steps to Reproduce
1. Boot the emulator and create clients for `cloudformation` and `network-security-manager`.
2. CreateStack with a template containing AWS::NetworkSecurityManager::Rule (FirewallType WAF, RuleType CONFIGURATION), AWS::NetworkSecurityManager::Template referencing Fn::GetAtt [Rule, RuleArn], and AWS::NetworkSecurityManager::Policy whose AssociatedTemplateAndRuleList entry names BOTH TemplateArn and RuleArn. Poll DescribeStacks until terminal — it reaches ROLLBACK_COMPLETE as AWS does.
3. Call ListRules with no arguments (e.g. via the `list_rules` paginator) to confirm the rolled-back rule name is absent.
4. Observe the ClientError (NotImplemented) "operation ListRules is scaffolded but not implemented in this emulator" instead of HTTP 200 with a `rules` list.
5. Equivalently, plain `GET /rules` on a fresh emulator returns the same refusal where AWS returns 200 {"rules": []}.
