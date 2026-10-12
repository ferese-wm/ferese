"""Exercise tree and scrolling tabs with real clients on a private nested desktop.

FERESE_TEST_TREE_TABS=1 python3 scripts/tests/test_tree_tabs_isolated.py
Requires foot, dbus-daemon, Pillow, and a Wayland host. FERESE_TEST_BINARY and
FERESE_TEST_CTL may point to build artifacts outside target/debug.
"""
import io
import json
import os
from pathlib import Path
from session_socket import ipc_environment, ipc_socket
import signal
import subprocess
import tempfile
import time
import unittest


@unittest.skipUnless(os.environ.get("FERESE_TEST_TREE_TABS") == "1", "nested tab test is opt-in")
class TabGroupsTest(unittest.TestCase):
    def test_tree_join_cycle_leave_and_close(self):
        self.exercise("tree")

    def test_scrolling_join_cycle_leave_and_close(self):
        self.exercise("scrolling")

    def exercise(self, mode):
        from PIL import Image

        repo = Path(__file__).resolve().parents[2]
        binary = repo / os.environ.get("FERESE_TEST_BINARY", "target/debug/ferese")
        ctl = repo / os.environ.get("FERESE_TEST_CTL", "target/debug/feresectl")
        children = []
        with tempfile.TemporaryDirectory(prefix="ferese-tabs-") as temporary:
            root = Path(temporary)
            runtime = root / "runtime"
            runtime.mkdir(mode=0o700)
            config = root / "config/ferese"
            config.mkdir(parents=True)
            (config / "config.kdl").write_text(f'layout {{ mode "{mode}"; }}\nanimations {{ enabled #false; }}\n'
                + 'scrolling { tab-indicator { active-color "#ff0000"; inactive-color "#00ff00"; }; }\n')
            host = Path(os.environ["WAYLAND_DISPLAY"])
            if not host.is_absolute():
                host = Path(os.environ["XDG_RUNTIME_DIR"]) / host
            env = dict(os.environ, WAYLAND_DISPLAY=str(host), XDG_RUNTIME_DIR=str(runtime),
                       XDG_CONFIG_HOME=str(root / "config"), XDG_STATE_HOME=str(root / "state"),
                       XDG_DATA_HOME=str(root / "data"))
            env.pop("WAYLAND_SOCKET", None)
            env.pop("FERESE_SHELL_CONTROL_SOCKET", None)
            log_path = root / "compositor.log"

            def launch(command, **kwargs):
                child = subprocess.Popen(command, env=ipc_environment(env), start_new_session=True, **kwargs)
                children.append(child)
                return child

            def command(*args):
                return json.loads(subprocess.check_output([str(ctl), "-j", *args],
                                  env=ipc_environment(env), timeout=10))

            def wait_for(predicate):
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    result = predicate()
                    if result:
                        return result
                    time.sleep(.025)
                self.fail("Timed out:\n" + log_path.read_text())

            def windows():
                return command("windows")

            def mapped():
                return [w for w in windows() if w["mapped"]]

            with log_path.open("w") as log:
                try:
                    bus = launch(["dbus-daemon", "--session", "--nofork", "--print-address=1"],
                                 stdout=subprocess.PIPE, stderr=log, text=True)
                    env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().strip()
                    launch([str(binary), "--backend", "nested"], stdout=log, stderr=log)
                    wait_for(lambda: ipc_socket(runtime).is_socket())
                    env["WAYLAND_DISPLAY"] = str(next(p for p in runtime.glob("wayland-*") if p.is_socket()))
                    for index, title in enumerate(("Editor", "Build", "Logs"), 1):
                        launch(["foot", "--app-id", f"ferese.test.tabs.{index}", "--title", title,
                                "sh", "-c", f"printf '{title}\\n'; sleep 120"], stdout=log, stderr=log)
                        wait_for(lambda: len(windows()) == index and len(mapped()) >= 1)
                        if index == 1 and mode == "scrolling":
                            command("toggle-column-tabbed")
                        if index > 1:
                            command("consume")
                            wait_for(lambda: len(mapped()) == 1)
                    ids = [w["id"] for w in windows()]
                    self.assertEqual(command("focused-window")["id"], ids[-1])
                    for expected in ids:
                        command("next-tab")
                        wait_for(lambda: len(mapped()) == 1 and mapped()[0]["id"] == expected)
                    command("previous-tab")
                    self.assertEqual(command("focused-window")["id"], ids[1])
                    # New windows split beside the group; they must not become hidden tabs.
                    launch(["foot", "--title", "Neighbor", "sleep", "120"], stdout=log, stderr=log)
                    wait_for(lambda: len(windows()) == 4 and len(mapped()) == 2)
                    neighbor = command("focused-window")["id"]
                    neighbor_rect = next(w for w in mapped() if w["id"] == neighbor)
                    command("focus-last-window")
                    command("next-tab")
                    wait_for(lambda: len(mapped()) == 2)
                    current_neighbor = next(w for w in mapped() if w["id"] == neighbor)
                    for key in ("x", "y", "width", "height"):
                        self.assertEqual(current_neighbor[key], neighbor_rect[key])
                    # Inspect actual text pixels; mapped windows alone do not prove the bar rendered.
                    active = next(w for w in mapped() if w["focused"])
                    scale = command("outputs")[0]["scale"]
                    captured = None

                    def labels_rendered():
                        nonlocal captured
                        captured = subprocess.check_output([str(ctl), "screenshot"],
                                                           env=ipc_environment(env), stderr=log, timeout=10)
                        image = Image.open(io.BytesIO(captured)).convert("RGB")
                        if mode == "scrolling":
                            # The overlay rail sits inside the left edge at the default 4px gap.
                            for index, window_id in enumerate(ids):
                                x = round((active["x"] + 2) * scale)
                                y = round((active["y"] + active["height"] / 2 + (index - 1) * 22) * scale)
                                r, g, b = image.getpixel((x, y))
                                if window_id == active["id"]:
                                    if r < 180 or g > 60 or b > 60:
                                        return False
                                elif g < 180 or r > 60 or b > 60:
                                    return False
                            self.assertEqual(active["y"], current_neighbor["y"])
                            self.assertEqual(active["height"], current_neighbor["height"])
                            return True
                        for index in range(3):
                            left = active["x"] + active["width"] * index / 3 + 4
                            right = active["x"] + active["width"] * (index + 1) / 3 - 4
                            box = (round(left * scale), round((active["y"] - 20) * scale),
                                   round(right * scale), round((active["y"] - 5) * scale))
                            pixels = image.crop(box).tobytes()
                            if not any(min(pixels[i:i + 3]) > 130 for i in range(0, len(pixels), 3)):
                                return False
                        return True

                    wait_for(labels_rendered)
                    if screenshot := os.environ.get("FERESE_TEST_TABS_SCREENSHOT"):
                        path = Path(screenshot)
                        path.with_stem(path.stem + "-" + mode).write_bytes(captured)
                    command("leave-tab")
                    wait_for(lambda: len(mapped()) == 3)
                    command("close")
                    wait_for(lambda: len(windows()) == 3 and len(mapped()) == 2)
                    command("toggle-layout")
                    wait_for(lambda: len(mapped()) == 3)
                finally:
                    for child in reversed(children):
                        if child.poll() is None:
                            os.killpg(child.pid, signal.SIGTERM)
                    for child in reversed(children):
                        try:
                            child.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            os.killpg(child.pid, signal.SIGKILL)
                            child.wait(timeout=5)
                    if bus.stdout:
                        bus.stdout.close()


if __name__ == "__main__":
    unittest.main()
