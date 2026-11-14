#!/usr/bin/env bash
# 하루 한 번 DB 백업(#135). 서버의 cron이 부른다. 최근 7개만 남긴다.
# 설치(서버에서 한 번): crontab에 아래 한 줄을 넣는다. 서버 시간은 UTC라 18:30 = 한국 03:30.
#   30 18 * * * $HOME/onair/deploy/backup-db.sh >> $HOME/db-backup.log 2>&1
# 파일 이름의 날짜는 서버 시간(UTC) 기준이다. 한국 15일 03:30에 만든 파일은 db-backup-daily-<14일>이다.
# 설치한 다음 날 ~/db-backup.log에 ok 줄이 있는지 본다(cron은 로그인할 때와 PATH가 달라 손으로 돌릴 때와 다를 수 있다).
# 되살리기(deploy 폴더에서, 사람이 한 줄씩 확인하며. 지금 DB를 지우므로 먼저 지금 DB도 백업한다):
#   docker compose stop backend
#   docker compose exec -T db psql -U onair -d postgres -c 'DROP DATABASE onair' -c 'CREATE DATABASE onair OWNER onair'
#   gunzip -c <파일> | docker compose exec -T db psql -v ON_ERROR_STOP=1 -U onair -d onair
#   docker compose start backend
set -euo pipefail
cd "$(dirname "$0")"
out="$HOME/db-backup-daily-$(date +%Y%m%d).sql.gz"
# 실패하면 쓰다 만 임시 파일을 지운다
trap 'rm -f "$out.tmp"' ERR
# 다 쓴 뒤에만 이름을 바꾼다. 중간에 실패하면 반쪽 파일이 백업처럼 남지 않는다.
docker compose exec -T db sh -c 'pg_dump -U "$POSTGRES_USER" "$POSTGRES_DB"' </dev/null | gzip > "$out.tmp"
mv "$out.tmp" "$out"
# 새것부터 7개를 남기고 나머지를 지운다(배포 전 손으로 만든 db-backup-<날짜-시각> 파일은 건드리지 않는다)
ls -1t "$HOME"/db-backup-daily-*.sql.gz | tail -n +8 | xargs -r rm --
echo "$(date -u +%FT%TZ) ok $out $(stat -c %s "$out")"
