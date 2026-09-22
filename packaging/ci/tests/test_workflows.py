"""远端工作流与仓库之间的几处「必须对得上」。

这些都是实际绊过人的地方，不是假想：
  · `release.yml` 的 `notes-path` 默认值指向某一版的发布说明。版本号升了却忘了
    改它，远端一执行就去找一个不存在的文件——0.6.1 发版时就是这么发现 0.6.0
    那个默认值还留着的。
  · 两个 builder 镜像一行 `COPY` 都没有，构建上下文应当是空的。少了
    `.dockerignore`，`docker build … .` 会把整个仓库塞给守护进程；本机
    `target/` 实测 136 GB。
"""

import re
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[3]


def cargo_version():
    text = (ROOT / 'Cargo.toml').read_text(encoding='utf-8')
    return re.search(r'(?m)^version = "([^"]+)"', text).group(1)


class ReleaseNotesDefaultTests(unittest.TestCase):
    def setUp(self):
        self.version = cargo_version()
        self.workflow = (ROOT / '.github/workflows/release.yml').read_text(encoding='utf-8')

    def test_the_default_notes_path_tracks_the_cargo_version(self):
        found = re.findall(r'(?m)^\s*default: (docs/releases/\S+/release-notes\.md)\s*$',
                           self.workflow)
        self.assertEqual(len(found), 1, '发布说明默认值只应有一处')
        self.assertEqual(found[0], f'docs/releases/{self.version}/release-notes.md')

    def test_the_notes_and_the_archived_changelog_both_exist(self):
        for name in ('release-notes.md', 'changelog.md'):
            path = ROOT / 'docs/releases' / self.version / name
            self.assertTrue(path.is_file(), f'缺少 {path.relative_to(ROOT)}')
            self.assertGreater(len(path.read_text(encoding='utf-8').strip()), 0, name)


class BuildContextTests(unittest.TestCase):
    def test_the_builder_images_take_no_build_context(self):
        for name in ('Dockerfile.gnu', 'Dockerfile.arch'):
            text = (ROOT / 'packaging/linux/builders' / name).read_text(encoding='utf-8')
            for line in text.splitlines():
                self.assertFalse(line.strip().upper().startswith(('COPY ', 'ADD ')),
                                 f'{name} 开始依赖构建上下文了，.dockerignore 得跟着改')

    def test_dockerignore_excludes_everything(self):
        path = ROOT / '.dockerignore'
        self.assertTrue(path.is_file(), '缺少 .dockerignore：构建上下文会带上整个 target/')
        entries = [line.strip() for line in path.read_text(encoding='utf-8').splitlines()
                   if line.strip() and not line.startswith('#')]
        self.assertEqual(entries, ['*'])


if __name__ == '__main__':
    unittest.main()
