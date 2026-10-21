use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
const REPO: &str = "cycleapple/fastly-route";
const EXE: &str = "fastly-route.exe";
const MAX_EXE: u64 = 100 * 1024 * 1024;
#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub id: u64,
    pub name: String,
    pub size: u64,
    pub digest: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub prerelease: bool,
    pub draft: bool,
    pub assets: Vec<Asset>,
}
#[derive(Clone, Debug)]
pub struct Available {
    pub version: String,
    pub exe: Asset,
    pub sums: Asset,
}
#[derive(Clone, Debug)]
pub enum UpdateEvent {
    Checked(Option<Available>),
    Progress(String),
    Installed(PathBuf),
    Error(String),
}
fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("FastlyRoute/", env!("CARGO_PKG_VERSION")))
        .build()?)
}
pub fn choose(
    releases: Vec<Release>,
    current: &str,
    prerelease: bool,
) -> Result<Option<Available>> {
    let current = semver::Version::parse(current)?;
    let mut candidates: Vec<_> = releases
        .into_iter()
        .filter(|r| !r.draft && (prerelease || !r.prerelease))
        .filter_map(|r| {
            semver::Version::parse(r.tag_name.trim_start_matches('v'))
                .ok()
                .map(|v| (v, r))
        })
        .filter(|(v, _)| v > &current && (prerelease || v.pre.is_empty()))
        .collect();
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    if let Some((version, r)) = candidates.into_iter().next() {
        let exe = r
            .assets
            .iter()
            .find(|a| a.name == EXE)
            .context("新版缺少 Windows 執行檔")?
            .clone();
        let sums = r
            .assets
            .iter()
            .find(|a| a.name == "SHA256SUMS.txt")
            .context("新版缺少 SHA256SUMS.txt，拒絕更新")?
            .clone();
        ensure!(exe.size > 0 && exe.size <= MAX_EXE, "新版檔案大小不合理");
        Ok(Some(Available {
            version: version.to_string(),
            exe,
            sums,
        }))
    } else {
        Ok(None)
    }
}
pub fn check(current: &str, prerelease: bool) -> Result<Option<Available>> {
    let token = crate::platform::github_token();
    let c = client()?;
    let mut request = c.get(format!(
        "https://api.github.com/repos/{REPO}/releases?per_page=100"
    ));
    if let Some(t) = token {
        request = request.bearer_auth(t);
    }
    let response = request.send()?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("無法存取私人 repo。請先在 GitHub CLI 登入具有此 repo 權限的帳號，或設定 GH_TOKEN。");
    }
    let releases: Vec<Release> = response.error_for_status()?.json()?;
    choose(releases, current, prerelease)
}
fn asset_bytes(asset: &Asset, limit: u64, cancel: &AtomicBool) -> Result<Vec<u8>> {
    ensure!(asset.size <= limit, "更新資產超過大小限制");
    let token = crate::platform::github_token();
    let c = client()?;
    let mut url = reqwest::Url::parse(&format!(
        "https://api.github.com/repos/{REPO}/releases/assets/{}",
        asset.id
    ))?;
    for _ in 0..5 {
        ensure!(!cancel.load(Ordering::Relaxed), "更新已取消");
        let host = url.host_str().unwrap_or("");
        ensure!(
            url.scheme() == "https"
                && matches!(
                    host,
                    "api.github.com"
                        | "release-assets.githubusercontent.com"
                        | "objects.githubusercontent.com"
                ),
            "更新重導向來源不受信任"
        );
        let mut request = c
            .get(url.clone())
            .header("Accept", "application/octet-stream");
        if host == "api.github.com"
            && let Some(t) = &token
        {
            request = request.bearer_auth(t);
        }
        let mut response = request.send()?;
        if response.status().is_redirection() {
            url = url.join(
                response
                    .headers()
                    .get("location")
                    .context("缺少更新下載位置")?
                    .to_str()?,
            )?;
            continue;
        }
        response = response.error_for_status()?;
        ensure!(
            response.content_length().unwrap_or(0) <= limit,
            "更新檔過大"
        );
        let mut bytes = Vec::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            ensure!(!cancel.load(Ordering::Relaxed), "更新已取消");
            let n = response.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            ensure!(bytes.len() as u64 + n as u64 <= limit, "更新檔超過限制");
            bytes.extend_from_slice(&buffer[..n]);
        }
        ensure!(bytes.len() as u64 == asset.size, "下載檔案不完整");
        return Ok(bytes);
    }
    bail!("更新下載重導向過多")
}
pub fn checksum(text: &str, name: &str) -> Result<String> {
    let matches: Vec<_> = text
        .lines()
        .filter_map(|line| {
            let mut p = line.split_whitespace();
            let hash = p.next()?;
            let file = p.next()?.trim_start_matches('*');
            (file == name && p.next().is_none()).then_some(hash)
        })
        .collect();
    ensure!(matches.len() == 1, "校驗清單缺少或重複指定檔案");
    let hash = matches[0];
    ensure!(
        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "SHA-256 格式錯誤"
    );
    Ok(hash.to_ascii_lowercase())
}
pub fn verify_exe(bytes: &[u8], expected: &str) -> Result<()> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    ensure!(actual == expected, "SHA-256 不符，已取消更新");
    ensure!(
        bytes.len() >= 128 && &bytes[..2] == b"MZ",
        "更新不是 Windows 執行檔"
    );
    let pe = u32::from_le_bytes(bytes[60..64].try_into()?) as usize;
    ensure!(
        pe.checked_add(94).is_some_and(|end| end <= bytes.len()),
        "PE 標頭損壞"
    );
    ensure!(
        &bytes[pe..pe + 4] == b"PE\0\0"
            && u16::from_le_bytes(bytes[pe + 4..pe + 6].try_into()?) == 0x8664,
        "更新不是 Windows x64"
    );
    ensure!(
        u16::from_le_bytes(bytes[pe + 24..pe + 26].try_into()?) == 0x20b
            && u16::from_le_bytes(bytes[pe + 92..pe + 94].try_into()?) == 2,
        "更新不是無主控台的 Windows GUI 執行檔"
    );
    Ok(())
}
pub fn replace_at(destination: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let parent = destination.parent().context("程式路徑無父目錄")?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .context("程式所在目錄不可寫入；請移至使用者目錄或以管理員執行")?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    let backup = parent.join(format!(
        "fastly-route-{}.previous.exe",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    ensure!(!backup.exists(), "更新備份名稱衝突");
    fs::rename(destination, &backup).context("無法備份目前的執行檔")?;
    if let Err(error) = staged.persist_noclobber(destination) {
        let rollback = fs::rename(&backup, destination);
        bail!(
            "更新替換失敗：{}；還原：{rollback:?}；舊版在 {}",
            error.error,
            backup.display()
        );
    }
    Ok(backup)
}
pub fn install(
    update: &Available,
    cancel: &AtomicBool,
    emit: impl Fn(UpdateEvent),
) -> Result<PathBuf> {
    ensure!(
        cfg!(all(windows, target_arch = "x86_64")),
        "此更新包僅支援 Windows x64"
    );
    emit(UpdateEvent::Progress("取得 SHA-256 校驗清單…".into()));
    let sums = asset_bytes(&update.sums, 64 * 1024, cancel)?;
    let expected = checksum(std::str::from_utf8(&sums)?, EXE)?;
    if let Some(digest) = &update.exe.digest {
        ensure!(
            digest == &format!("sha256:{expected}"),
            "GitHub 資產雜湊與校驗清單不一致"
        );
    }
    emit(UpdateEvent::Progress(format!(
        "下載 v{}（{:.1} MiB）…",
        update.version,
        update.exe.size as f64 / 1048576.
    )));
    let bytes = asset_bytes(&update.exe, MAX_EXE, cancel)?;
    verify_exe(&bytes, &expected)?;
    ensure!(!cancel.load(Ordering::Relaxed), "更新已取消");
    emit(UpdateEvent::Progress("校驗通過，備份並替換程式…".into()));
    replace_at(&std::env::current_exe()?, &bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Requires network and private repository access"]
    fn live_update_check() {
        check(env!("CARGO_PKG_VERSION"), true).expect("Authenticated release check");
    }
    fn asset(name: &str) -> Asset {
        Asset {
            id: 1,
            name: name.into(),
            size: 1000,
            digest: None,
        }
    }
    fn release(version: &str, pre: bool) -> Release {
        Release {
            tag_name: version.into(),
            prerelease: pre,
            draft: false,
            assets: vec![asset(EXE), asset("SHA256SUMS.txt")],
        }
    }
    #[test]
    fn compares_semver_and_filters_pre() {
        assert!(
            choose(vec![release("v0.10.0", false)], "0.9.0", false)
                .unwrap()
                .is_some()
        );
        assert!(
            choose(vec![release("v1.0.0-beta.1", true)], "0.2.0", false)
                .unwrap()
                .is_none()
        );
        assert!(
            choose(vec![release("v0.1.0", false)], "0.2.0", true)
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn checksums_are_exact() {
        let hash = "a".repeat(64);
        assert_eq!(
            checksum(&format!("{hash}  fastly-route.exe\n"), EXE).unwrap(),
            hash
        );
        assert!(checksum(&format!("{hash}  evil-fastly-route.exe\n"), EXE).is_err());
        assert!(checksum(&format!("{hash}  {EXE}\n{hash}  {EXE}"), EXE).is_err());
    }
    #[test]
    fn rejects_corrupt_and_console_exe() {
        let mut b = vec![0; 256];
        b[..2].copy_from_slice(b"MZ");
        b[60..64].copy_from_slice(&64u32.to_le_bytes());
        b[64..68].copy_from_slice(b"PE\0\0");
        b[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
        b[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
        b[156..158].copy_from_slice(&2u16.to_le_bytes());
        let hash = format!("{:x}", Sha256::digest(&b));
        verify_exe(&b, &hash).unwrap();
        assert!(verify_exe(&b, &"0".repeat(64)).is_err());
        b[156] = 3;
        let hash = format!("{:x}", Sha256::digest(&b));
        assert!(verify_exe(&b, &hash).is_err());
    }
    #[test]
    fn replacement_preserves_rollback() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("app.exe");
        fs::write(&p, b"old").unwrap();
        let backup = replace_at(&p, b"new").unwrap();
        assert_eq!(fs::read(p).unwrap(), b"new");
        assert_eq!(fs::read(backup).unwrap(), b"old");
    }
}
