use anyhow::{Context, Result};
use std::process::Command;

pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
pub fn restart() -> Result<()> {
    command(std::env::current_exe()?)
        .spawn()
        .context("無法重新啟動程式")?;
    Ok(())
}
pub fn elevate() -> Result<()> {
    #[cfg(windows)]
    {
        let exe = std::env::current_exe()?
            .to_string_lossy()
            .replace('\'', "''");
        let status = command("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                &format!("Start-Process -FilePath '{exe}' -Verb RunAs"),
            ])
            .status()?;
        anyhow::ensure!(status.success(), "管理員啟動已取消或失敗");
        Ok(())
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("請以 sudo 啟動此程式")
    }
}
pub fn flush_dns() -> String {
    #[cfg(windows)]
    {
        match command("ipconfig.exe").arg("/flushdns").output() {
            Ok(o) if o.status.success() => "DNS 快取已清除".into(),
            _ => "DNS 快取未能清除，請重新開啟瀏覽器".into(),
        }
    }
    #[cfg(not(windows))]
    {
        "請重新啟動瀏覽器以重新建立連線".into()
    }
}

// Credentials remain in memory and are only sent to the fixed GitHub API host.
pub fn github_token() -> Option<String> {
    for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(token) = std::env::var(key)
            && !token.trim().is_empty()
        {
            return Some(token);
        }
    }
    let output = command("gh")
        .args(["auth", "token", "--hostname", "github.com"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!token.is_empty()).then_some(token)
}
