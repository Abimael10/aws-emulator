Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: route53resolver.batch-create-firewall-rule-partial-failure
Issue-Shape-Id: com.amazonaws.route53resolver#Route53Resolver
Issue-Key: route53resolver.batch-create-firewall-rule-partial-failure|run#18|AssertionError|assert create_errors[0].get("Message") == (
Issue-Commit: 54e94d59ea058587fe0cd8b5d9bd32bdd3b540c1

# Route 53 Resolver BatchCreateFirewallRule: per-entry CreateErrors[].Message carries the top-level `Trace Id: "<Root>"` clause AWS does not put in an embedded batch error

> _Note: this issue was identified against the code base at commit `54e94d59ea058587fe0cd8b5d9bd32bdd3b540c1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchCreateFirewallRule

## Description
One net-new root cause in the Route 53 Resolver DNS Firewall rule-batch control plane.

=== Cause 1 of 1: the per-entry error Message of BatchCreateFirewallRule is built with the request's trace clause appended, but AWS's embedded per-entry Message is the bare coded sentence ===

Failing check id: route53resolver.batch-create-firewall-rule-partial-failure

AWS: BatchCreateFirewallRule is a partial-success operation — it answers HTTP 200 with CreatedFirewallRules and CreateErrors, and does NOT fail the whole call for an entry it cannot create (Smithy: BatchCreateFirewallRule(CreateFirewallRuleEntries: CreateFirewallRuleEntries required) -> BatchCreateFirewallRuleResponse{CreatedFirewallRules: FirewallRules, CreateErrors: BatchCreateFirewallRuleErrors}; errors AccessDeniedException | InternalServiceErrorException | LimitExceededException | ThrottlingException | ValidationException, all httpError 400). The per-entry error is a plain structure, NOT a modeled exception: BatchCreateFirewallRuleError{FirewallRule: CreateFirewallRuleEntry ("The firewall rule entry that caused the error"), Code: String ("The error code for the failure"), Message: String ("A message that provides details about the error")}. The model says nothing about the text, so the wording is recorded behavior only. The recorded response for a batch whose second entry names a FirewallDomainListId that does not exist is HTTP 200 with the first entry created, exactly one CreateErrors row naming that entry, Code `ResourceNotFoundException`, and Message:

    [RSLVR-02025] Can’t find the resource with ID "<FirewallDomainListId from that entry>".

Message anatomy. FIXED text: `[RSLVR-02025] Can’t find the resource with ID "` … `".` — the service's U+2019 right single quote in `Can’t`, the double quotes around the id, and the sentence-ending period as the LAST character of the message. VARIABLE part: the domain-list id taken verbatim from the offending CreateFirewallRuleEntry. The decisive point is what is ABSENT: the trailing ` Trace Id: "<Root>"` clause. That clause belongs to a top-level Resolver EXCEPTION body (the HTTP error response, where it is the X-Ray Root read from the caller's `X-Amzn-Trace-Id` header or minted by the service) — see the open issue 20261114T022311Z-route53resolver-route-53-resolver-dns-firewall-refusals-rslvr-02025.md, which documents that clause as a requirement of the top-level RSLVR-02025 body and does NOT cover this check. A batch per-entry error is not an HTTP error response: there is no exception for a trace to be attached to, so the embedded Message stops at the period. The check pins the message byte-exactly against the id it sent, so the requirement is the template, not any one id or Root.

Emulator: the call is otherwise right — HTTP 200, the good entry is created, exactly one CreateErrors row, that row names the bad entry, and Code is `ResourceNotFoundException` (all those assertions pass). Only the Message diverges: it is

    [RSLVR-02025] Can’t find the resource with ID "<id>". Trace Id: "<trace root>"

i.e. the correct sentence with a spurious ` Trace Id: "..."` clause appended. The handler reads the request's trace root and threads it into the single-rule create path (services/service-com-amazonaws-route53resolver-route53resolver/src/ops/batch_create_firewall_rule.rs:157 and :220 set `FirewallRuleCreate::trace_root`), the store formats the not-found sentence through the trace-appending formatter, and the handler then copies that whole exception Message verbatim into the per-entry row (same file, :227-231, via `FirewallRuleCreateRefusal::message()`). So a clause that is correct for a top-level exception body leaks into a response body where AWS never emits it.

Why: a client cannot compare the per-entry Message to anything stable. Code written against AWS that matches or asserts the exact per-entry sentence (negative tests, log matchers, SDK wrappers that re-raise the per-entry error, operator runbooks that strip the coded prefix and show the rest) sees extra text here; conversely a client that parses the per-entry Message for a trace Root to correlate with X-Ray will find one against the emulator and find none in production — the opposite failure of the top-level defect. Worse, the clause makes the per-entry message non-deterministic per call, so any equality assertion is impossible against the emulator while it is exactly what AWS supports. The check also aborts at this assertion, so the contracts after it are never exercised at all: that the rejected entry is absent from ListFirewallRules while the accepted one is visible.

Fix: the per-entry error projection of the batch operations, not the not-found sentence itself (the sentence is correct). `FirewallRuleCreateRefusal::message()` returns the stored exception's Message, which already has the trace clause baked in; the trace clause must be appended only where the refusal leaves as a top-level exception body (CreateFirewallRule), not where it is embedded in `CreateErrors`. Either keep the coded sentence and the trace clause separable on the refusal (sentence for the embedded row, sentence + clause for the exception) or have the batch handler project the untraced sentence. The same shape applies to the sibling batch surface — `FirewallRuleUpdateRefusal::message()` and the batch update/delete per-entry rows take their Message the same way — so the fix belongs in that shared refusal type rather than in one handler.

Traces: route53resolver.batch-create-firewall-rule-partial-failure (see trace_ids).

## Evidence
route53resolver.batch-create-firewall-rule-partial-failure [mismatch]

  File "check_route53resolver_batch_create_firewall_rule_partial_failure.py", line 69, in run
    assert create_errors[0].get("Message") == (
  AssertionError: {'FirewallRule': {'CreatorRequestId': 'sim2real-behav-cr-39b9a7afe0e3', 'FirewallRuleGroupId': 'rslvr-frg-910a2dec89025cc1', 'FirewallDomainListId': 'sim2real-behav-rslvr-fdl-doesnotexist-33b31615fccd', 'Priority': 501, 'Action': 'ALLOW', 'Name': 'sim2real-behav-fr-bad-975fa023ba2d'}, 'Code': 'ResourceNotFoundException', 'Message': '[RSLVR-02025] Can’t find the resource with ID "sim2real-behav-rslvr-fdl-doesnotexist-33b31615fccd". Trace Id: "1-6af8cd53-e6184fcb9718c9e308651c9f"'}

The check's assertion (the AWS requirement), from the check source:
    assert create_errors[0].get("Message") == (
        f'[RSLVR-02025] Can\u2019t find the resource with ID "{bogus_dl}".'
    ), create_errors[0]

Expected (AWS): HTTP 200; CreateErrors[0] = {FirewallRule: <the offending entry, echoed>, Code: "ResourceNotFoundException", Message: '[RSLVR-02025] Can’t find the resource with ID "<FirewallDomainListId of that entry>".'} — the message ENDS at the period.
Actual (emulator): HTTP 200; same FirewallRule echo (correct), same Code (correct), Message = that sentence plus a trailing ' Trace Id: "<root>"' clause. The id and the root above are this run's values filling the template; the divergence is the presence of the clause, not its value.

Everything the check asserts before the Message passes against the emulator, which isolates the defect to the embedded message text:
  - the good entry appears in CreatedFirewallRules;
  - the bad entry does NOT appear in CreatedFirewallRules;
  - len(CreateErrors) == 1;
  - CreateErrors[0].FirewallRule.Name is the bad entry's name;
  - CreateErrors[0].Code == "ResourceNotFoundException".
Not reached because of the abort: the ListFirewallRules observations (good rule visible, bad rule absent).

Model: BatchCreateFirewallRuleError{FirewallRule: CreateFirewallRuleEntry, Code: String, Message: String} is an ordinary output structure in BatchCreateFirewallRuleErrors — not an error shape, no httpError, no modeled message text; so the per-entry wording is recorded behavior only. The ` Trace Id: "<Root>"` clause is a property of Resolver's top-level exception bodies (open issue 20261114T022311Z-...-rslvr-02025.md requires it there and carries none of this check's keys).

Code path that produces the divergence:
  services/service-com-amazonaws-route53resolver-route53resolver/src/ops/batch_create_firewall_rule.rs:157  let trace_root = crate::state::request_trace_root(&ctx.parts().headers);
  services/service-com-amazonaws-route53resolver-route53resolver/src/ops/batch_create_firewall_rule.rs:220  trace_root: &trace_root,   // threaded into the single-rule create
  services/service-com-amazonaws-route53resolver-route53resolver/src/ops/batch_create_firewall_rule.rs:227-231 CreateErrors row built with message: refusal.message()
  services/service-com-amazonaws-route53resolver-route53resolver/src/state/firewall_rules.rs:108-114 FirewallRuleCreateRefusal::message() -> the exception's Message verbatim
  services/service-com-amazonaws-route53resolver-route53resolver/src/state.rs:2030-2032 traced_message(message, trace_root) -> format!("{message} Trace Id: \"{trace_root}\"")
  services/service-com-amazonaws-route53resolver-route53resolver/src/state.rs:1049-1068 resource_not_found_message / coded_resource_not_found_message -> the RSLVR-02025 sentence

## Steps to Reproduce
1. Create a boto3 route53resolver client against the emulator.
2. grp = route53resolver.create_firewall_rule_group(CreatorRequestId=<unique>, Name=<unique>)["FirewallRuleGroup"]["Id"].
3. dl = route53resolver.create_firewall_domain_list(CreatorRequestId=<unique>, Name=<unique>)["FirewallDomainList"]["Id"].
4. Pick bogus_dl = any id string that no domain list has (e.g. "rslvr-fdl-doesnotexist-<random>").
5. resp = route53resolver.batch_create_firewall_rule(CreateFirewallRuleEntries=[
     {"CreatorRequestId": <unique>, "FirewallDomainListId": dl,       "FirewallRuleGroupId": grp, "Priority": 500, "Action": "ALLOW", "Name": <good name>},
     {"CreatorRequestId": <unique>, "FirewallDomainListId": bogus_dl, "FirewallRuleGroupId": grp, "Priority": 501, "Action": "ALLOW", "Name": <bad name>},
   ])
6. Observe HTTP 200 with the good rule in resp["CreatedFirewallRules"] and exactly one row in resp["CreateErrors"] whose Code is "ResourceNotFoundException" — all correct.
7. Inspect resp["CreateErrors"][0]["Message"]. The emulator returns '[RSLVR-02025] Can\u2019t find the resource with ID "<bogus_dl>". Trace Id: "<root>"'. AWS returns '[RSLVR-02025] Can\u2019t find the resource with ID "<bogus_dl>".' with NO Trace Id clause — the message's last character is the period.
8. Repeat step 5 while sending NO X-Amzn-Trace-Id header, and again while pinning a fixed one via a before-sign hook: AWS's per-entry Message is identical in both cases (no clause), so the clause is not merely a per-run value — it is text the embedded row must not contain at all. By contrast, the TOP-LEVEL refusal of create_firewall_rule(FirewallDomainListId=bogus_dl, ...) must still end in ' Trace Id: "<Root>"' — the fix must not remove it there.
9. Then list_firewall_rules(FirewallRuleGroupId=grp): the good rule must be present and the rejected one absent (the assertions the check never reaches today).

Replay: run_behavioral_suite and re-run route53resolver.batch-create-firewall-rule-partial-failure.
