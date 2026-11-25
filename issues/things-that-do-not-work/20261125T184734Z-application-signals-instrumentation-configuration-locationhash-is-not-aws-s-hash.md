Issue-Source: trace-sweep-divergence
Issue-Trace: application-signals-instrumentation-config-errors
Issue-Step: setup#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: application-signals-instrumentation-config-errors application-signals-instrumentation-config-lifecycle
Issue-Shape-Id: com.amazonaws.applicationsignals#ApplicationSignals
Issue-Key: application-signals|CreateInstrumentationConfiguration|2xx->2xx|$.ARN
Issue-Key: application-signals|CreateInstrumentationConfiguration|2xx->2xx|$.LocationHash
Issue-Key: application-signals|DeleteInstrumentationConfiguration|2xx->4xx|$.DeletionStatus
Issue-Key: application-signals|DeleteInstrumentationConfiguration|2xx->4xx|status
Issue-Commit: 104107cc43bbc6c46135da3b49b9e3149b60f2be

# Instrumentation configuration LocationHash is not AWS's hash, and delete by that hash 404s

> _Note: this issue was identified against the code base at commit `104107cc43bbc6c46135da3b49b9e3149b60f2be`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateInstrumentationConfiguration

## Description
Two causes, one shared fault: the emulator derives a dynamic-instrumentation configuration's LocationHash with its own digest instead of the hash AWS computes for the same code location, so the identity it mints (LocationHash and the ARN that embeds it) is not the identity AWS would mint, and a later request that addresses the configuration by that pre-computed hash resolves nothing.

Cause 1 of 2: CreateInstrumentationConfiguration answers the wrong LocationHash (and therefore the wrong ARN).
AWS: Smithy model com.amazonaws.applicationsignals, CreateInstrumentationConfigurationResponse.LocationHash is a required String with @length min 16 max 16, documented as "A stable hash computed from the location that uniquely identifies this instrumentation point within the service, environment, and signal type"; ARN is a required InstrumentationConfigurationArn whose pattern is ^arn:[^:]+:application-signals:[^:]+:[0-9]{12}:instrumentationConfig/.+/[0-9a-f]{16}$ and whose doc spells the format arn:${Partition}:application-signals:${Region}:${Account}:instrumentationConfig/${service}/${environment}/${signalType}/${locationHash}. LocationIdentifier.LocationHash is documented as "The pre-computed location hash (16-character hex string)", so the hash is a VALUE CLIENTS CARRY AND RESEND, not an opaque server id: it is a pure, deterministic function of the CodeLocation members (Language, CodeUnit, ClassName, MethodName, FilePath, LineNumber) and nothing else — no account, region, clock or request id enters it. Recorded behavior: for the fixture's code location AWS returns the 16 hex characters 'fc14dd5a04207b41' as LocationHash, and the same 16 characters as the final ARN segment; both fixtures that create at that location record the same value, which is what makes it a deterministic function of the location rather than a per-run value. HTTP status is 200 in both AWS and the emulator.
Emulator: the response is 200 but LocationHash is a different 16 hex characters for the same code location ('52db650af7aae0bf' in this replay), and the ARN's last segment carries that same wrong hash, so $.LocationHash and $.ARN both diverge while every other member matches. The hash is computed as the first 8 bytes of a SHA-256 over the emulator's own injective field encoding (presence byte, little-endian length, bytes) of the union member name plus the CodeLocation members; that encoding is the emulator's invention, so it cannot agree with AWS's digest for any location.
Why: the hash is part of the API's addressing scheme. A client that stores the LocationHash (or the ARN) from a create and later passes it as LocationIdentifier.LocationHash to Get/Delete/GetStatus, or that computes the hash itself to pre-register an instrumentation point, addresses a configuration the emulator does not hold; a client that compares the returned ARN to one it recorded or constructed, or that uses it as the ResourceArn for TagResource, likewise addresses nothing. Tests and tools pinned to AWS's hash for a known code location fail against the emulator.
Fix: the LocationHash derivation shared by the create, the reads and the delete — state::location_hash / state::stable_hash in the service's instrumentation state module — must reproduce AWS's hash of a CodeLocation; the create handler's ARN minting (state::instrumentation_configuration_arn) then follows automatically because it embeds that hash.
Traces: application-signals-instrumentation-config-errors#setup#1 (representative) and application-signals-instrumentation-config-lifecycle#setup#1, both CreateInstrumentationConfiguration, both 200 with $.LocationHash and $.ARN mismatched.

Cause 2 of 2: DeleteInstrumentationConfiguration answers 404 ResourceNotFoundException for a configuration that exists.
AWS: Smithy model DeleteInstrumentationConfiguration, POST /delete-instrumentation-configuration, success status 200; output DeletionStatus is a required DynamicInstrumentationDeletionStatus whose only value is DELETED ("The value is DELETED after a successful deletion"); ResourceNotFoundException (HTTP 404) is modeled only for an identity the service holds no configuration for. The request names the configuration by InstrumentationType, Service, Environment, SignalType and a LocationIdentifier union that is either the full CodeLocation ("will be hashed internally") or the pre-computed 16-character LocationHash. Recorded behavior: the teardown delete of the configuration created earlier in the same fixture is answered 200 with {"DeletionStatus": "DELETED"}.
Emulator: the same delete is answered HTTP 404 and no DeletionStatus member at all ($.DeletionStatus missing, expected 'DELETED'), i.e. the modeled ResourceNotFoundException for an identity that was successfully created in setup#1 of the same fixture and never deleted in between. Hypothesis: the teardown addresses the configuration by the pre-computed LocationHash the create returned — the AWS hash 'fc14dd5a04207b41' — while the emulator stored the record under its own hash, so the lookup key (signalType/locationHash) never matches; on that reading this cause is closed by Cause 1, but it is listed separately because the emulator's delete also must not report 404 for a configuration it holds, and the replay confirms the delete path independently.
Why: a client cannot tear down what it created. The lifecycle leaks: the resource stays in the store, its tags stay attached, and a retry of the create at that location is answered 409 ConflictException while every delete is answered 404 — a client loops with no way out, and any teardown that treats 404 as fatal fails.
Fix: DeleteInstrumentationConfiguration's handler plus the identifier resolution it shares with the reads (state::identified_location_hash / state::instrumentation_key), so that a LocationIdentifier naming either the full CodeLocation or the hash the create reported resolves the record the create wrote, and a successful removal answers 200 with DeletionStatus DELETED.
Traces: application-signals-instrumentation-config-errors#teardown#5, DeleteInstrumentationConfiguration, expected 200/DeletionStatus=DELETED, got 404.

Note: the third divergence replayed for this service (CreateServiceLevelObjective duplicate-SLO ConflictException message) is already owned by open issue 20261120T083612Z-application-signals-createservicelevelobjective-duplicate-slo-conflictexception-message-does-not-name.md and is not part of this bundle.

## Evidence
Cause 1 — fixture application-signals-instrumentation-config-errors, step setup#1, CreateInstrumentationConfiguration (cluster also covers application-signals-instrumentation-config-lifecycle#setup#1):
  expected status 200, actual status 200
  $.LocationHash: expected 'fc14dd5a04207b41', got '52db650af7aae0bf'
  $.ARN: expected 'arn:aws:application-signals:us-west-2:000000000000:instrumentationConfig/<service>/<environment>/SNAPSHOT/fc14dd5a04207b41', got the same ARN ending in '/52db650af7aae0bf'
  (the ARN's fixed text is arn:aws:application-signals:<region>:<account>:instrumentationConfig/<Service>/<Environment>/<SignalType>/<locationHash>; service, environment, region and account are values from the request/environment, the final segment is the location hash under test)

Cause 2 — fixture application-signals-instrumentation-config-errors, step teardown#5, DeleteInstrumentationConfiguration:
  status: expected 200, got 404
  $.DeletionStatus: expected 'DELETED', got None (no member serialized; the body is the modeled ResourceNotFoundException instead)

Smithy citations: CreateInstrumentationConfigurationResponse.LocationHash (required, @length 16..16, "A stable hash computed from the location"); InstrumentationConfigurationArn pattern ^arn:[^:]+:application-signals:[^:]+:[0-9]{12}:instrumentationConfig/.+/[0-9a-f]{16}$; LocationIdentifier union members CodeLocation ("will be hashed internally") and LocationHash ("The pre-computed location hash (16-character hex string)"); DeleteInstrumentationConfigurationResponse.DeletionStatus required, enum DynamicInstrumentationDeletionStatus = [DELETED]; DeleteInstrumentationConfiguration errors include ResourceNotFoundException (404) only.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus) for trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture application-signals-instrumentation-config-errors step setup#1 (also application-signals-instrumentation-config-lifecycle step setup#1) — POST /create-instrumentation-configuration with InstrumentationType, Service, Environment, SignalType=SNAPSHOT, a Location.CodeLocation and a CaptureConfiguration. Compare $.LocationHash and $.ARN against the recorded response: the emulator returns a different 16-hex-character hash for the same CodeLocation, and the ARN's last segment carries it.
3. Cause 2: replay fixture application-signals-instrumentation-config-errors step teardown#5 — POST /delete-instrumentation-configuration naming the configuration created in setup#1 by its LocationIdentifier. The emulator answers 404 ResourceNotFoundException instead of 200 {"DeletionStatus":"DELETED"}.
4. Both steps pass only when the emulator's location hash matches AWS's for the same CodeLocation and the delete resolves the record the create wrote.
