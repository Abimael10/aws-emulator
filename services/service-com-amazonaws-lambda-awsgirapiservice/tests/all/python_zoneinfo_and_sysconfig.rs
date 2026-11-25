// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TIME ZONE and CONFIGURATION modules of the standard library a
//! `python3.x` function's run is given: `CreateFunction` then `Invoke` (`POST
//! /2015-03-31/functions/{FunctionName}/invocations`) over the router.
//!
//! The rule under test: a `python3.x` runtime presents the standard library of the runtime it
//! advertises, so `from zoneinfo import ZoneInfo` and `import sysconfig` import cleanly — and
//! what they answer is read out of the runtime itself: a zone's offset, its daylight-saving
//! part and its abbreviation at an instant come from the IANA time-zone database the runtime
//! ships, and the configuration variables and paths come from the execution environment the
//! function runs in.
//!
//! Every row drives real HTTP requests through the router, and the rows differ along what the
//! rule generalises over: which zone is named, which side of a daylight-saving transition the
//! moment falls on, whether the local time is ambiguous or nonexistent and which `fold` reads
//! it, which direction the conversion goes, and which surface of the configuration module is
//! read.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::invoke_request_id::{is_host_bound, one_invocation_stream_at_a_time};
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A SigV4 credential scope naming this service's signing name (`lambda`) in `us-east-1`.
/// Nothing in it is verified: the harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// How long past a due instant this suite waits before calling. The transition is owed by the
/// CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(150);

/// The `Timeout` every function this suite creates carries, in seconds. Nothing here is about a
/// deadline, so it is given generous room.
const FUNCTION_TIMEOUT_SECONDS: i32 = 600;

/// How many times a case re-drives its invocation while the answer reports one of this test
/// binary's own host bounds rather than a run.
const HOST_BOUND_ATTEMPTS: usize = 6;

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// Dispatch one request.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &[u8]) -> Answer {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// Create a `python3.12` function whose package holds `source` as `lambda_function.py`, and
/// wait out the provisioning window the create documents.
async fn create_function(router: &MultiServiceRouter, name: &str, source: &str) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "lambda_function.handler",
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", source)])),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {} {}",
        created.status,
        created.text()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// `Invoke` on `name` with `event`, re-driven while the answer reports a bound this whole test
/// binary shares rather than a run.
async fn invoke(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
    let mut answered = invoke_once(router, name, event).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        answered = invoke_once(router, name, event).await;
    }
    answered
}

/// One `Invoke` request on the function `name` names.
async fn invoke_once(router: &MultiServiceRouter, name: &str, event: &Value) -> Answer {
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        &serde_json::to_vec(event).expect("the event serializes"),
    )
    .await
}

/// What one case of the probe answered, or `None` when the run was ended by a bound this whole
/// test binary shares rather than by anything the case asked for.
async fn answered(
    router: &MultiServiceRouter,
    name: &str,
    case: &str,
    event: &Value,
) -> Option<Value> {
    let answer = invoke(router, name, event).await;
    if is_host_bound(answer.status, &answer.text()) {
        return None;
    }
    assert_eq!(
        answer.status,
        200,
        "case {case}: expected 200, got {} {}",
        answer.status,
        answer.text()
    );
    let failure = answer.header("x-amz-function-error");
    assert_eq!(
        failure, None,
        "case {case}: the handler's module imports cleanly and the handler catches each refusal \
         itself, so the invocation carries no function error, got {failure:?} with {}",
        answer.text()
    );
    Some(answer.json())
}

/// The whole rule, driven END TO END: ONE real `CreateFunction` whose handler module BODY
/// imports `zoneinfo` and `sysconfig` — which is where the recorded symptom failed, an absent
/// module of the library failing the whole package's import — and real `Invoke` requests over
/// the router, which the tables below read.
#[tokio::test]
async fn the_python_runtime_answers_out_of_the_library_it_ships() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let probe = "probe-zoneinfo-zones";
    create_function(&router, probe, PROBE_SOURCE).await;
    let mut read = a_zones_offset_is_the_databases_answer_for_that_moment(&router, probe).await;
    read += an_instant_converts_to_the_wall_time_of_the_zone(&router, probe).await;
    read += the_zones_are_the_databases_own(&router, probe).await;
    read += the_configuration_is_the_environments_own(&router, probe).await;
    assert!(
        read > 0,
        "expected at least one case of the tables above to be answered by a run, got {read}: \
         every invocation reported a bound this whole test binary shares, so nothing of the rule \
         was read"
    );
}

/// One row of the table below: the case, the zone key, the local date and time it is read at,
/// the `fold` it is read under, the offset and the daylight-saving part of it in seconds, and
/// the abbreviation the zone goes by.
type Reading = (
    &'static str,
    &'static str,
    [i64; 5],
    i64,
    i64,
    i64,
    &'static str,
);

/// The offset, the daylight-saving part of it and the abbreviation a zone has AT a moment.
///
/// Source of each expected value: the IANA time-zone database's own rules for those zones — the
/// United States' daylight saving from 2007 (second Sunday in March to first Sunday in
/// November), the European Union's (last Sunday in March to last Sunday in October), India's
/// single standard offset of +05:30, and Australia's southern-hemisphere summer. The rows name
/// zones the report this rule came from does not cite, each side of a transition, and the two
/// local times a transition makes: the AMBIGUOUS hour a zone repeats, whose two readings `fold`
/// chooses between, and the NONEXISTENT hour it skips.
///
/// Answers how many cases a run answered.
async fn a_zones_offset_is_the_databases_answer_for_that_moment(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    // (the case, the zone key, the local date and time, the fold, the offset in seconds, the
    //  daylight-saving part in seconds, the abbreviation)
    let cases: &[Reading] = &[
        ("the zone with no offset", "UTC", [2024, 1, 15, 12, 0], 0, 0, 0, "UTC"),
        (
            "a zone on standard time",
            "America/New_York",
            [2024, 1, 15, 12, 0],
            0,
            -18_000,
            0,
            "EST",
        ),
        (
            "the same zone on daylight saving",
            "America/New_York",
            [2024, 7, 15, 12, 0],
            0,
            -14_400,
            3_600,
            "EDT",
        ),
        (
            "a zone east of the meridian on standard time",
            "Europe/Berlin",
            [2024, 1, 15, 12, 0],
            0,
            3_600,
            0,
            "CET",
        ),
        (
            "the same zone on summer time",
            "Europe/Berlin",
            [2024, 7, 15, 12, 0],
            0,
            7_200,
            3_600,
            "CEST",
        ),
        (
            "a zone whose offset is not a whole hour",
            "Asia/Kolkata",
            [2024, 1, 15, 12, 0],
            0,
            19_800,
            0,
            "IST",
        ),
        (
            "a southern-hemisphere zone in its summer",
            "Australia/Sydney",
            [2024, 1, 15, 12, 0],
            0,
            39_600,
            3_600,
            "AEDT",
        ),
        (
            "the same zone in its winter",
            "Australia/Sydney",
            [2024, 7, 15, 12, 0],
            0,
            36_000,
            0,
            "AEST",
        ),
        (
            "the first reading of an ambiguous local hour",
            "America/New_York",
            [2024, 11, 3, 1, 30],
            0,
            -14_400,
            3_600,
            "EDT",
        ),
        (
            "the second reading of that same hour",
            "America/New_York",
            [2024, 11, 3, 1, 30],
            1,
            -18_000,
            0,
            "EST",
        ),
        (
            "a local time inside the hour a zone skips",
            "America/New_York",
            [2024, 3, 10, 2, 30],
            0,
            -18_000,
            0,
            "EST",
        ),
        (
            "that same skipped time read the other way round",
            "America/New_York",
            [2024, 3, 10, 2, 30],
            1,
            -14_400,
            3_600,
            "EDT",
        ),
    ];
    let case = "a zone's own offset at a moment";
    let asked: Vec<Value> = cases
        .iter()
        .map(|(_, key, when, fold, _, _, _)| json!({"key": key, "when": when, "fold": fold}))
        .collect();
    let event = json!({"case": "offsets", "zones": asked});
    let Some(document) = answered(router, probe, case, &event).await else {
        return 0;
    };
    let answers = document
        .get("answers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        answers.len(),
        cases.len(),
        "case {case}: expected one answer per row, got {document}"
    );
    for (index, (row, _key, _when, _fold, offset, dst, abbreviation)) in cases.iter().enumerate() {
        let answer = answers.get(index).cloned().unwrap_or(Value::Null);
        let expected = json!({
            "utcoffset": offset,
            "dst": dst,
            "tzname": abbreviation,
        });
        assert_eq!(
            answer, expected,
            "case {row}: expected {expected}, got {answer}"
        );
    }
    1
}

/// A conversion in each direction, which is what a zone is for: an instant in UTC read as the
/// wall time of the zone, and a wall time of the zone read back as the instant it is.
///
/// Source of each expected value: the same database rules as the table above — 2024-01-15
/// 12:00 UTC is 07:00 of that winter day in New York (−05:00) and 2024-07-15 12:00 UTC is
/// 08:00 (−04:00).
///
/// Answers how many cases a run answered.
async fn an_instant_converts_to_the_wall_time_of_the_zone(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    // (the case, the zone, the instant in UTC, the wall time it is in that zone)
    let cases: &[(&str, &str, [i64; 5], &str)] = &[
        (
            "an instant in the zone's standard time",
            "America/New_York",
            [2024, 1, 15, 12, 0],
            "2024-01-15T07:00:00-05:00",
        ),
        (
            "an instant in the zone's daylight saving",
            "America/New_York",
            [2024, 7, 15, 12, 0],
            "2024-07-15T08:00:00-04:00",
        ),
        (
            "an instant in a zone east of the meridian",
            "Asia/Kolkata",
            [2024, 1, 15, 12, 0],
            "2024-01-15T17:30:00+05:30",
        ),
        (
            "an instant the day's date differs on",
            "Australia/Sydney",
            [2024, 1, 15, 20, 0],
            "2024-01-16T07:00:00+11:00",
        ),
    ];
    let mut read = 0;
    for (case, key, instant, expected) in cases {
        let event = json!({"case": "converted", "key": key, "when": instant});
        let Some(document) = answered(router, probe, case, &event).await else {
            continue;
        };
        read += 1;
        let local = document
            .get("local")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            local, *expected,
            "case {case}: expected the zone's own wall time {expected}, got {local:?}"
        );
        // ... and the round trip back to the instant it came from, which is the invariant a
        // conversion has to keep.
        let back = document
            .get("back")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let instant = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:00+00:00",
            instant[0], instant[1], instant[2], instant[3], instant[4]
        );
        assert_eq!(
            back, instant,
            "case {case}: expected the wall time to convert back to {instant}, got {back:?}"
        );
    }
    read
}

/// WHICH zones there are: the keys are the database's own, a key it holds none of is refused as
/// the standard library refuses one, and a zone names itself by its key.
///
/// Answers how many cases a run answered.
async fn the_zones_are_the_databases_own(router: &MultiServiceRouter, probe: &str) -> usize {
    let case = "the zones the database holds";
    let Some(document) = answered(router, probe, case, &json!({"case": "zones"})).await else {
        return 0;
    };
    // The database holds the whole IANA list, which is hundreds of zones; the floor is far
    // below it and far above any list that could be written out by hand.
    let count = document.get("count").and_then(Value::as_i64).unwrap_or(0);
    assert!(
        count > 300,
        "case {case}: expected the whole IANA list of zones, got {count}"
    );
    let held = document.get("held").cloned().unwrap_or(Value::Null);
    assert_eq!(
        held,
        json!([true, true, true, false]),
        "case {case}: expected the keys the database holds and the one it does not, got {held}"
    );
    for (member, expected) in [
        // A key the database holds no zone under is refused the way the standard library
        // refuses one, which a program catches as a `KeyError`.
        ("refused", json!("ZoneInfoNotFoundError")),
        ("is_key_error", json!(true)),
        // A zone names itself by its key, and the constructor answers the same object for the
        // same key.
        ("key", json!("Europe/Paris")),
        ("named", json!("Europe/Paris")),
        ("cached", json!(true)),
        // ... and a reading taken now is an aware moment in the zone that was asked for.
        ("now_offset", json!(0)),
        ("now_key", json!("UTC")),
    ] {
        let actual = document.get(member).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case} {member}: expected {expected}, got {actual} in {document}"
        );
    }
    1
}

/// The CONFIGURATION of the installation the run is on, which is the execution environment's
/// own: the release the interpreter states, the machine the function's architecture names, and
/// the directories the runtime keeps its library under.
///
/// Source of each expected value: the layout a managed `python3.x` runtime has — the language
/// installation under `/var/lang`, its library directory named for the runtime identifier the
/// function is configured with — and `sysconfig`'s own documented members.
///
/// Answers how many cases a run answered.
async fn the_configuration_is_the_environments_own(
    router: &MultiServiceRouter,
    probe: &str,
) -> usize {
    let case = "the configuration of this installation";
    let Some(document) = answered(router, probe, case, &json!({"case": "sysconfig"})).await else {
        return 0;
    };
    for (member, expected) in [
        ("version", json!("3.12")),
        ("platform", json!("linux-x86_64")),
        ("prefix", json!("/var/lang")),
        ("stdlib", json!("/var/lang/lib/python3.12")),
        ("purelib", json!("/var/lang/lib/python3.12/site-packages")),
        ("is_python_build", json!(false)),
        ("default_scheme", json!("posix_prefix")),
        ("unknown", Value::Null),
        // The variable the time-zone module reads, which every Linux installation configures.
        (
            "tzpath",
            json!("/usr/share/zoneinfo:/usr/lib/zoneinfo:/usr/share/lib/zoneinfo:/etc/zoneinfo"),
        ),
    ] {
        let actual = document.get(member).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case} {member}: expected {expected}, got {actual} in {document}"
        );
    }
    let names = document
        .get("path_names")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for named in ["stdlib", "purelib", "platlib", "include", "scripts", "data"] {
        assert!(
            names.contains(&json!(named)),
            "case {case} path_names: expected the path {named} an installation has, got {names:?}"
        );
    }
    1
}

/// The probe every table drives: its module BODY imports `zoneinfo` and `sysconfig` — the
/// recorded symptom is one of those imports failing — and its handler answers the surface the
/// event names.
const PROBE_SOURCE: &str = r#"
import datetime
import sysconfig
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError, available_timezones

# The modules above were imported by this module's BODY, so a handler that runs at all is a
# clean import of each.
_IMPORTED = True


def _offsets(event):
    answers = []
    for asked in event.get("zones") or []:
        zone = ZoneInfo(asked["key"])
        year, month, day, hour, minute = asked["when"]
        when = datetime.datetime(
            year, month, day, hour, minute, tzinfo=zone, fold=asked.get("fold", 0)
        )
        answers.append(
            {
                "utcoffset": int(when.utcoffset().total_seconds()),
                "dst": int(when.dst().total_seconds()),
                "tzname": when.tzname(),
            }
        )
    return {"answers": answers}


def _converted(event):
    year, month, day, hour, minute = event["when"]
    instant = datetime.datetime(
        year, month, day, hour, minute, tzinfo=datetime.timezone.utc
    )
    local = instant.astimezone(ZoneInfo(event["key"]))
    return {
        "local": local.isoformat(),
        "back": local.astimezone(datetime.timezone.utc).isoformat(),
    }


def _zones(event):
    keys = available_timezones()
    try:
        ZoneInfo("Nowhere/Nozone")
        refused = ""
        is_key_error = False
    except ZoneInfoNotFoundError as error:
        refused = type(error).__name__
        is_key_error = isinstance(error, KeyError)
    paris = ZoneInfo("Europe/Paris")
    now = datetime.datetime.now(ZoneInfo("UTC"))
    return {
        "count": len(keys),
        "held": [
            "UTC" in keys,
            "America/New_York" in keys,
            "Asia/Tokyo" in keys,
            "Nowhere/Nozone" in keys,
        ],
        "refused": refused,
        "is_key_error": is_key_error,
        "key": paris.key,
        "named": str(paris),
        "cached": ZoneInfo("Europe/Paris") is paris,
        "now_offset": int(now.utcoffset().total_seconds()),
        "now_key": now.tzinfo.key,
    }


def _sysconfig(event):
    paths = sysconfig.get_paths()
    return {
        "imported": _IMPORTED,
        "version": sysconfig.get_python_version(),
        "platform": sysconfig.get_platform(),
        "prefix": sysconfig.get_config_var("prefix"),
        "stdlib": paths["stdlib"],
        "purelib": paths["purelib"],
        "is_python_build": sysconfig.is_python_build(),
        "default_scheme": sysconfig.get_default_scheme(),
        "unknown": sysconfig.get_config_var("NOT_A_CONFIGURED_VARIABLE"),
        "tzpath": sysconfig.get_config_var("TZPATH"),
        "path_names": list(sysconfig.get_path_names()),
    }


_CASES = {
    "offsets": _offsets,
    "converted": _converted,
    "zones": _zones,
    "sysconfig": _sysconfig,
}


def handler(event, context):
    case = (event or {}).get("case")
    answer = _CASES.get(case)
    if answer is None:
        raise AssertionError("the event named no case: %r" % (case,))
    return answer(event or {})
"#;
