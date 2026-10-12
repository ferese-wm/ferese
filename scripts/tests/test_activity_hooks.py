"""Hook composition/status checks use a fake publisher and never run metadata."""
from pathlib import Path
import shutil
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]

class ActivityHooks(unittest.TestCase):
    def test_bash_preserves_status_and_never_executes_captured_command(self):
        script = f'''
feresectl() {{ return 71; }}
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
