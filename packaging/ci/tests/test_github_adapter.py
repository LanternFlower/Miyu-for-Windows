import json
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from lib.github_release import GitHubRelease


class DraftLookupTests(unittest.TestCase):
    def response(self,code=0,value=None,stderr=''):
        return subprocess.CompletedProcess([],code,json.dumps(value),stderr)

    def test_draft_missing_from_tag_endpoint_is_found_in_authenticated_list(self):
        draft={'id':17,'tag_name':'v0.6.0','draft':True,'assets':[]}
        with patch.object(GitHubRelease,'gh',side_effect=[self.response(1,stderr='HTTP 404'),self.response(value=[[draft]])]):
            self.assertEqual(GitHubRelease('owner/repo').release('v0.6.0'),draft)

    def test_authentication_failure_is_not_treated_as_absence(self):
        with patch.object(GitHubRelease,'gh',return_value=self.response(1,stderr='HTTP 403')) as call:
            with self.assertRaises(RuntimeError):GitHubRelease('owner/repo').release('v0.6.0')
            self.assertEqual(call.call_count,1)

    def test_absent_release_requires_successful_complete_list(self):
        with patch.object(GitHubRelease,'gh',side_effect=[self.response(1,stderr='HTTP 404'),self.response(value=[[]])]) as call:
            self.assertIsNone(GitHubRelease('owner/repo').release('v0.6.0'))
            self.assertEqual(call.call_count,2)

    def test_ambiguous_drafts_are_rejected(self):
        draft={'tag_name':'v0.6.0','draft':True,'assets':[]}
        with patch.object(GitHubRelease,'gh',side_effect=[self.response(1,stderr='HTTP 404'),self.response(value=[[draft,draft]])]):
            with self.assertRaises(ValueError):GitHubRelease('owner/repo').release('v0.6.0')
