"""Run native regressions on a private software-rendered Weston host."""
import argparse
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time


def stop(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGINT)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log-dir", type=Path, required=True)
    parser.add_argument("--suite", choices=("panel-xwayland", "desktop"), default="panel-xwayland")
    args = parser.parse_args()
    logs = args.log_dir.resolve()
    logs.mkdir(parents=True, exist_ok=True)
    repo = Path(__file__).resolve().parents[2]
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("FERESE_") or key.startswith("FERESE_TEST_")}
    for key in ("DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XAUTHORITY",
                "DBUS_SESSION_BUS_ADDRESS", "DBUS_SESSION_BUS_PID", "EGL_PLATFORM"):
        env.pop(key, None)
    env.update(LIBGL_ALWAYS_SOFTWARE="1", FERESE_TEST_LOG_DIR=str(logs))
    for key, value in {"FERESE_TEST_BINARY": "target/debug/ferese",
                       "FERESE_TEST_CTL": "target/debug/feresectl",
                       "FERESE_TEST_SHELL": "target/debug/ferese-shell"}.items():
        env.setdefault(key, value)
    with tempfile.TemporaryDirectory(prefix="ferese-native-host-") as temporary:
        root = Path(temporary)
        runtime = root / "runtime"
        runtime.mkdir(mode=0o700)
        env.update(XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY="ferese-test-host",
                   XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                   XDG_CACHE_HOME=str(root / "cache"))
        with (logs / "weston.log").open("w") as log:
            host = subprocess.Popen([
                env.get("FERESE_TEST_WESTON", "weston"),
                "--backend=headless-backend.so", "--renderer=pixman",
                "--width=1600", "--height=1000", "--socket=ferese-test-host",
                "--idle-time=0", "--no-config",
            ], env=env, stdout=log, stderr=log, start_new_session=True)
            try:
                deadline = time.monotonic() + 15
                while not (runtime / env["WAYLAND_DISPLAY"]).is_socket():
                    if host.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError((logs / "weston.log").read_text())
                    time.sleep(.05)
                cases = [
                    ("shell-flat", "shell_motion", {"FERESE_TEST_SHELL_MOTION": "1", "FERESE_TEST_BAR_LAYOUT": "flat"}),
                    ("shell-floating", "shell_motion", {"FERESE_TEST_SHELL_MOTION": "1", "FERESE_TEST_BAR_LAYOUT": "floating"}),
                    ("shell-titles", "shell_title_updates", {"FERESE_TEST_SHELL_TITLES": "1"}),
                    ("invalid-config", "startup_config", {"FERESE_TEST_STARTUP_CONFIG": "1"}),
                    ("activities", "activities", {"FERESE_TEST_ACTIVITIES": "1"}),
                ]
                if args.suite == "desktop":
                    cases.insert(0, ("lock", "lock", {"FERESE_TEST_LOCK": "1", "FERESE_TEST_BIN_DIR": "target/debug"}))
                    cases.extend([
                        ("presentation", "presentation", {"FERESE_TEST_PRESENTATION": "1"}),
                        ("resize", "resize_dependencies", {"FERESE_TEST_RESIZE": "1"}),
                    ])
                else:
                    cases.append(("xwayland", "xwayland", {"FERESE_TEST_XWAYLAND": "1"}))
                for name, script, options in cases:
                    print(f"Running {name}", flush=True)
                    with (logs / f"{name}-test.log").open("w") as output:
                        test = subprocess.Popen([
                            sys.executable, f"scripts/tests/test_{script}_isolated.py", "-v",
                        ], cwd=repo, env=dict(env, **options), stdout=output, stderr=output,
                           start_new_session=True)
                        try:
                            status = test.wait(timeout=180)
                            if status:
                                raise RuntimeError((logs / f"{name}-test.log").read_text())
                        finally:
                            stop(test)
                    print(f"Passed {name}", flush=True)
            finally:
                stop(host)


if __name__ == "__main__":
    main()
