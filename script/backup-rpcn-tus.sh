#!/usr/bin/env bash
# RPCN tus_data 폴더를 하루 한 번 tar.gz로 백업하고 7일치만 남긴다.
set -Eeuo pipefail

TARGET_DIR="/home/ec2-user/backup/rpcn-tus"
ORIGIN_DIR="/home/ec2-user/rpcn-data/tus_data"
RETENTION_DAYS=7

mkdir -p "$TARGET_DIR"

# 겹치는 실행을 막는다.
exec 9>"$TARGET_DIR/.backup.lock"
if ! flock -n 9; then
    echo "이미 백업이 실행 중입니다. 이번 실행은 건너뜁니다."
    exit 0
fi

TODAY=$(date +%Y%m%d)
BACKUP_FILE="$TARGET_DIR/tus-$TODAY.tar.gz"

ARCHIVE_TMP=""
cleanup() {
    if [[ -n "$ARCHIVE_TMP" ]]; then
        rm -f -- "$ARCHIVE_TMP"
    fi
}
trap cleanup EXIT

# 같은 날 재실행되더라도 정상 백업을 덮어쓰지 않는다.
if [[ ! -e "$BACKUP_FILE" ]]; then
    ARCHIVE_TMP=$(mktemp "$TARGET_DIR/.tus-$TODAY.XXXXXX.tar.gz.tmp")

    # 서버가 도는 중이라 읽는 사이 파일이 생기거나 지워질 수 있다. tar는 그때 1을 돌려주므로
    # 1은 정상으로 보고 2 이상만 실패로 처리한다.
    RC=0
    tar -czf "$ARCHIVE_TMP" -C "$(dirname "$ORIGIN_DIR")" \
        --warning=no-file-changed --warning=no-file-removed \
        "$(basename "$ORIGIN_DIR")" || RC=$?
    if (( RC > 1 )); then
        echo "tar 실패 (exit $RC)" >&2
        exit "$RC"
    fi

    mv -- "$ARCHIVE_TMP" "$BACKUP_FILE"
    ARCHIVE_TMP=""
    echo "백업 완료: $BACKUP_FILE ($(du -h "$BACKUP_FILE" | cut -f1))"
else
    echo "이미 존재하는 백업입니다: $BACKUP_FILE"
fi

# 오늘을 포함해 총 7개 날짜 범위만 보존한다.
OLDEST_KEEP_DAY=$(date -d "$((RETENTION_DAYS - 1)) days ago" +%Y%m%d)

while IFS= read -r ARCHIVE_NAME; do
    ARCHIVE_DAY=${ARCHIVE_NAME:4:8}
    if [[ "$ARCHIVE_DAY" < "$OLDEST_KEEP_DAY" ]]; then
        rm -f -- "$TARGET_DIR/$ARCHIVE_NAME"
        echo "보관기한 지남, 삭제: $ARCHIVE_NAME"
    fi
done < <(find "$TARGET_DIR" -maxdepth 1 -type f -name 'tus-????????.tar.gz' -printf '%f\n')
