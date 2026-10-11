"""Opt-in lock protocol smoke test. Opens a temporary nested compositor.
Run: FERESE_TEST_LOCK=1 python3 scripts/tests/test_lock_isolated.py
Requires built binaries. FERESE_TEST_BIN_DIR selects debug/release.
No credentials are submitted; this test exercises protocol ownership, not PAM.
"""
import os
import unittest
from pathlib import Path
from session_socket import ipc_environment, ipc_socket

@unittest.skipUnless(os.environ.get("FERESE_TEST_LOCK") == "1", "visible nested lock test is opt-in")
class NativeLockTest(unittest.TestCase):
    def test_confirmation_capture_and_crash(self):
        import os, pathlib, signal, subprocess, tempfile, time
        os.chdir(Path(__file__).resolve().parents[2])
        root=pathlib.Path(tempfile.mkdtemp(prefix='ferese-lock-test-'))
        runtime=root/'runtime';runtime.mkdir(mode=0o700)
        config=root/'config'/'ferese';config.mkdir(parents=True)
        (config/'config.kdl').write_text('')
        host_display=os.environ.get('WAYLAND_DISPLAY','wayland-1')
        if not host_display.startswith('/'):host_display=str(pathlib.Path(os.environ['XDG_RUNTIME_DIR'])/host_display)
        env=dict(os.environ,XDG_RUNTIME_DIR=str(runtime),XDG_CONFIG_HOME=str(root/'config'),
                 XDG_STATE_HOME=str(root/'state'),XDG_CACHE_HOME=str(root/'cache'),
                 WAYLAND_DISPLAY=host_display,FERESE_ENABLE_SCREENCOPY='1')
        env.pop('WAYLAND_SOCKET', None)
        binary_dir = Path(os.environ.get('FERESE_TEST_BIN_DIR', 'target/release'))
        log=(root/'compositor.log').open('w')
        compositor=subprocess.Popen([str(binary_dir / 'ferese'),'--backend','nested'],env=ipc_environment(env),stdout=log,stderr=subprocess.STDOUT)
        ctl=binary_dir / 'feresectl'
        assert ctl.is_file(), f'missing {ctl}; build it with cargo build --release -p feresectl'
        locker=None
        locker_log=(root/'locker.log').open('w')
        try:
            for _ in range(100):
                if compositor.poll() is not None:raise RuntimeError('Nested compositor exited')
                sockets=[p for p in runtime.glob('wayland-*') if not p.name.endswith('.lock')]
                if sockets:break
                time.sleep(.1)
            childenv=dict(env,WAYLAND_DISPLAY=str(sockets[0]),FERESE_LOCK_READY='1',WAYLAND_DEBUG='1')
            def capture(destination, timeout=10):
                with open(destination,'wb') as png:
                    return subprocess.run([str(ctl),'screenshot'],env=ipc_environment(childenv),stdout=png,stderr=subprocess.PIPE,timeout=timeout)
            time.sleep(.6)
            result=capture(root/'unlocked.png')
            assert result.returncode == 0, f'Capture failed while unlocked: {result.stderr.decode()}'
            assert (root/'unlocked.png').stat().st_size > 0, 'Capture produced an empty file'
            print('Native capture succeeded while unlocked',flush=True)
            locker=subprocess.Popen([str(binary_dir / 'ferese-lock')],env=ipc_environment(childenv),stdout=subprocess.DEVNULL,stderr=locker_log,start_new_session=True)
            assert locker.wait(timeout=20) == 0, (root/'locker.log').read_text()
            self.assertRegex((root/'locker.log').read_text(), r'ext_session_lock_v1[@#]\d+\.locked\(')
            print('Native locker received compositor confirmation',flush=True)
            result=capture(root/'locked.png')
            assert result.returncode != 0, 'Capture unexpectedly succeeded during lock'
            print('Capture denied while native locker is running',flush=True)
            os.killpg(locker.pid, signal.SIGKILL)
            time.sleep(.3)
            result=capture(root/'after-crash.png')
            assert result.returncode != 0, 'Capture unexpectedly succeeded after locker crash'
            print('Capture remains denied after locker crash',flush=True)
            locker=subprocess.Popen([str(binary_dir / 'ferese-lock')],env=ipc_environment(childenv),stdout=subprocess.DEVNULL,stderr=locker_log,start_new_session=True)
            assert locker.wait(timeout=20) == 0, (root/'locker.log').read_text()
            result=capture(root/'replacement.png')
            assert result.returncode != 0, 'Capture unexpectedly succeeded after locker takeover'
            print('Replacement locker confirmed; capture remains denied',flush=True)
        finally:
            if locker is not None:
                try: os.killpg(locker.pid, signal.SIGKILL)
                except ProcessLookupError: pass
                locker.wait()
            compositor.terminate()
            try:compositor.wait(timeout=5)
            except subprocess.TimeoutExpired:compositor.kill();compositor.wait()
            log.close()
            locker_log.close()
            print(root,flush=True)

if __name__ == "__main__":
    unittest.main()
