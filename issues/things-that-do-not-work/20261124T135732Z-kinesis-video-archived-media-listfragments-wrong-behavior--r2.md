Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 3202d1035b21c847e49e20ac2e3c9eb6751ce1dc

# ListFragments (and GetMediaForFragmentList) serve the archive of a stream that retains no data, which this crate's own state docs say holds no archive and which every sibling read refuses

_Filed: 2026-11-24T13:57:32Z_
_Operation: ListFragments | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `3202d1035b21c847e49e20ac2e3c9eb6751ce1dc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

This crate states, twice and in two different structs, that a stream whose `DataRetentionInHours` is 0 HOLDS NO ARCHIVE, and four of the six read handlers implement that rule by refusing such a stream before they touch its fragments (`GetClip`, `GetImages`, `GetHLSStreamingSessionURL`, `GetDASHStreamingSessionURL` all call `stream.retains_data()`).

`ListFragments` and `GetMediaForFragmentList` never consult `retains_data()` at all — a crate-wide grep for `retains_data` matches only the four handlers above plus the two definitions in `state.rs`. So for one and the same stream, in one and the same engine state, this service answers two contradictory things: `GetClip`/`GetImages`/the two streaming-session operations say the stream retains nothing and therefore has no archived media (400 `NoDataRetentionException`, message "The stream data retention is set to 0 hours."), while `ListFragments` answers 200 with a populated `Fragments` list for the very same window and `GetMediaForFragmentList` answers 200 with that archive's media bytes.

The state is reachable through plain API calls: nothing gates ingest on retention. The Kinesis Video Media inlet's `archive_fragment` copies whatever retention the registry holds (including 0) into the archive entry and appends the fragment unconditionally, and nothing purges an archive when retention is later decreased to 0. So a stream created with `DataRetentionInHours: 0` (or decreased to 0 after ingest) ends up with a non-empty archive that two of the six reads report and four refuse.

I am not prescribing which side moves — either a non-retaining stream's archive must be empty (as the state docs say) or these two reads must not serve it — but the two answers cannot both be right, and today `ListFragments`/`GetMediaForFragmentList` report media the rest of the service says does not exist.

## Evidence

The rule this crate states for itself — services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src/state.rs:271-277 (`ArchivedStream::retains_data`):
```
    /// Whether this stream retains data at all. A stream with no retention
    /// holds no archive, which the archived-media operations report as
    /// `NoDataRetentionException`.
    #[must_use]
    pub fn retains_data(&self) -> bool {
        self.data_retention_in_hours > 0
    }
```
and the same sentence on the shape the handlers actually hold — src/state.rs:387-394 (`ResolvedStream::retains_data`): "/// Whether this stream retains data at all. A stream with no retention\n/// holds no archive, which the archived-media operations report as\n/// `NoDataRetentionException`." (also src/state.rs:185-188: "/// How long the stream retains data. `0` means no retention is configured,\n/// which is what makes the archived-media operations answer\n/// `NoDataRetentionException`.")

The four siblings that apply it, each before reading any fragment:
* src/ops/get_clip.rs:376-384 — `// "Data retention must be greater than 0."` / `if !stream.retains_data() { return Err(GetClipErrorModeled::NoDataRetentionException(...)) }`
* src/ops/get_images.rs:419-428 — `// "GetImages was requested for a stream that does not retain data (that is,` / `// has a DataRetentionInHours of 0)."` / `if !stream.retains_data() { ... NoDataRetentionException ... }`
* src/ops/get_hls_streaming_session_url.rs:472-480 — `if !stream.retains_data() { ... NoDataRetentionException ... }`
* src/ops/get_dash_streaming_session_url.rs:460-470 — `if !stream.retains_data() { ... NoDataRetentionException ... }`

The two that skip it. src/ops/list_fragments.rs:245-305 goes straight from `state.resolve_stream(...)` to the cursor and then to `stream.fragments_in_range(cursor.origin, cursor.start_millis, cursor.end_millis)` (list_fragments.rs:304-305) with no retention test anywhere in the file; src/ops/get_media_for_fragment_list.rs:227-273 goes straight from `state.resolve_stream(...)` to `stream.fragment(fragment_number)` and then concatenates the stored bytes (get_media_for_fragment_list.rs:293-308). `grep -rn 'retains_data' services/service-com-amazonaws-kinesisvideoarchivedmedia-awsacuityreader/src` matches only state.rs:275/392 and the four handler lines above.

The ingest side that makes a non-retaining stream's archive non-empty — services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/state.rs:650-661:
```
        store.put_stream(archive::ArchivedStream {
            name: registered.name.clone(),
            arn: registered.arn.clone(),
            data_retention_in_hours: registered.data_retention_in_hours,
            ...
        });
    }
    store.append_fragment(&registered.name, fragment);
```
(`grep -n 'retention|retains'` over that crate's `src` finds no refusal — only the field being copied, at state.rs:588-589, 608, 648, 654.)

Test coverage: the seeded `NO_RETENTION_STREAM` of tests/all/archived_media_read_path.rs:602 (`ArchivedStream::new(NO_RETENTION_STREAM, REGION, created_at, 0)`) is registered with an EMPTY archive — the seeding loop at tests/all/archived_media_read_path.rs:611-625 appends fragments only to `STREAM` (retention 24) — so the only no-retention assertions that exist are the four refusals (e.g. tests/all/archived_media_read_path.rs:1888-1914 `get_hls_streaming_session_url_refuses_a_stream_with_no_retention`, tests/all/archived_media_read_path.rs:1382-1389 for GetClip, tests/all/get_images_generation.rs:1052, tests/all/get_dash_streaming_session_url.rs:659). No test seeds a non-retaining stream WITH fragments, so nothing pins what `ListFragments`/`GetMediaForFragmentList` answer for one.

## Steps to Reproduce

1. Create a stream in the Kinesis Video Streams control plane with DataRetentionInHours set to 0 (or create it with retention and then decrease it to 0 — nothing purges this service's archive on that change).
2. Give that stream an archive: PutMedia one fragment on the Kinesis Video Media inlet, which archives regardless of retention (services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/src/state.rs:650-661), or in-process `state.put_stream(ArchivedStream::new("cam", REGION, now, 0))` followed by `state.append_fragment("cam", StoredFragment::new("1", t, t, 2000, media))`, the way tests/all/archived_media_read_path.rs:600-625 seeds the retaining stream.
3. POST /listFragments with {"StreamName": "cam", "FragmentSelector": {"FragmentSelectorType": "PRODUCER_TIMESTAMP", "TimestampRange": {"StartTimestamp": t, "EndTimestamp": t + 10}}} — the answer is HTTP 200 with the fragment listed.
4. POST /getMediaForFragmentList with {"StreamName": "cam", "Fragments": ["1"]} — the answer is HTTP 200 with the stored media bytes.
5. POST /getClip (and /getImages, /getHLSStreamingSessionURL, /getDASHStreamingSessionURL) for the same stream and window — each answers HTTP 400 NoDataRetentionException with "The stream data retention is set to 0 hours.", i.e. the same engine state is reported as holding an archive by two operations and as holding none by the other four, while src/state.rs:271-277 states "A stream with no retention holds no archive".
