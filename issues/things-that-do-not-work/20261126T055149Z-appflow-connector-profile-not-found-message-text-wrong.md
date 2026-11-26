Issue-Source: trace-sweep-divergence
Issue-Trace: appflow-connector-profile-error-paths
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appflow-connector-profile-error-paths
Issue-Shape-Id: com.amazonaws.appflow#SandstoneConfigurationServiceLambda
Issue-Key: appflow|DeleteConnectorProfile|4xx->4xx|$.message
Issue-Key: appflow|UpdateConnectorProfile|4xx->4xx|$.message
Issue-Commit: bb631773176252a429a8a518c5447d51faf32ae8

# AppFlow connector-profile not-found message text wrong on Delete and Update

> _Note: this issue was identified against the code base at commit `bb631773176252a429a8a518c5447d51faf32ae8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteConnectorProfile

## Description
Both connector-profile operations that can miss an existing profile answer with the right HTTP status (404) and the right modeled error shape (ResourceNotFoundException), but with message text the real service never emits. The shared fault is message construction in the connector-profile not-found branch: the emulator has ONE hand-written sentence that it formats slightly wrong and then reuses for an operation that AWS answers with a different sentence entirely. 2 causes.

Cause 1 of 2: DeleteConnectorProfile not-found message drops the colon after "name" and appends a trailing period.
AWS: Smithy: DeleteConnectorProfile (POST /delete-connector-profile) takes connectorProfileName (@required, @length 0..256, @pattern ^[\w/!@#+=.-]+$) and models ResourceNotFoundException with httpError 404, doc "The resource specified in the request (such as the source or destination connector profile) is not found.", carrying a single `message` member. The model does not state the text, so recorded behavior only for the wording: AWS answers 404 with the FIXED text "Connector Profile with name: " + <connectorProfileName> + " does not exist" — i.e. a colon immediately after "name", and NO terminating period. The only VARIABLE part is connectorProfileName, taken verbatim from the request; the probe-generated name in the recording is a per-run value, not part of the template.
Emulator: same 404 and same ResourceNotFoundException shape, but $.message is "Connector Profile with name " + <connectorProfileName> + " does not exist." — the colon after "name" is missing and a '.' is appended.
Why: a client or test that matches AWS's documented not-found sentence (exact equality, or a regex anchored on "with name: <name> does not exist") does not recognise the emulator's error, so a legitimately absent profile reads as an unclassified failure; because the status and error code are correct the divergence is silent until a string comparison runs.
Fix: the appflow DeleteConnectorProfile handler's lookup-miss branch where the ResourceNotFoundException message is built (the not-found message formatter shared by the connector-profile handlers).
Traces: appflow-connector-profile-error-paths#task#2.

Cause 2 of 2: UpdateConnectorProfile reuses the Delete-style sentence instead of AWS's generic not-found sentence.
AWS: Smithy: UpdateConnectorProfile (POST /update-connector-profile) takes connectorProfileName (@required), connectionMode (@required), connectorProfileConfig (@required) and models ResourceNotFoundException (404) among its errors. Recorded behavior only for the wording: when the named profile does not exist AWS answers 404 with the entirely FIXED sentence "The desired resource does not exist." — it interpolates NOTHING from the request (no profile name), so the message has no variable parts at all.
Emulator: 404 with $.message "Connector Profile with name <connectorProfileName> does not exist.", i.e. it formats the (already wrong, see cause 1) connector-profile-specific sentence and interpolates the requested name.
Why: AWS deliberately gives Update the opaque generic sentence while Delete names the resource; a client that distinguishes the two paths on message text, or that assumes no resource identifier leaks from Update's error, sees the wrong sentence and an unexpected echo of the request. Message assertions fail and any log-scrubbing expectation based on AWS's opaque text is violated.
Fix: the appflow UpdateConnectorProfile handler's lookup-miss branch — it must emit the fixed generic sentence rather than delegating to the connector-profile-specific not-found formatter.
Traces: appflow-connector-profile-error-paths#task#3.

Related but NOT covering these: open issue "AppFlow error message text diverges from AWS wording" (20261029T192701Z) covers the CreateConnectorProfile @pattern-anchor rendering and the DescribeFlow not-found sentence; neither of the two causes above is one of its causes.

## Evidence
Cause 1 — fixture appflow-connector-profile-error-paths, step task#2, DeleteConnectorProfile: expected status 404, actual 404 (status and error shape agree). $.message value_mismatch: expected "Connector Profile with name: <connectorProfileName> does not exist", actual "Connector Profile with name <connectorProfileName> does not exist." (missing ':' after "name", extra trailing '.'). Divergence key service=appflow|DeleteConnectorProfile|status=404->404|keys=$.message.

Cause 2 — fixture appflow-connector-profile-error-paths, step task#3, UpdateConnectorProfile: expected status 404, actual 404. $.message value_mismatch: expected "The desired resource does not exist.", actual "Connector Profile with name <connectorProfileName> does not exist." Divergence key service=appflow|UpdateConnectorProfile|status=404->404|keys=$.message.

Model basis: Smithy com.amazonaws.appflow ResourceNotFoundException { message: ErrorMessage } @httpError 404; bound to DeleteConnectorProfile and UpdateConnectorProfile.

## Steps to Reproduce
1. Boot the emulator and replay the wire corpus (replay_wire_corpus).
2. Cause 1: POST /delete-connector-profile with a connectorProfileName that does not exist in the account. Observe 404 ResourceNotFoundException whose message reads "Connector Profile with name <name> does not exist." where AWS sends "Connector Profile with name: <name> does not exist" (colon after "name", no trailing period). Fixture appflow-connector-profile-error-paths, step task#2.
3. Cause 2: POST /update-connector-profile with that same nonexistent connectorProfileName (plus a valid connectionMode and connectorProfileConfig). Observe 404 ResourceNotFoundException whose message is the connector-profile-specific sentence with the name interpolated, where AWS sends exactly "The desired resource does not exist." Fixture appflow-connector-profile-error-paths, step task#3.
