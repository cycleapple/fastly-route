#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod hosts;
mod model;
mod network;
mod platform;
mod storage;
mod updater;
use eframe::egui::{self, Color32, RichText, Vec2};
use model::{Report, Route, Settings};
use network::Event;
use std::{
    collections::BTreeMap,
    net::Ipv4Addr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};
const INK: Color32 = Color32::from_rgb(28, 43, 42);
const MUTED: Color32 = Color32::from_rgb(116, 129, 126);
const GREEN: Color32 = Color32::from_rgb(29, 112, 87);
const LINE: Color32 = Color32::from_rgb(224, 231, 226);
#[derive(Clone)]
enum Confirm {
    Apply(Vec<Route>),
    Restore,
    Purge,
    Update,
}
struct App {
    settings: Settings,
    report: Report,
    selected: BTreeMap<String, Ipv4Addr>,
    job: Option<mpsc::Receiver<Event>>,
    cancel: Arc<AtomicBool>,
    progress: f32,
    message: String,
    error: bool,
    status: String,
    tab: usize,
    confirm: Option<Confirm>,
    filter: String,
    custom_domain: String,
    custom_url: String,
    after_scan: bool,
    job_cancellable: bool,
    update_job: Option<mpsc::Receiver<updater::UpdateEvent>>,
    update_cancel: Arc<AtomicBool>,
    available: Option<updater::Available>,
    update_message: String,
    installed: bool,
    installing: bool,
}
fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::WHITE)
        .stroke(egui::Stroke::new(1.0_f32, LINE))
        .corner_radius(16)
        .inner_margin(20)
}
fn small(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(RichText::new(text).size(13.).color(MUTED));
}
fn primary(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(text).color(Color32::WHITE).size(15.))
            .fill(GREEN)
            .corner_radius(9)
            .min_size(Vec2::new(136., 40.)),
    )
    .clicked()
}
impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut fonts = egui::FontDefinitions::default();
        for path in [
            "C:/Windows/Fonts/msjh.ttc",
            "C:/Windows/Fonts/msyh.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/System/Library/Fonts/PingFang.ttc",
        ] {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert("cjk".into(), egui::FontData::from_owned(bytes).into());
                fonts
                    .families
                    .get_mut(&egui::FontFamily::Proportional)
                    .unwrap()
                    .push("cjk".into());
                break;
            }
        }
        cc.egui_ctx.set_fonts(fonts);
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals = egui::Visuals::light();
        style.visuals.override_text_color = Some(INK);
        style.visuals.panel_fill = Color32::from_rgb(247, 249, 246);
        style.visuals.extreme_bg_color = Color32::from_rgb(239, 244, 240);
        style.visuals.selection.bg_fill = Color32::from_rgb(215, 235, 222);
        style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, GREEN);
        style.spacing.item_spacing = Vec2::new(12., 10.);
        style.spacing.button_padding = Vec2::new(12., 8.);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(15.));
        cc.egui_ctx.set_style(style);
        let (mut error, mut message) = (false, "準備好，找一條更順的路。".into());
        let settings = storage::load_settings().unwrap_or_else(|e| {
            error = true;
            message = format!("無法載入設定：{e}");
            Settings::default()
        });
        let report = storage::load_report(&settings).unwrap_or_else(|e| {
            error = true;
            message = format!("無法載入快取：{e}");
            Report::default()
        });
        let selected = network::recommendations(&report)
            .iter()
            .map(|r| (r.service.clone(), r.ip))
            .collect();
        let mut app = Self {
            settings,
            report,
            selected,
            job: None,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: 0.,
            message,
            error,
            status: hosts::status(),
            tab: 0,
            confirm: None,
            filter: String::new(),
            custom_domain: String::new(),
            custom_url: String::new(),
            after_scan: false,
            job_cancellable: false,
            update_job: None,
            update_cancel: Arc::new(AtomicBool::new(false)),
            available: None,
            update_message: "尚未檢查更新".into(),
            installed: false,
            installing: false,
        };
        if app.settings.auto_check {
            app.check_update(&cc.egui_ctx);
        }
        app
    }
    fn busy(&self) -> bool {
        self.job.is_some() || self.installing
    }
    fn save_settings(&mut self) {
        if let Err(e) = storage::save_settings(&self.settings) {
            self.message = format!("設定未能儲存：{e}");
            self.error = true;
        }
    }
    fn work(
        &mut self,
        ctx: &egui::Context,
        cancellable: bool,
        work: impl FnOnce(mpsc::Sender<Event>, Arc<AtomicBool>, egui::Context) -> anyhow::Result<()>
        + Send
        + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        self.job = Some(rx);
        self.error = false;
        self.progress = 0.;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.job_cancellable = cancellable;
        let cancel = self.cancel.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                work(tx.clone(), cancel, ctx.clone())
            })) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    let _ = tx.send(Event::Error(format!("{e:#}")));
                }
                Err(_) => {
                    let _ = tx.send(Event::Error("背景工作發生錯誤，請重新嘗試".into()));
                }
            }
            ctx.request_repaint();
        });
    }
    fn scan(&mut self, ctx: &egui::Context, latency_only: bool, after: bool) {
        let services = self
            .settings
            .services()
            .into_iter()
            .filter(|s| self.settings.enabled.contains(&s.id))
            .collect();
        let options = network::ScanOptions {
            global: self.settings.global,
            region: self.settings.region.trim().to_ascii_uppercase(),
            latency_only,
        };
        if !options.region.is_empty()
            && (options.region.len() != 3
                || !options.region.bytes().all(|b| b.is_ascii_alphabetic()))
        {
            self.message = "地區請輸入三碼，如 ICN、NRT、HKG".into();
            self.error = true;
            return;
        }
        self.after_scan = after;
        self.message = "正在開始掃描…".into();
        self.work(ctx, true, move |tx, cancel, ctx| {
            network::scan(services, options, cancel, |event| {
                let _ = tx.send(event);
                ctx.request_repaint();
            })
        });
    }
    fn chosen(&self) -> Vec<Route> {
        self.report
            .routes
            .iter()
            .filter(|r| {
                self.settings.enabled.contains(&r.service)
                    && self.selected.get(&r.service) == Some(&r.ip)
            })
            .cloned()
            .collect()
    }
    fn poll(&mut self) {
        let events: Vec<_> = self
            .job
            .as_ref()
            .map(|r| r.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            match event {
                Event::Progress(p, s) => {
                    self.progress = p;
                    self.message = s;
                }
                Event::Done(report) => {
                    self.selected = network::recommendations(&report)
                        .iter()
                        .map(|r| (r.service.clone(), r.ip))
                        .collect();
                    self.message = format!(
                        "完成：{} 條路線；{} 個服務建議變更。",
                        report.routes.len(),
                        self.selected.len()
                    );
                    if self.selected.is_empty() {
                        self.message.push_str("目前路線已相當順暢，可手動選擇。");
                    }
                    self.report = report;
                    self.job = None;
                    self.progress = 1.;
                    if let Err(e) = storage::save_report(&self.report) {
                        self.message.push_str(&format!(" 快取儲存失敗：{e}"));
                    }
                    if self.after_scan && !self.chosen().is_empty() {
                        self.confirm = Some(Confirm::Apply(self.chosen()));
                    }
                    self.after_scan = false;
                }
                Event::Custom(s) => {
                    self.settings.enabled.insert(s.id.clone());
                    self.settings.custom.push(s);
                    self.save_settings();
                    self.custom_domain.clear();
                    self.custom_url.clear();
                    self.message = "自訂網域已通過 Fastly 與 HTTPS 驗證並加入。".into();
                    self.job = None;
                }
                Event::Message(s) => {
                    self.message = s;
                    self.job = None;
                    self.status = hosts::status();
                }
                Event::Purged => {
                    self.settings = Settings::default();
                    self.report = Report::default();
                    self.selected.clear();
                    self.job = None;
                    self.message = "已還原 hosts 並清除本工具設定與快取；備份檔仍保留。".into();
                    self.status = hosts::status();
                }
                Event::Error(s) => {
                    self.message = s;
                    self.error = true;
                    self.job = None;
                    self.after_scan = false;
                    self.status = hosts::status();
                }
            }
        }
        let events: Vec<_> = self
            .update_job
            .as_ref()
            .map(|r| r.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            match event {
                updater::UpdateEvent::Checked(update) => {
                    self.update_message = update
                        .as_ref()
                        .map(|u| format!("有新版本 v{}", u.version))
                        .unwrap_or_else(|| "目前已是此更新通道的最新版本".into());
                    self.available = update;
                    self.update_job = None;
                }
                updater::UpdateEvent::Progress(s) => self.update_message = s,
                updater::UpdateEvent::Installed(backup) => {
                    self.installed = true;
                    self.installing = false;
                    self.update_job = None;
                    self.update_message =
                        format!("更新已安裝，重新啟動後生效。舊版備份：{}", backup.display());
                }
                updater::UpdateEvent::Error(s) => {
                    self.update_message = format!("更新失敗：{s}");
                    self.installing = false;
                    self.update_job = None;
                }
            }
        }
    }
    fn apply(&mut self, ctx: &egui::Context, routes: Vec<Route>, purge: bool) {
        let services = self.settings.services();
        self.message = "正在重新驗證路線並更新 hosts…".into();
        self.work(ctx, false, move |tx, _, _| {
            let mut entries = vec![];
            if !routes.is_empty() {
                let nets = network::ranges()?;
                for r in &routes {
                    let s = services
                        .iter()
                        .find(|s| s.id == r.service)
                        .ok_or_else(|| anyhow::anyhow!("服務已移除"))?;
                    entries.extend(network::verify_route(s, r, &nets)?);
                }
            }
            let backup = hosts::write_at(&hosts::path(), &entries)?;
            let dns = platform::flush_dns();
            if purge {
                storage::purge()?;
                let _ = tx.send(Event::Purged);
            } else {
                let _ = tx.send(Event::Message(format!(
                    "{}；{dns}。備份：{}",
                    if entries.is_empty() {
                        "已還原系統路線"
                    } else {
                        "已套用所選路線"
                    },
                    backup.display()
                )));
            }
            Ok(())
        });
    }
    fn add_custom(&mut self, ctx: &egui::Context) {
        let domain = self.custom_domain.clone();
        let url = self.custom_url.clone();
        let existing = self.settings.services();
        self.message = "正在驗證自訂網域…".into();
        self.work(ctx, false, move |tx, _, _| {
            let normalized = model::normalize_domain(&domain)?;
            anyhow::ensure!(
                !existing
                    .iter()
                    .any(|s| s.hosts.iter().any(|h| h.domain == normalized)),
                "此網域已在服務清單內"
            );
            let s = network::validate_custom(&domain, &url)?;
            let _ = tx.send(Event::Custom(s));
            Ok(())
        });
    }
    fn refresh(&mut self, ctx: &egui::Context) {
        let services = self
            .settings
            .services()
            .into_iter()
            .filter(|s| self.settings.enabled.contains(&s.id))
            .collect::<Vec<_>>();
        self.message = "檢查目前路線…".into();
        self.work(ctx, true, move |tx, cancel, _| {
            let mut lines = vec![];
            for s in services {
                if cancel.load(Ordering::Relaxed) {
                    anyhow::bail!("已取消");
                }
                match network::resolve(&s.domain).and_then(|ips| {
                    let ip = ips.first().ok_or_else(|| anyhow::anyhow!("沒有 IPv4"))?;
                    network::probe(&s, *ip)
                        .map(|(ms, pop)| format!("{}：{pop} / {ip} / {ms:.0} ms", s.name))
                }) {
                    Ok(line) => lines.push(line),
                    Err(e) => lines.push(format!("{}：{e}", s.name)),
                }
            }
            let _ = tx.send(Event::Message(lines.join("\n")));
            Ok(())
        });
    }
    fn check_update(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        self.update_job = Some(rx);
        self.update_message = "正在檢查 GitHub Releases…".into();
        let pre = self.settings.prerelease;
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let event = match updater::check(env!("CARGO_PKG_VERSION"), pre) {
                Ok(u) => updater::UpdateEvent::Checked(u),
                Err(e) => updater::UpdateEvent::Error(format!("{e:#}")),
            };
            let _ = tx.send(event);
            ctx.request_repaint();
        });
    }
    fn install(&mut self, ctx: &egui::Context) {
        let Some(update) = self.available.clone() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.update_job = Some(rx);
        self.installing = true;
        self.update_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.update_cancel.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = updater::install(&update, &cancel, |event| {
                let _ = tx.send(event);
                ctx.request_repaint();
            });
            let _ = tx.send(match result {
                Ok(p) => updater::UpdateEvent::Installed(p),
                Err(e) => updater::UpdateEvent::Error(format!("{e:#}")),
            });
            ctx.request_repaint();
        });
    }
    fn overview(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.busy();
        let services = self.settings.services();
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            small(ui, "SERVICES / BENCHMARK");
            ui.label(
                RichText::new("選擇服務，找到穩定的下載路線")
                    .size(19.)
                    .strong(),
            );
            let mut changed = false;
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for s in &services {
                        let mut checked = self.settings.enabled.contains(&s.id);
                        if ui.checkbox(&mut checked, &s.name).changed() {
                            if checked {
                                self.settings.enabled.insert(s.id.clone());
                            } else {
                                self.settings.enabled.remove(&s.id);
                            }
                            changed = true;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut self.settings.global, "全球掃描").changed();
                    small(ui, "指定地區");
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut self.settings.region)
                                .desired_width(70.)
                                .hint_text("ICN"),
                        )
                        .changed();
                    small(ui, "留空：附近節點");
                });
            });
            if changed {
                self.save_settings();
            }
            ui.horizontal_wrapped(|ui| {
                if primary(
                    ui,
                    "掃描＋大檔測速",
                    !busy && !self.settings.enabled.is_empty(),
                ) {
                    self.scan(ctx, false, false);
                }
                if ui
                    .add_enabled(
                        !busy && !self.settings.enabled.is_empty(),
                        egui::Button::new("測速後準備套用"),
                    )
                    .clicked()
                {
                    self.scan(ctx, false, true);
                }
                if ui
                    .add_enabled(
                        !busy && !self.settings.enabled.is_empty(),
                        egui::Button::new("只看節點延遲"),
                    )
                    .clicked()
                {
                    self.scan(ctx, true, false);
                }
            });
            small(
                ui,
                "三輪交錯下載 · 每次最多 8 MiB / 約 2 秒 · 可能使用數百 MiB 流量",
            );
            if self.job.is_some() {
                ui.add(
                    egui::ProgressBar::new(self.progress)
                        .desired_width(ui.available_width())
                        .fill(GREEN)
                        .animate(true),
                );
                if self.job_cancellable && ui.button("取消操作").clicked() {
                    self.cancel.store(true, Ordering::Relaxed);
                }
            }
        });
        ui.add_space(10.);
        ui.horizontal(|ui| {
            ui.label(RichText::new("路線比較").size(20.).strong());
            small(ui, format!("{} ROUTES", self.report.routes.len()));
            if self.report.created > 0 {
                small(
                    ui,
                    format!(
                        "{} 分鐘前的掃描",
                        model::timestamp().saturating_sub(self.report.created) / 60
                    ),
                );
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("篩選地區、IP 或服務")
                    .desired_width(190.),
            );
            if ui
                .add_enabled(!busy, egui::Button::new("選取此地區最佳路線"))
                .clicked()
            {
                let region = self.filter.trim().to_ascii_uppercase();
                for s in &services {
                    if let Some(r) = self
                        .report
                        .routes
                        .iter()
                        .find(|r| r.service == s.id && r.pop == region)
                    {
                        self.selected.insert(s.id.clone(), r.ip);
                    }
                }
            }
            if ui
                .add_enabled(!busy, egui::Button::new("回到建議選擇"))
                .clicked()
            {
                self.selected = network::recommendations(&self.report)
                    .iter()
                    .map(|r| (r.service.clone(), r.ip))
                    .collect();
            }
        });
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if self.report.routes.is_empty() {
                ui.add_space(20.);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("○  ───  ○  ───  ○").size(28.).color(GREEN));
                    ui.label("真實下載，找到順暢的路線。");
                    small(ui, "開始測速後，比較 Mbps、順暢次數與目前的系統路線。");
                });
                ui.add_space(20.);
            } else {
                egui::ScrollArea::both()
                    .max_height(205.)
                    .id_salt("routes")
                    .show(ui, |ui| {
                        egui::Grid::new("table")
                            .num_columns(7)
                            .spacing([16., 8.])
                            .striped(true)
                            .show(ui, |ui| {
                                for h in [
                                    "服務 / 選取",
                                    "地區",
                                    "IP 位址",
                                    "延遲",
                                    "Mbps 中位數",
                                    "順暢",
                                    "來源",
                                ] {
                                    small(ui, h);
                                }
                                ui.end_row();
                                for r in &self.report.routes {
                                    let Some(s) = services.iter().find(|s| s.id == r.service)
                                    else {
                                        continue;
                                    };
                                    if !self.settings.enabled.contains(&s.id) {
                                        continue;
                                    }
                                    let filter = self.filter.to_lowercase();
                                    if !filter.is_empty()
                                        && !format!("{} {} {}", s.name, r.pop, r.ip)
                                            .to_lowercase()
                                            .contains(&filter)
                                    {
                                        continue;
                                    }
                                    let selected = self.selected.get(&r.service) == Some(&r.ip);
                                    if ui
                                        .add_enabled(
                                            !busy,
                                            egui::Button::new(&s.name).selected(selected),
                                        )
                                        .clicked()
                                    {
                                        if selected {
                                            self.selected.remove(&r.service);
                                        } else {
                                            self.selected.insert(r.service.clone(), r.ip);
                                        }
                                    }
                                    ui.label(&r.pop);
                                    ui.monospace(r.ip.to_string());
                                    ui.label(format!("{:.0} ms", r.latency));
                                    ui.label(if r.measured() {
                                        format!("{:.1}", r.median)
                                    } else {
                                        "未測速".into()
                                    })
                                    .on_hover_text(
                                        r.samples
                                            .iter()
                                            .enumerate()
                                            .map(|(i, s)| {
                                                format!(
                                                    "第 {} 輪：{:.2} Mbps / {} KiB / {:.2}s {}",
                                                    i + 1,
                                                    s.mbps,
                                                    s.bytes / 1024,
                                                    s.seconds,
                                                    s.error.as_deref().unwrap_or("")
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                            .join("\n"),
                                    );
                                    ui.colored_label(
                                        GREEN,
                                        if r.measured() {
                                            format!("{} / 3", r.smooth)
                                        } else {
                                            "—".into()
                                        },
                                    );
                                    ui.label(if r.baseline {
                                        "目前路線"
                                    } else if s.id == "x" && r.measured() {
                                        "同 POP 代理"
                                    } else {
                                        "候選路線"
                                    })
                                    .on_hover_text(&r.source);
                                    ui.end_row();
                                }
                            });
                    });
            }
        });
        ui.horizontal_wrapped(|ui| {
            if primary(ui, "套用所選路線", !busy && !self.chosen().is_empty()) {
                self.confirm = Some(Confirm::Apply(self.chosen()));
            }
            if ui
                .add_enabled(!busy, egui::Button::new("還原系統路線"))
                .clicked()
            {
                self.confirm = Some(Confirm::Restore);
            }
            if ui
                .add_enabled(!busy, egui::Button::new("檢查目前路線"))
                .clicked()
            {
                self.refresh(ctx);
            }
        });
        if !self.report.notes.is_empty() {
            egui::CollapsingHeader::new("測速備註").show(ui, |ui| {
                for note in &self.report.notes {
                    small(ui, note);
                }
            });
        }
    }
    fn services_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.busy();
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.heading("自訂 Fastly 網域");
            small(ui, "驗證官方網段、偵測 IP 尾碼、確認 TLS 憑證後才會加入。");
            ui.add_enabled_ui(!busy, |ui| {
                ui.label("網域");
                ui.add(
                    egui::TextEdit::singleline(&mut self.custom_domain)
                        .hint_text("files.pythonhosted.org")
                        .desired_width(480.),
                );
                ui.label("大檔測速網址（選填）");
                ui.add(
                    egui::TextEdit::singleline(&mut self.custom_url)
                        .hint_text("https://同一網域/至少256KiB的檔案")
                        .desired_width(680.),
                );
            });
            if primary(
                ui,
                "驗證並新增",
                !busy && !self.custom_domain.trim().is_empty(),
            ) {
                self.add_custom(ctx);
            }
            small(ui, "未填測速檔的網域依延遲選路，介面會標示未測速。");
        });
        ui.add_space(12.);
        let mut remove = None;
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.heading("服務清單");
            for s in self.settings.services() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&s.name).strong());
                    small(ui, format!("尾碼 .{}", s.suffix));
                    if s.custom && ui.add_enabled(!busy, egui::Button::new("移除")).clicked() {
                        remove = Some(s.id.clone());
                    }
                });
                small(
                    ui,
                    s.hosts
                        .iter()
                        .map(|h| h.domain.as_str())
                        .collect::<Vec<_>>()
                        .join(" · "),
                );
                if let model::Benchmark::Url(url) = &s.benchmark {
                    small(ui, url);
                }
                ui.separator();
            }
        });
        if let Some(id) = remove {
            self.settings.custom.retain(|s| s.id != id);
            self.settings.enabled.remove(&id);
            self.report.routes.retain(|r| r.service != id);
            self.selected.remove(&id);
            self.save_settings();
            self.message = "已移除服務；已套用的 hosts 會在下次套用或還原時移除。".into();
        }
    }
    fn settings_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.busy();
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.heading("程式更新");
            small(
                ui,
                format!("目前版本 v{} · Windows x64", env!("CARGO_PKG_VERSION")),
            );
            let mut changed = ui
                .checkbox(&mut self.settings.auto_check, "啟動時自動檢查更新")
                .changed();
            changed |= ui
                .checkbox(&mut self.settings.prerelease, "包含預覽版")
                .changed();
            if changed {
                self.save_settings();
            }
            ui.horizontal_wrapped(|ui| {
                if primary(
                    ui,
                    "檢查新版本",
                    self.update_job.is_none() && !self.installed,
                ) {
                    self.check_update(ctx);
                }
                if self.available.is_some()
                    && ui
                        .add_enabled(
                            !busy && self.update_job.is_none() && !self.installed,
                            egui::Button::new("下載並安裝更新"),
                        )
                        .clicked()
                {
                    self.confirm = Some(Confirm::Update);
                }
                if self.installing && ui.button("取消下載").clicked() {
                    self.update_cancel.store(true, Ordering::Relaxed);
                }
                if self.installed && primary(ui, "重新啟動", !busy) {
                    match platform::restart() {
                        Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                        Err(e) => self.update_message = e.to_string(),
                    }
                }
            });
            ui.label(&self.update_message);
            small(
                ui,
                "從本專案 GitHub Releases 下載，核對 SHA-256 與 GUI 執行檔格式後替換，並保留舊版。",
            );
            small(
                ui,
                "私人 repo 使用 GH_TOKEN / GITHUB_TOKEN 或本機 gh auth 登入；權杖不會儲存至設定。",
            );
        });
        ui.add_space(12.);
        card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.heading("系統與資料");
            small(ui, format!("hosts：{}", hosts::path().display()));
            if let Ok(dir) = storage::directory() {
                small(ui, format!("設定與掃描快取：{}", dir.display()));
            }
            small(
                ui,
                "啟動、DNS 清除、GitHub 登入查詢及重新啟動皆隱藏主控台視窗。",
            );
            if ui
                .add_enabled(!busy, egui::Button::new("以管理員身分重新啟動"))
                .clicked()
            {
                match platform::elevate() {
                    Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(e) => {
                        self.error = true;
                        self.message = e.to_string();
                    }
                }
            }
            if ui
                .add_enabled(!busy, egui::Button::new("還原並清除設定與快取"))
                .clicked()
            {
                self.confirm = Some(Confirm::Purge);
            }
            small(ui, "清除會保留 hosts 備份及舊版程式備份，以便手動復原。");
        });
    }
    fn help_ui(&self, ui: &mut egui::Ui) {
        card().show(ui,|ui|{ui.set_width(ui.available_width());for(title,body)in[
        ("01  選擇服務與地區","GitHub、X、Twitch 與自訂服務各自驗證。全球掃描列出所有可連線節點；\n指定三碼地區會完整搜尋該地區，系統目前路線仍會加入比較。"),
        ("02  交錯大檔測速","附近最快的 10 條候選與系統路線交錯測 3 輪，每輪最多 8 MiB / 約 2 秒。\n順暢 = 本次峰值的 30% 以上，且至少 3 Mbps；以順暢次數、速度中位數、延遲排序。\n測速流量隨服務與路線數增加，可能達數百 MiB。逾時上限為每次請求 5 秒。"),
        ("03  選擇與套用","系統路線同樣順暢時不建議變更；也能手動選擇或依地區篩選。\n快取僅供選擇，套用前重新驗證官方網段、TLS 與機房。變更前備份 hosts。"),
        ("服務限制","X 圖片太小，借用同 /24 且相同 POP 的 GitHub 測速，表格明確標示代理值。\nTwitch 使用網頁 JS 測速，支援網頁、圖片與 API，不涵蓋直播影片或聊天室。\n自訂網域必須目前由 Fastly 提供 IPv4。沒有可用測速檔時只依延遲。"),
        ("更新與隱私","啟動時可自動檢查更新，發現新版後由你按下載安裝，再重新啟動生效。\n無遙測。掃描採直接連線，不使用 HTTP 代理；VPN 仍可能影響路線。")
    ]{ui.label(RichText::new(title).strong().size(17.));small(ui,body);ui.add_space(12.);}ui.hyperlink_to("原始碼與版本下載","https://github.com/cycleapple/fastly-route");});
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        if ctx.input(|i| i.viewport().close_requested())
            && (self.installing || self.job.is_some() && !self.job_cancellable)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.message = "正在完成寫入或驗證，完成後即可關閉。".into();
        }
        egui::SidePanel::left("nav")
            .exact_width(196.)
            .resizable(false)
            .frame(egui::Frame::new().fill(Color32::WHITE).inner_margin(22))
            .show(ctx, |ui| {
                ui.add_space(14.);
                ui.label(RichText::new("f / ").size(37.).strong().color(GREEN));
                ui.label(RichText::new("Fastly Route").size(21.).strong());
                small(ui, "A clearer connection.");
                ui.add_space(30.);
                for (i, title) in [
                    "01   路線與測速",
                    "02   自訂網域",
                    "03   設定與更新",
                    "04   使用說明",
                ]
                .iter()
                .enumerate()
                {
                    if ui
                        .add_sized(
                            [152., 42.],
                            egui::Button::new(*title)
                                .selected(self.tab == i)
                                .corner_radius(8),
                        )
                        .clicked()
                    {
                        self.tab = i;
                    }
                }
                if self.available.is_some() && !self.installed {
                    ui.add_space(16.);
                    ui.colored_label(GREEN, "● 有新版本可用");
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    small(ui, format!("v{} / RUST NATIVE", env!("CARGO_PKG_VERSION")));
                    small(ui, "為每一次連線，\n留一條順暢的路。");
                });
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(247, 249, 246))
                    .inner_margin(28),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        small(ui, "NETWORK / ROUTE OPTIMIZER");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.colored_label(GREEN, &self.status);
                        });
                    });
                    ui.label(
                        RichText::new(
                            [
                                "更好的下載，從這裡開始。",
                                "讓更多服務，走上好路線。",
                                "保持更新，保持順暢。",
                                "每一步，都清楚掌握。",
                            ][self.tab],
                        )
                        .size(27.)
                        .strong(),
                    );
                    ui.add_space(12.);
                    match self.tab {
                        0 => self.overview(ui, ctx),
                        1 => self.services_ui(ui, ctx),
                        2 => self.settings_ui(ui, ctx),
                        _ => self.help_ui(ui),
                    }
                    ui.add_space(12.);
                    ui.label(RichText::new(&self.message).size(13.).color(if self.error {
                        Color32::from_rgb(174, 65, 54)
                    } else {
                        MUTED
                    }));
                });
            });
        if let Some(action) = self.confirm.clone() {
            egui::Modal::new(egui::Id::new("confirm")).show(ctx,|ui|{
            ui.heading(match action{Confirm::Apply(_)=>"確認套用路線",Confirm::Restore=>"還原系統路線",Confirm::Purge=>"還原並清除資料",Confirm::Update=>"下載並安裝更新"});
            match &action{
                Confirm::Apply(routes)=>{small(ui,"會重新驗證以下路線，備份後更新 hosts；需要管理員權限。");for r in routes{if let Some(s)=self.settings.services().iter().find(|s|s.id==r.service){for h in &s.hosts{ui.monospace(format!("{}  {}",model::same_subnet(r.ip,h.suffix),h.domain));}}}},
                Confirm::Restore=>{ui.label("只移除本工具的 hosts 區塊，其他內容與備份保留。");},
                Confirm::Purge=>{ui.label("還原 hosts，刪除自訂服務、設定與掃描快取。\nhosts 備份與舊版 exe 備份會保留。");},
                Confirm::Update=>{ui.label("下載新版並核對 SHA-256，備份與替換目前執行檔。\n重啟後生效；不會修改 hosts 或清除設定。");},
            }
            ui.horizontal(|ui|{if primary(ui,"確認",!self.busy()){self.confirm=None;match action{Confirm::Apply(routes)=>self.apply(ctx,routes,false),Confirm::Restore=>self.apply(ctx,vec![],false),Confirm::Purge=>self.apply(ctx,vec![],true),Confirm::Update=>self.install(ctx)}}
if ui.button("取消").clicked(){self.confirm=None;}});
        });
        }
    }
}
fn main() -> eframe::Result<()> {
    eframe::run_native(
        "Fastly Route",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1180., 860.])
                .with_min_inner_size([1000., 760.]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
