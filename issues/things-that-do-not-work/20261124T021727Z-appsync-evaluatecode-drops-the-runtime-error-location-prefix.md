Issue-Source: trace-sweep-divergence
Issue-Trace: appsync-eval-js-error-surface
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: appsync-eval-js-error-surface
Issue-Shape-Id: com.amazonaws.appsync#AWSDeepdishControlPlaneService
Issue-Key: appsync|EvaluateCode|2xx->2xx|$.error.message
Issue-Commit: 5abb52f22cc381e1bc2d964ae40a640a2e0d853b

# AppSync EvaluateCode drops the runtime-error location prefix and Uncaught error class from error.message

> _Note: this issue was identified against the code base at commit `5abb52f22cc381e1bc2d964ae40a640a2e0d853b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
EvaluateCode

## Description
One net-new appsync wire divergence on the APPSYNC_JS code-evaluation surface. The emulator returns the right status (HTTP 200) and the right envelope shape, but the text it puts in the runtime-error detail is a bare lowercased engine string instead of AWS's located, classified diagnostic. This issue bundles 1 cause; it must be closed.

=== Cause 1 of 1: EvaluateCode's runtime-error message omits the "code.js:<line>:<column>: Uncaught <ErrorClass>: " prefix and lowercases the engine message (fixture appsync-eval-js-error-surface, step task#2) ===
AWS: Smithy: EvaluateCode, POST /v1/dataplane-evaluatecode, http code 200; input runtime (AppSyncRuntime, required), code (Code, required, @length 1..32768), context (Context, required, @length 2..28000), function (String); output EvaluateCodeResponse{evaluationResult, error: EvaluateCodeErrorDetail, logs, stash, outErrors}; EvaluateCodeErrorDetail$message targets ErrorMessage and is documented only as "The error payload"; modeled errors AccessDeniedException(403), BadRequestException(400), InternalFailureException(500). The model does not state the message text, so for the wording this is recorded behavior only: code that COMPILES but throws at evaluation time is answered with HTTP 200 and `error.message` set to the thrown JavaScript error rendered as "code.js:<line>:<column>: Uncaught <ErrorClass>: <engine message>". The FIXED text of that template is the "code.js:" source-name prefix, the ":" separators, the literal word "Uncaught", the ": " after the error class, and the engine's own message text with its original capitalization (for a property read on undefined, the V8 wording "Cannot read property '<name>' of undefined", capital C). The VARIABLE parts are the line and column, which come from the position inside the `code` submitted in the request, the error class name (TypeError, ReferenceError, RangeError, ...) and the property/identifier name, which come from the submitted code and the evaluated `context`. Note this is the RUNTIME-throw envelope, distinct from the compile/parse envelope (error.codeErrors[*] with the "code.js(<line>,<column>): error TS<n>: ..." diagnostic form), which is a different response key.
Emulator: HTTP 200 with the same envelope, but error.message is only the engine phrase, lowercased and unlocated: "cannot read property 'b' of undefined" where AWS answers "code.js:2:10: Uncaught TypeError: Cannot read property 'b' of undefined". The "code.js:<line>:<column>: " prefix is absent, the "Uncaught " literal and the error-class name (TypeError) are absent, and the leading character of the engine message is lower-cased. Status and every other asserted member of the response match.
Why: EvaluateCode is the documented pre-flight for APPSYNC_JS resolver code, and error.message is the only place a runtime throw is reported — there is no codeErrors entry for a throw. A developer tool or CI check that surfaces this string shows no file position, so the user cannot tell WHERE in their resolver the throw happened; code that classifies the failure by parsing the error class out of the message (matching "Uncaught TypeError" / "Uncaught ReferenceError") sees no class at all and falls through to its unknown-error branch; and a case-sensitive assertion or log-grep on the engine wording ("Cannot read property") never matches. The failure is silent — the call succeeds with 200, so nothing signals that the diagnostic is degraded.
Fix: the EvaluateCode handler's APPSYNC_JS evaluation stage and its error projection — when the submitted code throws during evaluation, the caught exception must be rendered into EvaluateCodeErrorDetail$message using the located template above (source name "code.js", the throw's line/column, the literal "Uncaught", the exception's class name, then the engine message verbatim and un-case-folded) rather than passing the engine's bare message string through.
Traces: appsync-eval-js-error-surface#task#2.

Related open issues (none owns this cluster; the replay reported it with an empty owned_by and an empty stub_path): 20261122T111346Z-appsync-response-document-fidelity-getintrospectionschema-omits-schema-directives.md bundles the EvaluateCode COMPILE/parse diagnostic wording (error.message wrapping a codeErrors[*].value TS diagnostic, fixture appsync-unit-resolver-code-evaluation#task#4) — a different fixture, a different response key and a different error class (parse, not runtime throw). 20261111T092526Z-appsync-evaluatecode-diverges-across-the-board-context-not.md describes the broader EvaluateCode family. Neither carries this cluster's replay key, so this cause is filed here.

## Evidence
Mismatch cluster from trace set simparity-wire-aws-v1, service appsync, expected status 200 / actual status 200, affected_count 1, stub_path empty, owned_by empty.

Divergence key: service=appsync|EvaluateCode|status=200->200|keys=$.error.message
Detail: "$.error.message: value_mismatch (expected \"code.js:2:10: Uncaught TypeError: Cannot read property 'b' of undefined\", got \"cannot read property 'b' of undefined\")"

Quoting the divergence exactly: status 200 (matches, not a defect); no error code is involved (this is a 200-with-error-payload envelope, not a modeled exception); error message expected "code.js:2:10: Uncaught TypeError: Cannot read property 'b' of undefined", got "cannot read property 'b' of undefined".

Which parts of that recorded message are fixed text and which are values: FIXED — "code.js:", the ":" separators, "Uncaught ", the ": " after the class name, and the engine wording "Cannot read property '...' of undefined" with its original capitalization. FROM THE REQUEST/STATE — the line 2 and column 10 (the throw site inside the submitted `code`), the class name TypeError, and the property name 'b' (from the submitted code and the evaluated `context`). The literals 2, 10 and 'b' are this recording filling the template; they are not part of the requirement.

## Steps to Reproduce
Boot the emulator from this checkout and replay the wire corpus (trace set simparity-wire-aws-v1). This issue has one cause, which is one recorded step.

Cause 1: replay appsync-eval-js-error-surface step task#2 — call appsync.evaluate_code(runtime={'name':'APPSYNC_JS','runtimeVersion':'1.0.0'}, code=<APPSYNC_JS source that COMPILES but dereferences a property of an undefined value at evaluation time, e.g. reading `.b` off an absent object on line 2>, context=<the fixture's context JSON>, function='request'). Expect HTTP 200 with error.message in the form "code.js:<line>:<column>: Uncaught <ErrorClass>: <engine message>" (for this fixture's code, "code.js:2:10: Uncaught TypeError: Cannot read property 'b' of undefined"); observe HTTP 200 with error.message "cannot read property 'b' of undefined" — no source/line/column prefix, no "Uncaught", no error-class name, and the engine message lower-cased.
