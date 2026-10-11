"""Run native regressions against a private, headless Weston instance."""
import argparse
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log-dir", type=Path, required=True)
    args = parser.parse_args()
    logs = args.log_dir.resolve()
    logs.mkdir(parents=True, exist_ok=True)
    repo = Path(__file__).resolve().parents[2]

    # No inherited session endpoints or activation settings reach the test host.
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("FERESE_") or key.startswith("FERESE_TEST_")}
    for key in ("DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XAUTHORITY",
                "DBUS_SESSION_BUS_ADDRESS", "DBUS_SESSION_BUS_PID", "EGL_PLATFORM"):
        env.pop(key, None)
    env.update(LIBGL_ALWAYS_SOFTWARE="1", FERESE_TEST_LOG_DIR=str(logs))

    with tempfile.TemporaryDirectory(prefix="ferese-native-host-") as temporary:
        root = Path(temporary)
        runtime = root / "runtime"
        runtime.mkdir(mode=0o700)
        env.update(XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY="ferese-test-host",
                   XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                   XDG_CACHE_HOME=str(root / "cache"))
        with (logs / "weston.log").open("w") as log:
            host = subprocess.Popen([
                env.get("FERESE_TEST_WESTON", "weston"), "--backend=headless-backend.so",
                "--renderer=pixman", "--width=1600", "--height=1000",
                "--socket=ferese-test-host", "--idle-time=0", "--no-config",
            ], env=env, stdout=log, stderr=log, start_new_session=True)
            try:
                deadline = time.monotonic() + 15
                while not (runtime / env["WAYLAND_DISPLAY"]).is_socket():
                    if host.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError((logs / "weston.log").read_text())
                    time.sleep(.05)
                for layout in ("continuous", "islands"):
                    subprocess.run([
                        sys.executable, "scripts/tests/test_shell_motion_isolated.py", "-v",
                    ], cwd=repo, env=dict(env, FERESE_TEST_SHELL_MOTION="1",
                                        FERESE_TEST_BAR_LAYOUT=layout), check=True)
                subprocess.run([
                    sys.executable, "scripts/tests/test_xwayland_isolated.py", "-v",
                ], cwd=repo, env=dict(env, FERESE_TEST_XWAYLAND="1"), check=True)
            finally:
                if host.poll() is None:
                    os.killpg(host.pid, signal.SIGTERM)
                    try:
                        host.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(host.pid, signal.SIGKILL)
                        host.wait(timeout=5)


if __name__ == "__main__":
    main()
