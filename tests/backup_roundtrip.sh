#!/usr/bin/env bash
# backup.sh (no Drive upload) then restore.sh --from-dir must reproduce runtime data.
set -Eeuo pipefail
umask 077

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
for c in age age-keygen python3 sha256sum; do
    command -v "$c" >/dev/null || {
        echo "backup round trip: missing $c" >&2
        exit 1
    }
done

WORK=$(mktemp -d "${TMPDIR:-/tmp}/x-backup-test.XXXXXX")
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT
fail() { echo "backup round trip: $*" >&2; exit 1; }

age-keygen -o "$WORK/age.key" >/dev/null 2>&1
pub=$(sed -n 's/^# public key: //p' "$WORK/age.key")
[ -n "$pub" ]
printf '%s\n' "$pub" >"$WORK/age.pub"

SRC=$WORK/src
mkdir -p "$SRC/data/media/someone"
cp "$ROOT/Cargo.toml" "$SRC/Cargo.toml"
printf '{"id":"1","text":"one"}\n{"id":"2","text":"two"}\n' >"$SRC/data/someone.jsonl"
printf '{"last_success":"2026-10-22T00:00:00Z"}\n' >"$SRC/data/someone.state.json"
printf '{"2026-10-22":3}\n' >"$SRC/data/pageviews.json"
head -c 2048 /dev/urandom >"$SRC/data/media/someone/3_1.jpg"
touch "$SRC/data/media/someone/.3_2.jpg.tmp"
SECRET='X_SECRET_MARKER_5d2e8a1c9b7f4e3a'
printf 'x_bearer_token: "%s"\nlisten: "127.0.0.1:8181"\n' "$SECRET" >"$SRC/config.yml"
chmod 600 "$SRC/config.yml"

export X_DIR=$SRC
export X_BACKUP_ROOT=$SRC/backups
export X_BACKUP_NO_UPLOAD=1
export X_BACKUP_STDOUT=1
export X_AGE_PUB=$WORK/age.pub
export X_GDRIVE=$ROOT/scripts/gdrive.py
# Not the real OAuth file; only so the secrets archive has a gdrive.json.
export X_GDRIVE_CREDS=$WORK/gdrive.json
printf '{"client_id":"rt","client_secret":"rt","refresh_token":"rt"}\n' >"$WORK/gdrive.json"

bash "$ROOT/scripts/backup.sh" >"$WORK/backup.out"

SNAP=$(find "$SRC/backups" -mindepth 1 -maxdepth 1 -type d -name '20*_*' -print -quit)
[ -n "$SNAP" ]
for f in MANIFEST.txt SHA256SUMS data.tar.gz media.tar.gz secrets.tar.gz.age; do
    [ -f "$SNAP/$f" ] || fail "missing $f"
done
grep -q '^format=1$' "$SNAP/MANIFEST.txt"
grep -q '^posts=2$' "$SNAP/MANIFEST.txt"
grep -q '^data_files=3$' "$SNAP/MANIFEST.txt"
grep -q '^media_files=1$' "$SNAP/MANIFEST.txt"
(cd "$SNAP" && sha256sum -c --quiet SHA256SUMS)
# Each secret path once (a duplicate is a hardlink entry bsdtar will not extract).
dups=$(age -d -i "$WORK/age.key" "$SNAP/secrets.tar.gz.age" | tar -tzf - | sort | uniq -d)
[ -z "$dups" ] || fail "duplicate entries in secrets archive: $dups"
if tar -tzf "$SNAP/media.tar.gz" | grep -q '\.tmp$'; then fail "media.tar.gz has a .tmp file"; fi

# The secret must not appear in the log or any plaintext archive.
for f in "$SRC/backups/backup.log" "$WORK/backup.out" "$SNAP/MANIFEST.txt"; do
    if grep -q "$SECRET" "$f"; then fail "secret in $f"; fi
done
if zcat "$SNAP/data.tar.gz" "$SNAP/media.tar.gz" | grep -aq "$SECRET"; then fail "secret in plaintext archive"; fi

TARGET=$WORK/target
mkdir -p "$TARGET"
cp "$ROOT/Cargo.toml" "$TARGET/Cargo.toml"
bash "$ROOT/scripts/restore.sh" --from-dir "$SNAP" --target "$TARGET" --age-key "$WORK/age.key" >"$WORK/restore.out"

for f in data/someone.jsonl data/someone.state.json data/pageviews.json data/media/someone/3_1.jpg config.yml; do
    cmp "$SRC/$f" "$TARGET/$f"
done
[ "$(stat -c %a "$TARGET/config.yml")" = 600 ]
if grep -q "$SECRET" "$WORK/restore.out"; then fail "secret in restore output"; fi

bash "$ROOT/scripts/restore.sh" --from-dir "$SNAP" --target "$TARGET" --age-key "$WORK/age.key" >"$WORK/restore2.out"
grep -q 'already restored' "$WORK/restore2.out"
bash "$ROOT/scripts/restore.sh" --from-dir "$SNAP" --target "$TARGET" --age-key "$WORK/age.key" --force >"$WORK/restore3.out"
ls -d "$TARGET"/.restore_replaced_* >/dev/null
cmp "$SRC/config.yml" "$TARGET/config.yml"
bash "$ROOT/scripts/restore.sh" --help >/dev/null

echo "backup round trip ok"
