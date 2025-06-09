# milesstorm.com auth server

### envrionment variables

- `SERVER_PORT` : the port that the server will run on
- `SERVER_IP`: the ip that the server will run on
- `RUST_LOG`: the log level for the server
- `DATABASE_URL`: the url for the database
- `CLIENT_ID`: the client id for the github oauth
- `CLIENT_SECRET`: the client secret for the github oauth

## Running the server

for development purposes you can add an .env file to the root folder and the server will automatically parse. However for production you need to set the environment variables manually for security purposes.
