"""Hook composition/status checks use a fake publisher and never run metadata."""
from pathlib import Path
import fcntl
import json
import os
import pty
import select
import shutil
import subprocess
import tempfile
import termios
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]

class ActivityHooks(unittest.TestCase):
    def test_real_interactive_commands_publish_start_and_exit_status(self):
        for shell in ('bash', 'fish'):
            if not shutil.which(shell):
                continue
            with self.subTest(shell=shell), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                publisher = root / 'feresectl'
                publisher.write_text('#!/usr/bin/env python3\nimport json, os, sys\n'
                                     'with open(os.environ["ACTIVITY_TEST_LOG"], "a") as f:\n'
                                     '    f.write(json.dumps(sys.argv[1:]) + "\\n")\n')
                publisher.chmod(0o755)
                log = root / 'calls'
                startup = root / 'startup'
                dependency = f'source "{ROOT}/contrib/activities/bash-preexec/bash-preexec.sh"\n' if shell == 'bash' else ''
                startup.write_text(dependency + f'source "{ROOT}/contrib/activities/ferese.{shell}"\n')
                master, slave = pty.openpty()
                env = dict(os.environ, HOME=directory, XDG_CONFIG_HOME=directory, HISTFILE='/dev/null',
                           PATH=str(root) + ':' + os.environ['PATH'], ACTIVITY_TEST_LOG=str(log), TERM='dumb', PS1='ACTIVITY_PROMPT> ')
                command = (['bash', '--noprofile', '--rcfile', str(startup), '-i'] if shell == 'bash' else
                           ['fish', '--no-config', '-i', '--init-command', f'function fish_prompt; printf "ACTIVITY_PROMPT> "; end; source {startup}'])
                process = subprocess.Popen(command, env=env, stdin=slave, stdout=slave, stderr=slave,
                                           start_new_session=True, preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0))
                os.close(slave)
                trace = b''
                def wait_for(predicate):
                    nonlocal trace
                    deadline = time.monotonic() + 5
                    while time.monotonic() < deadline:
                        if select.select([master], [], [], .05)[0]:
                            trace += os.read(master, 65536)
                        if predicate():
                            return
                    self.fail(trace.decode(errors='replace'))
                def calls():
                    return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
                try:
                    # Wait for the first prompt before sending an interactive command.
                    wait_for(lambda: b'# ' in trace or b'$ ' in trace or b'> ' in trace)
                    os.write(master, b'false\n')
                    wait_for(lambda: any(call[:2] == ['activity', 'end'] for call in calls()))
                    records = calls()
                    start = next(call for call in records if call[:2] == ['activity', 'hook'])
                    end = next(call for call in records if call[:2] == ['activity', 'end'])
                    self.assertEqual(start[start.index('--command') + 1], 'false')
                    self.assertEqual(end[2], start[2])
                    self.assertEqual(end[end.index('--result') + 1], 'fail')
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                    os.close(master)

    @unittest.skipUnless(shutil.which("fish"), "fish unavailable")
    def test_fish_preserves_status_and_does_not_execute_metadata(self):
        script = f'''function feresectl; return 71; end
source "{ROOT}/contrib/activities/ferese.fish"
__ferese_preexec 'echo SHOULD_NOT_EXECUTE'
false
__ferese_postexec
set saved $status
printf 'status=%s\\n' $saved
'''
        result = subprocess.run(["fish", "--no-config", "-i", "-c", script], capture_output=True, text=True, timeout=10)
        self.assertIn("status=1", result.stdout)
        self.assertNotIn("SHOULD_NOT_EXECUTE", result.stdout)

    def test_bash_preserves_status_and_never_executes_captured_command(self):
        script = f'''
feresectl() {{ return 71; }}
source "{ROOT}/contrib/activities/bash-preexec/bash-preexec.sh"
source "{ROOT}/contrib/activities/ferese.bash"
_ferese_arm
_ferese_preexec 'echo SHOULD_NOT_EXECUTE'
false
_ferese_finish
saved=$?
printf 'status=%s\\n' "$saved"
'''
        result = subprocess.run(["bash", "--norc", "-i", "-c", script], capture_output=True, text=True, timeout=10)
        self.assertIn("status=1", result.stdout)
        self.assertNotIn("SHOULD_NOT_EXECUTE", result.stdout)

    def test_bash_preserves_existing_debug_trap(self):
        script = f'''trap ':' DEBUG
before=$(trap -p DEBUG)
source "{ROOT}/contrib/activities/ferese.bash"
after=$(trap -p DEBUG)
[[ "$before" == "$after" ]]
'''
        result = subprocess.run(["bash", "--norc", "-i", "-c", script], capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)

    @unittest.skipUnless(shutil.which("zsh"), "zsh unavailable")
    def test_zsh_composes_hooks_and_preserves_status(self):
        script = f'''feresectl() {{ return 71; }}
existing() {{ :; }}
autoload -Uz add-zsh-hook
add-zsh-hook precmd existing
source "{ROOT}/contrib/activities/ferese.zsh"
(( ${{precmd_functions[(Ie)existing]}} > 0 )) || exit 4
_ferese_preexec 'echo SHOULD_NOT_EXECUTE'
false
_ferese_precmd
saved=$?
printf 'status=%s\\n' "$saved"
'''
        result = subprocess.run(["zsh", "-f", "-i", "-c", script], capture_output=True, text=True, timeout=10)
        self.assertIn("status=1", result.stdout)
        self.assertNotIn("SHOULD_NOT_EXECUTE", result.stdout)

if __name__ == "__main__":
    unittest.main()
