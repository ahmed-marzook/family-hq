#!/bin/sh
# Starts family-hq as PUID:PGID (default 1000:1000), after making sure that
# user can write to /data.
#
# Why: the database folder is usually a bind mount like
# /opt/stacks/family-hq/data. If it didn't exist, Docker created it owned by
# root, and a non-root user can't create the database in it. So the
# container starts as root, fixes the folder's owner, then drops to the
# normal user before running the program. (The same idea as linuxserver.io
# images' PUID/PGID.)
set -eu

PUID="${PUID:-1000}"
PGID="${PGID:-1000}"

# Already started as a non-root user (compose `user:` set): nothing we're
# allowed to change, so just run it.
if [ "$(id -u)" != "0" ]; then
    exec family-hq "$@"
fi

# Only touch ownership if it's wrong, so restarts on a big folder stay fast.
# The backup folder (BACKUP_DIR, usually a second bind mount) needs the same.
for dir in /data ${BACKUP_DIR:-}; do
    mkdir -p "$dir"
    if [ "$(stat -c '%u:%g' "$dir")" != "$PUID:$PGID" ]; then
        echo "entrypoint: giving $dir to $PUID:$PGID"
        chown -R "$PUID:$PGID" "$dir"
    fi
done

# `exec` replaces this script, so family-hq becomes PID 1 and receives the
# stop signal directly. `setpriv` switches user and group, and
# --clear-groups drops root's extra groups.
exec setpriv --reuid="$PUID" --regid="$PGID" --clear-groups family-hq "$@"
