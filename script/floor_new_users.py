#!/usr/bin/env python3
"""Apply the tier rank floor to TTT2 accounts created after the one-time pass.

A new user is an account created at or after CUTOFF (account_timestamp.creation,
the start of the 2026-09-22 floor batch) that has a TTT2 save. Accounts made
before CUTOFF were covered by that batch.

Each run:
  1. find new users that are not in the done list yet
  2. floor the ones that are offline and add them to the done list

A save can only be edited while its owner is offline (the game overwrites the
edit on its next save), so an online account is simply found again on the next
run. The floor is applied once per account; the done list keeps it from being
applied again after the player is demoted.

The floor itself is tdt_admin.floor_account(), the same code path as
`tdt_admin.py floor <npid>` (backup, reseal, verify, audit).

    floor_new_users.py --init      create the done list; run once
    floor_new_users.py             one run
    floor_new_users.py --dry-run   one run without writing anything
    floor_new_users.py --status    show new users and whether they are done
"""
import os
import sys
import json
import sqlite3
import argparse
import datetime as dt

DB_PATH = "/home/ec2-user/rpcn-data/db/rpcn.db"
BACKUP_DIR = "/home/ec2-user/backup/tdt"
STATE = "/home/ec2-user/backup/tdt/floor_new_users.json"
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
    """[(npid, created)] for accounts created at or after CUTOFF that have a TTT2 save"""
    con = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True, timeout=2)
    rows = con.execute(
        "SELECT a.username, ts.creation FROM account a "
        "JOIN account_timestamp ts ON ts.user_id = a.user_id "
        "JOIN tus_data t ON t.owner_id = a.user_id "
        "WHERE ts.creation >= ? AND CAST(t.communication_id AS TEXT) = ? AND t.slot_id = ? "
        "ORDER BY ts.creation",
        (int(CUTOFF.timestamp()), COM_ID, SLOT)).fetchall()
    con.close()
    return [(npid, dt.datetime.fromtimestamp(c, KST).isoformat(timespec="seconds"))
            for npid, c in rows]


def already_floored(npid):
    """a pre-floor backup means the batch or a manual `floor` already raised this account"""
    d = os.path.join(BACKUP_DIR, npid)
    return os.path.isdir(d) and any(f.startswith("pre-floor-") for f in os.listdir(d))


# ------------------------------------------------------------------ run
def run_once(st, dry_run):
    todo = [(npid, c) for npid, c in new_users() if npid not in st["done"]]
    if not todo:
        return

    import tdt_admin as ta
    who = ta.online()
    if who is None:
        log("stat server unreachable, cannot tell who is offline; retry next run")
        return

    label = "pre-floor-new-" + dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    for npid, created in todo:
        if npid in who:
            continue
        status, _ = ta.floor_account(npid, who=who, label=label, dry_run=dry_run)
        log(f"{npid} (created {created}): {status}")
        if status in ("applied", "no_change") and not dry_run:
            st["done"][npid] = {"ts": now(), "status": status}
            save_state(st)


# ------------------------------------------------------------- commands
def cmd_init():
    if load_state() is not None:
        sys.exit(f"error: {STATE} already exists; delete it to re-init")
    # new users floored before this script existed must not be floored twice
    done = {npid: {"ts": now(), "status": "floored-before-init"}
            for npid, _ in new_users() if already_floored(npid)}
    save_state({"cutoff": CUTOFF.isoformat(), "done": done})
    log(f"init: cutoff {CUTOFF.isoformat()}, {len(done)} accounts already floored")
    for npid in done:
        print(f"  {npid}")


def cmd_status(st):
    rows = new_users()
    print(f"cutoff {st['cutoff']}   new users {len(rows)}   done {len(st['done'])}")
    for npid, created in rows:
        d = st["done"].get(npid)
        print(f"  {npid:20s} created {created}  " + (f"{d['status']} {d['ts']}" if d else "not done"))


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
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    main()
