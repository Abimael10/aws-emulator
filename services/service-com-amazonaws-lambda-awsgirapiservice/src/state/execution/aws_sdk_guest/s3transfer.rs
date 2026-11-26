// SPDX-License-Identifier: CC0-1.0

//! `s3transfer`: the MANAGED TRANSFER library the AWS SDK for Python is installed with, which
//! a managed `python3.x` runtime therefore carries on a handler's import path.
//!
//! The SDK is installed as a DISTRIBUTION with dependencies of its own, and a managed runtime
//! preinstalls that whole installation rather than the two top-level packages: `import
//! s3transfer` resolves in a managed runtime with nothing in the deployment package. It also
//! resolves for a package that bundles the SDK itself — the deployment package's own copy of
//! `boto3` wins over the runtime's, while a dependency the package did not bundle is still
//! resolved from the runtime behind it — which is what the SDK's own S3 customizations need,
//! because `boto3.s3.transfer` imports this library's manager, its executor, its subscriber
//! base and its file utilities.
//!
//! What these modules do and do not do:
//!
//! * A transfer is REAL: `upload` reads the bytes and calls the client's `PutObject` — or
//!   `CreateMultipartUpload`, `UploadPart` and `CompleteMultipartUpload` for an object over
//!   the configured threshold — `download` streams what `GetObject` answered to the file,
//!   `copy` calls `CopyObject` (or copies the parts) and `delete` calls `DeleteObject`. Every
//!   byte a caller reads back came from the emulated service's own answer, carried by the
//!   client the manager was built with ([`super`]), so nothing here synthesises a transfer and
//!   a call the service refuses is raised to the caller.
//! * The SURFACE is the library's: the manager and its configuration, the future a transfer
//!   answers with and the metadata on it, the subscriber hooks (`on_queued`, `on_progress`,
//!   `on_done`), the exception types a caller catches, the file utilities and the executors —
//!   so the SDK's own `upload_file`/`download_file`/`copy` and a handler written against the
//!   published library run unchanged.
//! * A transfer runs where it is SUBMITTED. This runtime runs one function on one thread
//!   ([`super::super::python`]), so there is no worker pool to hand the work to: the manager
//!   performs it and the future it answers is already done, with a failure held on the future
//!   and raised by `result()` the way a pool's would be. The concurrency members of the
//!   configuration are therefore read and validated but bound nothing.
//! * Nothing here is the containment. Which services can be reached, the bounds on a call and
//!   the signature it carries are the host's ([`super::super::aws_api`]), and the files a
//!   transfer reads and writes are the ones the environment's own filesystem serves
//!   ([`super::super::guest_mount`]).

/// The modules of the managed transfer library, under the paths the package holds them at.
#[must_use]
pub(super) fn modules() -> Vec<(&'static str, &'static str)> {
    vec![
        ("s3transfer/__init__.py", INIT_SOURCE),
        ("s3transfer/compat.py", COMPAT_SOURCE),
        ("s3transfer/constants.py", CONSTANTS_SOURCE),
        ("s3transfer/exceptions.py", EXCEPTIONS_SOURCE),
        ("s3transfer/subscribers.py", SUBSCRIBERS_SOURCE),
        ("s3transfer/utils.py", UTILS_SOURCE),
        ("s3transfer/futures.py", FUTURES_SOURCE),
        ("s3transfer/manager.py", MANAGER_SOURCE),
    ]
}

/// `s3transfer`: the names the package exports, and the single-threaded transfer interface the
/// library kept at its root before the manager.
const INIT_SOURCE: &str = r#"
"""The managed transfer library the AWS SDK for Python is installed with."""

from s3transfer.constants import MB, VERSION
from s3transfer.exceptions import (
    RetriesExceededError,
    S3UploadFailedError,
)
from s3transfer.manager import TransferConfig as _ManagedConfig
from s3transfer.manager import TransferManager
from s3transfer.utils import OSUtils, ReadFileChunk, random_file_extension

__author__ = "Amazon Web Services"

# The GENERATION of the library these modules implement, which is the one a managed
# `python3.x` runtime ships beside its SDK. It is not a published release of the library: no
# release of one is here to name.
__version__ = VERSION

# The sizes the library's own root module has always published, which a caller reads to size
# its own configuration.
MIN_UPLOAD_CHUNKSIZE = 5 * MB
MAX_SINGLE_UPLOAD_SIZE = 5 * 1024 * MB
MAX_PARTS = 10000


class TransferConfig:
    """The configuration of the root module's own transfer interface.

    The members are the ones that interface has always taken; what they mean is what the
    manager's own configuration means, and that is what a transfer is run under
    (`to_managed`).
    """

    def __init__(
        self,
        multipart_threshold=8 * MB,
        max_concurrency=10,
        multipart_chunksize=8 * MB,
        num_download_attempts=5,
        max_io_queue=100,
    ):
        self.multipart_threshold = multipart_threshold
        self.max_concurrency = max_concurrency
        self.multipart_chunksize = multipart_chunksize
        self.num_download_attempts = num_download_attempts
        self.max_io_queue = max_io_queue

    def to_managed(self):
        """The manager's configuration these members state."""
        return _ManagedConfig(
            multipart_threshold=self.multipart_threshold,
            multipart_chunksize=self.multipart_chunksize,
            max_request_concurrency=self.max_concurrency,
            num_download_attempts=self.num_download_attempts,
            max_io_queue_size=self.max_io_queue,
        )


class S3Transfer:
    """One file up and one file down, as the library's root module has always offered them.

    The transfer itself is the manager's, so a file sent through this interface and one sent
    through the manager are the same calls on the same client.
    """

    ALLOWED_DOWNLOAD_ARGS = TransferManager.ALLOWED_DOWNLOAD_ARGS
    ALLOWED_UPLOAD_ARGS = TransferManager.ALLOWED_UPLOAD_ARGS

    def __init__(self, client, config=None, osutil=None):
        self._client = client
        self._config = config if config is not None else TransferConfig()
        self._osutil = osutil if osutil is not None else OSUtils()

    def _manager(self):
        config = self._config
        if isinstance(config, TransferConfig):
            config = config.to_managed()
        return TransferManager(self._client, config, self._osutil)

    def upload_file(self, filename, bucket, key, callback=None, extra_args=None):
        """Send `filename` to `bucket`/`key`."""
        subscribers = _callback_subscribers(callback)
        with self._manager() as manager:
            future = manager.upload(filename, bucket, key, extra_args, subscribers)
            try:
                future.result()
            except Exception as error:
                raise S3UploadFailedError(
                    "Failed to upload %s to %s: %s" % (filename, "/".join([bucket, key]), error)
                )

    def download_file(self, bucket, key, filename, extra_args=None, callback=None):
        """Read `bucket`/`key` into `filename`."""
        subscribers = _callback_subscribers(callback)
        with self._manager() as manager:
            future = manager.download(bucket, key, filename, extra_args, subscribers)
            future.result()


def _callback_subscribers(callback):
    """The subscribers a caller's progress `callback` is invoked through, or none."""
    if callback is None:
        return None
    from s3transfer.subscribers import BaseSubscriber

    class _ProgressSubscriber(BaseSubscriber):
        def on_progress(self, future, bytes_transferred, **kwargs):
            callback(bytes_transferred)

    return [_ProgressSubscriber()]
"#;

/// `s3transfer.compat`: what the library reads of the interpreter it is running on.
const COMPAT_SOURCE: &str = r#"
"""What this library reads of the interpreter it runs on."""

import os
import sys

# The largest integer a bound of this library is expressed with.
MAXINT = sys.maxsize

# The error a transport failure arrives as, which a retry decides on.
SOCKET_ERROR = ConnectionError


def seekable(fileobj):
    """Whether `fileobj` can be read from a position a caller chooses."""
    if not hasattr(fileobj, "seekable"):
        return hasattr(fileobj, "seek") and hasattr(fileobj, "tell")
    try:
        return fileobj.seekable()
    except Exception:
        return False


def readable(fileobj):
    """Whether `fileobj` can be read at all."""
    if not hasattr(fileobj, "readable"):
        return hasattr(fileobj, "read")
    try:
        return fileobj.readable()
    except Exception:
        return False


def fallocate(fileobj, size):
    """Make `fileobj` hold `size` bytes, which is what a download writing parts out of order
    needs of the file it writes into."""
    if hasattr(os, "posix_fallocate"):
        os.posix_fallocate(fileobj.fileno(), 0, size)
    else:
        fileobj.truncate(size)


def rename_file(current_filename, new_filename):
    """Move `current_filename` to `new_filename`, which is how a download that wrote under a
    temporary name puts the file at the name the caller asked for."""
    os.rename(current_filename, new_filename)


def accepts_kwargs(function):
    """Whether `function` takes keyword arguments it does not name, which is what a
    subscriber's own methods are called with."""
    import inspect

    try:
        signature = inspect.signature(function)
    except (TypeError, ValueError):
        return False
    for parameter in signature.parameters.values():
        if parameter.kind == inspect.Parameter.VAR_KEYWORD:
            return True
    return False
"#;

/// `s3transfer.constants`: the sizes a transfer is measured in and the member names it is
/// allowed to carry.
const CONSTANTS_SOURCE: &str = r#"
"""The sizes and the member names a managed transfer is bounded by."""

KB = 1024
MB = KB * KB
GB = MB * KB

# The GENERATION of the library this installation ships (see `s3transfer.__version__`).
VERSION = "1.0.0"

USER_AGENT = "s3transfer/" + VERSION
PROCESS_USER_AGENT = USER_AGENT + " processpool"

# The members of GetObject a managed download may carry: the ones that name WHICH bytes of the
# object to read and who is asking, rather than how a get is answered.
#
# Source: the members of the Amazon S3 model's `GetObjectRequest` the library has always
# allowed on a managed download.
ALLOWED_DOWNLOAD_ARGS = [
    "ChecksumMode",
    "VersionId",
    "SSECustomerAlgorithm",
    "SSECustomerKey",
    "SSECustomerKeyMD5",
    "RequestPayer",
    "ExpectedBucketOwner",
]

# The checksum members that describe the WHOLE object rather than one part of it, so an upload
# carrying one of them cannot be sent as several parts.
#
# Source: the `Checksum*` members of the Amazon S3 model's `PutObjectRequest`.
FULL_OBJECT_CHECKSUM_ARGS = [
    "ChecksumCRC32",
    "ChecksumCRC32C",
    "ChecksumCRC64NVME",
    "ChecksumSHA1",
    "ChecksumSHA256",
]
"#;

/// `s3transfer.exceptions`: the errors a managed transfer raises, under the names a caller
/// catches.
const EXCEPTIONS_SOURCE: &str = r#"
"""The errors a managed transfer raises."""

from concurrent.futures import CancelledError


class RetriesExceededError(Exception):
    """Every attempt at one piece of a transfer failed."""

    def __init__(self, last_exception, msg="Max Retries Exceeded"):
        Exception.__init__(self, msg)
        self.last_exception = last_exception


class S3UploadFailedError(Exception):
    """An upload this library managed did not complete."""


class S3DownloadFailedError(Exception):
    """A download this library managed did not complete."""


class InvalidSubscriberMethodError(Exception):
    """A subscriber declared a method this library never calls."""


class TransferNotDoneError(Exception):
    """Something only a finished transfer answers was asked of one still running."""


class FatalError(CancelledError):
    """A transfer stopped for a failure rather than for a caller's cancellation."""
"#;

/// `s3transfer.subscribers`: the hooks a caller is told about a transfer through.
const SUBSCRIBERS_SOURCE: &str = r#"
"""What a caller is told about a transfer as it runs."""

from s3transfer.compat import accepts_kwargs
from s3transfer.exceptions import InvalidSubscriberMethodError


class BaseSubscriber:
    """The hooks a transfer invokes, which a caller subclasses to be told about one.

    A subclass is checked when it is CONSTRUCTED rather than when a transfer first invokes it,
    because a method of the wrong shape is a mistake in the caller's own code: every hook is
    invoked with keyword arguments this library may add to, so one that does not accept them
    is refused here instead of raising in the middle of a transfer.
    """

    VALID_SUBSCRIBER_TYPES = ["queued", "progress", "done"]

    def __new__(cls, *args, **kwargs):
        cls._validate_subscriber_methods()
        return super().__new__(cls)

    @classmethod
    def _validate_subscriber_methods(cls):
        for subscriber_type in cls.VALID_SUBSCRIBER_TYPES:
            subscriber_method = getattr(cls, "on_" + subscriber_type)
            if not callable(subscriber_method):
                raise InvalidSubscriberMethodError(
                    "Subscriber method %s must be callable." % subscriber_method
                )
            if not accepts_kwargs(subscriber_method):
                raise InvalidSubscriberMethodError(
                    "Subscriber method %s must accept keyword arguments (**kwargs)"
                    % subscriber_method
                )

    def on_queued(self, future, **kwargs):
        """The transfer has been submitted and nothing of it has been transferred yet."""

    def on_progress(self, future, bytes_transferred, **kwargs):
        """`bytes_transferred` more bytes of the transfer have moved."""

    def on_done(self, future, **kwargs):
        """The transfer finished, whether it succeeded, failed or was cancelled."""
"#;

/// `s3transfer.utils`: the file reading, the call arguments and the small helpers every
/// transfer is built out of.
const UTILS_SOURCE: &str = r#"
"""The files a transfer reads and writes, and the pieces it is built out of."""

import os
import random
import stat
import string
import threading

from s3transfer.compat import SOCKET_ERROR, fallocate, rename_file, seekable

# How many parts one multipart transfer may be sent as, which is what a part size is raised
# to stay inside.
#
# Source: the Amazon S3 model's `PartNumber` member, whose documented range ends at 10,000.
MAX_PARTS = 10000

# The failures of a read that a download attempts again rather than giving up on.
S3_RETRYABLE_DOWNLOAD_ERRORS = (SOCKET_ERROR,)


def random_file_extension(num_digits=8):
    """A suffix for the file a download writes before it is renamed into place."""
    return "".join(random.choice(string.hexdigits) for _ in range(num_digits))


def signal_not_transferring(request, operation_name, **kwargs):
    """Stop counting the body of `request` as transferred, which is what a retried request
    does before it is sent again."""
    if operation_name in ["PutObject", "UploadPart"] and hasattr(
        request.body, "signal_not_transferring"
    ):
        request.body.signal_not_transferring()


def signal_transferring(request, operation_name, **kwargs):
    """Count the body of `request` as transferred again."""
    body = getattr(request, "body", None)
    if operation_name in ["PutObject", "UploadPart"] and hasattr(
        body, "signal_transferring"
    ):
        body.signal_transferring()


def calculate_num_parts(size, part_size):
    """How many parts an object of `size` bytes is sent as at `part_size` each."""
    return int(size // part_size) + (1 if size % part_size else 0)


def calculate_range_parameter(part_size, part_index, num_parts, total_size=None):
    """The `Range` header one part of a ranged read asks for."""
    start_range = part_index * part_size
    if part_index == num_parts - 1:
        end_range = ""
        if total_size is not None:
            end_range = str(total_size - 1)
    else:
        end_range = str(start_range + part_size - 1)
    return "bytes=%s-%s" % (start_range, end_range)


def get_callbacks(transfer_future, callback_type):
    """The `callback_type` hooks of every subscriber of `transfer_future`, as callables."""
    callbacks = []
    subscribers = transfer_future.meta.call_args.subscribers or []
    for subscriber in subscribers:
        callback_name = "on_" + callback_type
        if getattr(subscriber, callback_name, None):
            callbacks.append(
                FunctionContainer(
                    getattr(subscriber, callback_name), future=transfer_future
                )
            )
    return callbacks


def invoke_progress_callbacks(callbacks, bytes_transferred):
    """Tell each of `callbacks` that `bytes_transferred` more bytes moved.

    A call that moved nothing is not reported: no progress was made.
    """
    if bytes_transferred:
        for callback in callbacks:
            callback(bytes_transferred=bytes_transferred)


class CallArgs:
    """The arguments one transfer was asked for, as the future reports them."""

    def __init__(self, **kwargs):
        for arg, value in kwargs.items():
            setattr(self, arg, value)


class FunctionContainer:
    """A call held with its arguments, made later."""

    def __init__(self, func, *args, **kwargs):
        self._func = func
        self._args = args
        self._kwargs = kwargs

    def __repr__(self):
        return "Function: %s with args %s and kwargs %s" % (
            self._func,
            self._args,
            self._kwargs,
        )

    def __call__(self, *args, **kwargs):
        merged = dict(self._kwargs)
        merged.update(kwargs)
        return self._func(*(self._args + args), **merged)


class CountCallbackInvoker:
    """One call made when the last of several things being waited on finishes."""

    def __init__(self, callback):
        self._lock = threading.Lock()
        self._callback = callback
        self._count = 0
        self._is_finalized = False

    @property
    def current_count(self):
        with self._lock:
            return self._count

    def increment(self):
        with self._lock:
            if self._is_finalized:
                raise RuntimeError("Counter has been finalized it can no longer be incremented.")
            self._count += 1

    def decrement(self):
        with self._lock:
            if self._count == 0:
                raise RuntimeError("Counter is at zero. It cannot dip below zero")
            self._count -= 1
            if self._is_finalized and self._count == 0:
                self._callback()

    def finalize(self):
        with self._lock:
            self._is_finalized = True
            if self._count == 0:
                self._callback()


class StreamReaderProgress:
    """A stream that reports what was read of it as progress."""

    def __init__(self, stream, callbacks=None):
        self._stream = stream
        self._callbacks = callbacks if callbacks is not None else []

    def read(self, *args, **kwargs):
        value = self._stream.read(*args, **kwargs)
        invoke_progress_callbacks(self._callbacks, len(value))
        return value

    def __getattr__(self, name):
        return getattr(self._stream, name)


class OSUtils:
    """The FILES a transfer reads and writes, as the library reaches them.

    Every call here is the execution environment's own filesystem: a file a transfer reads is
    a file of the deployment package, of a layer or of the ephemeral storage, and nothing
    outside them is opened.
    """

    _MAX_FILENAME_LEN = 255

    def get_file_size(self, filename):
        return os.path.getsize(filename)

    def open_file_chunk_reader(self, filename, start_byte, size, callbacks):
        return ReadFileChunk.from_filename(
            filename, start_byte, size, callbacks, enable_callbacks=False
        )

    def open_file_chunk_reader_from_fileobj(
        self,
        fileobj,
        chunk_size,
        full_file_size,
        callbacks,
        close_callbacks=None,
    ):
        return ReadFileChunk(
            fileobj,
            chunk_size,
            full_file_size,
            callbacks=callbacks,
            enable_callbacks=False,
            close_callbacks=close_callbacks,
        )

    def open(self, filename, mode):
        return open(filename, mode)

    def remove_file(self, filename):
        """Delete `filename`, and say nothing where it is already gone."""
        try:
            os.remove(filename)
        except OSError:
            pass

    def rename_file(self, current_filename, new_filename):
        rename_file(current_filename, new_filename)

    def is_special_file(self, filename):
        """Whether `filename` is a file whose size says nothing about what it will yield — a
        device, a socket or a pipe — which a transfer cannot read in parts."""
        try:
            mode = os.stat(filename).st_mode
        except OSError:
            return False
        if stat.S_ISCHR(mode) or stat.S_ISBLK(mode):
            return True
        if stat.S_ISFIFO(mode) or stat.S_ISSOCK(mode):
            return True
        return False

    def get_temp_filename(self, filename):
        """The name a download writes under before it is renamed to `filename`."""
        directory = os.path.dirname(filename)
        name = os.path.basename(filename)
        temporary = name + os.extsep + random_file_extension()
        if len(temporary) > self._MAX_FILENAME_LEN:
            temporary = temporary[-self._MAX_FILENAME_LEN :]
        return os.path.join(directory, temporary)

    def allocate(self, filename, size):
        try:
            with self.open(filename, "wb") as writing:
                fallocate(writing, size)
        except OSError:
            self.remove_file(filename)
            raise


class ReadFileChunk:
    """ONE part of a file, read as a stream of its own.

    It is what a multipart upload sends as a part's body: a reader bounded to `chunk_size`
    bytes from the position it was opened at, which reports what was read of it as progress.
    """

    def __init__(
        self,
        fileobj,
        chunk_size,
        full_file_size,
        callbacks=None,
        enable_callbacks=True,
        close_callbacks=None,
    ):
        self._fileobj = fileobj
        self._start_byte = self._fileobj.tell() if seekable(self._fileobj) else 0
        self._size = self._calculate_file_size(
            self._fileobj,
            requested_size=chunk_size,
            start_byte=self._start_byte,
            actual_file_size=full_file_size,
        )
        self._amount_read = 0
        self._callbacks = callbacks if callbacks is not None else []
        self._callbacks_enabled = enable_callbacks
        self._close_callbacks = close_callbacks if close_callbacks is not None else []

    @classmethod
    def from_filename(
        cls,
        filename,
        start_byte,
        chunk_size,
        callbacks=None,
        enable_callbacks=True,
    ):
        """One part of the file `filename`, opened at `start_byte`."""
        file_size = os.path.getsize(filename)
        reading = open(filename, "rb")
        reading.seek(start_byte)
        return cls(reading, chunk_size, file_size, callbacks, enable_callbacks)

    @staticmethod
    def _calculate_file_size(fileobj, requested_size, start_byte, actual_file_size):
        max_chunk_size = actual_file_size - start_byte
        return min(max_chunk_size, requested_size)

    def read(self, amount=None):
        amount_left = max(self._size - self._amount_read, 0)
        if amount is None:
            amount_to_read = amount_left
        else:
            amount_to_read = min(amount_left, amount)
        data = self._fileobj.read(amount_to_read)
        self._amount_read += len(data)
        if self._callbacks is not None and self._callbacks_enabled:
            invoke_progress_callbacks(self._callbacks, len(data))
        return data

    def signal_transferring(self):
        """Count what is read of this part as transferred."""
        self.enable_callback()
        if hasattr(self._fileobj, "signal_transferring"):
            self._fileobj.signal_transferring()

    def signal_not_transferring(self):
        """Stop counting what is read of this part as transferred."""
        self.disable_callback()
        if hasattr(self._fileobj, "signal_not_transferring"):
            self._fileobj.signal_not_transferring()

    def enable_callback(self):
        self._callbacks_enabled = True

    def disable_callback(self):
        self._callbacks_enabled = False

    def seek(self, where, whence=0):
        if whence == 0:
            target = self._start_byte + where
            read_back = self._amount_read - where
        elif whence == 1:
            target = self._start_byte + self._amount_read + where
            read_back = -where
        else:
            target = self._start_byte + self._size + where
            read_back = self._amount_read - (self._size + where)
        self._fileobj.seek(target)
        if self._callbacks is not None and self._callbacks_enabled:
            invoke_progress_callbacks(self._callbacks, bytes_transferred=-read_back)
        self._amount_read = max(target - self._start_byte, 0)

    def close(self):
        if self._close_callbacks is not None:
            for callback in self._close_callbacks:
                callback()
        self._fileobj.close()

    def tell(self):
        return self._amount_read

    def __len__(self):
        return self._size

    def __enter__(self):
        return self

    def __exit__(self, *args, **kwargs):
        self.close()

    def __iter__(self):
        return iter([])


class ChunksizeAdjuster:
    """The part size an object of a given size is really sent at.

    A part size the caller asked for is raised where the object would otherwise need more
    parts than a multipart upload admits, and clamped to the sizes one part may be.
    """

    def __init__(
        self,
        max_size=5 * 1024 * 1024 * 1024,
        min_size=5 * 1024 * 1024,
        max_parts=MAX_PARTS,
    ):
        self.max_size = max_size
        self.min_size = min_size
        self.max_parts = max_parts

    def adjust_chunksize(self, current_chunksize, file_size=None):
        chunksize = current_chunksize
        if file_size is not None:
            chunksize = self._adjust_for_max_parts(chunksize, file_size)
        return self._adjust_for_chunksize_limits(chunksize)

    def _adjust_for_chunksize_limits(self, current_chunksize):
        if current_chunksize > self.max_size:
            return self.max_size
        if current_chunksize < self.min_size:
            return self.min_size
        return current_chunksize

    def _adjust_for_max_parts(self, current_chunksize, file_size):
        chunksize = current_chunksize
        num_parts = int(file_size // chunksize) + (1 if file_size % chunksize else 0)
        while num_parts > self.max_parts:
            chunksize *= 2
            num_parts = int(file_size // chunksize) + (1 if file_size % chunksize else 0)
        return chunksize


class TaskSemaphore:
    """How many things may be in flight at once."""

    def __init__(self, count):
        self._semaphore = threading.Semaphore(count)

    def acquire(self, tag, blocking=True):
        logged = self._semaphore.acquire(blocking)
        if not logged:
            raise NoResourcesAvailable("Cannot acquire tag '%s'" % tag)
        return logged

    def release(self, tag, acquire_token=None):
        self._semaphore.release()


class SlidingWindowSemaphore(TaskSemaphore):
    """The same, released in the ORDER the things were acquired in."""

    def __init__(self, count):
        TaskSemaphore.__init__(self, count)
        self._lock = threading.Lock()
        self._tag_sequences = {}
        self._pending_release = {}
        self._lowest_sequence = {}

    def acquire(self, tag, blocking=True):
        TaskSemaphore.acquire(self, tag, blocking)
        with self._lock:
            sequence = self._tag_sequences.get(tag, 0)
            self._tag_sequences[tag] = sequence + 1
            self._lowest_sequence.setdefault(tag, 0)
            return sequence

    def release(self, tag, acquire_token=None):
        if acquire_token is None:
            TaskSemaphore.release(self, tag)
            return
        with self._lock:
            pending = self._pending_release.setdefault(tag, set())
            pending.add(acquire_token)
            while self._lowest_sequence.get(tag, 0) in pending:
                pending.discard(self._lowest_sequence[tag])
                self._lowest_sequence[tag] += 1
                TaskSemaphore.release(self, tag)


class NoResourcesAvailable(Exception):
    """Nothing is free to run one more thing right now."""
"#;

/// `s3transfer.futures`: what a submitted transfer is held and answered as.
const FUTURES_SOURCE: &str = r#"
"""What a submitted transfer is held and answered as."""

import logging

from concurrent.futures import CancelledError, TimeoutError

from s3transfer.exceptions import TransferNotDoneError

logger = logging.getLogger(__name__)


class BaseTransferFuture:
    """What every transfer answers with."""

    @property
    def meta(self):
        raise NotImplementedError("meta")

    def done(self):
        raise NotImplementedError("done()")

    def result(self):
        raise NotImplementedError("result()")

    def cancel(self):
        raise NotImplementedError("cancel()")


class BaseTransferMeta:
    """What a transfer was asked for."""

    @property
    def call_args(self):
        raise NotImplementedError("call_args")

    @property
    def transfer_id(self):
        raise NotImplementedError("transfer_id")

    @property
    def user_context(self):
        raise NotImplementedError("user_context")


class TransferMeta(BaseTransferMeta):
    """The arguments one transfer was submitted with, its id, and the size it turned out to
    be."""

    def __init__(self, call_args=None, transfer_id=None):
        self._call_args = call_args
        self._transfer_id = transfer_id
        self._size = None
        self._user_context = {}

    @property
    def call_args(self):
        return self._call_args

    @property
    def transfer_id(self):
        return self._transfer_id

    @property
    def size(self):
        return self._size

    @property
    def user_context(self):
        return self._user_context

    def provide_transfer_size(self, size):
        """State the size the transfer turned out to be, which is what the caller reads back
        off the future."""
        self._size = size


class TransferCoordinator:
    """The OUTCOME of one transfer: what it came to, and what is waiting on it.

    A transfer of this runtime runs where it was submitted, so a coordinator holds an outcome
    that is already decided: `announce_done` is what the manager calls once the calls behind
    the transfer have been made, and `result` hands back what they came to.
    """

    def __init__(self, transfer_id=None):
        self.transfer_id = transfer_id
        self._status = "not-started"
        self._result = None
        self._exception = None
        self._done_callbacks = []
        self._failure_cleanups = []

    def __repr__(self):
        return "%s(transfer_id=%s)" % (self.__class__.__name__, self.transfer_id)

    @property
    def exception(self):
        return self._exception

    @property
    def status(self):
        """Where the transfer got to: `not-started`, `queued`, `running`, `cancelled`,
        `failed` or `success`."""
        return self._status

    def set_result(self, result):
        """What the transfer came to, which `result()` hands back."""
        self._result = result
        self._status = "success"

    def set_exception(self, exception, override=False):
        """The failure the transfer came to, which `result()` raises."""
        if not self.done() or override:
            self._exception = exception
            self._status = "failed"

    def result(self):
        """What the transfer came to, raising the failure it came to instead."""
        if self._exception is not None:
            raise self._exception
        return self._result

    def cancel(self, msg="", exc_type=CancelledError):
        """Stop the transfer, where anything of it is still to be done."""
        if self.done():
            return False
        self._exception = exc_type(msg)
        self._status = "cancelled"
        return True

    def set_status_to_queued(self):
        self._status = "queued"

    def set_status_to_running(self):
        self._status = "running"

    def submit(self, executor, task, tag=None):
        """Run `task` on `executor`, which is where the work of a transfer is done."""
        return executor.submit(task)

    def done(self):
        return self._status in ("cancelled", "failed", "success")

    def add_failure_cleanup(self, function, *args, **kwargs):
        """A call made only if the transfer fails — an upload's own abort, say."""
        self._failure_cleanups.append((function, args, kwargs))

    def add_done_callback(self, function, *args, **kwargs):
        """A call made once the transfer is done, however it ended."""
        self._done_callbacks.append((function, args, kwargs))

    def announce_done(self):
        """Run what was waiting on the transfer being done, and the cleanups of a failure."""
        if self.status != "success":
            self._run_cleanups()
        self._run_done_callbacks()

    def _run_done_callbacks(self):
        callbacks = self._done_callbacks
        self._done_callbacks = []
        self._run_calls(callbacks)

    def _run_cleanups(self):
        cleanups = self._failure_cleanups
        self._failure_cleanups = []
        self._run_calls(cleanups)

    @staticmethod
    def _run_calls(calls):
        for function, args, kwargs in calls:
            try:
                function(*args, **kwargs)
            except Exception:
                logger.debug("Exception raised in %s." % function, exc_info=True)


class TransferFuture(BaseTransferFuture):
    """One submitted transfer, as the caller holds it."""

    def __init__(self, meta=None, coordinator=None):
        self._meta = meta if meta is not None else TransferMeta()
        self._coordinator = coordinator if coordinator is not None else TransferCoordinator()

    @property
    def meta(self):
        return self._meta

    def done(self):
        return self._coordinator.done()

    def result(self):
        return self._coordinator.result()

    def cancel(self):
        self._coordinator.cancel()

    def set_exception(self, exception):
        """Put `exception` on a transfer that is not done, which is what a caller abandoning
        one does."""
        if not self.done():
            raise TransferNotDoneError(
                "set_exception can only be called once the transfer is complete."
            )
        self._coordinator.set_exception(exception, override=True)


class BaseExecutor:
    """Where the work of a transfer is run."""

    def __init__(self, max_workers=None):
        self._max_workers = max_workers

    def submit(self, fn, *args, **kwargs):
        raise NotImplementedError("submit()")

    def shutdown(self, wait=True):
        raise NotImplementedError("shutdown()")


class NonThreadedExecutorFuture:
    """The answer of work that was done where it was submitted."""

    def __init__(self):
        self._result = None
        self._exception = None
        self._done = False
        self._done_callbacks = []

    def set_result(self, result):
        self._result = result
        self._done = True
        self._run_done_callbacks()

    def set_exception_info(self, exception, traceback):
        self._exception = exception
        self._done = True
        self._run_done_callbacks()

    def result(self, timeout=None):
        if self._exception is not None:
            raise self._exception
        return self._result

    def _run_done_callbacks(self):
        for callback in self._done_callbacks:
            self._run_callback(callback)

    def _run_callback(self, callback):
        try:
            callback(self)
        except Exception:
            logger.debug("Exception raised in %s" % callback, exc_info=True)

    def done(self):
        return self._done

    def add_done_callback(self, fn):
        if self._done:
            self._run_callback(fn)
        else:
            self._done_callbacks.append(fn)


class NonThreadedExecutor(BaseExecutor):
    """An executor that runs the work where it was submitted.

    It is what this runtime's own transfers run on: one function runs on one thread, so work
    handed to a pool would be work nothing ran.
    """

    def submit(self, fn, *args, **kwargs):
        future = NonThreadedExecutorFuture()
        try:
            future.set_result(fn(*args, **kwargs))
        except Exception as error:
            future.set_exception_info(error, None)
        return future

    def shutdown(self, wait=True):
        pass
"#;

/// `s3transfer.manager`: the transfers themselves, and the configuration they run under.
const MANAGER_SOURCE: &str = r#"
"""The managed transfers: what each one calls, and the configuration it runs under."""

import copy
import re

from s3transfer.constants import ALLOWED_DOWNLOAD_ARGS as _ALLOWED_DOWNLOAD_ARGS
from s3transfer.constants import FULL_OBJECT_CHECKSUM_ARGS, KB, MB
from s3transfer.futures import (
    NonThreadedExecutor,
    TransferCoordinator,
    TransferFuture,
    TransferMeta,
)
from s3transfer.utils import (
    CallArgs,
    OSUtils,
    calculate_num_parts,
    calculate_range_parameter,
    get_callbacks,
    invoke_progress_callbacks,
)


class TransferConfig:
    """What a managed transfer is run under.

    The sizes decide the transfer: an object larger than `multipart_threshold` is sent as
    parts of `multipart_chunksize` each, and a download is written in `io_chunksize` pieces.
    The CONCURRENCY members are read and validated the way the published library validates
    them, and bound nothing here: this runtime runs one function on one thread, so a transfer
    runs where it was submitted.
    """

    def __init__(
        self,
        multipart_threshold=8 * MB,
        multipart_chunksize=8 * MB,
        max_request_concurrency=10,
        max_submission_concurrency=5,
        max_request_queue_size=1000,
        max_submission_queue_size=1000,
        max_io_queue_size=1000,
        io_chunksize=256 * KB,
        num_download_attempts=5,
        max_in_memory_upload_chunks=10,
        max_in_memory_download_chunks=10,
        max_bandwidth=None,
    ):
        self.multipart_threshold = multipart_threshold
        self.multipart_chunksize = multipart_chunksize
        self.max_request_concurrency = max_request_concurrency
        self.max_submission_concurrency = max_submission_concurrency
        self.max_request_queue_size = max_request_queue_size
        self.max_submission_queue_size = max_submission_queue_size
        self.max_io_queue_size = max_io_queue_size
        self.io_chunksize = io_chunksize
        self.num_download_attempts = num_download_attempts
        self.max_in_memory_upload_chunks = max_in_memory_upload_chunks
        self.max_in_memory_download_chunks = max_in_memory_download_chunks
        self.max_bandwidth = max_bandwidth
        self._validate_attrs_are_nonzero()

    def _validate_attrs_are_nonzero(self):
        for attr, attr_val in self.__dict__.items():
            if attr_val is not None and attr_val <= 0:
                raise ValueError(
                    "Provided parameter %s of value %s must be greater than 0."
                    % (attr, attr_val)
                )


class TransferManager:
    """The managed transfers of ONE client.

    Each of them is the calls the published library makes: an upload is `PutObject`, or
    `CreateMultipartUpload`/`UploadPart`/`CompleteMultipartUpload` for an object over the
    threshold; a download is `GetObject`, streamed to the file; a copy is `CopyObject`, or
    `UploadPartCopy` per part; a delete is `DeleteObject`. The client is the caller's own, so
    every call is made as that client's identity and answered by the service itself.
    """

    ALLOWED_DOWNLOAD_ARGS = _ALLOWED_DOWNLOAD_ARGS

    ALLOWED_UPLOAD_ARGS = [
        "ACL",
        "CacheControl",
        "ChecksumAlgorithm",
        "ContentDisposition",
        "ContentEncoding",
        "ContentLanguage",
        "ContentType",
        "ExpectedBucketOwner",
        "Expires",
        "GrantFullControl",
        "GrantRead",
        "GrantReadACP",
        "GrantWriteACP",
        "Metadata",
        "ObjectLockLegalHoldStatus",
        "ObjectLockMode",
        "ObjectLockRetainUntilDate",
        "RequestPayer",
        "ServerSideEncryption",
        "StorageClass",
        "SSECustomerAlgorithm",
        "SSECustomerKey",
        "SSECustomerKeyMD5",
        "SSEKMSKeyId",
        "SSEKMSEncryptionContext",
        "Tagging",
        "WebsiteRedirectLocation",
    ] + FULL_OBJECT_CHECKSUM_ARGS

    ALLOWED_COPY_ARGS = ALLOWED_UPLOAD_ARGS + [
        "CopySourceIfMatch",
        "CopySourceIfModifiedSince",
        "CopySourceIfNoneMatch",
        "CopySourceIfUnmodifiedSince",
        "CopySourceSSECustomerAlgorithm",
        "CopySourceSSECustomerKey",
        "CopySourceSSECustomerKeyMD5",
        "MetadataDirective",
        "TaggingDirective",
    ]

    ALLOWED_DELETE_ARGS = [
        "MFA",
        "VersionId",
        "RequestPayer",
        "ExpectedBucketOwner",
    ]

    VALIDATE_SUPPORTED_BUCKET_VALUES = True

    # The ARN forms of a bucket member that name something a managed transfer cannot be run
    # against, because the object it addresses is not the object a part of a transfer would
    # read or write.
    _UNSUPPORTED_BUCKET_PATTERNS = {
        "S3 Object Lambda": re.compile(
            r"^arn:(aws).*:s3-object-lambda"
        ),
    }

    def __init__(self, client, config=None, osutil=None, executor_cls=None):
        self._client = client
        self._config = config if config is not None else TransferConfig()
        self._osutil = osutil if osutil is not None else OSUtils()
        self._executor_cls = executor_cls if executor_cls is not None else NonThreadedExecutor
        self._executor = self._executor_cls()
        self._id_counter = 0

    @property
    def client(self):
        return self._client

    @property
    def config(self):
        return self._config

    def upload(self, fileobj, bucket, key, extra_args=None, subscribers=None):
        """Send `fileobj` — a filename or a readable stream — to `bucket`/`key`."""
        if extra_args is None:
            extra_args = {}
        self._validate_all_known_args(extra_args, self.ALLOWED_UPLOAD_ARGS)
        self._validate_if_bucket_supported(bucket)
        call_args = CallArgs(
            fileobj=fileobj,
            bucket=bucket,
            key=key,
            extra_args=extra_args,
            subscribers=subscribers,
        )
        return self._transfer(call_args, self._upload)

    def download(self, bucket, key, fileobj, extra_args=None, subscribers=None):
        """Read `bucket`/`key` into `fileobj` — a filename or a writable stream."""
        if extra_args is None:
            extra_args = {}
        self._validate_all_known_args(extra_args, self.ALLOWED_DOWNLOAD_ARGS)
        self._validate_if_bucket_supported(bucket)
        call_args = CallArgs(
            bucket=bucket,
            key=key,
            fileobj=fileobj,
            extra_args=extra_args,
            subscribers=subscribers,
        )
        return self._transfer(call_args, self._download)

    def copy(
        self,
        copy_source,
        bucket,
        key,
        extra_args=None,
        subscribers=None,
        source_client=None,
    ):
        """Copy the object `copy_source` names to `bucket`/`key`."""
        if extra_args is None:
            extra_args = {}
        if source_client is None:
            source_client = self._client
        self._validate_all_known_args(extra_args, self.ALLOWED_COPY_ARGS)
        if isinstance(copy_source, dict):
            self._validate_if_bucket_supported(copy_source.get("Bucket"))
        self._validate_if_bucket_supported(bucket)
        call_args = CallArgs(
            copy_source=copy_source,
            bucket=bucket,
            key=key,
            extra_args=extra_args,
            subscribers=subscribers,
            source_client=source_client,
        )
        return self._transfer(call_args, self._copy)

    def delete(self, bucket, key, extra_args=None, subscribers=None):
        """Delete `bucket`/`key`."""
        if extra_args is None:
            extra_args = {}
        self._validate_all_known_args(extra_args, self.ALLOWED_DELETE_ARGS)
        self._validate_if_bucket_supported(bucket)
        call_args = CallArgs(
            bucket=bucket,
            key=key,
            extra_args=extra_args,
            subscribers=subscribers,
        )
        return self._transfer(call_args, self._delete)

    def _transfer(self, call_args, perform):
        """Submit one transfer and run it, answering the future it is read back off.

        A transfer runs where it is submitted, so the subscribers see the order they see on a
        pool: queued, then the progress of the calls, then done — and a failure is held on the
        future and raised by `result()` rather than here.
        """
        self._id_counter += 1
        transfer_id = self._id_counter
        coordinator = TransferCoordinator(transfer_id=transfer_id)
        future = TransferFuture(
            meta=TransferMeta(call_args, transfer_id=transfer_id),
            coordinator=coordinator,
        )
        coordinator.set_status_to_queued()
        for callback in get_callbacks(future, "queued"):
            callback()
        coordinator.set_status_to_running()
        try:
            coordinator.set_result(perform(future))
        except BaseException as error:
            coordinator.set_exception(error)
        coordinator.announce_done()
        for callback in get_callbacks(future, "done"):
            callback()
        return future

    def _upload(self, future):
        call_args = future.meta.call_args
        progress = get_callbacks(future, "progress")
        extra_args = copy.deepcopy(call_args.extra_args)
        reading, size, close_when_done = self._readable(call_args.fileobj)
        try:
            future.meta.provide_transfer_size(size)
            threshold = self._config.multipart_threshold
            if size is not None and size > threshold:
                return self._upload_parts(
                    reading, size, call_args, extra_args, progress
                )
            body = reading.read()
            answered = self._client.put_object(
                Bucket=call_args.bucket, Key=call_args.key, Body=body, **extra_args
            )
            invoke_progress_callbacks(progress, len(body))
            return answered
        finally:
            if close_when_done:
                reading.close()

    def _upload_parts(self, reading, size, call_args, extra_args, progress):
        """Send one object as several parts, which is what an object over the threshold is
        sent as."""
        chunksize = self._config.multipart_chunksize
        create_args = dict(extra_args)
        # A checksum of the WHOLE object describes bytes no single part carries, so it is not
        # a member of the parts' own calls.
        for member in FULL_OBJECT_CHECKSUM_ARGS:
            create_args.pop(member, None)
        created = self._client.create_multipart_upload(
            Bucket=call_args.bucket, Key=call_args.key, **create_args
        )
        upload_id = created["UploadId"]
        parts = []
        try:
            for part_number in range(1, calculate_num_parts(size, chunksize) + 1):
                body = reading.read(chunksize)
                if not body:
                    break
                answered = self._client.upload_part(
                    Bucket=call_args.bucket,
                    Key=call_args.key,
                    UploadId=upload_id,
                    PartNumber=part_number,
                    Body=body,
                )
                parts.append({"ETag": answered.get("ETag"), "PartNumber": part_number})
                invoke_progress_callbacks(progress, len(body))
            return self._client.complete_multipart_upload(
                Bucket=call_args.bucket,
                Key=call_args.key,
                UploadId=upload_id,
                MultipartUpload={"Parts": parts},
            )
        except BaseException:
            self._client.abort_multipart_upload(
                Bucket=call_args.bucket, Key=call_args.key, UploadId=upload_id
            )
            raise

    def _download(self, future):
        call_args = future.meta.call_args
        progress = get_callbacks(future, "progress")
        extra_args = copy.deepcopy(call_args.extra_args)
        answered = self._client.get_object(
            Bucket=call_args.bucket, Key=call_args.key, **extra_args
        )
        future.meta.provide_transfer_size(answered.get("ContentLength"))
        body = answered.get("Body")
        writing, close_when_done = self._writable(call_args.fileobj)
        try:
            for piece in self._pieces(body):
                writing.write(piece)
                invoke_progress_callbacks(progress, len(piece))
        finally:
            if close_when_done:
                writing.close()
        return None

    def _pieces(self, body):
        """The answered body in the pieces a download writes it in, each as large as the
        configured io size.

        A body the client handed over as a STREAM is read a piece at a time, so an object
        larger than one piece is never held whole; one handed over as bytes is already read.
        """
        if body is None:
            return
        if not hasattr(body, "read"):
            if body:
                yield bytes(body)
            return
        chunk = self._config.io_chunksize
        while True:
            piece = body.read(chunk)
            if not piece:
                return
            yield piece

    def _copy(self, future):
        call_args = future.meta.call_args
        progress = get_callbacks(future, "progress")
        extra_args = copy.deepcopy(call_args.extra_args)
        source = call_args.copy_source
        size = self._copy_source_size(call_args, source)
        future.meta.provide_transfer_size(size)
        if size is not None and size > self._config.multipart_threshold:
            return self._copy_parts(call_args, source, size, extra_args, progress)
        answered = self._client.copy_object(
            CopySource=source, Bucket=call_args.bucket, Key=call_args.key, **extra_args
        )
        invoke_progress_callbacks(progress, size or 0)
        return answered

    def _copy_source_size(self, call_args, source):
        """How large the object a copy reads is, as its own service reports it, or `None`
        where the source is not one this can be asked about."""
        if not isinstance(source, dict):
            return None
        bucket = source.get("Bucket")
        key = source.get("Key")
        if not bucket or not key:
            return None
        head_args = {}
        if source.get("VersionId"):
            head_args["VersionId"] = source["VersionId"]
        headed = call_args.source_client.head_object(
            Bucket=bucket, Key=key, **head_args
        )
        return headed.get("ContentLength")

    def _copy_parts(self, call_args, source, size, extra_args, progress):
        """Copy one object as several parts, which is what an object over the threshold is
        copied as."""
        chunksize = self._config.multipart_chunksize
        create_args = dict(extra_args)
        for member in self.ALLOWED_COPY_ARGS:
            if member.startswith("CopySource"):
                create_args.pop(member, None)
        for member in ("MetadataDirective", "TaggingDirective"):
            create_args.pop(member, None)
        created = self._client.create_multipart_upload(
            Bucket=call_args.bucket, Key=call_args.key, **create_args
        )
        upload_id = created["UploadId"]
        num_parts = calculate_num_parts(size, chunksize)
        parts = []
        try:
            for part_index in range(num_parts):
                ranged = calculate_range_parameter(
                    chunksize, part_index, num_parts, size
                )
                answered = self._client.upload_part_copy(
                    Bucket=call_args.bucket,
                    Key=call_args.key,
                    UploadId=upload_id,
                    PartNumber=part_index + 1,
                    CopySource=source,
                    CopySourceRange=ranged,
                )
                copied = answered.get("CopyPartResult") or {}
                parts.append(
                    {"ETag": copied.get("ETag"), "PartNumber": part_index + 1}
                )
                invoke_progress_callbacks(progress, min(chunksize, size - part_index * chunksize))
            return self._client.complete_multipart_upload(
                Bucket=call_args.bucket,
                Key=call_args.key,
                UploadId=upload_id,
                MultipartUpload={"Parts": parts},
            )
        except BaseException:
            self._client.abort_multipart_upload(
                Bucket=call_args.bucket, Key=call_args.key, UploadId=upload_id
            )
            raise

    def _delete(self, future):
        call_args = future.meta.call_args
        extra_args = copy.deepcopy(call_args.extra_args)
        return self._client.delete_object(
            Bucket=call_args.bucket, Key=call_args.key, **extra_args
        )

    def _readable(self, fileobj):
        """What an upload reads, how large it is, and whether this opened it: a filename is
        opened here, a stream is read as it is."""
        if isinstance(fileobj, (str, bytes)):
            name = fileobj.decode("utf-8") if isinstance(fileobj, bytes) else fileobj
            return self._osutil.open(name, "rb"), self._osutil.get_file_size(name), True
        if isinstance(fileobj, (bytearray, memoryview)):
            import io

            held = bytes(fileobj)
            return io.BytesIO(held), len(held), True
        size = None
        if hasattr(fileobj, "seek") and hasattr(fileobj, "tell"):
            try:
                start = fileobj.tell()
                fileobj.seek(0, 2)
                size = fileobj.tell() - start
                fileobj.seek(start)
            except Exception:
                size = None
        return fileobj, size, False

    def _writable(self, fileobj):
        """What a download writes into, and whether this opened it."""
        if isinstance(fileobj, (str, bytes)):
            name = fileobj.decode("utf-8") if isinstance(fileobj, bytes) else fileobj
            return self._osutil.open(name, "wb"), True
        return fileobj, False

    def _validate_all_known_args(self, actual, allowed):
        for kwarg in actual:
            if kwarg not in allowed:
                raise ValueError(
                    "Invalid extra_args key '%s', must be one of: %s"
                    % (kwarg, ", ".join(allowed))
                )

    def _validate_if_bucket_supported(self, bucket):
        if not self.VALIDATE_SUPPORTED_BUCKET_VALUES or not bucket:
            return
        for resource, pattern in self._UNSUPPORTED_BUCKET_PATTERNS.items():
            match = pattern.match(str(bucket))
            if match:
                raise ValueError(
                    "TransferManager does not support %s resource %s"
                    % (resource, bucket)
                )

    def shutdown(self, cancel=False, cancel_msg=""):
        """Stop the manager, which has nothing in flight to wait for."""
        self._executor.shutdown()

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc_value, *args):
        cancel = exc_type is not None
        self.shutdown(cancel, str(exc_value) if cancel else "")
"#;
