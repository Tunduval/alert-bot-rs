"""Exercise the real backup shell pipeline offline; never contact a server."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "backup-postgres.sh"
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
                "timeout": """#!/usr/bin/env python3
import os, sys
args = sys.argv[1:]
assert args[:3] == ['--signal=TERM', '--kill-after=5s', '60s']
os.execvp(args[3], args[3:])
""",
                "age": """#!/usr/bin/env python3
import json, os, stat, sys
from pathlib import Path
root = Path(os.environ['BACKUP_TEST_REMOTE'])
args = sys.argv[1:]
with (root / 'age-invocations.jsonl').open('a') as output:
    output.write(json.dumps(args) + '\\n')
recipient = args[args.index('--recipient') + 1]
if recipient != os.environ['BACKUP_TEST_AGE_RECIPIENT']:
    sys.exit(2)
if os.environ['BACKUP_TEST_FAILURE'] == 'recipient':
    sys.exit(2)
if '--output' not in args:
    sys.exit(0)
if os.environ['BACKUP_TEST_FAILURE'] == 'encrypt':
    sys.exit(3)
source = Path(args[-1])
assert source.name.endswith('.dump')
assert stat.S_IMODE(source.parent.stat().st_mode) == 0o700
assert stat.S_IMODE(source.stat().st_mode) == 0o600
(root / 'age-plaintext-seen').write_bytes(source.read_bytes())
target = Path(args[args.index('--output') + 1])
target.write_bytes(b'age-encryption.org/v1\\n' + source.read_bytes()[::-1])
""",
                "pg_dump": """#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
root = Path(os.environ['BACKUP_TEST_REMOTE'])
args = sys.argv[1:]
if args == ['--version']:
    print(os.environ.get('BACKUP_TEST_PG_DUMP_VERSION', 'pg_dump (PostgreSQL) 16.15'))
    sys.exit(0)
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
assert batch.splitlines()[0] == 'cd /' + os.environ['BACKUP_NETCUP_PATH']
with (root / 'sftp-batches').open('a') as output:
    output.write(batch + '--END--\\n')
if os.environ['BACKUP_TEST_FAILURE'] == 'sftp':
    sys.exit(3)
if 'ls -1' in batch and os.environ['BACKUP_TEST_FAILURE'] == 'old-files':
    print('alertbot-2000-01-01T000000Z.dump.age')
    print('alertbot-2000-01-01T000000Z.dump.age.manifest')
    print('alertbot-2000-01-01T000000Z.dump.age.manifest.sig')
    print('alertbot-2000-01-02T000000Z.dump.age.part')
    print('alertbot-2000-01-02T000000Z.dump.age.manifest.part')
    print('alertbot-2000-01-02T000000Z.dump.age.manifest.sig.part')
    print('alertbot-2000-01-03T000000Z.sql.gz')
    print('alertbot-2000-01-03T000000Z.sql.gz.part')
    print('not-a-backup.txt')
if 'ls -1' in batch and os.environ['BACKUP_TEST_FAILURE'] == 'list-overflow':
    sys.stdout.write('x' * 1048577)
""",
                "curl": """#!/usr/bin/env python3
import json, os, stat, sys
from pathlib import Path
root = Path(os.environ['BACKUP_TEST_REMOTE'])
failure = os.environ['BACKUP_TEST_FAILURE']
args = sys.argv[1:]
with (root / 'curl-invocations.jsonl').open('a') as output:
    output.write(json.dumps(args) + '\\n')
known_hosts = Path(os.environ['HOME']) / '.ssh' / 'known_hosts'
assert known_hosts.read_bytes() == Path(os.environ['BACKUP_KNOWN_HOSTS']).read_bytes()
assert stat.S_IMODE(known_hosts.stat().st_mode) == 0o600
config = Path(args[args.index('--config') + 1])
assert stat.S_IMODE(config.stat().st_mode) == 0o600
config_text = config.read_text()
assert config_text.startswith('user = "')
assert config_text.count('\\n') == 1
assert 'BACKUP_NETCUP_PASS' not in os.environ
if '--upload-file' in args:
    source = Path(args[args.index('--upload-file') + 1])
    if source.name.endswith('.dump.age'):
        assert not source.with_suffix('').exists()
    remote_path = args[-1].split(os.environ['BACKUP_NETCUP_HOST'], 1)[1]
    (root / 'uploaded-paths').open('a').write(remote_path + '\\n')
    if source.name.endswith('.manifest.sig'):
        target = 'signature.part'
    elif source.name.endswith('.manifest'):
        target = 'manifest.part'
    else:
        target = 'backup.part'
    (root / target).write_bytes(source.read_bytes())
    if failure == 'upload' or (failure == f'{target[:-5]}-upload'):
        sys.exit(22)
if '--quote' in args:
    command = args[args.index('--quote') + 1]
    parts = command.split()
    assert len(parts) == 3 and parts[0] == 'rename'
    assert parts[1] in (root / 'uploaded-paths').read_text().splitlines()
    assert parts[2] == parts[1].removesuffix('.part')
    with (root / 'renames').open('a') as output:
        output.write(command + '\\n')
    if failure == 'rename':
        sys.exit(21)
    if parts[1].endswith('.dump.age.manifest.sig.part'):
        (root / 'signature.part').rename(root / 'backup.dump.manifest.sig')
    elif parts[1].endswith('.dump.age.manifest.part'):
        (root / 'manifest.part').rename(root / 'backup.dump.manifest')
    elif parts[1].endswith('.dump.age.part'):
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
                BACKUP_AGE_RECIPIENT="age1" + "q" * 58,
                BACKUP_NETCUP_PATH="backups/test",
                BACKUP_KNOWN_HOSTS=str(known_hosts),
                CREDENTIALS_DIRECTORY=str(credentials),
                BACKUP_TEST_REMOTE=str(remote),
                BACKUP_TEST_FAILURE=failure,
            )
            env.update(overrides or {})
            env["BACKUP_TEST_EXPECTED_PASS"] = env["BACKUP_NETCUP_PASS"]
            env["BACKUP_TEST_AGE_RECIPIENT"] = env["BACKUP_AGE_RECIPIENT"]
            result = subprocess.run(
                ["bash", str(SCRIPT)], env=env, capture_output=True, timeout=10
            )
            final = remote / "backup.dump"
            manifest = remote / "backup.dump.manifest"
            signature = remote / "backup.dump.manifest.sig"

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
                "manifest": manifest.read_bytes() if manifest.exists() else None,
                "signature": signature.read_bytes() if signature.exists() else None,
                "renames": (remote / "renames").read_text().splitlines()
                if (remote / "renames").exists()
                else [],
                "curl": json_lines("curl-invocations.jsonl"),
                "sftp": json_lines("sftp-invocations.jsonl"),
                "age": json_lines("age-invocations.jsonl"),
                "age_plaintext": (remote / "age-plaintext-seen").read_bytes()
                if (remote / "age-plaintext-seen").exists()
                else None,
                "pg_dump": json.loads(pg_dump_arguments.read_text())
                if pg_dump_arguments.exists()
                else [],
                "batches": (remote / "sftp-batches").read_text()
                if (remote / "sftp-batches").exists()
                else "",
                "temporary_files": [path.name for path in root.glob("alertbot-backup.*")],
            }

    def assert_valid_manifest(self, result):
        self.assertIsNotNone(result["manifest"])
        lines = result["manifest"].decode("ascii").splitlines()
        self.assertEqual(len(lines), 6)
        fields = dict(line.split("=", 1) for line in lines)
        self.assertEqual(
            list(fields),
            [
                "format_version",
                "database",
                "filename",
                "created_at",
                "size_bytes",
                "sha256",
            ],
        )
        self.assertEqual(fields["format_version"], "2")
        self.assertEqual(fields["database"], "alertbot")
        self.assertRegex(
            fields["filename"],
            r"^alertbot-[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{6}Z\.dump\.age$",
        )
        self.assertEqual(
            fields["filename"], f"alertbot-{fields['created_at']}.dump.age"
        )
        self.assertEqual(int(fields["size_bytes"]), len(result["data"]))
        self.assertEqual(fields["sha256"], hashlib.sha256(result["data"]).hexdigest())
        return fields

    def test_success_publishes_only_signed_encrypted_dump(self):
        result = self.run_backup()
        self.assertEqual(result["status"], 0, result["stderr"])
        self.assertEqual(result["age_plaintext"], b"PGDMP\x01offline custom archive")
        self.assertEqual(
            result["data"],
            b"age-encryption.org/v1\n"
            + b"PGDMP\x01offline custom archive"[::-1],
        )
        self.assertIsNotNone(result["signature"])
        self.assert_valid_manifest(result)
        self.assertIn("--format=custom", result["pg_dump"])
        self.assertIn("--no-blobs", result["pg_dump"])
        self.assertNotIn("--role", result["pg_dump"])
        self.assertEqual(len(result["renames"]), 3)
        self.assertIn(".dump.age.manifest.sig.part", result["renames"][0])
        self.assertIn(".dump.age.manifest.part", result["renames"][1])
        self.assertIn(".dump.age.part", result["renames"][2])
        fields = self.assert_valid_manifest(result)
        uploaded_names = [
            Path(call[call.index("--upload-file") + 1]).name
            for call in result["curl"]
            if "--upload-file" in call
        ]
        self.assertEqual(
            sorted(uploaded_names),
            sorted(
                [
                    fields["filename"],
                    fields["filename"] + ".manifest",
                    fields["filename"] + ".manifest.sig",
                ]
            ),
        )
        self.assertEqual(result["temporary_files"], [])

        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "backup.dump.manifest"
            signature = Path(directory) / "backup.dump.manifest.sig"
            manifest.write_bytes(result["manifest"])
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
                    str(manifest),
                ],
                capture_output=True,
            )
            self.assertEqual(verify.returncode, 0, verify.stderr)

    def test_manifest_tampering_breaks_the_signature(self):
        result = self.run_backup()
        self.assertEqual(result["status"], 0, result["stderr"])
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "backup.dump.manifest"
            signature = Path(directory) / "backup.dump.manifest.sig"
            manifest.write_bytes(result["manifest"] + b"tampered")
            signature.write_bytes(result["signature"])
            rejected = subprocess.run(
                [
                    "openssl",
                    "dgst",
                    "-sha256",
                    "-verify",
                    str(self.public_key),
                    "-signature",
                    str(signature),
                    str(manifest),
                ],
                capture_output=True,
            )
            self.assertNotEqual(rejected.returncode, 0)

    def test_ciphertext_tampering_breaks_the_manifest_hash(self):
        result = self.run_backup()
        self.assertEqual(result["status"], 0, result["stderr"])
        fields = self.assert_valid_manifest(result)
        tampered = result["data"] + b"tampered"
        self.assertNotEqual(hashlib.sha256(tampered).hexdigest(), fields["sha256"])

    def test_credentials_are_not_command_arguments(self):
        result = self.run_backup()
        self.assertEqual(result["status"], 0, result["stderr"])
        calls = result["curl"] + result["sftp"] + [result["pg_dump"]]
        arguments = [value for call in calls for value in call]
        self.assertFalse(any("test-secret" in value for value in arguments))
        for call in result["curl"]:
            self.assertNotIn("--hostpubsha256", call)
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

    def test_pg_dump_accepts_only_known_fixed_versions(self):
        accepted = ["16.15", "16.99", "17.11", "18.6"]
        for version in accepted:
            with self.subTest(version=version):
                result = self.run_backup(
                    overrides={
                        "BACKUP_TEST_PG_DUMP_VERSION": (
                            f"pg_dump (PostgreSQL) {version} (Debian test)"
                        )
                    }
                )
                self.assertEqual(result["status"], 0, result["stderr"])

    def test_pg_dump_rejects_vulnerable_unknown_or_future_versions(self):
        rejected = ["16.14", "17.10", "18.5", "19.0", "not-a-version"]
        for version in rejected:
            with self.subTest(version=version):
                prefix = "" if version == "not-a-version" else "pg_dump (PostgreSQL) "
                result = self.run_backup(
                    overrides={"BACKUP_TEST_PG_DUMP_VERSION": prefix + version}
                )
                self.assertEqual(result["status"], 2)
                self.assertEqual(result["pg_dump"], [])
                self.assertEqual(result["curl"], [])
                self.assertEqual(result["sftp"], [])

    def test_curl_config_escapes_password_control_characters(self):
        password = 'secret"\\\nurl = "file:///tmp/exfiltrate"'
        result = self.run_backup(overrides={"BACKUP_NETCUP_PASS": password})
        self.assertEqual(result["status"], 0, result["stderr"])
        arguments = [value for call in result["curl"] + result["sftp"] for value in call]
        self.assertFalse(any(password in value for value in arguments))

    def test_failed_creation_never_requests_rename(self):
        for failure in [
            "dump",
            "encrypt",
            "sign",
            "upload",
            "manifest-upload",
            "signature-upload",
        ]:
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

    def test_retention_deletes_dump_manifest_and_signature(self):
        result = self.run_backup("old-files")
        self.assertEqual(result["status"], 0, result["stderr"])
        self.assertIn("rm alertbot-2000-01-01T000000Z.dump.age\n", result["batches"])
        self.assertIn(
            "rm alertbot-2000-01-01T000000Z.dump.age.manifest\n",
            result["batches"],
        )
        self.assertIn(
            "rm alertbot-2000-01-01T000000Z.dump.age.manifest.sig\n",
            result["batches"],
        )
        self.assertIn(
            "rm alertbot-2000-01-02T000000Z.dump.age.part\n", result["batches"]
        )
        self.assertIn(
            "rm alertbot-2000-01-02T000000Z.dump.age.manifest.part\n",
            result["batches"],
        )
        self.assertIn(
            "rm alertbot-2000-01-02T000000Z.dump.age.manifest.sig.part\n",
            result["batches"],
        )
        self.assertIn("rm alertbot-2000-01-03T000000Z.sql.gz\n", result["batches"])
        self.assertIn("rm alertbot-2000-01-03T000000Z.sql.gz.part\n", result["batches"])
        self.assertNotIn("not-a-backup.txt", result["batches"])

    def test_retention_rejects_an_oversized_listing(self):
        result = self.run_backup("list-overflow")
        self.assertNotEqual(result["status"], 0)
        self.assertIn(b"retention listing exceeds 1048576 bytes", result["stderr"])

    def test_invalid_remote_path_is_rejected_before_commands_run(self):
        result = self.run_backup(overrides={"BACKUP_NETCUP_PATH": "backups/test\nrm victim"})
        self.assertEqual(result["status"], 2)
        self.assertEqual(result["curl"], [])
        self.assertEqual(result["sftp"], [])

    def test_injected_environment_values_are_rejected(self):
        attacks = [
            {"BACKUP_NETCUP_HOST": "offline.invalid;id"},
            {"BACKUP_NETCUP_USER": "test$(id)"},
            {"BACKUP_AGE_RECIPIENT": "not-an-age-recipient"},
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

    def test_age_rejects_a_bad_recipient_before_reading_the_database(self):
        result = self.run_backup("recipient")
        self.assertEqual(result["status"], 2)
        self.assertEqual(result["pg_dump"], [])
        self.assertEqual(result["curl"], [])
        self.assertEqual(result["sftp"], [])

    def test_restore_rejects_renamed_replay_before_touching_database(self):
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
        stop = restore.index("systemctl stop")
        checks = [
            '[[ "$manifest_database" == alertbot ]]',
            '[[ "$manifest_filename" == "$backup_name" ]]',
            '[[ "$actual_size" == "$manifest_size" ]]',
            '[[ "$actual_sha256" == "$manifest_sha256" ]]',
        ]
        for check in checks:
            self.assertLess(restore.index(check), stop)
        self.assertNotIn('psql < "$backup"', restore)
        self.assertNotIn("--clean", restore)
        self.assertNotIn("--clean --if-exists", migration)
        for procedure in [migration, restore]:
            self.assertLess(procedure.index('[[ "$target_tables" == 0 ]]'),
                            procedure.index("sudo -u alertbot pg_restore"))
        self.assertIn("--max-filesize", restore)
        self.assertNotIn("--hostpubsha256", restore)
        self.assertEqual(restore.count('HOME="$curl_home" curl'), 3)
        self.assertIn(
            'install -m 0600 /etc/alertbot-backup-known-hosts "$curl_home/.ssh/known_hosts"',
            restore,
        )
        self.assertIn("/etc/alertbot-restore/signing-public.pem", restore)
        decrypt = restore.index("age --decrypt")
        self.assertLess(restore.index('[[ "$actual_sha256" == "$manifest_sha256" ]]'), decrypt)
        self.assertLess(decrypt, stop)
        self.assertLess(restore.index('umount "$identity_mount"'), stop)
        self.assertIn('age_identity="$identity_mount/alertbot-backup.agekey"', restore)
        self.assertNotIn("cp $age_identity", restore)
        self.assertNotIn("install $age_identity", restore)
        self.assertIn('return "$cleanup_status"', restore)
        self.assertIn('--dbname=alertbot < "$migration"', migration)
        self.assertIn('--dbname=alertbot < "$backup"', restore)
        self.assertNotIn('--dbname=alertbot "$migration"', migration)
        self.assertNotIn('--dbname=alertbot "$backup"', restore)
        self.assertIn("Never restore a dump without its signed manifest", restore)
        self.assertIn("never\ncopy it onto the production LXC", restore)
        self.assertIn("leaves the bot", restore)
        self.assertIn("stopped", restore)


if __name__ == "__main__":
    unittest.main()
