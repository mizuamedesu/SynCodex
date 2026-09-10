#!/usr/bin/env python3
"""Exercise install.sh offline with release-shaped archives and an isolated home."""
import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]

class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.home = self.root / "user's home"
        self.home.mkdir()
        self.fake = self.root / "commands"
        self.fake.mkdir()
        self.assets = self.root / "assets"
        self.assets.mkdir()
        self.env = dict(os.environ, HOME=str(self.home), PATH=str(self.fake) + os.pathsep + os.environ['PATH'],
                        FIXTURE_ASSETS=str(self.assets), FIXTURE_OS='Linux', FIXTURE_ARCH='x86_64')
        for key in ['SYNCODEX_VERSION', 'SYNCODEX_BIN_DIR', 'SYNCODEX_CODEX_SKILLS_DIR', 'SYNCODEX_CLAUDE_SKILLS_DIR', 'CLAUDE_CONFIG_DIR']:
            self.env.pop(key, None)
        self.tool('uname', '#!/bin/sh\ncase "$1" in -s) echo "$FIXTURE_OS";; -m) echo "$FIXTURE_ARCH";; esac\n')
        self.tool('curl', '''#!/usr/bin/env python3
import os,sys,pathlib,shutil
args=sys.argv[1:]; url=args[-1]
if url.endswith('/releases/latest'):
 print('https://github.com/mizuamedesu/SynCodex/releases/tag/v0.1.0',end='')
else:
 source=pathlib.Path(os.environ['FIXTURE_ASSETS'])/url.rsplit('/',1)[-1]
 if not source.exists():sys.exit(22)
 shutil.copyfile(source,args[args.index('-o')+1])
''')
        for target in ['x86_64-unknown-linux-musl', 'aarch64-apple-darwin']:
            path = self.assets / f'syncodex-v0.1.0-{target}.tar.gz'
            binary = os.environ.get('SYNCODEX_TEST_BINARY')
            payload = Path(binary).read_bytes() if binary else b'#!/bin/sh\nprintf "syncodex 0.1.0\\n"\n'
            with tarfile.open(path, 'w:gz') as archive:
                for name, contents, mode in [
                    ('syncodex', payload, 0o755),
                    ('skills/syncodex/SKILL.md', (ROOT/'skills/syncodex/SKILL.md').read_bytes(), 0o644),
                    ('LICENSE', (ROOT/'LICENSE').read_bytes(), 0o644),
                ]:
                    info = tarfile.TarInfo(name); info.size = len(contents); info.mode = mode
                    archive.addfile(info, io.BytesIO(contents))
            Path(str(path)+'.sha256').write_text(hashlib.sha256(path.read_bytes()).hexdigest()+'  '+path.name+'\n')

    def tearDown(self):
        self.tmp.cleanup()

    def tool(self, name, contents):
        path = self.fake/name; path.write_text(contents); path.chmod(0o755)

    def install(self, *args, success=True):
        # Feed through stdin to exercise the actual curl | sh execution shape.
        result = subprocess.run(['sh', '-s', '--', *args], input=(ROOT/'install.sh').read_text(),
                                text=True, env=self.env, capture_output=True)
        self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
        return result

    def test_install_and_repeat_with_path_and_both_skills(self):
        for _ in range(2): self.install()
        binary = self.home/'.local/bin/syncodex'
        self.assertTrue(os.access(binary, os.X_OK))
        for parent in ['.agents/skills', '.claude/skills']:
            self.assertEqual((self.home/parent/'syncodex/SKILL.md').read_bytes(), (ROOT/'skills/syncodex/SKILL.md').read_bytes())
        for name in ['.profile', '.bashrc', '.zshrc']:
            self.assertEqual((self.home/name).read_text().count('# SynCodex'), 1)
        probe = subprocess.check_output(['sh','-c','. "$HOME/.local/share/syncodex/env.sh"; . "$HOME/.local/share/syncodex/env.sh"; command -v syncodex'],env=self.env,text=True)
        self.assertEqual(probe.strip(),str(binary))

    def test_arm_mac_and_custom_directories(self):
        self.env.update(FIXTURE_OS='Darwin',FIXTURE_ARCH='arm64',
                        SYNCODEX_CODEX_SKILLS_DIR=str(self.home/'codex skills'),CLAUDE_CONFIG_DIR=str(self.home/'claude config'))
        bindir=self.home/"custom 'bin"
        self.install('--version','v0.1.0','--bin-dir',str(bindir))
        self.assertTrue((self.home/'codex skills/syncodex/SKILL.md').exists())
        self.assertTrue((self.home/'claude config/skills/syncodex/SKILL.md').exists())
        probe = subprocess.check_output(['sh','-c','. "$HOME/.profile"; command -v syncodex'],env=self.env,text=True)
        self.assertEqual(probe.strip(),str(bindir/'syncodex'))

    def test_bad_checksum_leaves_home_untouched(self):
        for path in self.assets.glob('*.sha256'): path.write_text('0'*64+'  archive\n')
        self.install(success=False)
        self.assertEqual(list(self.home.iterdir()),[])

    def test_failed_download_and_unsupported_platform(self):
        self.install('--version','v9.9.9',success=False)
        self.assertEqual(list(self.home.iterdir()),[])
        self.env['FIXTURE_ARCH']='aarch64'
        self.install(success=False)
        self.assertEqual(list(self.home.iterdir()),[])

    def test_unmanaged_skill_is_preserved(self):
        skill=self.home/'.claude/skills/syncodex'
        skill.mkdir(parents=True); (skill/'SKILL.md').write_text('my own skill')
        self.install(success=False)
        self.assertEqual((skill/'SKILL.md').read_text(),'my own skill')
        self.assertFalse((self.home/'.local/bin/syncodex').exists())

    def test_opt_out_and_invalid_version(self):
        self.install('--no-skills','--no-modify-path')
        self.assertTrue((self.home/'.local/bin/syncodex').exists())
        self.assertFalse((self.home/'.agents').exists())
        self.assertFalse((self.home/'.bashrc').exists())
        self.install('--version','../../bad',success=False)

if __name__ == '__main__':
    unittest.main()
