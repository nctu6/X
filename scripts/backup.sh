#!/usr/bin/env bash
# Daily backup of xfeed runtime data -> local + Google Drive.
#
# data/ is not in git, so this backup is the off-host copy of:
#   data.tar.gz          data/*.jsonl, data/*.state.json, data/pageviews.json.
#                        Each file is copied and parsed first (JSON or JSONL),
#                        retried so an atomic tmp+rename in flight is not stored
#                        half-written.
#   media.tar.gz         data/media/ (saved images; *.tmp skipped)
#   secrets.tar.gz.age   config.yml (bearer token), Drive creds, age.pub,
#                        crontab, the installed scripts (age-encrypted to the
#                        public key only)
#   MANIFEST.txt, SHA256SUMS
# Restore with scripts/restore.sh (see docs/RESTORE.md).
# The installed copy is $X_BACKUP_BIN/x_backup.sh; scripts/backup.sh is the
# versioned one. Snapshots are written to <repo>/backups/ (git-ignored).
#
# After a successful upload every local snapshot whose Drive copy verifies
# (gdrive.py verify: same folder name, every file with the same md5 and size,
# SHA256SUMS present) is deleted locally, since Drive holds it. Anything not
# verified stays, and the LOCAL_KEEP_DAYS prune is the fallback.
#   X_BACKUP_LOCAL_DRY_RUN=1   log the keep/delete decisions, delete nothing
#   x_backup.sh prune-local    run only that step (no new snapshot, no upload)
set -Eeuo pipefail
umask 077

# X_DIR: the repo this script sits in (scripts/backup.sh). The installed copy
# (in $X_BACKUP_BIN) has no repo next to it, so cron must set X_DIR.
SCRIPT_REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
if [ -z "${X_DIR:-}" ]; then
    if [ -f "$SCRIPT_REPO/Cargo.toml" ]; then
        X_DIR=$SCRIPT_REPO
    else
        echo "backup: set X_DIR to the xfeed checkout" >&2
        exit 2
    fi
fi
BACKUP_ROOT=${X_BACKUP_ROOT:-$X_DIR/backups}
# Credentials and the age public key; installed scripts.
CONF_DIR=${X_BACKUP_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/x-backup}
BIN_DIR=${X_BACKUP_BIN:-$HOME/bin}
LOG="$BACKUP_ROOT/backup.log"
if [ -z "${X_GDRIVE:-}" ]; then
    if [ -f "$BIN_DIR/x_gdrive.py" ]; then
        GDRIVE=$BIN_DIR/x_gdrive.py
    else
        GDRIVE=$SCRIPT_REPO/scripts/gdrive.py
    fi
else
    GDRIVE=$X_GDRIVE
fi
export X_GDRIVE_CREDS=${X_GDRIVE_CREDS:-$CONF_DIR/gdrive.json}
export X_GDRIVE_ROOT=${X_GDRIVE_ROOT:-X-backup}
AGE_RECIPIENT_FILE=${X_AGE_PUB:-$CONF_DIR/age.pub}
LOCAL_KEEP_DAYS=${X_BACKUP_LOCAL_KEEP_DAYS:-7}
REMOTE_KEEP_DAYS=${X_BACKUP_REMOTE_KEEP_DAYS:-90}
LOCAL_DRY_RUN=${X_BACKUP_LOCAL_DRY_RUN:-}
MODE=${1:-backup}
case "$MODE" in
backup | prune-local) ;;
*)
    echo "usage: $0 [prune-local]" >&2
    exit 2
    ;;
esac
export PATH=/usr/local/bin:/usr/bin:/bin:$PATH

mkdir -p "$BACKUP_ROOT"
touch "$LOG"
chmod 600 "$LOG"
if [ -n "${X_BACKUP_STDOUT:-}" ]; then
    exec > >(tee -a "$LOG") 2>&1
else
    exec >>"$LOG" 2>&1
fi
TS=$(date +%Y%m%d_%H%M%S)
DEST="$BACKUP_ROOT/$TS"
log() { echo "[$(date '+%F %T %Z')] $*"; }
trap 'log "FAILED (line $LINENO, exit $?)"' ERR

# Delete local snapshots that are confirmed on Drive. A snapshot is removed
# only when `gdrive.py verify` exits 0 for it; any failure keeps it.
prune_uploaded() {
    local d name kb freed=0 kept=0 deleted=0 tag=""
    [ -n "$LOCAL_DRY_RUN" ] && tag=" [dry-run]"
    log "local dedupe$tag: checking snapshots in $BACKUP_ROOT against gdrive:${X_GDRIVE_ROOT}"
    if ! python3 "$GDRIVE" verify --help >/dev/null 2>&1; then
        log "local dedupe: $GDRIVE has no verify command; keeping all local snapshots"
        return 0
    fi
    for d in "$BACKUP_ROOT"/20*_*; do
        name=${d##*/}
        [[ $name =~ ^[0-9]{8}_[0-9]{6}$ ]] || continue
        [ -d "$d" ] && [ ! -L "$d" ] || continue
        kb=$(du -sk -- "$d" | cut -f1)
        if python3 "$GDRIVE" verify "$name" --local-dir "$d"; then
            freed=$((freed + kb))
            deleted=$((deleted + 1))
            if [ -n "$LOCAL_DRY_RUN" ]; then
                log "local dedupe: would delete $name (${kb}K, verified on Drive)"
            else
                rm -rf -- "$d"
                log "local dedupe: deleted $name (${kb}K, verified on Drive)"
            fi
        else
            kept=$((kept + 1))
            log "local dedupe: keep $name (${kb}K, not verified on Drive)"
        fi
    done
    log "local dedupe$tag: $deleted $([ -n "$LOCAL_DRY_RUN" ] && echo "would be deleted" || echo deleted), $kept kept, $((freed / 1024))M freed"
}

if [ "$MODE" = prune-local ]; then
    log "=== prune-local start ==="
    prune_uploaded
    log "=== prune-local done ==="
    exit 0
fi

log "=== backup $TS start ==="
for c in age python3 tar sha256sum; do
    command -v "$c" >/dev/null || { log "missing dependency: $c"; exit 1; }
done
[ -f "$AGE_RECIPIENT_FILE" ] || { log "age public key not found: $AGE_RECIPIENT_FILE"; exit 1; }
mkdir -p "$DEST"
cd "$X_DIR"

STAGE=$(mktemp -d)
SECRETS_TMP=""
cleanup() { rm -rf "$STAGE" ${SECRETS_TMP:+"$SECRETS_TMP"}; }
trap cleanup EXIT
mkdir -p "$STAGE/data"

# Copy one data file and parse it (JSON, or JSONL line by line). xfeed writes
# with tmp + rename, so a read can race a rename; retry instead of storing a
# torn copy. Never log the file body.
CHECK_PY='
import json, sys
path, kind = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as f:
    if kind == "jsonl":
        n = 0
        for line in f:
            if line.strip():
                json.loads(line)
                n += 1
        print(n)
    else:
        json.load(f)
        print(0)
'
FILES=0
POSTS=0
copy_checked() { # <src> <json|jsonl>
    local src=$1 kind=$2 dest attempt n
    dest=$STAGE/$src
    for attempt in 1 2 3 4 5; do
        cp -f -- "$src" "$dest"
        if n=$(python3 -c "$CHECK_PY" "$dest" "$kind" 2>/dev/null); then
            FILES=$((FILES + 1))
            POSTS=$((POSTS + n))
            return 0
        fi
        log "$src: $kind parse failed (try $attempt)"
        sleep 0.2
    done
    log "ERROR: $src is not valid $kind after 5 tries"
    exit 1
}

shopt -s nullglob
for f in data/*.jsonl; do copy_checked "$f" jsonl; done
for f in data/*.state.json; do copy_checked "$f" json; done
if [ -f data/pageviews.json ]; then
    copy_checked data/pageviews.json json
else
    log "data/pageviews.json: absent, skipped"
fi
shopt -u nullglob
tar -czf "$DEST/data.tar.gz" -C "$STAGE" data
log "data.tar.gz: $FILES files, $POSTS posts ($(du -h "$DEST/data.tar.gz" | cut -f1))"

# data/media is a directory of images written with tmp + rename. A missing tree
# still produces an archive so restore always sees the same set. GNU tar exit 1
# ("file changed as we read it") is tolerated with a warning.
rc=0
if [ -d data/media ]; then
    tar -czf "$DEST/media.tar.gz" --exclude='*.tmp' data/media || rc=$?
else
    tar -czf "$DEST/media.tar.gz" --files-from=/dev/null
fi
if [ "$rc" -eq 1 ]; then
    log "warning: media.tar.gz: files changed while archiving"
elif [ "$rc" -ne 0 ]; then
    log "ERROR: tar media exit $rc"
    exit "$rc"
fi
MEDIA_FILES=$(tar -tzf "$DEST/media.tar.gz" | grep -vc '/$' || true)
log "media.tar.gz: $MEDIA_FILES files ($(du -h "$DEST/media.tar.gz" | cut -f1))"

# Secrets, encrypted with age to the public key only. The private key is kept
# off the server.
SECRETS_TMP=$(mktemp -d)
chmod 700 "$SECRETS_TMP"
crontab -l >"$SECRETS_TMP/crontab.txt" 2>/dev/null || true
SECRET_PATHS=()
add_secret() {
    local p=$1 q
    [ -e "$p" ] || return 0
    p=$(readlink -f -- "$p")
    # Each path once: a duplicate becomes a hardlink entry that bsdtar (macOS)
    # refuses to extract.
    for q in ${SECRET_PATHS[@]+"${SECRET_PATHS[@]}"}; do
        [ "$q" = "${p#/}" ] && return 0
    done
    SECRET_PATHS+=("${p#/}")
}
add_secret "$X_DIR/config.yml"
add_secret "$X_GDRIVE_CREDS"
add_secret "$GDRIVE"
add_secret "$AGE_RECIPIENT_FILE"
add_secret "$BIN_DIR/x_backup.sh"
add_secret "$BIN_DIR/x_gdrive.py"
# tar from / so the archive keeps absolute-relative paths, plus crontab.txt.
tar -czf - -C / "${SECRET_PATHS[@]}" -C "$SECRETS_TMP" crontab.txt |
    age -R "$AGE_RECIPIENT_FILE" -o "$DEST/secrets.tar.gz.age"
[ -s "$DEST/secrets.tar.gz.age" ] || {
    log "secrets archive empty"
    exit 1
}
log "secrets encrypted: secrets.tar.gz.age (${SECRET_PATHS[*]:-<none>} crontab.txt)"

{
    echo "format=1"
    echo "ts=$TS"
    echo "host=$(hostname)"
    echo "dir=$X_DIR"
    echo "git_head=$(git -C "$X_DIR" rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "data_files=$FILES"
    echo "posts=$POSTS"
    echo "media_files=$MEDIA_FILES"
} >"$DEST/MANIFEST.txt"
(
    cd "$DEST"
    sha256sum -- * >SHA256SUMS
)
log "local snapshot: $(du -sh "$DEST" | cut -f1) in $DEST"

# Prune local snapshots older than LOCAL_KEEP_DAYS (default 7), whether or not
# they reached Drive. This is the fallback for snapshots that never verified.
# The log stays.
find "$BACKUP_ROOT" -mindepth 1 -maxdepth 1 -type d -name '20*_*' -mtime +"$LOCAL_KEEP_DAYS" -print -exec rm -rf {} +

if [ -n "${X_BACKUP_NO_UPLOAD:-}" ]; then
    log "upload skipped (X_BACKUP_NO_UPLOAD set)"
else
    python3 "$GDRIVE" upload "$DEST"
    log "uploaded to gdrive:${X_GDRIVE_ROOT}/$TS"
    python3 "$GDRIVE" prune --days "$REMOTE_KEEP_DAYS"
    # Only reached when the upload succeeded (set -e stops the script above).
    prune_uploaded
fi

log "=== backup $TS done ==="
