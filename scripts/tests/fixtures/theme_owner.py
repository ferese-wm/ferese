"""Minimal compositor theme IPC peer for portal wire-contract tests."""
import json
import socket
import struct
import threading


def snapshot(appearance, accent, reduced_motion, contrast):
    # A complete schema-v2 snapshot, with deliberately different effective appearance
    # and surface brightness so consumers cannot infer the scheme from color tokens.
    theme = {
        "appearance": appearance, "requested_accent": accent,
        "accessibility": {"increase_contrast": contrast, "reduce_transparency": False},
        "reduced_motion": reduced_motion,
        "tokens": {
            "colors": {
                "surface_base": "#111111" if appearance == "light" else "#ffffff",
                "surface_raised": "#1E2530", "application_background": "#171E27",
                "text_primary": "#F4F7FB", "text_muted": "#8793A2", "accent": accent,
                "on_accent": "#FFFFFF", "border": "#FFFFFF18", "shadow": "#00000055",
            },
            "material": {"style": "solid", "opacity": 1.0, "blur_radius": 0.0, "tint_strength": 1.0},
            "geometry": {
                "border_width": 1.0, "focus_ring_width": 2.0, "window_radius": 14.0,
                "shell_radius": 14.0, "control_gap": 12.0,
            },
            "typography": {"font_family": "Inter"},
            "background": {"path": "", "lock_path": None, "mode": "fill"},
            "surface": {"bar": {"background": "#111111", "text_primary": "#F4F7FB", "text_muted": "#8793A2"}},
            "shadow": {"soft": {"offset_y": 4.0, "blur": 18.0, "opacity": 0.2}},
            "border": {"gradient": None}, "focus_ring": {"gradient": None},
        },
    }
    return {"version": 2, "revision": 1, "mode": appearance, "theme": theme,
            "presented": theme, "warnings": [], "error": None, "families": [], "fallback_note": None}


class ThemeOwner:
    def __init__(self, runtime):
        path = runtime / "ferese/control.sock"
        path.parent.mkdir()
        self.condition = threading.Condition()
        self.current = snapshot("light", "#ff8000", True, False)
        self.closed = False
        self.errors = []
        self.clients = []
        self.workers = []
        self.listener = socket.socket(socket.AF_UNIX)
        self.listener.bind(str(path))
        self.listener.listen()
        self.listener.settimeout(0.1)
        self.thread = threading.Thread(target=self.accept, daemon=True)
        self.thread.start()

    def publish(self, appearance, accent, reduced_motion, contrast):
        with self.condition:
            next_snapshot = snapshot(appearance, accent, reduced_motion, contrast)
            next_snapshot["revision"] = self.current["revision"] + 1
            self.current = next_snapshot
            self.condition.notify_all()

    def accept(self):
        while not self.closed:
            try:
                client, _ = self.listener.accept()
            except socket.timeout:
                continue
            except OSError:
                if self.closed:
                    return
                raise
            self.clients.append(client)
            worker = threading.Thread(target=self.serve, args=(client,), daemon=True)
            self.workers.append(worker)
            worker.start()

    def serve(self, client):
        try:
            with client, client.makefile("rwb") as stream:
                while not self.closed:
                    header = stream.read(4)
                    if not header:
                        return
                    assert len(header) == 4
                    size, = struct.unpack("!I", header)
                    assert size <= 1024 * 1024
                    request = json.loads(stream.read(size))
                    assert request["version"] == 1 and request["type"] == "command"
                    if request["command"] not in ("theme-get", "theme-watch"):
                        # Other portal components share this socket. Never implement
                        # desktop operations in the appearance-only test peer.
                        response = {"version": 1, "id": request["id"], "error": {
                            "code": "unsupported_command", "message": "Only theme IPC is available in this test",
                        }}
                    else:
                        with self.condition:
                            if request["command"] == "theme-watch":
                                self.condition.wait_for(lambda: self.closed or request["args"]["since"] != self.current["revision"])
                            if self.closed:
                                return
                            response = {"version": 1, "id": request["id"], "result": self.current}
                    payload = json.dumps(response).encode()
                    stream.write(struct.pack("!I", len(payload)) + payload)
                    stream.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass  # The portal cancels its outstanding watch when it exits.
        except Exception as error:
            if not self.closed:
                self.errors.append(error)

    def close(self):
        with self.condition:
            self.closed = True
            self.condition.notify_all()
        self.thread.join(timeout=2)
        self.listener.close()
        for client in self.clients:
            try:
                client.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        for worker in self.workers:
            worker.join(timeout=2)
        assert not self.thread.is_alive() and not any(worker.is_alive() for worker in self.workers)
        assert not self.errors, self.errors
