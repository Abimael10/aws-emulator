Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: mq.create-broker-rejects-password-with-colon-comma-or-equals mq.create-broker-rejects-rabbitmq-password-with-colon-comma-or-equals mq.rabbitmq-running-broker-user-api-refused-state-unchanged mq.native-protocol-stomp-roundtrip-activemq
Issue-Shape-Id: com.amazonaws.mq#mq
Issue-Key: mq.create-broker-rejects-password-with-colon-comma-or-equals|expect_error#4|AssertionError|assert got_message == message, got_message
Issue-Key: mq.create-broker-rejects-rabbitmq-password-with-colon-comma-or-equals|expect_error#4|AssertionError|assert got_message == message, got_message
Issue-Key: mq.rabbitmq-running-broker-user-api-refused-state-unchanged|expect_error#14|AssertionError|raise AssertionError(f"expected ({code}, {message!r}), but the call succeeded: {result!r}")
Issue-Key: mq.native-protocol-stomp-roundtrip-activemq|_stomp_connect#30|ConnectionRefusedError|raw = socket.create_connection((host, port), timeout=timeout_s)
Issue-Commit: 7f36b0d7bb42ff1fae0d29d904c044c9055c6237

# mq: 3 net-new causes — CreateBroker's invalid-password-character refusal uses non-AWS wording, the per-user API is served instead of refused on a RabbitMQ broker, and the advertised ActiveMQ STOMP+SSL endpoint has no listener

> _Note: this issue was identified against the code base at commit `7f36b0d7bb42ff1fae0d29d904c044c9055c6237`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateBroker

## Description
THREE net-new root causes in the mq emulator, grounded in four failing behavioral checks. Number them when closing: Cause 1 of 3, Cause 2 of 3, Cause 3 of 3. All three must be closed before submitting.

Cause 1 of 3 — CreateBroker refuses a user password containing ',', ':' or '=' with the emulator's own message text instead of Amazon MQ's. Failing checks: mq.create-broker-rejects-password-with-colon-comma-or-equals, mq.create-broker-rejects-rabbitmq-password-with-colon-comma-or-equals.
AWS: Smithy (services/service-com-amazonaws-mq-mq/smithy-model/com-amazonaws-mq-mq.json) — CreateBroker (POST /v1/brokers) takes Users (__listOfUser); User.Password is @required and documented "Required. The password of the user. This value must be at least 12 characters long, must contain at least 4 unique characters, and must not contain commas, colons, or equal signs (,:=)." CreateBroker declares BadRequestException (errorKind client, httpError 400). Recorded behavior for the text: a password containing any of ',' ':' '=' is refused with HTTP 400 BadRequestException whose message is the entirely fixed sentence "Make sure your password doesn't contain invalid characters [, :=]." — no part of it is taken from the request: it does not quote the password, the offending character, or the user's index. The refusal happens for both EngineType=ACTIVEMQ and EngineType=RABBITMQ, and no broker is minted (the rejected broker name is absent from ListBrokers).
Emulator: returns BadRequestException (the error code matches) but with the message "users[0]: Password must not contain commas, colons, or equal signs (,:=)." — the emulator's own wording, prefixed with the member path, paraphrasing the model's documentation sentence. Both the ActiveMQ and the RabbitMQ check fail on exactly this message comparison; the positive control (the same password shape without ',:=') is accepted.
Why: a client or operator surfacing or matching the service message sees text no AWS SDK or console would ever produce, so error-handling branches and user-facing messages keyed on AWS's sentence do not fire; a test suite asserting AWS's refusal text passes against AWS and fails against the emulator (and vice versa).
Fix: the password validation / BadRequestException serializer in the mq CreateBroker handler (and the same shared user-password validator used by CreateUser/UpdateUser) — emit the fixed AWS sentence for the invalid-character class, separate from the length class. Note the open issue 20261103T045546Z-mq-refuses-invalid-requests-with-the-wrong-status.md, Cause 8, covers only the password-LENGTH refusal ("Password is invalid. Enter a password 12 to 250 characters long." with errorAttribute users[<i>].password) and does not carry these two check ids or this invalid-character message.
Traces: mq.create-broker-rejects-password-with-colon-comma-or-equals, mq.create-broker-rejects-rabbitmq-password-with-colon-comma-or-equals.

Cause 2 of 3 — on a RabbitMQ broker the per-user API (DescribeUser, UpdateUser, DeleteUser, CreateUser) is served with 200 instead of being refused. Failing check: mq.rabbitmq-running-broker-user-api-refused-state-unchanged.
AWS: Smithy models DescribeUser (GET /v1/brokers/{BrokerId}/users/{Username}), UpdateUser (PUT same URI), DeleteUser (DELETE same URI) and CreateUser (POST same URI); CreateUser and UpdateUser declare ConflictException (errorKind client, httpError 409). Recorded behavior: Amazon MQ's per-user API applies only to the ActiveMQ engine — against a RABBITMQ broker all four operations are refused with HTTP 409 ConflictException and the entirely fixed message "The users API only applies to ActiveMQ brokers." (no part of it is taken from the request), and the broker's Users list from DescribeBroker is unchanged afterwards with no PendingChange. (DescribeUser and DeleteUser do not list ConflictException in the model's error set; the 409 ConflictException is recorded behavior for them.)
Emulator: DescribeUser on a RUNNING RABBITMQ broker returns HTTP 200 with a user body ({'BrokerId': 'b-...', 'ConsoleAccess': False, 'Groups': [], 'Username': '<the admin user>', 'ReplicationUser': False}), so the check never reaches the remaining three calls; the emulator treats the user API as engine-independent.
Why: a client gets the opposite answer from the service — it believes RabbitMQ brokers have a manageable per-user API, so code written against the emulator issues CreateUser/UpdateUser/DeleteUser against RabbitMQ brokers and is refused by AWS with a 409 it has no handling for. Worse, a mutation accepted here (UpdateUser/CreateUser/DeleteUser) changes emulator state AWS would never have changed, diverging the broker's user list from anything real; RabbitMQ users must be managed through the broker's own management interface instead, and nothing steers the client there.
Fix: the engine gate in the mq user operations' handlers (DescribeUser, CreateUser, UpdateUser, DeleteUser) — before any read or mutation, reject when the addressed broker's EngineType is RABBITMQ with ConflictException (409) carrying that fixed message, leaving the broker's Users and pending changes untouched.
Traces: mq.rabbitmq-running-broker-user-api-refused-state-unchanged.

Cause 3 of 3 — a RUNNING ActiveMQ broker advertises a stomp+ssl:// endpoint in BrokerInstances[].Endpoints that accepts no TCP connection, so the broker has no STOMP data plane. Failing check: mq.native-protocol-stomp-roundtrip-activemq.
AWS: Smithy declares DescribeBroker.BrokerInstances (jsonName "brokerInstances") of BrokerInstance, whose Endpoints member is documented as the broker's wire-level protocol endpoints. Recorded behavior only for the wire protocol (the model covers the control plane, not the broker protocol): the advertised stomp+ssl:// URI is a live STOMP-over-TLS listener that answers a CONNECT frame (accept-version:1.1, login/passcode = a broker user) with a CONNECTED frame, accepts SEND to /queue/<name> and delivers a MESSAGE frame with the identical body to a SUBSCRIBE on that destination, then honours DISCONNECT. Host, port-reachability path, queue name and payload are per-run values; the requirement is that whatever address DescribeBroker publishes is an actual STOMP+SSL listener that round-trips a message.
Emulator: the control plane returns 200 for CreateBroker, reports BrokerState RUNNING and publishes a stomp+ssl:// endpoint, but the TCP connect to that host:port is refused outright — ConnectionRefusedError: [Errno 111] Connection refused from socket.create_connection, before any TLS handshake or STOMP frame. The emulator announces an address other than one it can be reached on.
Why: a client that discovers the broker endpoint exactly as every ActiveMQ client library does cannot use it; a refused connection is indistinguishable from a broker outage while the control plane claims a healthy RUNNING broker, so no produce/consume/request-reply flow can be exercised at all.
Fix: the mq broker data plane — the component that must bind the broker's wire listeners when a broker reaches RUNNING and tear them down on DeleteBroker, together with the endpoint publication in the CreateBroker/DescribeBroker response construction so the published URI and the bound listener are the same address. It needs a STOMP 1.1 server over TLS (CONNECT/CONNECTED with login+passcode checked against the broker's users, SEND, SUBSCRIBE with ack:auto, MESSAGE, DISCONNECT) with per-broker queue state. The open issue 20261110T231924Z-mq-the-broker-endpoints-describebroker-now-advertises-have.md describes this same family but the replay report does not mark this check as owned by it (its recorded replay key no longer matches the current failure frame), so the cause is re-filed here and that issue is named rather than duplicated silently; also related and NOT covering it: 20261117T163254Z-mq-describebroker-publishes-broker-instance-addresses-that-do.md (the content of the published address, not whether it is listened on).
Traces: mq.native-protocol-stomp-roundtrip-activemq.

## Evidence
Behavioral suite simparity-behavioral-aws-v1: 22/31 passed (71.0%); service mq 21/26 (81%), 5 failed. Of those five, mq.create-configuration-duplicate-name-conflict-rejected is marked OWNED by 20261030T170907Z-mq-configuration-operations-createconfiguration-wrongly-refuses-a-duplicate.md and is excluded. The four below are marked neither ALREADY FILED nor OWNED.

Cause 1 — mq.create-broker-rejects-password-with-colon-comma-or-equals [mismatch]:
  File "check_mq_create_broker_rejects_password_with_colon_comma_or_equals.py", line 117, in run
    expect_error(create, *_INVALID_PASSWORD)
  line 32, in expect_error: assert got_message == message, got_message
  AssertionError: users[0]: Password must not contain commas, colons, or equal signs (,:=).
Expected (AWS): code BadRequestException (HTTP 400), message "Make sure your password doesn't contain invalid characters [, :=]." — entirely fixed text.
Actual (emulator): code BadRequestException, message "users[0]: Password must not contain commas, colons, or equal signs (,:=)."
Request: CreateBroker(EngineType=ACTIVEMQ, DeploymentMode=SINGLE_INSTANCE, HostInstanceType=mq.t3.micro, PubliclyAccessible=false, one subnet, one security group, Users=[{Username: <generated>, Password: "Sim2Real<bad char>StrongPwd2024"}]) for each bad char in ':', ',', '='.

Cause 1, second example — mq.create-broker-rejects-rabbitmq-password-with-colon-comma-or-equals [mismatch]:
  line 118, in run: expect_error(create, *_INVALID_PASSWORD)
  AssertionError: users[0]: Password must not contain commas, colons, or equal signs (,:=).
Same divergence on the RABBITMQ engine, i.e. the wording is engine-independent.

Cause 2 — mq.rabbitmq-running-broker-user-api-refused-state-unchanged [mismatch]:
  File "check_mq_rabbitmq_running_broker_user_api_refused_state_unchanged.py", line 118, in run
    expect_error(lambda: mq.describe_user(BrokerId=broker_id, Username=admin_user), *_USER_API_ACTIVEMQ_ONLY)
  line 27: raise AssertionError(f"expected ({code}, {message!r}), but the call succeeded: {result!r}")
  AssertionError: expected (ConflictException, 'The users API only applies to ActiveMQ brokers.'), but the call succeeded: {'ResponseMetadata': {'HTTPStatusCode': 200, ...}, 'BrokerId': 'b-...', 'ConsoleAccess': False, 'Groups': [], 'Username': 'sim2realadmin-...', 'ReplicationUser': False}
Expected (AWS): HTTP 409 ConflictException, message "The users API only applies to ActiveMQ brokers." for DescribeUser, UpdateUser, DeleteUser and CreateUser against a RUNNING RABBITMQ broker, with DescribeBroker afterwards reporting the unchanged Users list and no PendingChange.
Actual (emulator): DescribeUser answers HTTP 200 with a user body (the broker id and username in that body are this run's values, not requirements).
Positive control that passed first: CreateBroker(EngineType=RABBITMQ, EngineVersion=4.3, HostInstanceType=mq.m7g.medium, SINGLE_INSTANCE, one admin user) reached BrokerState RUNNING with that one user and no pending change, so the divergence is specific to the per-user API and not a dead broker.

Cause 3 — mq.native-protocol-stomp-roundtrip-activemq [error]:
  File "socket.py", line 865, in create_connection
    raise exceptions[0]
  File "socket.py", line 850, in create_connection
    sock.connect(sa)
  ConnectionRefusedError: [Errno 111] Connection refused
The check creates a configuration, then CreateBroker(EngineType=ACTIVEMQ, Configuration={Id, Revision}, SINGLE_INSTANCE, mq.t3.micro, PubliclyAccessible=false, one private subnet, one security group with ingress on 61614, one user), polls DescribeBroker to RUNNING, asserts BrokerInstances is non-empty and selects the stomp+ssl:// endpoint — all of which succeed — then opens a TLS socket to that endpoint's host:port and sends a STOMP CONNECT. Expected: a CONNECTED frame, then SEND to /queue/<q> and SUBSCRIBE returning a MESSAGE frame whose body equals the sent payload. Actual: the TCP connect is refused; nothing is bound to the advertised port.

## Steps to Reproduce
0. Clone the repo, build and boot the emulator from the checkout, and replay the behavioral suite (simparity-behavioral-aws-v1), service mq.

Cause 1 (password invalid characters):
1. POST /v1/brokers (CreateBroker) with EngineType=ACTIVEMQ, DeploymentMode=SINGLE_INSTANCE, HostInstanceType=mq.t3.micro, PubliclyAccessible=false, one subnet, one security group and Users=[{Username: <any>, Password: a >=12 character password containing a ':' (then repeat with ',' and with '=')}].
2. Expect HTTP 400 BadRequestException with message exactly "Make sure your password doesn't contain invalid characters [, :=]." and no broker minted (the name absent from ListBrokers). Observed: the same code with message "users[0]: Password must not contain commas, colons, or equal signs (,:=)."
3. Repeat with EngineType=RABBITMQ (EngineVersion from DescribeBrokerEngineTypes, HostInstanceType=mq.m7g.medium) — same expectation, same divergence.
4. Positive control: the same password with ',:=' removed is accepted and the broker reaches RUNNING with that one user.

Cause 2 (RabbitMQ per-user API):
1. CreateBroker with EngineType=RABBITMQ, EngineVersion=4.3, DeploymentMode=SINGLE_INSTANCE, HostInstanceType=mq.m7g.medium, PubliclyAccessible=false, one subnet, one security group, Users=[{Username: admin, Password: <valid>}]; poll GET /v1/brokers/{BrokerId} until brokerState is RUNNING.
2. GET /v1/brokers/{BrokerId}/users/{Username} (DescribeUser). Expect HTTP 409 ConflictException "The users API only applies to ActiveMQ brokers." Observed HTTP 200 with a user body.
3. Repeat with PUT (UpdateUser, e.g. ConsoleAccess=true), DELETE (DeleteUser) and POST (CreateUser with a second username) on the same URI — each must be refused identically.
4. GET /v1/brokers/{BrokerId} (DescribeBroker): brokerState still RUNNING, Users identical to before step 2, and no user carrying PendingChange.

Cause 3 (ActiveMQ STOMP data plane):
1. CreateBroker with EngineType=ACTIVEMQ, DeploymentMode=SINGLE_INSTANCE, HostInstanceType=mq.t3.micro, PubliclyAccessible=false, one private subnet, a security group allowing ingress on 61614, and one user; poll DescribeBroker until brokerState is RUNNING.
2. Read brokerInstances[0].endpoints and pick the stomp+ssl:// URI; open a TCP connection to its host and port and wrap it in TLS. Observed: the connect fails with ECONNREFUSED.
3. With a listener present, send STOMP CONNECT (accept-version:1.1, login/passcode = the created user) and expect a CONNECTED frame; then SEND a body to /queue/<name>, SUBSCRIBE to the same destination with ack:auto and expect a MESSAGE frame carrying that exact body; finish with DISCONNECT.
