Issue-Source: trace-sweep-divergence
Issue-Trace: bedrock-agentcore-error-double-stop-and-invoke-terminated
Issue-Step: setup#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: bedrock-agentcore-error-double-stop-and-invoke-terminated
Issue-Shape-Id: com.amazonaws.bedrockagentcore#AmazonBedrockAgentCore
Issue-Key: bedrock-agentcore|StartBrowserSession|2xx->2xx|$.streams.automationStream.streamEndpoint
Issue-Key: bedrock-agentcore|StartBrowserSession|2xx->2xx|$.streams.liveViewStream.streamEndpoint
Issue-Commit: b5d0ebc6502388b6804721a67a38a04334f3e770

# bedrock-agentcore browser session stream endpoints are minted on the emulator's own host:port instead of the service's regional endpoint host

> _Note: this issue was identified against the code base at commit `b5d0ebc6502388b6804721a67a38a04334f3e770`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartBrowserSession

## Description
Cause 1 of 1: StartBrowserSession materialises the session's `streams` endpoints against whatever address the emulator process happens to answer on, so the automation and live-view endpoints are minted in a form no AgentCore browser client can treat as a Bedrock AgentCore stream address.

AWS: Smithy (com.amazonaws.bedrockagentcore, version 2024-02-28, restJson1) models `StartBrowserSessionResponse$streams` (shape `BrowserSessionStream`) with `automationStream` @required; `AutomationStream$streamEndpoint` is @required with @length 10..512 and is documented as "The endpoint URL for the automation stream. This URL is used to establish a WebSocket connection to the stream for sending commands and receiving responses", and `LiveViewStream$streamEndpoint` (@length 10..512) as "The endpoint URL for the live view stream". The service's wire identity is `awsService.endpointPrefix` = `bedrock-agentcore` with an endpoint rule set (smithy_query query="service_info"), so the host a stream endpoint is addressed on is the service's own resolved regional endpoint host for the caller's region — `bedrock-agentcore.<region>.amazonaws.com` — and NOT an arbitrary process address. The recording shows the endpoint layout the service uses beneath that host (recorded behavior only for the path shape): `wss://bedrock-agentcore.<region>.amazonaws.com/browser-streams/<browserIdentifier>/sessions/<sessionId>/automation` for the automation stream and `https://bedrock-agentcore.<region>.amazonaws.com/browser-streams/<browserIdentifier>/sessions/<sessionId>/live-view` for the live view. Of that template, the fixed text is the scheme (`wss` for automation, the service's own `https` for the live view), the `bedrock-agentcore.<region>.amazonaws.com` authority and the `/browser-streams/.../sessions/.../automation|live-view` segments; the variable parts are the region (from the signed request), the `browserIdentifier` (taken from the request) and the `sessionId` (minted by this call and echoed in the same response). The recorded host/region values themselves are one run's values, not the requirement — the requirement is that the authority is the service endpoint host for the request's region.

Emulator: both endpoints are built from the address this emulator instance is answering on, with the service-endpoint authority replaced by a raw host:port. Replay of this step returned 200 with the right path layout and the right (minted) sessionId, but with the authority of the emulator process: `$.streams.automationStream.streamEndpoint` = `wss://127.0.0.1:45369/browser-streams/aws.browser.v1/sessions/<sessionId>/automation` and `$.streams.liveViewStream.streamEndpoint` = `https://127.0.0.1:45369/browser-streams/aws.browser.v1/sessions/<sessionId>/live-view`, where the step expects the `bedrock-agentcore.<region>.amazonaws.com` authority (comparator verdict `resource_not_identified` on both members). The code says so explicitly: `BrowserSessionStreamRecord::materialized` composes `{scheme}://{host}/browser-streams/...` from `OperationContext::endpoint()` (the address this emulator answers on) rather than from the service's endpoint resolution for the request's region. Note also that the announced authority is a port other than the data listener the replay reached this service on, so the announced address is not even the one the session can be dialled at — Hypothesis: the endpoint base handed to the handler is a per-process/listener address rather than the service address the caller used.

Why: the stream endpoints are the browser session's only usable surface. A client that starts a session in order to drive it must SigV4-sign the automation WebSocket connect for service `bedrock-agentcore` in the region the endpoint names, and derives both the signing region and the host header from that endpoint; an authority that is a bare host:port carries no region, is rejected by clients that validate the endpoint against the service's endpoint rules, and signs/dials wrongly when they do not. Embedding `liveViewStream.streamEndpoint` in a page has the same problem. So a session that correctly reports READY still cannot be automated or viewed by a client that relies on AWS's endpoint form, and the mismatch is silent — the member is present and well-formed, just not a Bedrock AgentCore stream address.

Fix: the browser-session state's stream materialisation plus the endpoint base the two reporting handlers hand it — the session record's stream endpoints should be composed on the service's resolved endpoint host for the request's region (endpointPrefix `bedrock-agentcore`), on an authority this emulator actually serves, keeping the existing `/browser-streams/{browserIdentifier}/sessions/{sessionId}/{automation|live-view}` layout, the `wss` scheme for the automation stream and the session's own minted `sessionId`. Because the record is written once at start and read back by GetBrowserSession/UpdateBrowserStream, fixing the materialisation fixes every reader. Prior filings: the earlier report that `streams` was omitted entirely (20261108T071214Z-bedrock-agentcore-browser-session-streams-are-never-reported-startbrowsersession-getbrowsersession.md) is a different cause and does not cover the endpoint form, and this divergence is not covered by any open issue's replay key.

Traces: bedrock-agentcore-error-double-stop-and-invoke-terminated#setup#1 (StartBrowserSession, 200, both `$.streams.automationStream.streamEndpoint` and `$.streams.liveViewStream.streamEndpoint` diverge). Code citations: services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/state/browser.rs lines 146-161 (`BrowserSessionStreamRecord::materialized`, which strips `https://` off the emulator's endpoint base and uses the rest as the authority) and lines 97-120 (the path/scheme constants); services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/start_browser_session.rs lines 231-235 and 290-299 (the endpoint base taken from `ctx.endpoint()` and the streams put on the response).

## Evidence
Step bedrock-agentcore-error-double-stop-and-invoke-terminated#setup#1, StartBrowserSession (browserIdentifier=aws.browser.v1), HTTP 200 both expected and actual; the divergence is in the response's `streams` member.

Expected (AWS form, authority = the service's resolved regional endpoint host for the request's region, sessionId = the id this same response minted):
  $.streams.automationStream.streamEndpoint = "wss://bedrock-agentcore.us-west-2.amazonaws.com/browser-streams/aws.browser.v1/sessions/{{session_id}}/automation"
  $.streams.liveViewStream.streamEndpoint   = "https://bedrock-agentcore.us-west-2.amazonaws.com/browser-streams/aws.browser.v1/sessions/{{session_id}}/live-view"
(the `us-west-2` in the recording is that run's region and the `{{session_id}}` is the per-call minted id; the requirement is the `bedrock-agentcore.<region>.amazonaws.com` authority plus the fixed `/browser-streams/{browserIdentifier}/sessions/{sessionId}/automation|live-view` path and the `wss`/`https` schemes)

Actual (emulator):
  $.streams.automationStream.streamEndpoint = "wss://127.0.0.1:45369/browser-streams/aws.browser.v1/sessions/1a1df6c953a0000000000000001/automation"
  $.streams.liveViewStream.streamEndpoint   = "https://127.0.0.1:45369/browser-streams/aws.browser.v1/sessions/1a1df6c953a0000000000000001/live-view"
Comparator verdict: `resource_not_identified` on both members (status 200 == 200; the path layout and the echoed sessionId are correct — only the authority is not a Bedrock AgentCore endpoint host). The emulator's own health report for this run names a different data-listener port (:42653) than the :45369 authority it announced here, so the announced stream address is additionally not the address the session is reachable at.

Source of the divergence (not a request-specific value): services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/state/browser.rs:146-161 composes both endpoints from `endpoint_base` (the emulator's own answering address, `OperationContext::endpoint()`), passed in at services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/src/ops/start_browser_session.rs:234-235 and stored with the session at :290.

## Steps to Reproduce
1. Against the emulator's bedrock-agentcore (data plane) endpoint, call StartBrowserSession with browserIdentifier="aws.browser.v1" (the service-provided built-in browser) and a sessionTimeoutSeconds, as fixture bedrock-agentcore-error-double-stop-and-invoke-terminated does in its setup step.
2. Read `streams.automationStream.streamEndpoint` and `streams.liveViewStream.streamEndpoint` from the 200 response. Both are `<scheme>://<emulator-host>:<emulator-port>/browser-streams/aws.browser.v1/sessions/<sessionId>/{automation,live-view}` — the authority is the emulator process's own address, not `bedrock-agentcore.<region>.amazonaws.com` as AWS returns for the signed region.
3. Call GetBrowserSession on the same (browserIdentifier, sessionId): the same stored endpoints are reported back, so no reader disagrees with the start — the form is wrong in one place.
4. Equivalent replay: replay fixture bedrock-agentcore-error-double-stop-and-invoke-terminated, step setup#1.
