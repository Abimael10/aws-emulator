// SPDX-License-Identifier: CC0-1.0

//! `dateutil`: the DATE AND TIME library the AWS SDK for Python is installed with, which a
//! managed `python3.x` runtime therefore carries on a handler's import path.
//!
//! Every timestamp the SDK reads off the wire is parsed with it and carried in one of its
//! zones, so the SDK's installation requires this distribution, a managed runtime preinstalls
//! it, and a deployment package that bundles the SDK itself but not its dependencies still
//! imports: the package's copy of `boto3` and `botocore` wins over the runtime's, while
//! `import dateutil` resolves from the runtime behind it.
//!
//! What these modules do and do not do:
//!
//! * A timestamp is really PARSED: the text is read into the fields it states, in the forms
//!   AWS's own protocols carry a timestamp in (the ISO 8601 forms, the HTTP date of RFC 9110
//!   and the two older forms it names, and the date-only forms), and a text no form admits is
//!   the library's own `ParserError`. Nothing is matched against one caller's string and
//!   nothing is guessed at: an offset the text states is the offset of the answer.
//! * A ZONE is a real `tzinfo`: the fixed offsets (`tzutc`, `tzoffset`), the environment's own
//!   (`tzlocal`) and a named one (`gettz`, over the zone database this runtime's standard
//!   library serves), so a datetime carried in one converts and compares the way the standard
//!   library's own do.
//! * The SURFACE is the library's: `parser.parse`, `parser.isoparse`, `parser.parserinfo`,
//!   the zones above, `relativedelta` and the small `utils` beside them, under the names the
//!   published library exports them — so the SDK and a handler written against it run
//!   unchanged.

/// The modules of the date and time library, under the paths the package holds them at.
#[must_use]
pub(super) fn modules() -> Vec<(&'static str, &'static str)> {
    vec![
        ("dateutil/__init__.py", INIT_SOURCE),
        ("dateutil/_version.py", VERSION_SOURCE),
        ("dateutil/tz/__init__.py", TZ_INIT_SOURCE),
        ("dateutil/tz/tz.py", TZ_SOURCE),
        ("dateutil/parser/__init__.py", PARSER_INIT_SOURCE),
        ("dateutil/parser/_parser.py", PARSER_SOURCE),
        ("dateutil/relativedelta.py", RELATIVEDELTA_SOURCE),
        ("dateutil/utils.py", UTILS_SOURCE),
    ]
}

/// `dateutil`: the package the zones, the parser and the deltas live under.
const INIT_SOURCE: &str = r#"
"""The date and time library the AWS SDK for Python is installed with."""

from dateutil._version import __version__

__all__ = ["parser", "relativedelta", "tz", "utils"]
"#;

/// `dateutil._version`: the generation these modules implement.
const VERSION_SOURCE: &str = r#"
"""The generation of the library these modules implement."""

# It is not a published release of the library: no release of one is here to name, so this
# names the generation and nothing finer.
__version__ = "2.9.0"
version = __version__
"#;

/// `dateutil.tz`: the names the zone package exports.
const TZ_INIT_SOURCE: &str = r#"
"""The zones a datetime of this library is carried in."""

from dateutil.tz.tz import (
    UTC,
    datetime_ambiguous,
    datetime_exists,
    enfold,
    gettz,
    resolve_imaginary,
    tzfile,
    tzlocal,
    tzoffset,
    tzstr,
    tzutc,
)

__all__ = [
    "UTC",
    "datetime_ambiguous",
    "datetime_exists",
    "enfold",
    "gettz",
    "tzfile",
    "tzlocal",
    "tzoffset",
    "tzstr",
    "tzutc",
    "resolve_imaginary",
]
"#;

/// `dateutil.tz.tz`: the zones themselves.
const TZ_SOURCE: &str = r#"
"""The zones a datetime of this library is carried in.

Each of them is a real `tzinfo`: what it answers for an offset, a name and a daylight
difference is what the standard library's own arithmetic and comparison use, so a datetime
carried in one of these converts exactly as one carried in a standard-library zone does.
"""

import datetime
import time

ZERO = datetime.timedelta(0)
EPOCH = datetime.datetime(1970, 1, 1, 0, 0)
EPOCHORDINAL = EPOCH.toordinal()


class tzutc(datetime.tzinfo):
    """Coordinated Universal Time, which is the zone AWS carries a timestamp in."""

    def utcoffset(self, dt):
        return ZERO

    def dst(self, dt):
        return ZERO

    def tzname(self, dt):
        return "UTC"

    def is_ambiguous(self, dt):
        """Whether `dt` is a time this zone has twice, which UTC never has."""
        return False

    def fromutc(self, dt):
        if dt.tzinfo is not None and dt.tzinfo is not self:
            raise ValueError("fromutc: dt.tzinfo is not self")
        return dt

    def __eq__(self, other):
        # Any zone whose offset is zero and that has no daylight difference IS this zone,
        # which is what the published library compares.
        if not isinstance(other, datetime.tzinfo):
            return NotImplemented
        if isinstance(other, (tzutc, tzoffset)):
            return other.utcoffset(None) == ZERO
        if other is datetime.timezone.utc:
            return True
        try:
            return other.utcoffset(None) == ZERO and (other.dst(None) or ZERO) == ZERO
        except Exception:
            return False

    def __ne__(self, other):
        equal = self.__eq__(other)
        if equal is NotImplemented:
            return NotImplemented
        return not equal

    def __hash__(self):
        return hash(ZERO)

    def __repr__(self):
        return "%s()" % self.__class__.__name__

    __reduce__ = object.__reduce__


UTC = tzutc()


class tzoffset(datetime.tzinfo):
    """A zone that is one FIXED offset from UTC, under the name it states."""

    def __init__(self, name, offset):
        self._name = name
        if isinstance(offset, datetime.timedelta):
            self._offset = offset
        else:
            self._offset = datetime.timedelta(seconds=offset)

    def utcoffset(self, dt):
        return self._offset

    def dst(self, dt):
        return ZERO

    def is_ambiguous(self, dt):
        return False

    def tzname(self, dt):
        return self._name

    def fromutc(self, dt):
        return dt + self._offset

    def __eq__(self, other):
        if not isinstance(other, (tzoffset, tzutc)):
            return NotImplemented
        return self._offset == other.utcoffset(None)

    def __ne__(self, other):
        equal = self.__eq__(other)
        if equal is NotImplemented:
            return NotImplemented
        return not equal

    def __hash__(self):
        return hash(self._offset)

    def __repr__(self):
        return "%s(%s, %s)" % (
            self.__class__.__name__,
            repr(self._name),
            int(self._offset.total_seconds()),
        )

    @classmethod
    def instance(cls, name, offset):
        """One zone per name and offset, as the published library hands them out."""
        return cls(name, offset)

    __reduce__ = object.__reduce__


class tzlocal(datetime.tzinfo):
    """The zone of the ENVIRONMENT this code is running in, as it reports its own offset."""

    def __init__(self):
        self._std_offset = datetime.timedelta(seconds=-_standard_offset())
        self._dst_offset = datetime.timedelta(seconds=-_daylight_offset())
        self._dst_saved = self._dst_offset - self._std_offset
        self._hasdst = bool(self._dst_saved)
        self._tznames = _zone_names()

    def utcoffset(self, dt):
        if dt is None:
            return None
        if self._isdst(dt):
            return self._dst_offset
        return self._std_offset

    def dst(self, dt):
        if dt is None:
            return None
        if self._isdst(dt):
            return self._dst_offset - self._std_offset
        return ZERO

    def tzname(self, dt):
        return self._tznames[self._isdst(dt)]

    def is_ambiguous(self, dt):
        """Whether `dt` is a time this zone has twice, which only a zone with a daylight
        difference ever has."""
        if not self._hasdst:
            return False
        naive_dst = self._naive_is_dst(dt)
        return naive_dst != self._naive_is_dst(dt - self._dst_saved)

    def _naive_is_dst(self, dt):
        timestamp = _timestamp_of(dt)
        try:
            return time.localtime(timestamp + _standard_offset()).tm_isdst
        except Exception:
            return 0

    def _isdst(self, dt, fold_naive=True):
        if dt is None:
            return False
        if not self._hasdst:
            return False
        return bool(self._naive_is_dst(dt))

    def __eq__(self, other):
        if isinstance(other, tzlocal):
            return (
                self._std_offset == other._std_offset
                and self._dst_offset == other._dst_offset
            )
        if isinstance(other, (tzutc, tzoffset)):
            return not self._hasdst and self._std_offset == other.utcoffset(None)
        return NotImplemented

    def __ne__(self, other):
        equal = self.__eq__(other)
        if equal is NotImplemented:
            return NotImplemented
        return not equal

    def __hash__(self):
        return hash((self._std_offset, self._dst_offset))

    def __repr__(self):
        return "%s()" % self.__class__.__name__

    __reduce__ = object.__reduce__


def _standard_offset():
    """The seconds west of UTC this environment reports for standard time."""
    return int(getattr(time, "timezone", 0) or 0)


def _daylight_offset():
    """The same for daylight time, which is standard time where there is none."""
    if getattr(time, "daylight", 0):
        return int(getattr(time, "altzone", _standard_offset()) or 0)
    return _standard_offset()


def _zone_names():
    """What this environment calls its standard and its daylight time."""
    names = getattr(time, "tzname", None)
    if not names:
        return ("UTC", "UTC")
    if len(names) == 1:
        return (names[0], names[0])
    return (names[0], names[1])


def _timestamp_of(dt):
    """The seconds since the epoch a NAIVE datetime stands for, read without a zone."""
    naive = dt.replace(tzinfo=None)
    return (naive - EPOCH).total_seconds()


class tzstr(tzoffset):
    """A zone stated as the text of a `TZ` value.

    What this runtime reads of such a value is its OFFSET, which is the part a timestamp of
    the wire ever carries; a value naming a zone of the database is better asked for by that
    name (`gettz`).
    """

    def __init__(self, s, posix_offset=False):
        name, offset = _read_tz_string(s)
        tzoffset.__init__(self, name, offset)
        self._s = s

    def __repr__(self):
        return "%s(%s)" % (self.__class__.__name__, repr(self._s))


def _read_tz_string(value):
    """The name and the offset in seconds a `TZ` value states."""
    text = str(value or "UTC").strip()
    name = ""
    index = 0
    while index < len(text) and (text[index].isalpha()):
        name += text[index]
        index += 1
    rest = text[index:]
    if not rest:
        return (name or "UTC", 0)
    sign = -1
    if rest[0] == "-":
        sign = 1
        rest = rest[1:]
    elif rest[0] == "+":
        rest = rest[1:]
    parts = rest.split(":")
    try:
        hours = int(parts[0] or 0)
    except ValueError:
        return (name or "UTC", 0)
    minutes = int(parts[1]) if len(parts) > 1 and parts[1].isdigit() else 0
    seconds = int(parts[2]) if len(parts) > 2 and parts[2].isdigit() else 0
    # A `TZ` offset is stated as the time to ADD to local time to reach UTC, so its sign is
    # the opposite of the zone's own offset.
    return (name or "UTC", sign * (hours * 3600 + minutes * 60 + seconds))


class tzfile(datetime.tzinfo):
    """A zone of the zone DATABASE, reached by name.

    The database is the one this runtime's standard library serves, so a name it holds is a
    real zone with its own transitions and a name it does not hold is no zone at all.
    """

    def __init__(self, name):
        self._name = str(name)
        self._zone = _database_zone(self._name)
        if self._zone is None:
            raise ValueError("unknown time zone: %s" % self._name)

    def utcoffset(self, dt):
        return self._zone.utcoffset(dt)

    def dst(self, dt):
        return self._zone.dst(dt)

    def tzname(self, dt):
        return self._zone.tzname(dt)

    def fromutc(self, dt):
        return self._zone.fromutc(dt.replace(tzinfo=self._zone)).replace(tzinfo=self)

    def __eq__(self, other):
        if isinstance(other, tzfile):
            return self._name == other._name
        return NotImplemented

    def __ne__(self, other):
        equal = self.__eq__(other)
        if equal is NotImplemented:
            return NotImplemented
        return not equal

    def __hash__(self):
        return hash(self._name)

    def __repr__(self):
        return "%s(%s)" % (self.__class__.__name__, repr(self._name))


def _database_zone(name):
    """The zone the standard library's own database holds under `name`, or `None`."""
    try:
        import zoneinfo
    except ImportError:
        return None
    try:
        return zoneinfo.ZoneInfo(name)
    except Exception:
        return None


def gettz(name=None):
    """The zone `name` names: the environment's own where nothing is named, UTC for the
    universal one, and the zone of the database otherwise — or `None` for a name no zone is
    held under."""
    if name is None:
        held = _environment_zone()
        if held is None:
            return tzlocal()
        name = held
    text = str(name)
    if text.upper() in ("UTC", "UNIVERSAL", "GMT", "Z", "ZULU", "GMT0", "UCT"):
        return UTC
    zone = _database_zone(text)
    if zone is not None:
        return tzfile(text)
    if any(character.isdigit() for character in text):
        return tzstr(text)
    return None


def _environment_zone():
    """What the environment states its own zone as, or `None` where it states none."""
    import os

    return os.environ.get("TZ") or None


def enfold(dt, fold=1):
    """`dt` again, marked as the FIRST or the SECOND time it happened, which is how an
    ambiguous local time is told apart."""
    return dt.replace(fold=fold)


def datetime_exists(dt, tz=None):
    """Whether `dt` is a time its zone really has, which a time inside a spring-forward gap
    is not."""
    if tz is None:
        if dt.tzinfo is None:
            raise ValueError("Datetime is naive and no time zone provided.")
        tz = dt.tzinfo
    dt = dt.replace(tzinfo=None)
    return dt == dt.replace(tzinfo=tz).astimezone(UTC).astimezone(tz).replace(tzinfo=None)


def datetime_ambiguous(dt, tz=None):
    """Whether `dt` is a time its zone has TWICE, which a time inside a fall-back repetition
    is."""
    if tz is None:
        if dt.tzinfo is None:
            raise ValueError("Datetime is naive and no time zone provided.")
        tz = dt.tzinfo
    is_ambiguous = getattr(tz, "is_ambiguous", None)
    if is_ambiguous is not None:
        try:
            return is_ambiguous(dt)
        except Exception:
            pass
    # Without the zone's own answer, a time is ambiguous when the two foldings of it stand
    # for different instants.
    first = dt.replace(fold=0, tzinfo=tz)
    second = dt.replace(fold=1, tzinfo=tz)
    return first.utcoffset() != second.utcoffset()


def resolve_imaginary(dt):
    """`dt` moved past the gap its zone has, where it falls inside one."""
    if dt.tzinfo is not None and not datetime_exists(dt):
        curr_offset = (dt + datetime.timedelta(hours=24)).utcoffset()
        old_offset = (dt - datetime.timedelta(hours=24)).utcoffset()
        dt += curr_offset - old_offset
    return dt
"#;

/// `dateutil.parser`: the names the parser package exports.
const PARSER_INIT_SOURCE: &str = r#"
"""Reading a timestamp out of the text that states it."""

from dateutil.parser._parser import (
    DEFAULTPARSER,
    ParserError,
    UnknownTimezoneWarning,
    isoparse,
    isoparser,
    parse,
    parser,
    parserinfo,
)

__all__ = [
    "parse",
    "parser",
    "parserinfo",
    "isoparse",
    "isoparser",
    "ParserError",
    "UnknownTimezoneWarning",
]
"#;

/// `dateutil.parser._parser`: the parse itself.
const PARSER_SOURCE: &str = r#"
"""Reading a timestamp out of the text that states it.

The text is read into the FIELDS it states, in the forms a timestamp is written in on the
wire: the ISO 8601 forms (with or without a zone, with `Z` or an offset, to any precision),
the HTTP date and the two older forms beside it, and the date-only forms. The offset a text
states is the offset of the answer; a text that states none is naive, or carries the zone the
caller asked for.
"""

import datetime
import re

from dateutil.tz import UTC, tzoffset, tzutc


class ParserError(ValueError):
    """The text is no timestamp this library can read."""

    def __str__(self):
        try:
            return self.args[0] % self.args[1:]
        except (TypeError, IndexError):
            return ValueError.__str__(self)


class UnknownTimezoneWarning(RuntimeWarning):
    """The text named a zone this library holds no offset for."""


# The named zones a timestamp of the wire states, as the offsets they are.
#
# Source: the zone abbreviations RFC 9110 admits in an HTTP date (`GMT`), beside the
# universal spellings and the military `Z`.
TZ_ABBREVIATIONS = {
    "UTC": 0,
    "GMT": 0,
    "UT": 0,
    "Z": 0,
    "EST": -5 * 3600,
    "EDT": -4 * 3600,
    "CST": -6 * 3600,
    "CDT": -5 * 3600,
    "MST": -7 * 3600,
    "MDT": -6 * 3600,
    "PST": -8 * 3600,
    "PDT": -7 * 3600,
}

# The forms a timestamp is written in, each read with the standard library's own reader.
#
# Source: the ISO 8601 forms AWS's protocols carry a timestamp in, the HTTP date of RFC 9110
# ("Tue, 02 Jan 2024 03:04:05 GMT") and the two older forms it names (the RFC 850 date and the
# ASCII `asctime` one), and the date-only forms a caller writes.
_FORMATS = [
    "%Y-%m-%dT%H:%M:%S.%f%z",
    "%Y-%m-%dT%H:%M:%S%z",
    "%Y-%m-%dT%H:%M:%S.%f",
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%dT%H:%M",
    "%Y-%m-%d %H:%M:%S.%f%z",
    "%Y-%m-%d %H:%M:%S%z",
    "%Y-%m-%d %H:%M:%S.%f",
    "%Y-%m-%d %H:%M:%S",
    "%Y-%m-%d %H:%M",
    "%Y-%m-%d",
    "%Y%m%dT%H%M%S%z",
    "%Y%m%dT%H%M%S",
    "%Y%m%d",
    "%a, %d %b %Y %H:%M:%S",
    "%A, %d-%b-%y %H:%M:%S",
    "%a %b %d %H:%M:%S %Y",
    "%d %b %Y %H:%M:%S",
    "%d %B %Y",
    "%b %d %Y",
    "%B %d, %Y",
    "%m/%d/%Y %H:%M:%S",
    "%m/%d/%Y",
    "%d.%m.%Y",
    "%H:%M:%S",
    "%H:%M",
]

# A trailing zone NAME rather than an offset, which the forms above do not carry.
_TRAILING_ZONE = re.compile(r"\s*\(?\b([A-Za-z]{1,5})\b\)?$")


class parserinfo:
    """What a parse takes as given: whether a bare date is day-first or year-first, and the
    century a two-digit year is read into."""

    def __init__(self, dayfirst=False, yearfirst=False):
        self.dayfirst = dayfirst
        self.yearfirst = yearfirst
        self._year = datetime.datetime.now().year
        self._century = self._year // 100 * 100

    def convertyear(self, year, century_specified=False):
        if not century_specified and year < 100:
            year += self._century
            if abs(year - self._year) >= 50:
                if year < self._year:
                    year += 100
                else:
                    year -= 100
        return year

    def validate(self, result):
        return True


class parser:
    """Reads one timestamp out of the text that states it."""

    def __init__(self, info=None):
        self.info = info if info is not None else parserinfo()

    def parse(
        self,
        timestr,
        default=None,
        ignoretz=False,
        tzinfos=None,
        **kwargs
    ):
        """The datetime `timestr` states.

        `default` is where the fields the text does not state come from, which is today at
        midnight when the caller names none; `ignoretz` drops the zone the text states, and
        `tzinfos` is what a zone NAME it states resolves through.
        """
        if isinstance(timestr, bytes):
            timestr = timestr.decode("ascii", "ignore")
        if not isinstance(timestr, str):
            raise TypeError("Parser must be a string or character stream, not %s" % type(timestr).__name__)
        text = timestr.strip()
        if not text:
            raise ParserError("String does not contain a date: %s", timestr)
        if default is None:
            today = datetime.datetime.now()
            default = datetime.datetime(today.year, today.month, today.day)
        parsed, zone_name, stated_date = self._parse_text(text)
        if parsed is None:
            raise ParserError("Unknown string format: %s", timestr)
        if not stated_date:
            parsed = self._fill_from(parsed, default)
        if zone_name is not None and parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=self._zone_named(zone_name, tzinfos))
        if ignoretz:
            return parsed.replace(tzinfo=None)
        return parsed

    def _parse_text(self, text):
        """The datetime the text states, the zone NAME it named if any, and whether it stated
        a DATE at all — a text that states only a time states none."""
        # The ISO forms first, which is what the wire carries: the standard library's own
        # reader takes every precision, and the universal zone is written both ways — as `Z`
        # and as the offset it stands for, which is the spelling every reader below takes.
        candidate = text[:-1] + "+00:00" if text.endswith(("Z", "z")) else text
        try:
            return (datetime.datetime.fromisoformat(candidate), None, True)
        except ValueError:
            pass
        for form in _FORMATS:
            try:
                return (
                    datetime.datetime.strptime(candidate, form),
                    None,
                    _states_date(form),
                )
            except ValueError:
                continue
        # A text whose zone is a NAME rather than an offset: the name is taken off, the rest
        # is read, and the name is resolved by the caller's own table or this module's.
        matched = _TRAILING_ZONE.search(text)
        if matched is not None:
            head = text[: matched.start()].strip()
            name = matched.group(1)
            for form in _FORMATS:
                try:
                    return (
                        datetime.datetime.strptime(head, form),
                        name,
                        _states_date(form),
                    )
                except ValueError:
                    continue
            try:
                return (datetime.datetime.fromisoformat(head), name, True)
            except ValueError:
                pass
        return (None, None, False)

    @staticmethod
    def _fill_from(parsed, default):
        """The date the text did not state, taken from `default`, which is what the published
        library reads a bare time against."""
        return parsed.replace(year=default.year, month=default.month, day=default.day)

    @staticmethod
    def _zone_named(name, tzinfos):
        """The zone the name `name` stands for: the caller's own table first, then the
        abbreviations a timestamp of the wire carries."""
        if tzinfos is not None:
            held = None
            if callable(tzinfos):
                held = tzinfos(name, None)
            elif name in tzinfos:
                held = tzinfos[name]
            if isinstance(held, datetime.tzinfo):
                return held
            if isinstance(held, int):
                return tzoffset(name, held)
            if isinstance(held, str):
                return tzoffset(name, _offset_of(held))
        offset = TZ_ABBREVIATIONS.get(name.upper())
        if offset is None:
            import warnings

            warnings.warn(
                "tzname %s identified but not understood." % name,
                UnknownTimezoneWarning,
                stacklevel=2,
            )
            return None
        if offset == 0:
            return tzutc()
        return tzoffset(name, offset)


def _states_date(form):
    """Whether the form `form` reads a DATE rather than only a time of day."""
    return "%Y" in form or "%y" in form


def _offset_of(text):
    """The seconds an offset written as text states."""
    stripped = text.strip()
    sign = -1 if stripped.startswith("-") else 1
    stripped = stripped.lstrip("+-")
    parts = stripped.split(":")
    hours = int(parts[0] or 0)
    minutes = int(parts[1]) if len(parts) > 1 else 0
    return sign * (hours * 3600 + minutes * 60)


DEFAULTPARSER = parser()


def parse(timestr, parserinfo=None, **kwargs):
    """The datetime `timestr` states."""
    if parserinfo is not None:
        return parser(parserinfo).parse(timestr, **kwargs)
    return DEFAULTPARSER.parse(timestr, **kwargs)


class isoparser:
    """Reads a timestamp written in ISO 8601, and nothing else."""

    def __init__(self, sep=None):
        if sep is not None:
            if len(sep) != 1 or ord(sep) >= 128:
                raise ValueError("Separator must be a single, non-numeric ASCII character")
        self._sep = sep

    def isoparse(self, dt_str):
        if isinstance(dt_str, bytes):
            dt_str = dt_str.decode("ascii", "ignore")
        text = str(dt_str).strip()
        if self._sep is not None and self._sep != "T":
            text = text.replace(self._sep, "T", 1)
        candidate = text[:-1] + "+00:00" if text.endswith("Z") else text
        try:
            return datetime.datetime.fromisoformat(candidate)
        except ValueError:
            raise ValueError("Unable to parse date string: %s" % dt_str)

    def parse_isodate(self, datestr):
        return self.isoparse(datestr).date()

    def parse_isotime(self, timestr):
        text = str(timestr).strip()
        candidate = text[:-1] + "+00:00" if text.endswith("Z") else text
        return datetime.time.fromisoformat(candidate)

    def parse_tzstr(self, tzstr, zero_as_utc=True):
        if tzstr in ("Z", "z"):
            return UTC
        seconds = _offset_of(tzstr)
        if seconds == 0 and zero_as_utc:
            return UTC
        return tzoffset(tzstr, seconds)


DEFAULTISOPARSER = isoparser()


def isoparse(dt_str):
    """The datetime an ISO 8601 text states."""
    return DEFAULTISOPARSER.isoparse(dt_str)
"#;

/// `dateutil.relativedelta`: the difference between two datetimes, in the fields a caller
/// states one in.
const RELATIVEDELTA_SOURCE: &str = r#"
"""The difference between two datetimes, in the fields a caller states one in.

A delta of MONTHS or YEARS is not a number of days, so it is held as the field it is and
applied to the date it is added to — a month added to the 31st lands on the last day of the
month that has no 31st, which is what the published library does with one.
"""

import calendar
import datetime


class relativedelta:
    """A difference stated in years, months, days and the time of day, or between two
    datetimes."""

    def __init__(
        self,
        dt1=None,
        dt2=None,
        years=0,
        months=0,
        days=0,
        leapdays=0,
        weeks=0,
        hours=0,
        minutes=0,
        seconds=0,
        microseconds=0,
        year=None,
        month=None,
        day=None,
        weekday=None,
        yearday=None,
        nlyearday=None,
        hour=None,
        minute=None,
        second=None,
        microsecond=None,
    ):
        if dt1 is not None and dt2 is not None:
            self._from_datetimes(dt1, dt2)
        else:
            self.years = years
            self.months = months
            self.days = days + weeks * 7
            self.leapdays = leapdays
            self.hours = hours
            self.minutes = minutes
            self.seconds = seconds
            self.microseconds = microseconds
        self.year = year
        self.month = month
        self.day = day
        self.weekday = weekday
        self.hour = hour
        self.minute = minute
        self.second = second
        self.microsecond = microsecond
        self._normalize()

    def _from_datetimes(self, dt1, dt2):
        """The difference between two datetimes, in whole months and the remainder."""
        if not isinstance(dt1, datetime.date) or not isinstance(dt2, datetime.date):
            raise TypeError("relativedelta only diffs datetime/date")
        if isinstance(dt1, datetime.datetime) != isinstance(dt2, datetime.datetime):
            if not isinstance(dt1, datetime.datetime):
                dt1 = datetime.datetime.fromordinal(dt1.toordinal())
            else:
                dt2 = datetime.datetime.fromordinal(dt2.toordinal())
        self.years = 0
        self.months = (dt1.year - dt2.year) * 12 + (dt1.month - dt2.month)
        self._fix_months(dt1, dt2)
        delta = dt1 - self._add_months(dt2, self.months)
        self.days = delta.days
        self.hours = delta.seconds // 3600
        self.minutes = (delta.seconds // 60) % 60
        self.seconds = delta.seconds % 60
        self.microseconds = delta.microseconds
        self.leapdays = 0

    def _fix_months(self, dt1, dt2):
        """The whole months between two datetimes, pulled back where the remainder would be
        negative."""
        while True:
            moved = self._add_months(dt2, self.months)
            if (self.months >= 0 and moved > dt1) or (self.months < 0 and moved < dt1):
                self.months += -1 if self.months >= 0 else 1
            else:
                return

    @staticmethod
    def _add_months(value, months):
        """`value` moved by whole `months`, kept inside the month it lands in."""
        month = value.month - 1 + months
        year = value.year + month // 12
        month = month % 12 + 1
        day = min(value.day, calendar.monthrange(year, month)[1])
        return value.replace(year=year, month=month, day=day)

    def _normalize(self):
        """The fields carried into one another, so a delta of 25 hours is a day and an
        hour."""
        total = self.months + self.years * 12
        self.years = int(total // 12)
        self.months = int(total - self.years * 12)

    @property
    def weeks(self):
        return int(self.days / 7.0)

    def normalized(self):
        return self

    def __add__(self, other):
        if isinstance(other, relativedelta):
            return relativedelta(
                years=other.years + self.years,
                months=other.months + self.months,
                days=other.days + self.days,
                hours=other.hours + self.hours,
                minutes=other.minutes + self.minutes,
                seconds=other.seconds + self.seconds,
                microseconds=other.microseconds + self.microseconds,
                year=other.year if other.year is not None else self.year,
                month=other.month if other.month is not None else self.month,
                day=other.day if other.day is not None else self.day,
                hour=other.hour if other.hour is not None else self.hour,
                minute=other.minute if other.minute is not None else self.minute,
                second=other.second if other.second is not None else self.second,
                microsecond=(
                    other.microsecond
                    if other.microsecond is not None
                    else self.microsecond
                ),
            )
        if isinstance(other, datetime.timedelta):
            return relativedelta(
                years=self.years,
                months=self.months,
                days=self.days + other.days,
                hours=self.hours,
                minutes=self.minutes,
                seconds=self.seconds + other.seconds,
                microseconds=self.microseconds + other.microseconds,
            )
        if not isinstance(other, datetime.date):
            return NotImplemented
        replaced = {}
        for name in ("year", "month", "day", "hour", "minute", "second", "microsecond"):
            stated = getattr(self, name)
            if stated is not None and hasattr(other, name):
                replaced[name] = stated
        moved = other.replace(**replaced) if replaced else other
        moved = self._add_months(moved, self.years * 12 + self.months)
        return moved + datetime.timedelta(
            days=self.days,
            hours=self.hours,
            minutes=self.minutes,
            seconds=self.seconds,
            microseconds=self.microseconds,
        )

    def __radd__(self, other):
        return self.__add__(other)

    def __rsub__(self, other):
        return self.__neg__().__radd__(other)

    def __sub__(self, other):
        if not isinstance(other, relativedelta):
            return NotImplemented
        return relativedelta(
            years=self.years - other.years,
            months=self.months - other.months,
            days=self.days - other.days,
            hours=self.hours - other.hours,
            minutes=self.minutes - other.minutes,
            seconds=self.seconds - other.seconds,
            microseconds=self.microseconds - other.microseconds,
        )

    def __neg__(self):
        return relativedelta(
            years=-self.years,
            months=-self.months,
            days=-self.days,
            hours=-self.hours,
            minutes=-self.minutes,
            seconds=-self.seconds,
            microseconds=-self.microseconds,
        )

    def __bool__(self):
        return any(
            [
                self.years,
                self.months,
                self.days,
                self.hours,
                self.minutes,
                self.seconds,
                self.microseconds,
                self.year is not None,
                self.month is not None,
                self.day is not None,
                self.hour is not None,
                self.minute is not None,
                self.second is not None,
                self.microsecond is not None,
            ]
        )

    def __mul__(self, other):
        factor = int(other)
        return relativedelta(
            years=self.years * factor,
            months=self.months * factor,
            days=self.days * factor,
            hours=self.hours * factor,
            minutes=self.minutes * factor,
            seconds=self.seconds * factor,
            microseconds=self.microseconds * factor,
        )

    __rmul__ = __mul__

    def __eq__(self, other):
        if not isinstance(other, relativedelta):
            return NotImplemented
        return (
            self.years == other.years
            and self.months == other.months
            and self.days == other.days
            and self.hours == other.hours
            and self.minutes == other.minutes
            and self.seconds == other.seconds
            and self.microseconds == other.microseconds
        )

    def __ne__(self, other):
        equal = self.__eq__(other)
        if equal is NotImplemented:
            return NotImplemented
        return not equal

    def __hash__(self):
        return hash(
            (
                self.years,
                self.months,
                self.days,
                self.hours,
                self.minutes,
                self.seconds,
                self.microseconds,
            )
        )

    def __repr__(self):
        parts = []
        for name in (
            "years",
            "months",
            "days",
            "hours",
            "minutes",
            "seconds",
            "microseconds",
        ):
            value = getattr(self, name)
            if value:
                parts.append("%s=%+d" % (name, value))
        for name in ("year", "month", "day", "hour", "minute", "second", "microsecond"):
            value = getattr(self, name)
            if value is not None:
                parts.append("%s=%s" % (name, repr(value)))
        return "%s(%s)" % (self.__class__.__name__, ", ".join(parts))
"#;

/// `dateutil.utils`: the small calls beside the parser.
const UTILS_SOURCE: &str = r#"
"""The small calls a caller makes beside the parser."""

from datetime import datetime, time

from dateutil.tz import tzlocal


def today(tzinfo=None):
    """Today at midnight, in `tzinfo`."""
    dt = datetime.now(tzinfo)
    return datetime.combine(dt.date(), time(0, tzinfo=tzinfo))


def default_tzinfo(dt, tzinfo):
    """`dt` with `tzinfo` where it carries no zone of its own, and unchanged where it does."""
    if dt.tzinfo is not None:
        return dt
    return dt.replace(tzinfo=tzinfo)


def within_delta(dt1, dt2, delta):
    """Whether two datetimes are no further apart than `delta`."""
    delta = abs(delta)
    difference = dt1 - dt2
    return -delta <= difference <= delta


def now(tzinfo=None):
    """The current time, in `tzinfo` or in the environment's own zone."""
    return datetime.now(tzinfo if tzinfo is not None else tzlocal())
"#;
