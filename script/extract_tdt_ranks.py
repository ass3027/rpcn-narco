#!/usr/bin/env python3
"""Extract Tekken Tag Tournament 2 character ranks from an RPCN TDT archive."""

from __future__ import annotations

import argparse
import json
import sqlite3
import sys
import tarfile
from pathlib import Path

from tdt_admin import REC, decode


def read_ranks(data: bytes) -> list[dict]:
    if len(data) != REC:
        raise ValueError(f"expected {REC} bytes, got {len(data)}")
    return decode(bytearray(data), all_chars=True)["chars"]


def print_table(rows: list[dict]) -> None:
    print(f"{'ID':>2}  {'Character':<14} {'Rank':<18} Code")
    print("--  -------------- ------------------ ----")
    for row in rows:
        print(f"{row['id']:>2}  {row['character']:<14} {row['rank_name']:<18} {row['rank']:>4}")


TAG2_COMMUNICATION_ID = b"NPWR02973_00"

SCRIPT_DIR = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIR.parent
DEFAULT_DB_PATH = REPOSITORY_ROOT / "db" / "rpcn.db"
DEFAULT_TAR_PATH = REPOSITORY_ROOT / "tus_data.tar"


def find_data_id(db_path: Path, username: str) -> tuple[int, int]:
    if not db_path.is_file():
        raise FileNotFoundError(f"Database file not found: {db_path}")

    db_uri = f"{db_path.resolve().as_uri()}?mode=ro"
    with sqlite3.connect(db_uri, uri=True) as connection:
        users = connection.execute(
            "SELECT user_id FROM account WHERE username = ? COLLATE NOCASE",
            (username,),
        ).fetchall()

        if not users:
            raise LookupError(f"No account found for username: {username}")
        if len(users) > 1:
            raise LookupError(f"More than one account matched username: {username}")

        user_id = users[0][0]
        row = connection.execute(
            """
            SELECT data_id
            FROM tus_data
            WHERE owner_id = ? AND communication_id = ?
            """,
            (user_id, TAG2_COMMUNICATION_ID),
        ).fetchone()

    if row is None:
        raise LookupError(f"No Tekken Tag 2 TUS data found for username: {username}")

    return user_id, row[0]


def read_tdt_data(tar_path: Path, data_id: int) -> bytes:
    if not tar_path.is_file():
        raise FileNotFoundError(f"TAR file not found: {tar_path}")

    expected_suffix = f"/{data_id:020d}.tdt"
    with tarfile.open(tar_path) as archive:
        member = next(
            (
                candidate
                for candidate in archive.getmembers()
                if candidate.isfile() and candidate.name.endswith(expected_suffix)
            ),
            None,
        )
        if member is None:
            raise LookupError(f"TDT file for data_id {data_id} was not found in {tar_path}")

        extracted = archive.extractfile(member)
        if extracted is None:
            raise RuntimeError(f"Could not read TDT file: {member.name}")
        return extracted.read()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Extract TTT2 character ranks from rpcn.db and tus_data.tar.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("username", help="RPCN account username to extract")
    parser.add_argument("--db", type=Path, default=DEFAULT_DB_PATH, help="Path to rpcn.db")
    parser.add_argument("--tar", type=Path, default=DEFAULT_TAR_PATH, help="Path to tus_data.tar")
    parser.add_argument("--format", choices=("table", "json"), default="table", help="Output format")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        user_id, data_id = find_data_id(args.db, args.username)
        rows = read_ranks(read_tdt_data(args.tar, data_id))
    except (FileNotFoundError, LookupError, RuntimeError, ValueError, sqlite3.Error, tarfile.TarError) as error:
        print(f"Error: {error}", file=sys.stderr)
        return 1

    if args.format == "json":
        print(json.dumps({"username": args.username, "user_id": user_id, "data_id": data_id, "ranks": rows}, ensure_ascii=False, indent=2))
    else:
        print(f"Username: {args.username} (user_id={user_id}, data_id={data_id})")
        print_table(rows)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
