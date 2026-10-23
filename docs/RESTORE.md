# Backup and restore

`scripts/backup.sh` runs daily from cron on the host and writes `backups/<TS>/` inside the repo (git-ignored), then uploads the same folder to the Google Drive folder `X-backup`.

| File | Contents |
| --- | --- |
| `data.tar.gz` | `data/*.jsonl`, `data/*.state.json`, `data/pageviews.json`, each parsed before it is stored |
| `media.tar.gz` | `data/media/` (saved images) |
| `secrets.tar.gz.age` | `config.yml` (bearer token), the Drive credentials, the age public key, the crontab, and the installed backup scripts, encrypted with [age](https://age-encryption.org) |
| `MANIFEST.txt` | timestamp, host, repo path, git commit, file and post counts |
| `SHA256SUMS` | checksums of the files above, uploaded last |

Local snapshots are kept 7 days and Drive snapshots 90 days. Every upload is checked against the md5 that Drive reports. The log is `backups/backup.log` and holds no secret values.

The age private key is not on the server; keep copies off-host. Without it `secrets.tar.gz.age` cannot be opened, but the data and media archives are plain gzip.

## Environment

| Variable | Default |
| --- | --- |
| `X_DIR` | the repo that contains the script, else `/srv/X` |
| `X_BACKUP_ROOT` | `$X_DIR/backups` |
| `X_GDRIVE_CREDS` | `/srv/x-backup/gdrive.json` (may be shared with other backup jobs) |
| `X_GDRIVE_ROOT` | `X-backup` |
| `X_AGE_PUB` | `/srv/x-backup/age.pub` |
| `X_AGE_KEY` (restore) | `~/.config/x-backup/age.key` |
| `X_BACKUP_LOCAL_KEEP_DAYS` / `X_BACKUP_REMOTE_KEEP_DAYS` | `7` / `90` |
| `X_BACKUP_NO_UPLOAD` | set to skip Drive (local snapshot only) |
| `X_BACKUP_STDOUT` | set to also print the log |

Cron (any daily hour; the example uses 03:00 in the host time zone):

```
0 3 * * * X_DIR=/srv/X /usr/local/bin/x_backup.sh
```

Drive helper: `scripts/gdrive.py list | latest | download <TS> <dir> | prune --days N [--dry-run]`.

## Restore on a new host

```bash
apt-get install -y age python3
git clone git@github.com:nctu6/X.git /srv/X
cd /srv/X
mkdir -p /srv/x-backup
# copy the age private key from its off-host copy (mode 600):
#   scp <key> root@<host>:/srv/x-backup/age.key
```

Then **one** of:

```bash
# a) no gdrive.json yet: in the browser open folder "X-backup", download only <newest TS>/secrets.tar.gz.age
scripts/restore.sh --secrets /tmp/secrets.tar.gz.age --install-cron
# b) gdrive.json already in /srv/x-backup/
scripts/restore.sh --install-cron
# c) a backup folder you already have (Drive download, or backups/<TS> on the old disk)
scripts/restore.sh --from-dir /path/to/<TS> --install-cron
```

`restore.sh`:

1. uses `--from-dir`, or downloads the newest complete Drive backup (or `--ts <TS>`), and verifies `SHA256SUMS`;
2. decrypts `secrets.tar.gz.age` in a private temp dir that is deleted on exit;
3. refuses if `data/` or `config.yml` already exist, unless `--force` (moved to `.restore_replaced_<time>/`, never deleted);
4. extracts `data.tar.gz` and `media.tar.gz`, installs `config.yml` mode 600, and writes `.restore_state`;
5. with `--install-cron` (root): installs `/usr/local/bin/x_backup.sh` and `/usr/local/bin/x_gdrive.py`, copies `gdrive.json` and `age.pub` only when missing, and appends the cron line when it is not there;
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
