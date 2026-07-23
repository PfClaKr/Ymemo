//! Syncthing 전송 계층: 바이너리를 자식 프로세스로 띄우고 REST API 로 제어한다.
//!
//! 결정 사항(재론 금지): Syncthing 을 재구현/임베드하지 않고 **통째로 번들**한다.
//! 이 모듈은 전송만 담당한다 — vault 디렉터리(암호화 로그)를 공유 폴더로 등록하면
//! Syncthing 이 기기 간 파일 전파를 맡고, 병합은 `Vault::rebuild` 가 한다.
//! 로그는 기기별 append-only 라 파일 충돌이 없고, 내용은 이미 E2E 암호화돼 있어
//! 전송 계층을 신뢰할 필요가 없다.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// 데몬 최초 기동(키 생성 포함) 대기 한도.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// 실행 중인 Syncthing 자식 프로세스 핸들 + REST 클라이언트.
///
/// Drop 시 데몬을 종료시킨다. (REST shutdown 시도 후 kill)
pub struct Syncthing {
    child: Child,
    base_url: String,
    api_key: String,
}

/// 이 vault 를 공유 중인 다른 기기 하나. (`shared_devices` 가 돌려준다.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedDevice {
    /// syncthing device-id (공유 해제 시 식별자).
    pub id: String,
    /// 사람이 읽을 이름 (설정에 없으면 빈 문자열).
    pub name: String,
    /// 지금 연결돼 있는지.
    pub connected: bool,
}

impl Syncthing {
    /// syncthing 바이너리 탐색: `YMEMO_SYNCTHING_BIN` → 실행파일 옆(번들) → PATH 순.
    ///
    /// 번들본은 사용자가 syncthing 사용 사실을 눈치채지 못하도록 `ymemo-sync`
    /// (Windows: `ymemo-sync.exe`) 라는 이름으로 배포한다 — ps/작업관리자에도 이 이름으로
    /// 뜬다. 원래 이름(`syncthing`)은 개발 환경(PATH 설치본) 폴백으로만 남긴다.
    pub fn find_binary() -> Option<PathBuf> {
        let bundled = if cfg!(windows) { "ymemo-sync.exe" } else { "ymemo-sync" };
        let plain = if cfg!(windows) { "syncthing.exe" } else { "syncthing" };

        if let Ok(p) = std::env::var("YMEMO_SYNCTHING_BIN") {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
        // 릴리스 패키지는 리네임한 바이너리를 앱 실행파일 옆(또는 같은 설치 폴더)에 둔다.
        if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
            for name in [bundled, plain] {
                let cand = dir.join(name);
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
        // 마지막으로 PATH (개발 환경에서 시스템 설치본 사용).
        let paths = std::env::var_os("PATH")?;
        for name in [bundled, plain] {
            if let Some(hit) = std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file()) {
                return Some(hit);
            }
        }
        None
    }

    /// 데몬 기동: 전용 home 디렉터리, 임의의 로컬 포트, 브라우저/기본폴더 없이.
    ///
    /// 최초 기동이면 키·설정 생성까지 기다린다. API key 는 home 의 config.xml 에서 읽는다.
    pub fn spawn(binary: &Path, home_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(home_dir)?;
        let port = free_port()?;
        let gui = format!("127.0.0.1:{port}");

        let mut cmd = Command::new(binary);
        cmd.arg("serve")
            .arg("--home")
            .arg(home_dir)
            .args(["--gui-address", &gui, "--no-browser", "--no-restart"])
            .env("STNOUPGRADE", "1") // 자동 업그레이드 금지 (우리가 번들한 버전 고정)
            // v1 은 --no-default-folder 플래그도 있지만 v2 에서 제거됨 → 환경변수로 통일
            .env("STNODEFAULTFOLDER", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Windows: 콘솔 서브시스템인 syncthing 자식이 자기 콘솔 창을 띄우지 않게 한다.
        // (앱을 GUI 서브시스템으로 바꾸면 자식이 새 콘솔을 만들어 창이 깜빡이거나 떠 있는다)
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = cmd
            .spawn()
            .with_context(|| format!("syncthing 실행 실패: {}", binary.display()))?;

        let mut st = Self {
            child,
            base_url: format!("http://{gui}"),
            api_key: String::new(),
        };

        // config.xml 이 생기고 <apikey> 가 채워질 때까지 대기.
        let config_path = home_dir.join("config.xml");
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        st.api_key = loop {
            if let Ok(xml) = std::fs::read_to_string(&config_path) {
                if let Some(key) = parse_api_key(&xml) {
                    break key;
                }
            }
            if Instant::now() > deadline {
                bail!("syncthing config.xml 대기 시간 초과: {}", config_path.display());
            }
            if let Some(status) = st.child.try_wait()? {
                bail!("syncthing 이 조기 종료됨: {status}");
            }
            std::thread::sleep(Duration::from_millis(200));
        };

        // REST 가 응답할 때까지 대기.
        while st.ping().is_err() {
            if Instant::now() > deadline {
                bail!("syncthing REST 응답 대기 시간 초과 ({})", st.base_url);
            }
            if let Some(status) = st.child.try_wait()? {
                bail!("syncthing 이 조기 종료됨: {status}");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(st)
    }

    fn ping(&self) -> Result<()> {
        ureq::get(format!("{}/rest/system/ping", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        Ok(())
    }

    /// 이 데몬의 Syncthing 기기 ID (다른 기기와 페어링할 때 QR 로 전달할 값).
    pub fn device_id(&self) -> Result<String> {
        let mut res = ureq::get(format!("{}/rest/system/status", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        let status: serde_json::Value = res.body_mut().read_json()?;
        status["myID"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| anyhow!("system/status 응답에 myID 없음"))
    }

    /// vault 디렉터리를 공유 폴더로 등록한다. 이미 있으면 손대지 않는다
    /// (기존 설정의 peer 목록을 덮어쓰지 않기 위해).
    pub fn ensure_folder(&self, folder_id: &str, label: &str, path: &Path) -> Result<()> {
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        if ureq::get(&url).header("X-API-Key", &self.api_key).call().is_ok() {
            return Ok(()); // 이미 등록됨
        }
        ureq::put(&url)
            .header("X-API-Key", &self.api_key)
            .send_json(serde_json::json!({
                "id": folder_id,
                "label": label,
                "path": path.to_string_lossy(),
                "type": "sendreceive",
                "fsWatcherEnabled": true,
                "rescanIntervalS": 60,
            }))?;
        Ok(())
    }

    /// 상대 기기를 등록하고 폴더를 공유한다 (페어링의 절반; 상대 기기도 같은 작업 필요).
    pub fn share_folder_with(&self, folder_id: &str, peer_device_id: &str) -> Result<()> {
        // 1. 기기 등록 (있으면 덮어써도 무해).
        ureq::put(format!("{}/rest/config/devices/{peer_device_id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_json(serde_json::json!({ "deviceID": peer_device_id }))?;

        // 2. 폴더 설정에 기기 추가.
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        let mut res = ureq::get(&url).header("X-API-Key", &self.api_key).call()?;
        let mut folder: serde_json::Value = res.body_mut().read_json()?;
        let devices = folder["devices"]
            .as_array_mut()
            .ok_or_else(|| anyhow!("folder 설정에 devices 배열 없음"))?;
        if !devices.iter().any(|d| d["deviceID"] == peer_device_id) {
            devices.push(serde_json::json!({ "deviceID": peer_device_id }));
            ureq::put(&url).header("X-API-Key", &self.api_key).send_json(&folder)?;
        }
        Ok(())
    }

    /// 이 폴더를 공유 중인 다른 기기 목록 (자기 자신은 제외).
    ///
    /// 폴더의 devices 목록에 기기 이름(설정)과 현재 연결 상태(연결 정보)를 합쳐 준다.
    pub fn shared_devices(&self, folder_id: &str) -> Result<Vec<SharedDevice>> {
        let my_id = self.device_id()?;

        // 이 폴더에 붙어 있는 기기 ID 들.
        let mut res = ureq::get(format!("{}/rest/config/folders/{folder_id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        let folder: serde_json::Value = res.body_mut().read_json()?;
        let ids: Vec<String> = folder["devices"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|d| d["deviceID"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // 기기 이름 맵 (사용자가 붙인 라벨; 없을 수 있음).
        let mut res = ureq::get(format!("{}/rest/config/devices", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        let devices: serde_json::Value = res.body_mut().read_json()?;
        let name_of = |id: &str| -> String {
            devices
                .as_array()
                .and_then(|a| a.iter().find(|d| d["deviceID"] == id))
                .and_then(|d| d["name"].as_str())
                .unwrap_or("")
                .to_string()
        };

        // 현재 연결 상태.
        let mut res = ureq::get(format!("{}/rest/system/connections", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        let conns: serde_json::Value = res.body_mut().read_json()?;

        let mut out = Vec::new();
        for id in ids {
            if id == my_id {
                continue; // 자기 자신은 목록에 넣지 않는다
            }
            let connected = conns["connections"][&id]["connected"].as_bool().unwrap_or(false);
            let name = name_of(&id);
            out.push(SharedDevice { id, name, connected });
        }
        Ok(out)
    }

    /// 상대 기기를 이 폴더 공유에서 제거하고 기기 설정도 지운다(연결 완전 차단).
    ///
    /// 자기 자신은 지울 수 없다. 상대 기기가 자기 쪽에서도 지워야 완전히 끊긴다 —
    /// 여기서 지우면 이 기기는 더 이상 상대에게 로그를 보내거나 받지 않는다.
    pub fn unshare_folder_with(&self, folder_id: &str, peer_device_id: &str) -> Result<()> {
        if peer_device_id == self.device_id()? {
            bail!("자기 자신은 공유 해제할 수 없습니다");
        }
        // 1. 폴더 devices 목록에서 제거.
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        let mut res = ureq::get(&url).header("X-API-Key", &self.api_key).call()?;
        let mut folder: serde_json::Value = res.body_mut().read_json()?;
        if let Some(devices) = folder["devices"].as_array_mut() {
            devices.retain(|d| d["deviceID"] != peer_device_id);
        }
        ureq::put(&url).header("X-API-Key", &self.api_key).send_json(&folder)?;

        // 2. 기기 설정 자체도 제거 (없어도 무해).
        let _ = ureq::delete(format!("{}/rest/config/devices/{peer_device_id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call();
        Ok(())
    }

    /// 데몬 정상 종료. (Drop 에서도 호출되지만, 명시적으로 부르면 에러를 볼 수 있다.)
    pub fn shutdown(mut self) -> Result<()> {
        self.shutdown_inner()
    }

    fn shutdown_inner(&mut self) -> Result<()> {
        let _ = ureq::post(format!("{}/rest/system/shutdown", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_empty();
        // 잠시 기다렸다가 안 죽으면 강제 종료.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            if Instant::now() > deadline {
                self.child.kill().ok();
                self.child.wait()?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Syncthing {
    fn drop(&mut self) {
        let _ = self.shutdown_inner();
    }
}

/// config.xml 에서 `<apikey>...</apikey>` 를 뽑아낸다. (XML 파서 의존성 없이 충분)
fn parse_api_key(xml: &str) -> Option<String> {
    let start = xml.find("<apikey>")? + "<apikey>".len();
    let end = xml[start..].find("</apikey>")? + start;
    let key = xml[start..end].trim();
    (!key.is_empty()).then(|| key.to_string())
}

/// 사용 가능한 로컬 포트 하나를 고른다.
fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_api_key_from_config_xml() {
        let xml = r#"<configuration version="37">
            <gui enabled="true" tls="false">
                <address>127.0.0.1:8384</address>
                <apikey>abcDEF123456</apikey>
            </gui>
        </configuration>"#;
        assert_eq!(parse_api_key(xml).as_deref(), Some("abcDEF123456"));
        assert_eq!(parse_api_key("<gui></gui>"), None);
        assert_eq!(parse_api_key("<apikey></apikey>"), None);
    }

    /// 실제 데몬 왕복. syncthing 바이너리가 있을 때만 돈다
    /// (`YMEMO_SYNCTHING_BIN` 또는 PATH). 없으면 조용히 스킵.
    #[test]
    fn spawn_configure_shutdown() {
        let Some(bin) = Syncthing::find_binary() else {
            eprintln!("skip: syncthing 바이너리 없음");
            return;
        };
        let home = std::env::temp_dir().join(format!("ymemo-st-{}", uuid::Uuid::new_v4()));
        let folder = std::env::temp_dir().join(format!("ymemo-vault-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();

        let st = Syncthing::spawn(&bin, &home).unwrap();
        let id = st.device_id().unwrap();
        assert!(!id.is_empty(), "기기 ID 가 비어 있음");

        st.ensure_folder("ymemo-vault", "Ymemo Vault", &folder).unwrap();
        st.ensure_folder("ymemo-vault", "Ymemo Vault", &folder).unwrap(); // 멱등

        st.shutdown().unwrap();
        std::fs::remove_dir_all(&home).ok();
        std::fs::remove_dir_all(&folder).ok();
    }
}
