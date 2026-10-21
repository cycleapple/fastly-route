use anyhow::{Context, Result, bail};
use ipnet::Ipv4Net;
use rayon::prelude::*;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    net::{Ipv4Addr, SocketAddr, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub struct Service {
    pub name: &'static str,
    pub domain: &'static str,
    pub path: &'static str,
    pub suffix: u8,
}
pub const SERVICES: [Service; 4] = [
    Service {
        name: "GitHub Raw",
        domain: "raw.githubusercontent.com",
        path: "/github/gitignore/main/README.md",
        suffix: 133,
    },
    Service {
        name: "GitHub Releases",
        domain: "release-assets.githubusercontent.com",
        path: "/",
        suffix: 133,
    },
    Service {
        name: "X 圖片",
        domain: "pbs.twimg.com",
        path: "/profile_images/1683325380441128960/yRsRRjGO_400x400.jpg",
        suffix: 159,
    },
    Service {
        name: "Twitch 圖片",
        domain: "static-cdn.jtvnw.net",
        path: "/jtv_user_pictures/xarth/404_user_70x70.png",
        suffix: 214,
    },
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Route {
    pub service: usize,
    pub ip: Ipv4Addr,
    pub pop: String,
    pub latency: f64,
    pub jitter: f64,
    pub successes: usize,
}
#[derive(Clone, Debug)]
pub enum Event {
    Progress(f32, String),
    Done(Vec<Route>),
    Written(String),
    Error(String),
}

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(4))
        .connect_timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("FastlyRoute/0.1.0")
        .build()?)
}

pub fn ranges() -> Result<Vec<Ipv4Net>> {
    #[derive(Deserialize)]
    struct List {
        addresses: Vec<String>,
    }
    let list: List = client()?
        .get("https://api.fastly.com/public-ip-list")
        .send()?
        .error_for_status()?
        .json()?;
    let nets: Vec<Ipv4Net> = list
        .addresses
        .iter()
        .map(|s| s.parse())
        .collect::<std::result::Result<_, _>>()?;
    if nets.is_empty() || nets.iter().any(|n| n.prefix_len() < 12) {
        bail!("Fastly IP 清單格式異常");
    }
    Ok(nets)
}

pub fn candidates(nets: &[Ipv4Net], suffix: u8) -> Vec<Ipv4Addr> {
    let mut ips = BTreeSet::new();
    for net in nets {
        let start = u32::from(net.network()) & !255;
        let end = u32::from(net.broadcast());
        for base in (u64::from(start)..=u64::from(end)).step_by(256) {
            let ip = Ipv4Addr::from(base as u32 | u32::from(suffix));
            if net.contains(&ip) {
                ips.insert(ip);
            }
        }
    }
    ips.into_iter().collect()
}

pub fn pop_from_header(value: &str) -> String {
    value
        .split(',')
        .next_back()
        .unwrap_or("")
        .trim()
        .rsplit('-')
        .next()
        .filter(|s| s.len() == 3 && s.bytes().all(|b| b.is_ascii_alphabetic()))
        .unwrap_or("—")
        .to_uppercase()
}

pub fn probe(service: usize, ip: Ipv4Addr) -> Result<(f64, String)> {
    let s = SERVICES.get(service).context("未知服務")?;
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .connect_timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .resolve(s.domain, SocketAddr::from((ip, 443)))
        .user_agent("FastlyRoute/0.1.0")
        .build()?;
    let start = Instant::now();
    let response = client
        .get(format!("https://{}{}", s.domain, s.path))
        .send()?;
    let status = response.status().as_u16();
    // Releases' root has no document. A TLS-verified Fastly 404 proves routing, not download speed.
    if !(response.status().is_success() || service == 1 && status == 404) {
        bail!("HTTP {status}");
    }
    let served = response
        .headers()
        .get("x-served-by")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let pop = pop_from_header(served);
    Ok((start.elapsed().as_secs_f64() * 1000.0, pop))
}

pub fn scan(
    enabled: Vec<usize>,
    cancel: Arc<AtomicBool>,
    emit: impl Fn(Event) + Sync,
) -> Result<()> {
    emit(Event::Progress(
        0.01,
        "正在取得 Fastly 官方 IP 清單…".into(),
    ));
    let nets = ranges().context("無法取得 Fastly 官方 IP 清單，請檢查網路")?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(32).build()?;
    let mut result = Vec::new();
    for (index, &service) in enabled.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            bail!("已取消掃描");
        }
        let s = SERVICES[service];
        emit(Event::Progress(
            index as f32 / enabled.len() as f32,
            format!("{} · 掃描可連線路線", s.name),
        ));
        let ips = candidates(&nets, s.suffix);
        let mut reachable: Vec<_> = pool.install(|| {
            ips.par_iter()
                .filter_map(|&ip| {
                    if cancel.load(Ordering::Relaxed) {
                        return None;
                    }
                    let t = Instant::now();
                    TcpStream::connect_timeout(
                        &SocketAddr::from((ip, 443)),
                        Duration::from_millis(650),
                    )
                    .ok()
                    .map(|_| (ip, t.elapsed().as_secs_f64() * 1000.0))
                })
                .collect()
        });
        reachable.sort_by(|a, b| a.1.total_cmp(&b.1));
        let nearest = reachable.first().map(|r| r.1).unwrap_or(0.0);
        reachable.retain(|r| r.1 <= nearest + 100.0);
        reachable.truncate(48);
        emit(Event::Progress(
            (index as f32 + 0.55) / enabled.len() as f32,
            format!("{} · TLS 驗證與三輪回應測試", s.name),
        ));
        let routes: Vec<Route> = pool.install(|| {
            reachable
                .par_iter()
                .filter_map(|&(ip, _)| {
                    let mut samples = Vec::new();
                    let mut pop = "—".to_string();
                    for _ in 0..3 {
                        if cancel.load(Ordering::Relaxed) {
                            return None;
                        }
                        if let Ok((ms, p)) = probe(service, ip) {
                            samples.push(ms);
                            pop = p;
                        }
                    }
                    if samples.is_empty() {
                        return None;
                    }
                    samples.sort_by(f64::total_cmp);
                    Some(Route {
                        service,
                        ip,
                        pop,
                        latency: samples[samples.len() / 2],
                        jitter: samples[samples.len() - 1] - samples[0],
                        successes: samples.len(),
                    })
                })
                .collect()
        });
        result.extend(routes);
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("已取消掃描");
    }
    result.sort_by(|a, b| {
        a.service
            .cmp(&b.service)
            .then(b.successes.cmp(&a.successes))
            .then(a.latency.total_cmp(&b.latency))
    });
    if result.is_empty() {
        bail!("沒有通過 HTTPS 驗證的路線，請稍後重試");
    }
    emit(Event::Done(result));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn candidates_stay_in_network() {
        let nets = [
            "151.101.0.0/23".parse().unwrap(),
            "151.101.2.128/25".parse().unwrap(),
        ];
        assert_eq!(
            candidates(&nets, 133),
            vec![
                Ipv4Addr::new(151, 101, 0, 133),
                Ipv4Addr::new(151, 101, 1, 133),
                Ipv4Addr::new(151, 101, 2, 133)
            ]
        );
        assert_eq!(candidates(&nets, 12).len(), 2);
    }
    #[test]
    fn uses_last_pop() {
        assert_eq!(pop_from_header("cache-a-nrt, cache-b-hkg"), "HKG");
        assert_eq!(pop_from_header(""), "—");
    }
}
