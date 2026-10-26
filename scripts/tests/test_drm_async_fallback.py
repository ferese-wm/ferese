"""Exercise the production DRM retry boundary without opening a DRM device."""
from pathlib import Path
import json
import shutil
import subprocess
import tempfile
import unittest


class AsyncFlipFallback(unittest.TestCase):
    def test_driver_rejection_retries_once_and_other_results_preserve_ownership(self):
        rustc = shutil.which("rustc")
        if rustc is None:
            self.skipTest("Rust compiler unavailable")
        source = Path(__file__).resolve().parents[2] / "vendor/smithay/src/backend/drm/async_flip.rs"
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            harness = directory / "harness.rs"
            harness.write_text(f"#[path = {json.dumps(str(source))}]\nmod async_flip;\n")
            binary = directory / "async-flip-tests"
            subprocess.run(
                [rustc, "--edition=2024", "--test", str(harness), "-o", str(binary)],
                check=True, capture_output=True, text=True, timeout=30,
            )
            result = subprocess.run(
                [str(binary), "--nocapture"],
                check=True, capture_output=True, text=True, timeout=10,
            )
            self.assertIn("2 passed", result.stdout)


if __name__ == "__main__":
    unittest.main()
