use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, net::Ipv4Addr};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Benchmark {
    Github,
    Twitch,
    Steam,
    ViaGithub,
    Url(String),
    None,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Host {
    pub domain: String,
    pub suffix: u8,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Service {
    pub id: String,
    pub name: String,
    pub domain: String,
    pub probe_path: String,
    pub suffix: u8,
    pub hosts: Vec<Host>,
    pub benchmark: Benchmark,
    pub custom: bool,
}
pub fn builtins() -> Vec<Service> {
    vec![
        Service {
            id: "github".into(),
            name: "GitHub 下載".into(),
            domain: "raw.githubusercontent.com".into(),
            probe_path: "/github/gitignore/main/README.md".into(),
            suffix: 133,
            hosts: vec![
                host("raw.githubusercontent.com", 133),
                host("release-assets.githubusercontent.com", 133),
            ],
            benchmark: Benchmark::Github,
            custom: false,
        },
        Service {
            id: "x".into(),
            name: "X 圖片".into(),
            domain: "pbs.twimg.com".into(),
            probe_path: "/profile_images/1683325380441128960/yRsRRjGO_400x400.jpg".into(),
            suffix: 159,
            hosts: vec![host("pbs.twimg.com", 159)],
            benchmark: Benchmark::ViaGithub,
            custom: false,
        },
        Service {
            id: "steam".into(),
            name: "Steam 商店".into(),
            domain: "cdn.fastly.steamstatic.com".into(),
            probe_path: "/store/home/store_home_share.jpg".into(),
            suffix: 52,
            hosts: vec![
                host("store.fastly.steamstatic.com", 52),
                host("cdn.fastly.steamstatic.com", 52),
                host("shared.fastly.steamstatic.com", 52),
            ],
            benchmark: Benchmark::Steam,
            custom: false,
        },
        Service {
            id: "twitch".into(),
            name: "Twitch 網頁與圖片".into(),
            domain: "static-cdn.jtvnw.net".into(),
            probe_path: "/jtv_user_pictures/xarth/404_user_70x70.png".into(),
            suffix: 214,
            hosts: vec![
                host("www.twitch.tv", 214),
                host("gql.twitch.tv", 214),
                host("static-cdn.jtvnw.net", 214),
                host("assets.twitch.tv", 214),
                host("k.twitchcdn.net", 167),
            ],
            benchmark: Benchmark::Twitch,
            custom: false,
        },
    ]
}
fn host(domain: &str, suffix: u8) -> Host {
    Host {
        domain: domain.into(),
        suffix,
    }
}
pub fn same_subnet(ip: Ipv4Addr, suffix: u8) -> Ipv4Addr {
    let mut octets = ip.octets();
    octets[3] = suffix;
    octets.into()
}
pub fn valid_domain(value: &str) -> bool {
    value.len() <= 253
        && value.contains('.')
        && value.parse::<std::net::IpAddr>().is_err()
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
pub fn normalize_domain(value: &str) -> Result<String> {
    let domain = value.trim().trim_end_matches('.').to_ascii_lowercase();
    ensure!(
        valid_domain(&domain),
        "請輸入有效網域，不含 https://、路徑、連接埠或 IP"
    );
    Ok(domain)
}
pub fn sample_url(value: &str, domain: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).context("測速網址格式錯誤")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some(domain)
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "測速網址必須是同一網域的 HTTPS 網址（443），且不可包含帳密或 #"
    );
    Ok(url)
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Sample {
    pub mbps: f64,
    pub bytes: u64,
    pub seconds: f64,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Route {
    pub service: String,
    pub ip: Ipv4Addr,
    pub pop: String,
    pub latency: f64,
    pub jitter: f64,
    pub successes: usize,
    pub baseline: bool,
    pub samples: Vec<Sample>,
    pub smooth: usize,
    pub median: f64,
    pub source: String,
}
impl Route {
    pub fn measured(&self) -> bool {
        self.samples.len() == 3
            && self
                .samples
                .iter()
                .any(|s| s.bytes > 0 && s.error.is_none())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Report {
    pub routes: Vec<Route>,
    pub notes: Vec<String>,
    pub created: u64,
    pub services: Vec<Service>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub custom: Vec<Service>,
    pub enabled: BTreeSet<String>,
    pub auto_check: bool,
    pub prerelease: bool,
    pub global: bool,
    pub region: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            custom: vec![],
            enabled: ["github", "x", "twitch", "steam"].map(str::to_owned).into(),
            auto_check: true,
            prerelease: false,
            global: false,
            region: String::new(),
        }
    }
}
impl Settings {
    pub fn services(&self) -> Vec<Service> {
        let mut s = builtins();
        s.extend(self.custom.clone());
        s
    }
}
pub fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_hosts_injection() {
        for s in [
            "x.com\n1.2.3.4 evil.com",
            "localhost",
            "1.2.3.4",
            "x.com:443",
            "https://x.com",
            "-x.com",
            "x..com",
        ] {
            assert!(normalize_domain(s).is_err(), "{s}");
        }
        assert_eq!(
            normalize_domain("FILES.PYTHONHOSTED.ORG.").unwrap(),
            "files.pythonhosted.org"
        );
    }
    #[test]
    fn validates_sample_origin() {
        for u in [
            "http://x.com/a",
            "https://x.com.evil.com/a",
            "https://x.com:444/a",
            "https://user@x.com/a",
            "https://x.com/a#b",
        ] {
            assert!(sample_url(u, "x.com").is_err());
        }
        assert!(sample_url("https://x.com/file?a=1", "x.com").is_ok());
    }
}
