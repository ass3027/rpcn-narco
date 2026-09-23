import ipaddress
import json
import logging
import os
import subprocess
import time
from pathlib import Path
from logging.handlers import TimedRotatingFileHandler

import requests


STATS_URL = "http://127.0.0.1:31314/rpcn_stats/usage"
RPCN_PORT = 31313
ZONE = "public"

POLL_INTERVAL = 5
LOOKUP_INTERVAL = 1.5
MAX_LOOKUPS_PER_CYCLE = 4

LOG_DIR = Path("/var/log/rpcn-vpn-monitor")
CACHE_FILE = Path("/home/ec2-user/rpcn-vpn-monitor/checked-ips.jsonl")
LOG_DIR.mkdir(parents=True, exist_ok=True)

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)s %(message)s",
    handlers=[
        TimedRotatingFileHandler(
            LOG_DIR / "rpcn-vpn-monitor.log",
            when="midnight",
            backupCount=30,
            encoding="utf-8",
        ),
        logging.StreamHandler(),
    ],
)
logger = logging.getLogger("rpcn-vpn-monitor")


def append_record(record):
    CACHE_FILE.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with CACHE_FILE.open("a", encoding="utf-8") as cache_file:
        json.dump(record, cache_file, ensure_ascii=False, sort_keys=True)
        cache_file.write("\n")
        cache_file.flush()
        os.fsync(cache_file.fileno())


def load_state():
    checked_ips = set()
    try:
        with CACHE_FILE.open(encoding="utf-8") as cache_file:
            for line in cache_file:
                record = json.loads(line)
                checked_ips.add(record["ip"])
    except FileNotFoundError:
        return checked_ips
    return checked_ips


def get_players(session):
    response = session.get(STATS_URL, timeout=5)
    response.raise_for_status()

    return {
        (name, str(ipaddress.ip_address(ip.strip())))
        for players in response.json().get("players_id", {}).values()
        for name, ip in players.items()
    }


def lookup_ip(session, ip, next_lookup_at):
    # 무료 API 호출 한도를 넘지 않도록 다음 조회 시점까지 대기한다.
    now = time.monotonic()
    lookup_delay = next_lookup_at - now
    if lookup_delay > 0:
        time.sleep(lookup_delay)

    next_lookup_at = time.monotonic() + LOOKUP_INTERVAL

    response = session.get(
        f"http://ip-api.com/json/{ip}",
        params={"fields": "status,message,proxy,hosting,isp,country"},
        timeout=3,
    )

    if response.status_code == 429 or response.headers.get("X-Rl") == "0":
        next_lookup_at = time.monotonic() + int(
            response.headers.get("X-Ttl", "60")
        ) + 1

    response.raise_for_status()
    data = response.json()

    if data.get("status") != "success":
        raise requests.RequestException(data.get("message", "IP 판정 API 응답 오류"))

    return data, next_lookup_at


def is_vpn(session, ip, next_lookup_at):
    data, next_lookup_at = lookup_ip(session, ip, next_lookup_at)
    blocked = bool(data.get("proxy") or data.get("hosting"))
    return blocked, data, next_lookup_at


def block_ip(ip):
    family = f"ipv{ipaddress.ip_address(ip).version}"

    rule = (
        f'rule family="{family}" priority="-100" '
        f'source address="{ip}" '
        f'port port="{RPCN_PORT}" protocol="tcp" drop'
    )

    # 즉시 적용하고, 재부팅·reload 이후에도 유지한다.
    for options in ([], ["--permanent"]):
        subprocess.run(
            (
                "firewall-cmd",
                "--quiet",
                f"--zone={ZONE}",
                *options,
                f"--add-rich-rule={rule}",
            ),
            check=True,
            timeout=10,
        )

    # 방화벽 규칙 추가만으로 남을 수 있는 기존 TCP 연결을 종료한다.
    subprocess.run(
        ("ss", "-K", "-t", "src", ip, "dport", "=", f":{RPCN_PORT}"),
        check=True,
        timeout=10,
    )


def monitor_once(session, checked_ips, next_lookup_at):
    players = get_players(session)
    unknown_ips = sorted({
        ip for _, ip in players
        if ip not in checked_ips and ip != "0.0.0.0"
    })

    for ip in unknown_ips[:MAX_LOOKUPS_PER_CYCLE]:
        player_ids = sorted(
            player_id for player_id, address in players
            if address == ip
        )
        blocked, data, next_lookup_at = is_vpn(session, ip, next_lookup_at)

        logger.info(
            "IP=%s 계정=%s 국가=%s ISP=%s 차단대상=%s",
            ip,
            player_ids,
            data.get("country", "unknown"),
            data.get("isp", "unknown"),
            blocked,
        )

        if blocked:
            block_ip(ip)
            logger.warning("IP 차단 처리: %s 계정=%s", ip, player_ids)

        append_record({
            "ip": ip,
            "player_ids": player_ids,
            "blocked": blocked,
        })
        checked_ips.add(ip)

    return next_lookup_at


def monitor():
    checked_ips = load_state()
    next_lookup_at = 0

    with requests.Session() as session:
        while True:
            try:
                next_lookup_at = monitor_once(session, checked_ips, next_lookup_at)
            except (requests.RequestException, subprocess.SubprocessError) as error:
                logger.error("처리 실패: %s", error, exc_info=True)

            time.sleep(POLL_INTERVAL)


if __name__ == "__main__":
    monitor()
