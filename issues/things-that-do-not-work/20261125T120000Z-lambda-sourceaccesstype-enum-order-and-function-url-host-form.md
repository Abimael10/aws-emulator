Issue-Source: issue-fixer-residual
Issue-Trace: lambda-rmq-traj-mapping-request-shape-mistakes
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lambda-rmq-traj-mapping-request-shape-mistakes lambda-function-url-config-lifecycle
Issue-Shape-Id: com.amazonaws.lambda#AWSGirApiService
Issue-Key: lambda|CreateEventSourceMapping|4xx->4xx|$.message
Issue-Key: lambda|CreateFunctionUrlConfig|2xx->2xx|$.FunctionUrl
Issue-Related: 20261124T024125Z-lambda-response-fidelity-wrong-esm-starting-position-refusal.md

# lambda: SourceAccessType's enum value set is listed in the model's order, and FunctionUrl cannot carry AWS's host form while staying an address this emulator answers

## Operation
CreateEventSourceMapping

## Description
The two causes left over from
`issues/things-that-now-work/20261124T024125Z-lambda-response-fidelity-wrong-esm-starting-position-refusal.md`,
filed here so the divergences they carry are not lost with that report. That issue's own
three numbered causes are settled: the `StartingPosition` refusal text and the Python
`stackTrace` frames are fixed, and the fixture steps it named for them replay clean. What
remains is one cause that needs a recording this report does not carry, and one that the
emulator's own addressing rule contradicts.

=== Cause 1 of 2: the `SourceAccessType` value set is listed in the model's declaration order ===
Fixture: lambda-rmq-traj-mapping-request-shape-mistakes, step task#2. It shares the
divergence key `lambda|CreateEventSourceMapping|4xx->4xx|$.message` with the
starting-position refusal the related issue fixed, which is why that issue described this
step as the same text; it is not.
AWS: the recorded 400 is the request-validation envelope over
`sourceAccessConfigurations.<i>.member.type` — `1 validation error detected: Value
'<value>' at 'sourceAccessConfigurations.1.member.type' failed to satisfy constraint:
Member must satisfy enum value set: [...]` — and the bracketed set opens with
`CLIENT_CERTIFICATE_TLS_AUTH`. The model declares `SourceAccessType` with 15 values
(`smithy_query(query="shape", shape="SourceAccessType")`: BASIC_AUTH, VPC_SUBNET,
VPC_SECURITY_GROUP, SASL_SCRAM_512_AUTH, SASL_SCRAM_256_AUTH, VIRTUAL_HOST,
CLIENT_CERTIFICATE_TLS_AUTH, SERVER_ROOT_CA_CERTIFICATE, OAUTHBEARER_AUTH,
OAUTHBEARER_SCOPE, OAUTHBEARER_AUDIENCE, OAUTHBEARER_LOGICAL_CLUSTER,
OAUTHBEARER_IDENTITY_POOL, IAM_AUTH, IAM_OAUTHBEARER_AUTH) and records no order for the
sentence, so AWS's order is recorded behavior per enum.
Emulator: the clause lists the set in the model's declaration order, starting
`[BASIC_AUTH, VPC_SU…`. Status (400) and error class (`ValidationException`) match; only
the order of the set diverges.
Why: a client that compares the whole refusal text — an SDK assertion, a log scraper —
does not recognise it, and the sentence is the one thing such a caller has.
Fix: add one row to `REPORTED_ENUM_ORDERS` in
`services/service-com-amazonaws-lambda-awsgirapiservice/src/service.rs`, which is where
this service already states AWS's order for the four enums whose order was recorded
(`UpdateRuntimeOn`, `InvocationType`, `EventSourcePosition`,
`KafkaSchemaRegistryAuthType`); the row is matched by value SET, so one row covers every
member targeting `SourceAccessType`.
What the fixer needs that this report does not carry: the WHOLE recorded order. The replay
withholds the recorded message past its first value, and the order is not computable — the
Java `HashMap`/`HashSet` iteration order of the four already-recorded value sets
reproduces none of them — so the remaining fourteen values have to come from an AWS answer
rather than from a guess.

=== Cause 2 of 2: FunctionUrl is announced on this emulator's own endpoint, not on AWS's url-id host ===
Fixture: lambda-function-url-config-lifecycle, step task#8 (CreateFunctionUrlConfig, 201 vs
201, `$.FunctionUrl`).
AWS: `CreateFunctionUrlConfig` answers 201 with `FunctionUrl` @required, @length min 40 max
100, documented as "The HTTP URL endpoint for your function"
(`smithy_query(query="operation", operation="CreateFunctionUrlConfig")`). AWS serves that
endpoint at `https://<url-id>.lambda-url.<region>.on.aws/`, the url id being the first DNS
label; the model states no host form.
Emulator: `https://<host>:<port>/_aws/lambda-url/<url-id>/` — the url id behind the
reserved data-plane path on the listener opened for that URL
(`state::function_url::announced_function_url`), which is an address this emulator answers
on and which `tests/all/function_url_announced_endpoint.rs` fetches end to end.
Why this is still open rather than fixed: the two requirements exclude each other as the
emulator stands. Announcing the documented form was tried and replayed: `$.FunctionUrl`
still diverged at this step and `GetFunctionUrlConfig`,
`ListFunctionUrlConfigs` and `UpdateFunctionUrlConfig` began diverging with it, which is
the verdict `issues/things-that-now-work/20261109T152638Z-lambda-function-url-payloads-advertise-a-lambda-url.md`
recorded for this very step when the value WAS the documented one (an announced host the
emulator does not answer on). Nothing resolves `*.on.aws` on the machine the emulator runs
on, and the core vends loopback listeners only (`ResourceListener::authority`), so a value
carrying the url id as a leading host label is an endpoint no client can reach.
Fix: a core-side way to serve a NAME — a resolver or hosts entry the emulator's own clients
honour for the zone a resource publishes in — would let this member carry the documented
form and stay reachable. Until then the choice is between the recorded shape and an
endpoint that answers, and the emulator keeps the one that answers.

## Evidence
Cause 1 — CreateEventSourceMapping, expected 400 / actual 400, key `$.message`:
  expected: `1 validation error detected: Value 'basic_auth' at
  'sourceAccessConfigurations.1.member.type' failed to satisfy constraint: Member must
  satisfy enum value set: [CLIENT_CERTIFICA…` (the replay truncates the recorded value
  there)
  actual:   `… Member must satisfy enum value set: [BASIC_AUTH, VPC_SU…`
Cause 2 — CreateFunctionUrlConfig, expected 201 / actual 201, key `$.FunctionUrl`:
  expected: `<shaped-not-asserted>` (recorded shape: `https://<url-id>.lambda-url.<region>.on.aws/`)
  actual:   `https://<host>:<port>/_aws/lambda-url/<32-hex-id>/`

## Steps to Reproduce
Cause 1: replay lambda-rmq-traj-mapping-request-shape-mistakes and read task#2's 400 body —
a `CreateEventSourceMapping` whose `SourceAccessConfigurations[1].Type` is outside the
enum's value set; compare the bracketed set's order.
Cause 2: replay lambda-function-url-config-lifecycle and read task#8's 201 body —
`CreateFunctionUrlConfig` with `AuthType` set; compare `$.FunctionUrl`'s form, then issue a
GET to the value and see which of the two forms a client on this machine can reach.
