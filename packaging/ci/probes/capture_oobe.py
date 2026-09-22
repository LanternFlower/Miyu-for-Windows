#!/usr/bin/env python3
"""Capture real Miyu OOBE PTY frames and render their terminal cells as PNG."""
import argparse
from datetime import datetime, timezone
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import termios
import time
import tomllib

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from lib.common import BlockedError, fresh_directory, sha256_file, write_json
from lib.isolation import Sandbox
from lib.reaper import OwnedReaper

REPO = Path(__file__).resolve().parents[3]
FRAME_END = b'\x1b[?2026l'
ENTER, DOWN = b'\r', b'\x1b[B'
BACKGROUND = '#14151c'
FOREGROUND = '#d6d2df'


class Terminal:
    def __init__(self, command, environment, sandbox, cols, rows, raw_path):
        import pyte
        self.cols, self.rows = cols, rows
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.ByteStream(self.screen)
        self.pending = b''
        self.frames = 0
        self.bytes = 0
        self.exited = False
        self.raw = raw_path.open('wb')
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            try:
                os.chdir(sandbox.root/'work')
                fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
                os.execve(command[0], command, environment)
            except BaseException:
                os._exit(127)

    def lines(self):
        # pyte.display can fail for a wide glyph's empty continuation cell.
        return [''.join(self.screen.buffer[y][x].data for x in range(self.cols)).rstrip()
                for y in range(self.rows)]

    def read(self, timeout=0.1):
        ready, _, _ = select.select([self.fd], [], [], timeout)
        if not ready:
            return
        try:
            data = os.read(self.fd, 65536)
        except OSError as error:
            if error.errno != errno.EIO:
                raise
            data = b''
        if not data:
            self.exited = True
            return
        self.raw.write(data)
        self.raw.flush()
        self.bytes += len(data)
        self.pending += data
        # Feed complete synchronized frames only. A capture cannot contain half
        # of ratatui's erase/redraw transaction.
        while FRAME_END in self.pending:
            frame, self.pending = self.pending.split(FRAME_END, 1)
            frame += FRAME_END
            self.stream.feed(frame)
            self.frames += 1
            self._cursor_reports(frame)
        # Before the first full-screen frame, initialization may query the cursor.
        if self.frames == 0 and b'\x1b[6n' in self.pending:
            self.stream.feed(self.pending)
            self._cursor_reports(self.pending)
            self.pending = b''

    def _cursor_reports(self, data):
        for _ in range(data.count(b'\x1b[6n')):
            self.send(f'\x1b[{self.screen.cursor.y + 1};{self.screen.cursor.x + 1}R'.encode())

    def send(self, keys):
        os.write(self.fd, keys)

    def wait(self, predicate, description, timeout=25, settled_frames=24):
        deadline = time.monotonic() + timeout
        ready_at = None
        while time.monotonic() < deadline:
            self.read()
            if self.exited:
                raise RuntimeError(f'OOBE exited before {description}.')
            if predicate(self.lines()):
                if ready_at is None:
                    ready_at = self.frames
                if self.frames - ready_at >= settled_frames:
                    return
            else:
                ready_at = None
        raise RuntimeError(f'OOBE readiness timed out: {description}. Current screen:\n'+ '\n'.join(self.lines()))

    def page(self, *phrases):
        self.wait(lambda lines: all(any(word in line for line in lines) for word in phrases),
                  ' / '.join(phrases))

    def close(self):
        try:
            if not self.exited:
                self.send(b'\x03')
                deadline = time.monotonic()+2
                while not self.exited and time.monotonic() < deadline:
                    self.read()
        finally:
            # This unreaped direct child pins its PID and owns the PTY session.
            try:
                os.killpg(self.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(self.pid, 0)
            os.close(self.fd)
            self.raw.close()


def colors(value, default):
    basic = {'black':'000000', 'red':'aa0000', 'green':'00aa00', 'brown':'aa5500',
             'blue':'0000aa', 'magenta':'aa00aa', 'cyan':'00aaaa', 'white':'aaaaaa',
             'brightblack':'555555', 'brightred':'ff5555', 'brightgreen':'55ff55',
             'brightbrown':'ffff55', 'brightblue':'5555ff', 'brightmagenta':'ff55ff',
             'brightcyan':'55ffff', 'brightwhite':'ffffff'}
    if value == 'default':
        return default
    value = basic.get(value, value)
    if re.fullmatch(r'[0-9a-fA-F]{6}', value):
        return '#'+value
    raise ValueError(f'Unsupported terminal color: {value}')


class Renderer:
    def __init__(self, symbols):
        from PIL import ImageFont
        from fontTools.ttLib import TTFont
        self.font_paths = [REPO/'assets/fonts/JetBrainsMono-Regular.ttf',
                           REPO/'assets/fonts/NotoSansCJK-Regular.ttc', symbols]
        self.fonts = [ImageFont.truetype(str(path), 26, index=0) for path in self.font_paths]
        self.maps = []
        for path in self.font_paths:
            with TTFont(path, fontNumber=0) as font:
                self.maps.append(font.getBestCmap())
        self.cell_width, self.cell_height = 16, 34

    def font_for(self, text):
        for font, cmap in zip(self.fonts, self.maps):
            if all(ord(char) in cmap or char.isspace() for char in text):
                return font
        raise ValueError(f'No captured-terminal font contains glyph: {text!r}')

    def draw(self, terminal, title, path):
        from PIL import Image, ImageDraw
        margin, header, inset = 28, 48, 22
        width = terminal.cols*self.cell_width+2*(margin+inset)
        height = terminal.rows*self.cell_height+2*margin+header+2*inset
        image = Image.new('RGB', (width, height), '#0d0e13')
        draw = ImageDraw.Draw(image)
        draw.rounded_rectangle((margin, margin, width-margin, height-margin), radius=16,
                               fill=BACKGROUND, outline='#30323e', width=1)
        draw.line((margin, margin+header, width-margin, margin+header), fill='#30323e')
        for offset, color in [(0,'#e38c9a'),(22,'#e4bf79'),(44,'#9ccfa0')]:
            draw.ellipse((margin+20+offset, margin+19, margin+30+offset, margin+29), fill=color)
        label = f'Miyu 0.6.0  ·  {title}'
        draw.text((width/2, margin+header/2), label, font=self.fonts[1], fill='#aebde8', anchor='mm')
        origin_x, origin_y = margin+inset, margin+header+inset
        # Paint every cell background first. A wide glyph's continuation cell
        # must not erase its right half after the glyph has been drawn.
        for y in range(terminal.rows):
            for x in range(terminal.cols):
                cell = terminal.screen.buffer[y][x]
                bg = colors(cell.fg, FOREGROUND) if cell.reverse else colors(cell.bg, BACKGROUND)
                if bg != BACKGROUND:
                    left, top = origin_x+x*self.cell_width, origin_y+y*self.cell_height
                    draw.rectangle((left, top, left+self.cell_width-1, top+self.cell_height-1), fill=bg)
        for y in range(terminal.rows):
            for x in range(terminal.cols):
                cell = terminal.screen.buffer[y][x]
                fg, bg = colors(cell.fg, FOREGROUND), colors(cell.bg, BACKGROUND)
                if cell.reverse:
                    fg, bg = bg, fg
                left, top = origin_x+x*self.cell_width, origin_y+y*self.cell_height
                if not cell.data or cell.data == ' ':
                    continue
                span = 2 if x+1 < terminal.cols and terminal.screen.buffer[y][x+1].data == '' else 1
                if cell.data == '█':
                    draw.rectangle((left, top, left+self.cell_width-1, top+self.cell_height-1), fill=fg)
                else:
                    draw.text((left+span*self.cell_width/2, top+self.cell_height/2), cell.data,
                              font=self.font_for(cell.data), fill=fg, anchor='mm',
                              stroke_width=1 if cell.bold else 0, stroke_fill=fg)
                if cell.underscore:
                    draw.line((left, top+self.cell_height-3, left+span*self.cell_width, top+self.cell_height-3), fill=fg)
        image.save(path, optimize=True)
        return {'width': width, 'height': height}


def capture(binary, out, evidence, *, cols, rows, symbols):
    import PIL
    import importlib.metadata
    binary = Path(binary)
    if not binary.is_absolute() or not binary.is_file():
        raise ValueError('--binary must name an existing absolute executable path.')
    binary = binary.resolve()
    unshare = shutil.which('unshare')
    if not unshare:
        raise BlockedError('Linux unshare is required for offline OOBE capture.')
    check = subprocess.run([unshare, '--user', '--map-root-user', '--net', 'true'],
                           capture_output=True, timeout=5)
    if check.returncode:
        raise BlockedError('An isolated network namespace is unavailable.')
    out, evidence = fresh_directory(out), fresh_directory(evidence)
    renderer = Renderer(symbols)
    provenance = {'schema_version': 1, 'captured_at_utc': datetime.now(timezone.utc).isoformat(),
                  'binary_sha256': sha256_file(binary), 'binary_name': binary.name,
                  'terminal': {'columns': cols, 'rows': rows, 'term': 'xterm-256color', 'color': 'truecolor'},
                  'renderer': {'pillow': PIL.__version__, 'pyte': importlib.metadata.version('pyte'),
                               'fonts': [{'name':p.name, 'sha256':sha256_file(p)} for p in renderer.font_paths]},
                  'network': 'Isolated Linux user/network namespace. No external or host-loopback connection.',
                  'screens': []}
    with Sandbox() as sandbox:
        (sandbox.root/'miyu/config/config.jsonc').unlink()
        env = sandbox.environment({'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'})
        env.update(TERM='xterm-256color', COLORTERM='truecolor', MIYU_COLOR='truecolor',
                   MIYU_OOBE_NO_IME='1', SHELL='/bin/bash')
        version = subprocess.run([str(binary), '--version'], env=env, cwd=sandbox.root/'work',
                                 check=True, capture_output=True, text=True, timeout=10).stdout.strip()
        # 版本从 Cargo.toml 读，不写死：0.6.1 发版时发现这儿还钉着 0.6.0，
        # 任何后续版本的截图都会被它拦下来。
        expected = tomllib.loads((REPO/'Cargo.toml').read_text())['package']['version']
        if not re.search(rf"\bmiyu {re.escape(expected)}\b", version, re.IGNORECASE):
            raise ValueError(f'Expected a real {expected} binary, received: {version}')
        provenance['version'] = version
        reaper = OwnedReaper()
        terminal = None
        try:
            terminal = Terminal([unshare, '--user', '--map-root-user', '--net', str(binary), 'oobe'],
                                env, sandbox, cols, rows, evidence/'session.ansi')
            def shot(id, title):
                lines = terminal.lines()
                text = '\n'.join(lines)+'\n'
                if str(sandbox.root) in text or '/home/' in text or 'local-test-only' in text:
                    raise ValueError('Private sandbox/configuration data appeared in a public frame.')
                (evidence/(id+'.txt')).write_text(text)
                dimensions = renderer.draw(terminal, title, out/(id+'.png'))
                provenance['screens'].append({'id': id, 'title': title, 'png': id+'.png',
                    'sha256': sha256_file(out/(id+'.png')), 'text_sha256': sha256_file(evidence/(id+'.txt')),
                    'terminal_frame': terminal.frames, 'raw_byte_offset': terminal.bytes, **dimensions})
            terminal.page('回车进入设置引导')
            shot('01-welcome', '欢迎')
            terminal.send(ENTER)
            terminal.page('用内置的 Miyu', '自己捏一个')
            shot('02-persona', '选择人格')
            terminal.send(ENTER)
            terminal.page('自选功能', '内置功能')
            shot('03-features', '自选功能')
            terminal.send(ENTER)
            terminal.page('怎么认识你', '继续')
            terminal.send(DOWN)
            terminal.wait(lambda lines: any('继续' in line and ('●' in line or '›' in line or '▸' in line or '❯' in line) for line in lines),
                          'identity Continue focus', settled_frames=2)
            terminal.send(ENTER)
            terminal.page('终端集成', '不装')
            for _ in range(8):
                if any('不装' in line and '●' in line for line in terminal.lines()):
                    break
                previous = terminal.frames
                terminal.send(DOWN)
                terminal.wait(lambda _: terminal.frames > previous, 'shell selection update', settled_frames=1)
            else:
                raise RuntimeError('Could not select shell integration skip.')
            terminal.send(ENTER)
            terminal.page('接模型', 'opencode Zen', 'OpenCode Go')
            shot('04-provider', '接模型')
        finally:
            try:
                if terminal is not None:
                    terminal.close()
            finally:
                try:
                    provenance['reaped_descendants'] = reaper.reap(str(sandbox.root/'miyu'))
                finally:
                    reaper.close()
    if sha256_file(binary) != provenance['binary_sha256']:
        raise ValueError('The binary changed while screenshots were being captured.')
    provenance['cleanup'] = 'Owned PTY processes and temporary HOME/MIYU_HOME/XDG roots removed.'
    provenance['raw_ansi_sha256'] = sha256_file(evidence/'session.ansi')
    write_json(out/'capture-provenance.json', provenance)
    write_json(evidence/'capture-provenance.json', provenance)
    return provenance


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--evidence', type=Path, required=True)
    parser.add_argument('--columns', type=int, default=108)
    parser.add_argument('--rows', type=int, default=38)
    parser.add_argument('--symbol-font', type=Path, default=Path('/usr/share/fonts/TTF/DejaVuSans.ttf'))
    args = parser.parse_args()
    if not 90 <= args.columns <= 160 or not 34 <= args.rows <= 60:
        parser.error('Use 90–160 columns and 34–60 rows.')
    try:
        result = capture(args.binary, args.out, args.evidence, cols=args.columns, rows=args.rows, symbols=args.symbol_font)
        print(f'Captured {len(result["screens"])} real OOBE screens from {result["version"]}.')
        return 0
    except (BlockedError, ImportError) as error:
        print(f'BLOCKED: {error}', file=sys.stderr)
        return 3
    except (ValueError, RuntimeError, OSError, subprocess.SubprocessError) as error:
        print(f'ERROR: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
