// SPDX-License-Identifier: CC0-1.0

//! The EPHEMERAL STORAGE of a `python3.x` execution environment: the writable `/tmp` the
//! function's `EphemeralStorage.Size` sizes, held in this process's own memory.
//!
//! "Each Lambda function receives an ephemeral `/tmp` directory", and it is the ONE writable
//! directory a function has: everything else the environment holds is a read-only mount
//! ([`super::guest_mount`]). Its size is configuration — `EphemeralStorage.Size` is "the size
//! of the function's /tmp directory in MB", from 512 to 10,240 — so a filesystem reading of
//! `/tmp` reports that size, a write past what it has left is refused as a full device is, and
//! a client can tell the configured number is a real limit rather than a reported one.
//!
//! The storage belongs to the EXECUTION ENVIRONMENT, not to one invocation: this source is
//! evaluated once per environment and the tree it holds stands for as long as that environment
//! does, so a file one invocation wrote is there for the next invocation served in the same
//! environment ("each execution environment provides […] /tmp […] that can be used for
//! temporary storage" and is reused with it) and a fresh environment starts with an empty
//! `/tmp`. A configuration change mints a new revision, and no environment built before it
//! serves an invocation that arrives after it ([`super::warm_pool::EnvironmentKey`]), so a
//! function whose ephemeral storage was resized gets a `/tmp` of the new size, empty, in the
//! next environment.
//!
//! It reaches nothing of the host:
//!
//! * the files are entries of this module's own tree, written into this process's memory; no
//!   host path is opened, created or removed, and nothing a run writes outlives the
//!   environment it wrote in. A filesystem reading reports the size the function configured,
//!   while the bytes this host really holds for one environment are bounded beside it
//!   (`super::MAX_EPHEMERAL_HELD_BYTES`), exactly as the memory one run may allocate is. EVERY
//!   entry is charged to that size — a name costs the record a filesystem keeps for one and
//!   takes one of the inodes a filesystem of this size has, and a path longer than the lengths
//!   this one states is refused — so no run can make this process hold files, directories,
//!   names or descriptors without end;
//! * the calls go on `os`, `io` and the builtins — the names a handler reads its filesystem
//!   through — and each of them DISPATCHES: a path of `/tmp` is answered here, a path of a
//!   read-only mount is answered by the mounts ([`super::guest_mount`]), and a path on neither
//!   is refused by that module itself, which holds nothing of the host's own file calls;
//! * the `posix` module `os` is built on keeps every refusal the host installed on it
//!   ([`super::host_posix`], `super::python`'s `narrow_posix`), so the containment of the host
//!   filesystem stays where the host put it;
//! * `/tmp` is a DISK on the real runtime and its bytes are not what `MemorySize` bounds, so
//!   what a write puts there is reported to the host and left out of what the run is measured
//!   holding (`super::python`'s `Bounds::wrote_to_ephemeral_storage`) — bounded there by the
//!   size this function's own configuration states and by the pool the runs of this process
//!   share, so nothing a run reports moves a bound of it.
//!
//! The three standard DESCRIPTORS are served here too, because they are the other half of the
//! surface a handler reaches for by number: a write to descriptor 1 or 2 is a write to the
//! invocation's output streams, which is where `print()` and the logging interface already go
//! ([`super::bootstrap`]), and a read of descriptor 0 answers the empty input an invocation is
//! given. A file the handler opens carries a descriptor of its own for the same reason: a file
//! object of a real runtime answers one, and a program that flushes, duplicates, positions or
//! reserves space on the number it reads off `fileno()` is making ordinary calls on an ordinary
//! open file.

/// The Python source of the ephemeral storage, which is the SECOND section of the runtime
/// source: it is evaluated in the runtime's own scope, after the read-only mounts it dispatches
/// to ([`super::guest_mount`]) and before the runtime itself, which installs both.
///
/// A macro rather than a constant for the reason the mounts are one: the sections are ONE
/// compiled unit — the runtime's own scope — so the gates here call the mounts' own names
/// directly and the runtime below calls `_tmp_install`, which builds the storage and puts the
/// dispatching calls on `os`. What it is handed is the rest of that source, as a literal.
macro_rules! ephemeral_source {
    ($($runtime:tt)*) => {
        concat!(
            r##"
"""The EPHEMERAL STORAGE of this execution environment: the writable /tmp a function has."""

import builtins
import errno
import io
import os
import sys
import time

# The one writable directory a function has.
_TMP_ROOT = "/tmp"

# Every FILE the storage holds: its absolute path, to the bytes written there, which are the
# entry itself rather than a copy — so a stream writes into the file it opened.
_TMP_FILES = {}
# Every DIRECTORY it holds, the root included.
_TMP_DIRECTORIES = set()
# When each entry was last written, which is what its times report.
_TMP_TIMES = {}
# The permissions each entry carries, for an entry whose mode was changed from the one it was
# created with.
_TMP_MODES = {}
# The number each entry is identified by, handed out as entries are asked about.
_TMP_INODES = {}
# How large the storage IS (the configured size), how much of it this host really holds, how
# much is written, and when the storage appeared.
_TMP_SPACE = {"size": 0, "writable": 0, "held": 0, "at": 0.0}
# The descriptors open on a file of the storage: the fd a caller holds, to the file, how far
# into it the descriptor is, whether it may write, whether every write appends and whether it
# may read. A duplicated descriptor is a second number on the SAME held entry, which is what
# makes the two share one position.
_TMP_DESCRIPTORS = {}

# The size of a block a filesystem reading reports, and the bytes-per-inode an ext filesystem
# is made with by default, which is what the inode count of one that size comes out of.
_TMP_BLOCK_SIZE = 4096
_TMP_BYTES_PER_INODE = 16384
# The longest a path component may be, which is what a filesystem reports as its `f_namemax`,
# and the longest a whole path may be, which is the other length a filesystem states.
_TMP_MAX_NAME = 255
_TMP_MAX_PATH = 4096
# What the RECORD of one entry costs the storage, beside the bytes of a file: a filesystem
# keeps an inode and a directory entry for every name it holds, so a tree of names and no
# content is not free on one either. Taken as ONE BLOCK, which is what the smallest entry of a
# real filesystem occupies — a filesystem hands space out in whole blocks, so this is a block
# like every other charge against the storage ([`_tmp_occupies`]) and what a reading reports as
# available stays exactly what may still be written.
_TMP_ENTRY_BYTES = 4096
# How much of a grown file's zeros are put there at a time ([`_tmp_zero_fill`]), so that making
# a file as long as the storage allows does not build a second copy of it beside the file.
_TMP_FILL_CHUNK = 1024 * 1024
# How many files one invocation may have OPEN at once: "File descriptors: 1,024" is the quota
# AWS states for an execution environment.
_TMP_MAX_DESCRIPTORS = 1024
# Said of an open past that quota, as the operating system says it.
_TMP_TOO_MANY_FILES = "Too many open files"
# Said of a path longer than this filesystem takes.
_TMP_NAME_TOO_LONG = "File name too long"
# The mode a file and a directory of the storage are created with: the type, then the
# permissions a process with the usual mask leaves on what it creates.
_TMP_FILE_MODE = 0o100644
_TMP_DIRECTORY_MODE = 0o040755
# The mode the root itself carries: a directory everyone may write to, whose entries only
# their owner may remove, which is what a temporary directory is.
_TMP_ROOT_MODE = 0o041777
# The mode one of the standard streams reports: a character device, which nobody may seek.
_TMP_STREAM_MODE = 0o020620
# The type bits of a mode.
_TMP_TYPE_MASK = 0o170000
# Who a function's own files belong to.
_TMP_UID = 993
_TMP_GID = 990
# Said of a write past what the storage has left, as the operating system says it.
_TMP_NO_SPACE = "No space left on device"
# The descriptors a process starts with: the input it was given, and the two streams its
# output goes to.
_TMP_STDIN = 0
_TMP_STDOUT = 1
_TMP_STDERR = 2


def _tmp_install(size, writable, at):
    """Build the ephemeral storage of this execution environment and serve it on `os`.

    `size` is how large the function's `/tmp` IS, which is what a filesystem reading of it
    reports; `writable` is how much of that this host really holds; `at` is the instant the
    environment appeared, which is the root's own time. The tree starts EMPTY, because a fresh
    execution environment has written nothing yet.
    """
    _TMP_SPACE["size"] = max(0, int(size))
    _TMP_SPACE["writable"] = max(0, int(writable))
    _TMP_SPACE["held"] = 0
    _TMP_SPACE["at"] = at
    _TMP_FILES.clear()
    _TMP_DIRECTORIES.clear()
    _TMP_TIMES.clear()
    _TMP_MODES.clear()
    _TMP_INODES.clear()
    _TMP_DESCRIPTORS.clear()
    _TMP_DIRECTORIES.add(_TMP_ROOT)
    _TMP_TIMES[_TMP_ROOT] = at
    _tmp_serve()


def _tmp_governs(resolved):
    """Whether `resolved` is a path of the ephemeral storage: the root itself, or a path under
    it, held or not."""
    return resolved == _TMP_ROOT or resolved.startswith(_TMP_ROOT + "/")


def _tmp_kind(resolved):
    """What `resolved` IS in the storage: a file, a directory, or nothing it holds."""
    if resolved in _TMP_FILES:
        return "file"
    if resolved in _TMP_DIRECTORIES:
        return "directory"
    return None


def _tmp_parent(resolved):
    """The directory `resolved` is in."""
    return resolved.rpartition("/")[0] or "/"


def _tmp_now():
    """The instant a write happens at, which is what the entry's times then report."""
    return time.time()


def _tmp_inode(resolved):
    """The number the entry at `resolved` is identified by, which is its own and no other
    path's."""
    if resolved not in _TMP_INODES:
        _TMP_INODES[resolved] = len(_TMP_INODES) + 1
    return _TMP_INODES[resolved]


def _tmp_names(resolved):
    """The names directly inside the directory at `resolved`, in order."""
    under = resolved + "/" if resolved != "/" else "/"
    named = set()
    for held in list(_TMP_FILES) + list(_TMP_DIRECTORIES):
        if not held.startswith(under):
            continue
        name = held[len(under):].partition("/")[0]
        if name:
            named.add(name)
    return sorted(named)


def _tmp_occupies(length):
    """How much of the storage a file of `length` bytes TAKES UP: whole blocks.

    A filesystem hands space out in blocks ([`_TMP_BLOCK_SIZE`], which is the block size a
    reading of this one reports), so a file occupies the blocks its bytes need and a partly
    filled block is taken up by it. Every charge against the storage is therefore a whole
    number of blocks, which is what makes the space a reading reports as available
    ([`_tmp_statvfs`], stated in those same blocks) exactly the space a write may still take:
    a growth to what the reading reports is admitted, and one byte more is refused as a full
    device refuses it.
    """
    blocks = (max(0, length) + _TMP_BLOCK_SIZE - 1) // _TMP_BLOCK_SIZE
    return blocks * _TMP_BLOCK_SIZE


def _tmp_zero_fill(held, wanted):
    """Grow the bytes `held` to `wanted`, which a grown file reads as zeros.

    Written a chunk at a time ([`_TMP_FILL_CHUNK`]): these bytes are the storage's own, so
    putting them there builds no second copy of the whole file beside it — a file grown to what
    the storage allows would otherwise cost twice its length at once.
    """
    chunk = bytes(_TMP_FILL_CHUNK)
    while len(held) < wanted:
        at = len(held)
        step = min(_TMP_FILL_CHUNK, wanted - at)
        held[at:at + step] = chunk if step == _TMP_FILL_CHUNK else chunk[:step]


def _tmp_room():
    """How many bytes may still be WRITTEN: what is left of the function's configured size,
    held to what this host really holds for one environment."""
    limit = min(_TMP_SPACE["size"], _TMP_SPACE["writable"])
    return max(0, limit - _TMP_SPACE["held"])


def _tmp_free():
    """How much of the storage is FREE, which is what a filesystem reading reports: the size
    the function is configured for, less what its entries take up ([`_tmp_occupies`]).

    What a reading reports is the function's own configuration — `EphemeralStorage.Size` is the
    size of the directory it has — rather than what this host happens to hold for it, which is
    what bounds the writes instead ([`_tmp_room`]).
    """
    return max(0, _TMP_SPACE["size"] - _TMP_SPACE["held"])


def _tmp_charge(grown, resolved):
    """Account for the storage growing by `grown` bytes, which may be negative.

    The amount is a whole number of BLOCKS, because that is how a filesystem hands space out
    ([`_tmp_occupies`]): what a reading of this one reports as available is therefore exactly
    what may still be written. A growth past what the storage has left is refused the way a
    full device refuses one, and nothing of it is written. What the storage GREW by is reported
    to the host, because these bytes are a disk on the real runtime and are held in this
    process's memory here: the memory a run is measured holding is what the function allocated
    and not the files it wrote. How much of that the host leaves out is its own decision and
    bounded by it, per run and for the process together, so what is reported here moves no
    bound of the run.
    """
    if grown <= 0:
        _TMP_SPACE["held"] = max(0, _TMP_SPACE["held"] + grown)
        return
    if grown > _tmp_room():
        raise OSError(errno.ENOSPC, _TMP_NO_SPACE, resolved)
    _TMP_SPACE["held"] += grown
    _host.ephemeral(grown)


def _tmp_inodes():
    """How many ENTRIES the storage holds at most, which is the inode count a filesystem of its
    size reports ([`_tmp_statvfs`]): one per `_TMP_BYTES_PER_INODE`."""
    return max(1, _TMP_SPACE["size"] // _TMP_BYTES_PER_INODE)


def _tmp_named(resolved):
    """Refuse a path longer than this filesystem takes, as a filesystem refuses one past the
    lengths it states ([`_TMP_MAX_NAME`], [`_TMP_MAX_PATH`])."""
    if len(resolved) > _TMP_MAX_PATH:
        raise OSError(errno.ENAMETOOLONG, _TMP_NAME_TOO_LONG, resolved)
    for part in resolved.split("/"):
        if len(part) > _TMP_MAX_NAME:
            raise OSError(errno.ENAMETOOLONG, _TMP_NAME_TOO_LONG, resolved)


def _tmp_admit(resolved):
    """Admit ONE new entry at `resolved`, before anything is held for it.

    A name is an entry of the filesystem, so it costs what one costs ([`_TMP_ENTRY_BYTES`]) and
    takes one of the inodes a filesystem of this size has: a tree of empty files and directories
    fills the storage exactly as the bytes of a file do, and a run cannot make this process hold
    names without end. Both refusals are the ones a full device answers with.
    """
    _tmp_named(resolved)
    if len(_TMP_FILES) + len(_TMP_DIRECTORIES) >= _tmp_inodes():
        raise OSError(errno.ENOSPC, _TMP_NO_SPACE, resolved)
    _tmp_charge(_TMP_ENTRY_BYTES, resolved)


def _tmp_release(resolved):
    """Give back what the RECORD of the entry at `resolved` held, and the number it was
    identified by: a path the storage no longer holds is remembered nowhere."""
    _tmp_charge(-_TMP_ENTRY_BYTES, resolved)
    _TMP_TIMES.pop(resolved, None)
    _TMP_MODES.pop(resolved, None)
    _TMP_INODES.pop(resolved, None)


def _tmp_mode(resolved, kind):
    """The mode the entry at `resolved` reports."""
    if resolved in _TMP_MODES:
        return _TMP_MODES[resolved]
    if kind != "directory":
        return _TMP_FILE_MODE
    return _TMP_ROOT_MODE if resolved == _TMP_ROOT else _TMP_DIRECTORY_MODE


def _tmp_stat_result(resolved, kind):
    """What a `stat` answers for the entry at `resolved`."""
    directory = kind == "directory"
    size = 0 if directory else len(_TMP_FILES[resolved])
    at = _TMP_TIMES.get(resolved, _TMP_SPACE["at"])
    nanoseconds = int(at * 1000000000.0)
    return os.stat_result(
        (
            _tmp_mode(resolved, kind),
            _tmp_inode(resolved),
            0,
            1,
            _TMP_UID,
            _TMP_GID,
            size,
            at,
            at,
            at,
        ),
        {
            "st_atime_ns": nanoseconds,
            "st_mtime_ns": nanoseconds,
            "st_ctime_ns": nanoseconds,
            "st_blksize": _TMP_BLOCK_SIZE,
            # The 512-byte units ALLOCATED for it, which is what this member reports: the
            # whole blocks the entry takes up ([`_tmp_occupies`]) and not its bytes rounded
            # up, so what a reading says the entry costs is what the storage charged for it.
            "st_blocks": _tmp_occupies(size) // 512,
        },
    )


def _tmp_stream_stat(descriptor):
    """What a `stat` answers for one of the standard streams: a character device of no size."""
    at = _TMP_SPACE["at"]
    nanoseconds = int(at * 1000000000.0)
    return os.stat_result(
        (
            _TMP_STREAM_MODE,
            _tmp_inode("<stream:%d>" % descriptor),
            0,
            1,
            _TMP_UID,
            _TMP_GID,
            0,
            at,
            at,
            at,
        ),
        {
            "st_atime_ns": nanoseconds,
            "st_mtime_ns": nanoseconds,
            "st_ctime_ns": nanoseconds,
            "st_blksize": _TMP_BLOCK_SIZE,
            "st_blocks": 0,
        },
    )


def _tmp_statvfs():
    """What a `statvfs` answers for the ephemeral storage: the size the function is configured
    for, and what is left of it."""
    size = _TMP_SPACE["size"]
    blocks = size // _TMP_BLOCK_SIZE
    available = _tmp_free() // _TMP_BLOCK_SIZE
    inodes = _tmp_inodes()
    free = max(0, inodes - len(_TMP_FILES) - len(_TMP_DIRECTORIES))
    return os.statvfs_result(
        (
            _TMP_BLOCK_SIZE,
            _TMP_BLOCK_SIZE,
            blocks,
            available,
            available,
            inodes,
            free,
            free,
            # No mount flag a `statvfs` reports is in force on it: it is writable, and the ids
            # a file is owned by are honoured.
            0,
            _TMP_MAX_NAME,
        ),
        {"f_fsid": 0},
    )


def _tmp_mount_statvfs():
    """What a `statvfs` answers for a path of a READ-ONLY mount: a filesystem exactly as large
    as the content it holds, with nothing free and the read-only flag in force on it."""
    total = 0
    for path in _MOUNT_FILES:
        total += _mount_size(path)
    blocks = (total + _TMP_BLOCK_SIZE - 1) // _TMP_BLOCK_SIZE
    inodes = len(_MOUNT_FILES) + len(_MOUNT_DIRECTORIES)
    return os.statvfs_result(
        (
            _TMP_BLOCK_SIZE,
            _TMP_BLOCK_SIZE,
            blocks,
            0,
            0,
            inodes,
            0,
            0,
            os.ST_RDONLY,
            _TMP_MAX_NAME,
        ),
        {"f_fsid": 0},
    )


def _tmp_read_only(resolved):
    """What a call that would WRITE outside the ephemeral storage answers.

    Everything else this environment holds is a read-only mount, so a write to one answers
    `EROFS` exactly as the real runtime answers a write under the task root or the layer root;
    a path on no mount of this function reaches no filesystem at all.
    """
    if _mount_governs(resolved):
        raise OSError(errno.EROFS, _MOUNT_READ_ONLY, resolved)
    return _mount_unreachable()


def _tmp_create(resolved):
    """Create an empty file at `resolved`, which is one ENTRY of the storage and costs what one
    costs ([`_tmp_admit`])."""
    if _tmp_kind(_tmp_parent(resolved)) != "directory":
        raise _mount_absent(resolved)
    _tmp_admit(resolved)
    _TMP_FILES[resolved] = bytearray()
    _TMP_TIMES[resolved] = _tmp_now()


def _tmp_write_at(resolved, position, chunk):
    """Write `chunk` into the file at `resolved`, from `position`."""
    held = _TMP_FILES[resolved]
    end = position + len(chunk)
    if end > len(held):
        _tmp_charge(_tmp_occupies(end) - _tmp_occupies(len(held)), resolved)
    if position > len(held):
        _tmp_zero_fill(held, position)
    held[position:end] = chunk
    _TMP_TIMES[resolved] = _tmp_now()
    return len(chunk)


def _tmp_truncate_to(resolved, size):
    """Make the file at `resolved` exactly `size` bytes long."""
    held = _TMP_FILES[resolved]
    size = max(0, int(size))
    if size != len(held):
        _tmp_charge(_tmp_occupies(size) - _tmp_occupies(len(held)), resolved)
    if size > len(held):
        _tmp_zero_fill(held, size)
    elif size < len(held):
        del held[size:]
    _TMP_TIMES[resolved] = _tmp_now()
    return size


def _tmp_remove(resolved):
    """Remove the file at `resolved`, giving back what it held: its bytes and its own record."""
    kind = _tmp_kind(resolved)
    if kind is None:
        raise _mount_absent(resolved)
    if kind != "file":
        raise IsADirectoryError(errno.EISDIR, "Is a directory", resolved)
    _tmp_charge(-_tmp_occupies(len(_TMP_FILES[resolved])), resolved)
    del _TMP_FILES[resolved]
    _tmp_release(resolved)


def _tmp_access(resolved, mode):
    """Whether the entry at `resolved` is reachable in the way `mode` asks, read off the
    permissions it carries."""
    kind = _tmp_kind(resolved)
    if kind is None:
        return False
    held = _tmp_mode(resolved, kind)
    if mode & os.R_OK and not held & 0o400:
        return False
    if mode & os.W_OK and not held & 0o200:
        return False
    if mode & os.X_OK and not held & 0o100:
        return False
    return True


def _tmp_encoding(encoding):
    """The encoding a TEXT stream of the storage reads and writes in.

    A caller that names none gets the encoding the environment's own locale is, and so does one
    that asks for exactly that (`"locale"`, which is what the standard library's own
    `text_encoding` answers for a caller that named none): the runtime reports a UTF-8 locale in
    `LANG`, so that is the encoding either of them means.
    """
    if not encoding or encoding == "locale":
        return "utf-8"
    return encoding


class _TmpStream:
    """One open file of the ephemeral storage, as `open` answers with one.

    It is a stream over a DESCRIPTOR of the storage, as a file object of a real runtime is: the
    bytes it reads and writes are the entry's own and how far into the file it is lives in the
    descriptor, so `os.write(stream.fileno(), ...)` and the stream's own `write` are two ways
    into one open file — and a write reaches the file where the handler made it rather than a
    copy flushed later, which is what makes a write past what the storage has left fail at that
    write.
    """

    def __init__(self, descriptor, mode, text, encoding, errors):
        self.name = _TMP_DESCRIPTORS[descriptor][0]
        self.mode = mode
        self.encoding = encoding if text else None
        self.errors = errors if text else None
        self.newlines = None
        self.closed = False
        self._descriptor = descriptor
        self._text = text

    def __enter__(self):
        return self

    def __exit__(self, *exception):
        self.close()
        return False

    def __iter__(self):
        return self

    def __next__(self):
        line = self.readline()
        if not line:
            raise StopIteration
        return line

    def _open(self):
        """The descriptor this stream is on, and the bytes of the file it is open on."""
        if self.closed:
            raise ValueError("I/O operation on closed file")
        if self._descriptor not in _TMP_DESCRIPTORS:
            raise OSError(errno.EBADF, "Bad file descriptor")
        return _tmp_descriptor_file(self._descriptor)

    def _decoded(self, chunk):
        if not self._text:
            return chunk
        return chunk.decode(self.encoding or "utf-8", self.errors or "strict")

    def read(self, size=-1):
        held, bytes_of = self._open()
        if not held[4]:
            raise io.UnsupportedOperation("not readable")
        if size is None or size < 0:
            end = len(bytes_of)
        else:
            end = min(len(bytes_of), held[1] + size)
        chunk = bytes(bytes_of[held[1]:end])
        held[1] = max(held[1], end)
        return self._decoded(chunk)

    def readline(self, size=-1):
        held, bytes_of = self._open()
        if not held[4]:
            raise io.UnsupportedOperation("not readable")
        found = bytes_of.find(b"\n", min(held[1], len(bytes_of)))
        end = len(bytes_of) if found < 0 else found + 1
        if size is not None and size >= 0:
            end = min(end, held[1] + size)
        chunk = bytes(bytes_of[held[1]:end])
        held[1] = max(held[1], end)
        return self._decoded(chunk)

    def readlines(self, hint=-1):
        lines = []
        while True:
            line = self.readline()
            if not line:
                return lines
            lines.append(line)

    def write(self, data):
        self._open()
        if self._text:
            chunk = data.encode(self.encoding or "utf-8", self.errors or "strict")
        else:
            chunk = bytes(data)
        _tmp_descriptor_write(self._descriptor, chunk)
        return len(data)

    def writelines(self, lines):
        for line in lines:
            self.write(line)

    def seek(self, position, whence=0):
        self._open()
        return _tmp_descriptor_lseek(self._descriptor, position, whence)

    def tell(self):
        held, _ = self._open()
        return held[1]

    def truncate(self, size=None):
        held, _ = self._open()
        if not held[2]:
            raise io.UnsupportedOperation("not writable")
        return _tmp_truncate_to(held[0], held[1] if size is None else size)

    def flush(self):
        self._open()
        return None

    def close(self):
        """Close the stream, and with it the descriptor it holds, as a file object does."""
        if not self.closed:
            self.closed = True
            _TMP_DESCRIPTORS.pop(self._descriptor, None)
        return None

    def readable(self):
        held, _ = self._open()
        return bool(held[4])

    def writable(self):
        held, _ = self._open()
        return bool(held[2])

    def seekable(self):
        return True

    def isatty(self):
        return False

    def fileno(self):
        """The DESCRIPTOR this stream is on, which is what a file object of a real runtime
        answers: a caller may make the operating-system calls on it itself."""
        if self.closed:
            raise ValueError("I/O operation on closed file")
        return self._descriptor


class _TmpEntry:
    """One entry of a directory of the ephemeral storage, as `scandir` answers them."""

    def __init__(self, directory, name):
        self.name = name
        self.path = directory + "/" + name if directory != "/" else "/" + name

    def __repr__(self):
        return "<DirEntry %r>" % (self.name,)

    def __fspath__(self):
        return self.path

    def inode(self):
        return _tmp_inode(self.path)

    def is_dir(self, *, follow_symlinks=True):
        return _tmp_kind(self.path) == "directory"

    def is_file(self, *, follow_symlinks=True):
        return _tmp_kind(self.path) == "file"

    def is_symlink(self):
        return False

    def stat(self, *, follow_symlinks=True):
        kind = _tmp_kind(self.path)
        if kind is None:
            raise _mount_absent(self.path)
        return _tmp_stat_result(self.path, kind)


class _TmpScan:
    """The entries of one directory of the storage, as the iterator `scandir` answers."""

    def __init__(self, directory, names):
        self._directory = directory
        self._names = list(names)
        self._at = 0

    def __iter__(self):
        return self

    def __next__(self):
        if self._at >= len(self._names):
            raise StopIteration
        name = self._names[self._at]
        self._at += 1
        return _TmpEntry(self._directory, name)

    def __enter__(self):
        return self

    def __exit__(self, *exception):
        self.close()
        return False

    def close(self):
        self._names = []
        self._at = 0


def _tmp_open_file(resolved, mode, encoding, errors):
    """Open a file of the storage, as `open` does: the mode says whether it is created,
    truncated, appended to, read and written."""
    text = "b" not in mode
    creating = any(character in mode for character in "wax")
    updating = "+" in mode
    kind = _tmp_kind(resolved)
    if kind == "directory":
        raise IsADirectoryError(errno.EISDIR, "Is a directory", resolved)
    if "x" in mode and kind == "file":
        raise FileExistsError(errno.EEXIST, "File exists", resolved)
    if kind is None:
        if not creating:
            raise _mount_absent(resolved)
        _tmp_create(resolved)
    elif "w" in mode:
        _tmp_truncate_to(resolved, 0)
    descriptor = _tmp_descriptor_for(
        resolved,
        appending="a" in mode,
        readable="r" in mode or updating,
        writable=creating or updating,
    )
    return _TmpStream(descriptor, mode, text, _tmp_encoding(encoding), errors)


def _tmp_stream_of(descriptor, mode, encoding, errors):
    """The stream a caller opens over a DESCRIPTOR it already holds, as `os.fdopen` does: the
    stream takes the descriptor over, so closing it closes that descriptor."""
    if descriptor in _TMP_DESCRIPTORS:
        return _TmpStream(descriptor, mode, "b" not in mode, _tmp_encoding(encoding), errors)
    if descriptor == _TMP_STDOUT:
        return sys.stdout
    if descriptor == _TMP_STDERR:
        return sys.stderr
    return _mount_unreachable()


def _tmp_descriptor_for(resolved, appending, readable, writable):
    """A descriptor open on the file at `resolved`, which is what every open of the storage
    hands out: the number a caller holds, how far into the file it is, and what it may do.

    An invocation holds at most the descriptors its environment has ([`_TMP_MAX_DESCRIPTORS`]),
    so a run that opens without closing is refused as a process out of descriptors is rather
    than making this process hold them without end.
    """
    descriptor = _tmp_next_descriptor(resolved)
    position = len(_TMP_FILES[resolved]) if appending else 0
    _TMP_DESCRIPTORS[descriptor] = [
        resolved,
        position,
        bool(writable),
        bool(appending),
        bool(readable),
    ]
    return descriptor


def _tmp_os_open(resolved, flags):
    """Open a file of the storage as a DESCRIPTOR, as the operating-system call does."""
    kind = _tmp_kind(resolved)
    if kind == "directory":
        raise IsADirectoryError(errno.EISDIR, "Is a directory", resolved)
    if kind is None:
        if not flags & os.O_CREAT:
            raise _mount_absent(resolved)
        _tmp_create(resolved)
    elif flags & os.O_CREAT and flags & os.O_EXCL:
        raise FileExistsError(errno.EEXIST, "File exists", resolved)
    elif flags & os.O_TRUNC:
        _tmp_truncate_to(resolved, 0)
    return _tmp_descriptor_for(
        resolved,
        appending=bool(flags & os.O_APPEND),
        readable=not flags & os.O_WRONLY,
        writable=bool(
            flags & (os.O_WRONLY | os.O_RDWR | os.O_APPEND | os.O_CREAT | os.O_TRUNC)
        ),
    )


def _tmp_descriptor_file(descriptor):
    """The file such a descriptor is open on, and what it holds."""
    held = _TMP_DESCRIPTORS[descriptor]
    bytes_of = _TMP_FILES.get(held[0])
    if bytes_of is None:
        raise _mount_absent(held[0])
    return held, bytes_of


def _tmp_descriptor_read(descriptor, size):
    """The next `size` bytes of such a descriptor."""
    held, bytes_of = _tmp_descriptor_file(descriptor)
    if not held[4]:
        raise OSError(errno.EBADF, "Bad file descriptor")
    at = held[1]
    read = bytes(bytes_of[at:at + max(0, size)])
    held[1] = at + len(read)
    return read


def _tmp_descriptor_write(descriptor, data):
    """A write on such a descriptor."""
    held, bytes_of = _tmp_descriptor_file(descriptor)
    if not held[2]:
        raise OSError(errno.EBADF, "Bad file descriptor")
    chunk = bytes(data)
    at = len(bytes_of) if held[3] else held[1]
    _tmp_write_at(held[0], at, chunk)
    held[1] = at + len(chunk)
    return len(chunk)


def _tmp_descriptor_lseek(descriptor, position, whence):
    """Move where such a descriptor is reading from."""
    held, bytes_of = _tmp_descriptor_file(descriptor)
    if whence == 1:
        position = held[1] + position
    elif whence == 2:
        position = len(bytes_of) + position
    if position < 0:
        raise OSError(errno.EINVAL, "Invalid argument")
    held[1] = position
    return position


def _fs_stat(path, *, dir_fd=None, follow_symlinks=True):
    """What a `stat` answers for a path of this environment: the storage first, then the
    read-only mounts."""
    resolved = _mount_resolved(path)
    if resolved is not None and _tmp_governs(resolved):
        kind = _tmp_kind(resolved)
        if kind is None:
            raise _mount_absent(resolved)
        return _tmp_stat_result(resolved, kind)
    return _mount_stat(path, dir_fd=dir_fd, follow_symlinks=follow_symlinks)


def _fs_statvfs(path):
    """What a `statvfs` answers for the filesystem the path is on."""
    resolved = _mount_resolved(path)
    if resolved is None:
        return _mount_unreachable()
    if _tmp_governs(resolved):
        if _tmp_kind(resolved) is None:
            raise _mount_absent(resolved)
        return _tmp_statvfs()
    if _mount_governs(resolved):
        if _mount_kind(resolved) is None:
            raise _mount_absent(resolved)
        return _tmp_mount_statvfs()
    return _mount_unreachable()


def _fs_fstatvfs(descriptor):
    """The same, for the filesystem a descriptor's own file is on."""
    if descriptor in _TMP_DESCRIPTORS:
        return _tmp_statvfs()
    if descriptor in _MOUNT_DESCRIPTORS:
        return _tmp_mount_statvfs()
    return _mount_unreachable()


def _fs_listdir(path="."):
    """The names the directory at `path` holds."""
    resolved = _mount_resolved(path)
    if resolved is not None and _tmp_governs(resolved):
        kind = _tmp_kind(resolved)
        if kind is None:
            raise _mount_absent(resolved)
        if kind != "directory":
            raise NotADirectoryError(errno.ENOTDIR, "Not a directory", resolved)
        return _tmp_names(resolved)
    return _mount_listdir(path)


def _fs_scandir(path="."):
    """The entries of the directory at `path`, as objects naming what each one is."""
    resolved = _mount_resolved(path)
    if resolved is not None and _tmp_governs(resolved):
        kind = _tmp_kind(resolved)
        if kind is None:
            raise _mount_absent(resolved)
        if kind != "directory":
            raise NotADirectoryError(errno.ENOTDIR, "Not a directory", resolved)
        return _TmpScan(resolved, _tmp_names(resolved))
    return _mount_scandir(path)


def _fs_access(path, mode, *, dir_fd=None, effective_ids=False, follow_symlinks=True):
    """Whether the path is reachable in the way `mode` asks."""
    resolved = _mount_resolved(path)
    if resolved is not None and _tmp_governs(resolved):
        return _tmp_access(resolved, mode)
    return _mount_access(
        path,
        mode,
        dir_fd=dir_fd,
        effective_ids=effective_ids,
        follow_symlinks=follow_symlinks,
    )


def _fs_open(path, flags, mode=0o777, *, dir_fd=None):
    """Open a path of this environment as a descriptor."""
    resolved = _mount_resolved(path)
    if resolved is not None and _tmp_governs(resolved):
        return _tmp_os_open(resolved, flags)
    return _mount_open(path, flags, mode, dir_fd=dir_fd)


def _fs_read(descriptor, size):
    """The next `size` bytes of a descriptor."""
    if descriptor in _TMP_DESCRIPTORS:
        return _tmp_descriptor_read(descriptor, size)
    if descriptor == _TMP_STDIN:
        # An invocation is given no input stream, so its standard input is at its end.
        return b""
    return _mount_read(descriptor, size)


def _fs_write(descriptor, data):
    """A write on a descriptor: a file of the storage, or one of the output streams."""
    if descriptor in _TMP_DESCRIPTORS:
        return _tmp_descriptor_write(descriptor, data)
    if descriptor in (_TMP_STDOUT, _TMP_STDERR):
        chunk = bytes(data)
        stream = sys.stdout if descriptor == _TMP_STDOUT else sys.stderr
        stream.write(chunk.decode("utf-8", "backslashreplace"))
        return len(chunk)
    return _mount_write(descriptor, data)


def _fs_close(descriptor):
    """Close a descriptor."""
    if descriptor in _TMP_DESCRIPTORS:
        del _TMP_DESCRIPTORS[descriptor]
        return None
    if descriptor in (_TMP_STDIN, _TMP_STDOUT, _TMP_STDERR):
        return None
    return _mount_close(descriptor)


def _fs_lseek(descriptor, position, whence=0):
    """Move where a descriptor is reading from."""
    if descriptor in _TMP_DESCRIPTORS:
        return _tmp_descriptor_lseek(descriptor, position, whence)
    if descriptor in (_TMP_STDIN, _TMP_STDOUT, _TMP_STDERR):
        raise OSError(errno.ESPIPE, "Illegal seek")
    return _mount_lseek(descriptor, position, whence)


def _fs_fstat(descriptor):
    """What a `stat` answers for the file a descriptor is open on."""
    if descriptor in _TMP_DESCRIPTORS:
        return _tmp_stat_result(_TMP_DESCRIPTORS[descriptor][0], "file")
    if descriptor in (_TMP_STDIN, _TMP_STDOUT, _TMP_STDERR):
        return _tmp_stream_stat(descriptor)
    return _mount_fstat(descriptor)


def _fs_ftruncate(descriptor, size):
    """Make the file a descriptor is open on exactly `size` bytes long."""
    if descriptor in _TMP_DESCRIPTORS:
        held, _ = _tmp_descriptor_file(descriptor)
        if not held[2]:
            raise OSError(errno.EBADF, "Bad file descriptor")
        _tmp_truncate_to(held[0], size)
        return None
    if descriptor in _MOUNT_DESCRIPTORS:
        raise OSError(errno.EROFS, _MOUNT_READ_ONLY, _MOUNT_DESCRIPTORS[descriptor][2])
    return _mount_unreachable()


def _fs_truncate(path, size):
    """The same, by path."""
    resolved = _mount_resolved(path)
    if resolved is None:
        return _mount_unreachable()
    if not _tmp_governs(resolved):
        return _tmp_read_only(resolved)
    kind = _tmp_kind(resolved)
    if kind is None:
        raise _mount_absent(resolved)
    if kind != "file":
        raise IsADirectoryError(errno.EISDIR, "Is a directory", resolved)
    _tmp_truncate_to(resolved, size)
    return None


def _fs_fsync(descriptor):
    """Flush a descriptor, which is written where it was written."""
    if descriptor in _TMP_DESCRIPTORS or descriptor in (
        _TMP_STDIN,
        _TMP_STDOUT,
        _TMP_STDERR,
    ):
        return None
    if descriptor in _MOUNT_DESCRIPTORS:
        return None
    return _mount_unreachable()


def _fs_isatty(descriptor):
    """Whether a descriptor is a terminal, which none of this environment's is."""
    return False


def _fs_mkdir(path, mode=0o777, *, dir_fd=None):
    """Make a directory in the storage."""
    resolved = _mount_resolved(path)
    if resolved is None:
        return _mount_unreachable()
    if not _tmp_governs(resolved):
        return _tmp_read_only(resolved)
    if _tmp_kind(resolved) is not None:
        raise FileExistsError(errno.EEXIST, "File exists", resolved)
    if _tmp_kind(_tmp_parent(resolved)) != "directory":
        raise _mount_absent(resolved)
    _tmp_admit(resolved)
    _TMP_DIRECTORIES.add(resolved)
    _TMP_TIMES[resolved] = _tmp_now()
    _TMP_MODES[resolved] = 0o040000 | (mode & 0o7777)
    return None


def _fs_rmdir(path, *, dir_fd=None):
    """Remove an empty directory of the storage."""
    resolved = _mount_resolved(path)
    if resolved is None:
        return _mount_unreachable()
    if not _tmp_governs(resolved):
        return _tmp_read_only(resolved)
    kind = _tmp_kind(resolved)
    if kind is None:
        raise _mount_absent(resolved)
    if kind != "directory":
        raise NotADirectoryError(errno.ENOTDIR, "Not a directory", resolved)
    if resolved == _TMP_ROOT:
        # The root is where the storage is mounted, so it is not an entry anyone removes.
        raise OSError(errno.EBUSY, "Device or resource busy", resolved)
    if _tmp_names(resolved):
        raise OSError(errno.ENOTEMPTY, "Directory not empty", resolved)
    _TMP_DIRECTORIES.discard(resolved)
    _tmp_release(resolved)
    return None


def _fs_unlink(path, *, dir_fd=None):
    """Remove a file of the storage."""
    resolved = _mount_resolved(path)
    if resolved is None:
        return _mount_unreachable()
    if not _tmp_governs(resolved):
        return _tmp_read_only(resolved)
    _tmp_remove(resolved)
    return None


def _fs_rename(source, target, *, src_dir_fd=None, dst_dir_fd=None):
    """Move an entry of the storage to another path of it."""
    from_path = _mount_resolved(source)
    to_path = _mount_resolved(target)
    if from_path is None or to_path is None:
        return _mount_unreachable()
    if not _tmp_governs(to_path):
        return _tmp_read_only(to_path)
    if not _tmp_governs(from_path):
        # Nothing moves OFF a read-only mount: the source would have to be removed from it.
        return _tmp_read_only(from_path)
    kind = _tmp_kind(from_path)
    if kind is None:
        raise _mount_absent(from_path)
    _tmp_named(to_path)
    if _tmp_kind(_tmp_parent(to_path)) != "directory":
        raise _mount_absent(to_path)
    if _tmp_kind(to_path) == "directory":
        raise IsADirectoryError(errno.EISDIR, "Is a directory", to_path)
    if _tmp_kind(to_path) == "file":
        _tmp_remove(to_path)
    moved = [
        path
        for path in list(_TMP_FILES) + list(_TMP_DIRECTORIES)
        if path == from_path or path.startswith(from_path + "/")
    ]
    for path in moved:
        under = to_path + path[len(from_path):]
        # The entry keeps its record, its space and its number — a rename moves a name rather
        # than making one — so what the storage holds for it is unchanged.
        if path in _TMP_FILES:
            _TMP_FILES[under] = _TMP_FILES.pop(path)
        else:
            _TMP_DIRECTORIES.discard(path)
            _TMP_DIRECTORIES.add(under)
        if path in _TMP_TIMES:
            _TMP_TIMES[under] = _TMP_TIMES.pop(path)
        if path in _TMP_MODES:
            _TMP_MODES[under] = _TMP_MODES.pop(path)
        if path in _TMP_INODES:
            _TMP_INODES[under] = _TMP_INODES.pop(path)
    return None


def _fs_utime(path, times=None, *, ns=None, dir_fd=None, follow_symlinks=True):
    """Set when an entry of the storage was last written."""
    resolved = _mount_resolved(path)
    if resolved is None:
        return _mount_unreachable()
    if not _tmp_governs(resolved):
        return _tmp_read_only(resolved)
    if _tmp_kind(resolved) is None:
        raise _mount_absent(resolved)
    if ns is not None:
        _TMP_TIMES[resolved] = ns[1] / 1000000000.0
    elif times is not None:
        _TMP_TIMES[resolved] = times[1]
    else:
        _TMP_TIMES[resolved] = _tmp_now()
    return None


def _fs_chmod(path, mode, *, dir_fd=None, follow_symlinks=True):
    """Set the permissions an entry of the storage carries."""
    resolved = _mount_resolved(path)
    if resolved is None:
        return _mount_unreachable()
    if not _tmp_governs(resolved):
        return _tmp_read_only(resolved)
    kind = _tmp_kind(resolved)
    if kind is None:
        raise _mount_absent(resolved)
    held = _tmp_mode(resolved, kind) & _TMP_TYPE_MASK
    _TMP_MODES[resolved] = held | (mode & 0o7777)
    return None


def _fs_sync():
    """Flush the filesystems of this environment, which is nothing to do: a write of the
    storage is held where it was made, and the read-only mounts are never written."""
    return None


def _fs_pread(descriptor, size, offset):
    """`size` bytes of a descriptor's file from `offset`, without moving where it is."""
    if descriptor in _TMP_DESCRIPTORS:
        held, bytes_of = _tmp_descriptor_file(descriptor)
        if not held[4]:
            raise OSError(errno.EBADF, "Bad file descriptor")
        return bytes(bytes_of[max(0, offset):max(0, offset) + max(0, size)])
    if descriptor in _MOUNT_DESCRIPTORS:
        held = _MOUNT_DESCRIPTORS[descriptor][0]
        return bytes(held[max(0, offset):max(0, offset) + max(0, size)])
    return _mount_unreachable()


def _fs_pwrite(descriptor, data, offset):
    """Write into a descriptor's file at `offset`, without moving where it is."""
    if descriptor in _TMP_DESCRIPTORS:
        held, _ = _tmp_descriptor_file(descriptor)
        if not held[2]:
            raise OSError(errno.EBADF, "Bad file descriptor")
        return _tmp_write_at(held[0], max(0, offset), bytes(data))
    if descriptor in _MOUNT_DESCRIPTORS:
        raise OSError(errno.EROFS, _MOUNT_READ_ONLY, _MOUNT_DESCRIPTORS[descriptor][2])
    return _mount_unreachable()


def _fs_readv(descriptor, buffers):
    """Read a descriptor into the buffers `buffers` names, filling each in turn."""
    read = 0
    for buffer in buffers:
        wanted = len(buffer)
        if wanted <= 0:
            continue
        chunk = _fs_read(descriptor, wanted)
        if not chunk:
            break
        buffer[:len(chunk)] = chunk
        read += len(chunk)
        if len(chunk) < wanted:
            break
    return read


def _fs_writev(descriptor, buffers):
    """Write the buffers `buffers` names to a descriptor, one after the other."""
    return _fs_write(descriptor, b"".join(bytes(buffer) for buffer in buffers))


def _fs_dup(descriptor):
    """A second descriptor on the same open file, which shares where that file is read and
    written from, as a duplicated descriptor does.

    A duplicate is a descriptor of this environment like any other, so it is admitted like one:
    an invocation holds at most the descriptors its environment has.
    """
    if descriptor in _TMP_DESCRIPTORS:
        duplicate = _tmp_next_descriptor()
        # The SAME held entry, so the two descriptors share one position.
        _TMP_DESCRIPTORS[duplicate] = _TMP_DESCRIPTORS[descriptor]
        return duplicate
    if descriptor in _MOUNT_DESCRIPTORS:
        duplicate = _tmp_next_descriptor()
        _MOUNT_DESCRIPTORS[duplicate] = _MOUNT_DESCRIPTORS[descriptor]
        return duplicate
    return _mount_unreachable()


def _tmp_next_descriptor(named=None):
    """The next descriptor number this environment hands out, refused once it holds as many as
    it has ([`_TMP_MAX_DESCRIPTORS`])."""
    if len(_TMP_DESCRIPTORS) + len(_MOUNT_DESCRIPTORS) >= _TMP_MAX_DESCRIPTORS:
        if named is None:
            raise OSError(errno.EMFILE, _TMP_TOO_MANY_FILES)
        raise OSError(errno.EMFILE, _TMP_TOO_MANY_FILES, named)
    descriptor = _MOUNT_NEXT_DESCRIPTOR[0]
    _MOUNT_NEXT_DESCRIPTOR[0] = descriptor + 1
    return descriptor


def _fs_dup2(descriptor, second, inheritable=True):
    """The same, onto the number `second`, which is closed first if it was open."""
    if descriptor not in _TMP_DESCRIPTORS and descriptor not in _MOUNT_DESCRIPTORS:
        return _mount_unreachable()
    if second < 0 or second >= _TMP_MAX_DESCRIPTORS:
        # A number outside the descriptors this environment has is not one it can hold.
        raise OSError(errno.EBADF, "Bad file descriptor")
    held = second in _TMP_DESCRIPTORS or second in _MOUNT_DESCRIPTORS
    if (
        not held
        and len(_TMP_DESCRIPTORS) + len(_MOUNT_DESCRIPTORS) >= _TMP_MAX_DESCRIPTORS
    ):
        raise OSError(errno.EMFILE, _TMP_TOO_MANY_FILES)
    if second in _TMP_DESCRIPTORS:
        del _TMP_DESCRIPTORS[second]
    if second in _MOUNT_DESCRIPTORS:
        del _MOUNT_DESCRIPTORS[second]
    if descriptor in _TMP_DESCRIPTORS:
        _TMP_DESCRIPTORS[second] = _TMP_DESCRIPTORS[descriptor]
    else:
        _MOUNT_DESCRIPTORS[second] = _MOUNT_DESCRIPTORS[descriptor]
    if second >= _MOUNT_NEXT_DESCRIPTOR[0]:
        _MOUNT_NEXT_DESCRIPTOR[0] = second + 1
    return second


def _fs_closerange(first, last):
    """Close every descriptor of this environment in the range, which is what the call is for:
    one that is not open is passed over."""
    for descriptor in list(_TMP_DESCRIPTORS):
        if first <= descriptor <= last:
            del _TMP_DESCRIPTORS[descriptor]
    for descriptor in list(_MOUNT_DESCRIPTORS):
        if first <= descriptor <= last:
            del _MOUNT_DESCRIPTORS[descriptor]
    return None


def _fs_get_blocking(descriptor):
    """Whether a descriptor blocks, which a file of this environment always does: every read
    and write of it is answered from memory and none of them waits."""
    if descriptor in _TMP_DESCRIPTORS or descriptor in _MOUNT_DESCRIPTORS:
        return True
    if descriptor in (_TMP_STDIN, _TMP_STDOUT, _TMP_STDERR):
        return True
    return _mount_unreachable()


def _fs_set_blocking(descriptor, blocking):
    """Ask a descriptor not to block, which changes nothing: none of them ever waits."""
    _fs_get_blocking(descriptor)
    return None


def _fs_get_inheritable(descriptor):
    """Whether a descriptor would be inherited by a child process. This environment starts
    none, and a descriptor it hands out is not inheritable, which is what the language's own
    opens ask for."""
    _fs_get_blocking(descriptor)
    return False


def _fs_set_inheritable(descriptor, inheritable):
    """Ask for a descriptor to be inheritable, which reaches nothing: no process is started
    for one to be inherited by."""
    _fs_get_blocking(descriptor)
    return None


def _fs_posix_fallocate(descriptor, offset, length):
    """Give the file a descriptor is open on the space `offset` and `length` name, which is
    what the call reserves: the file is made that long, and the space is taken off the storage
    exactly as a write of it would be."""
    if descriptor not in _TMP_DESCRIPTORS:
        if descriptor in _MOUNT_DESCRIPTORS:
            raise OSError(errno.EROFS, _MOUNT_READ_ONLY, _MOUNT_DESCRIPTORS[descriptor][2])
        return _mount_unreachable()
    held, bytes_of = _tmp_descriptor_file(descriptor)
    if not held[2]:
        raise OSError(errno.EBADF, "Bad file descriptor")
    wanted = max(0, offset) + max(0, length)
    if wanted > len(bytes_of):
        _tmp_truncate_to(held[0], wanted)
    return None


def _fs_lockf(descriptor, command, length, start=0, whence=0):
    """Take or release an advisory lock on a descriptor's file.

    An execution environment is one process with one invocation in it, so no other holder of a
    lock on a file of this storage exists: a lock asked for is held, and one released was.
    """
    if descriptor in _TMP_DESCRIPTORS:
        return None
    if descriptor in _MOUNT_DESCRIPTORS:
        return None
    return _mount_unreachable()


def _fs_readlink(path, *, dir_fd=None):
    """What the symbolic link at `path` points to.

    Nothing this environment holds is a symbolic link — the storage holds files and directories
    the handler made, and the mounts hold the entries an archive carried — so a path it holds is
    not a link, which is what `EINVAL` says of one.
    """
    resolved = _mount_resolved(path)
    if resolved is None:
        return _mount_unreachable()
    if _tmp_governs(resolved):
        if _tmp_kind(resolved) is None:
            raise _mount_absent(resolved)
        raise OSError(errno.EINVAL, "Invalid argument", resolved)
    if _mount_governs(resolved):
        if _mount_kind(resolved) is None:
            raise _mount_absent(resolved)
        raise OSError(errno.EINVAL, "Invalid argument", resolved)
    return _mount_unreachable()


def _fs_open_file(
    file,
    mode="r",
    buffering=-1,
    encoding=None,
    errors=None,
    newline=None,
    closefd=True,
    opener=None,
):
    """Open a file of this environment, as `open` does: the storage first, a descriptor the
    caller already holds second, and the read-only mounts last."""
    if isinstance(file, int) and not isinstance(file, bool):
        return _tmp_stream_of(file, mode, encoding, errors)
    resolved = _mount_resolved(file)
    if resolved is not None and _tmp_governs(resolved):
        return _tmp_open_file(resolved, mode, encoding, errors)
    return _mount_open_file(
        file, mode, buffering, encoding, errors, newline, closefd, opener
    )


def _tmp_serve():
    """Put the dispatching calls on `os`, and the file opening on `io`, on `_io` and on the
    builtins.

    Every one of them answers a path of the ephemeral storage HERE, a path of a read-only mount
    through the mounts' own call ([`super::guest_mount`]), and a path on neither through that
    module's own refusal — so nothing installed here can reach a file, a directory or a
    descriptor of the host, and the `posix` module `os` is built on keeps every refusal the host
    put on it. The three names a program opens a file through are installed together, because a
    module of the standard library reaches for whichever of them it was written against.
    """
    os.stat = _fs_stat
    os.lstat = _fs_stat
    os.statvfs = _fs_statvfs
    os.fstatvfs = _fs_fstatvfs
    os.listdir = _fs_listdir
    os.scandir = _fs_scandir
    os.access = _fs_access
    os.open = _fs_open
    os.read = _fs_read
    os.write = _fs_write
    os.close = _fs_close
    os.lseek = _fs_lseek
    os.fstat = _fs_fstat
    os.ftruncate = _fs_ftruncate
    os.truncate = _fs_truncate
    os.fsync = _fs_fsync
    os.fdatasync = _fs_fsync
    os.isatty = _fs_isatty
    os.mkdir = _fs_mkdir
    os.rmdir = _fs_rmdir
    os.unlink = _fs_unlink
    os.remove = _fs_unlink
    os.rename = _fs_rename
    os.replace = _fs_rename
    os.utime = _fs_utime
    os.chmod = _fs_chmod
    os.sync = _fs_sync
    os.pread = _fs_pread
    os.pwrite = _fs_pwrite
    os.readv = _fs_readv
    os.writev = _fs_writev
    os.dup = _fs_dup
    os.dup2 = _fs_dup2
    os.closerange = _fs_closerange
    os.get_blocking = _fs_get_blocking
    os.set_blocking = _fs_set_blocking
    os.get_inheritable = _fs_get_inheritable
    os.set_inheritable = _fs_set_inheritable
    os.posix_fallocate = _fs_posix_fallocate
    os.lockf = _fs_lockf
    os.readlink = _fs_readlink
    builtins.open = _fs_open_file
    io.open = _fs_open_file
    __import__("_io").open = _fs_open_file
"##,
            $($runtime)*
        )
    };
}

pub(super) use ephemeral_source;
