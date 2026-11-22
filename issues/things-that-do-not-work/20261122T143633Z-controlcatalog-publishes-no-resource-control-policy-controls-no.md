Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: controlcatalog.control-implementation-shape-follows-originating-service
Issue-Shape-Id: com.amazonaws.controlcatalog#ControlCatalog
Issue-Key: controlcatalog.control-implementation-shape-follows-originating-service|run#14|AssertionError|assert rcp_entry is not None, (
Issue-Commit: 518065cdf1c0387e4dc20c36bae80241fadcbe26

# ControlCatalog publishes no resource-control-policy controls: no ListControls row carries Implementation.Type AWS::Organizations::Policy::RESOURCE_CONTROL_POLICY

> _Note: this issue was identified against the code base at commit `518065cdf1c0387e4dc20c36bae80241fadcbe26`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListControls

## Description
The emulator's published control catalog contains no control implemented as an Organizations RESOURCE control policy, so the whole RCP implementation class is missing from ListControls (and therefore from GetControl, which can only be reached through an ARN the listing yields).

Cause 1 of 1: the control catalog has no AWS::Organizations::Policy::RESOURCE_CONTROL_POLICY row.

AWS: a control's Implementation reports the service that implements it. Smithy (services/service-com-amazonaws-controlcatalog-controlcatalog/smithy-model/com-amazonaws-controlcatalog-controlcatalog.json): ListControls output ControlSummary.Implementation is an ImplementationSummary whose Type is @required (ImplementationType, @length 7..2048, @pattern ^[A-Za-z0-9]+(::[A-Za-z0-9_]+){2,3}$) and whose Identifier is OPTIONAL; the ImplementationDetails doc states the Type format has "an optional fourth segment, which is present for applicable implementation types" and gives AWS::Organizations::Policy::SERVICE_CONTROL_POLICY and AWS::CloudFormation::Type::HOOK as examples of that four-segment form. ControlFilter.Implementations.Types is documented as the filter for "controls implemented as" a given service surface. The model does not enumerate the catalog's rows, so the existence of RESOURCE_CONTROL_POLICY controls is recorded behavior only: AWS's catalog publishes controls deployed as Organizations resource control policies under Implementation.Type 'AWS::Organizations::Policy::RESOURCE_CONTROL_POLICY', and because a policy has no per-control identifier assigned by the implementing service, such a row's ImplementationSummary carries Type ONLY (no Identifier) on both ListControls and GetControl, while still carrying a non-empty Aliases list of published designators. Nothing about these rows is per-run: a catalog control ARN (arn:aws:controlcatalog:::control/<25-char id>) and its Implementation are account- and Region-independent facts.

Emulator: scanning the catalog through ListControls with MaxResults=100 for up to 25 pages (i.e. the catalog in full) yields no control at all whose Implementation.Type is 'AWS::Organizations::Policy::RESOURCE_CONTROL_POLICY'. Every served row's Type comes from a fixed set of four values only — CONFIG_RULE 'AWS::Config::ConfigRule', SECURITY_HUB_CONTROL 'AWS::SecurityHub::SecurityControl', SERVICE_CONTROL_POLICY 'AWS::Organizations::Policy::SERVICE_CONTROL_POLICY' and CLOUDFORMATION_HOOK 'AWS::CloudFormation::Type::HOOK'. The requests themselves succeed (HTTP 200, well-formed ListControlsResponse); it is the catalog CONTENT that lacks the class. The check's earlier assertions on the AWS::Config::ConfigRule row (Implementation keys exactly ['Identifier','Type'], equal between ListControls and GetControl) passed, so this is specifically the RCP class and not a general Implementation serialization fault.

Why: a client that enumerates governance controls by implementation type — the documented purpose of ControlFilter.Implementations.Types — sees an RCP-free world. Filtering on 'AWS::Organizations::Policy::RESOURCE_CONTROL_POLICY' returns an empty page rather than AWS's resource-control-policy controls, so coverage reports, control-to-policy inventories and any lookup keyed on a published RCP control ARN silently find nothing instead of failing loudly; code paths that handle the Identifier-absent (Type-only) Implementation shape are never exercised at all, since the only identifier-less rows the emulator serves are SCP rows.

Fix: the ListControls/GetControl catalog state — the published-control data set the handlers project their responses out of must include AWS's resource-control-policy control rows, each with implementation_type 'AWS::Organizations::Policy::RESOURCE_CONTROL_POLICY', implementation_identifier None (so the serialized ImplementationSummary carries Type only) and its published non-empty Aliases, Name and Description text; the implementation-type filter in the ListControls handler must then match them. File and line citations are under Traces.

Traces: behavioral check controlcatalog.control-implementation-shape-follows-originating-service. The missing class is visible in the catalog data itself: services/service-com-amazonaws-controlcatalog-controlcatalog/src/state/published_controls.rs lines 143-147 declare the only four implementation-type constants the table uses, and no row in PUBLISHED_CATALOG_RECORDS names a RESOURCE_CONTROL_POLICY type (the SERVICE_CONTROL_POLICY rows at lines 1097, 1126, 1150 and 1175 are the only Organizations-policy rows).

Relation to open issues: none of the open controlcatalog issues covers this class. 20261122T055800Z-controlcatalog-listcontrols-serves-a-divergent-aws-config-configrule and 20261118T190218Z-controlcatalog-listcontrols-rewrites-the-published-description-of-the concern AWS::Config::ConfigRule rows' Name/Description text; 20261115T014344Z-controlcatalog-config-rule-controls-are-served-under-emulator-minted concerns Config-rule ARN minting and emptied Aliases; 20261115T014344Z-controlcatalog-getcontrol-answers-404-for-catalogued-control-arns-and concerns GetControl on ARNs that ARE catalogued; the remaining ones concern the domain/objective/common-control taxonomy and ListControlMappings. The replay reports no owning issue and no stub for this check.

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the emulator built from this checkout: 68/69 passed; the single failure is controlcatalog.control-implementation-shape-follows-originating-service (verdict: mismatch).

Check output:
  File "check_controlcatalog_control_implementation_shape_follows_originating_service.py", line 65, in run
    assert rcp_entry is not None, (
  AssertionError: no control with Implementation.Type 'AWS::Organizations::Policy::RESOURCE_CONTROL_POLICY' found while scanning the catalog

Expected (AWS): paging ListControls (MaxResults=100, following NextToken) reaches at least one ControlSummary with Implementation == {"Type": "AWS::Organizations::Policy::RESOURCE_CONTROL_POLICY"} — Type only, no Identifier — with a non-empty Aliases list, and GetControl(ControlArn=<that Arn>) returns the identical Implementation (Type only) and the same Aliases set.

Actual (emulator): all pages return HTTP 200 with well-formed ListControlsResponse bodies, but no row of that Implementation.Type exists anywhere in the catalog; the scan exhausts NextToken (up to 25 pages of 100) without a match, so the dependent GetControl comparison is never reachable. The earlier part of the same check — the AWS::Config::ConfigRule row's Implementation keys ['Identifier','Type'], identical between ListControls and GetControl — passed.

Corroborating catalog data in the checkout: services/service-com-amazonaws-controlcatalog-controlcatalog/src/state/published_controls.rs
  143: pub(super) const CONFIG_RULE: &str = "AWS::Config::ConfigRule";
  144: pub(super) const SECURITY_HUB_CONTROL: &str = "AWS::SecurityHub::SecurityControl";
  145-146: pub(super) const SERVICE_CONTROL_POLICY: &str = "AWS::Organizations::Policy::SERVICE_CONTROL_POLICY";
  147: pub(super) const CLOUDFORMATION_HOOK: &str = "AWS::CloudFormation::Type::HOOK";
There is no RESOURCE_CONTROL_POLICY constant and no row referencing one.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from this checkout.
2. Point a client at the emulator's controlcatalog endpoint.
3. Call ListControls (POST /list-controls) with Filter={"Implementations": {"Types": ["AWS::Organizations::Policy::RESOURCE_CONTROL_POLICY"]}} — the response is 200 with an empty Controls list.
4. Or enumerate without a filter: call ListControls with MaxResults=100, following NextToken to the end of the catalog, and collect every control's Implementation.Type. No returned control has Type 'AWS::Organizations::Policy::RESOURCE_CONTROL_POLICY'; the only values present are AWS::Config::ConfigRule, AWS::SecurityHub::SecurityControl, AWS::Organizations::Policy::SERVICE_CONTROL_POLICY and AWS::CloudFormation::Type::HOOK.
5. Against AWS the same scan reaches at least one resource-control-policy control, whose Implementation carries Type only (no Identifier) and whose Aliases list is non-empty, and GetControl on its Arn returns the same Implementation and Aliases.
6. To confirm a fix, re-replay the behavioral check controlcatalog.control-implementation-shape-follows-originating-service.
