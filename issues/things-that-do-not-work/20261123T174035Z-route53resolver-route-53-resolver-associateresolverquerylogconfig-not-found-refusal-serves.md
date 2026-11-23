Issue-Source: trace-sweep-divergence
Issue-Trace: route53resolver-query-log-config-missing-resource-errors
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: route53resolver-query-log-config-missing-resource-errors
Issue-Shape-Id: com.amazonaws.route53resolver#Route53Resolver
Issue-Key: route53resolver|AssociateResolverQueryLogConfig|4xx->4xx|$.Message
Issue-Commit: e1a216654277447e9cfd71796b175a3691c9764d

# Route 53 Resolver AssociateResolverQueryLogConfig not-found refusal serves a Message that diverges from the recorded AWS body

> _Note: this issue was identified against the code base at commit `e1a216654277447e9cfd71796b175a3691c9764d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateResolverQueryLogConfig

## Description
Cause 1 of 1: the 400 refusal AssociateResolverQueryLogConfig answers for a ResolverQueryLogConfigId the service holds no configuration under carries an error Message whose body does not match the Message the recorded AWS step carries for that same refusal. The HTTP status (400) and the error shape agree with the recording; only $.Message diverges, so the defect is in the body of the refusal, not in its routing.

AWS: Smithy (services/service-com-amazonaws-route53resolver-route53resolver/smithy-model/com-amazonaws-route53resolver-route53resolver.json) models AssociateResolverQueryLogConfig with required ResolverQueryLogConfigId and ResourceId (both ResourceId, @length 1-64) and binds AccessDeniedException, InternalServiceErrorException, InvalidParameterException, InvalidRequestException, LimitExceededException, ResourceExistsException, ResourceNotFoundException and ThrottlingException, every one HTTP 400 under awsJson1_1. The model states no message text for any of them, so the requirement is the RECORDED behavior of this step: Route 53 Resolver reports an unaddressable query logging configuration under its own service error code RSLVR-01601 and closes the body with the service-wide trailing trace clause. In that message the FIXED text is the bracketed code prefix `[RSLVR-01601] ` and the sentence "The specified query logging configuration doesn't exist."; the only VARIABLE part is the trace clause ` Trace Id: "<root>"`, whose value is per-request and must be the Root of the X-Amzn-Trace-Id header the request carried (a freshly minted root only when the caller sent none) — nothing in the message is derived from the request's ResolverQueryLogConfigId or ResourceId. Because the trace root is per-request, the fixture asserts this Message by shape rather than by literal text (the step reports its expected value as `<shaped-not-asserted>`), and the emulator's body does not satisfy that shape.

Emulator: replaying fixture route53resolver-query-log-config-missing-resource-errors step task#5 returns HTTP 400 with $.Message = '[RSLVR-01601] The specified query logging configuration doesn't exist. Trace Id: "1-6b04796d-200542efb6d10ae3491befff"' (the trace-id value is one run's, not a requirement), which the replay reports as a value_mismatch against the recorded body. In the checkout the body comes from crate::state::invalid_resolver_query_log_config_id, which the handler raises as InvalidParameterException with FieldName="ResolverQueryLogConfigId", while the sibling builder crate::state::resolver_query_log_config_not_found raises the SAME RSLVR-01601 sentence as ResourceNotFoundException with ResourceType="ResolverQueryLogConfig" — the two refusal bodies for one fault differ in class, in the trailing members they carry, and only one of them matches the recorded step. Hypothesis: the message the recorded AWS step carries belongs to the other of those two bodies (RSLVR-01601 reported as a missing RESOURCE rather than as a bad PARAMETER, i.e. with ResourceType rather than FieldName beside the sentence), or the trailing trace clause does not echo the Root of the request's X-Amzn-Trace-Id header because the handler mints a fresh root for forms crate::state::request_trace_root does not parse.

Why: a client that reads the refusal's Message is the one path AWS leaves it to distinguish "this configuration does not exist" (create it, or treat the association as absent) from any other 400 on this operation, because every modeled error of the operation shares HTTP 400 and the message's RSLVR code is the only discriminator. A body that differs from AWS's for this fault defeats log-, alarm- and retry-matching keyed on the RSLVR-01601 sentence, and a trace clause that does not echo the caller's trace root makes the refusal uncorrelatable with the request the caller traced — the very use the clause exists for. Idempotent cleanup paths (disassociate/delete after a failed associate) branch on this refusal and mis-branch when its body is not AWS's.

Fix: the AssociateResolverQueryLogConfig handler's config-resolution path in the route53resolver service crate and the shared refusal builder it calls — the handler must serve, for an unresolvable ResolverQueryLogConfigId, exactly the one RSLVR-01601 body the recording carries (its fixed code prefix and sentence, with the trailing trace clause taken from the request's own trace root), rather than a second, differently-classed copy of the same sentence; the two builders for this one fault should collapse into the recorded one. Note the related open behavioral issue 20261123T091006Z-route53resolver-route-53-resolver-associateresolverquerylogconfig-with-a-nonexistent-resolverquerylogconfigid.md, which asserts the CODE for this operation (ResourceNotFoundException vs InvalidParameterException) but carries no replay key for this wire step and so does not cover this response body; fixing the class alone will not make this step replay clean unless the Message body is brought to the recorded one too.

Traces: fixture route53resolver-query-log-config-missing-resource-errors step task#5 (mismatch, $.Message, expected status 400 / actual status 400).

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture route53resolver-query-log-config-missing-resource-errors, step task#5, operation AssociateResolverQueryLogConfig, verdict mismatch.

Expected (recorded AWS): HTTP 400 with $.Message asserted by shape (reported as `<shaped-not-asserted>` because its trailing ` Trace Id: "<root>"` clause is per-request). Fixed text required for this fault: `[RSLVR-01601] ` + "The specified query logging configuration doesn't exist."; variable part: the trace root echoed from the request.

Actual (emulator): HTTP 400 with $.Message = '[RSLVR-01601] The specified query logging configuration doesn't exist. Trace Id: "1-6b04796d-200542efb6d10ae3491befff"' — reported as `$.Message: value_mismatch`. Status and error shape matched (expected 400, got 400); no other key diverged in this step.

Source evidence in the checkout:
- services/service-com-amazonaws-route53resolver-route53resolver/src/ops/associate_resolver_query_log_config.rs lines 188-201 and 225-235: an id the store holds no configuration under is refused as InvalidParameterException via crate::state::invalid_resolver_query_log_config_id(&trace_root).
- services/service-com-amazonaws-route53resolver-route53resolver/src/state/query_log.rs lines 355-366: that builder emits the RSLVR-01601 sentence with FieldName="ResolverQueryLogConfigId".
- services/service-com-amazonaws-route53resolver-route53resolver/src/state/query_log.rs lines 206-217: crate::state::resolver_query_log_config_not_found emits the SAME RSLVR-01601 sentence as ResourceNotFoundException with ResourceType="ResolverQueryLogConfig" — two different bodies for one fault, only one of which can match the recording.
- services/service-com-amazonaws-route53resolver-route53resolver/src/state.rs lines 2085-2094: request_trace_root echoes the request's x-amzn-trace-id Root when continued_root parses it, and otherwise mints a new id; a form it fails to parse yields a trace clause that does not correspond to the caller's trace.

Model evidence: smithy_query(query="operation", operation="AssociateResolverQueryLogConfig") — ResolverQueryLogConfigId and ResourceId required @length 1-64; InvalidParameterException and ResourceNotFoundException both bound at HTTP 400; no modeled message text, so the recorded response is the requirement.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay wire fixture route53resolver-query-log-config-missing-resource-errors from trace set simparity-wire-aws-v1 and stop at step task#5.
3. Equivalent by hand: point a botocore route53resolver client at the emulator, create a VPC with ec2.create_vpc and wait for it to be available, then call route53resolver.associate_resolver_query_log_config(ResolverQueryLogConfigId="rqlc-0123456789abcdef0" (a well-formed id that was never created), ResourceId=<that VpcId>), sending an X-Amzn-Trace-Id header with a Root the caller chose.
4. Inspect the 400 response body: $.Message is '[RSLVR-01601] The specified query logging configuration doesn't exist. Trace Id: "<id>"' served behind InvalidParameterException with FieldName="ResolverQueryLogConfigId"; compare it to the recorded AWS Message for this step (fixed `[RSLVR-01601] ` prefix + sentence, trailing trace clause echoing the request's Root) — the replay reports $.Message as a value_mismatch.
5. Re-replay route53resolver-query-log-config-missing-resource-errors#task#5 to confirm a fix.
