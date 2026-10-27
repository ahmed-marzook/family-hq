# Running GOWA locally

GOWA is the bridge between WhatsApp and Family HQ. It logs in as the bot's
own WhatsApp number (like WhatsApp Web does) and:

- **sends** messages when we call its REST API, and
- **receives** messages and POSTs each one to a URL we choose (a *webhook*).

Until family-hq has its own webhook endpoint (Stage 1.4), webhooks go to a
small **echo server** that just prints every request. That lets you see the
real payloads and save them as test fixtures.

```
 phone (bot SIM) ⇄ WhatsApp servers ⇄ gowa :3001 ──webhook──▶ webhook-echo
                                         ▲
                              you, with curl / browser
```

## What you need

- Docker with Compose (`docker compose version` should work).
- A phone with the **bot's SIM** and WhatsApp installed on it. Never pair
  your personal WhatsApp.
- `curl` and `openssl` (already on macOS and most Linux).

## 1. Configure

```sh
cp .env.example .env
```

Edit `.env`:

- `GOWA_BASIC_AUTH`: pick a real password, e.g. `admin:something-long`.
- `GOWA_WEBHOOK_SECRET`: paste the output of `openssl rand -hex 32`.

`.env` is git-ignored. Don't commit it.

To save typing in the commands below, set your login in your shell:

```sh
export AUTH='admin:something-long'   # same as GOWA_BASIC_AUTH
```

## 2. Start it

```sh
docker compose up -d
docker compose ps        # gowa and webhook-echo should be "Up"
curl -u "$AUTH" localhost:3001/app/info
# {"code":"SUCCESS",...,"version":"v9.7.0"}
```

Without `-u` you get `401`, which is correct.

> **On a headless homelab server?** GOWA's port only listens on
> `127.0.0.1` by default. Either tunnel it to your laptop with
> `ssh -L 3001:localhost:3001 you@homelab` and use `localhost:3001` as
> usual, or set `GOWA_BIND=0.0.0.0` in `.env` to open it to your LAN.

## 3. Pair the bot's phone

### Option A: the web UI (easiest)

Open <http://localhost:3001>, log in with `GOWA_BASIC_AUTH`, add a device
and scan the QR code. On the bot's phone, go to WhatsApp → **Settings →
Linked devices → Link a device**.

(GOWA downloads the UI from GitHub when it first starts. If the page is
blank, wait a minute or use option B.)

### Option B: with curl

```sh
# Create a device slot called "family-hq" (once only)
curl -u "$AUTH" -X POST localhost:3001/devices \
  -H 'Content-Type: application/json' -d '{"device_id":"family-hq"}'

# Get a QR code. It expires after about 30 seconds, so run this again if needed.
curl -u "$AUTH" localhost:3001/devices/family-hq/login
```

The reply has a `qr_link`. Open it in a browser, **changing the port to
3001** if it says 3000 (GOWA only knows its port inside the container).

If you can't scan, for example because the phone is the screen, use a pairing code:

```sh
curl -u "$AUTH" -X POST \
  "localhost:3001/devices/family-hq/login/code?phone=447700900123"
```

Then in WhatsApp go to **Link a device → Link with phone number instead** and type the code.

### Check it worked

```sh
curl -u "$AUTH" localhost:3001/app/status
# "is_connected":true,"is_logged_in":true,"jid":"4477...@s.whatsapp.net"
```

The session is saved in the `gowa_storages` Docker volume, so it survives
restarts. You only pair once.

## 4. Watch incoming messages

Keep this running in a second terminal:

```sh
docker compose logs -f webhook-echo
```

Now, from **your own** phone, send the bot a DM like `+shop milk`. A request
for `/webhook/whatsapp` appears with:

- `headers."x-hub-signature-256"`: the signature (see below)
- `body`: the exact JSON GOWA sent, as a string
- `json`: the same body, parsed

Things to try, because the roadmap needs them all:

| Try this | Look for in the payload |
|---|---|
| DM the bot | `chat_id` ends in `@s.whatsapp.net` |
| Message in a group with the bot | `chat_id` ends in `@g.us`; `from` is the sender |
| Reply (swipe) to one of the bot's messages | `replied_to_id`, `quoted_body` |
| React ✅ to a message | `"event":"message.reaction"`, `reaction`, `reacted_message_id` |
| A message the bot sends itself | `is_from_me: true` (family-hq must ignore these) |
| Send the exact same text twice | different `id`s (the dedupe key) |

**Save real payloads as fixtures.** This answers the "what does the payload
look like" open question in the roadmap. Copy the `body` string of each
interesting one into `tests/fixtures/gowa/` (e.g. `text_dm.json`,
`text_group.json`, `reply.json`, `reaction.json`). Replace real phone
numbers with fake ones like `447700900xxx` before committing.

## 5. Send messages from the bot

```sh
# A DM (use the person's number in international format, no +)
curl -u "$AUTH" -X POST localhost:3001/send/message \
  -H 'Content-Type: application/json' \
  -d '{"phone":"447700900123@s.whatsapp.net","message":"🛒 Added milk"}'
```

The reply contains the new message's ID. Stage 1.5 stores these in
`wa_message_context`.

```sh
# Find the family group's ID (ends in @g.us)
curl -u "$AUTH" localhost:3001/user/my/groups

# Send to the group, as a reply to a specific message
curl -u "$AUTH" -X POST localhost:3001/send/message \
  -H 'Content-Type: application/json' \
  -d '{"phone":"120363xxxxxxxx@g.us","message":"Got it","reply_message_id":"3EB0..."}'

# React to a message
curl -u "$AUTH" -X POST localhost:3001/message/3EB0.../reaction \
  -H 'Content-Type: application/json' \
  -d '{"phone":"120363xxxxxxxx@g.us","emoji":"✅"}'
```

Full API reference: `docs/openapi.yaml` in the
[GOWA repo](https://github.com/aldinokemal/go-whatsapp-web-multidevice).

## 6. Webhook signatures

Anyone who can reach the webhook URL could POST a fake "message", so GOWA
signs each request. It computes an **HMAC-SHA256** of the raw body bytes
with `GOWA_WEBHOOK_SECRET` as the key, and sends the result as
`X-Hub-Signature-256: sha256=<hex>`. The receiver computes the same thing
and rejects the request if the two don't match. Only someone who knows the
secret can produce a valid signature.

You can check one by hand. Save a `body` string from the echo log exactly as is
(no reformatting, since the signature covers the exact bytes):

```sh
printf '%s' "$(cat payload.json)" \
  | openssl dgst -sha256 -hmac "$GOWA_WEBHOOK_SECRET"
# should equal the hex after "sha256=" in the header
```

That's the check `POST /webhook/whatsapp` will do in Rust in Stage 1.4.

## 7. Day-to-day commands

```sh
docker compose up -d                # start
docker compose stop                 # stop (keeps the session)
docker compose logs -f gowa         # GOWA's own logs (connection problems)
docker compose logs -f webhook-echo # incoming webhooks
docker compose down                 # remove containers (keeps the session)
docker compose down -v              # remove the session too: you must pair again
```

To unlink properly, also remove "FamilyHQ" from **Linked devices** on the
bot's phone.

## Later: pointing GOWA at family-hq

Once `/webhook/whatsapp` exists (Stage 1.4) and family-hq is in
`compose.yaml`, set this in `.env` and restart:

```sh
GOWA_WEBHOOK_URL=http://family-hq:3000/webhook/whatsapp
```

Inside Compose, services reach each other by service name (`family-hq`,
`gowa`), so family-hq will call GOWA at `http://gowa:3000`, not
`localhost:3001`.

## Troubleshooting

- **`401`**: missing or wrong `-u user:pass`.
- **`DEVICE_ID_REQUIRED`**: no device exists yet (step 3), or there are several.
  Add `-H 'X-Device-Id: family-hq'`.
- **Nothing in webhook-echo**: check `/app/status` says logged in, and that
  you messaged *the bot's* number. Only `message` and `message.reaction`
  events are forwarded (`WHATSAPP_WEBHOOK_EVENTS` in `compose.yaml`).
- **QR expired**: request a new one; each lasts about 30 seconds.
- **Port 3001 already in use**: change the left side of `3001:3000` in
  `compose.yaml`.
