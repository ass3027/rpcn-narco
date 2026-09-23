#!/usr/bin/env python3
"""TTT2 (NPWR02973_00) TUS save admin tool for the RPCN server.

Runs on the RPCN host. Reads the account/save mapping straight from the RPCN
database (read-only) and edits the save file the DB currently points at.

Every write takes an automatic backup first, appends to an audit log, and
verifies the result by md5. Nothing here touches the database.

    tdt_admin.py show <npid> | --input-file X.tdt [--all-chars] [--json]
    tdt_admin.py backup <npid>... | --all [--label NAME]
    tdt_admin.py restore <npid> [--label NAME | --file PATH]
    tdt_admin.py list-backups [<npid>]
    tdt_admin.py set-rank <npid> --char N --rank N [--points N]
    tdt_admin.py set-account-rank <npid> --rank N
    tdt_admin.py apply <npid> --file PATH
    tdt_admin.py floor <npid>... | --all [--rank N] [--label NAME]
    tdt_admin.py floor --input-file X.tdt [<npid> | --output-file Y.tdt] [--rank N]
    tdt_admin.py log [-n N]
    tdt_admin.py gc [--apply] [--archive] [--keep-days N]

Writes refuse to run while the target account is online unless --force is
given, because the game issues a new data_id on its next save and would
silently discard the edit.
"""
import os
import sys
import json
import glob
import time
import shutil
import sqlite3
import hashlib
import argparse
import subprocess
import datetime as dt
import urllib.request

REC = 3420
COM_ID = "NPWR02973_00"
SLOT = 1

DB_PATH = "/home/ec2-user/rpcn-data/db/rpcn.db"
TUS_DIR = "/home/ec2-user/rpcn-data/tus_data"
BACKUP_DIR = "/home/ec2-user/backup/tdt"
ARCHIVE_DIR = "/home/ec2-user/backup/tdt_archive"
AUDIT_LOG = "/home/ec2-user/backup/tdt/audit.jsonl"
STAT_URL = "http://127.0.0.1:31314/rpcn_stats/usage"

CHAR_BASE, CHAR_STRIDE, CHAR_N = 0x70, 0x30, 59
OFF_ACCOUNT_RANK, OFF_ACCOUNT_PROGRESS = 0x18, 0x1B
OFF_TOTAL, OFF_WINS, OFF_LOSSES = 0x20, 0xCDC, 0xCE0
SLOT_RANK, SLOT_POINTS, SLOT_WIN, SLOT_LOSS = 0x00, 0x02, 0x08, 0x0C

# floor policy: reached tier(account rank = high water mark) -> two tiers below
BASE_FLOOR = 10
TIERS = [10, 13, 17, 21, 25, 29, 33, 38, 41]

FLOOR_POINTS = {r: 200 * r for r in range(1, 10)}
FLOOR_POINTS[10] = 0
_P10 = {11: 2531, 12: 2735, 13: 2300, 14: 3112, 15: 2907, 16: 2799,
        17: 5993, 18: 1562, 19: 1964, 21: 2679}
for _r in range(11, 43):
    if _r in _P10:
        FLOOR_POINTS[_r] = _P10[_r]
    else:
        _lo = max([k for k in _P10 if k < _r], default=None)
        _hi = min([k for k in _P10 if k > _r], default=None)
        if _lo is not None and _hi is not None:
            FLOOR_POINTS[_r] = int(round(_P10[_lo] + (_r - _lo) / (_hi - _lo) * (_P10[_hi] - _P10[_lo])))
        else:
            FLOOR_POINTS[_r] = _P10[_lo if _lo is not None else _hi]

# --------------------------------------------------------------- checksum
P = 0x1DB710641
MASK = 0xFFFFFFFF
C_CONST = 0x85840EC7


def _mulx(v):
    v <<= 1
    if v >> 32 & 1:
        v ^= P
    return v & MASK


def _gf_mul(a, b):
    r = 0
    while b:
        if b & 1:
            r ^= a
        a = _mulx(a)
        b >>= 1
    return r


def _gf_pow(a, n):
    r = 1
    while n:
        if n & 1:
            r = _gf_mul(r, a)
        a = _gf_mul(a, a)
        n >>= 1
    return r


_X8 = _gf_pow(2, 8)
_LEAD = _gf_mul(C_CONST, _gf_pow(2, 32))


def checksum(buf):
    acc = 0
    for o in range(REC - 1, 3, -1):
        acc = _gf_mul(acc, _X8) ^ buf[o]
    return _gf_mul(_LEAD, acc)


def reseal(buf):
    """recompute and store the checksum in place"""
    v = checksum(buf)
    buf[0:4] = v.to_bytes(4, "big")
    return v


# ------------------------------------------------------------------ util
def die(msg):
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def md5(path):
    h = hashlib.md5()
    with open(path, "rb") as f:
        h.update(f.read())
    return h.hexdigest()


def be32(b, o):
    return int.from_bytes(b[o:o + 4], "big")


def be16(b, o):
    return int.from_bytes(b[o:o + 2], "big")


def db():
    if not os.path.exists(DB_PATH):
        die(f"database not found: {DB_PATH}")
    return sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True)


def lookup(npid):
    """npid -> (user_id, data_id, saved_at, path)"""
    con = db()
    row = con.execute(
        "SELECT a.user_id, t.data_id, t.timestamp FROM tus_data t "
        "JOIN account a ON a.user_id = t.owner_id "
        "WHERE a.username = ? AND CAST(t.communication_id AS TEXT) = ? AND t.slot_id = ?",
        (npid, COM_ID, SLOT)).fetchone()
    con.close()
    if not row:
        die(f"no {COM_ID} save for account {npid!r}")
    uid, data_id, ts = row
    saved = dt.datetime.utcfromtimestamp(ts / 1_000_000 - 62135596800)
    return uid, data_id, saved, os.path.join(TUS_DIR, f"{data_id:020d}.tdt")


def online():
    try:
        with urllib.request.urlopen(STAT_URL, timeout=5) as r:
            data = json.loads(r.read().decode())
    except Exception:
        return None
    out = set()
    for players in data.get("players_id", {}).values():
        out.update(players.keys())
    return out


def read_save(path):
    if not os.path.exists(path):
        die(f"save file missing: {path}")
    b = bytearray(open(path, "rb").read())
    if len(b) != REC:
        die(f"{path}: expected {REC} bytes, got {len(b)}")
    return b


def write_save(path, buf):
    """replace in place, preserving owner and mode (uses sudo when needed)"""
    st = os.stat(path)
    tmp = os.path.join("/tmp", f".tdt_admin_{os.getpid()}.tmp")
    with open(tmp, "wb") as f:
        f.write(bytes(buf))
    try:
        shutil.copyfile(tmp, path)
    except PermissionError:
        subprocess.run(["sudo", "cp", tmp, path], check=True)
        subprocess.run(["sudo", "chown", f"{st.st_uid}:{st.st_gid}", path], check=True)
        subprocess.run(["sudo", "chmod", oct(st.st_mode & 0o777)[2:], path], check=True)
    os.unlink(tmp)


def audit(action, npid, **kw):
    os.makedirs(os.path.dirname(AUDIT_LOG), exist_ok=True)
    rec = {"ts": dt.datetime.now().isoformat(timespec="seconds"),
           "user": os.environ.get("SUDO_USER") or os.environ.get("USER", "?"),
           "action": action, "npid": npid}
    rec.update(kw)
    with open(AUDIT_LOG, "a", encoding="utf-8") as f:
        f.write(json.dumps(rec, ensure_ascii=False) + "\n")


def _safe(npid):
    """npids are validated by RPCN, but never let one escape the backup dir"""
    if not npid or npid in (".", "..") or "/" in npid or "\\" in npid:
        die(f"unsafe account name: {npid!r}")
    return npid


def backup_path(npid, label=None):
    """one directory per account, so names containing '_' cannot collide"""
    d = os.path.join(BACKUP_DIR, _safe(npid))
    os.makedirs(d, exist_ok=True)
    stamp = label or dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    return os.path.join(d, f"{stamp}.tdt")


def take_backup(npid, path, label=None):
    dst = backup_path(npid, label)
    shutil.copyfile(path, dst)
    return dst


def guard_online(npid, force, who=None):
    if who is None:
        who = online()
    if who is None:
        print("  warn: stat server unreachable, cannot check online status")
        return
    if npid in who:
        if not force:
            die(f"{npid} is online right now. The game would overwrite this edit "
                f"on its next save. Wait until they log off, or pass --force.")
        print(f"  warn: {npid} is ONLINE and --force was given")


# ----------------------------------------------------------------- render
# TTT2 internal character id -> display name
CHARACTERS = {
    0x00: "Paul", 0x01: "Law", 0x02: "Lei", 0x03: "King",
    0x04: "Yoshimitsu", 0x05: "Nina", 0x06: "Hwoarang", 0x07: "Xiayu",
    0x08: "Christie", 0x09: "Jin", 0x0A: "Julia", 0x0B: "Kuma",
    0x0C: "Bryan", 0x0D: "Heihachi", 0x0E: "Kazuya", 0x0F: "Lee",
    0x10: "Steve", 0x11: "Marduk", 0x12: "Mokujin", 0x13: "Jack",
    0x14: "Roger Jr.", 0x15: "Anna", 0x16: "Wang", 0x17: "Ganryu",
    0x18: "Asuka", 0x19: "Bruce", 0x1A: "Baek", 0x1B: "Devil Jin",
    0x1C: "Raven", 0x1D: "Feng", 0x1E: "Armor King", 0x1F: "Lili",
    0x20: "Dragunov", 0x21: "Eddy", 0x22: "Bob", 0x23: "Zafina",
    0x24: "Miguel", 0x25: "Leo", 0x26: "Lars", 0x27: "Alisa",
    0x28: "Jinpachi", 0x29: "True Ogre", 0x2A: "Jun", 0x2B: "Panda",
    0x2C: "Unknown", 0x2D: "Kunimitsu", 0x2E: "Michelle", 0x2F: "Forest Law",
    0x30: "Miharu", 0x31: "P-Jack", 0x32: "Sebastian", 0x33: "Michelle",
    0x34: "Combot", 0x35: "Alex", 0x36: "Ancient Ogre", 0x37: "Violet",
    0x38: "Dr.", 0x39: "Slim Bob", 0x3A: "Tiger",
}

# rank code -> (display name, tier)
RANKS = {
    0: ("Beginner", "숫자단"), 1: ("9th kyu", "숫자단"),
    2: ("8th kyu", "숫자단"), 3: ("7th kyu", "숫자단"),
    4: ("6th kyu", "숫자단"), 5: ("5th kyu", "숫자단"),
    6: ("4th kyu", "숫자단"), 7: ("3rd kyu", "숫자단"),
    8: ("2nd kyu", "숫자단"), 9: ("1st kyu", "숫자단"),
    10: ("1st dan", "숫자단"), 11: ("2nd dan", "숫자단"),
    12: ("3rd dan", "숫자단"), 13: ("Disciple", "액자단"),
    14: ("Mentor", "액자단"), 15: ("Master", "액자단"),
    16: ("Grand Master", "액자단"), 17: ("Brawler", "녹단"),
    18: ("Marauder", "녹단"), 19: ("Fighter", "녹단"),
    20: ("Berserker", "녹단"), 21: ("Warrior", "노랑단"),
    22: ("Avenger", "노랑단"), 23: ("Duelist", "노랑단"),
    24: ("Pugilist", "노랑단"), 25: ("Vanquisher", "주황단"),
    26: ("Destroyer", "주황단"), 27: ("Conqueror", "주황단"),
    28: ("Savior", "주황단"), 29: ("Genbu", "빨강단"),
    30: ("Byakko", "빨강단"), 31: ("Seiryu", "빨강단"),
    32: ("Suzaku", "빨강단"), 33: ("Fujin", "파랑단"),
    34: ("Raijin", "파랑단"), 35: ("Yaksa", "파랑단"),
    36: ("Majin", "파랑단"), 37: ("Toshin", "파랑단"),
    38: ("Emperor", "보라단"), 39: ("Tekken Lord", "보라단"),
    40: ("Tekken Emperor", "보라단"), 41: ("Tekken God", "God"),
    42: ("True Tekken God", "God"),
}


def rank_name(code):
    return RANKS.get(code, (f"Unknown ({code})", "Unknown"))


def decode(b, all_chars=False):
    chars = []
    for i in range(CHAR_N):
        o = CHAR_BASE + i * CHAR_STRIDE
        w, l = be32(b, o + SLOT_WIN), be32(b, o + SLOT_LOSS)
        if all_chars or w or l or b[o]:
            s = b[o + 4]
            name, tier = rank_name(b[o])
            chars.append({"id": i, "character": CHARACTERS.get(i, f"Unknown (0x{i:02X})"),
                          "rank": b[o], "rank_name": name, "tier": tier,
                          "points": be16(b, o + SLOT_POINTS),
                          "streak": s - 256 if s > 127 else s, "wins": w, "losses": l})
    return {
        "account_rank": b[OFF_ACCOUNT_RANK], "progress": b[OFF_ACCOUNT_PROGRESS],
        "total": be32(b, OFF_TOTAL), "wins": be32(b, OFF_WINS), "losses": be32(b, OFF_LOSSES),
        "checksum": be32(b, 0), "chars": chars,
    }


def cmd_show(a):
    if bool(a.npid) == bool(a.file):
        die("give an npid or --input-file, not both")
    if a.file:
        path = a.file
        head = {}
    else:
        uid, data_id, saved, path = lookup(a.npid)
        head = {"npid": a.npid, "user_id": uid, "data_id": data_id, "saved_utc": str(saved)}
    b = read_save(path)
    d = decode(b, a.all_chars)
    ok = checksum(b) == d["checksum"]
    sha = hashlib.sha256(b).hexdigest()

    if a.json:
        print(json.dumps({**head, "file": path, "sha256": sha, "checksum_ok": ok, **d},
                         ensure_ascii=False, indent=2))
        return

    if head:
        print(f"{a.npid}  user_id={head['user_id']}  data_id={head['data_id']}  "
              f"saved={head['saved_utc']} UTC")
    print(f"  file      {path}")
    print(f"  sha256    {sha}")
    print(f"  checksum  0x{d['checksum']:08X}  {'OK' if ok else 'MISMATCH'}")
    an, at = rank_name(d["account_rank"])
    print(f"  account   rank={d['account_rank']} {an} ({at})  progress={d['progress']}/11")
    wr = d["wins"] / (d["wins"] + d["losses"]) if d["wins"] + d["losses"] else 0
    print(f"  record    {d['total']} matches = {d['wins']}W {d['losses']}L ({wr:.1%})")
    print(f"  characters {'(all)' if a.all_chars else 'in use'}: {len(d['chars'])}")
    # 전체 목록은 id 순, 사용 중 목록은 판수 많은 순
    key = (lambda x: x["id"]) if a.all_chars else (lambda x: -(x["wins"] + x["losses"]))
    for c in sorted(d["chars"], key=key):
        # 한글은 터미널에서 2칸이라 정렬이 깨지므로 단은 맨 끝에 둔다
        print(f"    {c['id']:2d} {c['character']:13s} {c['rank']:2d} {c['rank_name']:16s} "
              f"{c['points']:5d}pt  streak {c['streak']:+3d}  {c['wins']:5d}W {c['losses']:5d}L  "
              f"{c['tier']}")


def cmd_backup(a):
    names = a.npid
    if a.all:
        con = db()
        names = [r[0] for r in con.execute(
            "SELECT a.username FROM tus_data t JOIN account a ON a.user_id=t.owner_id "
            "WHERE CAST(t.communication_id AS TEXT)=? AND t.slot_id=? ORDER BY a.username",
            (COM_ID, SLOT))]
        con.close()
    if not names:
        die("give one or more npids, or --all")
    n = 0
    for npid in names:
        try:
            uid, data_id, saved, path = lookup(npid)
            dst = take_backup(npid, path, a.label)
            audit("backup", npid, data_id=data_id, backup=dst, md5=md5(dst))
            print(f"  {npid:20s} data_id={data_id:<8d} -> {os.path.basename(dst)}")
            n += 1
        except SystemExit:
            print(f"  {npid:20s} skipped (no save)")
    print(f"{n} backed up into {BACKUP_DIR}")


def cmd_list_backups(a):
    pat = os.path.join(_safe(a.npid), "*.tdt") if a.npid else os.path.join("*", "*.tdt")
    files = sorted(glob.glob(os.path.join(BACKUP_DIR, pat)))
    if not files:
        print("no backups")
        return
    for f in files:
        npid = os.path.basename(os.path.dirname(f))
        label = os.path.basename(f)[:-4]
        b = bytearray(open(f, "rb").read())
        d = decode(b) if len(b) == REC else None
        extra = f"{d['total']} matches, rank {d['account_rank']}" if d else "BAD SIZE"
        print(f"  {npid:20s} {label:20s} {extra}")


def _apply(npid, buf, action, force, label=None, who=None, **meta):
    uid, data_id, saved, path = lookup(npid)
    guard_online(npid, force, who)
    before = md5(path)
    bak = take_backup(npid, path, label)
    new_ck = reseal(buf)
    write_save(path, buf)
    after = md5(path)
    verified = open(path, "rb").read() == bytes(buf)
    # the game may have saved while we worked; the DB would then point elsewhere
    _, data_id2, _, _ = lookup(npid)
    audit(action, npid, data_id=data_id, backup=bak, md5_before=before,
          md5_after=after, checksum=f"0x{new_ck:08X}", verified=verified, **meta)
    print(f"  backup   {os.path.basename(bak)}")
    print(f"  checksum 0x{new_ck:08X}")
    print(f"  md5      {before[:12]} -> {after[:12]}")
    if not verified:
        die(f"verify failed: {path} does not match what was written")
    if data_id2 != data_id:
        print(f"  WARNING: data_id changed {data_id} -> {data_id2} while writing. "
              f"The game saved in the meantime and this edit is now orphaned.")
    else:
        print(f"  applied to data_id {data_id}")


def cmd_set_rank(a):
    if not 0 <= a.char < CHAR_N:
        die(f"--char must be 0..{CHAR_N - 1}")
    if not 0 <= a.rank <= 255:
        die("--rank must be 0..255")
    uid, data_id, saved, path = lookup(a.npid)
    b = read_save(path)
    o = CHAR_BASE + a.char * CHAR_STRIDE
    old_rank, old_pts = b[o], be16(b, o + SLOT_POINTS)
    b[o] = a.rank
    if a.points is not None:
        b[o + SLOT_POINTS:o + SLOT_POINTS + 2] = a.points.to_bytes(2, "big")
    print(f"{a.npid}  character {a.char}: rank {old_rank} -> {a.rank}"
          + (f", points {old_pts} -> {a.points}" if a.points is not None else ""))
    if a.dry_run:
        print("  (dry run, nothing written)")
        return
    _apply(a.npid, b, "set-rank", a.force, char=a.char, rank=a.rank, points=a.points)


def cmd_set_account_rank(a):
    uid, data_id, saved, path = lookup(a.npid)
    b = read_save(path)
    old = b[OFF_ACCOUNT_RANK]
    b[OFF_ACCOUNT_RANK] = a.rank
    print(f"{a.npid}  account rank {old} -> {a.rank}")
    if a.dry_run:
        print("  (dry run, nothing written)")
        return
    _apply(a.npid, b, "set-account-rank", a.force, rank=a.rank)


def cmd_apply(a):
    b = bytearray(open(a.file, "rb").read())
    if len(b) != REC:
        die(f"{a.file}: expected {REC} bytes, got {len(b)}")
    print(f"{a.npid}  applying {a.file}")
    if a.dry_run:
        print("  (dry run, nothing written)")
        return
    _apply(a.npid, b, "apply", a.force, source=a.file)


def floor_for(m):
    idx = -1
    for i, t in enumerate(TIERS):
        if m >= t:
            idx = i
    return TIERS[idx - 2] if idx >= 2 else BASE_FLOOR


def floor_buf(b, rank=None):
    """raise every character and the account rank to the floor; returns (reached, floor, raised)"""
    # 캐릭터는 강등되지만 계정 계급은 내려가지 않으므로 둘 중 최대를 도달 계급으로 본다
    m = max(max(b[CHAR_BASE + i * CHAR_STRIDE] for i in range(CHAR_N)), b[OFF_ACCOUNT_RANK])
    y = floor_for(m) if rank is None else rank
    n = 0
    for i in range(CHAR_N):
        o = CHAR_BASE + i * CHAR_STRIDE
        if b[o] < y:
            b[o] = y
            b[o + SLOT_POINTS:o + SLOT_POINTS + 2] = FLOOR_POINTS[y].to_bytes(2, "big")
            n += 1
    if b[OFF_ACCOUNT_RANK] < y:
        b[OFF_ACCOUNT_RANK] = y
        n += 1
    return m, y, n


def _floor_file(a):
    if len(a.npid) > 1 or a.all:
        die("--input-file takes at most one npid")
    b = read_save(a.file)
    m, y, n = floor_buf(b, a.rank)
    print(f"{a.file}  reached {m} -> floor {y}, {n} slots raised")

    if a.npid:
        # 지정 파일에 floor를 적용한 결과를 해당 계정의 현재 세이브로 쓴다
        if a.dry_run:
            print("  (dry run, nothing written)")
            return
        _apply(a.npid[0], b, "floor", a.force, source=a.file, floor=y, raised=n)
        return

    out = a.out or a.file
    if a.dry_run or n == 0:
        print("  (dry run, nothing written)" if a.dry_run else "  no change")
        return
    if out == a.file:
        shutil.copyfile(a.file, a.file + ".bak")
    ck = reseal(b)
    with open(out, "wb") as f:
        f.write(bytes(b))
    audit("floor-file", "-", source=a.file, out=out, floor=y, raised=n, checksum=f"0x{ck:08X}")
    print(f"  checksum 0x{ck:08X} -> {out}")


def cmd_floor(a):
    if a.rank is not None and a.rank not in FLOOR_POINTS:
        die(f"--rank must be {min(FLOOR_POINTS)}..{max(FLOOR_POINTS)}")
    if a.out and not a.file:
        die("--output-file needs --input-file")
    if a.file:
        return _floor_file(a)

    names = a.npid
    if a.all:
        con = db()
        names = [r[0] for r in con.execute(
            "SELECT a.username FROM tus_data t JOIN account a ON a.user_id=t.owner_id "
            "WHERE CAST(t.communication_id AS TEXT)=? AND t.slot_id=? ORDER BY a.username",
            (COM_ID, SLOT))]
        con.close()
    if not names:
        die("give one or more npids, or --all")

    # 접속자 조회는 한 번만 한다. 계정마다 부르면 stat 서버 장애 시 계정 수만큼 대기한다
    who = online()
    if who is None:
        print("  warn: stat server unreachable, cannot check online status")
        who = set()
    label = a.label or "pre-floor-" + dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    print(f"{len(names)} accounts, floor {a.rank if a.rank is not None else 'auto'}, "
          f"backup label {label}{'  (dry run)' if a.dry_run else ''}")

    done = nochange = online_skip = failed = chars = 0
    for npid in names:
        try:
            _, _, _, path = lookup(npid)
            b = read_save(path)
        except SystemExit:
            failed += 1
            continue
        m, y, n = floor_buf(b, a.rank)
        if n == 0:
            nochange += 1
            continue
        if npid in who and not a.force:
            online_skip += 1
            print(f"  {npid:20s} skipped (online)")
            continue
        print(f"  {npid:20s} reached {m:2d} -> floor {y:2d}, {n} slots raised")
        if a.dry_run:
            done += 1
            chars += n
            continue
        try:
            _apply(npid, b, "floor", a.force, label=label, who=who, floor=y, raised=n)
            done += 1
            chars += n
        except SystemExit:
            failed += 1

    print(f"{'would apply' if a.dry_run else 'applied'} {done}  slots {chars:,}  "
          f"no change {nochange}  online {online_skip}  failed {failed}")
    if not a.dry_run and len(names) > 1:
        audit("floor-batch", "-", accounts=done, slots=chars, label=label, rank=a.rank,
              skipped_online=online_skip, failed=failed)


def cmd_restore(a):
    if a.file:
        src = a.file
    else:
        name = f"{a.label}.tdt" if a.label else "*.tdt"
        found = sorted(glob.glob(os.path.join(BACKUP_DIR, _safe(a.npid), name)))
        if not found:
            die(f"no backup matching {a.npid}/{name}")
        src = found[-1]
    b = bytearray(open(src, "rb").read())
    if len(b) != REC:
        die(f"{src}: expected {REC} bytes, got {len(b)}")
    print(f"{a.npid}  restoring from {os.path.basename(src)}")
    if a.dry_run:
        print("  (dry run, nothing written)")
        return
    _apply(a.npid, b, "restore", a.force, source=src)


def cmd_log(a):
    if not os.path.exists(AUDIT_LOG):
        print("no audit log yet")
        return
    lines = open(AUDIT_LOG, encoding="utf-8").read().splitlines()
    for line in lines[-a.n:]:
        r = json.loads(line)
        extra = " ".join(f"{k}={v}" for k, v in r.items()
                         if k not in ("ts", "user", "action", "npid", "backup"))
        print(f"  {r['ts']}  {r['user']:10s} {r['action']:18s} {r['npid']:18s} {extra}")


def cmd_gc(a):
    """delete or archive save files the database no longer references"""
    con = db()
    live = {r[0] for r in con.execute("SELECT data_id FROM tus_data")}
    live |= {r[0] for r in con.execute("SELECT data_id FROM tus_data_vuser")}
    con.close()

    cutoff = time.time() - a.keep_days * 86400
    on_disk = glob.glob(os.path.join(TUS_DIR, "*.tdt"))
    orphans, kept_recent = [], 0
    for p in on_disk:
        try:
            did = int(os.path.basename(p)[:-4])
        except ValueError:
            continue
        if did in live:
            continue
        if os.path.getmtime(p) > cutoff:
            kept_recent += 1
            continue
        orphans.append(p)

    total_mb = sum(os.path.getsize(p) for p in orphans) / 1e6
    print(f"files on disk      {len(on_disk):,}")
    print(f"referenced by DB   {len(live):,}")
    print(f"newer than {a.keep_days}d    {kept_recent:,}  (kept)")
    print(f"removable orphans  {len(orphans):,}  ({total_mb:,.0f} MB)")
    if not orphans:
        return
    if not a.apply:
        print("\ndry run. re-run with --apply to act.")
        return

    if a.archive:
        os.makedirs(ARCHIVE_DIR, exist_ok=True)
        stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
        tar = os.path.join(ARCHIVE_DIR, f"tus_orphans_{stamp}.tar.gz")
        listing = os.path.join("/tmp", f"gc_{os.getpid()}.txt")
        with open(listing, "w") as f:
            for p in orphans:
                f.write(os.path.relpath(p, TUS_DIR) + "\n")
        subprocess.run(["tar", "czf", tar, "-C", TUS_DIR, "-T", listing], check=True)
        os.unlink(listing)
        print(f"archived -> {tar}  ({os.path.getsize(tar) / 1e6:,.1f} MB)")

    removed = 0
    for p in orphans:
        try:
            os.unlink(p)
            removed += 1
        except PermissionError:
            subprocess.run(["sudo", "rm", "-f", p], check=True)
            removed += 1
    audit("gc", "-", removed=removed, archived=bool(a.archive), keep_days=a.keep_days)
    print(f"removed {removed:,} files, freed ~{total_mb:,.0f} MB")


def main():
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    def common(sp):
        sp.add_argument("--force", action="store_true",
                        help="write even if the account is online")
        sp.add_argument("--dry-run", action="store_true")

    s = sub.add_parser("show")
    s.add_argument("npid", nargs="?")
    s.add_argument("--input-file", "--file", dest="file", help="read this .tdt file instead (no DB)")
    s.add_argument("--all-chars", action="store_true", help="list all 59 characters, not only used ones")
    s.add_argument("--json", action="store_true")
    s.set_defaults(fn=cmd_show)

    s = sub.add_parser("backup")
    s.add_argument("npid", nargs="*")
    s.add_argument("--all", action="store_true")
    s.add_argument("--label")
    s.set_defaults(fn=cmd_backup)

    s = sub.add_parser("list-backups")
    s.add_argument("npid", nargs="?")
    s.set_defaults(fn=cmd_list_backups)

    s = sub.add_parser("restore")
    s.add_argument("npid")
    g = s.add_mutually_exclusive_group()
    g.add_argument("--label")
    g.add_argument("--file")
    common(s); s.set_defaults(fn=cmd_restore)

    s = sub.add_parser("set-rank")
    s.add_argument("npid")
    s.add_argument("--char", type=int, required=True)
    s.add_argument("--rank", type=int, required=True)
    s.add_argument("--points", type=int)
    common(s); s.set_defaults(fn=cmd_set_rank)

    s = sub.add_parser("set-account-rank")
    s.add_argument("npid")
    s.add_argument("--rank", type=int, required=True)
    common(s); s.set_defaults(fn=cmd_set_account_rank)

    s = sub.add_parser("apply")
    s.add_argument("npid")
    s.add_argument("--file", required=True)
    common(s); s.set_defaults(fn=cmd_apply)

    s = sub.add_parser("floor")
    s.add_argument("npid", nargs="*")
    s.add_argument("--all", action="store_true")
    s.add_argument("--rank", type=int, help="floor rank to force (default: two tiers below reached)")
    s.add_argument("--input-file", "--file", dest="file",
                   help="floor this .tdt save file instead of the live one")
    s.add_argument("--output-file", "--out", dest="out",
                   help="with --input-file and no npid: write here instead of in place")
    s.add_argument("--label", help="backup label (default pre-floor-<stamp>)")
    common(s); s.set_defaults(fn=cmd_floor)

    s = sub.add_parser("log")
    s.add_argument("-n", type=int, default=20)
    s.set_defaults(fn=cmd_log)

    s = sub.add_parser("gc")
    s.add_argument("--apply", action="store_true", help="actually delete")
    s.add_argument("--archive", action="store_true", help="tar.gz before deleting")
    s.add_argument("--keep-days", type=int, default=7,
                   help="never touch orphans newer than this (default 7)")
    s.set_defaults(fn=cmd_gc)

    a = p.parse_args()
    try:
        sys.stdout.reconfigure(encoding="utf-8")
    except AttributeError:
        pass
    a.fn(a)


if __name__ == "__main__":
    main()
