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

## Match history API

The server records every finished two player room, and the stat server serves
those records:

```text
GET /{StatServerPath}/matches/{com_id}[?limit=n]
GET /{StatServerPath}/players/{npid}/matches[?limit=n]
```

Both return `match_id`, `room_id`, `timestamp`, the two npids and `winner`.
`limit` defaults to 50 and is capped at 500. An npid with no account is a `404`
rather than an empty list.

`winner` is `null` when the result was never recovered, which is not the same
as a draw. Nothing reports the result of a match - play is peer to peer - so it
is read afterwards out of the running record in the save the account uploads,
and a player who leaves without uploading one never reports.

## Per character ranks

For titles whose save layout the server understands, the ranks an account last
wrote are readable:

```text
GET /{StatServerPath}/players/{npid}/ranks?com_id={com_id}[&slot=n]
```

`slot` defaults to 1. A title the server has no layout for gets a `400`. The
response also carries `record`, the account's running win and loss totals as
the title itself keeps them.

## Leaderboard

```text
GET /{StatServerPath}/leaderboard/{com_id}[?limit=n][&slot=n]
```

Every account holding a save, ordered by its best character's rank, which is
also the rank the client offers for matchmaking. Each entry carries its
`position`, npid and online name, `best_rank`, `best_rank_points`,
`best_character`, the `record` the title keeps, and `server_record` - the
matches played on this server whose result it recovered, which is always the
smaller pair of numbers.

`limit` defaults to 100 and is capped at 10000. Banned accounts are left out,
and `ranked_players` counts the whole board rather than the part `limit`
returned.

The ranks live inside the saves rather than in any table, so a board is built
by reading every save of the title. It is therefore always cached, for at least
30 seconds, even where `StatServerCacheLife` would turn caching off.

## Reading from a browser

Every response carries `Access-Control-Allow-Origin: *`, and a preflight is
answered for `GET` alone. It does not allow `X-API-Key`, so the endpoints that
take the operator key stay callable from a backend and not from a page.

`/{StatServerPath}/usage` reports the name of every connected player. It also
holds their IP addresses, which are filled in only for a caller presenting
`X-API-Key`; everyone else gets a `null` in each address's place.

## Operator rank edit

With `ExternalUserApiKey` set, an operator can correct a rank:

```text
POST /{StatServerPath}/admin/character-rank
X-API-Key: {ExternalUserApiKey}
Content-Type: application/json
```

```json
{"npid":"example_user","com_id":"NPWR02973_00","character":14,"rank":25}
```

`slot` defaults to 1, `rank_points` to the bottom of the new rank. The edit is
written the way a save from the game is - a new `data_id` with the slot
repointed at it - so the save it replaced stays on disk to fall back on.

A connected account is refused with `409`, because the client holds its save
for the length of the session and writes it back on its next save, which would
discard the edit. Pass `"force": true` only for a session known to be stale.

## Starting and floor ranks

Some titles need a hand with ranks the player is not currently climbing with.
For `NPWR02973_00` the server gives a brand new account a starting rank, and
raises the rest of the roster when the account's best character crosses into a
new rank tier. Both only ever raise a rank, and only at the crossing, so a
character demoted afterwards stays where it fell.

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
