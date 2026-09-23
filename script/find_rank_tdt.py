#!/usr/bin/env python3
"""Find recent TDT files with a specific character rank."""

from __future__ import annotations

import argparse
import json
from datetime import datetime
from pathlib import Path

import read_tdt_ranks as tdt


def character_ids(value: str) -> tuple[int, ...]:
    matches = tuple(
        character_id
        for character_id, name in tdt.CHARACTERS.items()
        if name.casefold() == value.casefold()
    )
    if not matches:
        raise argparse.ArgumentTypeError(f"Unknown character: {value}")
    return matches


def rank_id(value: str) -> int:
    for candidate, (name, _) in tdt.RANKS.items():
        if name.casefold() == value.casefold():
            return candidate
    raise argparse.ArgumentTypeError(f"Unknown rank: {value}")


def positive_int(value: str) -> int:
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return number


def non_negative_int(value: str) -> int:
    number = int(value)
    if number < 0:
        raise argparse.ArgumentTypeError("must be at least 0")
    return number


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Find recent TDT files with a specific character rank.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("directory", type=Path, help="Directory containing .tdt files")
    parser.add_argument("character", type=character_ids, help="Character name")
    parser.add_argument("rank", type=rank_id, help="Rank name")
    parser.add_argument("--limit", type=positive_int, default=10, help="Number of recent files to inspect")
    parser.add_argument("--offset", type=non_negative_int, default=0, help="Number of recent files to skip")
    parser.add_argument(
        "--exclude",
        nargs=2,
        action="append",
        default=[],
        metavar=("CHARACTER", "RANK"),
        help="Exclude files where CHARACTER has RANK; may be repeated",
    )
    parser.add_argument("--format", choices=("table", "json"), default="table", help="Output format")
    args = parser.parse_args()
    try:
        args.exclude = tuple(
            (character_ids(character), rank_id(rank))
            for character, rank in args.exclude
        )
    except argparse.ArgumentTypeError as error:
        parser.error(str(error))
    return args


def read_rank(path: Path, character_id: int) -> int:
    offset = tdt.CHARACTER_RECORD_OFFSET + tdt.CHARACTER_RECORD_SIZE * character_id
    with path.open("rb") as tdt_file:
        tdt_file.seek(offset)
        rank = tdt_file.read(1)

    if not rank:
        raise ValueError(f"TDT file is too short: {path}")
    return rank[0]


def main() -> int:
    args = parse_args()

    if not args.directory.is_dir():
        raise NotADirectoryError(args.directory)

    files = sorted(
        args.directory.glob("*.tdt"),
        key=lambda path: path.stat().st_mtime_ns,
        reverse=True,
    )
    matches = []
    errors = []
    excluded = 0
    selected_files = files[args.offset:args.offset + args.limit]
    last_scanned_at = None
    if selected_files:
        last_scanned_at = datetime.fromtimestamp(
            selected_files[-1].stat().st_mtime
        ).astimezone().isoformat(timespec="seconds")

    for path in selected_files:
        try:
            is_excluded = any(
                read_rank(path, character_id) == excluded_rank
                for excluded_characters, excluded_rank in args.exclude
                for character_id in excluded_characters
            )
            if is_excluded:
                excluded += 1
                continue

            matched = any(
                read_rank(path, character_id) == args.rank
                for character_id in args.character
            )
        except ValueError as error:
            errors.append({"file": path.name, "error": str(error)})
            continue

        if not matched:
            continue

        modified_at = datetime.fromtimestamp(path.stat().st_mtime).astimezone()
        matches.append({
            "file": path.name,
            "modified_at": modified_at.isoformat(timespec="seconds"),
        })

    character = tdt.CHARACTERS[args.character[0]]
    rank = tdt.RANKS[args.rank][0]

    if args.format == "json":
        print(json.dumps({
            "character_ids": args.character,
            "character": character,
            "rank_id": args.rank,
            "rank": rank,
            "offset": args.offset,
            "last_scanned_at": last_scanned_at,
            "scanned": len(selected_files),
            "excluded": excluded,
            "matches": matches,
            "errors": errors,
        }, ensure_ascii=False, indent=2))
        return 0

    character_id_text = ", ".join(map(str, args.character))
    print(f"Character: {character} ({character_id_text})")
    print(f"Rank: {rank} ({args.rank})")
    print(f"Scanned: {len(selected_files)} (offset={args.offset})")
    print(f"Excluded: {excluded}")
    print(f"Last scanned at: {last_scanned_at}")
    print(f"Matches: {len(matches)}")
    for match in matches:
        print(f"{match['modified_at']}  {match['file']}")
    print(f"Errors: {len(errors)}")
    for error in errors:
        print(f"{error['file']}  {error['error']}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
