import os
from pathlib import Path
import subprocess
import tempfile
import unittest


HELPER = Path(__file__).with_name("docker-git-credential.sh")


class DockerCredentialsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.token = Path(self.temp.name) / "token"
        self.token.write_text("synthetic-build-token")
        self.env = {
            **os.environ,
            "GITHUB_TOKEN_FILE": str(self.token),
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_ASKPASS": "/bin/false",
            "GIT_CONFIG_COUNT": "3",
            "GIT_CONFIG_KEY_0": "credential.helper",
            "GIT_CONFIG_VALUE_0": "",
            "GIT_CONFIG_KEY_1": "credential.https://github.com.helper",
            "GIT_CONFIG_VALUE_1": f"!sh {HELPER}",
            "GIT_CONFIG_KEY_2": "credential.https://github.com.useHttpPath",
            "GIT_CONFIG_VALUE_2": "true",
        }

    def fill(self, url):
        return subprocess.run(
            ["git", "credential", "fill"],
            input=f"url={url}\n\n", text=True, capture_output=True,
            env=self.env, cwd=self.temp.name,
        )

    def test_private_dependency(self):
        result = self.fill("https://github.com/UTEXO-Protocol/rgb-lib-bfa")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("username=x-access-token", result.stdout)
        self.assertIn("password=synthetic-build-token", result.stdout)

    def test_unrelated_urls_never_receive_token(self):
        for url in (
            "https://github.com/another-org/repo",
            "https://example.com/UTEXO-Protocol/repo",
            "http://github.com/UTEXO-Protocol/repo",
        ):
            with self.subTest(url=url):
                result = self.fill(url)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("synthetic-build-token", result.stdout + result.stderr)

    def test_missing_or_empty_secret_fails(self):
        for missing in (False, True):
            if missing:
                self.token.unlink()
            else:
                self.token.write_text("")
            result = self.fill("https://github.com/UTEXO-Protocol/rgb-lib-bfa")
            self.assertNotEqual(result.returncode, 0)

    def test_store_and_erase_do_not_persist_credentials(self):
        for operation in ("store", "erase"):
            result = subprocess.run(
                ["sh", str(HELPER), operation],
                input="protocol=https\nhost=github.com\n"
                      "path=UTEXO-Protocol/rgb-lib-bfa\npassword=do-not-save\n\n",
                text=True, capture_output=True, env=self.env, cwd=self.temp.name,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, "")
        self.assertEqual(list(Path(self.temp.name).iterdir()), [self.token])
        self.assertEqual(self.token.read_text(), "synthetic-build-token")


if __name__ == "__main__":
    unittest.main()
