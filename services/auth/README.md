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

## Running the server

for development purposes you can add an .env file to the root folder and the server will automatically parse. However for production you need to set the environment variables manually for security purposes.
