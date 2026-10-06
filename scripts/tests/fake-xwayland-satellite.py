#!/usr/bin/env python3
"""Fake Satellite for child-launch and notification tests; it does not serve X11."""

import ctypes
import errno
import json
import os
import signal
import socket
import sys
import time

SO_ACCEPTCONN = 30
_LIBC = ctypes.CDLL(None, use_errno=True)
_LIBC.getsockopt.argtypes = [
    ctypes.c_int, ctypes.c_int, ctypes.c_int,
    ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint),
]

RECORD = os.environ.get('FAKE_SATELLITE_RECORD', '')
READY = os.environ.get('FAKE_SATELLITE_READY', '1')
READY_DELAY = float(os.environ.get('FAKE_SATELLITE_READY_DELAY', '0'))
EXIT_BEFORE_READY = os.environ.get('FAKE_SATELLITE_EXIT_BEFORE_READY', '0') == '1'
EXIT_AFTER_READY = os.environ.get('FAKE_SATELLITE_EXIT_AFTER_READY', '0') == '1'
FORK_NOTIFY = os.environ.get('FAKE_SATELLITE_FORK_NOTIFY', '0') == '1'
IGNORE_SIGTERM = os.environ.get('FAKE_SATELLITE_IGNORE_SIGTERM', '0') == '1'
HOLD_FDS = os.environ.get('FAKE_SATELLITE_HOLD_FDS', '0') == '1'
LINGER = float(os.environ.get('FAKE_SATELLITE_LINGER', '30'))


def parse_args(argv):
    display = None
    authority = None
    listenfds = []
    index = 0
    while index < len(argv):
        argument = argv[index]
        if display is None and not argument.startswith('-'):
            display = argument
            index += 1
            continue
        if argument == '-auth':
            index += 1
            authority = argv[index]
        elif argument == '-nolisten':
            index += 1
            if argv[index] != 'tcp':
                sys.exit('-nolisten must be followed by tcp')
        elif argument == '-listenfd':
            index += 1
            listenfds.append(int(argv[index]))
        else:
            sys.exit(f'unexpected argument: {argument}')
        index += 1

    if display is None or authority is None:
        sys.exit('missing display or -auth')
    if not listenfds:
        sys.exit('expected at least one -listenfd')
    if len(set(listenfds)) != len(listenfds):
        sys.exit('duplicate -listenfd')
    return display, authority, listenfds


# The scan's own descriptor and entries that vanish mid-scan are not held descriptors.
def inherited_fds():
    inherited = []
    with os.scandir('/proc/self/fd') as entries:
        for entry in entries:
            try:
                fd = int(entry.name)
            except ValueError:
                continue
            try:
                target = os.readlink(entry.path)
            except OSError:
                continue
            if target == '/proc/self/fd':
                continue
            inherited.append(fd)
    return sorted(inherited)


# Compare the link target: an fd number alone can be reused by an unrelated descriptor.
def sentinel_is_leaked(inherited):
    sentinel = os.environ.get('FAKE_SATELLITE_SENTINEL_FD')
    if not sentinel:
        return False
    try:
        fd = int(sentinel)
    except ValueError:
        return False
    target = os.environ.get('FAKE_SATELLITE_SENTINEL_TARGET')
    if target:
        try:
            return os.readlink(f'/proc/self/fd/{fd}') == target
        except OSError:
            return False
    return fd in inherited


def describe_fds(listenfds):
    inherited = inherited_fds()
    sentinel = os.environ.get('FAKE_SATELLITE_SENTINEL_FD')
    return {
        'inherited_fds': inherited,
        'listenfds': listenfds,
        'sentinel_fd': int(sentinel) if sentinel else None,
        'sentinel_leaked': sentinel_is_leaked(inherited),
        'listening': {
            str(fd): is_listening(fd) for fd in listenfds
        },
        'all_listening': all(is_listening(fd) for fd in listenfds),
    }


def is_listening(fd):
    value = ctypes.c_int(0)
    length = ctypes.c_uint(ctypes.sizeof(value))
    if _LIBC.getsockopt(
        fd,
        socket.SOL_SOCKET,
        SO_ACCEPTCONN,
        ctypes.byref(value),
        ctypes.byref(length),
    ) != 0:
        return False
    return value.value == 1


def notify(payload):
    notify_socket = os.environ.get('NOTIFY_SOCKET')
    if not notify_socket:
        return False
    address = notify_socket[1:] if notify_socket.startswith('@') else notify_socket
    sender = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
    try:
        sender.connect(address)
        sender.sendall(payload)
        return True
    except OSError as error:
        print(f'fake-satellite: notify failed: {error}', file=sys.stderr)
        return False
    finally:
        sender.close()


# The descendant never exits on its own: a live holder after the leader is gone means group cleanup failed.
def hold_descriptors(listenfds):
    child = os.fork()
    if child == 0:
        for fd in listenfds:
            try:
                os.dup2(fd, fd)
            except OSError:
                pass
        while True:
            time.sleep(3600)
    return child


def main():
    display, authority, listenfds = parse_args(sys.argv[1:])
    if IGNORE_SIGTERM:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)

    holder_pid = None
    if HOLD_FDS:
        holder_pid = hold_descriptors(listenfds)

    if RECORD:
        record = {
            'pid': os.getpid(),
            'display': display,
            'authority': authority,
            'argv': sys.argv[1:],
            'env': {
                key: os.environ.get(key)
                for key in ('DISPLAY', 'XAUTHORITY', 'WAYLAND_DISPLAY',
                            'WAYLAND_SOCKET', 'FERESE_PUBLIC_WAYLAND_DISPLAY',
                            'FERESE_SHELL_CONTROL_SOCKET')
            },
        }
        record.update(describe_fds(listenfds))
        record['holder_pid'] = holder_pid
        with open(RECORD, 'a', encoding='utf-8') as handle:
            handle.write(json.dumps(record) + '\n')

    if EXIT_BEFORE_READY:
        sys.exit(int(os.environ.get('FAKE_SATELLITE_EXIT_CODE', '17')))

    if READY_DELAY:
        time.sleep(READY_DELAY)

    if READY == '1':
        payload = b'READY=1'
    elif READY == 'child':
        payload = b'READY=1'
    elif READY == 'garbage':
        payload = b'READY'
    elif READY == 'truncated':
        # Oversize the buffer so the kernel really reports MSG_TRUNC; a short
        # payload would only be a malformed line.
        payload = b'READY=1' + b'X' * 4096
    else:
        payload = b''

    if payload:
        if FORK_NOTIFY or READY == 'child':
            pid = os.fork()
            if pid == 0:
                notify(payload)
                os._exit(0)
            os.waitpid(pid, 0)
        else:
            notify(payload)

    if EXIT_AFTER_READY:
        sys.exit(int(os.environ.get('FAKE_SATELLITE_EXIT_CODE', '19')))

    deadline = time.monotonic() + LINGER
    while time.monotonic() < deadline:
        time.sleep(0.05)
    sys.exit(0)


if __name__ == '__main__':
    try:
        main()
    except OSError as error:
        if error.errno != errno.EINTR:
            raise
