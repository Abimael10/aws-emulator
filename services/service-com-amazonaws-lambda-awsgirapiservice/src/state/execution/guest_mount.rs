// SPDX-License-Identifier: CC0-1.0

//! The FILESYSTEM a `python3.x` execution environment has: the mounts a run can read, served
//! from this process's own memory.
//!
//! A Lambda function's files are not the host's and are not general: "Lambda extracts the
//! layer contents into the `/opt` directory" and the deployment package is mounted at the task
//! root, both READ-ONLY. A file one of those mounts holds is an ordinary readable file to the
//! handler — `os.path.getsize('/opt/...')` answers its size, `open('/opt/...')` reads its
//! text, `os.listdir` lists the directory it is in — which is what makes a layer that ships a
//! data file, a certificate bundle or a table usable at all. This module is that filesystem,
//! and the same mounts the import path is resolved from are the ones it serves
//! ([`super::bootstrap`]), so a file is reachable through an import and through a filesystem
//! call because it is ONE held entry either way.
//!
//! It is not a host seam and holds no host call:
//!
//! * the entries are the ones the HOST already read out of the archives this emulator holds
//!   ([`super::package`]) and handed to the run, so a file this answers for is one the
//!   function's own package or one of its layers really carries, and one it does not carry
//!   reads as absent (`ENOENT`);
//! * the calls go on `os`, which is the module a handler reads its filesystem through. The
//!   `posix` module `os` is built on is left exactly as the host narrowed it
//!   ([`super::host_posix`], `super::python`'s `narrow_posix`): every call on it still refuses,
//!   so the containment of the host filesystem stays where the host put it and is not
//!   something this source could give away;
//! * a path that is on NO mount of this function is therefore not served here at all, and
//!   neither is a descriptor this filesystem did not hand out: each is refused by this module
//!   ITSELF (`_mount_unreachable`), with nothing of the calls it replaces kept anywhere in it,
//!   and the one call that answers a boolean rather than raising (`access`) answers that no
//!   such file is reachable, which is what it is. A path above a mount root is such a path
//!   too: a mount is all this filesystem has;
//! * the mounts are READ-ONLY, so every call that would write to one answers `EROFS` the way
//!   the real runtime answers a write under `/var/task` or `/opt`. Nothing is written anywhere:
//!   no host path is opened for a read either, because a file's bytes are the text the archive
//!   carried.
//!
//! The readings a file reports are the mount's own: a read-only mode, the ids a function's
//! files are owned by and the instant the mounts became visible, which is the same reading the
//! node runtime of this crate reports for its own read-only mounts
//! (`super::node::core_modules`'s `fs`).

/// The Python source of the filesystem, which is the FIRST section of the runtime source: it
/// is evaluated in the runtime's own scope, before the part of that source which installs the
/// mounts through it ([`super::bootstrap`], whose `RUNTIME_SOURCE` this macro opens).
///
/// A macro rather than a constant because the two sections are ONE compiled unit — the
/// runtime's own scope — which is what lets the runtime below call the names this section
/// leaves behind: `_mount_install`, which builds the mounts and puts the calls that read a
/// file on `os`, and `_mount_resolved` / `_mount_is_directory` / `_mount_names` /
/// `_mount_text`, which are how the runtime's own path processing collapses a path, asks the
/// mounts about a directory, and lists or reads one. What it is handed is the rest of that
/// source, as a literal.
macro_rules! mount_source {
    ($($runtime:tt)*) => {
        concat!(
            r##"
"""The FILESYSTEM of this execution environment: the mounts a run can read."""

import builtins
import errno
import io
import os

# Every FILE the mounts hold: its absolute path, to the text the archive carried.
_MOUNT_FILES = {}
# Every DIRECTORY they hold: the directories of those files, the roots the mounts are at and
# the directories those roots are under.
_MOUNT_DIRECTORIES = set()
# The roots the mounts are mounted at, which is what says whether a path is on one of them.
_MOUNT_ROOTS = []
# What a file's own size is, kept as it is asked for: the size is the length of the entry's
# bytes, and the entry is held as the text it was read as.
_MOUNT_SIZES = {}
# The number a path's own file is identified by, handed out as paths are asked about, so two
# readings of one path report one file and two paths never report the same.
_MOUNT_INODES = {}
# When the mounts became visible, which is the only time there is for a file that was
# extracted into the environment and never written.
_MOUNT_AT = [0.0]
# The descriptors open on a file of a mount: the fd a caller holds, to the bytes it is reading
# and how far into them it is.
_MOUNT_DESCRIPTORS = {}
# The next descriptor number handed out, above the three a process starts with.
_MOUNT_NEXT_DESCRIPTOR = [3]

# The mode a file and a directory of a read-only mount report: the file type, then read and
# execute for everyone and write for nobody, because nothing under these roots can be written.
_MOUNT_FILE_MODE = 0o100555
_MOUNT_DIRECTORY_MODE = 0o040555
# The type bits of a mode, and the type a directory is.
_MOUNT_TYPE_MASK = 0o170000
_MOUNT_DIRECTORY_TYPE = 0o040000
# Who a function's own files belong to, and the block size a filesystem reports.
_MOUNT_UID = 993
_MOUNT_GID = 990
_MOUNT_BLOCK_SIZE = 4096
# The flags that would WRITE: a mount is read-only, so an open naming one of them is refused.
_MOUNT_WRITE_FLAGS = os.O_WRONLY | os.O_RDWR | os.O_APPEND | os.O_CREAT | os.O_TRUNC
# Said of a write to a read-only mount, as the operating system says it.
_MOUNT_READ_ONLY = "Read-only file system"


def _mount_install(mounts, mounted_at):
    """Build the mounts of this execution environment and serve them on `os`.

    `mounts` is each mount as the directory it is mounted at and the entries it holds, keyed
    by their own path inside it; `mounted_at` is the instant they became visible, which is
    every file's own time.
    """
    for root, entries in mounts:
        if not root:
            continue
        if root not in _MOUNT_ROOTS:
            _MOUNT_ROOTS.append(root)
        # The root itself is a directory of this environment; the directories ABOVE it are
        # not. A mount is the only thing this filesystem has, so a path outside one is the
        # host's whether or not it happens to be a parent of a mount.
        _MOUNT_DIRECTORIES.add(root)
        for path in entries:
            if not path:
                continue
            parts = path.split("/")
            walked = root
            for part in parts[:-1]:
                if not part:
                    continue
                walked = walked + "/" + part
                _MOUNT_DIRECTORIES.add(walked)
            _MOUNT_FILES[walked + "/" + parts[-1]] = entries[path]
    _MOUNT_AT[0] = mounted_at
    _mount_serve()


def _mount_resolved(path):
    """`path` as one absolute path with no `.` or `..` left in it, or `None` when it is not a
    path at all — a descriptor, or an object that names no path."""
    try:
        named = os.fspath(path)
    except TypeError:
        return None
    if isinstance(named, bytes):
        try:
            named = named.decode("utf-8")
        except UnicodeDecodeError:
            return None
    if not isinstance(named, str):
        return None
    if not named.startswith("/"):
        named = os.getcwd() + "/" + named
    walked = []
    for part in named.split("/"):
        if not part or part == ".":
            continue
        if part == "..":
            if walked:
                walked.pop()
            continue
        walked.append(part)
    return "/" + "/".join(walked)


def _mount_kind(resolved):
    """What `resolved` IS on the mounts: a file, a directory, or nothing they hold."""
    if resolved in _MOUNT_FILES:
        return "file"
    if resolved in _MOUNT_DIRECTORIES:
        return "directory"
    return None


def _mount_governs(resolved):
    """Whether `resolved` is a path of this environment's own mounts.

    A path one of them holds is, and so is one UNDER a root that holds nothing — a file of the
    deployment package or of a layer that was never shipped is absent from the environment,
    not a path of the host. Every other path is the host's, and this module answers none of
    them.
    """
    if _mount_kind(resolved) is not None:
        return True
    for root in _MOUNT_ROOTS:
        if resolved.startswith(root + "/"):
            return True
    return False


def _mount_bytes(resolved):
    """The bytes the file at `resolved` holds."""
    return _MOUNT_FILES[resolved].encode("utf-8")


def _mount_size(resolved):
    """How many bytes the file at `resolved` is, kept once it has been asked for."""
    if resolved not in _MOUNT_SIZES:
        _MOUNT_SIZES[resolved] = len(_mount_bytes(resolved))
    return _MOUNT_SIZES[resolved]


def _mount_text(resolved):
    """The text of the file at `resolved`, or `None` when the mounts hold no such file.

    How the runtime's own path processing reads a file of a mount (a path-configuration file
    of a layer's site directory), rather than going through the calls below.
    """
    return _MOUNT_FILES.get(resolved)


def _mount_is_directory(resolved):
    """Whether the mounts hold a directory at `resolved`."""
    return resolved in _MOUNT_DIRECTORIES


def _mount_inode(resolved):
    """The number the file at `resolved` is identified by, which is its own and no other
    path's."""
    if resolved not in _MOUNT_INODES:
        _MOUNT_INODES[resolved] = len(_MOUNT_INODES) + 1
    return _MOUNT_INODES[resolved]


def _mount_names(resolved):
    """The names directly inside the directory at `resolved`, in order."""
    under = resolved + "/" if resolved != "/" else "/"
    named = set()
    for held in list(_MOUNT_FILES) + list(_MOUNT_DIRECTORIES):
        if not held.startswith(under):
            continue
        name = held[len(under):].partition("/")[0]
        if name:
            named.add(name)
    return sorted(named)


def _mount_absent(resolved):
    return FileNotFoundError(errno.ENOENT, "No such file or directory", resolved)


def _mount_unreachable(*arguments, **keywords):
    """What a path on NO mount of this function, and a descriptor this filesystem did not hand
    out, are answered with: there is no host filesystem behind this execution environment, so
    there is no such file, directory or descriptor to reach.

    Every gate below answers through THIS rather than through a call of its own: nothing of the
    host's own file calls is kept anywhere in this module, so no call through it can reach a
    file, a directory or a descriptor of the host.
    """
    raise OSError(
        errno.ENOSYS, "a Lambda function in this emulator has no host filesystem to reach"
    )


def _mount_stat(path, *, dir_fd=None, follow_symlinks=True):
    """What a `stat` answers for a path of the mounts.

    Nothing under these roots is a symbolic link, so following links or not is the same
    reading, which is why this answers `lstat` as well.
    """
    resolved = _mount_resolved(path)
    if resolved is None or not _mount_governs(resolved):
        return _mount_unreachable()
    kind = _mount_kind(resolved)
    if kind is None:
        raise _mount_absent(resolved)
    return _mount_stat_result(resolved, kind)


def _mount_stat_result(resolved, kind):
    """The `stat_result` the file or directory at `resolved` reports."""
    directory = kind == "directory"
    size = 0 if directory else _mount_size(resolved)
    at = _MOUNT_AT[0]
    nanoseconds = int(at * 1000000000.0)
    return os.stat_result(
        (
            _MOUNT_DIRECTORY_MODE if directory else _MOUNT_FILE_MODE,
            _mount_inode(resolved),
            0,
            1,
            _MOUNT_UID,
            _MOUNT_GID,
            size,
            at,
            at,
            at,
        ),
        {
            "st_atime_ns": nanoseconds,
            "st_mtime_ns": nanoseconds,
            "st_ctime_ns": nanoseconds,
            "st_blksize": _MOUNT_BLOCK_SIZE,
            "st_blocks": (size + 511) // 512,
        },
    )


def _mount_listdir(path="."):
    """The names the directory at `path` holds."""
    resolved = _mount_resolved(path)
    if resolved is None or not _mount_governs(resolved):
        return _mount_unreachable()
    kind = _mount_kind(resolved)
    if kind is None:
        raise _mount_absent(resolved)
    if kind != "directory":
        raise NotADirectoryError(errno.ENOTDIR, "Not a directory", resolved)
    return _mount_names(resolved)


class _MountEntry:
    """One entry of a directory of the mounts, as `scandir` answers them."""

    def __init__(self, directory, name):
        self.name = name
        self.path = directory + "/" + name if directory != "/" else "/" + name

    def __repr__(self):
        return "<DirEntry %r>" % (self.name,)

    def __fspath__(self):
        return self.path

    def inode(self):
        return _mount_inode(self.path)

    def is_dir(self, *, follow_symlinks=True):
        return _mount_kind(self.path) == "directory"

    def is_file(self, *, follow_symlinks=True):
        return _mount_kind(self.path) == "file"

    def is_symlink(self):
        return False

    def stat(self, *, follow_symlinks=True):
        kind = _mount_kind(self.path)
        if kind is None:
            raise _mount_absent(self.path)
        return _mount_stat_result(self.path, kind)


class _MountScan:
    """The entries of one directory of the mounts, as the iterator `scandir` answers."""

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
        return _MountEntry(self._directory, name)

    def __enter__(self):
        return self

    def __exit__(self, *exception):
        self.close()
        return False

    def close(self):
        self._names = []
        self._at = 0


def _mount_scandir(path="."):
    """The entries of the directory at `path`, as objects naming what each one is."""
    resolved = _mount_resolved(path)
    if resolved is None or not _mount_governs(resolved):
        return _mount_unreachable()
    kind = _mount_kind(resolved)
    if kind is None:
        raise _mount_absent(resolved)
    if kind != "directory":
        raise NotADirectoryError(errno.ENOTDIR, "Not a directory", resolved)
    return _MountScan(resolved, _mount_names(resolved))


def _mount_access(path, mode, *, dir_fd=None, effective_ids=False, follow_symlinks=True):
    """Whether the path is reachable in the way `mode` asks.

    An access check answers a boolean rather than raising, for every path: a file of a mount
    can be read and executed and never written, and a path that is on no mount of this
    function is reachable in no way at all — there is no host filesystem for it to be on.
    """
    resolved = _mount_resolved(path)
    if resolved is None or _mount_kind(resolved) is None:
        return False
    return not mode & os.W_OK


def _mount_open_file(
    file,
    mode="r",
    buffering=-1,
    encoding=None,
    errors=None,
    newline=None,
    closefd=True,
    opener=None,
):
    """Open a file of the mounts for reading, as `open` does.

    A read answers a stream over the bytes the archive carried; a mode that would write
    answers for the mount being read-only, exactly as a write under the task root or the layer
    root does on the real runtime. A path on no mount is refused HERE rather than handed on:
    nothing of the real file opening is kept anywhere in this module, so no call through it can
    reach a file of the host.
    """
    resolved = _mount_resolved(file)
    if resolved is None or not _mount_governs(resolved):
        return _mount_unreachable()
    if any(character in mode for character in "wax+"):
        raise OSError(errno.EROFS, _MOUNT_READ_ONLY, resolved)
    kind = _mount_kind(resolved)
    if kind is None:
        raise _mount_absent(resolved)
    if kind != "file":
        raise IsADirectoryError(errno.EISDIR, "Is a directory", resolved)
    held = io.BytesIO(_mount_bytes(resolved))
    if "b" in mode:
        return held
    return io.TextIOWrapper(held, encoding or "utf-8", errors, newline)


def _mount_open(path, flags, mode=0o777, *, dir_fd=None):
    """Open a file of the mounts as a DESCRIPTOR, as the operating-system call does."""
    resolved = _mount_resolved(path)
    if resolved is None or not _mount_governs(resolved):
        return _mount_unreachable()
    if flags & _MOUNT_WRITE_FLAGS:
        raise OSError(errno.EROFS, _MOUNT_READ_ONLY, resolved)
    kind = _mount_kind(resolved)
    if kind is None:
        raise _mount_absent(resolved)
    if kind != "file":
        raise IsADirectoryError(errno.EISDIR, "Is a directory", resolved)
    descriptor = _MOUNT_NEXT_DESCRIPTOR[0]
    _MOUNT_NEXT_DESCRIPTOR[0] = descriptor + 1
    _MOUNT_DESCRIPTORS[descriptor] = [_mount_bytes(resolved), 0, resolved]
    return descriptor


def _mount_read(descriptor, size):
    """The next `size` bytes of a descriptor open on a file of the mounts."""
    if descriptor not in _MOUNT_DESCRIPTORS:
        return _mount_unreachable()
    held = _MOUNT_DESCRIPTORS[descriptor]
    at = held[1]
    read = held[0][at:at + max(0, size)]
    held[1] = at + len(read)
    return read


def _mount_write(descriptor, data):
    """A write on such a descriptor, which the mount being read-only refuses."""
    if descriptor not in _MOUNT_DESCRIPTORS:
        return _mount_unreachable()
    raise OSError(errno.EROFS, _MOUNT_READ_ONLY, _MOUNT_DESCRIPTORS[descriptor][2])


def _mount_close(descriptor):
    """Close such a descriptor."""
    if descriptor not in _MOUNT_DESCRIPTORS:
        return _mount_unreachable()
    del _MOUNT_DESCRIPTORS[descriptor]
    return None


def _mount_lseek(descriptor, position, whence=0):
    """Move where such a descriptor is reading from."""
    if descriptor not in _MOUNT_DESCRIPTORS:
        return _mount_unreachable()
    held = _MOUNT_DESCRIPTORS[descriptor]
    if whence == 1:
        position = held[1] + position
    elif whence == 2:
        position = len(held[0]) + position
    if position < 0:
        raise OSError(errno.EINVAL, "Invalid argument")
    held[1] = position
    return position


def _mount_fstat(descriptor):
    """What a `stat` answers for the file such a descriptor is open on."""
    if descriptor not in _MOUNT_DESCRIPTORS:
        return _mount_unreachable()
    resolved = _MOUNT_DESCRIPTORS[descriptor][2]
    return _mount_stat_result(resolved, "file")


def _mount_serve():
    """Put the calls that read a file of the mounts on `os`, and the file opening on `io` and
    on the builtins.

    What is replaced is what `os` offers, which is the filesystem a handler reads through. The
    `posix` module `os` is built on keeps every refusal the host installed on it, and NOTHING
    of the calls being replaced is kept here: a path on no mount of this function, and a
    descriptor this filesystem did not hand out, are refused by this module itself
    (`_mount_unreachable`), so no call through what is installed here can reach a file, a
    directory or a descriptor of the host.
    """
    os.stat = _mount_stat
    os.lstat = _mount_stat
    os.listdir = _mount_listdir
    os.scandir = _mount_scandir
    os.access = _mount_access
    os.open = _mount_open
    os.read = _mount_read
    os.write = _mount_write
    os.close = _mount_close
    os.lseek = _mount_lseek
    os.fstat = _mount_fstat
    builtins.open = _mount_open_file
    io.open = _mount_open_file
"##,
            $($runtime)*
        )
    };
}

pub(super) use mount_source;
