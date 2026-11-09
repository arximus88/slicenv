#!/usr/bin/env python3
"""Перевірка живого прикладу лише читання без виводу секретів або імен файлів конфігурації."""
import ctypes
import hashlib
import os
from pathlib import Path
import struct
import subprocess
import sys

source = Path(os.environ.get("BAMBU_STUDIO_CONFIG", Path.home() / ".config/BambuStudio"))
libc = ctypes.CDLL(None, use_errno=True)
libc.inotify_init1.argtypes = [ctypes.c_int]
libc.inotify_add_watch.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_uint32]
fd = libc.inotify_init1(os.O_NONBLOCK | os.O_CLOEXEC)
if fd < 0:
    raise OSError(ctypes.get_errno(), "inotify_init1 failed")
# Записи, атрибути, створення, видалення й переміщення; читання не враховується.
mask = 0x2 | 0x4 | 0x8 | 0x40 | 0x80 | 0x100 | 0x200 | 0x400 | 0x800
try:
    for directory in [source, *(p for p in source.rglob("*") if p.is_dir() and not p.is_symlink())]:
        if libc.inotify_add_watch(fd, os.fsencode(directory), mask) < 0:
            raise OSError(ctypes.get_errno(), "inotify_add_watch failed")

    def fingerprint():
        result = []
        for name in ["BambuNetworkEngine.conf", "BambuStudio.conf"]:
            path = source / name
            if not path.exists():
                result.append(None)
                continue
            info = path.stat()
            result.append((info.st_ino, info.st_mode, info.st_size, info.st_mtime_ns,
                           info.st_ctime_ns, hashlib.sha256(path.read_bytes()).digest()))
        return result

    before = fingerprint()
    example = sys.argv[1] if len(sys.argv) > 1 else "status"
    if example not in {"status", "tasks", "camera", "verify_w5"}:
        raise ValueError("Дозволено лише status, tasks або camera")
    completed = subprocess.run(["cargo", "run", "-p", "cloud", "--example", example,
                                "--", *sys.argv[2:]], check=False)
    unchanged = before == fingerprint()
    count = 0
    overflow = False
    while True:
        try:
            data = os.read(fd, 65536)
        except BlockingIOError:
            break
        if not data:
            break
        offset = 0
        while offset < len(data):
            _, event_mask, _, length = struct.unpack_from("iIII", data, offset)
            overflow |= bool(event_mask & 0x4000)
            count += 1
            offset += 16 + length
    print(f"Original config tree mutation events: {count}; monitor overflow: {overflow}")
    print(f"Original session and Studio config content/metadata unchanged: {unchanged}")
    sys.exit(completed.returncode if completed.returncode else int(count != 0 or overflow or not unchanged))
finally:
    os.close(fd)
