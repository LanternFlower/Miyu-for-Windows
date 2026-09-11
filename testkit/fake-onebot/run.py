#!/usr/bin/env python3
"""最小 OneBot(NapCat) 假客户端,用来真实驱动 Miyu 的 QQ 群聊回合。

为什么要它:群聊那条路(`qq_turn_system_context` / `active_target_prompt`)
只有 OneBot 连接能走通,REPL 和单元测试都碰不到。而真群里有几十号人,不能
拿他们试。

隔离:用一整套假 id——假账号(x-self-id)、假群、假发送者。群历史/水位/好感度
的 key 都含 account_id(runtime.rs:255),所以与真群彻底分开;回复只回到本连接,
不进真 QQ。非白名单群不会被拒,只是限流档位不同(admission.rs:226)。

不依赖第三方库:手写 RFC6455 握手与分帧。

用法: python3 run.py            # 跑全部场景
      python3 run.py --keep 60  # 收完再多等 60 秒
"""
import base64, json, os, re, socket, struct, sys, threading, time

HOST, PORT, PATH = "127.0.0.1", 8300, "/ws"
SELF_ID   = 900000001          # 假机器人账号
GROUP_ID  = 999000001          # 假群(非白名单)
SENDER    = 800000043          # 假群友(非管理员)
OTHER     = 800000002          # 另一个假群友


def access_token() -> str:
    raw = open(os.path.expanduser("~/.miyu/config/config.jsonc"), encoding="utf-8").read()
    raw = re.sub(r"^\s*//.*", "", raw, flags=re.M)
    return json.loads(raw)["platforms"]["qq"].get("access_token", "") or ""


class WS:
    def __init__(self, sock):
        self.sock, self.buf = sock, b""

    @classmethod
    def connect(cls, token: str):
        sock = socket.create_connection((HOST, PORT), timeout=10)
        key = base64.b64encode(os.urandom(16)).decode()
        req = (
            f"GET {PATH} HTTP/1.1\r\nHost: {HOST}:{PORT}\r\n"
            "Upgrade: websocket\r\nConnection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n"
            f"X-Self-ID: {SELF_ID}\r\n"
            + (f"Authorization: Bearer {token}\r\n" if token else "")
            + "\r\n"
        )
        sock.sendall(req.encode())
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = sock.recv(4096)
            if not chunk:
                raise RuntimeError("握手时连接被关闭")
            head += chunk
        status = head.split(b"\r\n", 1)[0].decode(errors="replace")
        if "101" not in status:
            raise RuntimeError(f"握手失败: {status}")
        # 握手用的 10s 超时不能留给后续 recv:空闲十秒就会抛 socket.timeout
        # 把收帧线程打死,表现成"所有 API 都超时"(第一版就栽在这)。
        sock.settimeout(None)
        ws = cls(sock)
        ws.buf = head.split(b"\r\n\r\n", 1)[1]
        return ws

    def send(self, obj):
        payload = json.dumps(obj, ensure_ascii=False).encode()
        header, n = b"\x81", len(payload)
        if n < 126:
            header += struct.pack("!B", 0x80 | n)
        elif n < 1 << 16:
            header += struct.pack("!BH", 0x80 | 126, n)
        else:
            header += struct.pack("!BQ", 0x80 | 127, n)
        mask = os.urandom(4)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(header + mask + masked)

    def _fill(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("连接已关闭")
            self.buf += chunk

    def recv(self):
        self._fill(2)
        b1, b2 = self.buf[0], self.buf[1]
        length, offset = b2 & 0x7F, 2
        if length == 126:
            self._fill(4); length = struct.unpack("!H", self.buf[2:4])[0]; offset = 4
        elif length == 127:
            self._fill(10); length = struct.unpack("!Q", self.buf[2:10])[0]; offset = 10
        self._fill(offset + length)
        payload, self.buf = self.buf[offset:offset + length], self.buf[offset + length:]
        opcode = b1 & 0x0F
        if opcode == 8:
            raise ConnectionError("对端关闭")
        if opcode == 9:                      # ping → pong
            return None
        return json.loads(payload.decode(errors="replace")) if payload else None


REPLIES = []
# 发出去的消息留底,get_msg 要按 id 还原(含图片段)。
SENT_MESSAGES = {}


def api_data(action, params):
    """按 action 给出合理的返回体。返回 None 表示"不支持",让 Miyu 走降级。"""
    if action in ("send_group_msg", "send_msg", "send_private_msg"):
        return {"message_id": int(time.time() * 1000) % 2**31}
    if action == "get_group_info":
        return {"group_id": GROUP_ID, "group_name": "假群(测具)",
                "member_count": 3, "max_member_count": 200}
    if action == "get_login_info":
        return {"user_id": SELF_ID, "nickname": "Miyu"}
    if action == "get_group_member_info":
        uid = int(params.get("user_id") or SENDER)
        return {"group_id": GROUP_ID, "user_id": uid, "nickname": "测试群友",
                "card": "", "role": "member"}
    if action == "get_group_member_list":
        return [{"group_id": GROUP_ID, "user_id": u, "nickname": n, "role": "member"}
                for u, n in ((SENDER, "测试群友"), (OTHER, "另一个群友"))]
    if action == "get_msg":
        # 按消息 id 把原样内容还回去。vision_analyze 取历史图就走这条
        # (adapter.rs:80 message_images → get_msg → 下载图片段),返回空数组
        # 等于"这条消息没有图",第一版就是这么把整条链路测哑的。
        mid = params.get("message_id")
        try:
            mid = int(mid)
        except (TypeError, ValueError):
            pass
        sent = SENT_MESSAGES.get(mid)
        if not sent:
            return {"message_id": mid, "message": [],
                    "sender": {"user_id": SENDER, "nickname": "测试私聊"}}
        return {
            "message_id": mid,
            "message_type": sent["message_type"],
            "real_id": mid,
            "time": sent["time"],
            "user_id": sent["user_id"],
            "group_id": sent.get("group_id"),
            "target_id": sent.get("target_id"),
            "message": sent["message"],
            "raw_message": sent.get("raw_message", ""),
            "sender": {"user_id": sent["user_id"], "nickname": sent["nickname"]},
        }
    return {}


def pump(ws):
    """后台收:Miyu 发过来的都是 API 调用,逐一应答。异常必须可见——第一版
    悄悄死掉,表现成"所有 API 都超时",查了半天。"""
    import traceback
    while True:
        try:
            frame = ws.recv()
        except (ConnectionError, OSError) as error:
            print(f"\n  [pump] 连接结束: {error}")
            return
        except Exception:
            traceback.print_exc()
            return
        try:
            if not isinstance(frame, dict) or "action" not in frame:
                continue
            action, params = frame["action"], frame.get("params", {})
            if action in ("send_group_msg", "send_msg"):
                REPLIES.append(params.get("message"))
                print(f"\n  ← Miyu 回复: {render(params.get('message'))}")
            else:
                print(f"  [api] {action}")
            ws.send({"status": "ok", "retcode": 0,
                     "data": api_data(action, params),
                     "echo": frame.get("echo")})
        except Exception:
            traceback.print_exc()
            return


def render(message):
    if isinstance(message, str):
        return message
    parts = []
    for seg in message or []:
        t = seg.get("type")
        if t == "text":
            parts.append(seg["data"].get("text", ""))
        elif t == "at":
            parts.append(f"@{seg['data'].get('qq')}")
        elif t == "reply":
            parts.append(f"[引用{seg['data'].get('id')}]")
        else:
            parts.append(f"[{t}]")
    return "".join(parts).strip()


def private_msg(ws, text, *, image_b64=None, sender=SENDER, name="测试私聊"):
    """私聊消息。image_b64 非空时附一段 base64 图片段(OneBot 的 file 支持
    base64:// 前缀,见 inbound.rs:318)。"""
    segments = []
    if text:
        segments.append({"type": "text", "data": {"text": text}})
    if image_b64:
        segments.append({"type": "image", "data": {"file": f"base64://{image_b64}"}})
    mid = int(time.time() * 1000) % 2**31
    now = int(time.time())
    SENT_MESSAGES[mid] = {"message_type": "private", "user_id": sender,
                          "target_id": SELF_ID, "message": segments,
                          "raw_message": text, "nickname": name, "time": now}
    ws.send({
        "post_type": "message", "message_type": "private", "sub_type": "friend",
        "self_id": SELF_ID, "user_id": sender, "message_id": mid,
        "raw_message": text, "message": segments, "font": 0, "time": now,
        "sender": {"user_id": sender, "nickname": name},
    })
    return mid


def make_png(width=240, height=160, rgb=(220, 40, 60), band=(30, 90, 200)):
    """现造一张纯 PNG(zlib+struct,不依赖 Pillow)。

    上半纯色、下半另一色,右下角画个方块——追问"右下角是什么颜色"时有确定
    答案,能区分"真看到了"和"编的"。1×1 的图不行:模型看不清也会随口给个
    颜色,测出来的是幻觉不是能力。
    """
    import struct, zlib

    rows = []
    for y in range(height):
        row = bytearray([0])  # filter type 0
        for x in range(width):
            if y > height * 0.6 and x > width * 0.7:
                color = (250, 230, 40)          # 右下角方块:黄
            elif y > height * 0.5:
                color = band
            else:
                color = rgb
            row.extend(color)
        rows.append(bytes(row))
    raw = b"".join(rows)

    def chunk(tag, payload):
        body = tag + payload
        return struct.pack(">I", len(payload)) + body + struct.pack(">I", zlib.crc32(body))

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 6))
    png += chunk(b"IEND", b"")
    return base64.b64encode(png).decode()


TINY_PNG = make_png()


def private_image_probe(ws, keep: float):
    """私聊的图片跨轮引用:先发图并等她答,再不带图追问同一张。"""
    print("\n=== 私聊:发图 ===")
    before = len(REPLIES)
    private_msg(ws, "看看这张图上半部分是什么颜色", image_b64=TINY_PNG)
    waited = 0.0
    while waited < keep and len(REPLIES) == before:
        time.sleep(0.5); waited += 0.5
    time.sleep(3)

    print("\n=== 私聊:不带图追问上一张 ===")
    before = len(REPLIES)
    private_msg(ws, "那张图右下角那个方块是什么颜色")
    waited = 0.0
    while waited < keep and len(REPLIES) == before:
        time.sleep(0.5); waited += 0.5


def group_msg(ws, text, *, sender=SENDER, at_self=False, name="测试群友"):
    segments = []
    if at_self:
        segments.append({"type": "at", "data": {"qq": str(SELF_ID)}})
        segments.append({"type": "text", "data": {"text": " "}})
    segments.append({"type": "text", "data": {"text": text}})
    mid = int(time.time() * 1000) % 2**31
    ws.send({
        "post_type": "message", "message_type": "group", "sub_type": "normal",
        "self_id": SELF_ID, "group_id": GROUP_ID, "user_id": sender,
        "message_id": mid, "raw_message": text, "message": segments,
        "font": 0, "time": int(time.time()),
        "sender": {"user_id": sender, "nickname": name, "role": "member"},
    })
    return mid


SCENARIOS = [
    ("被 @ 叫醒（验 [you] 标记）", dict(text="你好，帮我看看这个", at_self=True)),
    ("唤醒词开头（验不再剥离）",   dict(text="为什么不查知识库")),
    ("群友之间闲聊（不该被叫醒）", dict(text="今天天气不错啊")),
]


# 房间里抛出的问题:不点名任何人,正是"概率抽话该不该接"的典型场景。
ROOM_TALK = [
    ("arch 装 nvidia 驱动老是黑屏，有人遇到过吗", 800000001),
    ("niri 的 waybar 配置有推荐的吗", 800000002),
    ("hyprland 0.55 配置真改成 lua 了？", 800000003),
    ("今天群里好安静啊", 800000001),
    ("有人用过 cachyos 吗，值得换吗", 800000002),
    ("我这 pacman 更新完开不了机了", 800000003),
    ("btrfs 和 ext4 你们选哪个", 800000001),
    ("wayland 下截图工具用啥好", 800000002),
    ("我的 fcitx5 又不工作了", 800000003),
    ("这周末干啥好呢", 800000001),
    ("kde 和 gnome 到底哪个省内存", 800000002),
    ("刚装完系统，接下来该干嘛", 800000003),
]


def probability_probe(ws, rounds: int, gap: float):
    """反复往假群里扔"房间问题",等概率抽样自己撞上。

    不触发的消息零成本(压根不调模型),所以可以多发。轮换发送者是为了避开
    续聊窗口——同一个人连着发会走 continuation 而不是 probability。
    """
    print(f"\n=== 概率探针:{rounds} 轮 ===")
    for index in range(rounds):
        text, sender = ROOM_TALK[index % len(ROOM_TALK)]
        before = len(REPLIES)
        print(f"\n[{index + 1}/{rounds}] ({sender}) {text}")
        group_msg(ws, text, sender=sender, name=f"群友{sender % 10}")
        waited = 0.0
        while waited < gap:
            time.sleep(0.5)
            waited += 0.5
            if len(REPLIES) > before:
                # 回复了就多等一会,让好感度/水位那些收尾跑完
                time.sleep(2)
                break
    print(f"\n探针结束,共 {len(REPLIES)} 条回复")


def main():
    keep = 75
    if "--keep" in sys.argv:
        keep = int(sys.argv[sys.argv.index("--keep") + 1])
    ws = WS.connect(access_token())
    print(f"已连接 Miyu (self_id={SELF_ID}, group={GROUP_ID})")
    threading.Thread(target=pump, args=(ws,), daemon=True).start()
    ws.send({"post_type": "meta_event", "meta_event_type": "lifecycle",
             "sub_type": "connect", "self_id": SELF_ID, "time": int(time.time())})
    time.sleep(1)
    if "--private" in sys.argv:
        private_image_probe(ws, keep=90)
        return
    if "--probe" in sys.argv:
        idx = sys.argv.index("--probe")
        rounds = int(sys.argv[idx + 1]) if len(sys.argv) > idx + 1 else 30
        probability_probe(ws, rounds, gap=float(os.environ.get("PROBE_GAP", "6")))
        return
    for title, kwargs in SCENARIOS:
        print(f"\n=== {title} ===\n  → {kwargs['text']}")
        before = len(REPLIES)
        group_msg(ws, **kwargs)
        for _ in range(keep * 2):
            time.sleep(0.5)
            if len(REPLIES) > before:
                break
        else:
            print("  ← （没有回复）")
        time.sleep(1.5)
    print(f"\n收到 {len(REPLIES)} 条回复")


if __name__ == "__main__":
    main()
