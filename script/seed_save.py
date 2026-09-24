#!/usr/bin/env python3
"""Give a TTT2 account that has no save yet a floored save built from a template.

Test only: checks whether the game accepts a save it did not create itself.

    seed_save.py <npid> [--template PATH]
"""
import os
import sys
import time
import sqlite3
import argparse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import tdt_admin as ta

TEMPLATE = "/home/ec2-user/backup/tdt/B-plan/pre-floor-new-20260924-142402.tdt"   # 0 matches, rank 0


def free_data_id(con):
    """an unused id below every file on disk; RPCN's dispenser never hands those out again"""
    used = {r[0] for r in con.execute("SELECT data_id FROM tus_data")}
    used |= {r[0] for r in con.execute("SELECT data_id FROM tus_data_vuser")}
    top = max(int(f[:-4]) for f in os.listdir(ta.TUS_DIR) if f.endswith(".tdt"))
    for i in range(top - 1000, 0, -1):
        if i not in used and not os.path.exists(os.path.join(ta.TUS_DIR, f"{i:020d}.tdt")):
            return i, top
    ta.die("no free data_id")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("npid")
    ap.add_argument("--template", default=TEMPLATE)
    a = ap.parse_args()

    b = ta.read_save(a.template)
    m, y, n = ta.floor_buf(b)
    ck = ta.reseal(b)
    print(f"template {a.template}: reached {m} -> floor {y}, {n} slots raised, checksum 0x{ck:08X}")

    con = sqlite3.connect(ta.DB_PATH, timeout=10)
    row = con.execute("SELECT user_id FROM account WHERE username = ?", (a.npid,)).fetchone()
    if not row:
        ta.die(f"account {a.npid!r} not found")
    uid = row[0]
    data_id, top = free_data_id(con)
    path = os.path.join(ta.TUS_DIR, f"{data_id:020d}.tdt")
    mode = os.stat(os.path.join(ta.TUS_DIR, f"{top:020d}.tdt")).st_mode & 0o777

    with open(path, "wb") as f:
        f.write(bytes(b))
    os.chmod(path, mode)
    try:
        with con:
            tick = (int(time.time()) + 62135596800) * 1_000_000
            # plain INSERT: fails if the game already created this account's save
            con.execute(
                "INSERT INTO tus_data (owner_id, communication_id, slot_id, data_id, data_info, timestamp, author_id) "
                "VALUES (?, ?, ?, ?, x'', ?, ?)",
                (uid, ta.COM_ID.encode(), ta.SLOT, data_id, tick, uid))
    except Exception:
        os.unlink(path)
        raise
    finally:
        con.close()

    ta.audit("seed-floor-save", a.npid, data_id=data_id, template=a.template, floor=y, raised=n,
             checksum=f"0x{ck:08X}", md5=ta.md5(path))
    print(f"{a.npid} (user_id {uid}): data_id {data_id} -> {path}")


if __name__ == "__main__":
    main()
