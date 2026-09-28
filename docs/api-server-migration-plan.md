# 포크 전용 HTTP API 분리 계획 (stat_server → api_server)

## 목표

- `src/server/stat_server*`는 **upstream(RipleyTom/rpcn) 원본으로 되돌린다.** upstream 병합 시 충돌 지점을 없애기 위함.
- 포크에서 추가한 HTTP 기능만 새 모듈 `src/server/api_server/`로 옮기고, **axum**으로 구현한다.
- 새 API 서버는 stat 서버와 **별도 포트·별도 설정**으로 띄운다.

## 배경

- 포크 기준점: upstream `671cfbe` (v1.8.5, 2026-02-11). 이후 커밋은 모두 포크 작업이다.
- upstream stat 서버 라우트는 `GET /usage`(공개, 게임별 인원 수)와 `GET /score/{com_id}[/{table_id}]` 두 가지뿐이다.
- 포크가 stat 서버에 추가한 것:

| 라우트 | 추가 커밋 | 인증 | 사용처 |
|---|---|---|---|
| `GET /usage`에 `players_id`(online_name→IP) 추가 | `6bdf17e`, `5e4c192` | `X-API-Key` (`3404fa5`) | `script/rpcn-vpn-monitor.py`, `script/tdt_admin.py` |
| `GET /rooms/{com_id}` | `f3aa56d` | 없음 | 확인된 내부 사용처 없음 (외부 사용 여부 확인 필요) |
| `POST /external/users/verify` | `cd1b79e` | `X-API-Key` | `tag2now-BE` 로그인 (`src/auth/adapters/rpcn_stat.py`) |
| `POST /admin/users/info`, `POST /admin/users/ban` | `37c02b2` | `X-API-Key` + 관리자 계정 id/pw | 관리자 수동 호출 |

- 모듈 분할(`c62a226`)도 포크 작업이다. 현재 파일: `stat_server.rs`, `stat_server/{admin,auth,cache,external,response,rooms,router,score,usage}.rs`

### `/usage`의 `players_id`는 옮기지 않고 전용 라우트로 바꾼다

`players_id`는 인원 통계가 아니라 **접속 중인 사용자와 IP를 모아 악성 유저를 ban하기 위해** 기존 `/usage`에 얹은 것이다. 실제로 필요한 것은 사용자 식별자와 IP의 쌍이다. 현재 구현은 이 용도에 맞지 않는다.

- **키가 `online_name`뿐이다.** `online_name`은 DB에 UNIQUE 제약이 없고(`username`에만 있음) 가입 시 중복 검사도 없다. 같은 이름의 두 계정이 접속하면 `HashMap`에서 나중 IP가 앞의 것을 덮어쓰고, 한 명이 나가면 남은 사람의 항목까지 지워진다.
- **ban은 `username`(npid)으로 한다.** `online_name`만으로는 ban 대상을 정할 수 없다.
- **`tdt_admin.py`의 접속 확인이 틀린다.** `username`을 `online_name` 집합에서 찾으므로, 두 값이 다른 계정은 접속 중이어도 쓰기를 막지 못한다.
- **게임을 등록한 사용자만 나온다.** `get_com_id_with_redir`에서 추가하므로 로그인만 한 사용자는 빠진다.
- **IP가 `0.0.0.0`일 수 있다.** 게임 등록 시점의 UDP signaling 주소(`addr_p2p_ipv4`)를 복사하는데, UDP 패킷이 아직 오지 않았으면 초기값 그대로 저장되고 이후 갱신되지 않는다.

그래서 새 API 서버에서는 `/usage`를 제공하지 않는다. 인원 수는 upstream stat 서버의 공개 `/usage`로 충분하다. 대신 `GET /admin/sessions`가 접속 세션 목록을 준다.

- 관리자는 게임에서 보이는 `online_name`으로 항목을 찾고, 그 항목의 `npid`로 ban한다.
- 같은 `online_name`이 둘이면 두 항목이 모두 나오므로 구분할 수 있다.
- 서버 내부에서는 `client_infos`의 키(`user_id`)로 관리하므로 이름이 같아도 덮어쓰지 않는다.

## 코어 코드 변경

### 그대로 유지 (API 서버가 의존)

- `room_manager.rs`: `get_rooms()` 등 방 조회
- `database.rs`: `check_user`의 `banned`, `ban_user`, `get_user_admin_info`
- `client.rs`: `ClientSharedInfo::kick`(ban 시 접속 세션 종료), 로그인 시 ban 차단(`cmd_account.rs`)
- `main.rs` 이메일 도메인 화이트리스트 등 stat 서버와 무관한 변경

### 추가

- `ClientSharedInfo`에 `npid: String`, `online_name: String`, `ip: IpAddr`를 추가하고 로그인 시(`cmd_account.rs`의 `client_infos.insert`) 채운다.
- `ip`는 TCP 연결의 peer 주소다. `server.rs`의 `accept`에서 받은 `peer_addr`를 `Client::new`로 넘긴다. 로그인 시점부터 항상 있고 UDP signaling 여부와 무관하다.

### upstream으로 되돌림

- `game_tracker.rs`: `GameInfo.players`, `add_player`/`remove_player`
- `client.rs`: `get_com_id_with_redir`의 IP 추출과 `add_player`/`remove_player` 호출, `Drop`의 `remove_player`
- 되돌린 뒤 `git diff 671cfbe -- src/server/game_tracker.rs`가 비어 있어야 한다.

## 새 API 서버 설계

### 설정 (`rpcn.cfg`)

```
# 포크 전용 HTTP API 서버
ApiServer=false
ApiServerHost=127.0.0.1
ApiServerPort=31315
# X-API-Key 헤더 값. 비어 있으면 키가 필요한 라우트는 404
ApiServerApiKey=
```

- 기존 `ExternalUserApiKey`는 제거하고 `ApiServerApiKey`로 대체한다. 운영 서버 `rpcn.cfg`와 `/etc/sysconfig/rpcn-vpn-monitor`의 키 값은 그대로 옮기면 된다.
- `StatServer*` 설정은 upstream 그대로 둔다.
- 경로 prefix(`rpcn_stats` 같은)는 두지 않는다. 포트로 분리되므로 불필요.

### 라우트

| 라우트 | 인증 | 비고 |
|---|---|---|
| `GET /admin/sessions` | API 키 | 접속 중인 사용자 목록 (아래 형식). 새 라우트 |
| `GET /rooms/{com_id}` | 없음 | 기존과 동일하게 공개. 캐시 없음 |
| `POST /external/users/verify` | API 키 | body `{username|id, password|pw}` |
| `POST /admin/users/info` | API 키 + 관리자 계정 | body `{admin_username, admin_password, username}` |
| `POST /admin/users/ban` | API 키 + 관리자 계정 | 접속 중이면 `ClientSharedInfo::kick()` |

`GET /admin/sessions` 응답:

```json
{
  "sessions": [
    {"online_name": "Foo", "npid": "foo123", "ip": "1.2.3.4"}
  ]
}
```

- `client_infos`를 읽기 잠금으로 한 번 훑어 만든다. `online_name` 순으로 정렬한다.
- `ip`는 IPv4/IPv6 모두 문자열로 준다.
- 현재 게임(`com_id`)은 담지 않는다. 현재 게임은 `Client.current_game`에만 있어 `ClientSharedInfo`로 옮겨야 하고, 두 소비자 모두 게임별로 구분하지 않는다. 필요해지면 추가한다.
- 목록은 키 없이 조회할 수 없어야 한다. 관리자 계정 인증은 요구하지 않는다 (`rpcn-vpn-monitor.py`가 주기적으로 호출).

기존 라우트의 경로·응답 형식은 그대로 유지한다.

- `/score/*`와 `/usage`는 upstream stat 서버에 남으므로 옮기지 않는다.
- JSON은 `format!`/`write!` 수동 조립 대신 `serde` 구조체 + `axum::Json`으로 만든다. **기존 라우트의 필드 이름·타입은 기존 응답과 동일해야 한다** (소비자 호환).
- 에러 응답 형식 `{"error": "<code>"}`와 상태 코드(400/401/403/404/405/500)도 유지한다.

### 구조 (axum 0.8, hyper 1 기반)

```
src/server/api_server.rs          // start_api_server, Router 구성, 서버 루프(term_watch로 graceful shutdown)
src/server/api_server/state.rs    // ApiState { db_pool, client_infos, room_manager, api_key }
src/server/api_server/error.rs    // ApiError enum + IntoResponse ({"error": ...} + StatusCode)
src/server/api_server/auth.rs     // require_api_key 미들웨어(from_fn_with_state), 관리자 인증 헬퍼
src/server/api_server/sessions.rs
src/server/api_server/rooms.rs
src/server/api_server/external.rs
src/server/api_server/admin.rs
```

- 라우터 예시:
  ```rust
  let keyed = Router::new()
      .route("/admin/sessions", get(sessions::list))
      .route("/external/users/verify", post(external::verify))
      .route("/admin/users/info", post(admin::user_info))
      .route("/admin/users/ban", post(admin::ban))
      .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_api_key));
  let app = Router::new().route("/rooms/{com_id}", get(rooms::get_rooms)).merge(keyed).with_state(state);
  ```
- API 키 비교는 기존처럼 상수 시간 비교(`openssl::memcmp::eq`)를 유지한다.
- DB 접근(`check_user`는 argon2라 느림)은 기존처럼 `tokio::task::spawn_blocking`에서 수행한다.
- body 크기 제한: `DefaultBodyLimit::max(4096)`.
- `/admin/sessions`는 캐시하지 않는다. `client_infos` 읽기 한 번이라 비용이 작고, ban 직후 목록이 바로 반영돼야 한다.
- `server.rs`에서 `start_stat_server`와 나란히 `start_api_server(term_watch.clone(), ...)`를 호출한다.

## 작업 순서

1. `Cargo.toml`에 `axum = "0.8"` 추가.
2. `main.rs` `Config`에 `ApiServer*` 설정 추가, `ExternalUserApiKey` 제거.
3. 코어 변경 (위 "코어 코드 변경"): `ClientSharedInfo`에 `npid`/`online_name`/`ip` 추가, `game_tracker.rs`와 `client.rs`의 `players` 관련 코드 되돌림.
4. `src/server/api_server/` 구현 (위 라우트 5개). 기존 `stat_server/{rooms,external,admin,auth}.rs`의 로직을 옮기고, `sessions.rs`는 새로 작성한다.
5. stat 서버를 upstream으로 되돌린다.
   - `git show 671cfbe:src/server/stat_server.rs > src/server/stat_server.rs`
   - `src/server/stat_server/` 디렉터리 삭제
   - `server.rs`의 `start_stat_server` 호출 시그니처를 upstream(`term_watch, game_tracker`)으로 복원
   - 되돌린 뒤 `git diff 671cfbe -- src/server/stat_server.rs`가 비어 있어야 한다.
   - upstream stat 서버는 `score_cache`를 `self`에서 가져온다. 현재 코어와 컴파일되는지 확인하고, 불가피한 수정이 있으면 최소한으로 하고 이 문서에 기록한다.
6. `Cargo.toml`에서 stat 서버만 쓰던 `http-body-util`이 더 이상 필요 없으면 제거 (`serde`, `serde_json`은 axum 쪽에서 계속 사용).
7. 소비자 수정 (아래 목록).
8. 문서: `rpcn.cfg.example`, `README.md`, `CHANGELOG.md` 갱신.
9. 검증 (아래).

## 소비자 수정 목록

| 위치 | 변경 |
|---|---|
| `script/rpcn-vpn-monitor.py` | `STATS_URL` → `http://127.0.0.1:31315/admin/sessions`. `get_players`는 `players_id` 대신 `sessions`에서 `(online_name, ip)`를 읽는다. 알림에 `npid`도 함께 남겨 바로 ban할 수 있게 한다 |
| `script/tdt_admin.py` | `STAT_URL` → `http://127.0.0.1:31315/admin/sessions`. `online()`은 `sessions`의 **`npid`** 집합을 돌려준다(이 스크립트는 `username`으로 세이브를 찾으므로). 경고 문구의 `ExternalUserApiKey` → `ApiServerApiKey` |
| `tag2now-BE` | 운영 env `RPCN_STAT_URL`을 `http://127.0.0.1:31315`로 변경 (`src/shared/settings.py` 주석, `docs/spec/07-auth.md`, `README.md`, `docs/aws-setup.md`의 예시 URL도 갱신). 코드의 `_VERIFY_PATH`는 그대로 |
| `rpcn.cfg.example` | `ExternalUserApiKey` 제거, `ApiServer*` 추가 |
| 운영 서버 | `rpcn.cfg`에 `ApiServer=true` 등 추가. compose는 `network_mode: host`라 포트 매핑 변경 불필요. 방화벽/리버스 프록시에서 31315를 외부에 열지 않을 것 |
| `/rooms/{com_id}` 외부 사용자 | 있다면 새 포트 안내 (확인 필요) |

## 검증

- `cargo build`, `cargo test`, `cargo fmt --check`
- `git diff 671cfbe -- src/server/stat_server.rs src/server/game_tracker.rs` 결과 없음, `src/server/stat_server/` 없음
- 로컬 서버를 띄워 curl로 확인:
  - stat 서버: `GET /rpcn_stats/usage`(키 없이 200, upstream 형식, `players_id` 없음), `GET /rpcn_stats/score/...`
  - API 서버: 키 없음/틀림 → 403, 키 미설정 → 404
  - `/admin/sessions`:
    - 로그인만 하고 게임을 등록하지 않은 사용자도 나온다
    - `ip`가 `0.0.0.0`이 아니라 실제 접속 주소다
    - `online_name`이 같은 두 계정이 동시에 접속하면 두 항목이 모두 나오고, 한 명이 나가도 다른 항목은 남는다
    - 로그아웃/ban 직후 목록에서 빠진다
  - `/external/users/verify` 성공/401
  - `/admin/users/info`, `/admin/users/ban`: 비관리자 403, 없는 유저 404, 접속 중 유저 ban 시 `kicked: true` 및 클라이언트 연결 종료, 이후 로그인 거부
- `tag2now-BE` 로그인이 새 URL로 동작하는지
- `rpcn-vpn-monitor.py`가 `sessions`를 읽는지
- `tdt_admin.py`가 `username`과 `online_name`이 다른 계정의 접속을 막는지

## 참고: 현재 상태 (이 문서 작성 시점)

- `master` 최신 커밋: `37c02b2 feat(stat-server): add admin user info and ban api`
- ban 관련 커밋: `d3012b9`(로그인 차단), `3251732`(접속 세션 종료), `37c02b2`(관리자 API)
- unban API는 아직 없음 (필요 시 새 API 서버에 `POST /admin/users/unban`로 추가)
