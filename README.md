# RPCN

## Production deployment

Production images are built by GitHub Actions and pushed to Amazon ECR when a
`v*` tag is pushed. The production server keeps its active image version in
an untracked `.env` file and starts the ECR image with `compose.prod.yml`.

On the production server, create the deployment directory and data directory,
then copy `.env.example` to `.env` and set its values. GitHub Actions uploads
`compose.prod.yml` during each deployment, so the server does not need a Git
clone or Git credentials. `IMAGE_TAG` is managed by the deployment workflow;
do not use `latest`.

Runtime configuration must not be built into the image. Keep `rpcn.cfg`,
`servers.cfg`, `server_redirs.cfg`, `scoreboards.cfg`, and
`domains_whitelist.txt` in `RPCN_DATA_PATH` on the production server. Create
`rpcn.cfg` from `rpcn.cfg.example`. The deployment stops before restarting RPCN
if any required runtime configuration file is missing.

Create these repository configuration values in GitHub:

- Variables: `AWS_REGION`, `ECR_REPOSITORY`
- Secrets: `AWS_ROLE_ARN`, `PROD_HOST`, `PROD_USER`,
  `PROD_DEPLOY_PATH`, `PROD_SSH_PRIVATE_KEY`, `PROD_SSH_KNOWN_HOSTS`

The AWS role must be trusted by this repository's GitHub Actions OIDC provider
and allowed to push to the ECR repository. The production server needs Docker
Compose, the AWS CLI, and an IAM role or credentials allowed to pull from that
ECR repository. The workflow refreshes the ECR Docker login token before
pulling the image. `PROD_SSH_KNOWN_HOSTS` must contain the server's pinned host key
(for example, the output of `ssh-keyscan -H <server-ip>` obtained through a
trusted channel).

To roll back, set `IMAGE_TAG` in the server's `.env` to an earlier ECR image
tag and run:

```sh
docker compose --env-file .env -f compose.prod.yml up -d --pull always
```

RPCN is a server that implements multiplayer functionality for RPCS3.  
It implements rooms which permit matchmaking, scoreboards, title user storage(ie cloud saves), etc.  
All the settings and their descriptions are in rpcn.cfg.

## External user verification API

When `StatServer=true` and `ExternalUserApiKey` is set in `rpcn.cfg`, the stat server exposes a password-verification endpoint for trusted service integration:

```text
POST /{StatServerPath}/external/users/verify
X-API-Key: {ExternalUserApiKey}
Content-Type: application/json
```

```json
{"username":"example_user","password":"example_password"}
```

The response contains only `user_id`, `username`, `online_name`, `avatar_url`, `admin`, and `banned`. Invalid credentials always receive `401`, regardless of whether the username exists. The endpoint does not create a login session or issue a token.

This stat server is HTTP-only. Bind it to `127.0.0.1` and put an HTTPS reverse proxy in front of it before allowing external requests. Do not send passwords or the API key over an untrusted HTTP connection.

# FAQ

## Will RPCN work with real PS3s?

No.

# Special Thanks

A special thanks to the various authors of the following libraries that RPCN is using:
- [Rusqlite](https://github.com/rusqlite/rusqlite)  
Perfect library if you plan to use SQLite with rust. The author has been incredibly helpful in diagnosing SQLite issues, thanks!
- [Tokio](https://github.com/tokio-rs/tokio)  
The king of async for Rust.

And all the other libraries I'm forgetting(check Cargo.toml)!
Also thanks to everyone that contributed directly or indirectly to RPCN!
