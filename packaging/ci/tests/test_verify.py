import subprocess
import tempfile
from pathlib import Path
import unittest
import sys
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from unittest.mock import Mock, patch

from verify import cleanup_container, install_commands
from package import package_inventory


class CleanupTests(unittest.TestCase):
    def run_cleanup(self, removal, listing):
        box=Mock(root=Path('/retained-test-home'))
        with tempfile.TemporaryDirectory() as temp:
            with patch('verify.subprocess.run',side_effect=[removal,listing]):
                try:
                    cleanup_container('owned-name',box,Path(temp),'fixture-secret')
                finally:
                    self.box=box
                    self.log=(Path(temp)/'cleanup.json').read_text()

    def result(self,code=0,out='',err=''):
        return subprocess.CompletedProcess([],code,out,err)

    def test_transport_failure_retains_home_and_fails(self):
        with self.assertRaisesRegex(ValueError,'Retained test home'):
            self.run_cleanup(self.result(1,err='fixture-secret'),self.result(1))
        self.box.cleanup.assert_not_called()
        self.assertNotIn('fixture-secret',self.log)

    def test_live_container_retains_home_and_fails(self):
        with self.assertRaisesRegex(ValueError,'Retained test home'):
            self.run_cleanup(self.result(),self.result(out='owned-name\n'))
        self.box.cleanup.assert_not_called()

    def test_remove_failure_after_confirmed_absence_still_fails(self):
        with self.assertRaisesRegex(ValueError,'removal failed'):
            self.run_cleanup(self.result(1),self.result())
        self.box.cleanup.assert_called_once()

    def test_success_removes_home_after_absence_confirmed(self):
        self.run_cleanup(self.result(),self.result())
        self.box.cleanup.assert_called_once()


class InstallationRecipeTests(unittest.TestCase):
    def test_rpm_does_not_own_filesystem_roots(self):
        entries=[{'path':name,'type':'directory'} for name in
            ('bin','lib','share','share/licenses','lib/miyu','share/miyu')]
        entries.append({'path':'bin/miyu','type':'file'})
        self.assertEqual([entry['path'] for entry in package_inventory({'format':'rpm'},entries)],
                         ['lib/miyu','share/miyu','bin/miyu'])
        self.assertEqual(package_inventory({'format':'deb'},entries),entries)

    def test_ubuntu_uses_image_sources_without_guessing_archive_migration(self):
        commands=install_commands('ubuntu2510-x86_64',['/package/test.deb'])
        self.assertEqual(commands[0],['apt-get','update'])
        self.assertFalse(any('old-releases' in arg for command in commands for arg in command))
