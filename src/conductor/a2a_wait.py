"""Linux mailbox change notifications with bounded polling reconciliation."""

from __future__ import annotations

import ctypes
import errno
import os
import select
import struct
import sys
import time
from pathlib import Path


class MailboxWakeup:
    """Watch a mailbox directory, including WAL replacement, without a daemon."""

    def __init__(self, directory: Path) -> None:
        self.fd = -1
        if (
            not sys.platform.startswith("linux")
            or not (directory / "store.sqlite").exists()
        ):
            return
        library = ctypes.CDLL(None, use_errno=True)
        library.inotify_init1.argtypes = [ctypes.c_int]
        library.inotify_init1.restype = ctypes.c_int
        library.inotify_add_watch.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint32,
        ]
        library.inotify_add_watch.restype = ctypes.c_int
        descriptor = library.inotify_init1(os.O_NONBLOCK | os.O_CLOEXEC)
        if descriptor < 0:
            print(
                f"A2A notifications unavailable: {os.strerror(ctypes.get_errno())}; polling",
                file=sys.stderr,
            )
            return
        # MODIFY, CLOSE_WRITE, MOVED_TO, CREATE, DELETE and directory invalidation.
        watch = library.inotify_add_watch(
            descriptor, os.fsencode(directory), 0x00000FC2
        )
        if watch < 0:
            error = os.strerror(ctypes.get_errno())
            os.close(descriptor)
            print(
                f"A2A notification watch unavailable: {error}; polling", file=sys.stderr
            )
            return
        self.fd = descriptor

    def close(self) -> None:
        if self.fd >= 0:
            os.close(self.fd)
            self.fd = -1

    def wait(self, timeout: float) -> bool:
        """Return on DB change; timeout reconciles missed events and retry timers."""
        if self.fd < 0:
            time.sleep(max(0.0, timeout))
            return False
        deadline = time.monotonic() + max(0.0, timeout)
        while True:
            if not select.select(
                [self.fd], [], [], max(0.0, deadline - time.monotonic())
            )[0]:
                return False
            try:
                events = os.read(self.fd, 65536)
            except OSError as exc:
                if exc.errno == errno.EAGAIN:
                    continue
                raise
            offset = 0
            while offset + 16 <= len(events):
                _, mask, _, size = struct.unpack_from("iIII", events, offset)
                name = events[offset + 16 : offset + 16 + size].rstrip(b"\0")
                offset += 16 + size
                if mask & 0x0000C000:  # overflow or watch removed: reconcile.
                    return True
                if name.startswith(b"store.sqlite"):
                    return True
