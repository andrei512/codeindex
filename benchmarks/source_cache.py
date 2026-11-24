"""Prepare and verify cold worktree file data outside a benchmark's timed window."""
import ctypes
import mmap
import os
import stat
import subprocess
import sys


class ColdSourceCache:
    def __init__(self, repo):
        if sys.platform not in ("darwin", "linux"):
            raise RuntimeError("cold source cache requires macOS or Linux; use --warm-source-cache")
        env = dict(os.environ, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        names = subprocess.check_output(
            ["git", "-C", repo, "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            env=env,
        )
        self.files = []
        for name in names.split(b"\0"):
            if name:
                path = repo / os.fsdecode(name)
                if stat.S_ISREG(path.lstat().st_mode):
                    self.files.append(path)

        self.lib = ctypes.CDLL(None, use_errno=True)
        ptr = ctypes.c_void_p
        size = ctypes.c_size_t
        integer = ctypes.c_int
        self.lib.mmap.argtypes = [ptr, size, integer, integer, integer, ctypes.c_longlong]
        self.lib.mmap.restype = ptr
        self.lib.munmap.argtypes = [ptr, size]
        self.lib.mincore.argtypes = [ptr, size, ptr]
        self.lib.msync.argtypes = [ptr, size, integer]

    @staticmethod
    def check(failed, path):
        if failed:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error), path)

    def visit(self, path, evict):
        with path.open("rb", buffering=0) as source:
            fd = source.fileno()
            length = os.fstat(fd).st_size
            if not length:
                return 0
            if evict and sys.platform == "linux":
                os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
                return 0
            address = self.lib.mmap(None, length, mmap.PROT_READ, mmap.MAP_SHARED, fd, 0)
            self.check(address == ctypes.c_void_p(-1).value, path)
            try:
                if evict:
                    # Darwin sys/mman.h: MS_SYNC | MS_INVALIDATE.
                    self.check(self.lib.msync(address, length, 0x10 | 0x02) != 0, path)
                    return 0
                pages = (length + mmap.PAGESIZE - 1) // mmap.PAGESIZE
                residency = (ctypes.c_ubyte * pages)()
                self.check(self.lib.mincore(address, length, residency) != 0, path)
                resident = sum(flags & 1 for flags in residency)
                if resident:
                    raise RuntimeError(f"{path}: {resident}/{pages} source pages still resident")
                return pages
            finally:
                self.check(self.lib.munmap(address, length) != 0, path)

    def prepare(self):
        for path in self.files:
            self.visit(path, evict=True)
        # Reopen and verify the entire corpus after the last eviction.
        pages = sum(self.visit(path, evict=False) for path in self.files)
        return dict(files=len(self.files), pages_checked=pages, pages_resident=0)
