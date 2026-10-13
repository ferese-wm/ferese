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
            if shutil.which(shell):
                with self.subTest(shell=shell):
                    self.check_interactive_commands(shell)

    def test_bash_prompt_composition_and_delayed_installation(self):
        for prompt in ('scalar', 'array'):
            for installed in (False, True):
                for extdebug in (False, True):
                    with self.subTest(prompt=prompt, installed=installed, extdebug=extdebug):
                        self.check_interactive_commands('bash', prompt, installed, extdebug)

    def test_bash_publisher_failure_does_not_skip_the_next_command(self):
        self.check_interactive_commands('bash', 'array', extdebug=True, publisher_status=71)

    def check_interactive_commands(self, shell, prompt=None, installed=False, extdebug=False, publisher_status=0):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            publisher = root / 'feresectl'
            publisher.write_text('#!/usr/bin/env python3\nimport json, os, sys\n'
                                 'with open(os.environ["ACTIVITY_TEST_LOG"], "a") as f:\n'
                                 '    f.write(json.dumps(sys.argv[1:]) + "\\n")\n'
                                 f'sys.exit({publisher_status})\n')
            publisher.chmod(0o755)
            log = root / 'calls'
            statuses = root / 'statuses'
            startup = root / 'startup'
            source_hook = f'source "{ROOT}/contrib/activities/ferese.{shell}"\n'
            setup = ''
            if shell == 'bash':
                if prompt:
                    setup += ('_activity_prompt() { printf "%s\\n" "$?" >> "$ACTIVITY_TEST_STATUS"; }\n'
                              '_activity_precmd() { return 7; }\n'
                              'precmd_functions=(_activity_precmd)\n')
                    setup += ('PROMPT_COMMAND=_activity_prompt\n' if prompt == 'scalar' else
                              'PROMPT_COMMAND=(_activity_prompt :)\n')
                setup += f'source "{ROOT}/contrib/activities/bash-preexec/bash-preexec.sh"\n'
                if extdebug:
                    setup += 'shopt -s extdebug\n'
            if not installed:
                setup += source_hook * 2
            startup.write_text(setup)
            master, slave = pty.openpty()
            env = dict(os.environ, HOME=directory, XDG_CONFIG_HOME=directory, HISTFILE='/dev/null',
                       PATH=str(root) + ':' + os.environ['PATH'], ACTIVITY_TEST_LOG=str(log),
                       ACTIVITY_TEST_STATUS=str(statuses), TERM='dumb', PS1='ACTIVITY_PROMPT> ')
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
            def sleep_is_foreground():
                try:
                    return (Path('/proc') / str(os.tcgetpgrp(master)) / 'comm').read_text().strip() == 'sleep'
                except FileNotFoundError:
                    return False
            def next_prompt(command, interrupt=False, publish=True):
                previous = trace.count(b'ACTIVITY_PROMPT> ')
                before = len(calls())
                os.write(master, command.encode() + b'\n')
                if interrupt:
                    wait_for(sleep_is_foreground)
                    os.write(master, b'\x03')
                if command and publish:
                    wait_for(lambda: len(calls()) >= before + 2)
                wait_for(lambda: trace.count(b'ACTIVITY_PROMPT> ') > previous)
            try:
                wait_for(lambda: b'ACTIVITY_PROMPT> ' in trace)
                if installed:
                    # Let bash-preexec finish its delayed installation before sourcing Ferese.
                    next_prompt(source_hook.strip() + '; ' + source_hook.strip(), publish=False)
                self.assertEqual(calls(), [], 'startup and prompt callbacks are not activities')
                seen = set()
                commands = [('false', 'fail', 1)]
                if shell == 'bash':
                    commands += [
                        ("printf 'ACTIVITY_STATUS=%s\\n' \"$?\"", 'ok', 0),
                        ('true', 'ok', 0),
                        ('true | false', 'fail', 1),
                        ('false | true', 'ok', 0),
                        ('sleep 30', 'cancel', 130),
                        ("sh -c 'exit 143'", 'cancel', 143),
                        ('true', 'ok', 0),
                    ]
                for command, result, status in commands:
                    before = len(calls())
                    next_prompt(command, interrupt=command == 'sleep 30')
                    records = calls()[before:]
                    self.assertEqual(len(records), 2, records)
                    start, end = records
                    self.assertEqual(start[:2], ['activity', 'hook'])
                    self.assertEqual(start[start.index('--command') + 1], command)
                    self.assertNotIn(start[2], seen)
                    seen.add(start[2])
                    self.assertEqual(end[:3], ['activity', 'cancel' if result == 'cancel' else 'end', start[2]])
                    if result != 'cancel':
                        self.assertEqual(end[end.index('--result') + 1], result)
                    if command.startswith("printf 'ACTIVITY_STATUS="):
                        self.assertIn(b'\r\nACTIVITY_STATUS=1\r\n', trace)
                    if prompt:
                        self.assertEqual(int(statuses.read_text().splitlines()[-1]), status)
                    next_prompt('')
                    self.assertEqual(calls()[before:], records, 'an empty prompt must not publish another event')
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
