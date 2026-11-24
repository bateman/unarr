#!/usr/bin/env bash
# Run a command as a user of its own that cannot become root, by default
# with no network but the loopback: how the workflows of issue #9 run an
# original nobody vetted (upstream/README.md, "The workflow: up to the
# draft PR").
#
#   sudo scripts/run-unprivileged.sh [--network] USER COMMAND [ARG...]
#
# On a GitHub-hosted runner the job's user has sudo and the docker socket,
# so whatever runs as that user can read the runner's memory, and with it
# the token that writes caches and artifacts, whatever `permissions:` says.
# Run as root (sudo), after every step that needs root; the command runs:
#
# - as USER, created when missing (a system user with a home, in no group
#   but its own); a USER that is root or in the groups sudo, admin, wheel,
#   docker, adm or lxd is refused;
# - with no_new_privs: sudo and every setuid binary stop working for it and
#   its children, so it cannot become root again;
# - with a clean environment (setpriv --reset-env): no token, no variable
#   of the job reaches it; PATH is the system's;
# - without --network, in a network namespace of its own where only the
#   loopback is up: the fixture scripts' mock answers on 127.0.0.1, nothing
#   else is reachable (a recording needs no network). With --network it
#   keeps the runner's network, for what must reach a real service.
#
# Every process of USER left behind is killed on the way out, so nothing
# the command started outlives it into the steps that follow. The exit
# code is the command's. The directory it starts in is the current one,
# which USER must be able to read (and write, where the command writes).
set -euo pipefail

die() { echo "[run-unprivileged] $*" >&2; exit 2; }

network=0
if [ "${1:-}" = "--network" ]; then
    network=1
    shift
fi
[ "$#" -ge 2 ] || die "usage: sudo $0 [--network] USER COMMAND [ARG...]"
[ "$(id -u)" = 0 ] || die "run as root (sudo)"
user="$1"
shift
case "$user" in
    root | "" | *[!a-z0-9_-]*) die "not a user name for this: $user" ;;
esac
if ! id "$user" >/dev/null 2>&1; then
    useradd --system --create-home --user-group --shell /usr/sbin/nologin "$user"
fi
uid="$(id -u "$user")"
gid="$(id -g "$user")"
[ "$uid" != 0 ] || die "$user is root"
for group in $(id -nG "$user"); do
    case "$group" in
        sudo | admin | wheel | docker | adm | lxd | root) die "$user is in the group $group, which leads to root" ;;
    esac
done

# The loopback up, by the ioctl `ip link set lo up` makes: no iproute2
# needed, python3 is there for the fixture scripts anyway.
bring_lo_up='
import fcntl, socket, struct
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
req = struct.pack("16sH14s", b"lo", 0, bytes(14))
flags = struct.unpack("16sH", fcntl.ioctl(s, 0x8913, req)[:18])[1]
fcntl.ioctl(s, 0x8914, struct.pack("16sH14s", b"lo", flags | 1, bytes(14)))
'
# --reset-env leaves PATH without the sbin directories, where ldconfig is:
# the fixture scripts look for libicu with it.
drop='
    dir="$1"; uid="$2"; gid="$3"; shift 3
    cd "$dir"
    exec setpriv --reuid="$uid" --regid="$gid" --init-groups --no-new-privs --reset-env -- \
        env PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin "$@"
'

code=0
if [ "$network" = 1 ]; then
    /bin/sh -ec "$drop" sh "$PWD" "$uid" "$gid" "$@" || code=$?
else
    unshare --net -- /bin/sh -ec "python3 -I -c \"\$0\"; $drop" "$bring_lo_up" "$PWD" "$uid" "$gid" "$@" || code=$?
fi

for _ in $(seq 50); do
    pkill -KILL -u "$uid" 2>/dev/null || break
    sleep 0.2
done
pgrep -u "$uid" >/dev/null && die "processes of $user still running after the command"
exit "$code"
