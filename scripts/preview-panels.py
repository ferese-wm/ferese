#!/usr/bin/env python3
"""Run the current shell and Settings in a private nested desktop."""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import signal
import socket
import struct
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[1]


def request(endpoint, command, args=None):
    data = json.dumps({"version": 1, "id": 1, "type": "command", "command": command, "args": args or {}}).encode()
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(2)
        connection.connect(str(endpoint))
        connection.sendall(struct.pack(">I", len(data)) + data)
        def receive(count):
            result = bytearray()
            while len(result) < count:
                part = connection.recv(count - len(result))
                if not part:
                    raise ConnectionError("Preview compositor disconnected")
                result.extend(part)
            return bytes(result)
        length = struct.unpack(">I", receive(4))[0]
        if length > 4 * 1024 * 1024:
            raise ValueError("Oversized preview response")
        result = json.loads(receive(length))
        if "error" in result:
            raise RuntimeError(result["error"])
        return result["result"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-build", action="store_true", help="Use existing debug binaries")
    parser.add_argument("--style", default="floating", help="Initial panel style")
    args = parser.parse_args()
    if not args.no_build:
        subprocess.run(["cargo", "build", "-p", "ferese", "-p", "ferese-shell", "-p", "ferese-settings"], cwd=REPO, check=True)
    binaries = {name: REPO / "target/debug" / name for name in ("ferese", "ferese-shell", "ferese-settings")}
    if any(not binary.is_file() for binary in binaries.values()):
        parser.error("Build the compositor, shell and Settings first")
    display = Path(os.environ.get("WAYLAND_DISPLAY", ""))
    if not os.environ.get("WAYLAND_DISPLAY"):
        parser.error("Run this preview inside a Wayland session")
    if not display.is_absolute():
        display = Path(os.environ["XDG_RUNTIME_DIR"]) / display
    children = []
    with tempfile.TemporaryDirectory(prefix="ferese-panel-preview-") as temporary:
        root = Path(temporary)
        runtime = root / "runtime"
        runtime.mkdir(mode=0o700)
        config = root / "config/ferese"
        config.mkdir(parents=True)
        # Keep the complete composition; an authored panel with no groups is empty.
        source = (REPO / "packaging/config.kdl").read_text().replace('style "flat"', 'style ' + json.dumps(args.style), 1)
        source = re.sub(r'keybinding-guide\s+#true', 'keybinding-guide #false', source)
        config_path = config / "config.kdl"
        config_path.write_text(source)
        subprocess.run([str(binaries["ferese"]), "--check-config", str(config_path)], check=True)
        env = {key: value for key, value in os.environ.items() if not key.startswith("FERESE_")}
        env.update(XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY=str(display), XDG_CONFIG_HOME=str(root / "config"),
                   XDG_STATE_HOME=str(root / "state"), XDG_CACHE_HOME=str(root / "cache"),
                   XDG_DATA_HOME=str(root / "data"), FERESE_ENABLE_SCREENCOPY="1",
                   FERESE_COMPOSITOR_WALLPAPER="1", WINIT_UNIX_BACKEND="wayland", ICED_BACKEND="wgpu", RUST_LOG="ferese=info")
        env.pop("WAYLAND_SOCKET", None)
        bus_config = root / "bus.conf"
        bus_config.write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>'
                              '<policy context="default"><allow send_destination="*"/>'
                              '<allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
        launcher = root / "launch.sh"
        launcher.write_text('#!/bin/bash\nunset DISPLAY\nexport WAYLAND_DISPLAY="${FERESE_PUBLIC_WAYLAND_DISPLAY}"\n'
                            'env -u WAYLAND_SOCKET -u FERESE_SHELL_CONTROL_SOCKET -u DISPLAY '
                            'WINIT_UNIX_BACKEND=wayland '
                            + shlex.quote(str(binaries["ferese-settings"])) + ' --page bar &\nexec '
                            + shlex.quote(str(binaries["ferese-shell"])) + '\n')
        launcher.chmod(0o700)
        log_path = root / "session.log"
        def stop(_signal, _frame):
            raise KeyboardInterrupt
        signal.signal(signal.SIGTERM, stop)
        try:
            with log_path.open("w") as log:
                bus = subprocess.Popen(["dbus-daemon", "--config-file=" + str(bus_config), "--nofork", "--print-address=1"],
                                       env=env, stdout=subprocess.PIPE, stderr=log, text=True, start_new_session=True)
                children.append(bus)
                env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
                compositor = subprocess.Popen([str(binaries["ferese"]), "--backend", "nested", "--grant-effects",
                                               "--grant-shell-control", "--", str(launcher)],
                                              env=env, stdout=log, stderr=log, start_new_session=True)
                children.append(compositor)
                deadline = time.monotonic() + 30
                readiness = "Waiting for the compositor socket"
                while True:
                    endpoints = list(runtime.glob("ferese/instances/*/control.sock"))
                    trace = log_path.read_text()
                    child = re.search(r'spawned Wayland client.*pid=(\d+)', trace)
                    if compositor.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError("Preview did not create its shell and Settings: " + readiness + "\n" + trace[-8000:])
                    if endpoints and child:
                        try:
                            panel = request(endpoints[0], "has-client-surfaces", {"pid": int(child.group(1))})
                            # The private socketpair has the compositor's peer credentials.
                            # Wallpaper is disabled, so this client can only own shell surfaces.
                            panel = panel or request(endpoints[0], "has-client-surfaces", {"pid": compositor.pid})
                            windows = request(endpoints[0], "get-windows")
                            settings = any(window["mapped"] and window["app_id"] == "dev.ferese.Settings"
                                           for window in windows)
                            readiness = f"shell surfaces={panel}; windows={windows}"
                            if panel and settings:
                                break
                        except (OSError, ConnectionError):
                            pass
                    time.sleep(.05)
                print(f"Panel preview ready. Changes apply only inside this window.\nConfig: {config_path}\nLog: {log_path}", flush=True)
                compositor.wait()
        except KeyboardInterrupt:
            pass
        finally:
            for child in reversed(children):
                try:
                    os.killpg(child.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait(timeout=5)
                if child.stdout is not None:
                    child.stdout.close()


if __name__ == "__main__":
    main()
