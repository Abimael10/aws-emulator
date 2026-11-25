// SPDX-License-Identifier: CC0-1.0

//! The TIME ZONE half of this runtime's standard library: the `zoneinfo` package a Python
//! handler imports, and the IANA time-zone database behind it.
//!
//! A `python3.x` runtime is a promise about an interpreter AND about the standard library that
//! comes with it: `from zoneinfo import ZoneInfo` imports cleanly on the runtime a handler was
//! written for, and `ZoneInfo("America/New_York")` resolves to a zone whose offset at an
//! instant — with its daylight-saving transitions — is the IANA database's answer. A managed
//! runtime carries that database as files of its own image; the interpreter embedded here has
//! no host path to read, so the database is the one this crate compiles in (`chrono-tz`, the
//! same copy a JavaScript run's `Intl` and local `Date` readings stand on), reached through the
//! host seam.
//!
//! What is this module's own:
//!
//! * the `zoneinfo` package — `ZoneInfo`, `ZoneInfoNotFoundError`, `available_timezones`,
//!   `TZPATH` and `reset_tzpath` — with the `datetime.tzinfo` contract a program relies on: the
//!   offset, the daylight-saving part of it and the abbreviation a zone has AT a given local
//!   time, the ambiguous and nonexistent local times of a transition resolved by `fold` as PEP
//!   495 states, and the conversion of an instant in UTC to that zone's own wall time;
//! * nothing about the zones themselves: every answer is read out of the compiled-in database,
//!   so a zone's rules are that database's and no offset is written out by hand here.
//!
//! A zone is named by its IANA KEY, which is the only way this runtime names one: there are no
//! time-zone files to read, so `TZPATH` is empty and `ZoneInfo.from_file` has no file to take.

use ::std::time::Duration;

use ::chrono::{MappedLocalTime, NaiveDate, NaiveDateTime, Offset, TimeDelta, TimeZone};
use ::chrono_tz::{OffsetComponents, OffsetName, Tz, TZ_VARIANTS};
use ::serde_json::{json, Value};

use super::package::PackageModule;

/// The modules of the standard library this half of the runtime supplies: the `zoneinfo`
/// package.
///
/// Resolved before the frozen standard library and after the deployment package's own entries,
/// so this IS the `zoneinfo` a handler imports, and a package shipping its own still wins.
#[must_use]
pub fn runtime_stdlib_modules() -> Vec<PackageModule> {
    vec![PackageModule {
        path: "zoneinfo/__init__.py".to_string(),
        source: ZONEINFO_SOURCE.to_string(),
    }]
}

/// How far from a nonexistent local time the offset either side of the gap is read.
///
/// A local time inside a daylight-saving GAP never happened, so the database has no offset for
/// it: PEP 495 resolves it by `fold`, to the offset in force before the transition or the one
/// after it. Each of those is read a day away from the gap, which is past every transition any
/// zone of the database makes (no zone changes offset twice inside a day) and so is inside the
/// span the gap's own transition separates.
const GAP_PROBE: Duration = Duration::from_secs(24 * 60 * 60);

/// Answer ONE call the guest made on this seam: `verb` with `document`.
///
/// `keys` is every zone the database holds; `local` is the offset a zone has at a LOCAL wall
/// time, resolved by `fold`; `utc` is the offset it has at an instant in UTC, with the `fold`
/// the resulting wall time falls in.
#[must_use]
pub fn answered(verb: &str, document: &str) -> String {
    let asked = ::serde_json::from_str::<Value>(document).unwrap_or(Value::Null);
    let answered = match verb {
        "keys" => Ok(json!({ "keys": keys() })),
        "local" => at_local_time(&asked),
        "utc" => at_instant(&asked),
        other => Err(format!("{other} is not a call a handler makes on a time zone")),
    };
    match answered {
        Ok(answer) => answer.to_string(),
        Err(detail) => json!({ "error": detail }).to_string(),
    }
}

/// Every zone KEY the compiled-in database holds, in the order it lists them.
fn keys() -> Vec<String> {
    TZ_VARIANTS
        .iter()
        .map(|zone| zone.name().to_string())
        .collect()
}

/// The zone the call named, or the sentence saying the database holds none of that name.
fn zone_of(asked: &Value) -> Result<Tz, String> {
    let key = asked.get("key").and_then(Value::as_str).unwrap_or_default();
    key.parse::<Tz>()
        .map_err(|_| format!("No time zone found with key {key}"))
}

/// The naive date and time the call named, read from its own members.
fn when_of(asked: &Value) -> Result<NaiveDateTime, String> {
    let part = |name: &str| asked.get(name).and_then(Value::as_i64).unwrap_or_default();
    let date = NaiveDate::from_ymd_opt(
        i32::try_from(part("year")).unwrap_or_default(),
        u32::try_from(part("month")).unwrap_or_default(),
        u32::try_from(part("day")).unwrap_or_default(),
    );
    date.and_then(|date| {
        date.and_hms_opt(
            u32::try_from(part("hour")).unwrap_or_default(),
            u32::try_from(part("minute")).unwrap_or_default(),
            u32::try_from(part("second")).unwrap_or_default(),
        )
    })
    .ok_or_else(|| "the call named no date and time".to_string())
}

/// The offset a zone has at the LOCAL wall time the call named.
///
/// An AMBIGUOUS wall time — the hour a zone repeats when it leaves daylight saving — is the
/// first occurrence for `fold` 0 and the second for `fold` 1; a NONEXISTENT one, inside the gap
/// a zone skips when it enters daylight saving, is the offset before the transition for `fold`
/// 0 and the one after it for `fold` 1. That is PEP 495's own reading, which is what the
/// standard library's `zoneinfo` performs.
fn at_local_time(asked: &Value) -> Result<Value, String> {
    let zone = zone_of(asked)?;
    let when = when_of(asked)?;
    let folded = asked
        .get("fold")
        .and_then(Value::as_i64)
        .unwrap_or_default()
        != 0;
    let offset = match zone.offset_from_local_datetime(&when) {
        MappedLocalTime::Single(offset) => offset,
        MappedLocalTime::Ambiguous(earlier, later) => {
            if folded {
                later
            } else {
                earlier
            }
        }
        MappedLocalTime::None => {
            let away = TimeDelta::from_std(GAP_PROBE).unwrap_or_default();
            let probe = if folded {
                when.checked_add_signed(away)
            } else {
                when.checked_sub_signed(away)
            }
            .unwrap_or(when);
            zone.offset_from_utc_datetime(&probe)
        }
    };
    Ok(reported(&offset))
}

/// The offset a zone has at the instant in UTC the call named, and which `fold` the wall time
/// that instant falls on is in.
///
/// This is what converts an instant to a zone's own wall time: the offset at that instant, and
/// whether the wall time it lands on is the SECOND occurrence of an ambiguous hour — which is
/// `fold` 1, as PEP 495 states it.
fn at_instant(asked: &Value) -> Result<Value, String> {
    let zone = zone_of(asked)?;
    let when = when_of(asked)?;
    let offset = zone.offset_from_utc_datetime(&when);
    let shift = i64::from(offset.fix().local_minus_utc());
    let local = when
        .checked_add_signed(TimeDelta::try_seconds(shift).unwrap_or_default())
        .unwrap_or(when);
    // The wall time is the second occurrence of an ambiguous hour when the offset in force at
    // the instant is the LATER of the two that wall time could stand in.
    let folded = match zone.offset_from_local_datetime(&local) {
        MappedLocalTime::Ambiguous(_earlier, later) => {
            later.fix().local_minus_utc() == offset.fix().local_minus_utc()
        }
        _ => false,
    };
    let mut answer = reported(&offset);
    if let Value::Object(members) = &mut answer {
        members.insert("fold".to_string(), json!(i64::from(folded)));
    }
    Ok(answer)
}

/// What one zone reading answers: the whole offset from UTC, the daylight-saving part of it and
/// the abbreviation the zone goes by, each in the form the `tzinfo` contract gives it.
fn reported(offset: &::chrono_tz::TzOffset) -> Value {
    json!({
        "utcoffset": i64::from(offset.fix().local_minus_utc()),
        "dst": offset.dst_offset().num_seconds(),
        "tzname": offset.abbreviation().unwrap_or_default(),
    })
}

/// `zoneinfo`: the package a program imports to name an IANA time zone.
const ZONEINFO_SOURCE: &str = r#""""The IANA time zones this runtime ships, as the standard library names them."""

import datetime as _datetime
import json as _json
import time as _time

_host = __import__("_lambda_host")

__all__ = [
    "ZoneInfo",
    "ZoneInfoNotFoundError",
    "InvalidTZPathWarning",
    "TZPATH",
    "available_timezones",
    "reset_tzpath",
]

# The DIRECTORIES a zone is read from. This runtime reads none: the time-zone database is the
# one it ships, reached by key, so there is no path to search and nothing a path could add.
TZPATH = ()


class ZoneInfoNotFoundError(KeyError):
    """Raised for a key the time-zone database holds no zone under."""


class InvalidTZPathWarning(RuntimeWarning):
    """Raised for an entry of ``PYTHONTZPATH`` that is not an absolute path."""


def reset_tzpath(to=None):
    """Set the directories a zone is read from, of which this runtime reads none."""
    global TZPATH
    TZPATH = tuple(to or ())


def available_timezones():
    """Every zone key this runtime's time-zone database holds."""
    return set(_json.loads(_host.zone("keys", "{}"))["keys"])


def _call(verb, document):
    answered = _json.loads(_host.zone(verb, _json.dumps(document)))
    failed = answered.get("error")
    if failed:
        raise ZoneInfoNotFoundError(failed)
    return answered


def _asked(key, when, fold=0):
    return {
        "key": key,
        "year": when.year,
        "month": when.month,
        "day": when.day,
        "hour": when.hour,
        "minute": when.minute,
        "second": when.second,
        "fold": fold,
    }


class ZoneInfo(_datetime.tzinfo):
    """One IANA time zone, by the key the database holds it under.

    Every offset, daylight-saving reading and abbreviation is the DATABASE's answer for the
    moment it is asked about, so a zone's transitions are the ones it really has.
    """

    # The instances handed out so far, keyed by zone key: ``ZoneInfo(key)`` answers the same
    # object for the same key, as the standard library's own constructor does.
    _held = {}

    def __new__(cls, key):
        key = str(key)
        held = cls._held.get(key)
        if held is not None:
            return held
        made = cls.no_cache(key)
        cls._held[key] = made
        return made

    @classmethod
    def no_cache(cls, key):
        """One zone, not shared with the instances the constructor hands out."""
        key = str(key)
        made = _datetime.tzinfo.__new__(cls)
        # Asked for once, here, so a key the database holds no zone under is refused where the
        # zone is named rather than later where it is read.
        _call("local", _asked(key, _EPOCH))
        made._key = key
        return made

    @classmethod
    def from_file(cls, file_object, key=None):
        """Read a zone from a TZif file, of which this runtime has none."""
        raise ValueError(
            "this runtime reads its time zones from the database it ships, by key, and has "
            "no time-zone files to read one from"
        )

    @classmethod
    def clear_cache(cls, only_keys=None):
        """Forget the instances handed out, so the next call builds them again."""
        if only_keys is None:
            cls._held.clear()
            return
        for key in only_keys:
            cls._held.pop(str(key), None)

    @property
    def key(self):
        return self._key

    def _at(self, dt):
        """What the database answers for the wall time ``dt`` stands at in this zone."""
        if dt is None:
            # No moment was named, so the reading is the one in force now: the zone's offset
            # at this instant.
            now = _EPOCH + _datetime.timedelta(seconds=_time.time())
            return _call("utc", _asked(self._key, now))
        return _call("local", _asked(self._key, dt, getattr(dt, "fold", 0)))

    def utcoffset(self, dt):
        return _datetime.timedelta(seconds=self._at(dt)["utcoffset"])

    def dst(self, dt):
        return _datetime.timedelta(seconds=self._at(dt)["dst"])

    def tzname(self, dt):
        return self._at(dt)["tzname"] or None

    def fromutc(self, dt):
        """The wall time in this zone of the instant ``dt`` states in UTC."""
        if not isinstance(dt, _datetime.datetime):
            raise TypeError("fromutc() requires a datetime argument")
        if dt.tzinfo is not self:
            raise ValueError("fromutc() dt.tzinfo is not self")
        answered = _call("utc", _asked(self._key, dt))
        moved = dt + _datetime.timedelta(seconds=answered["utcoffset"])
        return moved.replace(fold=answered.get("fold", 0))

    def __str__(self):
        return self._key

    def __repr__(self):
        return "%s(key=%r)" % (type(self).__name__, self._key)

    def __reduce__(self):
        return (type(self)._unpickle, (self._key,))

    @classmethod
    def _unpickle(cls, key):
        return cls(key)


# The instant a zone is checked to EXIST at, which is the start of the epoch every reading of
# the database is stated against.
_EPOCH = _datetime.datetime(1970, 1, 1)
"#;
