#!/usr/bin/env python3
"""Read Tekken Tag Tournament 2 character ranks from one TDT file.

This script is self-contained and uses only the Python standard library, so it
can be copied to a Linux server with a single ``.tdt`` file for verification.
"""

from __future__ import print_function

import argparse
import hashlib
import json
import sys
from pathlib import Path


CHARACTER_COUNT = 59
CHARACTER_RECORD_OFFSET = 0x70
CHARACTER_RECORD_SIZE = 0x30

# TTT2 internal character ID -> display name.
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

# TTT2 rank code -> display name and Korean rank tier.
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


def parse_args():
    parser = argparse.ArgumentParser(
        description="Print TTT2 character ranks from a single .tdt file.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("tdt_file", type=Path, help="Path to the .tdt file")
    parser.add_argument("--format", choices=("table", "json"), default="table", help="Output format")
    parser.add_argument("--only-ranked", action="store_true", help="Hide Beginner rank entries")
    return parser.parse_args()


def read_ranks(data):
    required_size = CHARACTER_RECORD_OFFSET + CHARACTER_RECORD_SIZE * (CHARACTER_COUNT - 1) + 1
    if len(data) < required_size:
        raise ValueError(
            "TDT file is too short for {0} character records: {1} bytes".format(
                CHARACTER_COUNT, len(data)
            )
        )

    result = []
    for character_id in range(CHARACTER_COUNT):
        rank_code = data[CHARACTER_RECORD_OFFSET + CHARACTER_RECORD_SIZE * character_id]
        rank_name, tier = RANKS.get(rank_code, ("Unknown ({0})".format(rank_code), "Unknown"))
        result.append(
            {
                "character_id": character_id,
                "character": CHARACTERS.get(character_id, "Unknown (0x{0:02X})".format(character_id)),
                "rank_code": rank_code,
                "rank": rank_name,
                "tier": tier,
            }
        )
    return result


def print_table(rows):
    print("{0:>2}  {1:<14} {2:<18} {3}".format("ID", "Character", "Rank", "Code"))
    print("--  -------------- ------------------ ----")
    for row in rows:
        print(
            "{0:>2}  {1:<14} {2:<18} {3:>4}".format(
                row["character_id"], row["character"], row["rank"], row["rank_code"]
            )
        )


def main():
    args = parse_args()
    try:
        sys.stdout.reconfigure(encoding="utf-8")
    except AttributeError:
        pass

    try:
        data = args.tdt_file.read_bytes()
        rows = read_ranks(data)
    except (OSError, ValueError) as error:
        print("Error: {0}".format(error), file=sys.stderr)
        return 1

    if args.only_ranked:
        rows = [row for row in rows if row["rank_code"] != 0]

    if args.format == "json":
        print(
            json.dumps(
                {
                    "file": str(args.tdt_file),
                    "size_bytes": len(data),
                    "sha256": hashlib.sha256(data).hexdigest(),
                    "ranks": rows,
                },
                ensure_ascii=False,
                indent=2,
            )
        )
    else:
        print("File: {0}".format(args.tdt_file))
        print("Size: {0} bytes".format(len(data)))
        print("SHA-256: {0}".format(hashlib.sha256(data).hexdigest()))
        print_table(rows)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
