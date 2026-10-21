use crate::model::valid_domain;
use anyhow::{Context, Result, bail};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
#[derive(Clone, Debug)]
pub struct Entry {
    pub domain: String,
    pub ip: std::net::Ipv4Addr,
}
const BEGIN: &str = "# >>> Fastly Route >>>";
const END: &str = "# <<< Fastly Route <<<";

pub fn path() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
            .join("System32/drivers/etc/hosts")
    } else {
        PathBuf::from("/etc/hosts")
    }
}

// Work on bytes so legacy encodings, BOMs, comments and line endings survive.
pub fn remove_block(original: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut inside = false;
    let mut seen = false;
    for line in original.split_inclusive(|&b| b == b'\n') {
        let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
        let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
        if trimmed == BEGIN.as_bytes() {
            if inside || seen {
                bail!("hosts 標記重複，請先手動檢查");
            }
            inside = true;
            seen = true;
        } else if trimmed == END.as_bytes() {
            if !inside {
                bail!("hosts 結束標記沒有起始標記");
            }
            inside = false;
        } else if !inside {
            out.extend_from_slice(line);
        }
    }
    if inside {
        bail!("hosts 設定區塊不完整，已停止寫入");
    }
    Ok(out)
}

pub fn compose(original: &[u8], routes: &[Entry]) -> Result<Vec<u8>> {
    let mut out = remove_block(original)?;
    if routes.is_empty() {
        return Ok(out);
    }
    let text = String::from_utf8_lossy(&out);
    let mut domains = std::collections::HashSet::new();
    for route in routes {
        let domain = route.domain.as_str();
        anyhow::ensure!(valid_domain(domain), "不合法的 hosts 網域");
        if !domains.insert(domain) {
            bail!("服務重複：{domain}");
        }
        for line in text.lines() {
            let fields: Vec<_> = line
                .split('#')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .collect();
            if fields
                .iter()
                .skip(1)
                .any(|s| s.trim_end_matches('.').eq_ignore_ascii_case(domain))
            {
                bail!("{domain} 已有其他 hosts 設定，請先處理衝突");
            }
        }
    }
    let nl = if original.windows(2).any(|w| w == b"\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    // Keep separator inside our block by requiring a newline in a non-empty original.
    if !out.is_empty() && !out.ends_with(b"\n") {
        bail!("hosts 最後一行沒有換行，請先補上換行後重試");
    }
    out.extend_from_slice(format!("{BEGIN}{nl}").as_bytes());
    for route in routes {
        out.extend_from_slice(format!("{} {}{nl}", route.ip, route.domain).as_bytes());
    }
    out.extend_from_slice(format!("{END}{nl}").as_bytes());
    Ok(out)
}

pub fn write_at(path: &Path, routes: &[Entry]) -> Result<PathBuf> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .context("無法寫入 hosts；請以系統管理員身分執行")?;
    file.try_lock().context("hosts 正被使用，請稍後再試")?;
    let mut original = Vec::new();
    file.read_to_end(&mut original)?;
    let updated = compose(&original, routes)?;
    if updated == original {
        return Ok(path.to_path_buf());
    }
    let backup = path.with_extension(format!(
        "fastly-route-{}.bak",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let mut backup_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
        .context("無法建立備份；請以系統管理員身分執行")?;
    backup_file.write_all(&original)?;
    backup_file.sync_all()?;
    // Write in place to preserve the system file's ACLs and ownership.
    file.seek(SeekFrom::Start(0))?;
    if let Err(error) = (|| -> std::io::Result<()> {
        file.write_all(&updated)?;
        file.set_len(updated.len() as u64)?;
        file.sync_all()
    })() {
        let rollback = (|| -> std::io::Result<()> {
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&original)?;
            file.set_len(original.len() as u64)?;
            file.sync_all()
        })();
        bail!(
            "寫入失敗：{error}；回復結果：{rollback:?}；備份：{}",
            backup.display()
        );
    }
    Ok(backup)
}

pub fn status() -> String {
    match fs::read(path()) {
        Ok(bytes) => match remove_block(&bytes) {
            Ok(clean) if clean != bytes => "已套用自訂路線".into(),
            Ok(_) => "使用系統路線".into(),
            Err(e) => format!("設定異常：{e}"),
        },
        Err(e) => format!("無法讀取 hosts：{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry() -> Entry {
        Entry {
            domain: "raw.githubusercontent.com".into(),
            ip: "151.101.1.133".parse().unwrap(),
        }
    }
    #[test]
    fn roundtrip_preserves_bytes() {
        for original in [
            b"# legacy \xff\r\n127.0.0.1 localhost\r\n".as_slice(),
            b"",
            b"# hi\n",
        ] {
            let applied = compose(original, &[entry()]).unwrap();
            assert_eq!(remove_block(&applied).unwrap(), original);
            assert_eq!(compose(&applied, &[entry()]).unwrap(), applied);
        }
    }
    #[test]
    fn refuses_broken_blocks() {
        assert!(remove_block(format!("{BEGIN}\nanything\n").as_bytes()).is_err());
        assert!(remove_block(format!("{END}\n").as_bytes()).is_err());
    }
    #[test]
    fn refuses_conflicts() {
        assert!(compose(b"1.2.3.4 RAW.githubusercontent.com. # user\n", &[entry()]).is_err());
    }
    #[test]
    fn rejects_injected_domain() {
        let mut r = entry();
        r.domain = "x.com\n1.1.1.1 evil.com".into();
        assert!(compose(b"", &[r]).is_err());
    }
    #[test]
    fn backup_and_restore_on_temp_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("hosts");
        let old = b"127.0.0.1 localhost\n";
        fs::write(&p, old).unwrap();
        let backup = write_at(&p, &[entry()]).unwrap();
        assert_eq!(fs::read(backup).unwrap(), old);
        write_at(&p, &[]).unwrap();
        assert_eq!(fs::read(p).unwrap(), old);
    }
}
