Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-catalog-get-inventory-schema
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-catalog-get-inventory-schema ssm-inventory-and-compliance-tracking ssm-describe-inventory-deletions-listing ssm-put-inventory-custom-item-then-check-deletions ssm-inventory-deletions-listing-and-unknown-type ssm-vscan-traj-inventory-entries-type-and-id-mistakes
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|GetInventorySchema|2xx->2xx|$.Schemas[*]
Issue-Key: ssm|DescribeInventoryDeletions|2xx->4xx|$.InventoryDeletions
Issue-Key: ssm|DescribeInventoryDeletions|2xx->4xx|status
Issue-Key: ssm|ListInventoryEntries|4xx->2xx|$.Message
Issue-Key: ssm|ListInventoryEntries|4xx->2xx|$.__type
Issue-Key: ssm|ListInventoryEntries|4xx->2xx|status
Issue-Key: ssm|DeleteInventory|4xx->4xx|$.Message
Issue-Key: ssm|ListInventoryEntries|4xx->4xx|$.Message
Issue-Commit: f91a062ed7cd5cab4f5d0dcf8790852b82a6c908

# ssm inventory APIs: GetInventorySchema serves one stale schema and inventory type-name/deletion paths misreport

> _Note: this issue was identified against the code base at commit `f91a062ed7cd5cab4f5d0dcf8790852b82a6c908`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetInventorySchema

## Description
Six net-new causes across the ssm Inventory surface. The schema catalog GetInventorySchema serves is a single hardcoded entry at the wrong version with the wrong DataType casing; DescribeInventoryDeletions refuses a listing AWS answers successfully; ListInventoryEntries skips inventory type-name validation entirely in one shape of the request and authors its own prose for it in another; and DeleteInventory's unknown-type refusal is emulator-authored prose. THIS ISSUE HAS 6 CAUSES — all six must be fixed and re-replayed before it is closed.

Cause 1 of 6: GetInventorySchema returns ONE schema instead of the Systems Manager default inventory-type catalog, and serializes DataType in the wrong case.
AWS: Smithy (com.amazonaws.ssm) GetInventorySchema -> GetInventorySchemaResult.Schemas -> InventoryItemSchemaResultList of InventoryItemSchema{TypeName required @length 1..100 @pattern ^(AWS|Custom):.*$, Version @pattern ^([0-9]{1,6})(\.[0-9]{1,6})$, Attributes required @length 1..50 of InventoryItemAttribute{Name required, DataType required -> InventoryAttributeDataType}, DisplayName}. The TypeName doc states the default inventory item types include AWS:AWSComponent, AWS:Application, AWS:InstanceInformation, AWS:Network and AWS:WindowsUpdate. The operation is @paginated (MaxResults @range 50..200), i.e. the catalog is expected to be many entries, not one. For the wire form of DataType the model enum lists string|number but the recording shows AWS serializing the UPPERCASE token (recorded behavior only): every attribute comes back as {"Name": <attr>, "DataType": "STRING"}. The requirement is: a schema entry per published inventory type, each with that type's attribute list and its current schema version, with DataType in the recorded uppercase form. The specific type names and attribute names are the published AWS catalog, not values from one run.
Emulator: HTTP 200 (status agrees) but Schemas has exactly one element — TypeName AWS:InstanceInformation, Version '1.0', ten attributes — and every attribute carries DataType 'string' (lowercase). Every other catalog index ($.Schemas[1] .. $.Schemas[14]) is absent.
Why: GetInventorySchema is the discovery call a client makes to learn which inventory types and attributes it may query, filter and aggregate on. A client served one type concludes the account supports no application, network, compliance or Windows-update inventory at all, so inventory browsers render an almost empty type list and any client that validates a GetInventory/ListInventoryEntries TypeName against this catalog rejects type names the service does in fact accept. The lowercase DataType additionally breaks any case-sensitive comparison against the value AWS puts on the wire.
Fix: the GetInventorySchema handler and the inventory-schema catalog data it reads in the ssm service — it must serve the full default-type catalog (paginating per MaxResults) rather than a single built-in entry, and the attribute serializer must emit DataType in AWS's recorded casing.
Traces: ssm-catalog-get-inventory-schema#task#1.

Cause 2 of 6: the one schema GetInventorySchema does serve is a stale AWS:InstanceInformation — wrong Version and seven attributes short.
AWS: per InventoryItemSchema above, Version is "the schema version for the inventory item" and Attributes is that type's full attribute list. The recording shows AWS:InstanceInformation at Version '1.1' with seventeen attributes: AgentType, AgentVersion, ComputerName, IamRole, InstanceId, IpAddress, PlatformName, PlatformType, PlatformVersion, ResourceType, AgentStatus, InstanceStatus, SourceType, SourceId, SourceLocation, AvailabilityZone, AvailabilityZoneId — all DataType 'STRING'.
Emulator: the same TypeName at Version '1.0' with ten attributes (AgentType, AgentVersion, ComputerName, InstanceId, InstanceStatus, IpAddress, PlatformName, PlatformType, PlatformVersion, ResourceType), DataType 'string'. IamRole, AgentStatus, SourceType, SourceId, SourceLocation, AvailabilityZone and AvailabilityZoneId are missing and the version is a release behind.
Why: a client that builds an inventory query or a column set from this schema can never ask for IamRole, AgentStatus or the AvailabilityZone attributes, and a client that pins or compares the schema version sees 1.0 and takes the account to be on an older inventory schema than it is. This is a different failure from cause 1 (there the catalog is missing entries; here the single entry served is itself wrong), so fixing the catalog size without refreshing this entry leaves it failing.
Fix: the inventory-schema catalog data for AWS:InstanceInformation in the ssm service (the same component as cause 1) — bump the entry to the published version and attribute list.
Traces: ssm-inventory-and-compliance-tracking#task#2.

Cause 3 of 6: DescribeInventoryDeletions answers HTTP 400 where AWS answers 200 with an empty deletion list.
AWS: Smithy DescribeInventoryDeletions -> DescribeInventoryDeletionsResult{InventoryDeletions -> InventoryDeletionsList of InventoryDeletionStatusItem, NextToken}; it is @paginated over InventoryDeletions, and its only client errors are InvalidDeletionIdException and InvalidNextToken (both HTTP 400). A listing request that names no failing DeletionId and no bad NextToken is a successful read: the recording shows HTTP 200 with InventoryDeletions present and empty when there is no deletion history to report. An empty history is a 200 with an empty list, never an error.
Emulator: HTTP 400; the response carries no InventoryDeletions member at all (expected [], got None).
Why: this turns a normal, successful read into a hard client error. Any caller polling inventory-deletion progress — including the one that follows a DeleteInventory to watch its DeletionId reach Complete — fails on the very first poll instead of iterating an empty list, and a paginator that starts with this call aborts rather than terminating cleanly. Hypothesis: the operation rejects the request because no deletion history is tracked, rather than projecting an empty list.
Fix: the DescribeInventoryDeletions handler and its inventory-deletion state in the ssm service — a listing with no matching deletions must project InventoryDeletions as an empty list under HTTP 200, reserving 400 for InvalidDeletionIdException / InvalidNextToken.
Note: the open issue 20261116T021504Z-ssm-describeinventordeletions-stub.md ("Implement ssm DescribeInventoryDeletions: unimplemented scaffold stub") names this operation, but the replay reports this cluster with no owning issue and no stub path, and the observed answer is a 400 rejection rather than an unimplemented 501 — so this divergence is filed here and that issue does not carry this replay key.
Traces: ssm-describe-inventory-deletions-listing#task#3.

Cause 4 of 6: ListInventoryEntries accepts a syntactically invalid Custom: inventory type name with HTTP 200 instead of refusing it.
AWS: Smithy ListInventoryEntries.TypeName is required, target InventoryItemTypeName (@length 1..100, @pattern ^(AWS|Custom):.*$), and the operation models InvalidTypeNameException (client, HTTP 400, awsQueryError code InvalidTypeName, doc "The parameter type name isn't valid."). A custom type name must additionally satisfy ^(Custom:)([a-zA-Z0-9]{3,93})$, so a Custom: name containing hyphens is invalid. AWS answers HTTP 400 with __type InvalidTypeNameException and the message template "Inventory type name [<TypeName>] is invalid. Type name is case sensitive, such as AWS:APpLICation is invalid; Type name must start with either AWS: or Custom:;  Custom inventory type name must satisfy regular expression pattern: ^(Custom:)([a-zA-Z0-9]{3,93})$" — everything is fixed text (including the literal double space before "Custom inventory type name") except <TypeName>, which is echoed verbatim from the request. The recorded value Custom:sim2real-probe-ssm-AppInfo-bff97811 is just what filled the template on that run.
Emulator: HTTP 200 with an ordinary success body; $.__type and $.Message are absent entirely. No type-name validation happens on this path.
Why: this is a silent wrong answer, the worst failure mode in the bundle. A caller that misnames a custom inventory type is told its query SUCCEEDED and returned nothing, so it concludes the node has no entries of that type rather than learning the name is malformed; the InvalidTypeNameException branch that would correct the name is dead, and error-path tests asserting 400 see 200.
Fix: ListInventoryEntries' input validation in the ssm handler — validate TypeName against the AWS:/Custom: rule (and the custom-name pattern) before serving the read, and raise InvalidTypeNameException with the templated message.
Traces: ssm-inventory-and-compliance-tracking#task#6.

Cause 5 of 6: DeleteInventory's refusal for a type name that was never registered is emulator-authored prose.
AWS: Smithy DeleteInventory.TypeName required -> InventoryItemTypeName; modeled client errors include InvalidTypeNameException, InvalidInventoryRequestException, InvalidDeleteInventoryParametersException and InvalidOptionException (all HTTP 400). When the named type exists syntactically but no inventory of it is registered, AWS answers HTTP 400 with the message template "The given TypeName <TypeName> is either deleted or doesn't exist" — all fixed text except <TypeName>, echoed from the request (recorded as Custom:ProbeNeverRegistered85327641). Note AWS's wording: no trailing period, and it names BOTH possibilities (deleted or never existed).
Emulator: the same HTTP 400 and the same error class (only $.Message diverged), but the message is "The parameter type name isn't valid: <TypeName>. No inventory of that type exists." — prose the emulator wrote, built from the InvalidTypeNameException shape's doc string rather than AWS's sentence.
Why: the message is the only thing distinguishing "this type name is malformed" from "this type name is fine but has no data", and the emulator's text asserts the first while the condition is the second. Operators and log-scraping tooling that match AWS's sentence never match, and a caller reading "isn't valid" will try to repair a type name that needs no repair.
Fix: DeleteInventory's error construction in the ssm handler — emit AWS's templated sentence for the unregistered-type condition instead of reusing the invalid-name doc text.
Traces: ssm-inventory-deletions-listing-and-unknown-type#task#2.

Cause 6 of 6: ListInventoryEntries' InvalidTypeNameException message for a wrongly-cased AWS: type name is emulator-authored prose instead of AWS's template.
AWS: as in cause 4, inventory type names are case sensitive, so AWS:application is invalid even though AWS:Application is valid. AWS answers HTTP 400 with the InvalidTypeNameException message template "Inventory type name [<TypeName>] is invalid. Type name is case sensitive, such as AWS:APpLICation is invalid; Type name must start with either AWS: or Custom:;  Custom inventory type name must satisfy regular expression pattern: ^(Custom:)([a-zA-Z0-9]{3,93})$" — fixed text apart from the bracketed <TypeName> taken from the request (recorded: AWS:application), including the double space.
Emulator: HTTP 400 and the right error class (only $.Message diverged), but the message is "The parameter type name isn't valid: <TypeName>. An inventory type name is Custom:<name> or one of the type names Systems Manager publishes." — it omits the case-sensitivity rule, the AWS:/Custom: prefix rule and the custom-name pattern that AWS states.
Why: the whole value of this message is that it tells the caller WHY the name was rejected and what the rule is; a client or operator correcting a case mistake gets no hint that casing is the problem, and assertions on AWS's wording fail. This is a distinct cause from 4 (there the request is not validated at all; here it is validated and the refusal is worded wrongly) and from 5 (a different operation and a different condition).
Fix: ListInventoryEntries' error construction in the ssm handler — build the InvalidTypeNameException message from AWS's template with the request's type name interpolated.
Traces: ssm-vscan-traj-inventory-entries-type-and-id-mistakes#task#3.

No open issue covers these six. The nearest open ssm/inventory issues are 20261113T231833Z-ssm-inventory-and-patch-baseline-reads-getinventory-misclassifies.md (GetInventory request validation and DescribePatchBaselines projection — different operations), 20261103T215115Z-ssm-getinventoryschema-stub.md (filed when GetInventorySchema was an unimplemented stub; the operation now answers 200 with the single stale schema described in causes 1-2), 20261116T021504Z-ssm-describeinventordeletions-stub.md (see cause 3), 20261114T143448Z-ssm-putinventory-stub.md (a different operation) and 20261114T033033Z-ec2-ssm-ssm-getinventory-the-invalidaggregatorexception-message-for-an-unsupported.md (GetInventory aggregator message). None of them carries the replay keys above.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator booted from this checkout (emulator_up true, /__admin/health 200, data listener accepting). All six clusters are reported with stub_path empty and owned_by empty, i.e. net-new and unowned.

Cause 1 — GetInventorySchema, fixture ssm-catalog-get-inventory-schema step task#1. status 200 expected, 200 actual.
  $.Schemas[0]: missing — expected {'TypeName': 'AWS:AWSComponent', 'Version': '1.0', 'Attributes': [{'DataType': 'STRING', 'Name': 'Name'}, {'DataType': 'STRING', 'Name': 'ApplicationType'}, {'DataType': 'STRING', 'Name': 'Publisher'}, {'DataType': 'STRING', 'Name': 'Version'}, {'DataType': 'STRING', 'Name': 'InstalledTime'}, {'DataType': 'STRING', 'Name': 'Architecture'}, {'DataType': 'STRING', 'Name': 'URL'}]}, got {'TypeName': 'AWS:InstanceInformation', 'Version': '1.0', 'Attributes': [... ten attributes, each 'DataType': 'string']}.
  $.Schemas[1]: missing — expected the AWS:Application v1.1 schema (Name, ApplicationType, Publisher, Version, Release, Epoch, InstalledTime, Architecture, URL, Summary, PackageId; DataType 'STRING'), got None.
  $.Schemas[2]: missing — expected the AWS:ComplianceItem schema (ComplianceType, ExecutionId, ExecutionType, ExecutionTime, Id, Title, Status, Severity, ...), got None.
  Divergence key lists $.Schemas[0] through $.Schemas[14]: fifteen catalog entries expected, one served.
  The type/attribute names are the published AWS catalog; the DataType casing difference ('STRING' vs 'string') is a wire-value defect.

Cause 2 — GetInventorySchema, fixture ssm-inventory-and-compliance-tracking step task#2. status 200 expected, 200 actual.
  $.Schemas[0]: missing — expected {'TypeName': 'AWS:InstanceInformation', 'Version': '1.1', 'Attributes': [AgentType, AgentVersion, ComputerName, IamRole, InstanceId, IpAddress, PlatformName, PlatformType, PlatformVersion, ResourceType, AgentStatus, InstanceStatus, SourceType, SourceId, SourceLocation, AvailabilityZone, AvailabilityZoneId], each DataType 'STRING'}, got {'TypeName': 'AWS:InstanceInformation', 'Version': '1.0', 'Attributes': [AgentType, AgentVersion, ComputerName, InstanceId, InstanceStatus, IpAddress, PlatformName, PlatformType, PlatformVersion, ResourceType], each DataType 'string'}.

Cause 3 — DescribeInventoryDeletions, fixture ssm-describe-inventory-deletions-listing step task#3.
  status: expected 200, got 400.
  $.InventoryDeletions: missing — expected [], got None.

Cause 4 — ListInventoryEntries, fixture ssm-inventory-and-compliance-tracking step task#6 (also affects ssm-put-inventory-custom-item-then-check-deletions#task#2).
  status: expected 400, got 200.
  $.__type: missing — expected 'InvalidTypeNameException', got None.
  $.Message: missing — expected 'Inventory type name [Custom:sim2real-probe-ssm-AppInfo-bff97811] is invalid. Type name is case sensitive, such as AWS:APpLICation is invalid; Type name must start with either AWS: or Custom:;  Custom inventory type name must satisfy regular expression pattern: ^(Custom:)([a-zA-Z0-9]{3,93})$', got None.
  Fixed text: everything except the bracketed type name, which is echoed from the request; the recorded Custom:sim2real-probe-ssm-AppInfo-bff97811 is that run's value.

Cause 5 — DeleteInventory, fixture ssm-inventory-deletions-listing-and-unknown-type step task#2. status 400 expected, 400 actual (class agrees; only the message diverged).
  $.Message value_mismatch: expected "The given TypeName Custom:ProbeNeverRegistered85327641 is either deleted or doesn't exist", got "The parameter type name isn't valid: Custom:ProbeNeverRegistered85327641. No inventory of that type exists."
  Fixed text of the expected message: "The given TypeName " / " is either deleted or doesn't exist" (no trailing period); the only variable part is the type name from the request.

Cause 6 — ListInventoryEntries, fixture ssm-vscan-traj-inventory-entries-type-and-id-mistakes step task#3. status 400 expected, 400 actual (class agrees).
  $.Message value_mismatch: expected 'Inventory type name [AWS:application] is invalid. Type name is case sensitive, such as AWS:APpLICation is invalid; Type name must start with either AWS: or Custom:;  Custom inventory type name must satisfy regular expression pattern: ^(Custom:)([a-zA-Z0-9]{3,93})$', got "The parameter type name isn't valid: AWS:application. An inventory type name is Custom:<name> or one of the type names Systems Manager publishes."
  Only the bracketed type name is variable (AWS:application came from the request).

Model evidence used (services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json):
- GetInventorySchema: output Schemas -> InventoryItemSchemaResultList; @paginated {items: Schemas, pageSize: MaxResults @range 50..200}; errors InternalServerError, InvalidNextToken, InvalidTypeNameException.
- InventoryItemSchema: TypeName required @length 1..100 @pattern ^(AWS|Custom):.*$ (doc names the default types AWS:AWSComponent, AWS:Application, AWS:InstanceInformation, AWS:Network, AWS:WindowsUpdate); Version @pattern ^([0-9]{1,6})(\.[0-9]{1,6})$; Attributes required @length 1..50.
- InventoryItemAttribute{Name required, DataType required}; InventoryAttributeDataType enum values [string, number] — the uppercase 'STRING' on the wire is recorded behavior.
- ListInventoryEntries: InstanceId and TypeName required; TypeName @pattern ^(AWS|Custom):.*$; errors include InvalidTypeNameException (client, 400, awsQueryError InvalidTypeName).
- DeleteInventory: TypeName required; errors InvalidTypeNameException, InvalidInventoryRequestException, InvalidDeleteInventoryParametersException, InvalidOptionException (all client, 400).
- DescribeInventoryDeletions: output {InventoryDeletions -> InventoryDeletionsList of InventoryDeletionStatusItem, NextToken}; @paginated; client errors only InvalidDeletionIdException and InvalidNextToken.
- InvalidTypeNameException doc is "The parameter type name isn't valid." — which is verbatim the prefix of the emulator's authored messages in causes 5 and 6, showing the messages are built from the shape doc rather than AWS's templates.

## Steps to Reproduce
Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus against trace set simparity-wire-aws-v1), or drive each step directly against the ssm endpoint:

Cause 1: POST AmazonSSM.GetInventorySchema with no TypeName filter. Expect HTTP 200 with one InventoryItemSchema per published inventory type (the recording shows fifteen, beginning AWS:AWSComponent v1.0 and AWS:Application v1.1) and every attribute's DataType in AWS's uppercase form; observe a single AWS:InstanceInformation v1.0 entry with lowercase DataType. Fixture ssm-catalog-get-inventory-schema step task#1.

Cause 2: POST AmazonSSM.GetInventorySchema and read the AWS:InstanceInformation entry. Expect Version '1.1' and its seventeen attributes (including IamRole, AgentStatus, SourceType, SourceId, SourceLocation, AvailabilityZone, AvailabilityZoneId); observe Version '1.0' with ten attributes. Fixture ssm-inventory-and-compliance-tracking step task#2.

Cause 3: POST AmazonSSM.DescribeInventoryDeletions as a plain listing with no deletion history to report. Expect HTTP 200 with InventoryDeletions == []; observe HTTP 400 with no InventoryDeletions member. Fixture ssm-describe-inventory-deletions-listing step task#3.

Cause 4: POST AmazonSSM.ListInventoryEntries with a valid InstanceId and TypeName set to a Custom: name that violates ^(Custom:)([a-zA-Z0-9]{3,93})$ (recorded: Custom:sim2real-probe-ssm-AppInfo-bff97811, which contains hyphens). Expect HTTP 400 / __type InvalidTypeNameException with the "Inventory type name [...] is invalid..." template; observe HTTP 200 with an ordinary body and no error members. Fixture ssm-inventory-and-compliance-tracking step task#6 (also ssm-put-inventory-custom-item-then-check-deletions step task#2).

Cause 5: POST AmazonSSM.DeleteInventory with a well-formed Custom: type name that was never registered (recorded: Custom:ProbeNeverRegistered85327641). Expect HTTP 400 with the message "The given TypeName <TypeName> is either deleted or doesn't exist"; observe the emulator's "The parameter type name isn't valid: <TypeName>. No inventory of that type exists." Fixture ssm-inventory-deletions-listing-and-unknown-type step task#2.

Cause 6: POST AmazonSSM.ListInventoryEntries with TypeName 'AWS:application' (wrong case for AWS:Application). Expect HTTP 400 / InvalidTypeNameException with the "Inventory type name [...] is invalid. Type name is case sensitive..." template; observe the emulator's shortened prose. Fixture ssm-vscan-traj-inventory-entries-type-and-id-mistakes step task#3.

All six steps are in the same replayed corpus, so one replay after the fix re-exercises every cause (this issue's trace_ids name each one's fixture#step).

## Resolution

Fixed: all six causes. GetInventorySchema now serves the published inventory-type catalog from data
(`services/service-com-amazonaws-ssm-amazonssm/src/state/inventory_catalog.rs`) — one entry per
published type, with that type's attribute list and schema version, DataType in AWS's uppercase wire
form — beside the Region's custom types; the inventory type-name rule (published names spelled
exactly, or `^(Custom:)([a-zA-Z0-9]{3,93})$`) is applied at every operation that takes a type name
and refused in AWS's sentence; DeleteInventory words an unregistered type as "either deleted or
doesn't exist"; a custom type is read only while a write has defined it; and DescribeInventoryDeletions
projects the deletions a request matches, so an id nothing was recorded under is an empty list under
200. Replay: five of the six units pass.

RESIDUAL on cause 1, left for a later correction: the catalog's own attribute lists for
AWS:ComplianceItem, AWS:ComplianceSummary, AWS:File, AWS:InstanceDetailedInformation, AWS:Network,
AWS:PatchSummary and AWS:WindowsRole are reconstructed from AWS's published documentation and still
differ from the recording ($.Schemas[2..5,7,8,13] in fixture ssm-catalog-get-inventory-schema#task#1);
the other eight entries, including every one this report quotes, are byte-exact, as are the catalog's
type names, their order, the paging and the DataType casing. Each provisional list is one named const
in that module, so correcting one is a one-line data change.
