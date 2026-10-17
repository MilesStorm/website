# milesstorm.com auth server

### envrionment variables

- `SERVER_PORT` : the port that the server will run on
- `SERVER_IP`: the ip that the server will run on
- `RUST_LOG`: the log level for the server
- `DATABASE_URL`: the url for the database
- `CLIENT_ID`: the client id for the github oauth
- `CLIENT_SECRET`: the client secret for the github oauth
- `RESEND_API_KEY`: API key for [Resend](https://resend.com), which sends the account emails. Without it no email is sent (debug builds print them to the log instead, links included)
- `MAIL_FROM`: the sender, default `milesstorm.com <no-reply@milesstorm.com>`; its domain must be verified in Resend
- `SITE_URL`: the website's address for links in emails, e.g. `https://milesstorm.com` (default: `BFF_CALLBACK_URL`)
- `ENSHROUDED_RPC_PASSWORD`: password of the Enshrouded server's control port (homelab `enshrouded/manifests/control.yaml`). Without it the panel shows the server but can't restart or update it
- `ENSHROUDED_RPC_URL`: the control port, default `http://enshrouded-control.enshrouded.svc.cluster.local:9001/RPC2`
- `ENSHROUDED_QUERY_ADDR`: the game's query port, default `enshrouded.enshrouded.svc.cluster.local:15637`

## Account emails

Confirm email, reset a forgotten password, and confirm deleting an account
(`src/auth/account_email.rs`, emails built in `src/auth/mail.rs`). Each email has a
link to a page on the website with a one-time code; only the code's SHA-256 is stored
(`email_tokens`). Links work for 48 hours (confirm) or 1 hour (reset, delete), and
each kind can be sent once a minute and 10 times a day per account.

Setting up sending (once):
1. Make a Resend account and add the domain `milesstorm.com` (Domains → Add domain).
2. Add the DNS records Resend shows (SPF and DKIM, and a DMARC record if you have
   none) where the domain's DNS is managed, and wait until Resend shows it verified.
3. Create an API key with "Sending access" for that domain, and give it to auth as
   `RESEND_API_KEY`. Set `SITE_URL` to the site's public address.

Resend's free plan sends 100 emails a day and 3,000 a month. Emails that couldn't be
sent are logged (`sending an account email failed`, with Resend's reason) and counted
in `auth_emails_total{status="failed"}`.

## Invite links

Links that give whoever opens them a role, e.g. `arcane_user` for the Arcane dice test
group (`src/auth/invites.rs`). Make them in the website's admin panel, Invites tab: pick
the role, how many days the link works and how many people can use it. The link is shown
once, when made (only its SHA-256 is stored, in `invites`), and can be turned off there.
Someone without an account is asked to sign up or log in first, and joins right after.
Links use `SITE_URL`, like the emails.

## Enshrouded server panel

The website's `/enshrouded` page, for users with the `enshrouded` permission (give them the
`enshrouded` role in the admin panel or with an invite link): whether the server is up, players
online and for how long, whether a newer build is out, and Restart / Force update
(`src/auth/enshrouded.rs`). Force update waits until nobody is online (like homelab's
`force-update.sh`), unless the user picks "update now anyway", which disconnects players. Commands go to supervisord in the game's pod; each is stored in
`enshrouded_actions` with who sent it, and the next one can only come 2 minutes later.
The game's version comes from its query answer; the installed Steam build from homelab's
`website-version` program, compared with Steam's latest to tell whether an update is out (by
build time instead, until the pod has started with that program).

## Running the server

for development purposes you can add an .env file to the root folder and the server will automatically parse. However for production you need to set the environment variables manually for security purposes.

## Tests

`cargo test` starts a throwaway Postgres in Docker for the tests that need a database
(testcontainers), so Docker has to be running. To use an existing empty database instead,
set `TEST_DATABASE_URL`. `cargo clippy --all-targets -- -D warnings` also enforces the
tracing rules: `clippy.toml` bans the untraced I/O and task APIs, and every exception is
listed in `allowed_raw_io.toml` (see TRACING.md at the repository root).
