#!/usr/bin/env bash
# Restore xfeed runtime data + config.yml into a fresh git clone from a backup
# made by scripts/backup.sh. See docs/RESTORE.md.
#
# Usage: scripts/restore.sh [options]
#   --ts TS             Drive folder X-backup/<TS> (default: newest complete one)
#   --from-dir DIR      use a local backup folder (backups/<TS> or a Drive download)
#   --secrets FILE      secrets.tar.gz.age downloaded by hand (bootstrap when there
#                       is no gdrive.json yet: the Drive credentials are inside it)
#   --creds FILE        Drive credentials gdrive.json
#                       (default: $X_GDRIVE_CREDS or $X_BACKUP_CONF/gdrive.json)
#   --age-key FILE      age private key (default: $X_AGE_KEY or $X_BACKUP_CONF/age.key)
#   --target DIR        repo to restore into (default: the repo this script is in)
#   --force             replace existing data (moved to <target>/.restore_replaced_<time>/, not deleted)
#   --install-cron      install $X_BACKUP_BIN/x_backup.sh + x_gdrive.py, Drive creds,
#                       age public key, and the daily cron line for this repo
#   --keep-secrets DIR  keep the decrypted secrets tree in DIR
#   -h, --help
# Idempotent: re-running for the TS that is already restored leaves the data in place.
set -Eeuo pipefail
umask 077

die() { echo "restore: ERROR: $*" >&2; exit 1; }
say() { echo "restore: $*"; }
# First path whose basename matches. -quit avoids a pipe to head.
first_named() { # <root> <filename>
    find "$1" -name "$2" -print -quit
}

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
TARGET=$(cd "$SCRIPT_DIR/.." && pwd)
TS="" FROM_DIR="" SECRETS_FILE="" FORCE=0 INSTALL_CRON=0 KEEP_SECRETS=""
# X_BACKUP_CONF defaults to ~/.config/x-backup; X_BACKUP_BIN to ~/bin.
CONF_DIR=${X_BACKUP_CONF:-${XDG_CONFIG_HOME:-$HOME/.config}/x-backup}
BIN_DIR=${X_BACKUP_BIN:-$HOME/bin}
CREDS=${X_GDRIVE_CREDS:-$CONF_DIR/gdrive.json}
AGE_KEY=${X_AGE_KEY:-$CONF_DIR/age.key}
export X_GDRIVE_ROOT=${X_GDRIVE_ROOT:-X-backup}

while [ $# -gt 0 ]; do
    case "$1" in
        --ts) TS=${2:?}; shift 2 ;;
        --from-dir) FROM_DIR=${2:?}; shift 2 ;;
        --secrets) SECRETS_FILE=${2:?}; shift 2 ;;
        --creds) CREDS=${2:?}; shift 2 ;;
        --age-key) AGE_KEY=${2:?}; shift 2 ;;
        --target)
            mkdir -p "${2:?}"
            TARGET=$(cd "$2" && pwd)
            shift 2
            ;;
        --force) FORCE=1; shift ;;
        --install-cron) INSTALL_CRON=1; shift ;;
        --keep-secrets) KEEP_SECRETS=${2:?}; shift 2 ;;
        -h | --help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
done

for c in python3 age tar gzip; do
    command -v "$c" >/dev/null || die "missing dependency: $c (Ubuntu: apt-get install -y python3 age; macOS: brew install age)"
done
if command -v sha256sum >/dev/null; then
    SHACHECK=(sha256sum -c --quiet)
else
    SHACHECK=(shasum -a 256 -c --quiet)
fi
[ -f "$AGE_KEY" ] || die "age private key not found: $AGE_KEY (it is kept off the server; pass --age-key)"
[ -f "$TARGET/Cargo.toml" ] || die "$TARGET is not an xfeed checkout (no Cargo.toml)"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/x-restore.XXXXXX")
chmod 700 "$WORK"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT
trap 'echo "restore: FAILED at line $LINENO" >&2' ERR

decrypt_secrets() { # <secrets.tar.gz.age> -> $WORK/secrets
    rm -rf "$WORK/secrets"
    mkdir -p "$WORK/secrets"
    age -d -i "$AGE_KEY" "$1" | tar -xzf - -C "$WORK/secrets" || die "cannot decrypt $1 with $AGE_KEY"
}

if [ -n "$FROM_DIR" ]; then
    BK=$(cd "$FROM_DIR" && pwd)
    TS=$(basename "$BK")
    say "using local backup $BK"
else
    if [ ! -f "$CREDS" ]; then
        if [ -z "$SECRETS_FILE" ]; then
            cat >&2 <<MSG
restore: no Drive credentials at $CREDS.
  Bootstrap: open https://drive.google.com -> folder "$X_GDRIVE_ROOT" -> newest <TS> folder,
  download only secrets.tar.gz.age, then re-run:
      $0 --secrets ~/Downloads/secrets.tar.gz.age [--ts <TS>]
  (the Drive credentials are inside it), or download the whole <TS> folder and use
      $0 --from-dir <folder>
MSG
            exit 2
        fi
        decrypt_secrets "$SECRETS_FILE"
        CREDS=$(first_named "$WORK/secrets" gdrive.json)
        [ -n "$CREDS" ] || die "no gdrive.json inside $SECRETS_FILE"
        chmod 600 "$CREDS"
    fi
    export X_GDRIVE_CREDS=$CREDS
    if [ -z "$TS" ]; then
        TS=$(python3 "$SCRIPT_DIR/gdrive.py" latest | tail -1)
        [ -n "$TS" ] || die "could not find a backup on Drive"
    fi
    BK="$WORK/backup/$TS"
    say "downloading $X_GDRIVE_ROOT/$TS from Drive"
    python3 "$SCRIPT_DIR/gdrive.py" download "$TS" "$BK"
fi

[ -f "$BK/SHA256SUMS" ] || die "$BK has no SHA256SUMS"
(cd "$BK" && "${SHACHECK[@]}" SHA256SUMS) || die "checksum mismatch in $BK"
say "SHA256SUMS ok ($(wc -l <"$BK/SHA256SUMS" | tr -d ' ') files)"
[ -f "$BK/secrets.tar.gz.age" ] || die "$BK has no secrets.tar.gz.age"
decrypt_secrets "$BK/secrets.tar.gz.age"

REL=""
if [ -f "$BK/MANIFEST.txt" ]; then
    p=$(sed -n 's/^dir=//p' "$BK/MANIFEST.txt")
    if [ -n "$p" ]; then
        REL=${p#/}
    fi
fi
S_CONFIG="$WORK/secrets/$REL/config.yml"
if [ -z "$REL" ] || [ ! -f "$S_CONFIG" ]; then
    S_CONFIG=$(first_named "$WORK/secrets" config.yml)
fi

STATE="$TARGET/.restore_state"
if [ -f "$STATE" ] && [ "$(sed -n 's/^ts=//p' "$STATE")" = "$TS" ] && [ "$FORCE" -eq 0 ]; then
    say "data already restored from $TS (see $STATE); skipping data step"
else
    existing=()
    if [ -d "$TARGET/data" ] && [ -n "$(ls -A "$TARGET/data" 2>/dev/null)" ]; then
        existing+=(data)
    fi
    if [ -e "$TARGET/config.yml" ]; then
        existing+=(config.yml)
    fi
    if [ ${#existing[@]} -gt 0 ]; then
        [ "$FORCE" -eq 1 ] || die "refusing to overwrite existing ${existing[*]} in $TARGET (use --force; old files are moved aside, not deleted)"
        ASIDE="$TARGET/.restore_replaced_$(date +%Y%m%d_%H%M%S)"
        mkdir -p "$ASIDE"
        for p in "${existing[@]}"; do
            mv "$TARGET/$p" "$ASIDE/$p"
        done
        say "moved existing ${existing[*]} to $ASIDE"
    fi

    mkdir -p "$TARGET/data"
    [ -f "$BK/data.tar.gz" ] || die "$BK has no data.tar.gz"
    tar -xzf "$BK/data.tar.gz" -C "$TARGET"
    say "extracted data.tar.gz ($(tar -tzf "$BK/data.tar.gz" | grep -vc '/$' || true) files)"
    if [ -f "$BK/media.tar.gz" ]; then
        tar -xzf "$BK/media.tar.gz" -C "$TARGET"
        say "extracted media.tar.gz ($(tar -tzf "$BK/media.tar.gz" | grep -vc '/$' || true) files)"
    fi
    if [ -n "${S_CONFIG:-}" ] && [ -f "$S_CONFIG" ]; then
        install -m 600 "$S_CONFIG" "$TARGET/config.yml"
        say "restored config.yml (mode 600)"
    else
        say "no config.yml in the secrets archive; copy config.example.yml and set x_bearer_token"
    fi
    {
        printf 'ts=%s\n' "$TS"
        printf 'restored_at=%s\n' "$(date '+%F %T %Z')"
        if [ -f "$BK/MANIFEST.txt" ]; then
            sed -n 's/^git_head=/backup_git_head=/p' "$BK/MANIFEST.txt"
            sed -n 's/^posts=/posts=/p' "$BK/MANIFEST.txt"
        fi
    } >"$STATE"
fi

if [ "$INSTALL_CRON" -eq 1 ]; then
    install -d -m 700 "$BIN_DIR" "$CONF_DIR"
    install -m 700 "$TARGET/scripts/backup.sh" "$BIN_DIR/x_backup.sh"
    install -m 700 "$TARGET/scripts/gdrive.py" "$BIN_DIR/x_gdrive.py"
    S_CREDS=$(first_named "$WORK/secrets" gdrive.json)
    S_PUB=$(first_named "$WORK/secrets" age.pub)
    if [ ! -f "$CONF_DIR/gdrive.json" ]; then
        if [ -z "$S_CREDS" ] || [ ! -f "$S_CREDS" ]; then
            die "no gdrive.json in the secrets archive"
        fi
        install -m 600 "$S_CREDS" "$CONF_DIR/gdrive.json"
    fi
    if [ ! -f "$CONF_DIR/age.pub" ]; then
        if [ -z "$S_PUB" ] || [ ! -f "$S_PUB" ]; then
            die "no age.pub in the secrets archive"
        fi
        install -m 600 "$S_PUB" "$CONF_DIR/age.pub"
    fi
    LINE="0 3 * * * X_DIR=$TARGET $BIN_DIR/x_backup.sh"
    if crontab -l 2>/dev/null | grep -q 'x_backup\.sh'; then
        say "cron already has an x_backup.sh line; left unchanged"
    else
        (crontab -l 2>/dev/null || true; echo "$LINE") | crontab -
        say "installed cron: $LINE (host time zone; edit the hour with crontab -e)"
    fi
fi

if [ -n "$KEEP_SECRETS" ]; then
    mkdir -p "$KEEP_SECRETS"
    chmod 700 "$KEEP_SECRETS"
    cp -a "$WORK/secrets/." "$KEEP_SECRETS/"
    say "decrypted secrets kept in $KEEP_SECRETS (delete it when done)"
fi

cat <<NEXT

restore: done. Restored $TS into $TARGET
Next steps:
  1. Build and start xfeed (port from config.yml, default 8181):
       cd $TARGET && XFEED_BUILD=1 scripts/run.sh     # in tmux, not systemd
       curl -fsS http://127.0.0.1:8181/health
  2. Daily backup: re-run with --install-cron, then run $BIN_DIR/x_backup.sh once.
       0 3 * * * X_DIR=$TARGET $BIN_DIR/x_backup.sh
     The hour is in the host time zone. Log: $TARGET/backups/backup.log
NEXT
