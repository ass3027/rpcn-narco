#!/usr/bin/env python3
"""TTT2 (NPWR02973_00) TUS save admin tool for the RPCN server.

Runs on the RPCN host. Reads the account/save mapping straight from the RPCN
database (read-only) and edits the save file the DB currently points at.

Every write takes an automatic backup first, appends to an audit log, and
verifies the result by md5. Nothing here touches the database.

    tdt_admin.py show <npid>
    tdt_admin.py backup <npid>... | --all [--label NAME]
    tdt_admin.py restore <npid> [--label NAME | --file PATH]
    tdt_admin.py list-backups [<npid>]
    tdt_admin.py set-rank <npid> --char N --rank N [--points N]
    tdt_admin.py set-account-rank <npid> --rank N
    tdt_admin.py apply <npid> --file PATH
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


def guard_online(npid, force):
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
def decode(b):
    chars = []
    for i in range(CHAR_N):
        o = CHAR_BASE + i * CHAR_STRIDE
        w, l = be32(b, o + SLOT_WIN), be32(b, o + SLOT_LOSS)
        if w or l or b[o]:
            s = b[o + 4]
            chars.append({"id": i, "rank": b[o], "points": be16(b, o + SLOT_POINTS),
                          "streak": s - 256 if s > 127 else s, "wins": w, "losses": l})
    return {
        "account_rank": b[OFF_ACCOUNT_RANK], "progress": b[OFF_ACCOUNT_PROGRESS],
        "total": be32(b, OFF_TOTAL), "wins": be32(b, OFF_WINS), "losses": be32(b, OFF_LOSSES),
        "checksum": be32(b, 0), "chars": chars,
    }


def cmd_show(a):
    uid, data_id, saved, path = lookup(a.npid)
    b = read_save(path)
    d = decode(b)
    ok = checksum(b) == d["checksum"]
    print(f"{a.npid}  user_id={uid}  data_id={data_id}  saved={saved} UTC")
    print(f"  file      {path}")
    print(f"  checksum  0x{d['checksum']:08X}  {'OK' if ok else 'MISMATCH'}")
    print(f"  account   rank={d['account_rank']} progress={d['progress']}/11")
    wr = d["wins"] / (d["wins"] + d["losses"]) if d["wins"] + d["losses"] else 0
    print(f"  record    {d['total']} matches = {d['wins']}W {d['losses']}L ({wr:.1%})")
    print(f"  characters in use: {len(d['chars'])}")
    for c in sorted(d["chars"], key=lambda x: -(x["wins"] + x["losses"])):
        print(f"    id {c['id']:2d}  rank {c['rank']:2d}  {c['points']:5d}pt  "
              f"streak {c['streak']:+3d}  {c['wins']:5d}W {c['losses']:5d}L")


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


def _apply(npid, buf, action, force, **meta):
    uid, data_id, saved, path = lookup(npid)
    guard_online(npid, force)
    before = md5(path)
    bak = take_backup(npid, path)
    new_ck = reseal(buf)
    write_save(path, buf)
    after = md5(path)
    # the game may have saved while we worked; the DB would then point elsewhere
    _, data_id2, _, _ = lookup(npid)
    audit(action, npid, data_id=data_id, backup=bak, md5_before=before,
          md5_after=after, checksum=f"0x{new_ck:08X}", **meta)
    print(f"  backup   {os.path.basename(bak)}")
    print(f"  checksum 0x{new_ck:08X}")
    print(f"  md5      {before[:12]} -> {after[:12]}")
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


def cmd_restore(a):
    if a.file:
        src = a.file
    else:
        name = f"{a.label}.tdt" if a.label else "*.tdt"
        found = sorted(glob.glob(os.path.join(BACKUP_DIR, _safe(a.npid), name)))
        if not found:
            die(f"no backup matching {pat}")
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

    s = sub.add_parser("show"); s.add_argument("npid"); s.set_defaults(fn=cmd_show)

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
    a.fn(a)


if __name__ == "__main__":
    main()
