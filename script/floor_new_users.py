#!/usr/bin/env python3
"""Start TTT2 accounts created after the one-time floor pass at the floor.

A new user is an account created at or after CUTOFF (account_timestamp.creation,
the start of the 2026-09-22 floor batch). Accounts made before CUTOFF were
covered by that batch.

Each run, for every new user not in the done list yet:
  - no TTT2 save yet -> give it a floored save built from TEMPLATE, so the game
    loads 1st Dan on its first login instead of creating a rank 0 save
  - has a TTT2 save  -> floor it while the player is offline, then add the
    account to the done list

A seeded account is not marked done right away. If the game created its own
save before the seed landed, that save is floored on a later run; otherwise
the floor finds nothing to raise and the account is marked done then.

A save can only be edited while its owner is offline (the game overwrites the
edit on its next save), so an online account is simply found again on the next
run. The floor is applied once per account; the done list keeps it from being
applied again after the player is demoted.

The floor itself is tdt_admin.floor_buf() / floor_account(), the same code path
as `tdt_admin.py floor <npid>` (backup, reseal, verify, audit).

    floor_new_users.py --init      create the done list; run once
    floor_new_users.py             one run
    floor_new_users.py --dry-run   one run without writing anything
    floor_new_users.py --status    show new users and whether they are done

The done list (floor_new_users.json) and the template save
(template_new_user.tdt) live next to this script.
"""
import os
import sys
import json
import time
import sqlite3
import argparse
import datetime as dt

HERE = os.path.dirname(os.path.abspath(__file__))
DB_PATH = "/home/ec2-user/rpcn-data/db/rpcn.db"
BACKUP_DIR = "/home/ec2-user/backup/tdt"
STATE = os.path.join(HERE, "floor_new_users.json")
# a fresh save the game made itself (0 matches, rank 0); floored before use
TEMPLATE = os.path.join(HERE, "template_new_user.tdt")
COM_ID = "NPWR02973_00"
SLOT = 1

KST = dt.timezone(dt.timedelta(hours=9))
CUTOFF = dt.datetime(2026, 9, 22, 15, 39, 48, tzinfo=KST)   # backup label pre-floor-20260922-153948


def now():
    return dt.datetime.now().isoformat(timespec="seconds")


def log(msg):
    print(f"{now()}  {msg}", flush=True)


def load_state():
    try:
        with open(STATE, encoding="utf-8") as f:
            return json.load(f)
    except FileNotFoundError:
        return None


def save_state(st):
    tmp = STATE + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(st, f, ensure_ascii=False, indent=1)
    os.replace(tmp, STATE)


def new_users():
    """[(npid, user_id, created, has_save)] for accounts created at or after CUTOFF"""
    con = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True, timeout=2)
    rows = con.execute(
        "SELECT a.username, a.user_id, ts.creation, t.data_id IS NOT NULL FROM account a "
        "JOIN account_timestamp ts ON ts.user_id = a.user_id "
        "LEFT JOIN tus_data t ON t.owner_id = a.user_id "
        "AND CAST(t.communication_id AS TEXT) = ? AND t.slot_id = ? "
        "WHERE ts.creation >= ? ORDER BY ts.creation",
        (COM_ID, SLOT, int(CUTOFF.timestamp()))).fetchall()
    con.close()
    return [(npid, uid, dt.datetime.fromtimestamp(c, KST).isoformat(timespec="seconds"), bool(s))
            for npid, uid, c, s in rows]


def already_floored(npid):
    """a pre-floor backup means the batch or a manual `floor` already raised this account"""
    d = os.path.join(BACKUP_DIR, npid)
    return os.path.isdir(d) and any(f.startswith("pre-floor-") for f in os.listdir(d))


# ----------------------------------------------------------------- seed
def floored_template(ta):
    b = ta.read_save(TEMPLATE)
    _, y, _ = ta.floor_buf(b)
    ta.reseal(b)
    return b, y


def free_data_id(ta, con):
    """an unused id below every file on disk; RPCN's dispenser never hands those out again"""
    used = {r[0] for r in con.execute("SELECT data_id FROM tus_data")}
    used |= {r[0] for r in con.execute("SELECT data_id FROM tus_data_vuser")}
    top = max(int(f[:-4]) for f in os.listdir(ta.TUS_DIR) if f.endswith(".tdt"))
    for i in range(top - 1000, 0, -1):
        if i not in used and not os.path.exists(os.path.join(ta.TUS_DIR, f"{i:020d}.tdt")):
            return i
    ta.die("no free data_id")


def seed(ta, npid, uid, buf, floor):
    """write buf as npid's first TTT2 save; False if the game created one first"""
    con = sqlite3.connect(DB_PATH, timeout=10)
    try:
        data_id = free_data_id(ta, con)
        path = os.path.join(ta.TUS_DIR, f"{data_id:020d}.tdt")
        with open(path, "wb") as f:
            f.write(bytes(buf))
        os.chmod(path, 0o644)
        tick = (int(time.time()) + 62135596800) * 1_000_000
        try:
            with con:
                # plain INSERT: fails if the game already created this account's save
                con.execute(
                    "INSERT INTO tus_data (owner_id, communication_id, slot_id, data_id, data_info, "
                    "timestamp, author_id) VALUES (?, ?, ?, ?, x'', ?, ?)",
                    (uid, COM_ID.encode(), SLOT, data_id, tick, uid))
        except sqlite3.IntegrityError:
            os.unlink(path)
            return False
    finally:
        con.close()
    ta.audit("seed-floor-save", npid, data_id=data_id, template=TEMPLATE, floor=floor,
             md5=ta.md5(path))
    return True


# ------------------------------------------------------------------ run
def run_once(st, dry_run):
    todo = [u for u in new_users() if u[0] not in st["done"]]
    if not todo:
        return

    import tdt_admin as ta

    # accounts that have never saved: seed a floored save before the game makes one
    template = None
    for npid, uid, created, has_save in todo:
        if has_save:
            continue
        if template is None:
            template = floored_template(ta)
        if dry_run:
            log(f"{npid} (created {created}): would seed a floor {template[1]} save")
        elif seed(ta, npid, uid, *template):
            log(f"{npid} (created {created}): seeded a floor {template[1]} save")
        else:
            log(f"{npid} (created {created}): the game saved first, floored on a later run")

    # accounts with a save: floor it once the player is offline
    saved = [(npid, created) for npid, _, created, has_save in todo if has_save]
    if not saved:
        return
    who = ta.online()
    if who is None:
        log("stat server unreachable, cannot tell who is offline; retry next run")
        return
    label = "pre-floor-new-" + dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    for npid, created in saved:
        if npid in who:
            continue
        status, _ = ta.floor_account(npid, who=who, label=label, dry_run=dry_run)
        log(f"{npid} (created {created}): {status}")
        if status in ("applied", "no_change", "likely_demoted") and not dry_run:
            st["done"][npid] = {"ts": now(), "status": status}
            save_state(st)


# ------------------------------------------------------------- commands
def cmd_init():
    if load_state() is not None:
        sys.exit(f"error: {STATE} already exists; delete it to re-init")
    # new users floored before this script existed must not be floored twice
    done = {npid: {"ts": now(), "status": "floored-before-init"}
            for npid, _, _, has_save in new_users() if has_save and already_floored(npid)}
    save_state({"cutoff": CUTOFF.isoformat(), "done": done})
    log(f"init: cutoff {CUTOFF.isoformat()}, {len(done)} accounts already floored")
    for npid in done:
        print(f"  {npid}")


def cmd_status(st):
    rows = new_users()
    print(f"cutoff {st['cutoff']}   new users {len(rows)}   done {len(st['done'])}")
    for npid, _, created, has_save in rows:
        d = st["done"].get(npid)
        state = f"{d['status']} {d['ts']}" if d else ("not done" if has_save else "no save yet")
        print(f"  {npid:20s} created {created}  {state}")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--init", action="store_true", help="create the done list")
    ap.add_argument("--status", action="store_true")
    ap.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()

    if a.init:
        return cmd_init()
    st = load_state()
    if st is None:
        sys.exit(f"error: {STATE} missing; run with --init first")
    if a.status:
        return cmd_status(st)
    run_once(st, a.dry_run)


if __name__ == "__main__":
    sys.path.insert(0, HERE)
    main()
