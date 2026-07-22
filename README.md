# Ymemo

여러 기기(Linux · Windows · Android)에서 쓰고 서로 **동기화**되는 메모 앱.
텍스트 메모와 사진을 저장하며, **자체 서버 없이(serverless)** 동작하는 것을 목표로 한다.

## 설계 철학: Local-first

각 기기가 **완전한 로컬 사본**을 갖고, 그 위에 동기화를 얹는다.

- **Local-first** — 오프라인에서도 완전히 동작, 데이터는 내 기기에
- **P2P** — 기기끼리 직접 동기화 (중앙 서버 없음)
- **E2E 암호화** — 동기화 경로에 올라가는 데이터는 항상 암호문
- **CRDT** — 여러 기기에서 동시 수정해도 자동 병합, 손실 없음

## 기술 스택

| 계층 | 선택 | 비고 |
|---|---|---|
| 공유 코어 | **Rust** (`ymemo-core`) | 데이터 모델·저장소·CRDT·암호화·동기화 |
| 데스크탑 UI | **Slint** (`ymemo-desktop`) | 순수 Rust GUI, 웹뷰 없음(저메모리), 스티커 메모 스타일 |
| 모바일 UI | **Flutter** (예정) | Android/iOS, `flutter_rust_bridge` 로 코어 FFI |
| 로컬 저장소 | **SQLite** (`rusqlite`, bundled) | 각 기기의 materialized view |
| CRDT | **Automerge** (예정) | 순서 무관 변경 병합 — P2P 와 궁합 |
| 암호화 | **libsodium** (예정) | XChaCha20-Poly1305 + Argon2id |
| 동기화 전송 | **Syncthing 번들** (예정) | 발견·NAT 통과·relay 를 위임. 바이너리/gomobile 라이브러리로 동봉하고 REST API 로 제어 |

### 동기화 구조

Syncthing 은 **암호화된 파일을 나르는 운반책**일 뿐이고, 그 위 로직은 전부 `ymemo-core` 가 담당한다.

```
[ymemo-core / Rust]  메모 · CRDT 병합 · E2E 암호화
        ↓  암호화된 change 로그 + 내용해시 blob 을 폴더에 기록
[Syncthing]          그 폴더를 기기 간 P2P 로 운반 (발견 · NAT 통과 · relay · 전송 TLS)
```

데이터를 **기기별 append-only 로그 + 내용해시(content-addressed) blob** 으로 두어,
두 기기가 같은 파일을 건드릴 일이 없다 → 파일 레벨 충돌 0, 내용 병합은 CRDT 가 담당.

## 저장소 구조

```
Ymemo/
├── Cargo.toml            # Rust 워크스페이스
├── rust-toolchain.toml   # stable (>= 1.85 필요)
├── crates/
│   ├── ymemo-core/       # 공유 코어 (데이터 모델 + SQLite, 향후 CRDT/암호화/동기화)
│   └── ymemo-desktop/    # Slint 데스크탑 앱
│       ├── ui/app.slint  # UI 정의
│       └── src/main.rs   # 코어 ↔ UI 연결
└── (mobile/)             # 향후 Flutter 앱
```

## 빌드 & 실행

사전 요구: **Rust ≥ 1.85** (`rustup update stable`).

### Linux 시스템 의존성

Slint 는 리눅스에서 `fontconfig` 를 링크한다:

```bash
sudo apt install libfontconfig1-dev
```

> dev 패키지를 못 깔면, 이미 설치된 `libfontconfig.so.1` 을 가리키는 pkg-config shim 을
> 만들고 `PKG_CONFIG_PATH` 로 지정하는 우회법도 있다 (로컬 `.cargo/config.toml`, gitignore 됨).

한글 등 CJK 표시에는 폰트가 필요하다: `sudo apt install fonts-noto-cjk`

### 명령

```bash
cargo test -p ymemo-core     # 코어 테스트
cargo run -p ymemo-desktop   # 데스크탑 앱 실행
```

개발 중에는 PATH 에 설치된 `syncthing` 을 그대로 쓴다. 릴리스에서는 syncthing 을
`ymemo-sync` 로 리네임해 인스톨러에 함께 넣는다 (아래 참조).

### 패키징 (인스톨러)

syncthing 을 인스톨러 안에 번들해 **사용자가 별도로 설치하지 않고**, 앱을 제거하면
syncthing 도 함께 지워진다. 사용자는 syncthing 사용 사실을 알 필요가 없다
(GUI 를 열지 않고, 프로세스도 `ymemo-sync` 로 뜬다).

```bash
# Debian/Ubuntu .deb (로컬 테스트). syncthing 바이너리 경로를 넘긴다.
packaging/linux/build-deb.sh \
  --app target/release/ymemo-desktop --sync /path/to/syncthing \
  --version 0.1.0 --outdir dist
# → dist/ymemo_0.1.0_amd64.deb  (syncthing 은 /usr/lib/ymemo/ymemo-sync 로 설치)

# Fedora .rpm — rpmbuild 가 필요하므로 Fedora 컨테이너에서 빌드한다.
packaging/linux/build-rpm.sh \
  --app target/release/ymemo-desktop --sync /path/to/syncthing \
  --version 0.1.0 --outdir dist
# → dist/ymemo-0.1.0-1.fc*.x86_64.rpm
```

Windows 는 Inno Setup(`packaging/windows/ymemo.iss`)으로 `ymemo-setup-x86_64.exe` 를
만든다. 셋 다 릴리스 태그(`v*`)에서 CI 가 자동 생성한다 (`.github/workflows/release.yml`).
Fedora 잡은 Fedora 컨테이너 안에서 데스크탑을 빌드해 라이브러리 호환을 맞춘다.

## 현재 상태 (로드맵)

- [x] **Phase 0** — 워크스페이스 스캐폴딩, `ymemo-core` SQLite CRUD, Slint 스티커 창 연결
- [ ] **Phase 1** — libsodium 키 유도 + change 로그 암호화 저장
- [ ] **Phase 2** — CRDT(Automerge) 병합, 폴더 감시 → UI 반영
- [ ] **Phase 3** — Syncthing 번들 + REST 제어로 자동 동기화
- [ ] **Phase 4** — 사진 첨부, 새 기기 QR 키 전달, 데스크탑 잠금/마스킹
- [ ] Flutter 모바일 앱 + 코어 FFI

## 라이선스

GPL-3.0-only
