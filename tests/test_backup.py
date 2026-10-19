"""Exercise the real backup shell pipeline offline; never contact a server."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "backup-postgres.sh"
LOGS_SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "logs.sh"
README = Path(__file__).resolve().parents[1] / "README.md"


class BackupTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.keys = tempfile.TemporaryDirectory()
        cls.private_key = Path(cls.keys.name) / "private.pem"
        cls.public_key = Path(cls.keys.name) / "public.pem"
        subprocess.run(
            [
                "openssl",
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
                "-out",
                str(cls.private_key),
            ],
            check=True,
            capture_output=True,
        )
        subprocess.run(
            [
                "openssl",
                "pkey",
                "-in",
                str(cls.private_key),
                "-pubout",
                "-out",
                str(cls.public_key),
            ],
            check=True,
            capture_output=True,
        )

    @classmethod
    def tearDownClass(cls):
        cls.keys.cleanup()

    def run_backup(self, failure="", overrides=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binaries = root / "bin"
            remote = root / "remote"
            credentials = root / "credentials"
            binaries.mkdir()
            remote.mkdir()
            credentials.mkdir()
            signing_key = credentials / "backup-signing-key"
            signing_key.write_bytes(self.private_key.read_bytes())
            if failure == "sign":
                signing_key.write_text("invalid key")
            signing_key.chmod(0o400)
            known_hosts = root / "known_hosts"
            known_hosts.write_text("offline.invalid ssh-ed25519 AAAAtest\n")
            programs = {
                "pg_dump": """#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
root = Path(os.environ['BACKUP_TEST_REMOTE'])
args = sys.argv[1:]
(root / 'pg-dump-arguments.json').write_text(json.dumps(args))
if os.environ['BACKUP_TEST_FAILURE'] == 'dump':
    sys.exit(2)
output = next(arg.split('=', 1)[1] for arg in args if arg.startswith('--file='))
Path(output).write_bytes(b'PGDMP\\x01offline custom archive')
""",
                "sshpass": """#!/bin/sh
[ "$1" = -e ] || exit 90
[ "$SSHPASS" = "$BACKUP_TEST_EXPECTED_PASS" ] || exit 91
shift
exec "$@"
""",
                "sftp": """#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
root = Path(os.environ['BACKUP_TEST_REMOTE'])
args = sys.argv[1:]
with (root / 'sftp-invocations.jsonl').open('a') as output:
    output.write(json.dumps(args) + '\\n')
batch = Path(args[args.index('-b') + 1]).read_text()
with (root / 'sftp-batches').open('a') as output:
    output.write(batch + '--END--\\n')
if os.environ['BACKUP_TEST_FAILURE'] == 'sftp':
    sys.exit(3)
if 'ls -1' in batch and os.environ['BACKUP_TEST_FAILURE'] == 'old-files':
    print('alertbot-2000-01-01T000000Z.dump')
    print('alertbot-2000-01-01T000000Z.dump.sig')
    print('alertbot-2000-01-02T000000Z.dump.part')
    print('alertbot-2000-01-02T000000Z.dump.sig.part')
    print('not-a-backup.txt')
""",
                "curl": """#!/usr/bin/env python3
import json, os, stat, sys
from pathlib import Path
root = Path(os.environ['BACKUP_TEST_REMOTE'])
failure = os.environ['BACKUP_TEST_FAILURE']
args = sys.argv[1:]
if args == ['--help', 'all']:
    print('     --hostpubsha256 <sha256>')
    sys.exit(0)
with (root / 'curl-invocations.jsonl').open('a') as output:
    output.write(json.dumps(args) + '\\n')
config = Path(args[args.index('--config') + 1])
assert stat.S_IMODE(config.stat().st_mode) == 0o600
config_text = config.read_text()
assert config_text.startswith('user = "')
assert config_text.count('\\n') == 1
assert 'BACKUP_NETCUP_PASS' not in os.environ
if '--upload-file' in args:
    source = Path(args[args.index('--upload-file') + 1])
    target = 'signature.part' if source.name.endswith('.sig') else 'backup.part'
    (root / target).write_bytes(source.read_bytes())
    if failure == 'upload' or (failure == 'signature-upload' and target == 'signature.part'):
        sys.exit(22)
if '--quote' in args:
    command = args[args.index('--quote') + 1]
    parts = command.split()
    assert len(parts) == 3 and parts[0] == 'rename'
    with (root / 'renames').open('a') as output:
        output.write(command + '\\n')
    if failure == 'rename':
        sys.exit(21)
    if parts[1].endswith('.dump.sig.part'):
        (root / 'signature.part').rename(root / 'backup.dump.sig')
    elif parts[1].endswith('.dump.part'):
        (root / 'backup.part').rename(root / 'backup.dump')
""",
            }
            for name, program in programs.items():
                path = binaries / name
                path.write_text(program)
                path.chmod(0o755)
            env = dict(
                os.environ,
                PATH=str(binaries) + os.pathsep + os.environ["PATH"],
                TMPDIR=str(root),
                BACKUP_NETCUP_HOST="offline.invalid",
                BACKUP_NETCUP_USER="test",
                BACKUP_NETCUP_PASS="test-secret",
                BACKUP_SFTP_HOSTKEY_SHA256="AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                BACKUP_NETCUP_PATH="backups/test",
                BACKUP_KNOWN_HOSTS=str(known_hosts),
                CREDENTIALS_DIRECTORY=str(credentials),
                BACKUP_TEST_REMOTE=str(remote),
                BACKUP_TEST_FAILURE=failure,
            )
            env.update(overrides or {})
            env["BACKUP_TEST_EXPECTED_PASS"] = env["BACKUP_NETCUP_PASS"]
            result = subprocess.run(
                ["bash", str(SCRIPT)], env=env, capture_output=True, timeout=10
            )
            final = remote / "backup.dump"
            signature = remote / "backup.dump.sig"

            def json_lines(name):
                path = remote / name
                if not path.exists():
                    return []
                return [json.loads(line) for line in path.read_text().splitlines()]

            pg_dump_arguments = remote / "pg-dump-arguments.json"
            return {
                "status": result.returncode,
                "stdout": result.stdout,
                "stderr": result.stderr,
                "data": final.read_bytes() if final.exists() else None,
                "signature": signature.read_bytes() if signature.exists() else None,
                "renames": (remote / "renames").read_text().splitlines()
                if (remote / "renames").exists()
                else [],
                "curl": json_lines("curl-invocations.jsonl"),
                "sftp": json_lines("sftp-invocations.jsonl"),
                "pg_dump": json.loads(pg_dump_arguments.read_text())
                if pg_dump_arguments.exists()
                else [],
                "batches": (remote / "sftp-batches").read_text()
                if (remote / "sftp-batches").exists()
                else "",
                "temporary_files": [path.name for path in root.glob("alertbot-backup.*")],
            }

    def test_success_publishes_signed_custom_dump(self):
        result = self.run_backup()
        self.assertEqual(result["status"], 0, result["stderr"])
        self.assertEqual(result["data"], b"PGDMP\x01offline custom archive")
        self.assertIsNotNone(result["signature"])
        self.assertIn("--format=custom", result["pg_dump"])
        self.assertNotIn("--role", result["pg_dump"])
        self.assertEqual(len(result["renames"]), 2)
        self.assertIn(".dump.sig.part", result["renames"][0])
        self.assertIn(".dump.part", result["renames"][1])
        self.assertEqual(result["temporary_files"], [])

        with tempfile.TemporaryDirectory() as directory:
            dump = Path(directory) / "backup.dump"
            signature = Path(directory) / "backup.dump.sig"
            dump.write_bytes(result["data"])
            signature.write_bytes(result["signature"])
            verify = subprocess.run(
                [
                    "openssl",
                    "dgst",
                    "-sha256",
                    "-verify",
                    str(self.public_key),
                    "-signature",
                    str(signature),
                    str(dump),
                ],
                capture_output=True,
            )
            self.assertEqual(verify.returncode, 0, verify.stderr)
            dump.write_bytes(result["data"] + b"tampered")
            rejected = subprocess.run(
                [
                    "openssl",
                    "dgst",
                    "-sha256",
                    "-verify",
                    str(self.public_key),
                    "-signature",
                    str(signature),
                    str(dump),
                ],
                capture_output=True,
            )
            self.assertNotEqual(rejected.returncode, 0)

    def test_credentials_are_not_command_arguments(self):
        result = self.run_backup()
        self.assertEqual(result["status"], 0, result["stderr"])
        calls = result["curl"] + result["sftp"] + [result["pg_dump"]]
        arguments = [value for call in calls for value in call]
        self.assertFalse(any("test-secret" in value for value in arguments))
        for call in result["curl"]:
            self.assertIn("--hostpubsha256", call)
            self.assertIn("--connect-timeout", call)
            self.assertIn("--max-time", call)
        for call in result["sftp"]:
            self.assertIn("StrictHostKeyChecking=yes", call)
            self.assertIn("BatchMode=no", call)
            self.assertIn("ConnectTimeout=15", call)
            self.assertIn("ConnectionAttempts=1", call)
            self.assertIn("ServerAliveInterval=15", call)
            self.assertIn("ServerAliveCountMax=3", call)
            self.assertTrue(any(value.startswith("UserKnownHostsFile=") for value in call))

        unit = (SCRIPT.parent / "systemd/alertbot-backup.service").read_text()
        self.assertIn("TimeoutStartSec=1h", unit)

    def test_curl_config_escapes_password_control_characters(self):
        password = 'secret"\\\nurl = "file:///tmp/exfiltrate"'
        result = self.run_backup(overrides={"BACKUP_NETCUP_PASS": password})
        self.assertEqual(result["status"], 0, result["stderr"])
        arguments = [value for call in result["curl"] + result["sftp"] for value in call]
        self.assertFalse(any(password in value for value in arguments))

    def test_failed_creation_never_requests_rename(self):
        for failure in ["dump", "sign", "upload", "signature-upload"]:
            with self.subTest(failure=failure):
                result = self.run_backup(failure)
                self.assertNotEqual(result["status"], 0)
                self.assertIsNone(result["data"])
                self.assertEqual(result["renames"], [])

    def test_rename_and_retention_failures_are_reported(self):
        for failure in ["rename", "sftp"]:
            with self.subTest(failure=failure):
                result = self.run_backup(failure)
                self.assertNotEqual(result["status"], 0)

    def test_retention_deletes_dump_and_signature(self):
        result = self.run_backup("old-files")
        self.assertEqual(result["status"], 0, result["stderr"])
        self.assertIn("rm alertbot-2000-01-01T000000Z.dump\n", result["batches"])
        self.assertIn("rm alertbot-2000-01-01T000000Z.dump.sig\n", result["batches"])
        self.assertIn("rm alertbot-2000-01-02T000000Z.dump.part\n", result["batches"])
        self.assertIn("rm alertbot-2000-01-02T000000Z.dump.sig.part\n", result["batches"])
        self.assertNotIn("not-a-backup.txt", result["batches"])

    def test_invalid_remote_path_is_rejected_before_commands_run(self):
        result = self.run_backup(overrides={"BACKUP_NETCUP_PATH": "backups/test\nrm victim"})
        self.assertEqual(result["status"], 2)
        self.assertEqual(result["curl"], [])
        self.assertEqual(result["sftp"], [])

    def test_injected_environment_values_are_rejected(self):
        attacks = [
            {"BACKUP_NETCUP_HOST": "offline.invalid;id"},
            {"BACKUP_NETCUP_USER": "test$(id)"},
            {"BACKUP_SFTP_HOSTKEY_SHA256": "SHA256:not-a-valid-pin"},
            {"DB_NAME": "alertbot;drop"},
            {"RETENTION_DAYS": "30;id"},
        ]
        for override in attacks:
            with self.subTest(override=override):
                result = self.run_backup(overrides=override)
                self.assertEqual(result["status"], 2)
                self.assertEqual(result["curl"], [])
                self.assertEqual(result["sftp"], [])

    def test_missing_trust_files_are_rejected(self):
        overrides = [
            {"BACKUP_KNOWN_HOSTS": "/missing/known_hosts"},
            {"CREDENTIALS_DIRECTORY": "/missing/credentials"},
        ]
        for override in overrides:
            with self.subTest(override=override):
                result = self.run_backup(overrides=override)
                self.assertEqual(result["status"], 2)
                self.assertEqual(result["curl"], [])
                self.assertEqual(result["sftp"], [])

    def test_restore_verifies_signature_before_touching_database(self):
        readme = README.read_text()
        migration = readme.split("#### 6. Migrate data", 1)[1].split(
            "#### 7. Cloudflare", 1
        )[0]
        restore = readme.split("### Restoring from backup", 1)[1].split(
            "## Update workflow", 1
        )[0]
        verify = restore.index("openssl dgst -sha256 -verify")
        self.assertLess(verify, restore.index("systemctl stop"))
        self.assertLess(verify, restore.index("pg_restore"))
        self.assertNotIn("psql", restore)
        self.assertIn("--max-filesize", restore)
        self.assertIn("--hostpubsha256", restore)
        self.assertIn('--dbname=alertbot < "$migration"', migration)
        self.assertIn('--dbname=alertbot < "$backup"', restore)
        self.assertNotIn('--dbname=alertbot "$migration"', migration)
        self.assertNotIn('--dbname=alertbot "$backup"', restore)
        self.assertIn("Never restore an unsigned dump", restore)


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
        self.assertEqual(
            arguments,
            [
                "-t",
                "root@offline.invalid",
                "exec journalctl --unit=alert-bot.service --lines=500 --follow",
            ],
        )

    def test_logs_rejects_shell_injection_before_ssh(self):
        attacks = [
            ("alert-bot;id", "100"),
            ("$(id)", "100"),
            ("alert-bot", "100;id"),
            ("alert-bot", "10001"),
        ]
        for service, lines in attacks:
            with self.subTest(service=service, lines=lines):
                result, arguments = self.run_logs(service, lines)
                self.assertEqual(result.returncode, 2)
                self.assertIsNone(arguments)


if __name__ == "__main__":
    unittest.main()
