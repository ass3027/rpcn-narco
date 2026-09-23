#!/usr/bin/env python3
"""One-time application of the tier rank floor to every TTT2 save.

Policy (no ratchet - this is a single pass, nothing is re-applied later):
  every character  -> rank >= 10 (1st Dan), rank_points 0
  account that reached a tier gets a floor two tiers below

"Reached" means the account rank at 0x18, not the best character's rank now.
A character is demoted when it loses; the account rank only ever goes up, so
it is the high water mark. Across the population it sits above the best
character in 132 accounts of 544 and below it in none.

      M         floor
      0..20     10  1st Dan
      21..24    13  Disciple
      25..28    17  Brawler
      29..32    21  Warrior
      33..37    25  Vanquisher
      38..40    29  Genbu
      41+       33  Fujin

Only rank and rank_points are written; matches, wins and losses are untouched.
Runs on the RPCN host, reads the DB read-only, and edits the file the DB
currently points at.

Safety: takes a fresh backup of every save first, refuses accounts that are
online (the game would overwrite the edit on its next save), preserves owner
and mode, and verifies every file after writing.

    apply_floor.py            dry run, writes a report only
    apply_floor.py --apply    actually writes
"""
import os
import csv
import sys
import json
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
AUDIT_LOG = "/home/ec2-user/backup/tdt/audit.jsonl"
REPORT = "/home/ec2-user/backup/tdt/floor_apply_report.csv"
STAT_URL = "http://127.0.0.1:31314/rpcn_stats/usage"

CB, CS, CN = 0x70, 0x30, 59
ACC_RANK = 0x18   # 계정 계급. 강등되지 않는 최고 달성 기록
BASE_FLOOR = 10
TIERS = [10, 13, 17, 21, 25, 29, 33, 38, 41]

POINTS = {r: 200 * r for r in range(1, 10)}
POINTS[10] = 0
_P10 = {11: 2531, 12: 2735, 13: 2300, 14: 3112, 15: 2907, 16: 2799,
        17: 5993, 18: 1562, 19: 1964, 21: 2679}
for _r in range(11, 43):
    if _r in _P10:
        POINTS[_r] = _P10[_r]
    else:
        _lo = max([k for k in _P10 if k < _r], default=None)
        _hi = min([k for k in _P10 if k > _r], default=None)
        if _lo is not None and _hi is not None:
            POINTS[_r] = int(round(_P10[_lo] + (_r - _lo) / (_hi - _lo) * (_P10[_hi] - _P10[_lo])))
        else:
            POINTS[_r] = _P10[_lo if _lo is not None else _hi]

# ------------------------------------------------------------------ checksum
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


# ---------------------------------------------------------------------- util
def md5(path):
    return hashlib.md5(open(path, "rb").read()).hexdigest()


def be32(b, o):
    return int.from_bytes(b[o:o + 4], "big")


def be16(b, o):
    return int.from_bytes(b[o:o + 2], "big")


def floor_for(m):
    idx = -1
    for i, t in enumerate(TIERS):
        if m >= t:
            idx = i
    return TIERS[idx - 2] if idx >= 2 else BASE_FLOOR


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


def write_save(path, buf):
    st = os.stat(path)
    tmp = f"/tmp/.floor_{os.getpid()}.tmp"
    open(tmp, "wb").write(bytes(buf))
    try:
        shutil.copyfile(tmp, path)
    except PermissionError:
        subprocess.run(["sudo", "cp", tmp, path], check=True)
        subprocess.run(["sudo", "chown", f"{st.st_uid}:{st.st_gid}", path], check=True)
        subprocess.run(["sudo", "chmod", oct(st.st_mode & 0o777)[2:], path], check=True)
    os.unlink(tmp)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--apply", action="store_true", help="actually write")
    ap.add_argument("--force", action="store_true",
                    help="write even for accounts the server still lists as online; "
                         "only for a session known to be stale, since a live client "
                         "overwrites the edit on its next save")
    ap.add_argument("--label", default="pre-floor-" + dt.datetime.now().strftime("%Y%m%d-%H%M%S"))
    a = ap.parse_args()

    con = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True)
    rows = con.execute(
        "SELECT a.username, t.data_id FROM tus_data t JOIN account a ON a.user_id = t.owner_id "
        "WHERE CAST(t.communication_id AS TEXT) = ? AND t.slot_id = ? ORDER BY a.username",
        (COM_ID, SLOT)).fetchall()
    con.close()
    print(f"대상 계정 {len(rows)}")

    who = online() or set()
    print(f"현재 접속자 {len(who)}: {sorted(who)}")
    if a.force and who:
        print("  --force: 접속 중 계정도 함께 적용한다")
    print(f"모드: {'실제 적용' if a.apply else '드라이런'}   백업 라벨 {a.label}\n")

    report = []
    done = skipped_online = missing = nochange = failed = 0
    tot_changed = 0

    for npid, data_id in rows:
        path = os.path.join(TUS_DIR, f"{data_id:020d}.tdt")
        if not os.path.exists(path):
            missing += 1
            report.append({"npid": npid, "data_id": data_id, "result": "file_missing",
                           "floor": "", "changed": 0, "md5_before": "", "md5_after": ""})
            continue
        if npid in who and not a.force:
            skipped_online += 1
            report.append({"npid": npid, "data_id": data_id, "result": "skipped_online",
                           "floor": "", "changed": 0, "md5_before": "", "md5_after": ""})
            continue

        b = bytearray(open(path, "rb").read())
        if len(b) != REC:
            failed += 1
            report.append({"npid": npid, "data_id": data_id, "result": f"bad_size_{len(b)}",
                           "floor": "", "changed": 0, "md5_before": "", "md5_after": ""})
            continue

        # 최고 달성 계급은 캐릭터 최고값이 아니라 계정 계급이다. 캐릭터는
        # 강등되지만 계정 계급은 내려가지 않으므로, 오래 한 사람일수록 둘이
        # 벌어진다. 바닥은 '그 사람이 도달했던 곳' 기준이어야 한다.
        m = max(max(b[CB + i * CS] for i in range(CN)), b[ACC_RANK])
        Y = floor_for(m)
        n = 0
        for i in range(CN):
            o = CB + i * CS
            if b[o] < Y:
                b[o] = Y
                b[o + 2:o + 4] = POINTS[Y].to_bytes(2, "big")
                n += 1
        # 계정 계급이 바닥보다 낮으면 같이 올린다. 그대로 두면 자기 캐릭터
        # 전부보다 낮은 계급으로 표시된다.
        if b[ACC_RANK] < Y:
            b[ACC_RANK] = Y
            n += 1
        if n == 0:
            nochange += 1
            report.append({"npid": npid, "data_id": data_id, "result": "no_change",
                           "floor": Y, "changed": 0, "md5_before": md5(path), "md5_after": ""})
            continue

        before = md5(path)
        b[0:4] = checksum(b).to_bytes(4, "big")
        tot_changed += n

        if not a.apply:
            report.append({"npid": npid, "data_id": data_id, "result": "would_change",
                           "floor": Y, "changed": n, "md5_before": before, "md5_after": ""})
            done += 1
            continue

        d = os.path.join(BACKUP_DIR, npid)
        os.makedirs(d, exist_ok=True)
        shutil.copyfile(path, os.path.join(d, a.label + ".tdt"))
        write_save(path, b)
        after = md5(path)
        chk = bytearray(open(path, "rb").read())
        ok = len(chk) == REC and checksum(chk) == be32(chk, 0) and be32(chk, 0) == be32(b, 0)
        report.append({"npid": npid, "data_id": data_id,
                       "result": "applied" if ok else "VERIFY_FAILED",
                       "floor": Y, "changed": n, "md5_before": before, "md5_after": after})
        if ok:
            done += 1
        else:
            failed += 1

    with open(REPORT, "w", newline="", encoding="utf-8-sig") as fh:
        w = csv.DictWriter(fh, fieldnames=list(report[0].keys()))
        w.writeheader()
        w.writerows(report)

    print(f"{'적용' if a.apply else '적용 예정'} {done}   변경 캐릭터 {tot_changed:,}")
    print(f"접속중 제외 {skipped_online}   변경 없음 {nochange}   파일 없음 {missing}   실패 {failed}")
    if skipped_online:
        print("  접속중 제외 계정: " +
              ", ".join(r["npid"] for r in report if r["result"] == "skipped_online"))
    if failed:
        print("  실패: " + ", ".join(r["npid"] for r in report if "FAIL" in r["result"]
                                     or r["result"].startswith("bad_size")))
    print(f"-> {REPORT}")

    if a.apply:
        with open(AUDIT_LOG, "a", encoding="utf-8") as fh:
            fh.write(json.dumps({
                "ts": dt.datetime.now().isoformat(timespec="seconds"),
                "user": os.environ.get("SUDO_USER") or os.environ.get("USER", "?"),
                "action": "floor_apply_batch", "npid": "-",
                "accounts": done, "chars": tot_changed, "label": a.label,
                "skipped_online": skipped_online, "failed": failed}, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
