#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
bse.py — CÔNG CỤ ALL-IN-ONE (AIO) CHO DỰ ÁN BSE-AUTOROUTER (Rust -> WASM -> main.html)

Chỉ dùng thư viện chuẩn của Python 3 (0 phụ thuộc ngoài, không cần pip install).
Bạn chỉ cần làm việc trên 2 file:
    1. RUST_PROJECT_BUNDLE.txt   (dán nối tiếp các khối "=== FILE: ... ===" khi cập nhật thuật toán)
    2. main.html                 (khi chỉnh sửa giao diện)

Cách chạy:
    • Trên Windows: Bấm đúp vào `bse.bat` hoặc gõ `bse` trong Terminal
    • Đa nền tảng : Gõ `python bse.py`

Bảng điều khiển tương tác (gõ lệnh rồi nhấn Enter để xác nhận):
    open / o       Mở main.html                 copy / c       Copy báo cáo lỗi/cảnh báo
    ocd / l        Chuẩn hóa OCD                 pack / p       Gom gọn bundle
    backup / b     Tạo/khôi phục/xuất/nhập backup clean / x      Dọn rác an toàn
    run / r        Chạy lại                     force / f      Ép test/build bỏ qua cache
    build-warn / w Tiếp tục build khi Cargo chỉ còn cảnh báo (nếu khả dụng)
    quit / q       Thoát                        help / h       Xem hướng dẫn

Các lệnh CLI trực tiếp (nếu muốn gọi từ dòng lệnh):
    python bse.py                 Chạy toàn bộ quy trình tự động + Bảng điều khiển AIO + Live Watcher
    python bse.py ocd             Làm sạch chuẩn OCD cho cả mã Rust (builder.rs) và main.html
    python bse.py pack            Chỉ gom gọn lại RUST_PROJECT_BUNDLE.txt từ mã nguồn trên đĩa
    python bse.py build           Chỉ biên dịch WASM + tạo wasm-bridge.js + kiểm tra
    python bse.py check           Kiểm tra nhanh độ khớp main.html <-> Rust <-> Bridge + Kiểm toán OCD
    python bse.py backup          Mở Trung tâm Sao lưu / Khôi phục / Xuất / Nhập
    python bse.py clean           Mở Trình dọn rác & giải phóng dung lượng ổ cứng
"""
from __future__ import annotations

import argparse
import base64
import difflib
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import stat
import unicodedata
import time
import zipfile
from collections import deque
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Callable, Dict, List, Optional, Tuple

# ════════════════════════════════════════════════════════════════════════════
#  CẤU HÌNH ĐƯỜNG DẪN & HẰNG SỐ HỆ THỐNG
# ════════════════════════════════════════════════════════════════════════════
ROOT = Path(__file__).resolve().parent
BUNDLE = ROOT / "RUST_PROJECT_BUNDLE.txt"
HTML = ROOT / "main.html"
BRIDGE = ROOT / "wasm-bridge.js"
PKG = ROOT / "pkg"
TARGET_DIR = ROOT / "target"
WASM_CRATE = ROOT / "crates" / "bse-wasm"
WORK = ROOT / "_bse"
BACKUPS = WORK / "backups"
LEGACY_ARCHIVE = WORK / "legacy_scripts"
STATE_FILE = WORK / "state.json"
REPORT_FILE = WORK / "last_report.txt"

MARKER = "=== FILE: "
BUNDLE_HEADER = "# BẢN GOM TOÀN BỘ MÃ NGUỒN RUST (DÙNG ĐỂ UPLOAD CHO AI)\n\n"
SRC_EXT = (".rs", ".toml")
SKIP_DIRS = {"target", ".git", "node_modules", "__pycache__", "pkg", "_bse"}
LEGACY_FILES = ("sync_bundle.py", "fix_and_pack.py", "wasm-worker.js")
KEEP_AUTO_BACKUPS = 20  # Số bản sao lưu tự động giữ lại (các bản thủ công/ghim luôn được giữ)
BACKUP_PAGE_SIZE = 5    # Số bản backup hiển thị mỗi trang (mỗi bản dùng 2 dòng)
MAX_SHOW = 8            # Số dòng lỗi/cảnh báo hiển thị trực tiếp trên màn hình chính
WASM_ARGS = [
    "build", "crates/bse-wasm",
    "--target", "no-modules",
    "--release",
    "--no-typescript",
    "--out-dir", "../../pkg",
]

# ════════════════════════════════════════════════════════════════════════════
#  GIAO DIỆN CONSOLE CHUẨN UTF-8 & ĐỌC PHÍM TỨC THÌ
# ════════════════════════════════════════════════════════════════════════════
if os.name == "nt":
    os.system("")  # Bật chế độ xử lý mã màu ANSI/VT100 trên Windows
    try:
        import ctypes
        ctypes.windll.kernel32.SetConsoleCP(65001)
        ctypes.windll.kernel32.SetConsoleOutputCP(65001)
    except Exception:
        pass

for _st in (sys.stdout, sys.stderr):
    try:
        _st.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass

COLOR = sys.stdout.isatty() and "NO_COLOR" not in os.environ


def _c(code: str, s: str) -> str:
    return f"\x1b[{code}m{s}\x1b[0m" if COLOR else s


def green(s: str) -> str: return _c("32", s)
def red(s: str) -> str: return _c("31", s)
def yellow(s: str) -> str: return _c("33", s)
def cyan(s: str) -> str: return _c("36", s)
def magenta(s: str) -> str: return _c("35", s)
def dim(s: str) -> str: return _c("2", s)
def bold(s: str) -> str: return _c("1", s)


OK, BAD, WARN, SKIP, INFO = green("✔"), red("✘"), yellow("⚠"), dim("↷"), cyan("•")


def term_width() -> int:
    """Chiều rộng thực tế của terminal, giới hạn để log không thành một dòng quá dài."""
    try:
        columns = shutil.get_terminal_size((96, 24)).columns
    except OSError:
        columns = 96
    return max(20, min(112, columns - 1))


_ANSI_RE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


def _char_width(ch: str) -> int:
    if not ch or unicodedata.combining(ch):
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


def visible_width(value: str) -> int:
    plain = _ANSI_RE.sub("", str(value))
    return sum(_char_width(ch) for ch in plain)


def clip(s: str, n: Optional[int] = None) -> str:
    """Cắt theo độ rộng hiển thị (không tính ANSI màu, tính ký tự CJK rộng 2 ô)."""
    limit = max(4, int(n or term_width()))
    s = str(s).replace("\r", " ").replace("\n", " ")
    if visible_width(s) <= limit:
        return s
    out = []
    width = 0
    i = 0
    while i < len(s):
        match = _ANSI_RE.match(s, i)
        if match:
            out.append(match.group(0))
            i = match.end()
            continue
        ch = s[i]
        cw = _char_width(ch)
        if width + cw > limit - 1:
            break
        out.append(ch)
        width += cw
        i += 1
    out.append("…")
    if "\x1b[" in s:
        out.append("\x1b[0m")
    return "".join(out)


def _pad_visible(value: str, width: int) -> str:
    fitted = clip(value, width)
    return fitted + (" " * max(0, width - visible_width(fitted)))


def draw_panel(title: str, lines: List[str], width: Optional[int] = None) -> None:
    """Vẽ khung co giãn theo terminal, tránh các hộp cố định làm lệch giao diện."""
    width = max(20, min(int(width or term_width()), term_width()))
    content_width = width - 6
    print("  ┌" + "─" * (width - 4) + "┐")
    print("  │ " + _pad_visible(bold(title), content_width) + " │")
    print("  ├" + "─" * (width - 4) + "┤")
    if not lines:
        lines = [""]
    for line in lines:
        print("  │ " + _pad_visible(line, content_width) + " │")
    print("  └" + "─" * (width - 4) + "┘")


def stage(sym: str, label: str, text: str = "") -> None:
    label_part = bold(label[:15].ljust(15))
    print(clip(f" {sym} {label_part} {text}", term_width()))


def sub(text: str, sym: str = "") -> None:
    prefix = f"      {sym + ' ' if sym else ''}"
    print(clip(prefix + text, term_width()))












class Live:
    """Dòng tiến trình động tự xóa trên 1 hàng duy nhất."""
    on = sys.stdout.isatty()
    _last = 0.0

    @classmethod
    def show(cls, msg: str, throttle: float = 0.08) -> None:
        now = time.perf_counter()
        if not cls.on or now - cls._last < throttle:
            return
        cls._last = now
        sys.stdout.write("\r\x1b[K" + dim(clip(f" … {msg}", term_width())))
        sys.stdout.flush()

    @classmethod
    def end(cls) -> None:
        if cls.on:
            sys.stdout.write("\r\x1b[K")
            sys.stdout.flush()


def fmt_size(n: float) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024 or unit == "GB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024
    return f"{n} B"


def dir_size(p: Path) -> int:
    if not p.exists():
        return 0
    total = 0
    for root, _, files in os.walk(p):
        for f in files:
            try:
                total += Path(root, f).stat().st_size
            except OSError:
                pass
    return total


def short_names(rels: List[str], k: int = 4) -> str:
    names = [r.replace("crates/", "", 1).replace("/src/", "/", 1) for r in rels[:k]]
    return ", ".join(names) + (f" … +{len(rels) - k}" if len(rels) > k else "")


def read_command(prompt: str = "Lệnh: ", poll: Optional[Callable[[], bool]] = None) -> str:
    """Nhập lệnh có chỉnh sửa dòng và xác nhận bằng Enter; vẫn kiểm tra watcher khi chờ."""
    if not sys.stdin.isatty():
        return "q"
    sys.stdout.write(prompt)
    sys.stdout.flush()
    chars: List[str] = []
    if os.name == "nt":
        import msvcrt
        while True:
            if msvcrt.kbhit():
                ch = msvcrt.getwch()
                if ch in ("\x00", "\xe0"):
                    msvcrt.getwch()
                    continue
                if ch == "\x03":
                    raise KeyboardInterrupt
                if ch in ("\r", "\n"):
                    sys.stdout.write("\n")
                    sys.stdout.flush()
                    return "".join(chars).strip()
                if ch in ("\b", "\x7f"):
                    if chars:
                        chars.pop()
                        sys.stdout.write("\b \b")
                        sys.stdout.flush()
                    continue
                if ch == "\x15":  # Ctrl+U: xóa cả dòng đang nhập
                    if chars:
                        sys.stdout.write("\b \b" * len(chars))
                        sys.stdout.flush()
                        chars.clear()
                    continue
                if ch.isprintable():
                    chars.append(ch)
                    sys.stdout.write(ch)
                    sys.stdout.flush()
            if poll and not chars and poll():
                sys.stdout.write("\r" + " " * min(120, len(prompt) + 2) + "\r")
                sys.stdout.flush()
                return "\x00watch"
            time.sleep(0.08)

    import select
    import termios
    import tty
    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    escape_state = 0
    escape_at = 0.0
    try:
        tty.setcbreak(fd, termios.TCSANOW)
        while True:
            # Bỏ qua phím mũi tên/ANSI thay vì để chuỗi như "[D" lọt vào lệnh.
            if escape_state and time.perf_counter() - escape_at > 0.08:
                escape_state = 0
            ready, _, _ = select.select([fd], [], [], 0.12)
            if ready:
                raw = os.read(fd, 1)
                if not raw:
                    sys.stdout.write("\n")
                    return ""
                ch = raw.decode("utf-8", "ignore")
                if escape_state == 1:
                    escape_state = 2 if ch in ("[", "O") else 0
                    escape_at = time.perf_counter()
                    continue
                if escape_state == 2:
                    escape_state = 0
                    continue
                if ch == "\x1b":
                    escape_state = 1
                    escape_at = time.perf_counter()
                    continue
                if ch in ("\r", "\n"):
                    sys.stdout.write("\n")
                    sys.stdout.flush()
                    return "".join(chars).strip()
                if ch in ("\x03",):
                    raise KeyboardInterrupt
                if ch in ("\x04",):
                    sys.stdout.write("\n")
                    return ""
                if ch in ("\x7f", "\x08"):
                    if chars:
                        chars.pop()
                        sys.stdout.write("\b \b")
                        sys.stdout.flush()
                    continue
                if ch == "\x15":
                    if chars:
                        sys.stdout.write("\b \b" * len(chars))
                        sys.stdout.flush()
                        chars.clear()
                    continue
                # Menu commands are ASCII; ignore non-ASCII bytes instead of corrupting UTF-8 input.
                if ch and ch.isprintable() and ord(ch) < 128:
                    chars.append(ch)
                    sys.stdout.write(ch)
                    sys.stdout.flush()
            if poll and not chars and poll():
                sys.stdout.write("\r" + " " * min(120, len(prompt) + 2) + "\r")
                sys.stdout.flush()
                return "\x00watch"
    finally:
        termios.tcsetattr(fd, termios.TCSANOW, old)


def normalize_menu_command(raw: str) -> str:
    value = raw.strip().lower()
    aliases = {
        "o": "open", "open": "open",
        "c": "copy", "copy": "copy",
        "p": "pack", "pack": "pack",
        "w": "build-warn", "build-warn": "build-warn",
        "l": "ocd", "ocd": "ocd", "clean-code": "ocd",
        "b": "backup", "backup": "backup", "restore": "backup",
        "x": "clean", "clean": "clean",
        "r": "run", "run": "run", "again": "run",
        "f": "force", "force": "force",
        "q": "quit", "quit": "quit", "exit": "quit", "back": "quit",
        "h": "help", "help": "help", "?": "help",
        "s": "save", "save": "save",
        "e": "export", "export": "export",
        "i": "import", "import": "import",
        "d": "delete", "delete": "delete",
        "n": "next", "next": "next", "older": "next",
        "v": "prev", "prev": "prev", "newer": "prev",
        "light": "1", "deep": "2",
    }
    return aliases.get(value, value)



def ask_yes(question: str, default: bool = False) -> bool:
    """Xác nhận có/không bằng dòng lệnh; chỉ thực hiện sau khi nhấn Enter."""
    hint = "Y/n" if default else "y/N"
    try:
        answer = input(f"  ? {question} [{hint}] · nhập rồi nhấn Enter: ").strip().lower()
    except (EOFError, KeyboardInterrupt):
        print()
        return False
    if not answer:
        return default
    if answer in ("y", "yes", "c", "co", "có"):
        return True
    if answer in ("n", "no", "k", "khong", "không"):
        return False
    print(f"  {WARN} Câu trả lời không hợp lệ; xem như KHÔNG để an toàn.")
    return False



def ask_line(prompt_text: str) -> str:
    if not sys.stdin.isatty():
        return ""
    try:
        return input(f"  › {prompt_text}: ").strip()
    except (EOFError, KeyboardInterrupt):
        print()
        return ""



def copy_to_clipboard(text: str) -> bool:
    """Sao chép văn bản UTF-8 vào Clipboard trên Windows, macOS và Linux."""
    try:
        if os.name == "nt":
            with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False, encoding="utf-8") as f:
                f.write(text)
                tmp = f.name
            try:
                escaped_tmp = str(tmp).replace("'", "''")
                r = subprocess.run(
                    [
                        "powershell", "-NoProfile", "-Command",
                        f"Get-Content -Raw -Encoding UTF8 -LiteralPath '{escaped_tmp}' | Set-Clipboard",
                    ],
                    capture_output=True, timeout=20,
                )
                if r.returncode == 0:
                    return True
            finally:
                try:
                    os.unlink(tmp)
                except OSError:
                    pass
            r = subprocess.run(["clip"], input=("\ufeff" + text).encode("utf-16-le"), timeout=20)
            return r.returncode == 0
        if sys.platform == "darwin":
            return subprocess.run(["pbcopy"], input=text.encode("utf-8"), timeout=20).returncode == 0
        for cmd in (["wl-copy"], ["xclip", "-selection", "clipboard"], ["xsel", "-ib"]):
            if shutil.which(cmd[0]):
                return subprocess.run(cmd, input=text.encode("utf-8"), timeout=20).returncode == 0
    except Exception:
        pass
    try:
        import tkinter
        r = tkinter.Tk()
        r.withdraw()
        r.clipboard_clear()
        r.clipboard_append(text)
        r.update()
        r.destroy()
        return True
    except Exception:
        return False


def pick_file_dialog(save: bool = False, default_name: str = "") -> Optional[Path]:
    """Mở hộp thoại chọn file chuẩn của hệ điều hành (nếu có tkinter), hoặc nhập đường dẫn."""
    try:
        import tkinter
        from tkinter import filedialog
        root = tkinter.Tk()
        root.withdraw()
        root.attributes("-topmost", True)
        if save:
            p = filedialog.asksaveasfilename(
                title="Xuất bản sao lưu BSE-AutoRouter (.zip)",
                initialdir=str(ROOT),
                initialfile=default_name or f"BSE-Backup_{datetime.now():%Y-%m-%d_%H-%M-%S}.zip",
                defaultextension=".zip",
                filetypes=[("Gói sao lưu ZIP", "*.zip")],
            )
        else:
            p = filedialog.askopenfilename(
                title="Chọn gói sao lưu BSE-AutoRouter (.zip) để nhập",
                initialdir=str(ROOT),
                filetypes=[("Gói sao lưu ZIP", "*.zip")],
            )
        root.destroy()
        if p:
            return Path(p)
    except Exception:
        pass
    raw = ask_line("Nhập đường dẫn file .zip (Enter để hủy)")
    if not raw:
        return None
    p = Path(raw.strip("\"'"))
    if save and p.suffix.lower() != ".zip":
        p = p.with_suffix(".zip")
    return p


# ════════════════════════════════════════════════════════════════════════════
#  TIỆN ÍCH ĐỌC/GHI NGUYÊN TỬ & BĂM SHA-1
# ════════════════════════════════════════════════════════════════════════════
def read_text(p: Path) -> str:
    return p.read_text(encoding="utf-8-sig")


def write_atomic(p: Path, data: bytes) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    fd, temp_name = tempfile.mkstemp(prefix=f".{p.name}.bse-", suffix=".tmp", dir=str(p.parent))
    tmp = Path(temp_name)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
            stream.flush()
            try:
                os.fsync(stream.fileno())
            except OSError:
                pass
        os.replace(tmp, p)
    except BaseException:
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
        raise




def copy_atomic(source: Path, destination: Path) -> None:
    """Sao chép file qua tên tạm ngẫu nhiên, tránh để lại ZIP dang dở."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    fd, temp_name = tempfile.mkstemp(prefix=f".{destination.name}.bse-", suffix=".tmp", dir=str(destination.parent))
    os.close(fd)
    temp_path = Path(temp_name)
    try:
        shutil.copy2(source, temp_path)
        os.replace(temp_path, destination)
    except BaseException:
        try:
            temp_path.unlink(missing_ok=True)
        except OSError:
            pass
        raise



def ensure_work_directory() -> None:
    """Bảo đảm thư mục state/report do BSE dùng nằm bên trong project root."""
    if WORK.is_symlink():
        raise OSError("Từ chối dùng _bse vì đường dẫn là symlink.")
    WORK.mkdir(parents=True, exist_ok=True)
    try:
        WORK.resolve(strict=False).relative_to(ROOT)
    except ValueError as exc:
        raise OSError("Thư mục _bse nằm ngoài dự án.") from exc

def ensure_backup_directory() -> None:
    """Tạo kho backup chỉ bên trong root dự án, không đi theo symlink."""
    ensure_work_directory()
    if BACKUPS.is_symlink():
        raise OSError("Từ chối dùng _bse/backups vì đường dẫn là symlink.")
    BACKUPS.mkdir(parents=True, exist_ok=True)
    try:
        BACKUPS.resolve(strict=False).relative_to(ROOT)
    except ValueError as exc:
        raise OSError("Kho backup nằm ngoài thư mục dự án.") from exc

def sha12(data: bytes) -> str:
    return hashlib.sha1(data).hexdigest()[:12]


def file_sha(p: Path) -> str:
    return sha12(p.read_bytes()) if p.exists() else "-"


def walk_sources() -> List[str]:
    out: List[str] = []
    for root, dirs, files in os.walk(ROOT, topdown=True, followlinks=False):
        dirs[:] = [
            d for d in sorted(dirs)
            if d not in SKIP_DIRS and not Path(root, d).is_symlink()
        ]
        for name in sorted(files):
            source = Path(root, name)
            if name.endswith(SRC_EXT) and not source.is_symlink() and source.is_file():
                out.append(source.relative_to(ROOT).as_posix())
    return out



def sources_hash() -> str:
    h = hashlib.sha1()
    for rel in walk_sources():
        h.update(rel.encode())
        h.update(b"\0")
        h.update(read_text(ROOT / rel).strip().encode("utf-8"))
        h.update(b"\0")
    return h.hexdigest()[:12]


def bridge_hash() -> Optional[str]:
    try:
        with BRIDGE.open("rb") as f:
            head = f.read(1500).decode("utf-8", "ignore")
    except OSError:
        return None
    m = re.search(r"src:([0-9a-f]{12})", head)
    return m.group(1) if m else None


def state_load() -> dict:
    try:
        if WORK.is_symlink() or STATE_FILE.is_symlink():
            return {}
        STATE_FILE.resolve(strict=False).relative_to(ROOT)
        return json.loads(STATE_FILE.read_text(encoding="utf-8"))
    except Exception:
        return {}



def state_save(st: dict) -> None:
    ensure_work_directory()
    if STATE_FILE.is_symlink():
        raise OSError("Từ chối ghi state.json vì đây là symlink.")
    payload = json.dumps(st, indent=2, ensure_ascii=False).encode("utf-8")
    write_atomic(STATE_FILE, payload)



# ════════════════════════════════════════════════════════════════════════════
#  ĐỒNG BỘ BUNDLE: UNPACK TĂNG DẦN & PACK DỌN SẠCH
# ════════════════════════════════════════════════════════════════════════════
@dataclass
class Plan:
    contents: Dict[str, str] = field(default_factory=dict)
    new: List[str] = field(default_factory=list)
    changed: List[str] = field(default_factory=list)
    same: List[str] = field(default_factory=list)
    dups: Dict[str, int] = field(default_factory=dict)
    bad: List[str] = field(default_factory=list)
    fences: int = 0


def safe_rel(p: str) -> Optional[str]:
    raw = str(p).strip().replace("\\", "/")
    while raw.startswith("./"):
        raw = raw[2:]
    if not raw or "\x00" in raw or raw.startswith("/") or re.match(r"^[A-Za-z]:", raw):
        return None
    parts = raw.split("/")
    if any(part in ("", ".", "..") or ":" in part for part in parts):
        return None
    if any(part in SKIP_DIRS for part in parts):
        return None
    if not raw.endswith(SRC_EXT):
        return None
    return "/".join(parts)


def safe_destination(rel: str) -> Optional[Path]:
    """Resolve a project-relative path without following any symlink component."""
    normalized = safe_rel(rel)
    if normalized is None:
        return None
    candidate = ROOT.joinpath(*normalized.split("/"))
    current = ROOT
    try:
        for part in normalized.split("/"):
            current = current / part
            if current.is_symlink():
                return None
        candidate.resolve(strict=False).relative_to(ROOT)
    except (OSError, ValueError):
        return None
    return candidate



def strip_fences(body: str) -> Tuple[str, int]:
    """Tự động gỡ dấu ``` thừa ở đầu/cuối khối khi copy từ khung chat AI."""
    lines = body.split("\n")
    removed = 0
    i = 0
    while i < len(lines) and not lines[i].strip():
        i += 1
    if i < len(lines) and re.fullmatch(r"```[\w+-]*", lines[i].strip()):
        del lines[i]
        removed += 1
    j = len(lines) - 1
    while j >= 0 and not lines[j].strip():
        j -= 1
    if j >= 0 and lines[j].strip() == "```":
        del lines[j]
        removed += 1
    return "\n".join(lines), removed


def plan_unpack(text: str) -> Plan:
    plan = Plan()
    cur: Optional[str] = None
    buf: List[str] = []
    seen: Dict[str, int] = {}

    def flush() -> None:
        if cur is None:
            return
        rel = safe_rel(cur)
        if rel is None or safe_destination(rel) is None:
            plan.bad.append(cur)
            return
        body, n = strip_fences("\n".join(buf))
        plan.fences += n
        seen[rel] = seen.get(rel, 0) + 1
        plan.contents[rel] = body.strip() + "\n"

    for line in text.split("\n"):
        s = line.strip()
        if line.startswith(MARKER) and s.endswith("==="):
            flush()
            cur, buf = s[len(MARKER):-3].strip(), []
        elif cur is not None:
            buf.append(line)
    flush()
    plan.dups = {k: v for k, v in seen.items() if v > 1}
    for rel, body in plan.contents.items():
        p = ROOT / rel
        if not p.exists():
            plan.new.append(rel)
        elif read_text(p).strip() + "\n" == body:
            plan.same.append(rel)
        else:
            plan.changed.append(rel)
    return plan


def apply_unpack(plan: Plan) -> None:
    for rel in plan.new + plan.changed:
        destination = safe_destination(rel)
        if destination is None:
            raise ValueError(f"Từ chối ghi đường dẫn không an toàn trong bundle: {rel}")
        write_atomic(destination, plan.contents[rel].encode("utf-8"))



def build_bundle_text() -> Tuple[str, int]:
    parts = [BUNDLE_HEADER]
    n = 0
    for rel in walk_sources():
        parts.append(f"{MARKER}{rel} ===\n{read_text(ROOT / rel).strip()}\n\n")
        n += 1
    return "".join(parts), n


def pack_bundle() -> Tuple[int, int, int, bool]:
    text, count = build_bundle_text()
    new_data = text.encode("utf-8")
    old_data = BUNDLE.read_bytes() if BUNDLE.exists() else b""
    if old_data == new_data:
        return len(old_data), len(new_data), count, False
    # Bundle có thể chứa bản cập nhật thủ công từ AI; chụp lại trước khi chuẩn hóa/ghi đè.
    snapshot = make_snapshot("truoc-pack", note="Tự động lưu trước khi ghi lại RUST_PROJECT_BUNDLE.txt")
    if snapshot is None or not snapshot.exists():
        raise OSError("Không tạo được backup trước khi ghi đè RUST_PROJECT_BUNDLE.txt")
    write_atomic(BUNDLE, new_data)
    return len(old_data), len(new_data), count, True



# ════════════════════════════════════════════════════════════════════════════
#  KIỂM TOÁN & LÀM SẠCH MÃ NGUỒN / HTML CHUẨN "OCD" (OCD SANITIZER)
# ════════════════════════════════════════════════════════════════════════════
BAND_AID_ALLOWS = re.compile(
    r"#\[\s*allow\s*\(\s*(unused_variables|unused_imports|dead_code|unused_mut|unused)\s*\)\s*\]"
)


def audit_rust_ocd() -> List[Tuple[str, int, str]]:
    """Quét toàn bộ mã Rust tìm các cờ #[allow(unused_...)] vá tạm của AI."""
    findings: List[Tuple[str, int, str]] = []
    for rel in walk_sources():
        if not rel.endswith(".rs"):
            continue
        p = ROOT / rel
        for idx, line in enumerate(read_text(p).split("\n"), 1):
            m = BAND_AID_ALLOWS.search(line)
            if m:
                findings.append((rel, idx, m.group(0)))
    return findings


def sanitize_rust_ocd() -> List[str]:
    """
    Dọn dẹp tận gốc các bản vá #[allow(unused_variables)] trong mã nguồn Rust:
    1. Trong builder.rs: đổi `let (mut board, fps, _, _, _) = rebuild_routing_state(...)`
       thành `let (mut board, _, _, _, _) = ...` và xóa hẳn `#[allow(unused_variables)]`.
    2. Nếu có `cargo clippy` / `cargo fix`, hỗ trợ gỡ các cờ allow vá tạm khác một cách an toàn.
    """
    actions: List[str] = []
    builder = safe_destination("crates/bse-core/src/meta_search/builder.rs")
    if builder is None:
        stage(WARN, "An toàn", "Bỏ qua tự sửa builder.rs vì đường dẫn có symlink hoặc nằm ngoài dự án.")
        return actions
    if builder.exists():
        src = read_text(builder)
        orig = src
        # Sửa biến thừa `fps` trong try_routing_first_lns và gỡ bỏ #[allow(unused_variables)]
        src_new = re.sub(
            r"let\s*\(\s*mut\s+board\s*,\s*fps\s*,\s*_\s*,\s*_\s*,\s*_\s*\)\s*=\s*\n?\s*rebuild_routing_state",
            "let (mut board, _, _, _, _) =\n        rebuild_routing_state",
            src,
        )
        if src_new != src or "let (mut board, _, _, _, _) =" in src:
            src = re.sub(r"^[ \t]*#\[\s*allow\s*\(\s*unused_variables\s*\)\s*\]\n", "", src_new, flags=re.M)
        if src != orig:
            try:
                snap = make_snapshot("truoc-sua-rust", note="Tự động lưu trước khi chuẩn hóa builder.rs", pinned=True)
            except Exception as exc:
                stage(WARN, "An toàn", f"Không tạo được backup; bỏ qua sửa builder.rs: {exc}")
                return actions
            if snap is None:
                stage(WARN, "An toàn", "Không tạo được backup; đã bỏ qua việc tự sửa builder.rs")
                return actions
            write_atomic(builder, src.encode("utf-8"))
            actions.append("bse-core/meta_search/builder.rs: gỡ `#[allow(unused_variables)]` & dọn biến thừa `fps`")
    return actions


def sanitize_html_ocd() -> List[str]:
    """
    Chuẩn hóa main.html theo đúng yêu cầu thiết kế:
    1. Sửa `wasm_bindgen.WasmSolver.new(...)` -> `new wasm_bindgen.WasmSolver(...)` (khớp constructor Rust).
    2. Bỏ dòng tự ngắt sớm `if (stepRes.bestErrorCount === 0 && budget !== -1) break;` để bộ giải
       LUÔN chạy hết thời gian (hoặc đến khi bấm Tạm dừng) nhằm tối ưu thẩm mỹ P2Ortho / P3Compact.
    3. Xóa nhánh thừa `else if (c.type === 'BTN4') { ... }` còn sót lại trong `parseNetlist`.
    4. Đảm bảo có thẻ `<script src="wasm-bridge.js"></script>` trước `</body>`.
    """
    if not HTML.exists():
        return []
    text = read_text(HTML)
    orig = text
    fixes: List[str] = []

    new_text, n1 = re.subn(r"\bwasm_bindgen\.WasmSolver\.new\s*\(", "new wasm_bindgen.WasmSolver(", text)
    if n1:
        text = new_text
        fixes.append("đồng bộ constructor `new wasm_bindgen.WasmSolver(...)`")

    new_text, n2 = re.subn(
        r"^[ \t]*if\s*\(\s*stepRes\.bestErr(?:or)?Count\s*===\s*0\s*&&\s*budget\s*!==\s*-1\s*\)\s*break\s*;?[ \t]*\n?",
        "",
        text,
        flags=re.M,
    )
    if n2:
        text = new_text
        fixes.append("bỏ ngắt sớm ở 0 lỗi (luôn chạy hết thời gian/đến khi bấm dừng để tối ưu thẩm mỹ)")

    btn4_pat = re.compile(
        r"\s*else if\s*\(\s*c\.type\s*===\s*'BTN4'\s*\)\s*\{\s*"
        r"const leftNets[^}]*?\n[ \t]*for\s*\([^}]*?\}\s*\}",
        re.S,
    )
    new_text, n3 = btn4_pat.subn("", text)
    if n3:
        text = new_text
        fixes.append("dọn nhánh thừa `BTN4` trong `parseNetlist`")

    if not re.search(r"<script[^>]+src=[\"']wasm-bridge\.js[\"']", text) and "</body>" in text:
        text = text.replace("</body>", '<script src="wasm-bridge.js"></script>\n</body>')
        fixes.append('thêm `<script src="wasm-bridge.js"></script>`')

    if text != orig:
        try:
            snap = make_snapshot("truoc-sua-html", note="Tự động lưu trước khi chuẩn hóa main.html", pinned=True)
        except Exception as exc:
            stage(WARN, "An toàn", f"Không tạo được backup; bỏ qua sửa main.html: {exc}")
            return []
        if snap is None:
            stage(WARN, "An toàn", "Không tạo được backup; đã bỏ qua việc tự sửa main.html")
            return []
        write_atomic(HTML, text.encode("utf-8"))
    return fixes


# ════════════════════════════════════════════════════════════════════════════
#  TRUNG TÂM SAO LƯU KIỂU WINDOWS BACKUP / SYSTEM RESTORE (HỖ TRỢ BẢN LỖI & NHẬP/XUẤT)
# ════════════════════════════════════════════════════════════════════════════
def tree_sig() -> str:
    """Dấu vân tay gồm mã nguồn và các tệp đầu vào/đầu ra quan trọng."""
    parts = (
        sources_hash(),
        file_sha(BUNDLE),
        file_sha(ROOT / "Cargo.lock"),
        file_sha(HTML),
        file_sha(BRIDGE),
        file_sha(REPORT_FILE),
    )
    return sha12("\0".join(parts).encode("utf-8"))



def list_snapshots() -> List[dict]:
    out = []
    try:
        if not BACKUPS.exists() or BACKUPS.is_symlink():
            return out
        BACKUPS.resolve(strict=False).relative_to(ROOT)
    except (OSError, ValueError):
        return out
    for path in BACKUPS.glob("*.zip"):
        if path.is_symlink() or not path.is_file():
            continue
        try:
            with zipfile.ZipFile(path) as archive:
                if archive.getinfo("manifest.json").file_size > 1024 * 1024:
                    raise ValueError("manifest too large")
                manifest = json.loads(archive.read("manifest.json").decode("utf-8"))
                if not isinstance(manifest, dict):
                    manifest = {}
                for field_name in ("time", "tag", "status", "note", "summary", "src_hash", "sig"):
                    value = manifest.get(field_name, "")
                    if not isinstance(value, str):
                        value = str(value)
                    value = _ANSI_RE.sub("", value)
                    manifest[field_name] = "".join(ch if ch >= " " and ch != "\x7f" else " " for ch in value).strip()
                manifest["pinned"] = bool(manifest.get("pinned", False))
                manifest["good"] = bool(manifest.get("good", False))
        except Exception:
            manifest = {"tag": "zip-khong-hop-le", "status": "unknown", "note": "Không đọc được manifest"}
        try:
            stat_result = path.stat()
            manifest["path"] = path
            manifest["size"] = stat_result.st_size
            manifest["_mtime"] = stat_result.st_mtime
            out.append(manifest)
        except OSError:
            continue
    out.sort(key=lambda item: (str(item.get("time", "")), item.get("_mtime", 0.0)), reverse=True)
    for item in out:
        item.pop("_mtime", None)
    return out



def prune_snapshots() -> None:
    """Giữ tối đa KEEP_AUTO_BACKUPS bản tự động và luôn giữ bản tốt duy nhất."""
    snapshots = list_snapshots()
    automatic = [item for item in snapshots if not item.get("pinned")]
    if len(automatic) <= KEEP_AUTO_BACKUPS:
        return
    retained_auto = automatic[:KEEP_AUTO_BACKUPS]
    has_good = any(item.get("status") == "ok" or item.get("good") for item in retained_auto)
    if not has_good:
        has_good = any(item.get("pinned") and (item.get("status") == "ok" or item.get("good")) for item in snapshots)
    for old in automatic[KEEP_AUTO_BACKUPS:]:
        good = old.get("status") == "ok" or old.get("good")
        if good and not has_good:
            has_good = True
            continue
        try:
            old["path"].unlink()
        except OSError:
            pass



def make_snapshot(
    tag: str,
    note: str = "",
    status: Optional[str] = None,
    summary: str = "",
    pinned: bool = False,
    dest_path: Optional[Path] = None,
) -> Optional[Path]:
    """Tạo ZIP snapshot nguyên tử, tên dễ đọc và không ghi đè bản cũ."""
    srcs = walk_sources()
    if not srcs and not BUNDLE.exists():
        return None
    cur_src = sources_hash() if srcs else file_sha(BUNDLE)
    sig = tree_sig()
    st = state_load()
    if status is None:
        if st.get("last_ok_src") == cur_src:
            status = "ok"
        elif st.get("last_status_src") == cur_src:
            status = st.get("last_status", "unknown")
        else:
            status = "unknown"
    if not summary and st.get("last_status_src") == cur_src:
        summary = st.get("last_summary", "")

    if dest_path is None and not pinned:
        snaps = list_snapshots()
        if snaps and snaps[0].get("sig") == sig and tag not in (
            "truoc-khoi-phuc", "thu-cong", "truoc-sua-html", "truoc-sua-rust"
        ):
            if status != "unknown" and snaps[0].get("status") in (None, "unknown"):
                _update_snapshot_manifest(snaps[0]["path"], status=status, summary=summary)
            return snaps[0]["path"]

    if dest_path is None:
        ensure_backup_directory()
        stamp = datetime.now().strftime("%Y-%m-%d_%H-%M-%S")
        slug = re.sub(r"[^a-zA-Z0-9_-]+", "-", tag).strip("-_") or "backup"
        path = BACKUPS / f"{stamp}__{slug}.zip"
        idx = 2
        while path.exists():
            path = BACKUPS / f"{stamp}__{slug}_{idx}.zip"
            idx += 1
    else:
        path = dest_path.expanduser().absolute()
        path.parent.mkdir(parents=True, exist_ok=True)

    manifest = {
        "time": datetime.now().isoformat(timespec="seconds"),
        "tag": tag,
        "note": note,
        "status": status,
        "good": (status == "ok"),
        "summary": summary,
        "pinned": pinned,
        "src_hash": cur_src,
        "sig": sig,
        "files_count": len(srcs),
    }
    fd, temp_name = tempfile.mkstemp(prefix=f".{path.stem}.bse-", suffix=".tmp", dir=str(path.parent))
    os.close(fd)
    tmp_path = Path(temp_name)
    try:
        with zipfile.ZipFile(tmp_path, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as z:
            for rel in srcs:
                z.write(ROOT / rel, "src/" + rel)
            for name in ("Cargo.lock", "main.html", "wasm-bridge.js", "RUST_PROJECT_BUNDLE.txt"):
                source = ROOT / name
                if source.is_file() and not source.is_symlink():
                    z.write(source, "root/" + name)
            if REPORT_FILE.is_file() and not REPORT_FILE.is_symlink():
                z.write(REPORT_FILE, "meta/last_report.txt")
            z.writestr("manifest.json", json.dumps(manifest, indent=2, ensure_ascii=False))
        with zipfile.ZipFile(tmp_path, "r") as verify:
            if verify.testzip() is not None:
                raise OSError("Gói sao lưu tạo ra không vượt qua kiểm tra toàn vẹn")
        os.replace(tmp_path, path)
    except BaseException:
        try:
            tmp_path.unlink(missing_ok=True)
        except OSError:
            pass
        raise

    if dest_path is None:
        prune_snapshots()
    return path



def _update_snapshot_manifest(zip_path: Path, **updates) -> None:
    """Cập nhật manifest bằng file tạm an toàn và thay thế nguyên tử."""
    temp_path: Optional[Path] = None
    try:
        if zip_path.is_symlink() or not zip_path.is_file():
            return
        with zipfile.ZipFile(zip_path, "r") as zin:
            items = {name: zin.read(name) for name in zin.namelist()}
        manifest = json.loads(items.get("manifest.json", b"{}").decode("utf-8"))
        if not isinstance(manifest, dict):
            return
        manifest.update(updates)
        if "status" in updates:
            manifest["good"] = manifest["status"] == "ok"
        items["manifest.json"] = json.dumps(manifest, indent=2, ensure_ascii=False).encode("utf-8")
        fd, temp_name = tempfile.mkstemp(prefix=f".{zip_path.name}.bse-", suffix=".tmp", dir=str(zip_path.parent))
        os.close(fd)
        temp_path = Path(temp_name)
        with zipfile.ZipFile(temp_path, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as zout:
            for name, data in items.items():
                zout.writestr(name, data)
        os.replace(temp_path, zip_path)
    except Exception:
        if temp_path is not None:
            try:
                temp_path.unlink(missing_ok=True)
            except OSError:
                pass




_ALLOWED_ROOT_SNAPSHOT_FILES = {
    "Cargo.lock", "main.html", "wasm-bridge.js", "RUST_PROJECT_BUNDLE.txt"
}
_ALLOWED_META_SNAPSHOT_FILES = {"meta/last_report.txt", "manifest.json"}
_MAX_SNAPSHOT_TOTAL = 256 * 1024 * 1024
_MAX_SNAPSHOT_ENTRIES = 20000


def inspect_snapshot_zip(path: Path, check_destinations: bool = False):
    """Đọc và xác minh snapshot trước khi tin bất kỳ đường dẫn nào bên trong ZIP."""
    try:
        with zipfile.ZipFile(path, "r") as z:
            infos = z.infolist()
            if len(infos) > _MAX_SNAPSHOT_ENTRIES:
                raise ValueError("Gói sao lưu chứa quá nhiều mục")
            total = 0
            source_names: Dict[str, str] = {}
            root_names: Dict[str, str] = {}
            seen_names = set()
            seen_source_paths = set()
            for info in infos:
                name = info.filename
                if info.is_dir():
                    continue
                if name in seen_names:
                    raise ValueError(f"Gói sao lưu có đường dẫn trùng: {name}")
                seen_names.add(name)
                total += info.file_size
                if info.file_size < 0 or total > _MAX_SNAPSHOT_TOTAL:
                    raise ValueError("Gói sao lưu vượt giới hạn dung lượng giải nén 256 MiB")
                if info.flag_bits & 0x1:
                    raise ValueError("Không hỗ trợ ZIP được mã hóa")
                mode = (info.external_attr >> 16) & 0xFFFF
                if stat.S_ISLNK(mode):
                    raise ValueError(f"Không chấp nhận liên kết tượng trưng trong ZIP: {name}")
                if name.startswith("src/"):
                    rel = safe_rel(name[4:])
                    if rel is None or rel != name[4:]:
                        raise ValueError(f"Đường dẫn mã nguồn không an toàn trong ZIP: {name}")
                    folded_rel = rel.casefold()
                    if folded_rel in seen_source_paths:
                        raise ValueError(f"Đường dẫn mã nguồn trùng nếu không phân biệt hoa thường: {rel}")
                    seen_source_paths.add(folded_rel)
                    if check_destinations and safe_destination(rel) is None:
                        raise ValueError(f"Đường dẫn đích nằm ngoài dự án hoặc qua symlink: {rel}")
                    source_names[rel] = name
                elif name.startswith("root/"):
                    root_rel = name[5:]
                    if root_rel not in _ALLOWED_ROOT_SNAPSHOT_FILES:
                        raise ValueError(f"Mục root không được hỗ trợ trong ZIP: {name}")
                    root_names[root_rel] = name
                elif name not in _ALLOWED_META_SNAPSHOT_FILES:
                    raise ValueError(f"Mục không được hỗ trợ trong gói sao lưu: {name}")
            if not source_names and "RUST_PROJECT_BUNDLE.txt" not in root_names:
                raise ValueError("ZIP không chứa mã nguồn hay RUST_PROJECT_BUNDLE.txt")
            bad = z.testzip()
            if bad:
                raise ValueError(f"Dữ liệu ZIP bị lỗi tại: {bad}")
            manifest = {}
            if "manifest.json" in seen_names:
                if z.getinfo("manifest.json").file_size > 1024 * 1024:
                    raise ValueError("manifest.json quá lớn")
                try:
                    manifest = json.loads(z.read("manifest.json").decode("utf-8"))
                except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                    raise ValueError("manifest.json không hợp lệ") from exc
                if not isinstance(manifest, dict):
                    raise ValueError("manifest.json phải là một JSON object")
            sources = {rel: z.read(name) for rel, name in source_names.items()}
            roots = {rel: z.read(name) for rel, name in root_names.items()}
            return manifest, sources, roots
    except (zipfile.BadZipFile, OSError) as exc:
        raise ValueError(f"Không đọc được gói ZIP: {exc}") from exc

def restore_snapshot(path: Path, with_html: bool) -> None:
    """Khôi phục snapshot hợp lệ; luôn lưu trạng thái hiện tại trước khi ghi/xóa."""
    manifest, sources, roots = inspect_snapshot_zip(path, check_destinations=True)
    safety_copy = make_snapshot("truoc-khoi-phuc", note=f"Tự động lưu trước khi khôi phục {path.name}", pinned=True)
    if safety_copy is None or not safety_copy.exists():
        raise RuntimeError("Không thể tạo bản sao lưu an toàn trước khi khôi phục; đã hủy thao tác.")

    if sources:
        keep = set(sources)
        for rel in walk_sources():
            if rel not in keep:
                destination = safe_destination(rel)
                if destination is None:
                    raise ValueError(f"Từ chối xóa đường dẫn không an toàn: {rel}")
                try:
                    destination.unlink()
                except FileNotFoundError:
                    pass
        for rel, data in sources.items():
            destination = safe_destination(rel)
            if destination is None:
                raise ValueError(f"Từ chối ghi đường dẫn không an toàn: {rel}")
            write_atomic(destination, data)
    elif "RUST_PROJECT_BUNDLE.txt" in roots:
        write_atomic(BUNDLE, roots["RUST_PROJECT_BUNDLE.txt"])
        plan = plan_unpack(read_text(BUNDLE))
        apply_unpack(plan)

    for name in ("Cargo.lock", "wasm-bridge.js") + (("main.html",) if with_html else ()):
        if name in roots:
            write_atomic(ROOT / name, roots[name])

    crates = ROOT / "crates"
    if crates.exists() and not crates.is_symlink():
        for d in sorted((p for p in crates.rglob("*") if p.is_dir() and not p.is_symlink()), key=lambda p: len(p.parts), reverse=True):
            try:
                d.rmdir()
            except OSError:
                pass
    pack_bundle()
    st = state_load()
    if manifest.get("good") or manifest.get("status") == "ok":
        st["last_ok_src"] = manifest.get("src_hash")
    else:
        st.pop("last_ok_src", None)
    state_save(st)



def describe_snapshot(i: int, s: dict) -> List[str]:
    """Hai dòng/bản: dòng đầu là trạng thái, dòng hai là tên file và ghi chú."""
    when = str(s.get("time", "?")).replace("T", " ")
    status = s.get("status") or ("ok" if s.get("good") else "unknown")
    if status == "ok":
        badge = green("✔ 0 lỗi")
    elif status == "warn":
        badge = yellow("⚠ Cảnh báo")
    elif status == "fail":
        badge = red("✘ Bản lỗi")
    else:
        badge = dim("• Chưa xác định")
    pin = magenta("★ ") if s.get("pinned") else "  "
    tag_map = {
        "truoc-cap-nhat": "Trước cập nhật",
        "truoc-khoi-phuc": "Trước khôi phục",
        "truoc-sua-html": "Trước sửa main.html",
        "truoc-sua-rust": "Trước sửa mã Rust",
        "truoc-pack": "Trước đóng gói bundle",
        "ban-chay-tot": "Bản chuẩn 0 lỗi",
        "ban-nhap-loi": "Bản đang phát triển (còn lỗi)",
        "thu-cong": "Điểm khôi phục thủ công",
        "nhap-ngoai": "Nhập từ file ngoài",
        "xuat-ngoai": "Bản xuất ra ngoài",
    }
    label = tag_map.get(str(s.get("tag", "")), str(s.get("tag", "Sao lưu")))
    note = str(s.get("note") or s.get("summary") or "").strip()
    filename = s["path"].name if isinstance(s.get("path"), Path) else "tệp ZIP"
    size = fmt_size(s.get("size", 0))
    first = f"{i:>2}. {pin}{when}  {badge}  {label}"
    second = f"    ↳ {filename} · {size}" + (f" · {note}" if note else "")
    return [first, second]



def backup_center_interactive() -> bool:
    """Trung tâm sao lưu/khôi phục; mọi lệnh menu đều phải nhấn Enter."""
    page = 0
    while True:
        snaps = list_snapshots()
        page_count = max(1, (len(snaps) + BACKUP_PAGE_SIZE - 1) // BACKUP_PAGE_SIZE)
        page = min(page, page_count - 1)
        first_index = page * BACKUP_PAGE_SIZE
        visible_snaps = snaps[first_index:first_index + BACKUP_PAGE_SIZE]
        lines: List[str] = []
        if not snaps:
            lines.append("Chưa có điểm khôi phục trong _bse/backups/")
        else:
            lines.append(f"Trang {page + 1}/{page_count} · {len(snaps)} bản trong toàn kho · số thứ tự dùng cho toàn kho")
            lines.append("")
            for offset, snapshot in enumerate(visible_snaps, first_index + 1):
                lines.extend(describe_snapshot(offset, snapshot))
        lines.extend([
            "",
            "s / save    Tạo điểm khôi phục thủ công (kể cả bản đang lỗi)",
            "1..N        Chọn bản theo số thứ tự để thao tác",
            "e / export  Xuất một bản ZIP hoặc trạng thái hiện tại",
            "i / import  Nhập và kiểm tra một gói ZIP từ ngoài",
            "d / delete  Xóa một bản sao lưu sau xác nhận",
        ])
        if page_count > 1:
            lines.append(f"n / next    Trang cũ hơn · v / prev    Trang mới hơn ({len(visible_snaps)} bản/trang)")
        lines.extend([
            "q / back    Quay lại menu trước",
            "Nhập lệnh rồi nhấn Enter để xác nhận; Ctrl+C quay lại menu trước.",
        ])
        print()
        draw_panel("TRUNG TÂM SAO LƯU & KHÔI PHỤC", lines)
        try:
            k = normalize_menu_command(read_command("Backup > "))
        except KeyboardInterrupt:
            print(f"\n  {INFO} Đã hủy menu backup; quay lại menu trước.")
            return False
        if k == "\x00watch":
            continue
        if k in ("q", "quit"):
            return False
        if not k:
            stage(INFO, "Lệnh", "Nhập một lệnh rồi nhấn Enter; dùng q để quay lại.")
            continue
        if k == "next":
            page = min(page + 1, max(0, page_count - 1))
            continue
        if k == "prev":
            page = max(0, page - 1)
            continue
        if k == "save":
            note = ask_line("Ghi chú cho bản sao lưu (Enter để dùng ghi chú mặc định)")
            try:
                snap = make_snapshot("thu-cong", note=note or "Sao lưu thủ công", pinned=True)
                if snap:
                    stage(OK, "Đã sao lưu", f"{snap.name} · {fmt_size(snap.stat().st_size)} · đã ghim bảo vệ")
                else:
                    stage(BAD, "Đã sao lưu", "Không tạo được bản sao lưu; hãy kiểm tra mã nguồn/bundle.")
            except (OSError, zipfile.BadZipFile) as exc:
                stage(BAD, "Đã sao lưu", str(exc))
            continue
        if k.isdigit() and int(k) >= 1:
            idx = int(k)
            if idx > len(snaps):
                stage(WARN, "Khôi phục", f"Không có bản số {idx}")
                continue
            target = snaps[idx - 1]
            print("  Bản được chọn:")
            for detail_line in describe_snapshot(idx, target):
                print("  " + clip(detail_line, term_width() - 2))
            if ask_yes("Khôi phục mã Rust + bundle về bản này?", default=False):
                html_too = ask_yes("Khôi phục cả main.html trong gói?", default=False)
                try:
                    restore_snapshot(target["path"], html_too)
                except Exception as exc:
                    stage(BAD, "Khôi phục", f"Đã hủy hoặc khôi phục thất bại: {exc}")
                    continue
                stage(OK, "Khôi phục", "Hoàn tất; trạng thái dự án đã được nạp lại.")
                return True
            continue
        if k == "export":
            raw_num = ask_line("Số bản muốn xuất (Enter = xuất trạng thái hiện tại)")
            if raw_num and (not raw_num.isdigit() or not (1 <= int(raw_num) <= len(snaps))):
                stage(WARN, "Xuất ZIP", "Số thứ tự không hợp lệ; chưa có file nào được xuất.")
                continue
            default_name = f"BSE-Backup_{datetime.now():%Y-%m-%d_%H-%M-%S}.zip"
            dest = pick_file_dialog(save=True, default_name=default_name)
            if not dest:
                stage(dim("↷"), "Xuất ZIP", "Đã hủy chọn nơi lưu.")
                continue
            src_zip = snaps[int(raw_num) - 1]["path"] if raw_num else None
            if src_zip is not None and src_zip.resolve() == dest.expanduser().resolve():
                stage(WARN, "Xuất ZIP", "File đích trùng bản gốc; không cần sao chép.")
                continue
            if dest.exists() and not ask_yes(f"File {dest.name} đã tồn tại. Ghi đè?", default=False):
                stage(INFO, "Xuất ZIP", "Đã hủy ghi đè file đích.")
                continue
            try:
                if raw_num:
                    assert src_zip is not None
                    copy_atomic(src_zip, dest)
                    stage(OK, "Đã xuất", f"Bản #{raw_num} → {dest.name} · {fmt_size(dest.stat().st_size)}")
                else:
                    note = ask_line("Ghi chú đính kèm (không bắt buộc)")
                    exported = make_snapshot("xuat-ngoai", note=note or "Bản xuất trực tiếp", pinned=True, dest_path=dest)
                    if not exported:
                        raise OSError("Không tạo được gói sao lưu")
                    stage(OK, "Đã xuất", f"Trạng thái hiện tại → {dest.name} · {fmt_size(dest.stat().st_size)}")
            except (OSError, zipfile.BadZipFile) as exc:
                stage(BAD, "Xuất ZIP", str(exc))
            continue
        if k == "import":
            src = pick_file_dialog(save=False)
            if not src or not src.exists():
                stage(dim("↷"), "Nhập ZIP", "Đã hủy hoặc không tìm thấy file đã chọn.")
                continue
            try:
                inspect_snapshot_zip(src, check_destinations=False)
            except Exception as exc:
                stage(BAD, "Nhập ZIP", f"Gói không hợp lệ: {exc}")
                continue
            try:
                ensure_backup_directory()
            except OSError as exc:
                stage(BAD, "Nhập ZIP", str(exc))
                continue
            stamp = datetime.now().strftime("%Y-%m-%d_%H-%M-%S")
            slug = re.sub(r"[^a-zA-Z0-9_-]+", "-", src.stem).strip("-_")[:36] or "backup"
            imported = BACKUPS / f"{stamp}__nhap-ngoai__{slug}.zip"
            index = 2
            while imported.exists():
                imported = BACKUPS / f"{stamp}__nhap-ngoai__{slug}_{index}.zip"
                index += 1
            try:
                copy_atomic(src, imported)
                _update_snapshot_manifest(imported, tag="nhap-ngoai", note=f"Nhập từ {src.name}", pinned=True)
            except OSError as exc:
                stage(BAD, "Nhập ZIP", str(exc))
                continue
            stage(OK, "Đã nhập", f"{src.name} → {imported.name}")
            if ask_yes("Khôi phục ngay bản vừa nhập?", default=False):
                html_too = ask_yes("Khôi phục cả main.html trong gói?", default=False)
                try:
                    restore_snapshot(imported, html_too)
                except Exception as exc:
                    stage(BAD, "Khôi phục", f"Đã hủy hoặc khôi phục thất bại: {exc}")
                    continue
                stage(OK, "Khôi phục", "Hoàn tất; trạng thái dự án đã được nạp lại.")
                return True
            continue
        if k == "delete":
            raw_num = ask_line("Số thứ tự bản sao lưu muốn xóa (Enter để hủy)")
            if not raw_num:
                continue
            if not raw_num.isdigit() or not (1 <= int(raw_num) <= len(snaps)):
                stage(WARN, "Xóa backup", "Số thứ tự không hợp lệ; không xóa gì cả.")
                continue
            target = snaps[int(raw_num) - 1]
            if ask_yes(f"Xóa vĩnh viễn {target['path'].name}?", default=False):
                try:
                    target["path"].unlink()
                    stage(OK, "Đã xóa", target["path"].name)
                except OSError as exc:
                    stage(BAD, "Lỗi xóa", str(exc))
            continue
        stage(WARN, "Lệnh", "Không nhận ra lệnh. Nhập s, số bản, e, i, d hoặc q rồi nhấn Enter.")



# ════════════════════════════════════════════════════════════════════════════
#  TRÌNH DỌN RÁC & GIẢI PHÓNG DUNG LƯỢNG (~1GB TARGET/ & FILE TẠM)
# ════════════════════════════════════════════════════════════════════════════
def clean_temp_artifacts(archive_legacy: bool = False) -> Tuple[int, List[str]]:
    """Dọn artefact do dự án tạo, bỏ qua target/, .git/, dependencies và kho backup."""
    freed = 0
    actions: List[str] = []

    if PKG.exists() and not PKG.is_symlink():
        sz = dir_size(PKG)
        shutil.rmtree(PKG, ignore_errors=True)
        freed += sz
        actions.append(f"xóa pkg/ ({fmt_size(sz)})")

    protected_dirs = {"target", ".git", "node_modules", "pkg", "_bse"}
    removed_pyc = 0
    removed_tmp = 0
    tmp_bytes = 0
    for current, dirs, files in os.walk(ROOT, topdown=True, followlinks=False):
        current_path = Path(current)
        dirs[:] = [d for d in dirs if d not in protected_dirs and not (current_path / d).is_symlink()]
        if current_path.name == "__pycache__":
            size = dir_size(current_path)
            shutil.rmtree(current_path, ignore_errors=True)
            freed += size
            removed_pyc += 1
            dirs[:] = []
            continue
        for filename in files:
            # Chỉ xóa file tạm có dấu nhận diện do BSE tạo; không đụng *.tmp của người dùng.
            if not filename.endswith(".tmp") or ".bse-" not in filename:
                continue
            candidate = current_path / filename
            try:
                if candidate.is_symlink() or not candidate.is_file():
                    continue
                size = candidate.stat().st_size
                candidate.unlink()
                freed += size
                tmp_bytes += size
                removed_tmp += 1
            except OSError:
                continue
    if removed_pyc:
        actions.append(f"xóa {removed_pyc} thư mục __pycache__")
    if removed_tmp:
        actions.append(f"xóa {removed_tmp} file tạm BSE ({fmt_size(tmp_bytes)})")

    if archive_legacy:
        if WORK.is_symlink() or LEGACY_ARCHIVE.is_symlink():
            raise OSError("Từ chối cất script cũ vì _bse/legacy_scripts là symlink.")
        LEGACY_ARCHIVE.mkdir(parents=True, exist_ok=True)
        if LEGACY_ARCHIVE.is_symlink():
            raise OSError("Từ chối cất script cũ vào symlink.")
        try:
            LEGACY_ARCHIVE.resolve(strict=False).relative_to(ROOT)
        except ValueError as exc:
            raise OSError("Kho script cũ nằm ngoài thư mục dự án.") from exc
        moved = []
        for name in LEGACY_FILES:
            source = ROOT / name
            if not source.exists() or not source.is_file():
                continue
            LEGACY_ARCHIVE.mkdir(parents=True, exist_ok=True)
            destination = LEGACY_ARCHIVE / name
            if destination.exists():
                stamp = datetime.now().strftime("%Y-%m-%d_%H-%M-%S")
                destination = LEGACY_ARCHIVE / f"{source.stem}_{stamp}{source.suffix}"
                index = 2
                while destination.exists():
                    destination = LEGACY_ARCHIVE / f"{source.stem}_{stamp}_{index}{source.suffix}"
                    index += 1
            shutil.move(str(source), str(destination))
            moved.append(destination.name)
        if moved:
            actions.append(f"cất {len(moved)} script cũ vào _bse/legacy_scripts/ ({', '.join(moved)})")
    return freed, actions



def cleaner_interactive() -> None:
    """Menu dọn dẹp; hiển thị dung lượng trực tiếp và yêu cầu Enter cho mọi lệnh."""
    while True:
        sz_target = dir_size(TARGET_DIR)
        sz_pkg = dir_size(PKG)
        sz_backups = dir_size(BACKUPS)
        legacy_present = [f for f in LEGACY_FILES if (ROOT / f).exists()]
        lines = [
            f"target/       {fmt_size(sz_target):>10}   Dữ liệu build Rust; có thể build lại",
            f"pkg/          {fmt_size(sz_pkg):>10}   WASM trung gian; có thể được tạo lại",
            f"_bse/backups/ {fmt_size(sz_backups):>10}   {len(list_snapshots())} bản sao lưu (không tự xóa ở đây)",
            "Script cũ: " + (", ".join(legacy_present) if legacy_present else "Đã sạch"),
            "",
            "1 / light   Dọn pkg/, __pycache__, file tạm do BSE tạo và cất script cũ",
            f"2 / deep    Xóa target/ để thu hồi khoảng {fmt_size(sz_target)}",
            "q / back    Quay lại menu trước",
            "Nhập lệnh rồi nhấn Enter để xác nhận; Ctrl+C quay lại menu trước.",
        ]
        print()
        draw_panel("DỌN DẸP & GIẢI PHÓNG DUNG LƯỢNG", lines)
        try:
            k = normalize_menu_command(read_command("Clean > "))
        except KeyboardInterrupt:
            print(f"\n  {INFO} Đã hủy menu dọn dẹp; quay lại menu trước.")
            return
        if k in ("q", "quit"):
            return
        if not k:
            stage(INFO, "Lệnh", "Nhập 1, 2 hoặc q rồi nhấn Enter.")
            continue
        if k == "1":
            try:
                freed, acts = clean_temp_artifacts(archive_legacy=True)
                detail = " · ".join(acts) if acts else "Không có artefact phù hợp để dọn"
                stage(OK, "Dọn nhẹ", f"{detail} · giải phóng {fmt_size(freed)}")
            except OSError as exc:
                stage(BAD, "Dọn nhẹ", str(exc))
            continue
        if k == "2":
            if sz_target == 0:
                stage(OK, "Dọn sâu", "target/ đã trống (0 B)")
                continue
            if TARGET_DIR.is_symlink():
                stage(BAD, "Dọn sâu", "target/ là symlink; từ chối xóa để tránh tác động ngoài dự án.")
                continue
            if ask_yes(f"Xóa toàn bộ target/ để thu hồi {fmt_size(sz_target)}? Lần build sau sẽ lâu hơn.", default=False):
                started = time.perf_counter()
                Live.show("Đang dọn target/…", throttle=0.0)
                try:
                    if shutil.which("cargo"):
                        subprocess.run(["cargo", "clean"], cwd=ROOT, capture_output=True, timeout=120)
                    if TARGET_DIR.exists() and not TARGET_DIR.is_symlink():
                        shutil.rmtree(TARGET_DIR)
                    Live.end()
                    stage(OK, "Dọn sâu", f"Đã xử lý {fmt_size(sz_target)} trong {time.perf_counter() - started:.1f}s")
                except (OSError, subprocess.TimeoutExpired) as exc:
                    Live.end()
                    stage(BAD, "Dọn sâu", str(exc))
            continue
        stage(WARN, "Lệnh", "Nhập 1, 2 hoặc q rồi nhấn Enter.")



# ════════════════════════════════════════════════════════════════════════════
#  CARGO TEST -> 1 DÒNG KẾT LUẬN NGẮN GỌN
# ════════════════════════════════════════════════════════════════════════════
@dataclass
class Diag:
    level: str
    code: str
    file: str
    line: int
    msg: str
    rendered: str

    def short(self) -> str:
        loc = f"{self.file}:{self.line}" if self.file else "(cargo)"
        code = f" [{self.code}]" if self.code and self.code[0] in "EW" else ""
        return f"{loc}  {self.msg}{code}"


@dataclass
class Outcome:
    rc: int = 0
    secs: float = 0.0
    passed: int = 0
    failed: int = 0
    ignored: int = 0
    errors: List[Diag] = field(default_factory=list)
    warnings: List[Diag] = field(default_factory=list)
    failed_names: List[str] = field(default_factory=list)
    fail_blocks: List[str] = field(default_factory=list)
    tail: List[str] = field(default_factory=list)

    @property
    def clean(self) -> bool:
        return self.rc == 0 and not self.errors and not self.warnings and self.failed == 0

    @property
    def blocking(self) -> bool:
        return bool(self.errors) or self.failed > 0 or self.rc != 0

    def one_liner(self) -> Tuple[str, str]:
        e, w, t = len(self.errors), len(self.warnings), self.passed + self.failed
        secs = f" · {self.secs:.0f}s"
        if e:
            return BAD, f"{e} lỗi biên dịch · {w} cảnh báo · test chưa chạy{secs}"
        if self.failed:
            return BAD, f"{self.failed}/{t} test thất bại · {w} cảnh báo{secs}"
        if self.rc != 0:
            return BAD, f"cargo kết thúc với mã {self.rc}{secs}"
        if w:
            return WARN, f"{self.passed} test đạt · 0 lỗi · {w} cảnh báo{secs}"
        return OK, f"{self.passed} test đạt · 0 lỗi · 0 cảnh báo{secs}"


class CargoParser:
    RESULT = re.compile(r"^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored")
    SUMMARY = re.compile(r"^(?:aborting due to|\d+ (?:warnings?|errors?) emitted|could not compile)")
    IGNORE_ERR = re.compile(r"^error: (?:test failed|could not compile|\d+ targets? failed|aborting)")

    def __init__(self) -> None:
        self.errors: Dict[tuple, Diag] = {}
        self.warnings: Dict[tuple, Diag] = {}
        self.passed = self.failed = self.ignored = 0
        self.failed_names: List[str] = []
        self.fail_blocks: List[str] = []
        self.cargo_errs: List[str] = []
        self.cargo_warns: List[str] = []
        self.tail: deque = deque(maxlen=60)
        self.progress = "khởi động"
        self.tests_seen = 0
        self._blk: Optional[List[str]] = None

    def _end_blk(self) -> None:
        if self._blk:
            self.fail_blocks.append("\n".join(self._blk).rstrip())
        self._blk = None

    def feed(self, line: str) -> None:
        if line.startswith("{"):
            try:
                obj = json.loads(line)
            except ValueError:
                obj = None
            if isinstance(obj, dict):
                self._json(obj)
                return
        self.tail.append(line)
        m = self.RESULT.match(line)
        if m:
            self.passed += int(m.group(1))
            self.failed += int(m.group(2))
            self.ignored += int(m.group(3))
            self._end_blk()
            return
        m = re.match(r"^---- (.+) stdout ----$", line)
        if m:
            self._end_blk()
            self._blk = [line]
            return
        if self._blk is not None:
            if line.strip() == "failures:":
                self._end_blk()
            elif not line.startswith("note: run with `RUST_BACKTRACE"):
                self._blk.append(line)
            return
        st = line.strip()
        if st.startswith("Running "):
            self.progress = "chạy " + st[8:].split(" (")[0]
        elif st.startswith("Compiling "):
            self.progress = st.replace("Compiling ", "biên dịch ").split(" (")[0]
        elif st.startswith("test "):
            self.tests_seen += 1
            self.progress = f"đã chạy {self.tests_seen} test"
            if st.endswith("FAILED"):
                name = st[5:].rsplit(" ... ", 1)[0]
                if name not in self.failed_names:
                    self.failed_names.append(name)
        elif line.startswith("error") and not self.IGNORE_ERR.match(line):
            self.cargo_errs.append(line)
        elif line.startswith("warning:") and "generated" not in line:
            self.cargo_warns.append(line)

    def _json(self, obj: dict) -> None:
        reason = obj.get("reason")
        if reason == "compiler-artifact":
            self.progress = "biên dịch " + str((obj.get("target") or {}).get("name", ""))
            return
        if reason != "compiler-message":
            return
        m = obj.get("message") or {}
        level = m.get("level")
        if level not in ("error", "warning"):
            return
        text = m.get("message", "")
        spans = m.get("spans") or []
        if not spans and self.SUMMARY.match(text):
            return
        prim = next((s for s in spans if s.get("is_primary")), spans[0] if spans else None)
        file = (prim or {}).get("file_name", "").replace("\\", "/")
        ln = (prim or {}).get("line_start", 0)
        col = (prim or {}).get("column_start", 0)
        code = (m.get("code") or {}).get("code", "") or ""
        d = Diag(level, code, file, ln, text, m.get("rendered") or text)
        key = (level, code, text, file, ln, col)
        (self.errors if level == "error" else self.warnings).setdefault(key, d)

    def outcome(self, rc: int, secs: float) -> Outcome:
        self._end_blk()
        o = Outcome(
            rc=rc, secs=secs,
            passed=self.passed, failed=self.failed, ignored=self.ignored,
            errors=list(self.errors.values()),
            warnings=list(self.warnings.values()),
            failed_names=self.failed_names,
            fail_blocks=self.fail_blocks,
            tail=list(self.tail),
        )
        for ln in self.cargo_errs:
            o.errors.append(Diag("error", "", "", 0, ln.strip(), ln))
        for ln in self.cargo_warns:
            o.warnings.append(Diag("warning", "", "", 0, ln.strip(), ln))
        if o.failed == 0 and o.failed_names:
            o.failed = len(o.failed_names)
        if rc != 0 and not o.errors and not o.failed:
            o.errors.append(Diag(
                "error", "", "", 0,
                f"cargo kết thúc với mã {rc} (xem log bên dưới)",
                "\n".join(o.tail[-40:]),
            ))
        return o


def run_stream(cmd: List[str], on_line: Callable[[str], None], env: Optional[dict] = None) -> int:
    p = subprocess.Popen(
        cmd, cwd=ROOT,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, encoding="utf-8", errors="replace", bufsize=1, env=env,
    )
    try:
        assert p.stdout is not None
        for raw in p.stdout:
            on_line(raw.rstrip("\r\n"))
        return p.wait()
    except KeyboardInterrupt:
        p.terminate()
        try:
            p.wait(timeout=5)
        except Exception:
            p.kill()
        raise


def run_cargo_test() -> Outcome:
    parser = CargoParser()
    t0 = time.perf_counter()
    env = dict(os.environ, CARGO_TERM_COLOR="never", RUST_BACKTRACE="0")

    def on_line(line: str) -> None:
        parser.feed(line)
        Live.show(f"cargo test · {parser.progress} · {time.perf_counter() - t0:.1f}s")

    rc = run_stream(["cargo", "test", "--workspace", "--message-format=json"], on_line, env)
    Live.end()
    return parser.outcome(rc, time.perf_counter() - t0)


def build_report(o: Outcome) -> str:
    sym, line = o.one_liner()
    out = [
        f"=== BÁO CÁO cargo test · BSE-AutoRouter · {datetime.now():%Y-%m-%d %H:%M:%S} ===",
        re.sub(r"\x1b\[[0-9;]*m", "", f"{sym} {line}"),
        "",
    ]
    if o.errors:
        out += [f"--- LỖI ({len(o.errors)}) ---"] + [d.rendered.rstrip() + "\n" for d in o.errors]
    if o.fail_blocks or o.failed_names:
        out += [f"--- TEST THẤT BẠI ({len(o.failed_names) or o.failed}) ---"]
        out += o.fail_blocks or [f"test {n} ... FAILED" for n in o.failed_names]
        out.append("")
    if o.warnings:
        out += [f"--- CẢNH BÁO ({len(o.warnings)}) ---"] + [d.rendered.rstrip() + "\n" for d in o.warnings]
    return "\n".join(out).rstrip() + "\n"


def show_problems(o: Outcome) -> None:
    items: List[Tuple[str, str]] = [(red("E"), d.short()) for d in o.errors]
    for n in o.failed_names:
        blk = next((b for b in o.fail_blocks if b.startswith(f"---- {n} stdout")), "")
        why = next((l.strip() for l in blk.split("\n")[1:] if l.strip() and not l.startswith("thread ")), "")
        loc = next((l for l in blk.split("\n") if l.startswith("thread ")), "")
        m = re.search(r"panicked at (\S+?):?$", loc)
        items.append((red("T"), f"{n}" + (f"  @ {m.group(1)}" if m else "") + (f"  — {why}" if why else "")))
    items += [(yellow("W"), d.short()) for d in o.warnings]
    for sym, text in items[:MAX_SHOW]:
        sub(text, sym)
    if len(items) > MAX_SHOW:
        sub(dim(f"… và {len(items) - MAX_SHOW} mục nữa — bấm [C] để copy đầy đủ"))


# ════════════════════════════════════════════════════════════════════════════
#  KIỂM TRA HỢP ĐỒNG GIAO TIẾP (main.html <-> crates/bse-wasm)
# ════════════════════════════════════════════════════════════════════════════
@dataclass
class Issue:
    level: str  # "error" | "warn"
    line: int
    msg: str
    fixable: bool = False


def _find_matching_brace(src: str, open_brace_pos: int) -> int:
    depth = 0
    for i in range(open_brace_pos, len(src)):
        ch = src[i]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return i
    return len(src)


def parse_rust_exports(src: str) -> dict:
    impls = []
    for m in re.finditer(r"#\[wasm_bindgen\][ \t]*\n[ \t]*impl\s+(\w+)\s*\{", src):
        end = _find_matching_brace(src, m.end() - 1)
        impls.append((m.group(1), m.end(), end))
    classes: Dict[str, dict] = {}
    for cls, s, e in impls:
        info = classes.setdefault(cls, {"inst": set(), "static": set(), "ctor": False})
        pat = r"((?:[ \t]*(?:#\[[^\n]*\]|///[^\n]*)\n)*)[ \t]*pub fn (\w+)\s*\(\s*(&\s*mut\s+self\b|&\s*self\b|mut\s+self\b|self\b)?"
        for fm in re.finditer(pat, src[s:e]):
            attrs, name, selfarg = fm.group(1), fm.group(2), fm.group(3)
            js = re.search(r"js_name\s*=\s*(\w+)", attrs)
            name = js.group(1) if js else name
            if "constructor" in attrs:
                info["ctor"] = True
            else:
                info["inst" if selfarg else "static"].add(name)
    free = set()
    for fm in re.finditer(r"#\[wasm_bindgen(?:\(([^)]*)\))?\][ \t]*\n(?:[ \t]*(?:///|#\[)[^\n]*\n)*[ \t]*pub fn (\w+)", src):
        if any(s <= fm.start() < e for _, s, e in impls):
            continue
        js = re.search(r"js_name\s*=\s*(\w+)", fm.group(1) or "")
        free.add(js.group(1) if js else fm.group(2))
    return {"classes": classes, "free": free}


def rust_struct_fields(name: str) -> Optional[set]:
    for rel in walk_sources():
        if not rel.endswith(".rs"):
            continue
        src = read_text(ROOT / rel)
        m = re.search(rf"((?:[ \t]*#\[[^\n]*\]\n)*)[ \t]*pub struct {name}\s*\{{(.*?)\n\}}", src, re.S)
        if m:
            camel = "camelCase" in m.group(1)
            names = re.findall(r"^\s*pub (\w+)\s*:", m.group(2), re.M)
            return {re.sub(r"_([a-z0-9])", lambda x: x.group(1).upper(), n) if camel else n for n in names}
    return None


def check_contract() -> List[Issue]:
    if not HTML.exists():
        return [Issue("error", 0, "Không thấy main.html")]
    lib = WASM_CRATE / "src" / "lib.rs"
    if not lib.exists():
        return [Issue("error", 0, "Không thấy crates/bse-wasm/src/lib.rs")]
    html, ex = read_text(HTML), parse_rust_exports(read_text(lib))
    ln = lambda pos: html.count("\n", 0, pos) + 1
    issues: List[Issue] = []

    if not re.search(r"<script[^>]+src=[\"']wasm-bridge\.js[\"']", html):
        issues.append(Issue(
            "error", 0,
            'main.html thiếu thẻ <script src="wasm-bridge.js"></script> (đặt trước </body>)',
            fixable=("</body>" in html),
        ))

    def suggest(name: str, pool: set) -> str:
        m = difflib.get_close_matches(name, sorted(pool), n=1, cutoff=0.6)
        return f" — ý bạn là `{m[0]}`?" if m else ""

    seen = set()
    for m in re.finditer(r"wasm_bindgen\.(\w+)\.(\w+)\s*\(", html):
        cls, meth = m.group(1), m.group(2)
        info = ex["classes"].get(cls)
        if (cls, meth) in seen:
            continue
        seen.add((cls, meth))
        if info is None:
            issues.append(Issue("error", ln(m.start()), f"`{cls}` không được export từ Rust"))
        elif meth == "new" and info["ctor"] and "new" not in info["static"]:
            issues.append(Issue(
                "warn", ln(m.start()),
                f"`{cls}.new()` không tồn tại → bấm [L] để đổi thành `new wasm_bindgen.{cls}(...)`",
                fixable=True,
            ))
        elif meth not in info["static"]:
            issues.append(Issue("error", ln(m.start()), f"`{cls}.{meth}()` không có trong Rust{suggest(meth, info['static'])}"))

    for m in re.finditer(r"(new\s+)?wasm_bindgen\.(\w+)\s*\(", html):
        is_new = bool(m.group(1))
        name = m.group(2)
        if name in ex["classes"]:
            if not is_new and not ex["classes"][name]["ctor"]:
                issues.append(Issue("error", ln(m.start()), f"`{name}` là class, cần gọi `new wasm_bindgen.{name}(...)`"))
            continue
        if name not in ex["free"]:
            issues.append(Issue("error", ln(m.start()), f"hàm `{name}()` không có trong Rust{suggest(name, ex['free'])}"))

    solver = ex["classes"].get("WasmSolver")
    if solver:
        allowed = solver["inst"] | {"free"}
        for var in set(re.findall(r"\b(\w+)\s*=\s*(?:new\s+)?wasm_bindgen\.WasmSolver\b", html)):
            seen_m = set()
            for m in re.finditer(rf"\b{var}\.(\w+)\s*\(", html):
                if m.group(1) not in allowed and m.group(1) not in seen_m:
                    seen_m.add(m.group(1))
                    issues.append(Issue("error", ln(m.start()),
                                        f"`{var}.{m.group(1)}()` không có trong Rust{suggest(m.group(1), solver['inst'])}"))

    # Kiểm tra dòng break sớm ở 0 lỗi trong main.html
    m_brk = re.search(r"if\s*\(\s*stepRes\.bestErr(?:or)?Count\s*===\s*0\s*&&\s*budget\s*!==\s*-1\s*\)\s*break", html)
    if m_brk:
        issues.append(Issue(
            "warn", ln(m_brk.start()),
            "Có lệnh `break` ngắt sớm khi đạt 0 lỗi → bấm [L] để dọn, giúp bộ giải chạy hết thời gian tối ưu thẩm mỹ",
            fixable=True,
        ))
    return issues


def show_issues(issues: List[Issue]) -> None:
    for i in issues:
        loc = f"dòng {i.line}: " if i.line else ""
        sub(f"{loc}{i.msg}", red("E") if i.level == "error" else yellow("W"))


# ════════════════════════════════════════════════════════════════════════════
#  BIÊN DỊCH WASM & ĐÓNG GÓI wasm-bridge.js (CHẠY OFFLINE file:///)
# ════════════════════════════════════════════════════════════════════════════
BRIDGE_TEMPLATE = r"""/* ============================================================================
 * BSE-AutoRouter · wasm-bridge.js
 * FILE SINH TỰ ĐỘNG bởi bse.py — KHÔNG SỬA TAY (sẽ bị ghi đè ở lần build sau).
 * build:@@TIME@@ · src:@@HASH@@ · wasm:@@WASMKB@@ KB
 * Chạy trực tiếp bằng file:/// (WASM nhúng Base64, không cần server).
 * Tự động kết nối WasmSolver với LiveControl, SearchLogger, KnowledgeBank & SteerMode.
 * ========================================================================== */
@@GLUE@@

;(function () {
    'use strict';
    if (typeof wasm_bindgen !== 'function') {
        console.error('[BSE] Glue không định nghĩa wasm_bindgen — cần build với --target no-modules');
        return;
    }
    var B64 = "@@B64@@";
    var raw = wasm_bindgen, bytes = null, booting = null, shimmed = false;
    function decode() {
        if (bytes) return bytes;
        var bin = atob(B64), n = bin.length, u8 = new Uint8Array(n);
        for (var i = 0; i < n; i++) u8[i] = bin.charCodeAt(i);
        return (bytes = u8);
    }
    function bindUiHooks(solver) {
        if (!solver || typeof document === 'undefined') return solver;
        try {
            var steerSel = document.getElementById('steer-mode');
            if (steerSel && typeof solver.set_steer_mode === 'function') {
                solver.set_steer_mode(steerSel.value || 'auto');
                if (!steerSel._bseBound) {
                    steerSel._bseBound = true;
                    steerSel.addEventListener('change', function () {
                        if (window._bseActiveSolver) window._bseActiveSolver.set_steer_mode(steerSel.value || 'auto');
                    });
                }
            }
            window._bseActiveSolver = solver;
            if (typeof LiveControl !== 'undefined' && LiveControl) {
                LiveControl.triggerTabuJump = function () {
                    if (!window._bseActiveSolver) return;
                    var tab = (typeof curTab === 'number') ? curTab : 0;
                    window._bseActiveSolver.trigger_tabu_jump(tab);
                    var p = document.getElementById('prog');
                    if (p) p.textContent = '⚡ [Rust+WASM] Đã khóa Tabu bố cục hiện tại & nhảy sang khung IC mới!';
                };
                LiveControl.triggerDeepSurgery = function () {
                    if (!window._bseActiveSolver) return;
                    var tab = (typeof curTab === 'number') ? curTab : 0;
                    window._bseActiveSolver.trigger_deep_surgery(tab);
                    var p = document.getElementById('prog');
                    if (p) p.textContent = '🔬 [Rust+WASM] Đã bơm 6 ca Đại phẫu R-LED cho PA ' + (tab + 1) + '!';
                };
            }
        } catch (_) {}
        return solver;
    }
    function syncLoggerAndKb(solver) {
        if (!solver || typeof window === 'undefined') return;
        try {
            if (typeof SearchLogger !== 'undefined' && SearchLogger) {
                if (typeof solver.get_log_report === 'function') SearchLogger._wasmReport = solver.get_log_report();
                if (typeof solver.get_log_json === 'function') SearchLogger.data = JSON.parse(solver.get_log_json());
            }
            if (typeof KnowledgeBank !== 'undefined' && KnowledgeBank && typeof solver.get_kb_json === 'function') {
                var kbObj = JSON.parse(solver.get_kb_json() || '{}');
                if (Array.isArray(kbObj.seeds)) {
                    kbObj.seeds.forEach(function (s) { KnowledgeBank.addSeed(s, false); });
                    if (typeof KnowledgeBank.saveToStorage === 'function') KnowledgeBank.saveToStorage();
                }
            }
        } catch (_) {}
    }
    function installShims() {
        if (shimmed || !raw.WasmSolver) return;
        shimmed = true;
        var WS = raw.WasmSolver;
        var proto = WS.prototype;
        if (typeof WS.new !== 'function') {
            WS.new = function (compsJson, netlistTxt, useStarHub, seed, kbJson) {
                return bindUiHooks(new WS(compsJson, netlistTxt, useStarHub, seed, kbJson));
            };
        }
        if (typeof WS.new_with_sym_led === 'function') {
            var origNewSym = WS.new_with_sym_led;
            WS.new_with_sym_led = function (compsJson, netlistTxt, symLedSpec, useStarHub, seed, kbJson) {
                if ((kbJson === null || kbJson === undefined) && typeof KnowledgeBank !== 'undefined' && KnowledgeBank && Array.isArray(KnowledgeBank.seeds) && KnowledgeBank.seeds.length) {
                    try { kbJson = JSON.stringify({ archetypes: {}, seeds: KnowledgeBank.seeds }); } catch (_) {}
                }
                return bindUiHooks(origNewSym.call(WS, compsJson, netlistTxt, symLedSpec, useStarHub, seed, kbJson));
            };
        }
        if (proto && typeof proto.get_top_solutions === 'function') {
            var origTop = proto.get_top_solutions;
            proto.get_top_solutions = function (k) {
                var res = origTop.call(this, k);
                syncLoggerAndKb(this);
                return res;
            };
            if (typeof proto.get_top_solutions_json !== 'function') {
                proto.get_top_solutions_json = proto.get_top_solutions;
            }
        }
        if (proto && typeof proto.get_kb_json === 'function' && typeof proto.get_kb_seeds_json !== 'function') {
            proto.get_kb_seeds_json = function () {
                try {
                    var kb = JSON.parse(this.get_kb_json() || '{}');
                    return JSON.stringify(kb.seeds || []);
                } catch (_) {
                    return '[]';
                }
            };
        }
    }
    function boot() {
        if (!booting) {
            booting = Promise.resolve(raw(@@INIT_ARG@@)).then(function (val) {
                installShims();
                return val;
            }).catch(function (e) {
                booting = null;
                throw e;
            });
        }
        return booting;
    }
    installShims();
    wasm_bindgen = new Proxy(raw, {
        apply: function (target, thisArg, args) {
            return args.length ? Reflect.apply(target, thisArg, args) : boot();
        }
    });
    if (typeof window !== 'undefined') window.wasm_bindgen = wasm_bindgen;
    if (typeof globalThis !== 'undefined') globalThis.wasm_bindgen = wasm_bindgen;
    try { window.BSE_BUILD = { built: "@@TIME@@", src: "@@HASH@@", wasmBytes: @@WASMLEN@@ }; } catch (_) {}
})();
"""

SMOKE_JS = r"""
const fs = require('fs'), vm = require('vm');
vm.runInThisContext(fs.readFileSync(process.argv[2], 'utf8'), { filename: 'wasm-bridge.js' });
(async () => {
  const wb = vm.runInThisContext('wasm_bindgen');
  await wb();
  const comps = JSON.stringify([
    {id:'U1',type:'IC',pins:14,rot:0,pwrMode:'default',vccPin:14,gndPin:7},
    {id:'B1',type:'BTN2',pins:2},{id:'R1',type:'R',pins:2},{id:'R2',type:'R',pins:2},
    {id:'L1',type:'LED',pins:2},{id:'L2',type:'LED',pins:2}]);
  const net = 'VCC U1:14; GND U1:7\nU1:12 -> R1 -> L1 -> GND\nU1:3 -> R2 -> L2 -> GND\nB1:1 -> R1:1; B1:2 -> GND';
  const s = wb.WasmSolver.new_with_sym_led(comps, net, 'L1=L2', true, 42, null);
  const step = JSON.parse(s.step_batch(3, 0));
  const top = JSON.parse(s.get_top_solutions(1));
  if (!(step.iterations >= 1)) throw new Error('step_batch không trả về iterations');
  if (!Array.isArray(top) || top.length < 1) throw new Error('get_top_solutions trả về rỗng');
  console.log(JSON.stringify({ iterations: step.iterations, errors: top[0].errors.length }));
})().catch(e => { console.error(String(e && e.stack || e)); process.exit(2); });
"""


def build_wasm() -> Tuple[bool, float, List[str]]:
    if PKG.is_symlink():
        return False, 0.0, ["Từ chối build vì pkg/ là symlink; hãy đổi nó thành thư mục thật bên trong dự án."]
    if PKG.exists() and not PKG.is_dir():
        return False, 0.0, ["Từ chối build vì pkg/ tồn tại nhưng không phải thư mục."]
    if shutil.which("wasm-pack") is None:
        return False, 0.0, ["Không tìm thấy 'wasm-pack'. Cài bằng:  cargo install wasm-pack"]
    t0 = time.perf_counter()
    build_started_at = time.time()  # so sánh với st_mtime (Unix epoch), không dùng đồng hồ đơn điệu
    tail: deque = deque(maxlen=60)

    def on_line(line: str) -> None:
        tail.append(line)
        if line.strip():
            Live.show(f"wasm-pack · {line.strip()} · {time.perf_counter() - t0:.1f}s")

    rc = run_stream(["wasm-pack"] + WASM_ARGS, on_line)
    Live.end()
    wasm, glue = PKG / "bse_wasm_bg.wasm", PKG / "bse_wasm.js"
    if rc != 0:
        return False, time.perf_counter() - t0, list(tail)
    if not wasm.is_file() or not glue.is_file() or wasm.is_symlink() or glue.is_symlink() or min(wasm.stat().st_mtime, glue.stat().st_mtime) < build_started_at - 2:
        return False, time.perf_counter() - t0, list(tail) + ["pkg/ không có file mới sau khi build (wasm-pack không xuất file?)"]
    return True, time.perf_counter() - t0, list(tail)


def make_bridge(src_hash: str) -> Tuple[bool, str, int]:
    if PKG.is_symlink():
        return False, "Từ chối đọc pkg/ vì đường dẫn là symlink.", 0
    wasm_path, glue_path = PKG / "bse_wasm_bg.wasm", PKG / "bse_wasm.js"
    if wasm_path.is_symlink() or glue_path.is_symlink():
        return False, "Từ chối đọc artefact WASM qua symlink trong pkg/.", 0
    if not wasm_path.is_file() or not glue_path.is_file():
        return False, "Thiếu artefact WASM trong pkg/.", 0
    wasm = wasm_path.read_bytes()
    glue = read_text(glue_path)
    if re.search(r"^\s*export\s", glue, re.M) or "import.meta" in glue:
        return False, "pkg/bse_wasm.js là ES module — cần build với --target no-modules", 0
    if not re.search(r"\bwasm_bindgen\b", glue):
        return False, "pkg/bse_wasm.js không có biến wasm_bindgen — kiểm tra phiên bản wasm-pack", 0
    init_arg = "{ module_or_path: decode() }" if "module_or_path" in glue else "decode()"
    text = (
        BRIDGE_TEMPLATE
        .replace("@@GLUE@@", glue.rstrip())
        .replace("@@INIT_ARG@@", init_arg)
        .replace("@@TIME@@", datetime.now().strftime("%Y-%m-%d %H:%M:%S"))
        .replace("@@HASH@@", src_hash)
        .replace("@@WASMKB@@", str(round(len(wasm) / 1024)))
        .replace("@@WASMLEN@@", str(len(wasm)))
        .replace("@@B64@@", base64.b64encode(wasm).decode("ascii"))
    )
    write_atomic(BRIDGE, text.encode("utf-8"))
    # Dọn sạch thư mục tạm pkg/ ngay sau khi đã nhúng vào wasm-bridge.js; không theo symlink.
    if PKG.exists() and not PKG.is_symlink():
        shutil.rmtree(PKG, ignore_errors=True)
    return True, "", len(wasm)


def smoke_test() -> Tuple[str, str]:
    node = shutil.which("node")
    if not node:
        return "skip", "không có Node.js — bỏ qua chạy thử (cài Node để tự kiểm tra engine)"
    with tempfile.NamedTemporaryFile("w", suffix=".js", delete=False, encoding="utf-8") as f:
        f.write(SMOKE_JS)
        tmp = f.name
    try:
        r = subprocess.run(
            [node, tmp, str(BRIDGE)],
            capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=120,
        )
    except subprocess.TimeoutExpired:
        return "fail", "engine không phản hồi sau 120s"
    finally:
        try:
            os.unlink(tmp)
        except OSError:
            pass
    if r.returncode != 0:
        return "fail", (r.stderr or r.stdout).strip().split("\n")[0][:300]
    try:
        d = json.loads(r.stdout.strip().split("\n")[-1])
        ver = subprocess.run([node, "-v"], capture_output=True, text=True).stdout.strip()
        return "ok", f"solver chạy tốt ({d['iterations']} vòng thử) · Node {ver}"
    except Exception:
        return "ok", "solver chạy tốt"


# ════════════════════════════════════════════════════════════════════════════
#  LIVE WATCHER: TỰ ĐỘNG CHẠY LẠI KHI LƯU FILE BUNDLE HOẶC main.html
# ════════════════════════════════════════════════════════════════════════════
class Watcher:
    def __init__(self) -> None:
        self.mark()

    @staticmethod
    def _sig() -> tuple:
        out = []
        for p in (BUNDLE, HTML):
            try:
                st = p.stat()
                out.append((st.st_mtime_ns, st.st_size))
            except OSError:
                out.append(None)
        return tuple(out)

    def mark(self) -> None:
        self.sig = self._sig()
        self.hashes = (file_sha(BUNDLE), file_sha(HTML))

    def changed(self) -> bool:
        s = self._sig()
        if s == self.sig:
            return False
        for _ in range(8):
            time.sleep(0.7)
            s2 = self._sig()
            if s2 == s:
                break
            s = s2
        self.sig = s
        h = (file_sha(BUNDLE), file_sha(HTML))
        if h == self.hashes:
            return False
        self.hashes = h
        return True


# ════════════════════════════════════════════════════════════════════════════
#  ĐIỀU PHỐI PIPELINE TỰ ĐỘNG AIO
# ════════════════════════════════════════════════════════════════════════════
@dataclass
class Result:
    status: str = "fail"      # ok | warn | fail
    phase: str = "prepare"    # prepare | cargo | ship
    blocking: bool = False
    ocd_pending: bool = False
    report: str = ""
    src_hash: str = ""

    @property
    def usable(self) -> bool:
        return self.phase == "ship" and self.status in ("ok", "warn")


def banner() -> None:
    print()
    print(bold("  BSE-AutoRouter AIO") + dim(" · Đồng bộ · Kiểm toán OCD · Sao lưu · Đóng gói WASM"))
    print(dim("  " + "─" * max(20, term_width() - 2)))


def save_report(text: str) -> None:
    ensure_work_directory()
    if REPORT_FILE.is_symlink():
        raise OSError("Từ chối ghi last_report.txt vì đây là symlink.")
    write_atomic(REPORT_FILE, text.encode("utf-8"))



def phase_prepare() -> Optional[Plan]:
    if not BUNDLE.exists():
        stage(BAD, "Bundle", f"không thấy {BUNDLE.name}")
        return None
    plan = plan_unpack(read_text(BUNDLE))
    if not plan.contents:
        stage(BAD, "Bundle", f"không có khối '{MARKER.strip()} ... ===' nào trong {BUNDLE.name}")
        return None
    extra = []
    if plan.dups:
        extra.append(f"{sum(v - 1 for v in plan.dups.values())} khối trùng → lấy khối cuối")
    if plan.fences:
        extra.append(f"gỡ {plan.fences} dấu ``` thừa")
    stage(
        INFO, "Bundle",
        f"{len(plan.contents)} file · {len(plan.new) + len(plan.changed)} cần cập nhật · "
        f"{len(plan.same)} không đổi" + (" · " + " · ".join(extra) if extra else ""),
    )
    for b in plan.bad:
        sub(f"bỏ qua đường dẫn không hợp lệ/không phải .rs .toml: {b}", yellow("W"))
    todo = plan.new + plan.changed
    if todo:
        try:
            snap = make_snapshot("truoc-cap-nhat")
        except Exception as exc:
            stage(BAD, "Sao lưu", f"Không thể tạo backup trước khi cập nhật: {exc}")
            return None
        if snap is None or not snap.exists():
            stage(BAD, "Sao lưu", "Không tạo được backup; đã hủy cập nhật để tránh mất dữ liệu.")
            return None
        stage(OK, "Sao lưu", f"{snap.relative_to(ROOT).as_posix()} · {fmt_size(snap.stat().st_size)}")
        try:
            apply_unpack(plan)
        except Exception as exc:
            stage(BAD, "Giải nén", f"Không thể áp dụng bundle: {exc} · đã có backup để khôi phục.")
            return None
        stage(OK, "Giải nén", f"{len(todo)} file: {short_names(todo)}")
    else:
        stage(SKIP, "Giải nén", "bundle không có thay đổi mới so với mã trên đĩa")

    # Tự động dọn vết vá #[allow(unused_variables)] + biến thừa fps trong builder.rs
    rust_fixes = sanitize_rust_ocd()
    html_fixes = sanitize_html_ocd()
    if rust_fixes or html_fixes:
        stage(OK, "Chuẩn OCD", " · ".join(rust_fixes + html_fixes))

    allow_left = audit_rust_ocd()
    if allow_left:
        stage(WARN, "Kiểm toán OCD", f"phát hiện {len(allow_left)} cờ #[allow(unused_...)] vá tạm trong mã Rust")
        for rel, ln_no, attr in allow_left[:4]:
            sub(f"{rel}:{ln_no}  {attr}", yellow("W"))
    return plan


def phase_test(opts, src_hash: str) -> Tuple[Optional[Outcome], bool]:
    fresh = (not opts.force) and state_load().get("last_ok_src") == src_hash and bridge_hash() == src_hash
    if fresh:
        stage(SKIP, "cargo test", "mã Rust không đổi kể từ lần build OK trước (bấm [F] để ép chạy lại)")
        return None, True
    if shutil.which("cargo") is None:
        stage(BAD, "cargo test", "không tìm thấy 'cargo' — cài Rust từ https://rustup.rs")
        return Outcome(rc=127, errors=[Diag("error", "", "", 0, "không tìm thấy 'cargo'", "")]), False
    o = run_cargo_test()
    sym, line = o.one_liner()
    stage(sym, "cargo test", line)
    if not o.clean:
        show_problems(o)
    # Lưu trạng thái test hiện tại vào state + tự động chụp bản nháp kể cả khi có lỗi!
    st = state_load()
    st["last_status_src"] = src_hash
    st["last_status"] = "ok" if o.clean else ("fail" if o.blocking else "warn")
    st["last_summary"] = re.sub(r"\x1b\[[0-9;]*m", "", line)
    state_save(st)
    if not o.clean:
        make_snapshot("ban-nhap-loi", status=st["last_status"], summary=st["last_summary"])
    return o, False


def phase_ship(opts, src_hash: str, skipped: bool) -> Result:
    res = Result(phase="ship", src_hash=src_hash)
    try:
        old, new, n, wrote = pack_bundle()
        stage(OK, "Pack bundle", f"{n} file · {fmt_size(old)} → {fmt_size(new)}" + ("" if wrote else " (đã gọn sẵn)"))
    except OSError as e:
        stage(WARN, "Pack bundle", f"không ghi được bundle ({e}) — đang mở ở chương trình khác?")

    if skipped and bridge_hash() == src_hash and BRIDGE.exists():
        stage(SKIP, "WASM", "bridge đã khớp mã Rust hiện tại — không cần build lại")
    else:
        ok, secs, tail = build_wasm()
        if not ok:
            stage(BAD, "WASM", f"build thất bại ({secs:.1f}s)")
            for l in [t for t in tail if t.strip()][-6:]:
                sub(l.strip(), dim("│"))
            res.report = "=== LỖI wasm-pack build ===\n" + "\n".join(tail) + "\n"
            return res
        ok, why, wlen = make_bridge(src_hash)
        if not ok:
            stage(BAD, "Bridge", why)
            res.report = f"=== LỖI tạo wasm-bridge.js ===\n{why}\n"
            return res
        stage(OK, "WASM", f"release · {fmt_size(wlen)} · {secs:.1f}s (đã tự dọn sạch pkg/)")
        stage(OK, "Bridge", f"{BRIDGE.name} · {fmt_size(BRIDGE.stat().st_size)} · nhúng sẵn WASM (chạy trực tiếp file:///)")

    issues = check_contract()
    errs = [i for i in issues if i.level == "error"]
    res.ocd_pending = any(i.fixable for i in issues) or bool(audit_rust_ocd())
    if errs:
        stage(BAD, "Khớp HTML", f"main.html ↔ Rust: {len(errs)} lỗi")
    elif issues:
        stage(WARN, "Khớp HTML", f"main.html ↔ Rust: chạy được nhưng có {len(issues)} điểm lệch nhẹ")
    else:
        stage(OK, "Khớp HTML", "main.html ↔ Rust ↔ wasm-bridge.js ăn khớp hoàn hảo")
    show_issues(issues)

    status, msg = smoke_test()
    stage({"ok": OK, "skip": SKIP, "fail": BAD}[status], "Chạy thử", msg)

    if errs or status == "fail":
        res.report = "=== LỖI KIỂM TRA SAU BUILD ===\n" + "\n".join(
            [f"main.html dòng {i.line}: {i.msg}" for i in issues]
            + ([f"Chạy thử: {msg}"] if status == "fail" else [])
        ) + "\n"
        return res

    res.status = "warn" if issues else "ok"
    st = state_load()
    st["last_ok_src"] = src_hash
    st["last_ok_time"] = datetime.now().isoformat(timespec="seconds")
    st["last_status_src"] = src_hash
    st["last_status"] = res.status
    state_save(st)
    if not any((s.get("status") == "ok" or s.get("good")) and s.get("src_hash") == src_hash for s in list_snapshots()):
        make_snapshot("ban-chay-tot", status="ok", summary="Build & Smoke Test thành công")
    return res


def finish_line(res: Result, t0: float, note: str = "") -> None:
    print(dim("  " + "─" * max(20, term_width() - 2)))
    secs = f"{time.perf_counter() - t0:.1f}s"
    if res.status == "ok":
        print(f"  {OK} {bold('HOÀN TẤT 100%')} · {secs}{note} — bấm {bold('[O]')} hoặc mở {bold('main.html')} là chạy ngay.")
    elif res.status == "warn":
        print(f"  {WARN} {bold('XONG (chạy được ngay, có nhắc nhở nhẹ ở trên)')} · {secs}{note}.")


def pipeline_once(opts) -> Tuple[Result, Optional[Outcome]]:
    t0 = time.perf_counter()
    banner()
    if phase_prepare() is None:
        print(dim("  " + "─" * max(20, term_width() - 2)))
        return Result(phase="prepare"), None
    src_hash = sources_hash()
    outcome, skipped = phase_test(opts, src_hash)
    if outcome is not None and not outcome.clean:
        res = Result(
            status="fail", phase="cargo", blocking=outcome.blocking,
            ocd_pending=bool(audit_rust_ocd()),
            src_hash=src_hash, report=build_report(outcome),
        )
        save_report(res.report)
        print(dim("  " + "─" * max(20, term_width() - 2)))
        return res, outcome
    res = phase_ship(opts, src_hash, skipped)
    if res.report:
        save_report(res.report)
    if res.usable:
        finish_line(res, t0)
    else:
        print(dim("  " + "─" * max(20, term_width() - 2)))
    return res, outcome


def open_html() -> None:
    import webbrowser
    webbrowser.open(HTML.as_uri())




def interactive_loop(opts) -> int:
    watch = Watcher()
    while True:
        try:
            res, _ = pipeline_once(opts)
        except KeyboardInterrupt:
            Live.end()
            print(f"\n  {WARN} Đã hủy bước đang chạy; quay lại bảng điều khiển. Dùng run để thử lại.")
            if opts.once or not sys.stdin.isatty():
                raise
            res = Result(status="fail", phase="prepare", report="Bước đang chạy đã bị hủy bằng Ctrl+C.\n")
        watch.mark()
        opts.force = False
        code = 0 if res.usable and not (res.phase == "cargo") else 1
        if opts.open and res.usable:
            open_html()
            opts.open = False
        if opts.once or not sys.stdin.isatty():
            return code
        while True:
            print()
            draw_panel("BSE-AutoRouter · BẢNG ĐIỀU KHIỂN", [
                f"Trạng thái: {res.status.upper()} · Giai đoạn: {res.phase}",
                f"Dự án: {ROOT}",
                f"Chi tiết lỗi: {REPORT_FILE.relative_to(ROOT).as_posix()}" if res.report else "Chi tiết lỗi: không có báo cáo lỗi đang chờ",
                "",
                "o / open      Mở main.html trong trình duyệt" if res.usable else "o / open      Chỉ khả dụng khi bản build dùng được",
                "c / copy      Sao chép báo cáo lỗi/cảnh báo",
                "p / pack      Đóng gói Rust sources vào RUST_PROJECT_BUNDLE.txt",
                "l / ocd       Chuẩn hóa mã theo quy tắc OCD",
                "b / backup    Trung tâm sao lưu / khôi phục / nhập / xuất",
                "x / clean     Dọn artefact và giải phóng dung lượng",
                "r / run       Chạy lại quy trình",
                "f / force     Ép chạy lại test và build, bỏ qua cache",
                "w / build-warn Tiếp tục build nếu Cargo chỉ còn cảnh báo" if res.phase == "cargo" and not res.blocking else "w / build-warn Chỉ khả dụng khi Cargo có cảnh báo không chặn build",
                "q / quit      Thoát chương trình",
                "",
                f"Watcher đang theo dõi {BUNDLE.name} và main.html; lưu file sẽ kích hoạt chạy lại.",
                "Nhập lệnh rồi nhấn Enter để xác nhận; dùng help để xem hướng dẫn.",
            ])
            try:
                k = normalize_menu_command(read_command("BSE > ", poll=watch.changed))
            except KeyboardInterrupt:
                print(f"\n  {INFO} Đang ở menu chính; nhập q rồi nhấn Enter để thoát.")
                continue
            if k == "\x00watch":
                print(f"\n  {INFO} Phát hiện thay đổi file → chạy lại quy trình…")
                break
            if k in ("q", "quit"):
                return 0 if res.usable else 1
            if k == "run":
                break
            if k == "force":
                opts.force = True
                break
            if k == "open":
                if res.usable and HTML.exists():
                    try:
                        open_html()
                        stage(OK, "Mở HTML", "Đã gửi main.html tới trình duyệt mặc định.")
                    except Exception as exc:
                        stage(BAD, "Mở HTML", str(exc))
                else:
                    stage(WARN, "Mở HTML", "Chưa có bản build dùng được hoặc không tìm thấy main.html.")
            elif k == "copy":
                if not res.report:
                    stage(INFO, "Clipboard", "Không có báo cáo lỗi/cảnh báo để sao chép.")
                elif copy_to_clipboard(res.report):
                    stage(OK, "Đã copy", f"{fmt_size(len(res.report.encode('utf-8')))} · báo cáo tại {REPORT_FILE.relative_to(ROOT).as_posix()}")
                else:
                    stage(WARN, "Clipboard", f"Không copy được; mở file {REPORT_FILE.relative_to(ROOT).as_posix()} trực tiếp.")
            elif k == "pack":
                try:
                    old, new, n, wrote = pack_bundle()
                    watch.mark()
                    stage(OK, "Pack bundle", f"{n} file · {fmt_size(old)} → {fmt_size(new)}" + ("" if wrote else " · đã gọn sẵn"))
                except OSError as exc:
                    stage(BAD, "Pack bundle", str(exc))
            elif k == "ocd":
                try:
                    r_acts = sanitize_rust_ocd()
                    h_acts = sanitize_html_ocd()
                    pack_bundle()
                    watch.mark()
                    if r_acts or h_acts:
                        stage(OK, "Làm sạch OCD", " · ".join(r_acts + h_acts))
                        break
                    stage(OK, "Làm sạch OCD", "Mã nguồn Rust và main.html đã đạt chuẩn sạch.")
                except Exception as exc:
                    stage(BAD, "Làm sạch OCD", str(exc))
            elif k == "build-warn" and res.phase == "cargo" and not res.blocking:
                t1 = time.perf_counter()
                print(f"\n  {INFO} Đang build WASM dù cargo còn cảnh báo…")
                try:
                    res = phase_ship(opts, res.src_hash, False)
                    watch.mark()
                    if res.report:
                        save_report(res.report)
                    if res.usable:
                        finish_line(res, t1, " (cargo còn cảnh báo)")
                    else:
                        print(dim("  " + "─" * max(20, term_width() - 2)))
                except KeyboardInterrupt:
                    Live.end()
                    res = Result(status="fail", phase="prepare", report="Build WASM đã bị hủy bằng Ctrl+C.\n")
                    print(f"\n  {WARN} Đã dừng build; quay lại bảng điều khiển.")
            elif k == "backup":
                try:
                    restored = backup_center_interactive()
                except KeyboardInterrupt:
                    print(f"\n  {WARN} Đã hủy thao tác backup; quay lại bảng điều khiển.")
                    restored = False
                except Exception as exc:
                    stage(BAD, "Backup", str(exc))
                    restored = False
                watch.mark()
                if restored:
                    break
            elif k == "clean":
                try:
                    cleaner_interactive()
                except KeyboardInterrupt:
                    Live.end()
                    print(f"\n  {WARN} Đã dừng thao tác dọn dẹp; quay lại bảng điều khiển.")
                watch.mark()
            elif k in ("help", ""):
                stage(INFO, "Cách dùng", "Nhập một lệnh ở trên rồi nhấn Enter; Ctrl+C hủy thao tác đang chạy.")
            else:
                stage(WARN, "Lệnh", f"Không nhận ra '{k}'. Nhập help rồi nhấn Enter để xem hướng dẫn." )



# ════════════════════════════════════════════════════════════════════════════
#  CÁC LỆNH CLI PHỤ TRỰC TIẾP
# ════════════════════════════════════════════════════════════════════════════
def cmd_ocd() -> int:
    banner()
    try:
        rust_actions = sanitize_rust_ocd()
        html_actions = sanitize_html_ocd()
        old, new, count, _ = pack_bundle()
    except Exception as exc:
        stage(BAD, "Làm sạch OCD", f"Không hoàn tất: {exc}")
        return 1
    for action in rust_actions + html_actions:
        stage(OK, "Làm sạch OCD", action)
    if not (rust_actions or html_actions):
        stage(OK, "Làm sạch OCD", "Mã Rust và main.html đã sạch chuẩn OCD")
    stage(OK, "Pack bundle", f"{count} file · {fmt_size(old)} → {fmt_size(new)}")
    return 0



def cmd_pack() -> int:
    banner()
    try:
        old, new, count, wrote = pack_bundle()
    except Exception as exc:
        stage(BAD, "Pack bundle", f"Không ghi được bundle: {exc}")
        return 1
    stage(OK, "Pack bundle", f"{count} file · {fmt_size(old)} → {fmt_size(new)}" + ("" if wrote else " · đã gọn sẵn"))
    return 0



def cmd_build(opts) -> int:
    banner()
    res = phase_ship(opts, sources_hash(), False)
    if res.report:
        save_report(res.report)
        sub(f"chi tiết lỗi: {REPORT_FILE.relative_to(ROOT).as_posix()}")
    return 0 if res.usable else 1


def cmd_check() -> int:
    banner()
    cur = sources_hash()
    bh = bridge_hash()
    if not BRIDGE.exists():
        stage(BAD, "Bridge", "chưa có wasm-bridge.js — chạy: python bse.py")
    elif bh != cur:
        stage(WARN, "Bridge", f"cũ hơn mã Rust (bridge {bh or '?'} ≠ mã {cur}) — chạy: python bse.py")
    else:
        stage(OK, "Bridge", f"khớp mã Rust hiện tại ({cur})")
    issues = check_contract()
    errs = [i for i in issues if i.level == "error"]
    stage(
        BAD if errs else (WARN if issues else OK), "Khớp HTML",
        "main.html ↔ Rust " + ("ăn khớp hoàn hảo" if not issues else f"có {len(issues)} điểm lưu ý"),
    )
    show_issues(issues)
    allows = audit_rust_ocd()
    stage(WARN if allows else OK, "Kiểm toán OCD", "0 cờ #[allow(unused_...)] vá tạm" if not allows else f"{len(allows)} cờ allow vá tạm")
    for rel, ln_no, attr in allows:
        sub(f"{rel}:{ln_no}  {attr}", yellow("W"))
    st = state_load()
    stage(
        INFO, "Dung lượng",
        f"target/: {fmt_size(dir_size(TARGET_DIR))} · backups/: {fmt_size(dir_size(BACKUPS))} ({len(list_snapshots())} bản) · "
        f"OK cuối: {st.get('last_ok_time', 'chưa có')}",
    )
    return 1 if errs else 0


def main() -> int:
    ap = argparse.ArgumentParser(
        description="BSE-AutoRouter AIO: Đồng bộ · Kiểm toán OCD · Sao lưu · Test · Build WASM",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    ap.add_argument(
        "cmd", nargs="?", default="update",
        choices=["update", "ocd", "pack", "build", "check", "backup", "restore", "clean"],
    )
    ap.add_argument("arg", nargs="?", help="tham số phụ (nếu có)")
    ap.add_argument("--force", action="store_true", help="bỏ qua cache, ép chạy lại cargo test và wasm-pack")
    ap.add_argument("--once", action="store_true", help="chạy 1 lượt rồi thoát, không chờ phím/không theo dõi file")
    ap.add_argument("--open", action="store_true", help="tự động mở main.html sau khi hoàn tất")
    opts = ap.parse_args()
    try:
        if opts.cmd == "ocd":
            return cmd_ocd()
        if opts.cmd == "pack":
            return cmd_pack()
        if opts.cmd == "build":
            return cmd_build(opts)
        if opts.cmd == "check":
            return cmd_check()
        if opts.cmd in ("backup", "restore"):
            backup_center_interactive()
            return 0
        if opts.cmd == "clean":
            cleaner_interactive()
            return 0
        return interactive_loop(opts)
    except KeyboardInterrupt:
        Live.end()
        print(f"\n  {WARN} Đã dừng thao tác. Dùng `python bse.py backup` nếu cần khôi phục.")
        return 130


if __name__ == "__main__":
    sys.exit(main())