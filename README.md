# RPCN

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
