"""包里的 glibc 下限必须跟着构建基座走。

在更新的发行版上构建出的二进制会引用更高版本的 glibc 符号，装到更老的系统上
直接起不来——所以「基座的 glibc」和「包里声明的 libc6 下限」必须是同一个数。
2026-09-20 之前它在 nfpm 的两份 yaml 和 package.py 里各写了一遍 2.41，把支持
范围从 Ubuntu 24.04 抬到了 25.10。
"""

import json
from pathlib import Path
import unittest
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from package import nfpm_config

ROOT = Path(__file__).resolve().parents[3]
LOCK = json.loads((ROOT / 'packaging/common/toolchain.lock.json').read_text())


def manifest_with(glibc):
    return {'version': '0.6.0', 'package_revision': 2, 'fedora_version': 44,
            'builders': {'gnu-x86_64': {'glibc': glibc}}}


def depends(glibc, fmt, component):
    config = nfpm_config(manifest_with(glibc),
                         {'format': fmt, 'component': component},
                         Path('/stage'), [])
    return config['depends']


class GlibcFloorTests(unittest.TestCase):
    def test_deb_core_declares_the_builder_glibc(self):
        self.assertIn('libc6 (>= 2.39)', depends('2.39', 'deb', 'core'))

    def test_deb_voice_declares_the_builder_glibc(self):
        self.assertIn('libc6 (>= 2.39)', depends('2.39', 'deb', 'voice'))

    def test_rpm_uses_the_rpm_spelling(self):
        self.assertIn('glibc >= 2.39', depends('2.39', 'rpm', 'core'))
        self.assertIn('glibc >= 2.39', depends('2.39', 'rpm', 'voice'))

    def test_the_floor_follows_the_base_image_rather_than_a_literal(self):
        # 换基座只改锁文件，包里的声明必须自己跟上。
        self.assertIn('libc6 (>= 2.36)', depends('2.36', 'deb', 'core'))
        self.assertNotIn('libc6 (>= 2.39)', depends('2.36', 'deb', 'core'))

    def test_no_second_copy_of_the_floor_is_left_in_the_nfpm_files(self):
        for name in ('nfpm-deb.yaml', 'nfpm-fedora.yaml'):
            text = (ROOT / 'packaging/linux' / name).read_text()
            self.assertNotIn('libc6 (>=', text, name)
            self.assertNotIn('glibc >=', text, name)

    def test_the_shipped_lock_supports_ubuntu_2404(self):
        # Ubuntu 24.04 LTS 是 glibc 2.39（实测 24.04.5）。基座比它新就装不上。
        floor = LOCK['builders']['gnu-x86_64']['glibc']
        self.assertLessEqual([int(part) for part in floor.split('.')], [2, 39],
                             '构建基座的 glibc 高于 Ubuntu 24.04，DEB 装不上 24.04')
        self.assertIn('ubuntu2404-x86_64', LOCK['install_images'])

    def test_mint_is_pinned_to_a_release_built_on_the_same_glibc(self):
        # Mint 22.x = Ubuntu 24.04 基座。镜像固定在最新稳定版 22.3(zena)。
        self.assertIn('mint22.3-amd64@sha256:', LOCK['install_images']['mint22-x86_64'])


if __name__ == '__main__':
    unittest.main()
