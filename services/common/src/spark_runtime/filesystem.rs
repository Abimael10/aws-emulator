// SPDX-License-Identifier: CC0-1.0

//! `pyspark.filesystem`: the Hadoop-compatible filesystem a path resolves in.
//!
//! This is where a Spark program's reads and writes LAND. A path in Amazon S3 is an object
//! of this emulator's own object store, reached with the AWS SDK the runtime supplies the
//! guest; every other filesystem is one this emulator holds nothing for, and a path in it
//! raises the way Hadoop raises for a scheme it has no filesystem for.

/// The source of `pyspark/filesystem.py`.
pub(super) const FILESYSTEM_SOURCE: &str = r#"
"""The filesystems a Spark application's paths resolve in."""

# The schemes that name an object of Amazon S3: the EMR filesystem's own and the Hadoop
# S3A/S3N ones, which address the same store.
S3_SCHEMES = ("s3", "s3a", "s3n")

# How many bytes one write of this filesystem holds before it is flushed to the store. The
# bounds that matter are the interpreter's own, in host code; this one keeps a single stream
# from holding more than one object's worth of bytes.
MAX_OBJECT_BYTES = 64 * 1024 * 1024


class UnsupportedFileSystem(IOError):
    """A path whose filesystem this emulator does not have, as Hadoop words it."""


def _text(value):
    """A path argument as its text, whether it arrived as a string or as a Path."""
    if hasattr(value, "toString"):
        return value.toString()
    return str(value)


def split_uri(path):
    """The (scheme, authority, path) a URI names; the scheme is None for a bare path."""
    text = _text(path)
    if "://" not in text:
        return (None, None, text)
    scheme, rest = text.split("://", 1)
    if "/" not in rest:
        return (scheme, rest, "")
    authority, within = rest.split("/", 1)
    return (scheme, authority, within)


def object_of(path):
    """The (bucket, key) an Amazon S3 URI names, or None for any other path."""
    scheme, authority, within = split_uri(path)
    if scheme is None or scheme.lower() not in S3_SCHEMES:
        return None
    if not authority or not within:
        return None
    return (authority, within)


def _client():
    """The object store client, built through the SDK this runtime supplies the guest."""
    import boto3

    return boto3.client("s3")


class FileStatus(object):
    """One entry of a listing: the path, its length and whether it is a directory."""

    def __init__(self, path, length=0, directory=False):
        self._path = path
        self._length = length
        self._directory = directory

    def getPath(self):
        return self._path

    def getLen(self):
        return self._length

    def isDirectory(self):
        return self._directory

    def isDir(self):
        return self._directory

    def isFile(self):
        return not self._directory


class FSDataOutputStream(object):
    """An open write: the bytes are held until the stream is closed, then stored."""

    def __init__(self, bucket, key):
        self._bucket = bucket
        self._key = key
        self._buffer = bytearray()
        self._closed = False

    def write(self, data, offset=None, length=None):
        if self._closed:
            raise IOError("the stream for s3://%s/%s is closed" % (self._bucket, self._key))
        if isinstance(data, int):
            chunk = bytes(bytearray([data & 0xFF]))
        elif isinstance(data, str):
            chunk = data.encode("utf-8")
        else:
            chunk = bytes(bytearray(data))
        if offset is not None:
            end = len(chunk) if length is None else offset + length
            chunk = chunk[offset:end]
        if len(self._buffer) + len(chunk) > MAX_OBJECT_BYTES:
            raise IOError(
                "an object of at most %d bytes may be written" % (MAX_OBJECT_BYTES,)
            )
        self._buffer.extend(chunk)

    def writeBytes(self, text):
        self.write(text)

    def writeUTF(self, text):
        self.write(text)

    # Hadoop's flush and sync make written bytes visible to a reader. An object store has
    # one visible state, the stored object, so these hold the bytes until the close that
    # stores them.
    def flush(self):
        return None

    def hflush(self):
        return None

    def hsync(self):
        return None

    def sync(self):
        return None

    def close(self):
        if self._closed:
            return None
        self._closed = True
        held = "s3://%s/%s" % (self._bucket, self._key)
        try:
            _client().put_object(
                Bucket=self._bucket, Key=self._key, Body=bytes(self._buffer)
            )
        except Exception as error:
            # The write is signed as the identity the program runs as, so a store that
            # refuses that identity is what the program raises — worded as Hadoop words it
            # and naming the object it could not write.
            if _is_denial(error):
                raise _denied(held, "putObject", held, error)
            raise
        return None

    def __enter__(self):
        return self

    def __exit__(self, kind, value, traceback):
        self.close()
        return False


class FSDataInputStream(object):
    """An open read of a stored object."""

    def __init__(self, body):
        self._body = body
        self._at = 0

    def read(self, size=None):
        if size is None or size < 0:
            chunk = self._body[self._at:]
            self._at = len(self._body)
            return chunk
        chunk = self._body[self._at:self._at + size]
        self._at += len(chunk)
        return chunk

    def readFully(self, *rest):
        return self.read()

    def available(self):
        return len(self._body) - self._at

    def close(self):
        return None

    def __enter__(self):
        return self

    def __exit__(self, kind, value, traceback):
        self.close()
        return False


class FileSystem(object):
    """The filesystem of one Amazon S3 bucket, as `Path.getFileSystem` hands one out."""

    def __init__(self, scheme, bucket):
        self._scheme = scheme
        self._bucket = bucket

    @staticmethod
    def get(uri, conf=None):
        """The filesystem a URI resolves in. `FileSystem.get(uri, conf)` in Hadoop."""
        named = object_of(uri)
        if named is None:
            scheme, authority, within = split_uri(uri)
            if scheme is not None and scheme.lower() in S3_SCHEMES and authority:
                return FileSystem(scheme.lower(), authority)
            raise UnsupportedFileSystem("No FileSystem for scheme: %s" % (scheme,))
        bucket, _ = named
        scheme, _, _ = split_uri(uri)
        return FileSystem(scheme.lower(), bucket)

    def getUri(self):
        return "%s://%s" % (self._scheme, self._bucket)

    def _key(self, path):
        named = object_of(path)
        if named is None:
            raise UnsupportedFileSystem("No FileSystem for path: %s" % (_text(path),))
        bucket, key = named
        if bucket != self._bucket:
            raise UnsupportedFileSystem(
                "Wrong FS: %s, expected %s" % (_text(path), self.getUri())
            )
        return key

    def create(self, path, overwrite=True, *rest, **named):
        key = self._key(path)
        if not overwrite and self.exists(path):
            raise IOError("%s already exists" % (_text(path),))
        return FSDataOutputStream(self._bucket, key)

    def append(self, path, *rest, **named):
        raise UnsupportedFileSystem(
            "Append is not supported by %s" % (self.getUri(),)
        )

    def open(self, path, bufferSize=None):
        # Resolved through `_key`, like every other method of this filesystem: a path in
        # another bucket, or in no filesystem this holds, is refused here rather than read
        # through a bucket this instance was not obtained for.
        return FSDataInputStream(read_bytes("%s/%s" % (self.getUri(), self._key(path))))

    def exists(self, path):
        key = self._key(path)
        client = _client()
        try:
            client.head_object(Bucket=self._bucket, Key=key)
            return True
        except Exception:
            listed = client.list_objects_v2(
                Bucket=self._bucket, Prefix=key.rstrip("/") + "/", MaxKeys=1
            )
            return int(listed.get("KeyCount") or 0) > 0

    def delete(self, path, recursive=False):
        key = self._key(path)
        client = _client()
        if not recursive:
            client.delete_object(Bucket=self._bucket, Key=key)
            return True
        for status in self.listStatus(path):
            client.delete_object(Bucket=self._bucket, Key=self._key(status.getPath()))
        return True

    def mkdirs(self, path, *rest):
        # An object store holds objects, not directories: the prefix a path names exists
        # once an object is written under it, so there is nothing to create here.
        self._key(path)
        return True

    def listStatus(self, path):
        prefix = self._key(path).rstrip("/")
        listed = _client().list_objects_v2(Bucket=self._bucket, Prefix=prefix)
        statuses = []
        for entry in listed.get("Contents") or []:
            statuses.append(
                FileStatus(
                    "%s://%s/%s" % (self._scheme, self._bucket, entry["Key"]),
                    int(entry.get("Size") or 0),
                )
            )
        return statuses

    def getFileStatus(self, path):
        key = self._key(path)
        read = _client().head_object(Bucket=self._bucket, Key=key)
        return FileStatus(
            "%s://%s/%s" % (self._scheme, self._bucket, key),
            int(read.get("ContentLength") or 0),
        )

    def close(self):
        return None


def read_bytes(path):
    """The bytes of the object `path` names, read as the identity the program runs as."""
    named = object_of(path)
    if named is None:
        scheme, _, _ = split_uri(path)
        raise UnsupportedFileSystem("No FileSystem for scheme: %s" % (scheme,))
    bucket, key = named
    try:
        read = _client().get_object(Bucket=bucket, Key=key)
    except Exception as error:
        if _is_denial(error):
            raise _denied(path, "getObject", _uri(bucket, key), error)
        raise
    return _as_bytes(read["Body"].read())


def read_text(path):
    """The object `path` names, decoded as the text a line-based read sees."""
    return read_bytes(path).decode("utf-8")


def write_text(path, text):
    """Store `text` as the object `path` names, through one open stream."""
    stream = FileSystem.get(path).create(path, True)
    stream.write(text)
    stream.close()
    return None


def write_bytes(path, data):
    """Store `data` as the object `path` names, through one open stream."""
    stream = FileSystem.get(path).create(path, True)
    stream.write(data)
    stream.close()
    return None


# The characters a path segment may not carry, which the Hive layout spells as `%XX` instead:
# the path separators and the characters its own grammar gives a meaning (the `=` between a
# partition column and its value among them), and every control character. A segment built
# from DATA — the value of a partition column — is written through this, so a value spelling a
# separator or a parent directory cannot name an object outside the path the write was given.
# The first two are given by code point: the double quote and the hash.
ESCAPED_IN_A_PATH = chr(34) + chr(35) + "%'*/:=?\\\x7f{[]^"


def escape_path_name(value):
    """One path segment with every character a segment may not carry spelled as `%XX`."""
    text = _text(value)
    out = []
    for character in text:
        if character in ESCAPED_IN_A_PATH or ord(character) < 0x20:
            out.append("%%%02X" % (ord(character),))
        else:
            out.append(character)
    return "".join(out)


def unescape_path_name(value):
    """The value a path segment spells, with every `%XX` read back as its character."""
    text = _text(value)
    out = []
    index = 0
    while index < len(text):
        character = text[index]
        if character == "%" and index + 2 < len(text):
            try:
                out.append(chr(int(text[index + 1 : index + 3], 16)))
                index += 3
                continue
            except ValueError:
                pass
        out.append(character)
        index += 1
    return "".join(out)


# --- what a program's READ of a source path covers, and what a refused one raises ---------

# The error codes an object store answers when the identity the call was signed as may not
# make it. The role the work runs as is what signs these calls, so this is the answer a read
# or a write gets when that role is refused the access.
DENIED_CODES = (
    "AccessDenied",
    "AccessDeniedException",
    "AllAccessDisabled",
    "Forbidden",
    "UnauthorizedAccess",
)

# The error codes an object store answers for a key it does not hold — the one answer a read
# of a path that may instead be a PREFIX of objects falls through on.
MISSING_CODES = ("NoSuchKey", "NotFound", "404")

# The HTTP status a refusal reports when the store's own answer named none.
DENIED_STATUS = 403


class AccessDeniedException(IOError):
    """An access the identity the call was signed as was refused, as Hadoop words it."""


class AnalysisException(Exception):
    """A source a read cannot be planned over, as Spark words it."""


def _refusal(error):
    """The (code, message) a store's refusal carried, or ('', text) for anything else."""
    response = getattr(error, "response", None)
    if isinstance(response, dict):
        held = response.get("Error") or {}
        return (str(held.get("Code") or ""), str(held.get("Message") or ""))
    return ("", str(error))


def _is_denial(error):
    """Whether a store's answer refused the identity the call was signed as."""
    code, _ = _refusal(error)
    return code in DENIED_CODES


def is_denial(error):
    """The same question, for the readers of a source path that live beside this module."""
    return _is_denial(error)


def unresolved_source(path):
    """The analysis failure a source path this filesystem could not resolve raises.

    A read RESOLVES its source path before it plans anything over it: it asks the filesystem
    which files are there. An object store answers a caller it may not read the same way
    whether or not an object is there, so a resolution it refuses yields no file at the path,
    exactly as a path holding nothing does — neither tells the filesystem the path is there.
    Both are therefore this one analysis failure, naming the path. A WRITE is not a
    resolution: it names the object it is making, so the store's refusal of it is reported as
    it stands (`_denied`).
    """
    return AnalysisException("Path does not exist: %s" % (_text(path),))


def _is_missing(error):
    """Whether a store's answer was that it holds no such key."""
    code, _ = _refusal(error)
    return code in MISSING_CODES


def _status(error):
    """The HTTP status the store's own answer carried, or the one a refusal carries."""
    response = getattr(error, "response", None)
    if isinstance(response, dict):
        metadata = response.get("ResponseMetadata") or {}
        held = metadata.get("HTTPStatusCode")
        if held is not None:
            try:
                return int(held)
            except (TypeError, ValueError):
                return DENIED_STATUS
    return DENIED_STATUS


def _denied(path, operation, where, error):
    """The access-denied failure a refused call raises, worded as Hadoop words it."""
    code, message = _refusal(error)
    stated = message or "Access Denied"
    return AccessDeniedException(
        "%s: %s on %s: Access Denied (Service: Amazon S3; Status Code: %d; Error Code: %s; "
        "Message: %s)"
        % (_text(path), operation, where, _status(error), code or "AccessDenied", stated)
    )


def _uri(bucket, key):
    """The S3 URI of one object, in the scheme a program names a path in."""
    return "s3://%s/%s" % (bucket, key)


def uri(bucket, key):
    """The same address, for the readers of a source path that live beside this module."""
    return _uri(bucket, key)


def _as_bytes(body):
    """An answered body as bytes, however the SDK handed it over."""
    if isinstance(body, str):
        return body.encode("utf-8")
    return bytes(bytearray(body))


def prefix_of(path):
    """The (bucket, key) an Amazon S3 URI names, where the key MAY be empty.

    A path naming a bucket, or a bucket and a prefix, addresses every object under it, which
    is what a dataset source or an output directory is.
    """
    scheme, authority, within = split_uri(path)
    if scheme is None or scheme.lower() not in S3_SCHEMES or not authority:
        return None
    return (authority, within)


def list_objects(path):
    """Every object stored under the prefix `path` names, as (key, size) pairs."""
    named = prefix_of(path)
    if named is None:
        raise UnsupportedFileSystem("No FileSystem for scheme: %s" % (split_uri(path)[0],))
    bucket, key = named
    prefix = key.rstrip("/")
    if prefix:
        prefix = prefix + "/"
    client = _client()
    found = []
    token = None
    while True:
        asked = {"Bucket": bucket, "Prefix": prefix}
        if token:
            asked["ContinuationToken"] = token
        try:
            listed = client.list_objects_v2(**asked)
        except Exception as error:
            if _is_denial(error):
                raise _denied(path, "listStatus", _text(path), error)
            raise
        for entry in listed.get("Contents") or []:
            if not str(entry["Key"]).endswith("/"):
                found.append((str(entry["Key"]), int(entry.get("Size") or 0)))
        token = listed.get("NextContinuationToken")
        if not listed.get("IsTruncated") or not token:
            break
    return found


def source_objects(path):
    """Every object ONE read of `path` covers: the object itself, or the prefix's objects.

    A path naming a stored object reads that object; one naming a prefix reads every object
    under it, which is how a Spark source path addresses a directory of part files. A path
    whose files this filesystem could not resolve — the store holds nothing under it, or the
    identity the read is signed as is refused it — is the analysis failure Spark raises for
    such a path ([`unresolved_source`]).
    """
    named = prefix_of(path)
    if named is None:
        raise UnsupportedFileSystem("No FileSystem for scheme: %s" % (split_uri(path)[0],))
    bucket, key = named
    if key and not key.endswith("/"):
        try:
            read = _client().get_object(Bucket=bucket, Key=key)
            return [(_uri(bucket, key), _as_bytes(read["Body"].read()))]
        except Exception as error:
            # A read the store REFUSES is one the resolution of this path got no file from,
            # which is what a path holding nothing answers too ([`unresolved_source`]).
            if _is_denial(error):
                raise unresolved_source(path)
            # A path the store holds no object for may still name a PREFIX of objects, which
            # is the one answer worth reading on; anything else the store said about this
            # read is what the read failed with.
            if not _is_missing(error):
                raise
    read = []
    try:
        listed = list_objects(path)
    except AccessDeniedException:
        raise unresolved_source(path)
    for stored, _size in listed:
        try:
            answer = _client().get_object(Bucket=bucket, Key=stored)
        except Exception as error:
            if _is_denial(error):
                raise unresolved_source(path)
            raise
        read.append((_uri(bucket, stored), _as_bytes(answer["Body"].read())))
    if not read:
        raise unresolved_source(path)
    return read


def source_text(path):
    """The lines of every object one read of `path` covers, in the order of their keys."""
    lines = []
    for _held, body in source_objects(path):
        lines.extend(body.decode("utf-8").splitlines())
    return lines
"#;
