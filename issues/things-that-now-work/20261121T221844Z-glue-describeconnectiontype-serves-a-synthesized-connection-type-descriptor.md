Issue-Source: trace-sweep-divergence
Issue-Trace: glue-connection-type-catalog
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-connection-type-catalog
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|DescribeConnectionType|2xx->2xx|$.AuthenticationConfiguration
Issue-Key: glue|DescribeConnectionType|2xx->2xx|$.Capabilities
Issue-Key: glue|DescribeConnectionType|2xx->2xx|$.Categories
Issue-Key: glue|DescribeConnectionType|2xx->2xx|$.ConnectionOptions
Issue-Key: glue|DescribeConnectionType|2xx->2xx|$.ConnectionProperties
Issue-Key: glue|DescribeConnectionType|2xx->2xx|$.Description
Issue-Key: glue|DescribeConnectionType|2xx->2xx|$.DisplayName
Issue-Key: glue|DescribeConnectionType|2xx->2xx|$.LogoUrl
Issue-Key: glue|DescribeConnectionType|2xx->2xx|$.PhysicalConnectionRequirements
Issue-Key: glue|DescribeConnectionType|2xx->2xx|$.Vendor
Issue-Commit: 1b2af264a6420d6af5cb343754deb67638848237

# Glue DescribeConnectionType serves a synthesized connection-type descriptor instead of the service's catalog

> _Note: this issue was identified against the code base at commit `1b2af264a6420d6af5cb343754deb67638848237`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeConnectionType

## Description
One cause (Cause 1 of 1).

Cause 1 of 1: DescribeConnectionType answers 200 but its body is not the connection-type catalog entry AWS publishes for the requested ConnectionType — ten top-level members of the response diverge at once.

AWS: Smithy (services/service-com-amazonaws-glue-awsglue): DescribeConnectionType takes one required input member ConnectionType (target NameString, @length 1..255, pattern ^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$) and returns ConnectionType, Description, Capabilities, ConnectionProperties (PropertiesMap), ConnectionOptions (PropertiesMap), AuthenticationConfiguration (AuthConfiguration), ComputeEnvironmentConfigurations, PhysicalConnectionRequirements, AthenaConnectionProperties, PythonConnectionProperties, SparkConnectionProperties and RestConfiguration; modeled errors are AccessDeniedException, InvalidInputException, ValidationException (400) and InternalServiceException (500). The response is a READ of a fixed, service-owned catalog: for each supported connection type AWS returns that type's display metadata and, for every connection/authentication property, a descriptor carrying Name, DisplayName, Description, DataType, PropertyType/PropertyTypes, Phase (CONNECTION_CREATION vs AUTHENTICATION), Required/ConditionallyRequired, DefaultValue, AllowedValues and Validations, plus the AuthConfiguration listing the authentication types the type supports (e.g. the AuthenticationType AllowedValues set) and, for OAUTH2, the OAuth2Properties descriptors (authorization-code URL, authorization code, scope, user-managed client application, ...). Recorded behavior only (the on-disk model is older than the recorded API): AWS additionally returns the vendor-facing members DisplayName, Vendor, Categories and LogoUrl for a vendor connection type. The catalog values are properties of the connection type itself, not of the account, the region or the run, so they are the same on every call.

Emulator: for the same request it answers 200 with a body whose AuthenticationConfiguration, Capabilities, Categories, ConnectionOptions, ConnectionProperties, Description, DisplayName, LogoUrl, PhysicalConnectionRequirements and Vendor all diverge from the catalog entry (the replay reports '$.AuthenticationConfiguration: value_mismatch' and the same verdict across the other nine keys; divergence_key service=glue|DescribeConnectionType|status=200->200|keys=$.AuthenticationConfiguration,$.Capabilities,$.Categories,$.ConnectionOptions,$.ConnectionProperties,$.Description,$.DisplayName,$.LogoUrl,$.PhysicalConnectionRequirements,$.Vendor). In other words the handler returns a generic/synthesized descriptor rather than the per-connection-type catalog, so the authentication types, the OAuth2 property descriptors and the property phases/requiredness it advertises are not the ones the connection type actually has.

Why: DescribeConnectionType is the discovery call a client (and the console) makes before CreateConnection: it drives which authentication type to pick, which ConnectionProperties/ConnectionOptions keys are legal, which are required at CONNECTION_CREATION vs AUTHENTICATION, and what the OAuth2 authorization-code URL and scope defaults are. Against the emulator a client building a connection from the described contract selects properties the type does not accept, misses conditionally-required OAuth2 members, and cannot render the type's display metadata (DisplayName/Vendor/Categories/LogoUrl) at all — it either builds an unusable connection or diverges from the code path it would take on AWS.

Fix: the DescribeConnectionType handler and the static connection-type catalog data it reads in the glue service crate — the handler must look the requested ConnectionType up in a per-type catalog carrying that type's display metadata, its ConnectionProperties/ConnectionOptions/PhysicalConnectionRequirements property descriptors, its Capabilities and its AuthConfiguration (including OAuth2Properties descriptors), instead of serving one synthesized shape for every type. No open issue covers this operation (the open glue connection-type issues are the RegisterConnectionType and DeleteConnectionType scaffold stubs, 20261117T183344Z-glue-registerconnectiontype-stub.md and 20261121T204301Z-glue-deleteconnectiontype-stub.md, which are unrelated unimplemented-operation stubs).

Traces: glue-connection-type-catalog#task#2 (expected 200, got 200; ten response keys diverge).

## Evidence
Fixture glue-connection-type-catalog, step task#2, operation DescribeConnectionType, verdict mismatch.
Expected status 200, actual status 200.
Replay detail (truncated by the harness): "$.AuthenticationConfiguration: value_mismatch (expected {'__type': 'AuthConfiguration', 'AuthenticationType': {'AllowedValues': [{'Description': 'OAUTH2 Authentication', 'DisplayName': 'OAUTH2', 'Value': 'OAUTH2'}], 'DataType': 'STRING', 'Description': 'Authentication type to connect to data source.', 'DisplayName': 'Authentication Type', 'Name': 'AuthenticationType', 'Phase': 'CONNECTION_CREATION', 'PropertyType': 'USER_INPUT', 'PropertyTypes': ['USER_INPUT'], 'Required': True, 'Validations': []}, 'OAuth2Properties': {'AuthorizationCodeUrl': {... 'PropertyType': 'READ_ONLY', 'Phase': 'AUTHENTICATION', ...}, 'AuthorizationCode': {'ConditionallyRequired': [{'OAuth2GrantType': 'AUTHORIZATION_CODE', ...}], ...}, 'Scope': {...}, 'USER_MANAGED_CLIENT_APPL..."
Diverging keys (all at status 200 -> 200): $.AuthenticationConfiguration, $.Capabilities, $.Categories, $.ConnectionOptions, $.ConnectionProperties, $.Description, $.DisplayName, $.LogoUrl, $.PhysicalConnectionRequirements, $.Vendor.
Note on values: the concrete vendor strings and URLs in the recording (an Adobe Analytics IMS authorize URL, that vendor's default scope list) are the catalog content of the connection type asked about in this fixture, not per-run values; the requirement is that each member be the catalog entry for the requested ConnectionType, with the fixed descriptor fields (Name, DisplayName, Description, DataType, PropertyType/PropertyTypes, Phase, Required/ConditionallyRequired, DefaultValue, AllowedValues, Validations) populated per property. No error code or error message is involved: both sides answer 200.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay fixture glue-connection-type-catalog.
2. At step task#2 the fixture calls glue DescribeConnectionType with the fixture's ConnectionType (a vendor OAUTH2-capable type).
3. Compare the 200 response body against the recorded one: AuthenticationConfiguration, Capabilities, Categories, ConnectionOptions, ConnectionProperties, Description, DisplayName, LogoUrl, PhysicalConnectionRequirements and Vendor all differ from the catalog entry, whereas AWS returns that connection type's catalog metadata (authentication types, OAuth2 property descriptors with their Phase/Required/DefaultValue/AllowedValues, and the type's display metadata).
4. Re-replay glue-connection-type-catalog#task#2 after the fix to confirm all ten members match.
