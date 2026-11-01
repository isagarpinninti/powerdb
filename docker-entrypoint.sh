#!/bin/sh
# Starts PowerDB as an unprivileged user, whatever owns the data directory.
#
# A bind-mounted host folder (-v /srv/powerdb:/data) belongs to root or to the host user, so the unprivileged
# `powerdb` user could not write to it. When this runs as root it hands /data to that user (only if it is not
# theirs already, so a large database is not re-walked on every start) and drops privileges. When the container is
# started as a specific user (--user 1000:1000) there is nothing to fix: it runs as that user.
set -eu
DATA="${POWERDB_DATA:-/data}"
if [ "$(id -u)" = 0 ]; then
  mkdir -p "$DATA"
  if [ "$(stat -c %u "$DATA")" != 10001 ]; then
    chown -R 10001:10001 "$DATA"
  fi
  exec setpriv --reuid=10001 --regid=10001 --clear-groups "$@"
fi
exec "$@"
