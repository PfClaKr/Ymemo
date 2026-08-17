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
| 데스크탑 UI | **Slint** (`ymemo-desktop`) | 순수 Rust GUI, 웹뷰 없음(저메모리), 트레이 상주 스티커 메모 |
| 모바일 UI | **Flutter** (`apps/mobile`, 진행 중) | Android/iOS, `flutter_rust_bridge` 로 코어 FFI (`ymemo-ffi`) |
| 로컬 저장소 | **SQLite** (`rusqlite`, bundled) | 각 기기의 materialized view |
| CRDT | **Automerge** | 순서 무관 변경 병합 — P2P 와 궁합 |
| 암호화 | **RustCrypto** | XChaCha20-Poly1305 + Argon2id. 순수 Rust라 크로스컴파일에 시스템 라이브러리가 필요 없다 |
| 다국어 | **자체 카탈로그** (`ymemo-i18n`) | `i18n/*.json` 한 벌을 코어·UI·모바일이 공유 (ko/en) |
| 동기화 전송 | **Syncthing 번들** | 발견·NAT 통과·relay 를 위임. 바이너리/gomobile 라이브러리로 동봉하고 REST API 로 제어 |

### 동기화 구조

Syncthing 은 **암호화된 파일을 나르는 운반책**일 뿐이고, 그 위 로직은 전부 `ymemo-core` 가 담당한다.

```
[ymemo-core / Rust]  메모 · CRDT 병합 · E2E 암호화
        ↓  암호화된 change 로그를 vault 폴더에 기록
[Syncthing]          그 폴더를 기기 간 P2P 로 운반 (발견 · NAT 통과 · relay · 전송 TLS)
```

vault 폴더 구조:

```
vault/
├── vault.json              # Argon2id salt + 키 확인용 카나리. 생성 시 1회만 기록(불변)
└── logs/<device-id>.ymlog  # 기기별 append-only 암호화 로그 (레코드 = automerge change)
```

각 기기는 **자기 로그 파일 끝에만** 덧붙이므로 두 기기가 같은 파일을 건드릴 일이 없다
→ 파일 레벨 충돌 0, 내용 병합은 CRDT 가 담당. 사진 첨부는 이후 내용해시(content-addressed)
blob 을 같은 폴더에 두는 방식으로 확장한다.

암호화가 어디까지를 지키는지(그리고 **로컬 캐시는 평문**이라는 점)는 [SECURITY.md](SECURITY.md) 참조.

## 저장소 구조

```
Ymemo/
├── Cargo.toml            # Rust 워크스페이스
├── rust-toolchain.toml   # stable (>= 1.87 필요)
├── i18n/                 # 번역 카탈로그 ko.json(정본) · en.json
├── crates/
│   ├── ymemo-core/       # 공유 코어: 모델·SQLite 캐시·암호화·automerge vault·Syncthing·페어링
│   ├── ymemo-desktop/    # Slint 데스크탑 앱
│   │   ├── ui/           # 화면별 .slint (app=진입, lock/list/sticky/settings/pairing/theme)
│   │   └── src/          # main(배선) · state · sticky · list · lock · pairing · sync · tray
│   ├── ymemo-i18n/       # 카탈로그 로더 (t! 매크로)
│   └── ymemo-ffi/        # 모바일용 FFI (flutter_rust_bridge)
├── apps/mobile/          # Flutter 앱 (Android 착수 단계)
└── packaging/            # .deb / .rpm / Inno Setup 스크립트 + 아이콘
```

## 빌드 & 실행

사전 요구: **Rust ≥ 1.87** (`rustup update stable`).

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
cargo test --workspace       # 전체 테스트
cargo run -p ymemo-desktop   # 데스크탑 앱 실행
```

첫 실행에서 마스터 암호를 정하면 vault 가 만들어진다. 앱 데이터는 플랫폼 데이터 디렉터리
(Linux 는 `~/.local/share/Ymemo`)에 있고, 암호화된 vault(`vault/`)만 동기화된다.

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

## 기능 (데스크탑)

- **트레이 상주 스티커** — 트레이 아이콘으로 목록을 토글하고, 메모마다 무프레임 스티커 창이 뜬다.
  본문이 곧 편집칸(자동 저장), 제목 바 더블클릭으로 접기, 창을 끌면 화면·다른 스티커에 자석 스냅.
- **꾸미기** — 메모별 색상 팔레트와 창 불투명도.
- **그룹** — 중첩 가능한 폴더 트리, 드래그&드롭으로 이동.
- **잠금** — 마스터 암호로 열고, 트레이에서 즉시 잠금, 자리 비움 시 자동 잠금,
  원하면 정해진 기간 동안 암호 없이 열기(기기 로컬 세션 키, 동기화 안 됨).
- **기기 연결** — QR/페어링 코드 또는 같은 LAN 에서 **6자리 코드**로 연결. 연결된 기기 목록/해제 제공.
- **한국어 · 영어** — 시스템 로캘 자동 감지, 설정에서 변경.

## 현재 상태 (로드맵)

- [x] **Phase 0** — 워크스페이스 스캐폴딩, `ymemo-core` SQLite CRUD, Slint 스티커 창 연결
- [x] **Phase 1** — Argon2id 키 유도 + change 로그 암호화 저장 (RustCrypto)
- [x] **Phase 2** — CRDT(Automerge) 병합 → SQLite 캐시 재구축 → UI 반영
- [x] **Phase 3** — Syncthing 번들 + REST 제어로 자동 동기화, 기기 페어링
- [x] **Phase 4a** — 데스크탑 잠금(수동·자리비움·기간 자동 해제), 설정 창, ko/en 다국어
- [x] 패키징/CI — `.deb` · `.rpm` · Windows 인스톨러를 릴리스 태그에서 자동 생성
- [ ] **Phase 4b** — 사진 첨부
- [ ] Flutter 모바일 앱 구현 (FFI 계층 `ymemo-ffi` 는 준비됨) + 모바일 Syncthing(gomobile `.aar`)
- [ ] macOS 지원 (트레이·패키징)

## 라이선스

GPL-3.0-only
