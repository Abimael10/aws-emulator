Issue-Source: trace-sweep-divergence
Issue-Trace: rum-app-monitor-full-configuration
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rum-app-monitor-full-configuration rum-basic-app-monitor-lifecycle rum-page-view-event-is-returned-by-get-app-monitor-data
Issue-Shape-Id: com.amazonaws.rum#RUM
Issue-Key: rum|GetAppMonitor|2xx->2xx|$.AppMonitor.DataStorage.CwLog.CwLogGroup
Issue-Key: rum|GetAppMonitorData|2xx->2xx|$.Events[*]
Issue-Commit: c439b0c06ad4c0c74cea84caf475018e0421a0f9

# rum: service-derived response fields diverge from AWS (vended CwLogGroup, enriched RUM event document)

> _Note: this issue was identified against the code base at commit `c439b0c06ad4c0c74cea84caf475018e0421a0f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetAppMonitor

## Description
This issue bundles TWO distinct net-new root causes in service `rum`, both in values RUM DERIVES itself (as opposed to echoing from the request): the vended CloudWatch Logs group an app monitor advertises, and the enriched RUM event document the data plane stores and a query reports. Both causes must be closed before submitting; each is numbered and carries its own fixture and step.

Neither cause is covered by the one open RUM issue, 20261029T192752Z-rum-app-monitor-data-plane-resource-policy-tagging.md (behavioral corpus): that issue's Cause 2 is that submitted events never become READABLE at all, while the wire corpus shows events are now readable and the divergence is the CONTENT of the reported document; nothing in it mentions DataStorage.CwLog.CwLogGroup.

Cause 1 of 2: GetAppMonitor reports a DataStorage.CwLog.CwLogGroup that is not the vended log-group value the corpus holds for that app monitor (fixture rum-app-monitor-full-configuration, step task#2; same cluster also at rum-basic-app-monitor-lifecycle#task#4).
AWS: Smithy: AppMonitor$DataStorage -> DataStorage$CwLog -> CwLog$CwLogGroup is a plain String with NO @pattern and NO @length; its documentation is "The name of the log group where the copies are stored", and CwLog$CwLogEnabled is "Indicated whether the app monitor stores copies of the data that RUM collects in CloudWatch Logs". Everything else here is recorded behavior only: the recorded GetAppMonitor response carries RUM's vended-logs group name for that monitor, and the corpus keeps it as the single normalized binding `{{cw_log_group}}` — one value per app monitor, so every read of that monitor in the fixture resolves to the SAME name, and that name is the group RUM actually provisioned in CloudWatch Logs. The literal recorded name is a per-run generated value and is NOT the requirement; the requirement is the FORM of the name and that one monitor has exactly one such name, surfaced consistently everywhere the fixture binds it from.
Emulator: the replayed step reports `$.AppMonitor.DataStorage.CwLog.CwLogGroup: value_mismatch (expected '{{cw_log_group}}', got '/aws/vendedlogs/RUMService_sim2real-probe-rum-rum-fullcfg-d17644086394ef43fb48')` — the expected side is still the raw, UNRESOLVED binding, i.e. nothing the emulator answered during the replay bound `cw_log_group`, while GetAppMonitor nonetheless hands back a name it minted locally: the vended prefix, the app monitor's name, and twelve lowercase hex characters concatenated with NO separator. Status is 200 on both sides; only this key diverges.
Hypothesis (not stated by the model and not stated by the recording): the minted name's shape is what the binding rejects — the service-chosen part is a bare 12-hex run glued straight onto the monitor name, so a name AWS produced for the same monitor and a name this emulator produces are not members of the same family, and the fixture's one vended-log-group value never resolves.
Why: CwLogGroup is the only handle a client has on where its telemetry copies land. A client that reads it and then tails, sets retention on, or reads the policy of that group is relying on the advertised name naming RUM's vended group for that monitor; a name of a different family (or one that disagrees with the name any other RUM response surfaces for the same monitor) sends every such client to a group it cannot correlate, and makes the app monitor's log destination unassertable.
Fix: the app monitor's vended-log-group state and its projection into the GetAppMonitor response — the one place the name is minted and the one place it is read back onto the wire (state.rs: CW_LOG_GROUP_PREFIX / CW_LOG_GROUP_SUFFIX_LEN, cw_log_group_name(), ComAmazonawsRumRumState::mint_cw_log_group(), AppMonitorRecord::vended_log_group / cw_log_group(), and the DataStorage/CwLog projection in the record->AppMonitor mapping). Whatever name is settled on must stay the single stored value that is both advertised and provisioned, so the same string is reported by every read of that monitor.
Traces: rum-app-monitor-full-configuration#task#2 (representative), rum-basic-app-monitor-lifecycle#task#4 (same cluster).

Cause 2 of 2: the enriched RUM event document GetAppMonitorData reports is built with the wrong shape for several service-derived fields (fixture rum-page-view-event-is-returned-by-get-app-monitor-data, step task#3).
AWS: Smithy: GetAppMonitorData returns Events as an EventDataList of EventData strings; RumEvent carries only {id, timestamp, type, metadata, details} and the model publishes NO schema for the document a read returns, so the document's fields are recorded behavior only. The recorded document for a page-view event shows, beside the fields the emulator already gets right (userId, sessionId, application_id, application_name, application_version, event_id, event_type, event_version, metadata, event_details, user_details, account_id):
  (a) `log_stream` is a STRUCTURE, `{"seconds": <epoch seconds>, "nanos": <subsecond nanos>}`, carrying an instant — in this fixture its `seconds` tracked the timestamp the replayed request submitted (the recorded value moved with the run), so it is derived from the event/delivery instant and is not a free-form name;
  (b) `metadata_values` carries one `"<key>=<value>"` text per first-level member of `metadata`, positionally aligned with `metadata_keys` (recorded: ["version=1.0.0","pageId=/checkout","title=Checkout","domain=example.com","countryCode=US","subdivisionCode=OR","localityName=Boardman"]);
  (c) `metadata` is ENRICHED by the service with the caller's geolocation members `countryCode`, `subdivisionCode` and `localityName` on top of the members the client serialized, and those members appear in `metadata_keys`/`metadata_values` too. Their VALUES are derived from the caller's address and are environment-specific (the recording's were US / OR / Boardman); the requirement is that the service adds those members, not the recorded values;
  (d) `event_interaction` is a STRING naming the interaction the event belongs to — recorded as `"%2Fcheckout-0 &"` for an event whose details carried `"pageInteractionId":"/checkout-0"` and `"interaction":0.0`. Hypothesis: it is the percent-encoded pageInteractionId of the event; what the recording establishes without interpretation is that it is NOT the numeric `interaction` member of the client's details.
Emulator: the document it stores at ingestion and replays on the read diverges on exactly those derived fields: `log_stream` is the plain string `"2c667ce1-45af-314b-f6ef-5a384812a78b"` — the batch's UserDetails$sessionId, i.e. the CloudWatch Logs STREAM NAME it chose — where AWS has the {seconds,nanos} structure; `metadata_values` is `["1.0.0","example.com","/checkout","Checkout"]`, the bare value texts with no `<key>=` prefix; `metadata` (and therefore `metadata_keys`/`metadata_values`) carries only the four members the client sent, with no countryCode/subdivisionCode/localityName; and `event_interaction` is filled from the client's numeric `interaction` member (0.0 here) rather than the interaction string (the emulator's value for this field was clipped out of the replay detail, so only its construction is observable). Status is 200 on both sides; the divergence is in $.Events[0].
NOT part of this cause, and deliberately excluded so a fixer does not chase them: `event_timestamp` (expected 1793914958000, got 1795092508000) and `batch_id` differ only because the replay submitted a fresh timestamp and a fresh BatchId — the handler echoes RumEvent$timestamp and PutRumEventsRequest$BatchId verbatim, so these are per-run values, not defects. The first-level ORDER of the metadata members also differs and is likewise not claimed here, since it follows the order of the submitted request body.
Why: the document a read returns is RUM's published event schema — it is what custom-metric EventPatterns/ValueKeys are matched against, what dashboards and log consumers parse, and the same bytes RUM copies into CloudWatch Logs. A consumer that reads `log_stream` as a timestamp structure gets a string; one that splits `metadata_values` on `=` to recover key/value pairs gets values with no keys and cannot align them; one that filters or groups by country/region/city finds those members absent altogether; one that groups by `event_interaction` gets a number where AWS gives an interaction identifier.
Fix: the data-plane enrichment that BUILDS the event document (state/rum_event.rs: event_document(), its Delivery struct — whose `log_stream` member is currently handed the CloudWatch Logs stream name from cw_log_delivery::delivery_stream_name() — and metadata_projections()/projected_text()). GetAppMonitorData only replays the stored bytes and must not re-render them, so the fix belongs at ingestion in PutRumEvents' document builder (the CloudWatch Logs copy shares those bytes and is corrected with it).
Traces: rum-page-view-event-is-returned-by-get-app-monitor-data#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 47 fixtures replayed, 283 steps, 280 passed, 3 mismatched, 0 errored, 0 blocked, score 98.9%, bootstrap_signal false, emulator_up true (/__admin/health 200). All 3 mismatched steps are in service `rum`, in 2 clusters, and neither cluster carries a stub_path or an owned_by.

Cause 1 — cluster divergence_key `service=rum|GetAppMonitor|status=200->200|keys=$.AppMonitor.DataStorage.CwLog.CwLogGroup`, expected_status 200, actual_status 200, verdict mismatch, affected_count 2:
  rum-app-monitor-full-configuration#task#2 (representative) and rum-basic-app-monitor-lifecycle#task#4.
  detail: $.AppMonitor.DataStorage.CwLog.CwLogGroup: value_mismatch (expected '{{cw_log_group}}', got '/aws/vendedlogs/RUMService_sim2real-probe-rum-rum-fullcfg-d17644086394ef43fb48')
  The expected side is the corpus's unresolved vended-log-group binding; the actual is the emulator's locally minted name (prefix '/aws/vendedlogs/RUMService_' + app monitor name + 12 lowercase hex, concatenated without a separator — state.rs cw_log_group_name()/CW_LOG_GROUP_SUFFIX_LEN).

Cause 2 — cluster divergence_key `service=rum|GetAppMonitorData|status=200->200|keys=$.Events[0]`, expected_status 200, actual_status 200, verdict mismatch, affected_count 1:
  rum-page-view-event-is-returned-by-get-app-monitor-data#task#3.
  Expected $.Events[0] (recorded), abridged to the diverging members:
    "log_stream":{"seconds":1795092508,"nanos":0}
    "metadata":{"version":"1.0.0","pageId":"/checkout","title":"Checkout","domain":"example.com","countryCode":"US","subdivisionCode":"OR","localityName":"Boardman"}
    "metadata_keys":["version","pageId","title","domain","countryCode","subdivisionCode","localityName"]
    "metadata_values":["version=1.0.0","pageId=/checkout","title=Checkout","domain=example.com","countryCode=US","subdivisionCode=OR","localityName=Boardman"]
    "event_details":{"version":"1.0.0","pageId":"/checkout","pageInteractionId":"/checkout-0","interaction":0.0}
    "event_interaction":"%2Fcheckout-0 &"
  Actual $.Events[0] (emulator), same members:
    "log_stream":"2c667ce1-45af-314b-f6ef-5a384812a78b"   (== UserDetails$sessionId of the batch)
    "metadata":{"version":"1.0.0","domain":"example.com","pageId":"/checkout","title":"Checkout"}
    "metadata_keys":["version","domain","pageId","title"]
    "metadata_values":["1.0.0","example.com","/checkout","Checkout"]
    (the tail of the actual document, including its event_interaction, was clipped out of the replay detail)
  Per-run-only differences inside the same document, NOT filed: event_timestamp 1793914958000 -> 1795092508000 and batch_id 2a526089-0dda-4c57-a64c-441ba0255536 -> 2005d6cc-9b8a-44aa-8f96-53c10c4913bf (both are echoed verbatim from the replayed request).

Supporting code read in this checkout (for orientation only): services/service-com-amazonaws-rum-rum/src/state/rum_event.rs lines 156-228 (event_document: log_stream taken from Delivery$log_stream; event_interaction from the client's `interaction` member) and 246-268 (metadata_projections/projected_text: value text with no key prefix); src/ops/put_rum_events.rs lines 251-277 (log_stream = cw_log_delivery::delivery_stream_name(sessionId, id)); src/state.rs lines 996-1000 and 2783-2795 (DataStorage/CwLog projection and cw_log_group_name).

## Steps to Reproduce
Cause 1 (fixture rum-app-monitor-full-configuration, step task#2; also rum-basic-app-monitor-lifecycle#task#4):
1. CreateAppMonitor(Name=<m>, Domain=example.com, CwLogEnabled=true, AppMonitorConfiguration={...}).
2. GetAppMonitor(Name=<m>) and read AppMonitor.DataStorage.CwLog.CwLogGroup.
Observed: a locally minted name of the form /aws/vendedlogs/RUMService_<m><12 lowercase hex> that the corpus's single `cw_log_group` binding for that monitor never resolves to (the expected side stays the literal '{{cw_log_group}}').
Expected: the vended log-group name RUM advertises for that monitor — one value per monitor, of AWS's vended-logs family, identical in every response that surfaces it and naming the group that was actually provisioned in CloudWatch Logs. Do not assert the recorded literal: it is a generated per-run value.

Cause 2 (fixture rum-page-view-event-is-returned-by-get-app-monitor-data, step task#3):
1. CreateAppMonitor(Name=<m>, Domain=example.com) and note the returned Id.
2. PutRumEvents(Id=<id>, BatchId=<uuid>, AppMonitorDetails={name:<m>, version:"1.0.0"}, UserDetails={userId:<uuid>, sessionId:<uuid>}, RumEvents=[{id:<uuid>, timestamp:<now>, type:"com.amazon.rum.page_view_event", metadata:'{"version":"1.0.0","pageId":"/checkout","title":"Checkout","domain":"example.com"}', details:'{"version":"1.0.0","pageId":"/checkout","pageInteractionId":"/checkout-0","interaction":0.0}'}]).
3. GetAppMonitorData(Name=<m>, TimeRange={After:<now_ms-3600000>, Before:<now_ms+60000>}, MaxResults=100) and parse Events[0].
Observed: log_stream is the sessionId string; metadata_values is the bare value texts; metadata/metadata_keys/metadata_values carry only the client's four members; event_interaction is built from the numeric `interaction` member.
Expected: log_stream as {"seconds":<epoch seconds of the event instant>,"nanos":<subsecond nanos>}; metadata_values as ["version=1.0.0","pageId=/checkout","title=Checkout","domain=example.com", ...] aligned with metadata_keys; metadata additionally carrying countryCode/subdivisionCode/localityName from the caller's address (values environment-derived, presence required) and those members reflected in metadata_keys/metadata_values; event_interaction as the event's interaction identifier string (recorded '%2Fcheckout-0 &' for pageInteractionId '/checkout-0'), not the numeric interaction. event_timestamp and batch_id must keep echoing the request and are not part of this repro.
