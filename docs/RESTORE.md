# Backup and restore

`scripts/backup.sh` runs daily from cron on the host and writes `backups/<TS>/` inside the repo (git-ignored), then uploads the same folder to the Google Drive folder `X-backup`.

| File | Contents |
| --- | --- |
| `data.tar.gz` | `data/*.jsonl`, `data/*.state.json`, `data/pageviews.json`, each parsed before it is stored |
| `media.tar.gz` | `data/media/` (saved images) |
| `secrets.tar.gz.age` | `config.yml` (bearer token), the Drive credentials, the age public key, the crontab, and the installed backup scripts, encrypted with [age](https://age-encryption.org) |
| `MANIFEST.txt` | timestamp, host, repo path, git commit, file and post counts |
| `SHA256SUMS` | checksums of the files above, uploaded last |

Every upload is checked against the md5 that Drive reports. After a successful upload, each local snapshot in `backups/` is checked with `gdrive.py verify <TS>` (a Drive folder with the same name, every local file present with the same md5 and size, and `SHA256SUMS` present) and deleted locally only when that passes. If the upload or the check fails, the local copy stays; local snapshots older than 7 days are removed regardless. Drive snapshots are kept 90 days. So normally the only copy is on Drive, and a restore downloads from there (`--from-dir backups/<TS>` works only for a snapshot that is still local). The log is `backups/backup.log`; it records each keep or delete and holds no secret values.

The age private key is not on the server; keep copies off-host. Without it `secrets.tar.gz.age` cannot be opened, but the data and media archives are plain gzip.

## Environment

| Variable | Default |
| --- | --- |
| `X_DIR` | the repo that contains the script; required for the installed copy |
| `X_BACKUP_ROOT` | `$X_DIR/backups` |
| `X_BACKUP_CONF` | `~/.config/x-backup` (Drive credentials and age keys) |
| `X_BACKUP_BIN` | `~/bin` (installed `x_backup.sh` and `x_gdrive.py`) |
| `X_GDRIVE_CREDS` | `$X_BACKUP_CONF/gdrive.json` (may be shared with other backup jobs) |
| `X_GDRIVE_ROOT` | `X-backup` |
| `X_AGE_PUB` | `$X_BACKUP_CONF/age.pub` |
| `X_AGE_KEY` (restore) | `$X_BACKUP_CONF/age.key` |
| `X_BACKUP_LOCAL_KEEP_DAYS` / `X_BACKUP_REMOTE_KEEP_DAYS` | `7` / `90` |
| `X_BACKUP_NO_UPLOAD` | set to skip Drive (local snapshot only, nothing deleted) |
| `X_BACKUP_LOCAL_DRY_RUN` | set to log the local keep/delete decisions without deleting |
| `X_BACKUP_STDOUT` | set to also print the log |

Cron (any daily hour; the example uses 03:00 in the host time zone):

```
0 3 * * * X_DIR=<repo> ~/bin/x_backup.sh
```

Drive helper: `scripts/gdrive.py list | latest | verify <TS> [--local-dir DIR] | download <TS> <dir> | prune --days N [--dry-run]`. `verify` is read-only and exits 0 only when the Drive copy is complete.

Run only the local dedupe step, without a new snapshot or upload (add `X_BACKUP_LOCAL_DRY_RUN=1` to see what it would delete):

```
X_DIR=<repo> X_BACKUP_STDOUT=1 ~/bin/x_backup.sh prune-local
```

## Restore on a new host

```bash
apt-get install -y age python3
git clone https://github.com/nctu6/X.git <repo>
cd <repo>
mkdir -p ~/.config/x-backup
# copy the age private key from its off-host copy (mode 600):
#   scp <key> <user>@<host>:.config/x-backup/age.key
```

Then **one** of:

```bash
# a) no gdrive.json yet: in the browser open folder "X-backup", download only <newest TS>/secrets.tar.gz.age
scripts/restore.sh --secrets ~/secrets.tar.gz.age --install-cron
# b) gdrive.json already in ~/.config/x-backup/
scripts/restore.sh --install-cron
# c) a backup folder you already have (Drive download, or backups/<TS> on the old disk)
scripts/restore.sh --from-dir /path/to/<TS> --install-cron
```

`restore.sh`:

1. uses `--from-dir`, or downloads the newest complete Drive backup (or `--ts <TS>`), and verifies `SHA256SUMS`;
2. decrypts `secrets.tar.gz.age` in a private temp dir that is deleted on exit;
3. refuses if `data/` or `config.yml` already exist, unless `--force` (moved to `.restore_replaced_<time>/`, never deleted);
4. extracts `data.tar.gz` and `media.tar.gz`, installs `config.yml` mode 600, and writes `.restore_state`;
5. with `--install-cron`: installs `~/bin/x_backup.sh` and `~/bin/x_gdrive.py`, copies `gdrive.json` and `age.pub` only when missing, and appends the cron line when it is not there;
6. prints the next steps. Re-running for the same timestamp skips the data step.

Other flags: `--target DIR`, `--creds FILE`, `--age-key FILE`, `--keep-secrets DIR` (keep the decrypted tree; delete it afterwards). See `scripts/restore.sh --help`.

It does not build. Afterwards, in tmux:

```bash
XFEED_BUILD=1 scripts/run.sh
curl -fsS http://127.0.0.1:8181/health
```

The reverse proxy or tunnel in front of xfeed is not part of the backup. `crontab.txt` in the secrets archive may list other services on the old host; `--install-cron` only adds the X line.

## After restore

- If the old disk may have been copied, rotate the X bearer token in `config.yml`.
- Check the next nightly run in `backups/backup.log` and in the Drive folder `X-backup`.

## Test

`tests/backup_roundtrip.sh` makes a throwaway age key and fake data, runs `backup.sh` with `X_BACKUP_NO_UPLOAD=1`, restores with `restore.sh --from-dir` into a temp checkout, and compares every file. It does not touch Drive or real data.
