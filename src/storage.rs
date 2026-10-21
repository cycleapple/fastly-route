use crate::model::{Report, Settings};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
pub fn directory() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FASTLY_ROUTE_DATA_DIR") {
        let path = PathBuf::from(path);
        ensure!(path.is_absolute(), "FASTLY_ROUTE_DATA_DIR 必須是絕對路徑");
        return Ok(path);
    }
    Ok(directories::ProjectDirs::from("", "", "FastlyRoute")
        .context("無法取得資料目錄")?
        .data_local_dir()
        .to_owned())
}
pub fn save_at(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let parent = path.parent().context("資料路徑沒有父目錄")?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut tmp, value)?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
pub fn save_settings(settings: &Settings) -> Result<()> {
    save_at(&directory()?.join("settings.json"), settings)
}
pub fn load_settings() -> Result<Settings> {
    let p = directory()?.join("settings.json");
    if !p.exists() {
        return Ok(Settings::default());
    }
    let settings: Settings = serde_json::from_slice(&fs::read(p)?)?;
    let mut seen = std::collections::HashSet::new();
    for s in settings.services() {
        ensure!(
            crate::model::valid_domain(&s.domain)
                && s.hosts
                    .iter()
                    .all(|h| crate::model::valid_domain(&h.domain)),
            "設定含不合法網域"
        );
        ensure!(seen.insert(s.id.clone()), "設定含重複服務");
    }
    for s in &settings.custom {
        ensure!(
            s.custom
                && s.id == format!("custom:{}", s.domain)
                && s.hosts.len() == 1
                && s.hosts[0].domain == s.domain
                && s.hosts[0].suffix == s.suffix,
            "自訂服務設定異常"
        );
        match &s.benchmark {
            crate::model::Benchmark::Url(u) => {
                crate::model::sample_url(u, &s.domain)?;
            }
            crate::model::Benchmark::None => {}
            _ => anyhow::bail!("自訂測速方式異常"),
        }
    }
    Ok(settings)
}
pub fn save_report(report: &Report) -> Result<()> {
    save_at(&directory()?.join("scan.json"), report)
}
pub fn load_report(settings: &Settings) -> Result<Report> {
    let p = directory()?.join("scan.json");
    if !p.exists() {
        return Ok(Report::default());
    }
    let mut report: Report = serde_json::from_slice(&fs::read(p)?)?;
    let services = settings.services();
    report.routes.retain(|r| {
        report
            .services
            .iter()
            .any(|s| s.id == r.service && services.contains(s))
    });
    Ok(report)
}
pub fn purge() -> Result<()> {
    let dir = directory()?;
    for name in ["scan.json", "settings.json"] {
        let p = dir.join(name);
        if p.exists() {
            fs::remove_file(p)?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_settings_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        let mut s = Settings::default();
        save_at(&p, &s).unwrap();
        s.auto_check = false;
        save_at(&p, &s).unwrap();
        let loaded: Settings = serde_json::from_slice(&fs::read(p).unwrap()).unwrap();
        assert!(!loaded.auto_check);
    }
}
