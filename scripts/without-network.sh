#!/usr/bin/env bash
# Run a command with no network but the loopback, as a user of its own,
# with no way back to root: the sandbox the workflow `upstream` records
# a new original in (issue #9; upstream/README.md, "The workflow").
#
#   sudo scripts/without-network.sh USER COMMAND [ARG...]
#
# Run as root (sudo). The command runs:
#
# - in a network namespace of its own, where only the loopback is up: the
#   fixture scripts' mock answers on 127.0.0.1, nothing else is reachable
#   (the recording needs no network, upstream/README.md);
# - as USER, an unprivileged account that owns nothing of the runner, with
#   a clean environment (setpriv --reset-env: no token, no variable of the
#   job reaches it);
# - with no_new_privs set: sudo and every setuid binary stop working for
#   it and its children, so it cannot become root again and leave the
#   namespace.
#
# Every process of USER left behind is killed on the way out, so nothing
# the command started outlives it into the steps that follow. The exit
# code is the command's. The directory it starts in is the current one.
set -euo pipefail

die() { echo "[without-network] $*" >&2; exit 2; }

[ "$#" -ge 2 ] || die "usage: sudo $0 USER COMMAND [ARG...]"
[ "$(id -u)" = 0 ] || die "run as root (sudo): the network namespace needs it"
user="$1"
shift
uid="$(id -u "$user" 2>/dev/null)" || die "no user $user"
[ "$uid" != 0 ] || die "$user is root: the command must run as an unprivileged user"
gid="$(id -g "$user")"

# The loopback up, by the ioctl `ip link set lo up` makes: no iproute2
# needed, python3 is there for the fixture scripts anyway.
bring_lo_up='
import fcntl, socket, struct
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
req = struct.pack("16sH14s", b"lo", 0, bytes(14))
flags = struct.unpack("16sH", fcntl.ioctl(s, 0x8913, req)[:18])[1]
fcntl.ioctl(s, 0x8914, struct.pack("16sH14s", b"lo", flags | 1, bytes(14)))
'

code=0
unshare --net -- /bin/sh -ec '
    python3 -I -c "$1"
    shift
    dir="$1"; uid="$2"; gid="$3"; shift 3
    cd "$dir"
    # --reset-env leaves PATH without the sbin directories, where ldconfig
    # is: the fixture scripts look for libicu with it.
    exec setpriv --reuid="$uid" --regid="$gid" --init-groups --no-new-privs --reset-env -- \
        env PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin "$@"
' sh "$bring_lo_up" "$PWD" "$uid" "$gid" "$@" || code=$?

for _ in $(seq 50); do
    pkill -KILL -u "$uid" 2>/dev/null || break
    sleep 0.2
done
pgrep -u "$uid" >/dev/null && die "processes of $user still running after the command"
exit "$code"
