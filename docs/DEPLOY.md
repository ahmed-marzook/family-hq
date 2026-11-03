# Running Family HQ on the home server

The server runs three containers from three images, started together by one
Compose file:

| Container | Image | What it does |
|---|---|---|
| `family-hq` | [`kaizenflow/family-hq`](https://hub.docker.com/r/kaizenflow/family-hq) | Our Rust server: commands, database, replies |
| `admin` | [`kaizenflow/family-hq-admin`](https://hub.docker.com/r/kaizenflow/family-hq-admin) | The admin dashboard: serves the web app, and passes `/api` to `family-hq` |
| `gowa` | `aldinokemal2104/go-whatsapp-web-multidevice` | Logs in to WhatsApp as the bot's number |

```
 WhatsApp ⇄ gowa ──webhook──▶ family-hq ──▶ SQLite (/data volume)
              ▲                    │
              └──── replies ───────┘      (both on Compose's private network)
```

**Why two containers, not one:** each gets its own restarts and logs, GOWA
is upgraded by changing one tag (we don't rebuild it), and the WhatsApp
login and the database live in separate volumes you can back up or reset
on their own. Compose makes running both a single command anyway.

Nothing needs to be opened to the internet: GOWA connects *out* to
WhatsApp, and GOWA → family-hq traffic stays inside Docker.

The admin dashboard listens on port 8080, on the server only until you set
`ADMIN_BIND=0.0.0.0` in `.env`. That opens it to your home network and
Tailscale. It asks for the same login as GOWA (`GOWA_BASIC_AUTH`), from
version 0.8.1 on; 0.8.0 has no password, so keep it on the server only with
that version. **Never port-forward 8080 on your router.** Docker's published
ports also bypass `ufw`, so a firewall rule on the server won't limit who
reaches it. Plain HTTP on the home network shows the password to anyone able
to watch that network's traffic; Tailscale encrypts it.

---

## 1. One-time server setup

You need Docker with the Compose plugin. On a Debian/Ubuntu server:

```sh
curl -fsSL https://get.docker.com | sudo sh
sudo usermod -aG docker $USER     # run docker without sudo (log out and back in)
docker compose version             # should print a version
```

Check the CPU is x86-64 (the image is built for `linux/amd64`):

```sh
uname -m    # x86_64
```

## 2. Copy the two files to the server

Everything the server needs is in `deploy/`. From your laptop, in the repo:

```sh
ssh you@homeserver 'mkdir -p ~/family-hq'
scp deploy/compose.yaml deploy/.env.example you@homeserver:~/family-hq/
```

No source code, no building: the server only pulls images.

## 3. Create the server's `.env`

On the server:

```sh
cd ~/family-hq
cp .env.example .env
echo "GOWA_BASIC_AUTH=admin:$(openssl rand -hex 16)"
echo "GOWA_WEBHOOK_SECRET=$(openssl rand -hex 32)"
```

Paste those two lines into `.env` (replacing the `change-me` ones) with
`nano .env`. Leave `FAMILY_HQ_TAG=0.17.0`. Then lock it down, since it holds
the passwords:

```sh
chmod 600 .env
```

## 4. Hand the bot over from your laptop

> ⚠️ **Only one GOWA may be linked to the bot at a time.** If your laptop's
> GOWA and the server's GOWA are both linked, both receive every message
> and both reply, so the family sees every answer twice (and two separate
> databases).

On the laptop, in the repo:

```sh
docker compose down          # stops laptop GOWA, family-hq, echo (keeps volumes)
```

Then on the **bot's phone**: WhatsApp → Settings → **Linked devices** →
tap **FamilyHQ** → **Log out**.

(Your laptop's test database stays on the laptop. The server starts with a
fresh, empty one.)

## 5. Start it

On the server:

```sh
cd ~/family-hq
docker compose up -d
docker compose ps
```

Both should be `Up`; `family-hq` turns `(healthy)` after a few seconds.

```sh
curl localhost:3000/health          # {"status":"ok"}
docker compose logs family-hq       # "database ready", "listening on ..."
```

## 6. Pair the bot's phone

GOWA's web UI only listens on the server itself. From your **laptop**, open
an SSH tunnel and leave it running:

```sh
ssh -L 3001:localhost:3001 you@homeserver
```

Now open <http://localhost:3001> on the laptop. That's the server's GOWA,
through the tunnel. Log in with the `GOWA_BASIC_AUTH` user and password from
the server's `.env`, add a device, and scan the QR code from the bot's
phone (Linked devices → Link a device). Close the tunnel when done.

Check it from the server:

```sh
AUTH=$(grep ^GOWA_BASIC_AUTH= .env | cut -d= -f2-)
curl -s -u "$AUTH" localhost:3001/app/status
# ... "is_connected":true,"is_logged_in":true ...
```

The login is saved in the `gowa_storages` volume: restarts, reboots and
updates don't need pairing again.

## 7. Try it

In the family group, send `+todo test the server`. You should get a 👍
reaction and a "📝 Added" reply. Watch it happen:

```sh
docker compose logs -f family-hq
```

Both containers have `restart: unless-stopped`, so they come back by
themselves after a crash or a server reboot (as long as Docker starts at
boot, which `get.docker.com` sets up).

---

## Releasing a new version

The server runs whatever version the **stacks repo**
(`my-homeserver-stacks/family-hq`) names in `FAMILY_HQ_VERSION`; Dockhand
deploys from it. So a release is: an image on Docker Hub, then a PR in the
stacks repo that points at it.

### The automatic way: push a tag

`.github/workflows/release.yml` does steps 2 and 3 below on GitHub when a
version tag is pushed. After bumping and committing (step 1):

```sh
git tag v0.5.1
git push origin main v0.5.1
```

It checks (fmt, clippy, tests, tag = `Cargo.toml` version), builds the
image, runs the image checks from step 2, pushes both tags to Docker Hub,
and opens the stacks PR. Follow it in the repo's **Actions** tab. A failed
check stops it before anything is pushed; fix, then move the tag
(`git tag -f v0.5.1 && git push -f origin v0.5.1`).

One-time setup, in this repo's Settings → Secrets and variables → Actions:

| Kind | Name | Value |
|---|---|---|
| Variable | `DOCKERHUB_USERNAME` | `kaizenflow` |
| Secret | `DOCKERHUB_TOKEN` | A Docker Hub access token (Account settings → Personal access tokens, Read & Write) |
| Secret | `STACKS_TOKEN` | A fine-grained GitHub token for `my-homeserver-stacks` only, with Contents and Pull requests: Read and write |

The workflow also pushes `kaizenflow/family-hq-admin`, so that Docker Hub
repository must exist and the token must be allowed to push to it.

The rest of this section is the same thing by hand.

### 1. In this repo: check, bump, commit

```sh
cargo fmt --check
cargo clippy --all-targets      # no warnings
cargo test
cargo sqlx prepare              # only if SQL changed; commit .sqlx/
```

Bump `version` in `Cargo.toml` (`cargo build` updates `Cargo.lock`), and
`FAMILY_HQ_TAG` in `deploy/.env.example` and step 3 above. Patch
(`0.4.2` → `0.4.3`) for fixes, minor (`0.4.x` → `0.5.0`) for new commands
or a new migration. Commit as `Release 0.5.0: <what changed>` and push
`main`.

### 2. Build, check and push the image

```sh
V=0.5.0
docker build --platform linux/amd64 -t kaizenflow/family-hq:$V -t kaizenflow/family-hq:latest .
docker build --platform linux/amd64 -t kaizenflow/family-hq-admin:$V -t kaizenflow/family-hq-admin:latest web
```

Before pushing, check the image:

- **Upgrade test:** start the *previous* version on a scratch data folder,
  send it a message or two, stop it, then start the new image on the same
  folder. It should turn `(healthy)` and log the new migrations.
- **No secrets inside:** this prints nothing (no `.env` or database):
  `docker run --rm --entrypoint sh kaizenflow/family-hq:$V -c 'find / -xdev \( -name .env -o -name "*.db" \) 2>/dev/null'`
- **Runs unprivileged:** `docker top <container>` shows user `1000`.

Then push (`docker login` first if asked):

```sh
docker push kaizenflow/family-hq:$V
docker push kaizenflow/family-hq:latest
docker push kaizenflow/family-hq-admin:$V
docker push kaizenflow/family-hq-admin:latest
```

### 3. In the stacks repo: a PR for the new version

```sh
cd ../my-homeserver-stacks
git switch main && git pull
git switch -c family-hq-$V
# family-hq/.env.example: FAMILY_HQ_VERSION=0.5.0
git commit -am "family-hq: $V" && git push -u origin family-hq-$V
gh pr create --title "family-hq: $V" --body "What changed, and any migrations."
```

Merge the PR once you're happy; Dockhand then pulls the new image and
recreates the container. New migrations run on start-up.

## Updating a server run by hand (without Dockhand)

```sh
cd ~/family-hq
nano .env                     # FAMILY_HQ_TAG=0.5.0
docker compose pull
docker compose up -d          # recreates only what changed
docker image prune -f         # delete the old image
```

New migrations run automatically when family-hq starts. To roll back, put
the old tag back and `up -d` again. (A version whose migrations already ran
is fine to roll back to only if the newer migrations didn't remove
anything it needs, so take a backup before updating.)

**Updating GOWA** is the same, but by editing the `image:` tag in
`compose.yaml`. Read GOWA's release notes first: its webhook format is what
our parser depends on.

## Clearing data nobody uses

The bot clears the data of people and groups that stop using it, after 14
days by default. Change that with `INACTIVE_DAYS` in the stack's `.env`, or
set it to `0` to never clear anything. The admin dashboard's user pages
show who will be cleared, and when. What happens:

- **A person** inactive for `INACTIVE_DAYS` is warned in their DM, then 3 days
  later their DM (lists, settings, reminders), group memberships, prayer log
  and usage log are deleted. Their name and number go too, unless a group's
  list still mentions them; then those stay until the list is cleared.
- **A group** nobody uses the bot in gets the same warning, then the bot
  leaves it and deletes its lists and settings. To bring the bot back,
  someone in the group adds its number again.
- **A group the bot was removed from** is deleted a day after GOWA stops
  listing it.

Using the bot (a command, a list reply, a reaction to a reminder, or any
message in a DM) keeps everything. Each step is in the server log
(`cleanup: ...`). Deleted data only comes back from a backup.

## Backups

Every day, family-hq copies the whole database into the
`family-hq_family_hq_backups` volume as `family-hq-2026-11-03.db`, and keeps
the newest 7 (`BACKUP_KEEP` in `.env`). The copy is made on the first check
after midnight, or when the server starts if it was down then. Each copy is
one complete file that never changes after it's written. The log says
`backup: wrote ...` each day.

To keep the copies on another disk, set `BACKUP_PATH` in `.env` to a folder
there (for example `/mnt/backup/family-hq`); the container makes it
writable on start. Kopia, or anything else, should back up that folder
rather than the live database, which is two files (the database and its
`-wal`) that change all the time.

To see them:

```sh
docker run --rm -v family-hq_family_hq_backups:/backups debian:bookworm-slim ls -l /backups
```

To restore one, stop family-hq, put the copy in place of the database, and
remove the old `-wal` and `-shm` files, which belong to the database being
replaced:

```sh
cd ~/family-hq
docker compose stop family-hq
docker run --rm -v family-hq_family_hq_data:/data -v family-hq_family_hq_backups:/backups   debian:bookworm-slim sh -c 'rm -f /data/family-hq.db-wal /data/family-hq.db-shm   && cp /backups/family-hq-2026-11-03.db /data/family-hq.db'
docker compose start family-hq
```

Keep a copy somewhere other than the server too.

The GOWA login (`family-hq_gowa_storages`) can be backed up the same way,
but re-pairing is only a QR scan, so it matters less.

## Day-to-day

```sh
docker compose ps                    # status and health
docker compose logs -f family-hq     # what the bot is doing
docker compose logs -f gowa          # WhatsApp connection problems
docker compose restart family-hq
docker compose down                  # stop everything (keeps all data)
```

`docker compose down -v` also **deletes the volumes**: the database and the
WhatsApp login. Don't, unless you mean it.

## Logs

Each command is one line, with who sent it, in which chat, what it was and
whether it worked:

```text
INFO message: family_hq::routes::webhook: handled command=shop.add ok=true ms=0 id=3EB0... chat=...@g.us user="Bilal"
```

Everything logged while handling that message (a failed reply, say) carries
the same `id`, `chat` and `user`, so `grep 3EB0...` finds all of it. Lines
start with a level: `INFO`, `WARN` (something went wrong but it carried on,
e.g. GOWA didn't answer) or `ERROR` (something failed).

`RUST_LOG` in `.env` picks how much is shown: `info` by default. `debug`
adds ordinary chat and what each command contained, including list items,
so use it only while looking into a problem, and only for the part you
need, e.g. `RUST_LOG=info,family_hq::routes::webhook=debug`. Restart
family-hq after changing it.

## Troubleshooting

| Symptom | Check |
|---|---|
| No reaction or reply | `docker compose logs family-hq`: is the message arriving? If not, `logs gowa` and `/app/status` (step 6). |
| `webhook: rejected, bad or missing signature` | The two containers disagree on `GOWA_WEBHOOK_SECRET`. Both read it from the same `.env`, so run `docker compose up -d` after any edit. |
| `couldn't reply ... 401` | `GOWA_BASIC_AUTH` changed but one container wasn't recreated: `docker compose up -d`. |
| Every answer appears twice | Two GOWAs are linked (step 4). Log one out under Linked devices. |
| `family-hq` keeps restarting | `docker compose logs family-hq`: a missing variable or a database problem is printed at startup. |
| `exec format error` | The server isn't x86-64; the image needs building for its CPU. |
