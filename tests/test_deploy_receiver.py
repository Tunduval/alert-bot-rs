"""Keep the deploy receiver's pinned unit hash in step with the unit file."""
import hashlib
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[1]
RECEIVER = ROOT / "scripts" / "alertbot-deploy-receiver.sh"
UNIT = ROOT / "scripts" / "systemd" / "alert-bot.service"


class DeployReceiverTest(unittest.TestCase):
    def test_pinned_unit_hash_matches_the_unit_file(self):
        pinned = re.search(
            r"^readonly SERVICE_UNIT_SHA256=([0-9a-f]{64})$",
            RECEIVER.read_text(),
            re.MULTILINE,
        )
        self.assertIsNotNone(pinned, "receiver must pin the unit hash")
        self.assertEqual(
            pinned.group(1),
            hashlib.sha256(UNIT.read_bytes()).hexdigest(),
            "update SERVICE_UNIT_SHA256 when alert-bot.service changes",
        )


if __name__ == "__main__":
    unittest.main()
