"""Exercise the remote log helper without contacting a server."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

LOGS_SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "logs.sh"


class LogsScriptTest(unittest.TestCase):
    def run_logs(self, service, lines):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ssh = root / "ssh"
            output = root / "arguments.json"
            ssh.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                "from pathlib import Path\n"
                "Path(os.environ['LOGS_TEST_OUTPUT']).write_text(json.dumps(sys.argv[1:]))\n"
            )
            ssh.chmod(0o755)
            env = dict(
                os.environ,
                PATH=str(root) + os.pathsep + os.environ["PATH"],
                LXC_HOST="root@offline.invalid",
                LOGS_TEST_OUTPUT=str(output),
            )
            result = subprocess.run(
                ["bash", str(LOGS_SCRIPT), service, lines],
                env=env,
                capture_output=True,
                timeout=10,
            )
            arguments = json.loads(output.read_text()) if output.exists() else None
            return result, arguments

    def test_logs_passes_validated_values_to_ssh(self):
        result, arguments = self.run_logs("alert-bot.service", "500")
        self.assertEqual(result.returncode, 0, result.stderr)
        # A developer's scripts/local.env may set LXC_HOST, so the host is not
        # checked. Only the remote command carries the validated values.
        self.assertIsNotNone(arguments, "ssh was not called")
        self.assertEqual(len(arguments), 3)
        self.assertEqual(arguments[0], "-t")
        self.assertEqual(
            arguments[2],
            "exec journalctl --unit=alert-bot.service --lines=500 --follow",
        )

    def test_logs_rejects_shell_injection_before_ssh(self):
        attacks = [
            ("alert-bot;id", "100"),
            ("$(id)", "100"),
            ("alert-bot", "100;id"),
            ("alert-bot", "10001"),
            ("alert-bot", "18446744073709551617"),
        ]
        for service, lines in attacks:
            with self.subTest(service=service, lines=lines):
                result, arguments = self.run_logs(service, lines)
                self.assertEqual(result.returncode, 2)
                self.assertIsNone(arguments)


if __name__ == "__main__":
    unittest.main()
