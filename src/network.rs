use crate::model::{Benchmark, Host, Report, Route, Sample, Service, same_subnet, timestamp};
use anyhow::{Context, Result, bail, ensure};
use ipnet::Ipv4Net;
use rayon::prelude::*;
use reqwest::{Url, blocking::Client};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    io::Read,
    net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
pub const SAMPLE_BYTES: u64 = 8 * 1024 * 1024;
pub const SAMPLE_SECONDS: f64 = 2.0;
#[derive(Clone)]
pub enum Event {
    Progress(f32, String),
    Done(Report),
    Custom(Service),
    Purged,
    Message(String),
    Error(String),
}
#[derive(Clone, Default)]
pub struct ScanOptions {
    pub global: bool,
    pub region: String,
    pub latency_only: bool,
}
pub fn client() -> Result<Client> {
    http(None, Duration::from_secs(12))
}
fn http(resolve: Option<(&str, Ipv4Addr)>, timeout: Duration) -> Result<Client> {
    let mut builder = Client::builder()
        .no_proxy()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("FastlyRoute/", env!("CARGO_PKG_VERSION")));
    if let Some((host, ip)) = resolve {
        builder = builder.resolve(host, SocketAddr::from((ip, 443)));
    }
    Ok(builder.build()?)
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
    ensure!(
        !nets.is_empty() && nets.iter().all(|n| n.prefix_len() >= 12),
        "Fastly IP 清單格式異常"
    );
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
        .rev()
        .find_map(|part| {
            let bits: Vec<_> = part.trim().split('-').collect();
            let last = bits.last()?;
            (bits.len() >= 3
                && bits[0] == "cache"
                && last.len() == 3
                && last.bytes().all(|b| b.is_ascii_alphabetic()))
            .then(|| last.to_uppercase())
        })
        .unwrap_or_else(|| "—".into())
}
pub fn resolve(domain: &str) -> Result<Vec<Ipv4Addr>> {
    Ok((domain, 443)
        .to_socket_addrs()?
        .filter_map(|a| match a.ip() {
            std::net::IpAddr::V4(ip) => Some(ip),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}
fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "已取消操作");
    Ok(())
}
fn head(domain: &str, path: &str, ip: Ipv4Addr, any_status: bool) -> Result<(f64, String)> {
    let c = http(Some((domain, ip)), Duration::from_secs(4))?;
    let start = Instant::now();
    let r = c.head(format!("https://{domain}{path}")).send()?;
    ensure!(
        r.status().is_success() || (any_status && r.status().as_u16() < 500),
        "{domain}: HTTP {}",
        r.status()
    );
    let pop = pop_from_header(
        r.headers()
            .get("x-served-by")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
    );
    Ok((start.elapsed().as_secs_f64() * 1000., pop))
}
pub fn probe(s: &Service, ip: Ipv4Addr) -> Result<(f64, String)> {
    let (ms, mut pop) = head(&s.domain, &s.probe_path, ip, s.custom)?;
    if s.id == "twitch"
        && let Ok((_, p)) = head("k.twitchcdn.net", "/", same_subnet(ip, 167), true)
    {
        pop = p;
    }
    Ok((ms, pop))
}
fn route(s: &Service, ip: Ipv4Addr, baseline: bool) -> Result<Route> {
    let (ms, pop) = probe(s, ip)?;
    Ok(Route {
        service: s.id.clone(),
        ip,
        pop,
        latency: ms,
        jitter: 0.,
        successes: 1,
        baseline,
        samples: vec![],
        smooth: 0,
        median: 0.,
        source: String::new(),
    })
}
pub fn validate_custom(domain: &str, test_url: &str) -> Result<Service> {
    let domain = crate::model::normalize_domain(domain)?;
    let benchmark = if test_url.trim().is_empty() {
        Benchmark::None
    } else {
        Benchmark::Url(crate::model::sample_url(test_url.trim(), &domain)?.to_string())
    };
    let nets = ranges()?;
    let ips: Vec<_> = resolve(&domain)?
        .into_iter()
        .filter(|ip| nets.iter().any(|n| n.contains(ip)))
        .collect();
    ensure!(
        !ips.is_empty(),
        "此網域目前的 DNS IPv4 不在 Fastly 官方網段內"
    );
    let mut counts = std::collections::BTreeMap::new();
    for ip in &ips {
        *counts.entry(ip.octets()[3]).or_insert(0) += 1;
    }
    let suffix = *counts.iter().max_by_key(|(_, n)| *n).unwrap().0;
    let ip = *ips.iter().find(|ip| ip.octets()[3] == suffix).unwrap();
    let s = Service {
        id: format!("custom:{domain}"),
        name: domain.clone(),
        domain: domain.clone(),
        probe_path: "/".into(),
        suffix,
        hosts: vec![Host { domain, suffix }],
        benchmark,
        custom: true,
    };
    probe(&s, ip)?;
    if let Benchmark::Url(url) = &s.benchmark {
        let sample = download_sample(url, ip, &AtomicBool::new(false));
        ensure!(
            sample.error.is_none(),
            "測速檔無法使用：{}",
            sample.error.unwrap_or_default()
        );
    }
    Ok(s)
}
fn bounded_body(mut response: reqwest::blocking::Response, limit: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    response.by_ref().take(limit + 1).read_to_end(&mut out)?;
    ensure!(out.len() as u64 <= limit, "回應超過大小限制");
    Ok(out)
}
pub fn github_release_url() -> Result<String> {
    #[derive(Deserialize)]
    struct Asset {
        size: u64,
        browser_download_url: String,
    }
    #[derive(Deserialize)]
    struct Release {
        assets: Vec<Asset>,
    }
    let c = client()?;
    let mut last = String::new();
    for repo in ["cli/cli", "ollama/ollama"] {
        let result = (|| -> Result<String> {
            let r: Release = c
                .get(format!(
                    "https://api.github.com/repos/{repo}/releases/latest"
                ))
                .send()?
                .error_for_status()?
                .json()?;
            let asset = r
                .assets
                .iter()
                .filter(|a| a.size >= SAMPLE_BYTES)
                .max_by_key(|a| a.size)
                .context("沒有大型 release 檔案")?;
            let mut url = Url::parse(&asset.browser_download_url)?;
            for _ in 0..5 {
                ensure!(
                    url.scheme() == "https"
                        && matches!(
                            url.host_str(),
                            Some("github.com" | "release-assets.githubusercontent.com")
                        ),
                    "測速檔重導向不受支援"
                );
                if url.host_str() == Some("release-assets.githubusercontent.com") {
                    return Ok(url.to_string());
                }
                let response = c.get(url.clone()).send()?;
                ensure!(
                    response.status().is_redirection(),
                    "下載頁沒有提供 release-assets 重導向"
                );
                url = url.join(
                    response
                        .headers()
                        .get("location")
                        .context("缺少 Location")?
                        .to_str()?,
                )?;
            }
            bail!("重導向次數過多")
        })();
        match result {
            Ok(url) => return Ok(url),
            Err(e) => last = e.to_string(),
        }
    }
    bail!("無法取得 GitHub 大檔：{last}")
}
fn twitch_url(routes: &[Route]) -> Result<String> {
    let mut html = None;
    for ip in routes
        .iter()
        .take(3)
        .map(|r| Some(r.ip))
        .chain(std::iter::once(None))
    {
        let c = http(
            ip.map(|ip| ("www.twitch.tv", same_subnet(ip, 214))),
            Duration::from_secs(8),
        )?;
        if let Ok(body) = c
            .get("https://www.twitch.tv/")
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(anyhow::Error::from)
            .and_then(|r| bounded_body(r, 4 * 1024 * 1024))
        {
            html = Some(String::from_utf8_lossy(&body).into_owned());
            break;
        }
    }
    let html = html.context("無法取得 Twitch 首頁")?;
    let re = regex::Regex::new(r#"https://assets\.twitch\.tv/assets/[^"'\s>]+\.js"#)?;
    let mut urls: Vec<_> = re.find_iter(&html).map(|m| m.as_str()).collect();
    urls.sort();
    urls.dedup();
    for key in ["nomodulePolyfills", "core-"] {
        if let Some(url) = urls.iter().find(|u| u.contains(key)) {
            return Ok((*url).into());
        }
    }
    Ok(urls
        .first()
        .context("Twitch 首頁沒有可測速的 JS 檔案")?
        .to_string())
}
pub fn download_sample(url: &str, ip: Ipv4Addr, cancel: &AtomicBool) -> Sample {
    match measure(url, ip, cancel) {
        Ok(s) => s,
        Err(e) => Sample {
            error: Some(e.to_string()),
            ..Default::default()
        },
    }
}
fn measure(url: &str, ip: Ipv4Addr, cancel: &AtomicBool) -> Result<Sample> {
    check_cancel(cancel)?;
    let url = Url::parse(url)?;
    ensure!(
        url.scheme() == "https" && url.port_or_known_default() == Some(443),
        "只允許 HTTPS 測速"
    );
    let host = url.host_str().context("測速網域缺失")?;
    let c = http(Some((host, ip)), Duration::from_secs(5))?;
    let mut response = c
        .get(url.clone())
        .header("Range", format!("bytes=0-{}", SAMPLE_BYTES - 1))
        .header("Accept-Encoding", "identity")
        .send()?
        .error_for_status()?;
    ensure!(
        !response.status().is_redirection(),
        "測速檔不可重導向至其他路線"
    );
    let declared = response.content_length().unwrap_or(0);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    ensure!(
        !content_type.contains("text/html"),
        "測速回應是 HTML，不是大檔"
    );
    let start = Instant::now();
    let mut got = 0u64;
    let mut eof = false;
    let mut buffer = [0; 32 * 1024];
    while got < SAMPLE_BYTES && start.elapsed().as_secs_f64() < SAMPLE_SECONDS {
        check_cancel(cancel)?;
        let limit = buffer.len().min((SAMPLE_BYTES - got) as usize);
        match response.read(&mut buffer[..limit]) {
            Ok(0) => {
                eof = true;
                break;
            }
            Ok(n) => got += n as u64,
            Err(e) => {
                if got > 0 && start.elapsed().as_secs_f64() >= SAMPLE_SECONDS {
                    break;
                }
                return Err(e.into());
            }
        }
    }
    ensure!(got > 0, "沒有收到下載資料");
    ensure!(
        !(eof && got < 256 * 1024 || declared > 0 && declared < 256 * 1024),
        "檔案小於 256 KiB，無法可靠測速"
    );
    let seconds = start.elapsed().as_secs_f64().max(0.000001);
    Ok(Sample {
        mbps: got as f64 * 8. / seconds / 1_000_000.,
        bytes: got,
        seconds,
        error: None,
    })
}
pub fn rank(routes: &mut [Route]) {
    let peak = routes
        .iter()
        .flat_map(|r| &r.samples)
        .map(|s| s.mbps)
        .fold(0.0, f64::max);
    let threshold = (peak * 0.3).max(3.0);
    for r in routes.iter_mut() {
        r.smooth = r
            .samples
            .iter()
            .filter(|s| s.error.is_none() && s.mbps >= threshold)
            .count();
        let mut values: Vec<_> = r.samples.iter().map(|s| s.mbps).collect();
        values.sort_by(f64::total_cmp);
        r.median = if values.is_empty() {
            0.
        } else {
            values[values.len() / 2]
        };
    }
    routes.sort_by(|a, b| {
        b.measured()
            .cmp(&a.measured())
            .then(b.smooth.cmp(&a.smooth))
            .then(b.median.total_cmp(&a.median))
            .then(a.latency.total_cmp(&b.latency))
    });
}
pub fn recommendations(report: &Report) -> Vec<Route> {
    report
        .services
        .iter()
        .filter_map(|s| {
            let rows: Vec<_> = report.routes.iter().filter(|r| r.service == s.id).collect();
            let best = *rows.first()?;
            let baseline = rows.iter().copied().filter(|r| r.baseline).min_by(|a, b| {
                b.smooth
                    .cmp(&a.smooth)
                    .then(a.latency.total_cmp(&b.latency))
            });
            if best.baseline {
                return None;
            }
            match baseline {
                Some(base) if best.measured() => (best.smooth > base.smooth).then(|| best.clone()),
                Some(base) => (best.latency + 10. < base.latency).then(|| best.clone()),
                None => Some(best.clone()),
            }
        })
        .collect()
}
pub fn scan(
    services: Vec<Service>,
    options: ScanOptions,
    cancel: Arc<AtomicBool>,
    emit: impl Fn(Event) + Sync,
) -> Result<()> {
    ensure!(!services.is_empty(), "請至少選擇一個服務");
    check_cancel(&cancel)?;
    emit(Event::Progress(0.01, "取得 Fastly 官方網段…".into()));
    let nets = ranges()?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(32).build()?;
    let mut report = Report {
        services: services.clone(),
        created: timestamp(),
        ..Default::default()
    };
    for (index, s) in services.iter().enumerate() {
        check_cancel(&cancel)?;
        let fraction = |p: f32| (index as f32 + p) / services.len() as f32;
        emit(Event::Progress(
            fraction(0.02),
            format!("{} · 掃描 TCP 路線", s.name),
        ));
        let mut base = resolve(&s.domain).unwrap_or_default();
        // The content and download hosts can have different default DNS routes.
        // Compare the actual download origin as well, not only the small probe file.
        let download_domain = match s.benchmark {
            Benchmark::Github => Some("release-assets.githubusercontent.com"),
            Benchmark::Twitch => Some("assets.twitch.tv"),
            _ => None,
        };
        if let Some(domain) = download_domain {
            base.extend(resolve(domain).unwrap_or_default());
            base.sort();
            base.dedup();
        }
        let mut reachable: Vec<_> = pool.install(|| {
            candidates(&nets, s.suffix)
                .par_iter()
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
                    .map(|_| (ip, t.elapsed().as_secs_f64() * 1000.))
                })
                .collect()
        });
        reachable.sort_by(|a, b| a.1.total_cmp(&b.1));
        if !options.global && options.region.is_empty() {
            let nearest = reachable.first().map(|r| r.1).unwrap_or(0.);
            reachable.retain(|r| r.1 <= nearest + 100.);
        }
        // Always include the actual system-resolved addresses for a fair comparison.
        for ip in &base {
            if !reachable.iter().any(|(i, _)| i == ip) {
                reachable.push((*ip, 0.));
            }
        }
        emit(Event::Progress(
            fraction(0.2),
            format!("{} · 驗證 {} 條 HTTPS 路線", s.name, reachable.len()),
        ));
        let mut routes: Vec<_> = pool.install(|| {
            reachable
                .par_iter()
                .filter_map(|&(ip, _)| {
                    if cancel.load(Ordering::Relaxed) {
                        return None;
                    }
                    route(s, ip, base.contains(&ip)).ok()
                })
                .collect()
        });
        if !options.region.is_empty() {
            routes.retain(|r| r.pop.eq_ignore_ascii_case(&options.region) || r.baseline);
        }
        routes.sort_by(|a, b| a.latency.total_cmp(&b.latency));
        if routes.is_empty() {
            report.notes.push(format!("{}：找不到可用路線", s.name));
            continue;
        }
        if !options.latency_only {
            let mut indices: Vec<usize> = routes
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    options.region.is_empty() || r.pop.eq_ignore_ascii_case(&options.region)
                })
                .take(if options.region.is_empty() { 10 } else { 8 })
                .map(|(i, _)| i)
                .collect();
            for (i, r) in routes.iter().enumerate() {
                if r.baseline && !indices.contains(&i) {
                    indices.push(i);
                }
            }
            let mut alias: Vec<(usize, Ipv4Addr)> =
                indices.iter().map(|&i| (i, routes[i].ip)).collect();
            if s.benchmark == Benchmark::ViaGithub {
                let github = crate::model::builtins().remove(0);
                alias.retain(|&(i, ip)| {
                    probe(&github, same_subnet(ip, 133))
                        .is_ok_and(|(_, pop)| pop != "—" && pop == routes[i].pop)
                });
                for (_, ip) in &mut alias {
                    *ip = same_subnet(*ip, 133);
                }
                if alias.is_empty() {
                    report
                        .notes
                        .push("X 圖片：沒有與 GitHub 相同 POP 的路線，僅依延遲選路".into());
                }
            }
            let static_url = match &s.benchmark {
                Benchmark::Twitch => twitch_url(&routes),
                Benchmark::Url(u) => Ok(u.clone()),
                Benchmark::None => Err(anyhow::anyhow!("未設定測速檔")),
                _ => Ok(String::new()),
            };
            match static_url {
                Err(e) => report.notes.push(format!("{}：{e}；僅延遲", s.name)),
                Ok(static_url) => {
                    for round in 0..3 {
                        check_cancel(&cancel)?;
                        let url = if matches!(s.benchmark, Benchmark::Github | Benchmark::ViaGithub)
                        {
                            match github_release_url() {
                                Ok(u) => u,
                                Err(e) => {
                                    report.notes.push(format!(
                                        "{}：{e}；第 {} 輪中止",
                                        s.name,
                                        round + 1
                                    ));
                                    break;
                                }
                            }
                        } else {
                            static_url.clone()
                        };
                        // Sequential, rotated order prevents candidates competing for bandwidth.
                        let count = alias.len();
                        for offset in 0..count {
                            check_cancel(&cancel)?;
                            let (i, ip) = alias[(offset + round) % count];
                            emit(Event::Progress(
                                fraction(
                                    0.35 + 0.6 * (round * count + offset) as f32
                                        / (3 * count) as f32,
                                ),
                                format!(
                                    "{} · 下載第 {}/3 輪 · {}/{} · {}",
                                    s.name,
                                    round + 1,
                                    offset + 1,
                                    count,
                                    routes[i].ip
                                ),
                            ));
                            routes[i].samples.push(download_sample(&url, ip, &cancel));
                            routes[i].source = match s.benchmark {
                                Benchmark::ViaGithub => "GitHub 同 POP 代理測速",
                                Benchmark::Github => "GitHub Release 大檔",
                                Benchmark::Twitch => "Twitch 網頁程式檔",
                                _ => "自訂測速檔",
                            }
                            .into();
                            if let Ok((ms, pop)) = probe(s, routes[i].ip) {
                                routes[i].jitter =
                                    routes[i].jitter.max((routes[i].latency - ms).abs());
                                routes[i].successes += 1;
                                routes[i].pop = pop;
                            }
                        }
                    }
                }
            }
        }
        rank(&mut routes);
        report.routes.extend(routes);
    }
    check_cancel(&cancel)?;
    ensure!(!report.routes.is_empty(), "沒有通過 HTTPS 驗證的路線");
    emit(Event::Done(report));
    Ok(())
}
pub fn verify_route(
    service: &Service,
    route: &Route,
    nets: &[Ipv4Net],
) -> Result<Vec<crate::hosts::Entry>> {
    ensure!(service.id == route.service, "服務與路線不符");
    ensure!(
        nets.iter().any(|n| n.contains(&route.ip)),
        "IP 已不在 Fastly 網段"
    );
    let (_, pop) = probe(service, route.ip)?;
    ensure!(
        route.pop == "—" || pop == route.pop,
        "節點地區已改變，請重新掃描"
    );
    let mut entries = vec![];
    for h in &service.hosts {
        let ip = same_subnet(route.ip, h.suffix);
        ensure!(
            nets.iter().any(|n| n.contains(&ip)),
            "服務 IP 不在 Fastly 網段"
        );
        head(
            &h.domain,
            if h.domain == service.domain {
                &service.probe_path
            } else {
                "/"
            },
            ip,
            true,
        )?;
        entries.push(crate::hosts::Entry {
            domain: h.domain.clone(),
            ip,
        });
    }
    Ok(entries)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Full network scan, up to several hundred MiB"]
    fn live_full_scan() {
        let output = std::sync::Mutex::new(None);
        scan(
            vec![crate::model::builtins().remove(0)],
            ScanOptions::default(),
            Arc::new(AtomicBool::new(false)),
            |event| match event {
                Event::Progress(_, text) => println!("{text}"),
                Event::Done(report) => {
                    *output.lock().unwrap() = Some(report);
                }
                _ => {}
            },
        )
        .unwrap();
        let report = output.into_inner().unwrap().unwrap();
        assert!(report.routes.iter().any(Route::measured));
        assert!(report.routes.iter().any(|r| r.baseline));
        assert!(report.routes.iter().all(|r| r.samples.len() <= 3));
        println!(
            "{} routes, {} measured, {} recommendations",
            report.routes.len(),
            report.routes.iter().filter(|r| r.measured()).count(),
            recommendations(&report).len()
        );
    }
    #[test]
    #[ignore = "Live network test, downloads at most 8 MiB per source"]
    fn live_network_downloads_and_custom() {
        let url = github_release_url().expect("GitHub signed release URL");
        let ip = "151.101.0.133".parse().unwrap();
        let sample = download_sample(&url, ip, &AtomicBool::new(false));
        println!("GitHub sample: {sample:?}");
        assert!(sample.error.is_none(), "{sample:?}");
        assert!(sample.bytes > 0 && sample.bytes <= SAMPLE_BYTES);
        let custom = validate_custom("files.pythonhosted.org", "").expect("Fastly custom domain");
        assert_eq!(custom.hosts[0].domain, "files.pythonhosted.org");
        assert!(validate_custom("example.com", "").is_err());
        let url = twitch_url(&[]).expect("Twitch dynamic JS URL");
        let ips = resolve("assets.twitch.tv").unwrap();
        let sample = download_sample(&url, ips[0], &AtomicBool::new(false));
        println!("Twitch sample: {sample:?}");
        assert!(sample.error.is_none(), "{sample:?}");
    }
    #[test]
    fn candidates_stay_in_network() {
        let nets = [
            "151.101.0.0/23".parse().unwrap(),
            "151.101.2.128/25".parse().unwrap(),
        ];
        assert_eq!(candidates(&nets, 133).len(), 3);
        assert_eq!(candidates(&nets, 12).len(), 2);
    }
    #[test]
    fn finds_last_valid_pop() {
        assert_eq!(
            pop_from_header("cache-nrt-aaa-NRT, cache-sin-sss-SIN, cache-tw-ZZZ1"),
            "SIN"
        );
        assert_eq!(pop_from_header("unknown"), "—");
    }
    fn r(ip: u8, speeds: &[f64]) -> Route {
        Route {
            service: "github".into(),
            ip: Ipv4Addr::new(151, 101, ip, 133),
            pop: "NRT".into(),
            latency: 10.,
            jitter: 0.,
            successes: 3,
            baseline: false,
            samples: speeds
                .iter()
                .map(|&mbps| Sample {
                    mbps,
                    bytes: 1024 * 1024,
                    seconds: 1.,
                    error: None,
                })
                .collect(),
            smooth: 0,
            median: 0.,
            source: String::new(),
        }
    }
    #[test]
    fn stability_beats_one_fast_burst() {
        let mut rows = vec![r(1, &[100., 0.1, 0.1]), r(2, &[40., 40., 40.]), r(3, &[])];
        rank(&mut rows);
        assert_eq!(rows[0].ip.octets()[2], 2);
        assert_eq!(rows[0].smooth, 3);
        assert_eq!(rows[2].samples.len(), 0);
    }
    #[test]
    fn equal_smoothness_keeps_system() {
        let mut a = r(1, &[10., 10., 10.]);
        a.baseline = true;
        let mut rows = vec![r(2, &[12., 12., 12.]), a];
        rank(&mut rows);
        let report = Report {
            routes: rows,
            services: vec![crate::model::builtins().remove(0)],
            ..Default::default()
        };
        assert!(recommendations(&report).is_empty());
    }
    #[test]
    fn cancelled_scan_does_not_access_network() {
        let cancel = Arc::new(AtomicBool::new(true));
        assert!(
            scan(
                crate::model::builtins(),
                ScanOptions::default(),
                cancel,
                |_| panic!("no event expected")
            )
            .is_err()
        );
    }
}
