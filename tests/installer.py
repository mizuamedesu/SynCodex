#!/usr/bin/env python3
"""Exercise install.sh offline with release-shaped archives and an isolated home."""
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
VERSION = re.search(r'^version\s*=\s*"([^"]+)"', (ROOT/'Cargo.toml').read_text(), re.MULTILINE).group(1)
TAG = f'v{VERSION}'

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
        self.env = dict(os.environ, HOME=str(self.home), PATH=str(self.fake),
                        FIXTURE_ASSETS=str(self.assets), FIXTURE_OS='Linux', FIXTURE_ARCH='x86_64', FIXTURE_TAG=TAG,
                        FIXTURE_COMMANDS=str(self.fake), FIXTURE_PM_LOG=str(self.root/'packages.jsonl'), FIXTURE_UID='0')
        # Keep package-manager tests isolated from the host's nano, sudo and package managers.
        for name in ['sh', 'tar', 'awk', 'sed', 'mktemp', 'grep', 'cp', 'chmod', 'mv', 'mkdir', 'rm', 'env', 'sha256sum', 'shasum']:
            executable = shutil.which(name)
            if executable:
                (self.fake/name).symlink_to(executable)
        (self.fake/'python3').symlink_to(sys.executable)
        self.tool('nano', '#!/bin/sh\nexit 0\n')
        self.tool('id', '#!/bin/sh\nprintf "%s\\n" "$FIXTURE_UID"\n')
        for key in ['SYNCODEX_VERSION', 'SYNCODEX_BIN_DIR', 'SYNCODEX_CODEX_SKILLS_DIR', 'SYNCODEX_CLAUDE_SKILLS_DIR', 'CLAUDE_CONFIG_DIR']:
            self.env.pop(key, None)
        self.tool('uname', '#!/bin/sh\ncase "$1" in -s) echo "$FIXTURE_OS";; -m) echo "$FIXTURE_ARCH";; esac\n')
        self.tool('curl', '''#!/usr/bin/env python3
import os,sys,pathlib,shutil
args=sys.argv[1:]; url=args[-1]
if url.endswith('/releases/latest'):
 print('https://github.com/mizuamedesu/SynCodex/releases/tag/'+os.environ['FIXTURE_TAG'],end='')
else:
 source=pathlib.Path(os.environ['FIXTURE_ASSETS'])/url.rsplit('/',1)[-1]
 if not source.exists():sys.exit(22)
 shutil.copyfile(source,args[args.index('-o')+1])
''')
        for target in ['x86_64-unknown-linux-musl', 'aarch64-apple-darwin']:
            path = self.assets / f'syncodex-{TAG}-{target}.tar.gz'
            binary = os.environ.get('SYNCODEX_TEST_BINARY')
            payload = Path(binary).read_bytes() if binary else f'#!/bin/sh\nprintf "syncodex {VERSION}\\n"\n'.encode()
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

    def missing_nano(self, manager=None):
        (self.fake/'nano').unlink(missing_ok=True)
        for name in ['apt-get', 'apk', 'dnf', 'yum', 'pacman', 'brew']:
            (self.fake/name).unlink(missing_ok=True)
        if manager:
            self.tool(manager, '''#!/usr/bin/env python3
import json,os,pathlib,sys
name=pathlib.Path(sys.argv[0]).name
with open(os.environ['FIXTURE_PM_LOG'],'a') as log:
 log.write(json.dumps([name,*sys.argv[1:]])+'\\n')
if os.environ.get('FIXTURE_PM_FAIL')==' '.join([name,*sys.argv[1:]]):sys.exit(1)
if 'nano' in sys.argv[1:]:
 path=pathlib.Path(os.environ['FIXTURE_COMMANDS'])/'nano'
 path.write_text('#!/bin/sh\\nexit 0\\n');path.chmod(0o755)
''')

    def package_calls(self):
        path = self.root/'packages.jsonl'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

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
        self.install('--version',TAG,'--bin-dir',str(bindir))
        self.assertTrue((self.home/'codex skills/syncodex/SKILL.md').exists())
        self.assertTrue((self.home/'claude config/skills/syncodex/SKILL.md').exists())
        probe = subprocess.check_output(['sh','-c','. "$HOME/.profile"; command -v syncodex'],env=self.env,text=True)
        self.assertEqual(probe.strip(),str(bindir/'syncodex'))

    def test_bad_checksum_leaves_home_untouched(self):
        self.missing_nano('apt-get')
        for path in self.assets.glob('*.sha256'): path.write_text('0'*64+'  archive\n')
        self.install(success=False)
        self.assertEqual(list(self.home.iterdir()),[])
        self.assertEqual(self.package_calls(),[])

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

    def test_missing_nano_installs_with_available_package_manager(self):
        expected = {
            'apt-get': [['apt-get','update'],['apt-get','install','-y','nano']],
            'apk': [['apk','add','--no-cache','nano']],
            'dnf': [['dnf','install','-y','nano']],
            'yum': [['yum','install','-y','nano']],
            'pacman': [['pacman','-S','--needed','--noconfirm','nano']],
        }
        for manager, calls in expected.items():
            with self.subTest(manager=manager):
                self.missing_nano(manager)
                (self.root/'packages.jsonl').unlink(missing_ok=True)
                self.install()
                self.assertTrue(os.access(self.fake/'nano', os.X_OK))
                self.assertEqual(self.package_calls(),calls)
                # A repeated install must not install the editor again.
                self.install()
                self.assertEqual(self.package_calls(),calls)

    def test_missing_nano_uses_noninteractive_sudo(self):
        self.missing_nano('apt-get')
        self.env['FIXTURE_UID']='1000'
        self.tool('sudo', '''#!/usr/bin/env python3
import json,os,sys
with open(os.environ['FIXTURE_PM_LOG'],'a') as log:
 log.write(json.dumps(['sudo',*sys.argv[1:]])+'\\n')
assert sys.argv[1]=='-n'
if sys.argv[2]=='true':sys.exit(0)
os.execvp(sys.argv[2],sys.argv[2:])
''')
        self.install()
        self.assertEqual(self.package_calls(),[
            ['sudo','-n','true'], ['sudo','-n','apt-get','update'], ['apt-get','update'],
            ['sudo','-n','env','DEBIAN_FRONTEND=noninteractive','apt-get','install','-y','nano'],
            ['apt-get','install','-y','nano'],
        ])

    def test_missing_nano_requires_privilege_before_installing(self):
        self.missing_nano('apt-get')
        self.env['FIXTURE_UID']='1000'
        result=self.install(success=False)
        self.assertIn('Install nano as root or with sudo',result.stderr)
        self.assertEqual(list(self.home.iterdir()),[])
        self.assertEqual(self.package_calls(),[])

    def test_nano_package_failure_stops_install(self):
        self.missing_nano('apt-get')
        for command in ['apt-get update','apt-get install -y nano']:
            with self.subTest(command=command):
                self.env['FIXTURE_PM_FAIL']=command
                result=self.install(success=False)
                self.assertIn('Could not',result.stderr)
                self.assertEqual(list(self.home.iterdir()),[])

    def test_missing_nano_without_package_manager_can_opt_out(self):
        self.missing_nano()
        result=self.install(success=False)
        self.assertIn('--no-install-editor',result.stderr)
        self.assertEqual(list(self.home.iterdir()),[])
        self.install('--no-install-editor')
        self.assertTrue((self.home/'.local/bin/syncodex').exists())
        self.assertFalse((self.fake/'nano').exists())

    def test_mac_can_install_missing_nano_with_homebrew(self):
        self.env.update(FIXTURE_OS='Darwin',FIXTURE_ARCH='arm64')
        self.missing_nano('brew')
        self.install()
        self.assertEqual(self.package_calls(),[['brew','install','nano']])

if __name__ == '__main__':
    unittest.main()
