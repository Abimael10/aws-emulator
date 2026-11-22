// SPDX-License-Identifier: CC0-1.0

//! `pyspark.parquet`: the Apache Parquet files a Spark read takes in and a Spark write
//! leaves behind.
//!
//! Parquet's layout is a published one (the `parquet-format` specification): the magic bytes
//! `PAR1`, the pages of each column chunk, a Thrift-compact-encoded `FileMetaData` footer,
//! that footer's length and the magic again. This module is that layout, as the ordinary
//! Python the rest of this runtime is written in.
//!
//! # Why it is Python, and not the workspace's Parquet crate
//!
//! The program that writes this output is a CALLER'S, run by the contained interpreter, and
//! the only host surface that interpreter gives guest code is the fixed allow-list of the
//! engine itself (the AWS SDK seam and the few calls beside it). The maintained `parquet`
//! crate other services encode with runs in the host, on the other side of that seam, and
//! widening the seam to reach it would put a host entry point for one file format into the
//! surface every caller-supplied program can call — which is the one thing that allow-list
//! exists to keep closed. So the encoding lives where the engine that produces the rows
//! lives, and the crate stays where it is: the Rust side of this workspace, including the
//! test that READS what this writes back.
//!
//! Every byte of it is COMPUTED from the rows handed over: the pages hold the values
//! themselves, the definition levels say which of them are null, and the footer states the
//! row count, the schema and each chunk's statistics read off those same values. So a write
//! leaves a file any Parquet reader reads, and a read answers the rows a Parquet file really
//! holds — never a body that merely looks like one.
//!
//! # Bounds
//!
//! A file a read takes in is content a caller put in the object store, so neither its size
//! nor the counts its own metadata states may decide how much of this process one read
//! takes: the size is bounded before anything is read ([`MAX_FILE_BYTES`]), the metadata walk
//! carries its own depth ([`MAX_METADATA_DEPTH`]), every count the file states is spent
//! against ONE allowance for the whole read ([`MAX_FILE_VALUES`]), a page is decompressed
//! only as far as it states it holds ([`MAX_PAGE_BYTES`]), and the walk of a chunk's pages is
//! bounded and must make progress ([`MAX_PAGES_PER_CHUNK`]). Past those, the interpreter's own
//! step, memory, recursion and wall-clock ceilings — in host code no program can reach —
//! bound the read as they bound everything else the program does.
//!
//! # What it covers
//!
//! The layout a Spark write of a dataset of rows produces: a flat schema of optional
//! columns, one row group, one data page per column, values in the PLAIN encoding,
//! definition levels in the run-length one, and the page bytes stored as they are or
//! compressed with a codec this runtime can really perform. A file holding anything else —
//! nested groups, a dictionary-encoded column, a codec nothing here decompresses — is NAMED
//! in the failure a read of it raises, rather than being decoded as something it is not.

/// The source of `pyspark/parquet.py`.
pub(super) const PARQUET_SOURCE: &str = r#"
"""The Apache Parquet layout, as a write produces it and a read takes it in."""

import struct

# The Apache Parquet writer this environment's own Spark release ships, which is what stamps
# the footer of a file written here ([`pyspark.release`]); None for an environment nothing is
# known about, whose files are stamped with nothing.
from pyspark.release import PARQUET_WRITER

# The four bytes a Parquet file begins and ends with.
MAGIC = b"PAR1"

# The format version a footer states. Version 1 is the one a flat, PLAIN-encoded file with
# no page-level index needs, and the one every reader of the format reads.
FORMAT_VERSION = 1

# What the root of a schema is named. Spark names it `spark_schema`, and a schema's root name
# is not data: it names the message the columns are fields of.
ROOT_NAME = "spark_schema"

# The physical types of the format (`parquet.thrift`: enum Type).
PHYSICAL_BOOLEAN = 0
PHYSICAL_INT32 = 1
PHYSICAL_INT64 = 2
PHYSICAL_FLOAT = 4
PHYSICAL_DOUBLE = 5
PHYSICAL_BYTE_ARRAY = 6

# The converted types a column may carry (`parquet.thrift`: enum ConvertedType).
CONVERTED_UTF8 = 0
CONVERTED_DATE = 6
CONVERTED_TIMESTAMP_MILLIS = 9
CONVERTED_TIMESTAMP_MICROS = 10

# How often a field occurs (`parquet.thrift`: enum FieldRepetitionType).
REPETITION_REQUIRED = 0
REPETITION_OPTIONAL = 1
REPETITION_REPEATED = 2

# The encodings a page may be in (`parquet.thrift`: enum Encoding).
ENCODING_PLAIN = 0
ENCODING_PLAIN_DICTIONARY = 2
ENCODING_RLE = 3
ENCODING_BIT_PACKED = 4
ENCODING_RLE_DICTIONARY = 8

# The names of the encodings, for the failure a page in one nothing here reads names.
ENCODING_NAMES = {
    ENCODING_PLAIN: "PLAIN",
    ENCODING_PLAIN_DICTIONARY: "PLAIN_DICTIONARY",
    ENCODING_RLE: "RLE",
    ENCODING_BIT_PACKED: "BIT_PACKED",
    ENCODING_RLE_DICTIONARY: "RLE_DICTIONARY",
}

# The compression codecs (`parquet.thrift`: enum CompressionCodec), and the names a write
# asks for them by.
CODEC_UNCOMPRESSED = 0
CODEC_SNAPPY = 1
CODEC_GZIP = 2

CODEC_NAMES = {
    CODEC_UNCOMPRESSED: "UNCOMPRESSED",
    CODEC_SNAPPY: "SNAPPY",
    CODEC_GZIP: "GZIP",
    3: "LZO",
    4: "BROTLI",
    5: "LZ4",
    6: "ZSTD",
    7: "LZ4_RAW",
}

# What a write names a codec by, as Spark's `compression` option spells it, and the suffix
# the part files of a write in it carry — which is how Spark names them. A codec outside this
# mapping is named in the failure rather than written as another one.
CODECS_BY_NAME = {
    "none": (CODEC_UNCOMPRESSED, ""),
    "uncompressed": (CODEC_UNCOMPRESSED, ""),
    "snappy": (CODEC_SNAPPY, ".snappy"),
}

# The kinds of page a column chunk holds (`parquet.thrift`: enum PageType).
PAGE_DATA = 0
PAGE_INDEX = 1
PAGE_DICTIONARY = 2
PAGE_DATA_V2 = 3

# The Thrift compact protocol's type ids, as a field header and a list header carry them.
THRIFT_STOP = 0x00
THRIFT_TRUE = 0x01
THRIFT_FALSE = 0x02
THRIFT_BYTE = 0x03
THRIFT_I16 = 0x04
THRIFT_I32 = 0x05
THRIFT_I64 = 0x06
THRIFT_DOUBLE = 0x07
THRIFT_BINARY = 0x08
THRIFT_LIST = 0x09
THRIFT_SET = 0x0A
THRIFT_MAP = 0x0B
THRIFT_STRUCT = 0x0C

# The day and the microsecond a date and a timestamp are counted from.
EPOCH_DAY_ONE = (1970, 1, 1)

# The BOUNDS a read of a caller-supplied file is held to. A file read here is content a
# caller put in the object store, so neither its size nor the shape of its metadata may
# decide how much of this process one read takes.
#
# How many bytes ONE file a read takes in may hold: the same ceiling one write of this
# filesystem is held to ([`pyspark.filesystem`]), so a read takes in no more than a write
# could have left.
MAX_FILE_BYTES = 64 * 1024 * 1024

# How deeply the structures of the metadata may nest. The footer's own shape is three deep
# (the file, its row groups, their column chunks and each chunk's statistics), so this is
# generous for every file the format defines and bounds the walk of one that nests without
# end — the read answers for such a file rather than recurring on a depth its own bytes
# chose.
MAX_METADATA_DEPTH = 16

# How many VALUES one reading of a metadata structure may materialise. The bytes that are
# left are no bound on this: a list header states how many elements it holds, and two of the
# kinds an element may be (the two booleans) are carried by the header itself and consume no
# byte of their own, so what bounds the walk is this allowance of its own.
MAX_METADATA_VALUES = 100_000

# How many bytes ONE variable-length number may take. Ten is what the widest number the
# protocol carries (an eight-byte integer) needs, so a longer run of them is no number — and
# reading one without this bound is work that grows with the file rather than with the value.
MAX_VARINT_BYTES = 10

# How many values ONE read takes in, across every column of one file: a bound on what one
# read of this process holds, in the one quantity the file's own metadata states.
MAX_FILE_VALUES = 5_000_000

# How many bytes ONE page of a file may hold once its own compression has been undone: a
# page states that size itself, so this is what keeps a page of a few bytes from being
# inflated into a demand for the whole host's memory.
MAX_PAGE_BYTES = 16 * 1024 * 1024

# How many pages ONE column chunk may hold. Every page carries at least one value of the
# chunk, so this is generous for any file the format defines, and it bounds the walk of a
# chunk whose pages carry none.
MAX_PAGES_PER_CHUNK = 4096


class ParquetError(Exception):
    """A Parquet file this runtime cannot write or cannot read, naming why."""


def _varint(value):
    """One unsigned number, in the variable-length encoding Thrift and Parquet use."""
    out = bytearray()
    held = int(value)
    while True:
        if held & ~0x7F == 0:
            out.append(held)
            return bytes(out)
        out.append((held & 0x7F) | 0x80)
        held >>= 7


def _zigzag(value):
    """One signed number, mapped onto an unsigned one the way Thrift's compact form does."""
    held = int(value)
    return (held << 1) ^ (held >> 63)


class _Writer(object):
    """A Thrift compact encoder: a struct's fields, written in field order."""

    def __init__(self):
        self.out = bytearray()
        self._held = []
        self._last = 0

    def bytes(self):
        return bytes(self.out)

    def begin_struct(self):
        self._held.append(self._last)
        self._last = 0

    def end_struct(self):
        self.out.append(THRIFT_STOP)
        self._last = self._held.pop()

    def _field(self, field, kind):
        delta = field - self._last
        if 0 < delta < 16:
            self.out.append((delta << 4) | kind)
        else:
            self.out.append(kind)
            self.out.extend(_varint(_zigzag(field)))
        self._last = field

    def boolean(self, field, value):
        self._field(field, THRIFT_TRUE if value else THRIFT_FALSE)

    def i32(self, field, value):
        self._field(field, THRIFT_I32)
        self.out.extend(_varint(_zigzag(value)))

    def i64(self, field, value):
        self._field(field, THRIFT_I64)
        self.out.extend(_varint(_zigzag(value)))

    def binary(self, field, data):
        self._field(field, THRIFT_BINARY)
        self.out.extend(_varint(len(data)))
        self.out.extend(data)

    def text(self, field, value):
        self.binary(field, value.encode("utf-8"))

    def struct(self, field):
        """Begin a struct-valued field; the caller closes it with `end_struct`."""
        self._field(field, THRIFT_STRUCT)
        self.begin_struct()

    def list_of(self, field, kind, count):
        """Begin a list-valued field of `count` elements of type `kind`."""
        self._field(field, THRIFT_LIST)
        if count < 15:
            self.out.append((count << 4) | kind)
        else:
            self.out.append(0xF0 | kind)
            self.out.extend(_varint(count))

    def element_i32(self, value):
        self.out.extend(_varint(_zigzag(value)))

    def element_binary(self, data):
        self.out.extend(_varint(len(data)))
        self.out.extend(data)


class _Reader(object):
    """A Thrift compact decoder: the values a footer or a page header holds.

    The bytes it reads are a caller's, so the walk is BOUNDED in every quantity the bytes
    themselves state: it never reads past the end of what it was given, it carries the depth
    it is at so a structure that nests without end is answered rather than recurred on
    ([`MAX_METADATA_DEPTH`]), it carries how many values it may still materialise so a header
    declaring elements that consume no bytes is answered too ([`MAX_METADATA_VALUES`]), and
    one number is read from at most the bytes a number takes ([`MAX_VARINT_BYTES`]).
    """

    def __init__(self, body, at=0, depth=0, left=None):
        self._body = body
        self.at = at
        self._depth = depth
        self._left = MAX_METADATA_VALUES if left is None else left

    def _byte(self):
        if self.at >= len(self._body):
            raise ParquetError("the file ends inside its own metadata")
        held = self._body[self.at]
        self.at += 1
        return held

    def _unsigned(self):
        value = 0
        shift = 0
        for _held in range(MAX_VARINT_BYTES):
            byte = self._byte()
            value |= (byte & 0x7F) << shift
            if not byte & 0x80:
                return value
            shift += 7
        raise ParquetError(
            "the metadata of this file states a number longer than the %d bytes one takes"
            % (MAX_VARINT_BYTES,)
        )

    def _signed(self):
        held = self._unsigned()
        return (held >> 1) ^ -(held & 1)

    def _take(self, size):
        if size < 0 or self.at + size > len(self._body):
            raise ParquetError("the file ends inside its own metadata")
        held = self._body[self.at : self.at + size]
        self.at += size
        return bytes(held)

    def _deeper(self):
        """This reader, one level further in — or the failure a metadata too deep raises."""
        if self._depth >= MAX_METADATA_DEPTH:
            raise ParquetError(
                "the metadata of this file nests deeper than the %d levels a read of one "
                "walks" % (MAX_METADATA_DEPTH,)
            )
        self._depth += 1
        return self

    def _shallower(self):
        self._depth -= 1
        return self

    def _spend(self):
        """Take one of the values this reading may still materialise."""
        if self._left <= 0:
            raise ParquetError(
                "the metadata of this file holds more than the %d values a read of one "
                "walks" % (MAX_METADATA_VALUES,)
            )
        self._left -= 1

    def _value(self, kind):
        self._spend()
        if kind == THRIFT_TRUE:
            return True
        if kind == THRIFT_FALSE:
            return False
        if kind == THRIFT_BYTE:
            return self._byte()
        if kind in (THRIFT_I16, THRIFT_I32, THRIFT_I64):
            return self._signed()
        if kind == THRIFT_DOUBLE:
            return struct.unpack("<d", self._take(8))[0]
        if kind == THRIFT_BINARY:
            return self._take(self._unsigned())
        if kind in (THRIFT_LIST, THRIFT_SET):
            return self._list()
        if kind == THRIFT_STRUCT:
            return self.struct()
        raise ParquetError("the metadata holds a value of an unknown kind: %d" % (kind,))

    def _list(self):
        head = self._byte()
        count = head >> 4
        kind = head & 0x0F
        if count == 15:
            count = self._unsigned()
        # A list states how many elements it holds, and every element is one of the values
        # this reading may still materialise — which is the bound, because an element of
        # either boolean kind is carried by the header and consumes no byte of its own.
        if count > self._left:
            raise ParquetError(
                "the metadata of this file states a list of %d elements, and a read of one "
                "walks %d values" % (count, MAX_METADATA_VALUES)
            )
        self._deeper()
        held = [self._value(kind) for _held in range(count)]
        self._shallower()
        return held

    def struct(self):
        """One struct, as the mapping of its field ids to their values."""
        self._deeper()
        held = {}
        field = 0
        while True:
            head = self._byte()
            kind = head & 0x0F
            if kind == THRIFT_STOP:
                self._shallower()
                return held
            delta = head >> 4
            if delta:
                field = field + delta
            else:
                field = self._signed()
            held[field] = self._value(kind)


# --- the compression a page's bytes are stored under -------------------------------------

# A Snappy literal whose length is stored in the four bytes after its tag: the tag's top six
# bits are 59 plus the number of those bytes, as the Snappy format states it.
SNAPPY_LONG_LITERAL = (63 << 2) & 0xFF


def snappy_compressed(data):
    """`data` as a Snappy block: the uncompressed length, then one literal of every byte.

    Snappy's block format is the published one — a variable-length uncompressed length
    followed by tagged elements — and one literal element carrying the bytes themselves IS
    one of its encodings, so this answers a block any Snappy reader decompresses back to
    exactly `data`.
    """
    out = bytearray(_varint(len(data)))
    if data:
        out.append(SNAPPY_LONG_LITERAL)
        out.extend(struct.pack("<I", len(data) - 1))
        out.extend(data)
    return bytes(out)


def snappy_uncompressed(data, ceiling=MAX_PAGE_BYTES):
    """The bytes a Snappy block holds: its literals, and the copies of what came before.

    The block is a caller's, so what it states it holds is checked BEFORE anything is built
    and the bytes built are checked against it as they are built: a block whose elements
    produce more than it states, or more than `ceiling`, is abandoned where it passes that
    bound rather than after the memory has been taken.
    """
    reader = _Reader(data)
    stated = reader._unsigned()
    if stated > ceiling:
        raise ParquetError(
            "a Snappy block states %d bytes, past the %d one page holds"
            % (stated, ceiling)
        )
    at = reader.at
    out = bytearray()
    while at < len(data):
        tag = data[at]
        at += 1
        kind = tag & 0x03
        if kind == 0:
            size = tag >> 2
            if size < 60:
                size += 1
            else:
                count = size - 59
                size = int.from_bytes(data[at : at + count], "little") + 1
                at += count
            if len(out) + size > stated:
                raise ParquetError(
                    "a Snappy block holds more bytes than the %d it states" % (stated,)
                )
            out.extend(data[at : at + size])
            at += size
            continue
        if kind == 1:
            size = 4 + ((tag >> 2) & 0x07)
            offset = ((tag >> 5) << 8) | data[at]
            at += 1
        elif kind == 2:
            size = (tag >> 2) + 1
            offset = int.from_bytes(data[at : at + 2], "little")
            at += 2
        else:
            size = (tag >> 2) + 1
            offset = int.from_bytes(data[at : at + 4], "little")
            at += 4
        if offset <= 0 or offset > len(out):
            raise ParquetError("a Snappy block copies from outside itself")
        if len(out) + size > stated:
            raise ParquetError(
                "a Snappy block holds more bytes than the %d it states" % (stated,)
            )
        for _held in range(size):
            out.append(out[len(out) - offset])
    if len(out) != stated:
        raise ParquetError(
            "a Snappy block states %d bytes and holds %d" % (stated, len(out))
        )
    return bytes(out)


def _compressed(codec, data):
    """`data` as the page bytes a chunk in `codec` stores."""
    if codec == CODEC_UNCOMPRESSED:
        return data
    if codec == CODEC_SNAPPY:
        return snappy_compressed(data)
    raise ParquetError(
        "this runtime writes a Parquet column stored as %s; %s is a codec it cannot perform"
        % ("/".join(sorted(name for name in CODECS_BY_NAME)), CODEC_NAMES.get(codec, codec))
    )


def _uncompressed(codec, data, stated):
    """The page bytes a chunk in `codec` holds, which `stated` of them decompress to.

    What the page STATES it holds is the ceiling the decompression is held to, and that
    ceiling is itself bounded ([`MAX_PAGE_BYTES`]) — so no page can ask one read for more of
    this process's memory than one page may hold.
    """
    if stated < 0 or stated > MAX_PAGE_BYTES:
        raise ParquetError(
            "a page states %d bytes, and one page of a read holds at most %d"
            % (stated, MAX_PAGE_BYTES)
        )
    if codec == CODEC_UNCOMPRESSED:
        if len(data) > stated:
            raise ParquetError(
                "a page holds %d bytes and states %d" % (len(data), stated)
            )
        return data
    if codec == CODEC_SNAPPY:
        held = snappy_uncompressed(data, stated)
        if len(held) != stated:
            raise ParquetError(
                "a page states %d bytes and holds %d" % (stated, len(held))
            )
        return held
    raise ParquetError(
        "a Parquet column compressed with %s is one this runtime cannot decompress"
        % (CODEC_NAMES.get(codec, codec),)
    )


# --- the run-length/bit-packed hybrid the definition levels are encoded in ---------------


def _levels_encoded(levels, width=1):
    """The definition levels, in the run-length/bit-packed hybrid encoding of the format."""
    out = bytearray()
    size = (width + 7) // 8
    index = 0
    count = len(levels)
    while index < count:
        value = levels[index]
        run = 1
        while index + run < count and levels[index + run] == value:
            run += 1
        out.extend(_varint(run << 1))
        out.extend(int(value).to_bytes(size, "little"))
        index += run
    return bytes(out)


def _levels_decoded(data, count, width=1):
    """The `count` definition levels the hybrid encoding in `data` holds.

    `count` is the number of levels the PAGE states, which the read has already bounded, and
    every run a level encoding declares is CLAMPED to what is still needed — so a run length
    of a few bytes cannot drive a list longer than the count it belongs to.
    """
    reader = _Reader(data)
    size = (width + 7) // 8
    levels = []
    while len(levels) < count:
        header = reader._unsigned()
        if header & 1:
            # A bit-packed run: the header states how many groups of eight values follow,
            # and no more of them are read than the count still needs.
            groups = header >> 1
            if groups == 0:
                raise ParquetError("a page's definition levels hold a run of no values")
            groups = min(groups, (count - len(levels) + 7) // 8)
            packed = reader._take(groups * width)
            for index in range(groups * 8):
                if len(levels) >= count:
                    break
                bit = index * width
                held = 0
                for step in range(width):
                    byte = packed[(bit + step) // 8]
                    held |= ((byte >> ((bit + step) % 8)) & 1) << step
                levels.append(held)
            continue
        run = header >> 1
        if run == 0:
            raise ParquetError("a page's definition levels hold a run of no values")
        value = int.from_bytes(reader._take(size), "little")
        for _held in range(min(run, count - len(levels))):
            levels.append(value)
    return levels[:count]


# --- what one column's values are, as the format holds them ------------------------------


class _Kind(object):
    """How one column's values are held: its Parquet type, and how a value becomes bytes."""

    # The spelling a schema of this engine gives the type ([`pyspark.sql.types`]).
    name = "string"
    # The physical type of the format the values are written as.
    physical = PHYSICAL_BYTE_ARRAY
    # The converted type the column carries, where the format has one for it.
    converted = None
    # The field of the format's `LogicalType` union the column carries, where it has one.
    logical = None

    def holds(self, value):
        """Whether `value` is one of this type's own values."""
        raise NotImplementedError

    def read(self, value):
        """`value` as this type holds it, or raise when it is not one of its values."""
        if not self.holds(value):
            raise ParquetError(
                "the value %r is not a %s" % (value, self.name)
            )
        return value

    def plain(self, values):
        """The PLAIN encoding of these values."""
        raise NotImplementedError

    def unplain(self, data, count):
        """The `count` values the PLAIN encoding in `data` holds."""
        raise NotImplementedError

    def statistic(self, value):
        """The bytes one statistic of this type is stated as."""
        return self.plain([value])

    def ordered(self, value):
        """The key the minimum and the maximum of this type are compared on."""
        return value


class _Boolean(_Kind):
    name = "boolean"
    physical = PHYSICAL_BOOLEAN

    def holds(self, value):
        return isinstance(value, bool)

    def plain(self, values):
        out = bytearray()
        for index, value in enumerate(values):
            if index % 8 == 0:
                out.append(0)
            if value:
                out[len(out) - 1] |= 1 << (index % 8)
        return bytes(out)

    def unplain(self, data, count):
        return [
            bool((data[index // 8] >> (index % 8)) & 1) for index in range(count)
        ]

    def statistic(self, value):
        return b"\x01" if value else b"\x00"


class _Long(_Kind):
    name = "bigint"
    physical = PHYSICAL_INT64

    def holds(self, value):
        return isinstance(value, int) and not isinstance(value, bool)

    def plain(self, values):
        try:
            return struct.pack("<%dq" % (len(values),), *values)
        except (struct.error, OverflowError):
            raise ParquetError(
                "a whole number a %s column holds is outside the eight bytes it is held in"
                % (self.name,)
            )

    def unplain(self, data, count):
        return list(struct.unpack("<%dq" % (count,), data[: count * 8]))


class _Integer(_Long):
    name = "int"
    physical = PHYSICAL_INT32

    def plain(self, values):
        try:
            return struct.pack("<%di" % (len(values),), *values)
        except (struct.error, OverflowError):
            raise ParquetError(
                "a whole number a %s column holds is outside the four bytes it is held in"
                % (self.name,)
            )

    def unplain(self, data, count):
        return list(struct.unpack("<%di" % (count,), data[: count * 4]))


class _Double(_Kind):
    name = "double"
    physical = PHYSICAL_DOUBLE

    def holds(self, value):
        return isinstance(value, float) or (
            isinstance(value, int) and not isinstance(value, bool)
        )

    def read(self, value):
        return float(_Kind.read(self, value))

    def plain(self, values):
        return struct.pack("<%dd" % (len(values),), *values)

    def unplain(self, data, count):
        return list(struct.unpack("<%dd" % (count,), data[: count * 8]))


class _Float(_Double):
    name = "float"
    physical = PHYSICAL_FLOAT

    def plain(self, values):
        return struct.pack("<%df" % (len(values),), *values)

    def unplain(self, data, count):
        return list(struct.unpack("<%df" % (count,), data[: count * 4]))


class _String(_Kind):
    name = "string"
    physical = PHYSICAL_BYTE_ARRAY
    converted = CONVERTED_UTF8
    # `LogicalType.STRING`, which is the first field of that union.
    logical = 1

    def holds(self, value):
        return isinstance(value, str)

    def plain(self, values):
        out = bytearray()
        for value in values:
            held = value.encode("utf-8")
            out.extend(struct.pack("<I", len(held)))
            out.extend(held)
        return bytes(out)

    def unplain(self, data, count):
        values = []
        at = 0
        for _held in range(count):
            size = struct.unpack("<I", data[at : at + 4])[0]
            at += 4
            values.append(data[at : at + size].decode("utf-8"))
            at += size
        return values

    def statistic(self, value):
        return value.encode("utf-8")

    def ordered(self, value):
        # The minimum and the maximum of a string column are the byte-wise ones, which is
        # the sort order the format states for this type.
        return value.encode("utf-8")


class _Binary(_String):
    name = "binary"
    converted = None
    logical = None

    def holds(self, value):
        return isinstance(value, (bytes, bytearray))

    def read(self, value):
        return bytes(_Kind.read(self, value))

    def plain(self, values):
        out = bytearray()
        for value in values:
            out.extend(struct.pack("<I", len(value)))
            out.extend(value)
        return bytes(out)

    def unplain(self, data, count):
        values = []
        at = 0
        for _held in range(count):
            size = struct.unpack("<I", data[at : at + 4])[0]
            at += 4
            values.append(bytes(data[at : at + size]))
            at += size
        return values

    def statistic(self, value):
        return bytes(value)

    def ordered(self, value):
        return bytes(value)


class _Date(_Kind):
    name = "date"
    physical = PHYSICAL_INT32
    converted = CONVERTED_DATE

    def holds(self, value):
        import datetime

        return isinstance(value, datetime.date) and not isinstance(
            value, datetime.datetime
        )

    def _days(self, value):
        import datetime

        return (value - datetime.date(*EPOCH_DAY_ONE)).days

    def plain(self, values):
        return struct.pack(
            "<%di" % (len(values),), *[self._days(value) for value in values]
        )

    def unplain(self, data, count):
        import datetime

        held = struct.unpack("<%di" % (count,), data[: count * 4])
        return [
            datetime.date(*EPOCH_DAY_ONE) + datetime.timedelta(days=days)
            for days in held
        ]

    def statistic(self, value):
        return struct.pack("<i", self._days(value))


class _Timestamp(_Kind):
    name = "timestamp"
    physical = PHYSICAL_INT64
    converted = CONVERTED_TIMESTAMP_MICROS

    def holds(self, value):
        import datetime

        return isinstance(value, datetime.datetime)

    def _micros(self, value):
        import datetime

        epoch = datetime.datetime(*EPOCH_DAY_ONE)
        if value.tzinfo is not None:
            epoch = datetime.datetime(
                *EPOCH_DAY_ONE, tzinfo=datetime.timezone.utc
            )
        held = value - epoch
        return (held.days * 86400 + held.seconds) * 1000000 + held.microseconds

    def plain(self, values):
        return struct.pack(
            "<%dq" % (len(values),), *[self._micros(value) for value in values]
        )

    def unplain(self, data, count):
        import datetime

        held = struct.unpack("<%dq" % (count,), data[: count * 8])
        return [
            datetime.datetime(*EPOCH_DAY_ONE) + datetime.timedelta(microseconds=micros)
            for micros in held
        ]

    def statistic(self, value):
        return struct.pack("<q", self._micros(value))


# Every type a column of this engine is written as, by the spelling its schema gives it.
KINDS_BY_NAME = {
    "boolean": _Boolean,
    "bool": _Boolean,
    "tinyint": _Integer,
    "byte": _Integer,
    "smallint": _Integer,
    "short": _Integer,
    "int": _Integer,
    "integer": _Integer,
    "bigint": _Long,
    "long": _Long,
    "float": _Float,
    "real": _Float,
    "double": _Double,
    "string": _String,
    "str": _String,
    "binary": _Binary,
    "date": _Date,
    "timestamp": _Timestamp,
}

# The types a value is read as when the schema states none, in the order a column of mixed
# values widens through: a column holding whole numbers and fractions is a double column, and
# one holding text is a text column.
INFERRED_KINDS = (_Boolean, _Long, _Double, _Date, _Timestamp, _String, _Binary)

# How wide a type is, so that a column holding values of two of them is written as the wider.
WIDENS_TO = {("bigint", "double"): _Double, ("double", "bigint"): _Double}

# The type a column that holds nothing but nulls is written as. The format states no type for
# the absence of values, and text is the type Spark reads such a column back as.
EMPTY_KIND = _String


def _kind_of(column, values, declared):
    """The type column `column` is written as: the one its schema states, or its values'.

    A schema's own type is used when every value the column holds really is one of that
    type's values; otherwise the values decide, because what is written has to be what the
    column holds. A column of two types that do not widen into one is refused naming both,
    the way a schema that cannot be merged is.
    """
    held = [value for value in values if value is not None]
    stated = KINDS_BY_NAME.get(str(declared or "").strip().lower())
    if stated is not None:
        kind = stated()
        if all(kind.holds(value) for value in held):
            return kind
    if not held:
        return EMPTY_KIND()
    kind = None
    for value in held:
        for candidate in INFERRED_KINDS:
            if candidate().holds(value):
                found = candidate
                break
        else:
            raise ParquetError(
                "the column %s holds %r, which is not a value this writer writes"
                % (column, value)
            )
        if kind is None or kind is found:
            kind = found
            continue
        widened = WIDENS_TO.get((kind().name, found().name))
        if widened is None:
            raise ParquetError(
                "the column %s holds both a %s and a %s, which are not one type"
                % (column, kind().name, found().name)
            )
        kind = widened
    return kind()


class _Chunk(object):
    """One written column chunk: where its page is, how big it is, and what it holds."""

    def __init__(self, column, kind, offset, uncompressed, compressed, count, nulls,
                 smallest, largest):
        self.column = column
        self.kind = kind
        self.offset = offset
        self.uncompressed = uncompressed
        self.compressed = compressed
        self.count = count
        self.nulls = nulls
        self.smallest = smallest
        self.largest = largest


def _statistics(writer, field, chunk):
    """The statistics of one chunk: how many nulls it holds, and its bounds.

    The bounds are the pair `parquet.thrift` states them in: `max_value` is the fifth field
    of `Statistics` and `min_value` the sixth.
    """
    writer.struct(field)
    writer.i64(3, chunk.nulls)
    if chunk.smallest is not None:
        writer.binary(5, chunk.kind.statistic(chunk.largest))
        writer.binary(6, chunk.kind.statistic(chunk.smallest))
    writer.end_struct()


def _page_header(chunk, uncompressed, compressed):
    """The header of one data page: its sizes, and what the page itself states."""
    writer = _Writer()
    writer.begin_struct()
    writer.i32(1, PAGE_DATA)
    writer.i32(2, uncompressed)
    writer.i32(3, compressed)
    writer.struct(5)
    writer.i32(1, chunk.count)
    writer.i32(2, ENCODING_PLAIN)
    writer.i32(3, ENCODING_RLE)
    writer.i32(4, ENCODING_RLE)
    _statistics(writer, 5, chunk)
    writer.end_struct()
    writer.end_struct()
    return writer.bytes()


def _schema_element(writer, chunk):
    """One field of the schema: the column, its type, and that it admits nulls."""
    writer.begin_struct()
    writer.i32(1, chunk.kind.physical)
    writer.i32(3, REPETITION_OPTIONAL)
    writer.text(4, chunk.column)
    if chunk.kind.converted is not None:
        writer.i32(6, chunk.kind.converted)
    if chunk.kind.logical is not None:
        # The `LogicalType` union, whose named field is itself an empty struct.
        writer.struct(10)
        writer.struct(chunk.kind.logical)
        writer.end_struct()
        writer.end_struct()
    writer.end_struct()


def _column_chunk(writer, chunk, codec):
    """One column chunk of the row group: where it is, and everything it states."""
    writer.begin_struct()
    writer.i64(2, chunk.offset)
    writer.struct(3)
    writer.i32(1, chunk.kind.physical)
    writer.list_of(2, THRIFT_I32, 2)
    writer.element_i32(ENCODING_PLAIN)
    writer.element_i32(ENCODING_RLE)
    writer.list_of(3, THRIFT_BINARY, 1)
    writer.element_binary(chunk.column.encode("utf-8"))
    writer.i32(4, codec)
    writer.i64(5, chunk.count)
    writer.i64(6, chunk.uncompressed)
    writer.i64(7, chunk.compressed)
    writer.i64(9, chunk.offset)
    _statistics(writer, 12, chunk)
    writer.end_struct()
    writer.end_struct()


def _footer(chunks, count, codec):
    """The file's own metadata: its schema, its rows, and the row group holding them."""
    writer = _Writer()
    writer.begin_struct()
    writer.i32(1, FORMAT_VERSION)
    writer.list_of(2, THRIFT_STRUCT, len(chunks) + 1)
    writer.begin_struct()
    writer.text(4, ROOT_NAME)
    writer.i32(5, len(chunks))
    writer.end_struct()
    for chunk in chunks:
        _schema_element(writer, chunk)
    writer.i64(3, count)
    writer.list_of(4, THRIFT_STRUCT, 1)
    writer.begin_struct()
    writer.list_of(1, THRIFT_STRUCT, len(chunks))
    for chunk in chunks:
        _column_chunk(writer, chunk, codec)
    writer.i64(2, sum(chunk.uncompressed for chunk in chunks))
    writer.i64(3, count)
    writer.end_struct()
    # WHO wrote the file: the Parquet writer this environment's own release ships, which is
    # what a reader of the footer reads a file's statistics against.
    if PARQUET_WRITER:
        writer.text(6, PARQUET_WRITER)
    # The sort order each column's statistics are stated in: the one the type itself
    # defines, which is what `ColumnOrder.TYPE_ORDER` names.
    writer.list_of(7, THRIFT_STRUCT, len(chunks))
    for _held in chunks:
        writer.begin_struct()
        writer.struct(1)
        writer.end_struct()
        writer.end_struct()
    writer.end_struct()
    return writer.bytes()


def codec_of(compression):
    """The (codec, part-file suffix) one `compression` option names."""
    named = str(compression or "snappy").strip().lower()
    held = CODECS_BY_NAME.get(named)
    if held is None:
        raise ParquetError(
            "this runtime writes a Parquet column stored as %s; %r is a codec it cannot "
            "perform" % ("/".join(sorted(CODECS_BY_NAME)), named)
        )
    return held


def file_bytes(columns, rows, declared=None, compression="snappy"):
    """The bytes of one Parquet file holding `rows` under the column names `columns`.

    `rows` are the values of each row in column order, `declared` is the type the dataset's
    own schema states for a column, and `compression` is the codec the pages are stored
    under.
    """
    codec, _suffix = codec_of(compression)
    stated = dict(declared or {})
    body = bytearray(MAGIC)
    chunks = []
    for index, column in enumerate(columns):
        values = [row[index] for row in rows]
        kind = _kind_of(column, values, stated.get(column))
        held = [kind.read(value) for value in values if value is not None]
        ordered = [kind.ordered(value) for value in held]
        page = bytearray()
        levels = _levels_encoded([0 if value is None else 1 for value in values])
        page.extend(struct.pack("<I", len(levels)))
        page.extend(levels)
        page.extend(kind.plain(held))
        smallest = None
        largest = None
        if held:
            smallest = held[ordered.index(min(ordered))]
            largest = held[ordered.index(max(ordered))]
        chunk = _Chunk(
            column,
            kind,
            0,
            0,
            0,
            len(values),
            len(values) - len(held),
            smallest,
            largest,
        )
        stored = _compressed(codec, bytes(page))
        header = _page_header(chunk, len(page), len(stored))
        chunk.offset = len(body)
        chunk.uncompressed = len(header) + len(page)
        chunk.compressed = len(header) + len(stored)
        body.extend(header)
        body.extend(stored)
        chunks.append(chunk)
    footer = _footer(chunks, len(rows), codec)
    body.extend(footer)
    body.extend(struct.pack("<I", len(footer)))
    body.extend(MAGIC)
    return bytes(body)


# --- reading one back --------------------------------------------------------------------


def file_metadata(body):
    """The `FileMetaData` a Parquet file's footer states, as its field mapping.

    The file is a caller's, so its size is bounded before anything is read of it
    ([`MAX_FILE_BYTES`]) and every reading of its metadata is bounded by the end of the
    bytes it was given.
    """
    if len(body) > MAX_FILE_BYTES:
        raise ParquetError(
            "a read takes in a Parquet file of at most %d bytes; this one holds %d"
            % (MAX_FILE_BYTES, len(body))
        )
    if len(body) < 2 * len(MAGIC) + 4:
        raise ParquetError("a file of %d bytes is not a Parquet file" % (len(body),))
    if bytes(body[: len(MAGIC)]) != MAGIC or bytes(body[-len(MAGIC) :]) != MAGIC:
        raise ParquetError("a file not framed by the Parquet magic bytes is not one")
    size = struct.unpack("<I", bytes(body[-8:-4]))[0]
    at = len(body) - 8 - size
    if at < len(MAGIC):
        raise ParquetError("the footer a Parquet file states is longer than the file")
    return _Reader(body, at).struct()


def _leaves(schema):
    """The leaf fields of a footer's schema, in column order, with their repetition."""
    if not schema:
        raise ParquetError("a Parquet file states no schema")
    leaves = []
    for element in schema[1:]:
        if element.get(5):
            raise ParquetError(
                "the column %s of this file is a group, and this runtime reads flat "
                "columns" % (element.get(4, b"").decode("utf-8"),)
            )
        leaves.append(element)
    return leaves


def _kind_read(element):
    """The type one leaf of a footer's schema holds, as this runtime reads it."""
    physical = element.get(1)
    converted = element.get(6)
    if physical == PHYSICAL_BOOLEAN:
        return _Boolean()
    if physical == PHYSICAL_INT32:
        if converted == CONVERTED_DATE:
            return _Date()
        return _Integer()
    if physical == PHYSICAL_INT64:
        if converted in (CONVERTED_TIMESTAMP_MICROS, CONVERTED_TIMESTAMP_MILLIS):
            return _Timestamp()
        return _Long()
    if physical == PHYSICAL_FLOAT:
        return _Float()
    if physical == PHYSICAL_DOUBLE:
        return _Double()
    if physical == PHYSICAL_BYTE_ARRAY:
        if converted == CONVERTED_UTF8 or element.get(10, {}).get(1) is not None:
            return _String()
        return _Binary()
    raise ParquetError(
        "the physical type %r is one this runtime does not read" % (physical,)
    )


def _page_values(kind, data, count, levels):
    """The values one data page holds, with a null wherever its levels say there is one."""
    at = 0
    present = count
    marks = None
    if levels:
        size = struct.unpack("<I", bytes(data[:4]))[0]
        at = 4 + size
        marks = _levels_decoded(bytes(data[4 : 4 + size]), count)
        present = len([mark for mark in marks if mark])
    held = kind.unplain(bytes(data[at:]), present)
    if marks is None:
        return held
    values = []
    index = 0
    for mark in marks:
        if mark:
            values.append(held[index])
            index += 1
        else:
            values.append(None)
    return values


class _Budget(object):
    """How many values ONE read may still take in ([`MAX_FILE_VALUES`]).

    The counts a file states are its own, so they are spent against one allowance for the
    whole read rather than checked one at a time: a file of a thousand chunks each stating a
    permissible count states, together, a count this read does not hold.
    """

    def __init__(self, left):
        self.left = left

    def take(self, count, what):
        """Spend `count` of the allowance, or raise naming `what` stated it."""
        if count < 0:
            raise ParquetError("%s states a count below zero: %d" % (what, count))
        if count > self.left:
            raise ParquetError(
                "a read takes in at most %d values of one Parquet file; %s states %d more"
                % (MAX_FILE_VALUES, what, count)
            )
        self.left -= count
        return count


def _chunk_values(body, chunk, kind, levels, budget):
    """Every value one column chunk holds, page by page.

    Every count this walk reads is a caller's, so each is spent against the read's own
    allowance (`budget`) before anything is built; a page that carries NONE of the chunk's
    values, one whose sizes are below zero and one that leaves the cursor where it was all
    end the read, and how many pages one chunk may hold is bounded
    ([`MAX_PAGES_PER_CHUNK`]) — so a file whose metadata disagrees with its pages costs this
    process a failure rather than a walk without end.
    """
    meta = chunk.get(3) or {}
    codec = meta.get(4, CODEC_UNCOMPRESSED)
    count = budget.take(meta.get(5, 0), "a column chunk")
    at = meta.get(9)
    if at is None:
        raise ParquetError("a column chunk of this file states no page offset")
    values = []
    pages = 0
    while len(values) < count:
        pages += 1
        if pages > MAX_PAGES_PER_CHUNK:
            raise ParquetError(
                "a column chunk of this file holds more than the %d pages a read walks"
                % (MAX_PAGES_PER_CHUNK,)
            )
        if at is None or at < 0 or at >= len(body):
            raise ParquetError(
                "a column chunk of this file states %d values and its pages hold %d"
                % (count, len(values))
            )
        began = at
        reader = _Reader(body, at)
        header = reader.struct()
        at = reader.at
        stored = header.get(3, 0)
        if stored < 0 or header.get(2, 0) < 0:
            raise ParquetError("a page of this file states a size below zero")
        page = bytes(body[at : at + stored])
        at += stored
        if at <= began:
            raise ParquetError("a page of this file leaves the read where it began")
        kinds = header.get(1)
        if kinds == PAGE_DICTIONARY:
            raise ParquetError(
                "a dictionary-encoded Parquet column is one this runtime does not read"
            )
        if kinds != PAGE_DATA:
            if kinds == PAGE_DATA_V2:
                raise ParquetError(
                    "a Parquet column written in the second page layout is one this "
                    "runtime does not read"
                )
            continue
        detail = header.get(5) or {}
        encoding = detail.get(2, ENCODING_PLAIN)
        if encoding != ENCODING_PLAIN:
            raise ParquetError(
                "a Parquet column encoded as %s is one this runtime does not read"
                % (ENCODING_NAMES.get(encoding, encoding),)
            )
        # The PAGE states how many values it carries, which is its own count and not the
        # chunk's: a page carrying more than the chunk has left to take is one this read
        # does not hold.
        held = detail.get(1, 0)
        if held < 0 or held > count - len(values):
            raise ParquetError(
                "a page of this column states %d values, and the chunk it belongs to has "
                "%d left" % (held, count - len(values))
            )
        taken = _page_values(
            kind, _uncompressed(codec, page, header.get(2, 0)), held, levels
        )
        if not taken:
            raise ParquetError(
                "a page of this column carries none of the %d values the chunk states"
                % (count,)
            )
        values.extend(taken)
    return values[:count]


def read_file(body):
    """The (columns, types, rows) one Parquet file holds, in the order it holds them.

    A file this runtime cannot read is NAMED in the failure, whatever about it could not be
    read: the bytes are a caller's, so every way the reading of them can give out answers as
    this one failure rather than as whatever the reading itself raised.
    """
    try:
        return _read_file(body)
    except ParquetError:
        raise
    except (IndexError, KeyError, MemoryError, OverflowError, TypeError, ValueError,
            struct.error) as error:
        raise ParquetError(
            "this file is not a Parquet file this runtime reads: %s" % (error,)
        )


def _read_file(body):
    """See [`read_file`], whose failures this one's are reported as.

    How many values ONE read takes in is bounded by one allowance for the whole read
    ([`_Budget`], [`MAX_FILE_VALUES`]) — spent by every count the file states, the rows of
    the file, the rows of each row group and the values of each column chunk alike — so a
    file stating more than a read of this process holds is answered rather than read.
    """
    metadata = file_metadata(body)
    leaves = _leaves(metadata.get(2) or [])
    columns = [element.get(4, b"").decode("utf-8") for element in leaves]
    kinds = [_kind_read(element) for element in leaves]
    levels = [element.get(3) != REPETITION_REQUIRED for element in leaves]
    wide = max(len(leaves), 1)
    # The file's own row count, in the values it stands for, is the first thing spent: a
    # file whose footer states more rows than this read holds is answered here.
    rows_budget = _Budget(MAX_FILE_VALUES // wide)
    rows_budget.take(metadata.get(3, 0), "this file")
    budget = _Budget(MAX_FILE_VALUES)
    rows = []
    for group in metadata.get(4) or []:
        # And a ROW GROUP states its own count, which is spent against the same allowance
        # before any row of it is made.
        stated = rows_budget.take(group.get(3, 0), "a row group of this file")
        held = []
        for index, chunk in enumerate(group.get(1) or []):
            if index >= len(kinds):
                raise ParquetError(
                    "a row group of this file holds more columns than its schema states"
                )
            held.append(_chunk_values(body, chunk, kinds[index], levels[index], budget))
        for row in range(stated):
            rows.append([column[row] if row < len(column) else None for column in held])
    return columns, [kind.name for kind in kinds], rows
"#;
