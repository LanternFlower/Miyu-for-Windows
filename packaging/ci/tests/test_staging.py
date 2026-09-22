import copy
import json
from pathlib import Path
import tempfile
import unittest

from lib.common import load_json
from lib.manifest import COMMON
from lib.staging import selected_files, validate_assets, install_file, tree_manifest


class StagingTests(unittest.TestCase):
    def test_real_resource_inventory(self):
        catalog = validate_assets(load_json(COMMON/'assets.json'))
        source = COMMON.parents[1]
        for rule in catalog['assets']:
            if rule['source_root'] == 'source':
                with self.subTest(rule=rule['id']):
                    self.assertTrue(selected_files(rule, {'source': source}))

    def test_missing_font_license_or_model_fails(self):
        catalog = validate_assets(load_json(COMMON/'assets.json'))
        with tempfile.TemporaryDirectory() as tmp:
            for rule in catalog['assets']:
                if rule['source_root'] == 'source':
                    with self.subTest(rule=rule['id']), self.assertRaises(ValueError):
                        selected_files(rule, {'source': Path(tmp)})

    def test_missing_index_image_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root/'memes').mkdir()
            (root/'memes/index.json').write_text(json.dumps({'memes':[{'file':'missing.png'}]}))
            rule = {'id':'memes','source_root':'source','source':'memes','destination':'share/memes',
                    'type':'tree','include':['*.json','*.png'],'indexes':['index.json']}
            with self.assertRaisesRegex(ValueError, 'missing image'):
                selected_files(rule, {'source': root})

    def test_manifest_modes_links_and_duplicate_install(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root/'script'
            source.write_text('fixture')
            stage = root/'stage'
            install_file(source, stage/'bin/miyu', 0o755)
            (stage/'bin/miyupm').symlink_to('miyu')
            inventory = tree_manifest(stage)
            self.assertEqual(inventory, tree_manifest(stage))
            self.assertEqual(next(i for i in inventory if i['path']=='bin/miyu')['mode'], '0755')
            self.assertEqual(next(i for i in inventory if i['path']=='bin/miyupm')['target'], 'miyu')
            with self.assertRaises(ValueError):
                install_file(source, stage/'bin/miyu', 0o755)

    def test_catalog_duplicate_and_path_escape_rejected(self):
        catalog = load_json(COMMON/'assets.json')
        duplicate = copy.deepcopy(catalog)
        duplicate['assets'].append(duplicate['assets'][0])
        with self.assertRaises(ValueError):
            validate_assets(duplicate)
        catalog['assets'][0]['destination'] = '../outside'
        with self.assertRaises(ValueError):
            validate_assets(catalog)
