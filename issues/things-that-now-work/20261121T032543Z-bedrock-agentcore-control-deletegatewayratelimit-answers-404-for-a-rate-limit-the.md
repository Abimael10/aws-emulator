Issue-Source: trace-sweep-divergence
Issue-Trace: acc-gateway-rate-limit-batch-and-update
Issue-Step: teardown#12
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: acc-gateway-rate-limit-batch-and-update
Issue-Shape-Id: com.amazonaws.bedrockagentcorecontrol#AmazonBedrockAgentCoreControl
Issue-Key: bedrock-agentcore-control|DeleteGatewayRateLimit|2xx->4xx|$.rateLimitId
Issue-Key: bedrock-agentcore-control|DeleteGatewayRateLimit|2xx->4xx|$.status
Issue-Key: bedrock-agentcore-control|DeleteGatewayRateLimit|2xx->4xx|status
Issue-Commit: ddedefe2ba7626fc434a3b439556791987e3d078

# DeleteGatewayRateLimit answers 404 for a rate limit the gateway still holds, because the batch write replaces the whole set

> _Note: this issue was identified against the code base at commit `ddedefe2ba7626fc434a3b439556791987e3d078`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteGatewayRateLimit

## Description
One net-new wire divergence on the bedrock-agentcore-control gateway rate-limit surface. There is ONE cause and it must be closed.

Cause 1 of 1: DeleteGatewayRateLimit refuses with 404 ResourceNotFoundException a rateLimitId the same gateway was given earlier in the session and that no caller has deleted, so the rate limit is unreachable for deletion (fixture acc-gateway-rate-limit-batch-and-update, step teardown#12).

AWS: Smithy: DeleteGatewayRateLimit is DELETE /gateways/{gatewayIdentifier}/rate-limits/{rateLimitId}, success status 200, @idempotent; both path members are @required (gatewayIdentifier pattern ^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$, rateLimitId GatewayRateLimitId @length 2..64). The output shape DeleteGatewayRateLimitResponse declares rateLimitId @required and status @required targeting GatewayRateLimitStatus (CREATING | ACTIVE | UPDATING | DELETING). Modeled errors include ResourceNotFoundException (404), reserved for "a resource referenced by the operation does not exist". Recorded behavior: deleting a rate limit that the gateway holds answers 200 with a body naming the rateLimitId taken from the request path and status 'DELETING' (the recording's id value 'sl6xa23nov' is one run's generated id and is not part of the requirement). The limit survives the gateway's batch write: Smithy BatchPutGatewayRateLimits documents "Atomically creates or updates multiple rate limits for a gateway. The operation updates existing limits with matching keys and creates new limits for new keys" — an upsert, with no removal of a limit the batch does not name — so a limit created before the batch is still addressable afterwards, which is exactly what the recording's teardown relies on when it deletes it.

Emulator: answers status 404 with x-amzn-errortype ResourceNotFoundException and NO body members at all for the delete ($.rateLimitId and $.status are both absent where the modeled @required members were expected). The 404 is raised at the rate-limit lookup in the DeleteGatewayRateLimit handler, i.e. the gateway resolves but the id names no limit of it, so by that point the emulator's state no longer holds the limit.

Why: a client that created a rate limit and later deletes it — the ordinary create/configure/clean-up cycle, and what any teardown or IaC destroy does — is told the rate limit does not exist. Cleanup aborts or is reported as a failure, the throttle the operator believes is in force is in an unknown state, and a client that treats 404 as "already gone" is silently wrong about which limits the gateway still enforces. The two @required output members are also missing, so an SDK gets neither the id nor the DELETING status it must poll on.

Hypothesis: the limit was destroyed by the preceding batch write rather than by any delete. BatchPutGatewayRateLimits assigns the submitted set over the stored set wholesale instead of upserting it, so every rate limit the batch does not name is dropped from the gateway; the later delete of that earlier-created id then finds nothing and raises the modeled 404. A second possibility, if the step repeats a delete, is that the accepted-deletion record is removed as soon as the chosen removal span elapses, so a repeat delete 404s where AWS still reports DELETING for the limit it is removing.

Fix: the write into gateway rate-limit state performed by BatchPutGatewayRateLimits — it must upsert the submitted entries onto the gateway's existing rate-limit map (matching on rateLimitId, and on the dimension-key set where the entry supplies no id) rather than overwriting the map, so a limit the batch never names stays addressable by DeleteGatewayRateLimit; and the DeleteGatewayRateLimit handler's not-found path, which must only be reached for an id the gateway genuinely does not hold, and which must keep reporting 200 with rateLimitId + status DELETING for an accepted deletion (including a repeat while the removal is still owed, since the operation is @idempotent).

Traces: acc-gateway-rate-limit-batch-and-update#teardown#12. Code: services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/batch_put_gateway_rate_limits.rs line 302 (`gateway.rate_limits = written;`); services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/ops/delete_gateway_rate_limit.rs lines 196-212 (the lookup that raises crate::state::gateway_rate_limit_not_found, then accept_deletion/delete_output); services/service-com-amazonaws-bedrockagentcorecontrol-amazonbedrockagentcorecontrol/src/state/gateway_rate_limits.rs lines 44 and 215-224 (the removal span and accept_deletion).

Related open issue, which does NOT cover this divergence: 20261118T170231Z-bedrock-agentcore-control-batchputgatewayratelimits-destroys-rate-limits-it-does-not-name.md reports the destructive-replace fault from the BEHAVIORAL suite on the GetGatewayRateLimit read path only, and carries no wire replay key for this delete step; its other two causes (GetGatewayRateLimit and UpdateOauth2CredentialProvider wording) are unrelated to the delete path. 20261118T191327Z-bedrock-agentcore-control-5-net-new-causes-in.md covers this same fixture at steps task#7 and task#11 (UpdateGatewayRateLimit timestamps and 404 wording), not teardown#12. This divergence was reported by the replay with no owning issue and no stub path.

## Evidence
Replay of trace set simparity-wire-aws-v1 (58 fixtures, 298 steps, 246 passed, score 82.6%). This cluster carried an empty stub_path and an empty owned_by, so no filed issue holds its replay key.

Cause 1 — DeleteGatewayRateLimit, fixture acc-gateway-rate-limit-batch-and-update, step teardown#12, verdict mismatch:
  status: expected 200, got 404
  $.rateLimitId: missing (expected the rateLimitId from the request path — 'sl6xa23nov' in this run — got None)
  $.status: missing (expected 'DELETING', got None)
  divergence_key service=bedrock-agentcore-control|DeleteGatewayRateLimit|status=200->404|keys=$.rateLimitId,$.status,status
  affected_count 1 (acc-gateway-rate-limit-batch-and-update#teardown#12)

The 404 body carries x-amzn-errortype ResourceNotFoundException and none of the modeled output members. HTTP 404 and the code ResourceNotFoundException are the right error CLASS for an id a gateway does not hold — the defect is that this id IS one the gateway was given and that nothing deleted, so no refusal is due at all.

Model citations: DeleteGatewayRateLimit DELETE /gateways/{gatewayIdentifier}/rate-limits/{rateLimitId}, success 200, @idempotent; DeleteGatewayRateLimitResponse.rateLimitId @required (GatewayRateLimitId @length 2..64, pattern ^[a-zA-Z0-9][a-zA-Z0-9\-_\.]{0,62}[a-zA-Z0-9]$) and .status @required (GatewayRateLimitStatus: CREATING | ACTIVE | UPDATING | DELETING); errors AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400. BatchPutGatewayRateLimits doc: "Atomically creates or updates multiple rate limits for a gateway. The operation updates existing limits with matching keys and creates new limits for new keys."

Emulator code read at the current checkout: batch_put_gateway_rate_limits.rs line 302 replaces the stored map (`gateway.rate_limits = written;`) rather than upserting onto it, which leaves no record for the earlier-created id the teardown then deletes.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the simparity-wire-aws-v1 wire corpus; the divergence is fixture acc-gateway-rate-limit-batch-and-update, step teardown#12.
3. By hand: CreateGateway (authorizerType AWS_IAM, protocolType MCP) and keep its gatewayIdentifier.
4. CreateGatewayRateLimit on that gateway with one dimension-key scope (e.g. dimensionKeys=['targetName']) and one entry; keep the returned rateLimitId R.
5. BatchPutGatewayRateLimits on the same gateway submitting rate limits over DIFFERENT dimension-key scopes (e.g. ['toolName'] and ['qualifiedModelId']), none of them carrying rateLimitId R.
6. DELETE /gateways/{gatewayIdentifier}/rate-limits/R (DeleteGatewayRateLimit): the emulator answers 404 ResourceNotFoundException with no body members; AWS answers 200 with {"rateLimitId": R, "status": "DELETING"}.
7. Cross-check the alternative path: delete a rate limit the gateway holds, wait longer than the emulator's chosen removal span, then repeat the same DELETE — an @idempotent repeat must still report 200 with status DELETING while the removal is owed.
