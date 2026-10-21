#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod hosts;
mod network;
use eframe::egui::{self, Color32, RichText, Vec2};
use network::{Event, Route, SERVICES};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
const INK: Color32 = Color32::from_rgb(28, 43, 42);
const MUTED: Color32 = Color32::from_rgb(116, 129, 126);
const GREEN: Color32 = Color32::from_rgb(29, 112, 87);
const LINE: Color32 = Color32::from_rgb(224, 231, 226);
struct App {
    enabled: [bool; 4],
    routes: Vec<Route>,
    selected: [Option<usize>; 4],
    receiver: Option<mpsc::Receiver<Event>>,
    cancel: Arc<AtomicBool>,
    progress: f32,
    message: String,
    error: bool,
    status: String,
    tab: usize,
    confirm: Option<bool>,
    filter: String,
    writing: bool,
}
impl Default for App {
    fn default() -> Self {
        Self {
            enabled: [true, true, true, false],
            routes: vec![],
            selected: [None; 4],
            receiver: None,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: 0.,
            message: "準備好，找一條更順的路。".into(),
            error: false,
            status: hosts::status(),
            tab: 0,
            confirm: None,
            filter: String::new(),
            writing: false,
        }
    }
}
fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::WHITE)
        .stroke(egui::Stroke::new(1., LINE))
        .corner_radius(16)
        .inner_margin(22)
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
            .min_size(Vec2::new(136., 42.)),
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
        style.visuals.selection.bg_fill = Color32::from_rgb(215, 235, 222);
        style.visuals.selection.stroke = egui::Stroke::new(1., GREEN);
        style.spacing.item_spacing = Vec2::new(12., 12.);
        style.spacing.button_padding = Vec2::new(14., 9.);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(15.));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(14.));
        cc.egui_ctx.set_style(style);
        Self::default()
    }
    fn scan(&mut self, ctx: &egui::Context) {
        let enabled = self
            .enabled
            .iter()
            .enumerate()
            .filter_map(|(i, &v)| v.then_some(i))
            .collect();
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.error = false;
        self.progress = 0.;
        self.routes.clear();
        self.selected = [None; 4];
        self.message = "正在開始掃描…".into();
        let cancel = self.cancel.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            if let Err(e) = network::scan(enabled, cancel, |event| {
                let _ = tx.send(event);
                ctx.request_repaint();
            }) {
                let _ = tx.send(Event::Error(format!("{e:#}")));
                ctx.request_repaint();
            }
        });
    }
    fn poll(&mut self) {
        let events: Vec<_> = self
            .receiver
            .as_ref()
            .map(|r| r.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            match event {
                Event::Progress(p, s) => {
                    self.progress = p;
                    self.message = s;
                }
                Event::Done(routes) => {
                    self.routes = routes;
                    self.selected = [None; 4];
                    for (i, r) in self.routes.iter().enumerate() {
                        if self.selected[r.service].is_none() && r.successes == 3 {
                            self.selected[r.service] = Some(i);
                        }
                    }
                    self.progress = 1.;
                    self.message = format!("掃描完成，找到 {} 條可用路線。", self.routes.len());
                    self.receiver = None;
                }
                Event::Written(s) => {
                    self.message = s;
                    self.error = false;
                    self.writing = false;
                    self.receiver = None;
                    self.status = hosts::status();
                }
                Event::Error(s) => {
                    self.message = s;
                    self.error = true;
                    self.writing = false;
                    self.receiver = None;
                    self.status = hosts::status();
                }
            }
        }
    }
    fn chosen(&self) -> Vec<Route> {
        self.selected
            .iter()
            .enumerate()
            .filter(|(i, _)| self.enabled[*i])
            .filter_map(|(_, i)| i.and_then(|i| self.routes.get(i)).cloned())
            .collect()
    }
    fn apply(&mut self, restore: bool, ctx: &egui::Context) {
        let routes = if restore { vec![] } else { self.chosen() };
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.writing = true;
        self.error = false;
        self.message = "正在驗證並更新 hosts…".into();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<String> {
                if !restore {
                    let nets = network::ranges()?;
                    anyhow::ensure!(!routes.is_empty(), "沒有選取路線");
                    for route in &routes {
                        anyhow::ensure!(
                            nets.iter().any(|n| n.contains(&route.ip)),
                            "IP 已不在 Fastly 官方清單"
                        );
                        network::probe(route.service, route.ip)?;
                    }
                }
                let backup = hosts::write_at(&hosts::path(), &routes)?;
                Ok(format!(
                    "{}。備份：{}",
                    if restore {
                        "已還原系統路線"
                    } else {
                        "已套用路線"
                    },
                    backup.display()
                ))
            })();
            let _ = tx.send(match result {
                Ok(s) => Event::Written(s),
                Err(e) => Event::Error(format!("{e:#}")),
            });
            ctx.request_repaint();
        });
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        let busy = self.receiver.is_some();
        egui::SidePanel::left("nav")
            .exact_width(206.)
            .resizable(false)
            .frame(egui::Frame::new().fill(Color32::WHITE).inner_margin(24))
            .show(ctx, |ui| {
                ui.add_space(16.);
                ui.label(RichText::new("f / ").size(37.).strong().color(GREEN));
                ui.add_space(8.);
                ui.label(RichText::new("Fastly Route").size(21.).strong());
                small(ui, "A clearer connection.");
                ui.add_space(42.);
                for (i, title) in ["01   路線總覽", "02   使用說明"].iter().enumerate() {
                    if ui
                        .add_sized(
                            [158., 43.],
                            egui::Button::new(*title)
                                .selected(self.tab == i)
                                .corner_radius(8),
                        )
                        .clicked()
                    {
                        self.tab = i;
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    small(ui, "v0.1.0 / RUST NATIVE");
                    ui.label(
                        RichText::new("為每一次連線，\n留一條順暢的路。")
                            .size(13.)
                            .color(MUTED),
                    );
                });
            });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::from_rgb(247,249,246)).inner_margin(32)).show(ctx,|ui|{
     egui::ScrollArea::vertical().show(ui,|ui|{
        ui.horizontal(|ui|{small(ui,"NETWORK / ROUTE OPTIMIZER");ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{ui.label(RichText::new(format!("●  {}",self.status)).size(13.).color(GREEN));});});
        ui.add_space(12.);ui.label(RichText::new(if self.tab==0{"更好的連線，從這裡開始。"}else{"簡單操作，清楚掌握。"}).size(29.).strong());small(ui,"探索 Fastly 路線，讓常用服務找到合適的連線。");ui.add_space(18.);
        if self.tab==1{
            card().show(ui,|ui|{ui.set_width(ui.available_width());for(title,body)in[
                ("01   選擇服務","勾選需要的服務後開始掃描。測試使用直接連線，不經系統代理。"),
                ("02   比較路線","先篩選 TCP 延遲最低的 48 條候選，再驗證 HTTPS 並測試三輪回應。\n數字代表收到回應標頭的時間，不代表下載 Mbps。"),
                ("03   套用或還原","只有三輪皆成功的路線可以套用。確認視窗會列出網域與 IP。\n請以系統管理員身分執行程式才能修改 hosts。切換網路後建議還原。"),
                ("關於第一版","GitHub Releases 僅驗證 TLS 與根路徑 404；未測試實際下載。\nTwitch 僅涵蓋圖片，不含影片、聊天室與網頁。\n不含自訂網域、下載頻寬測速、掃描快取與自動更新。"),
                ("備份與資料","每次變更前在 hosts 旁建立帶時間戳記的備份。\n還原只移除本工具區塊。掃描結果僅存於記憶體，無遙測。")
            ]{ui.label(RichText::new(title).strong().size(17.));small(ui,body);ui.add_space(15.);}ui.hyperlink_to("GitHub · Fastly Route","https://github.com/cycleapple/fastly-route");});return;
        }
        card().show(ui,|ui|{ui.set_width(ui.available_width());small(ui,"YOUR SERVICES");ui.label(RichText::new("選擇要優化的服務").strong().size(19.));ui.add_space(8.);
            ui.add_enabled_ui(!busy,|ui|{ui.horizontal_wrapped(|ui|{for(i,s)in SERVICES.iter().enumerate(){ui.checkbox(&mut self.enabled[i],s.name);}});});
            ui.add_space(8.);ui.separator();ui.add_space(4.);ui.horizontal(|ui|{if primary(ui,if busy{"處理中…"}else{"開始掃描  →"},!busy&&self.enabled.iter().any(|v|*v)){self.scan(ctx);}
if busy&&!self.writing&&ui.button("取消").clicked(){self.cancel.store(true,Ordering::Relaxed);}small(ui,"約 1–3 分鐘 · 掃描不會修改設定");});if busy{ui.add(egui::ProgressBar::new(self.progress).desired_width(ui.available_width()).fill(GREEN).animate(true));}
        });ui.add_space(14.);
        ui.horizontal(|ui|{ui.label(RichText::new("可用路線").size(20.).strong());small(ui,format!("{} ROUTES",self.routes.len()));ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("篩選地區 / IP").desired_width(150.));});});
        card().show(ui,|ui|{ui.set_width(ui.available_width());if self.routes.is_empty(){ui.add_space(25.);ui.vertical_centered(|ui|{ui.label(RichText::new("○  ───  ○  ───  ○").size(31.).color(GREEN));ui.add_space(12.);ui.label(RichText::new(if busy{"正在尋找適合你的路線"}else{"你的下一條好路線，等著被發現"}).size(18.));small(ui,"開始掃描後，在這裡比較地區、回應時間與穩定度。");});ui.add_space(27.);}else{
            egui::ScrollArea::vertical().max_height(250.).id_salt("routes").show(ui,|ui|{egui::Grid::new("table").num_columns(6).spacing([16.,10.]).striped(true).show(ui,|ui|{
                for h in["服務","地區","IP 位址","回應","波動範圍","成功率"]{small(ui,h);}ui.end_row();
                for(i,r)in self.routes.iter().enumerate().filter(|(_,r)|self.enabled[r.service]).filter(|(_,r)|self.filter.is_empty()||r.pop.contains(&self.filter.to_uppercase())||r.ip.to_string().contains(&self.filter)){
                    if ui.add_enabled(!busy&&r.successes==3,egui::Button::new(SERVICES[r.service].name).selected(self.selected[r.service]==Some(i))).clicked(){self.selected[r.service]=Some(i);}ui.label(RichText::new(&r.pop).strong());ui.monospace(r.ip.to_string());ui.label(format!("{:.0} ms",r.latency));ui.label(format!("{:.0} ms",r.jitter));ui.colored_label(if r.successes==3{GREEN}else{Color32::DARK_RED},format!("{} / 3",r.successes));ui.end_row();
                }
            });});
        }});ui.add_space(10.);
        ui.horizontal(|ui|{if primary(ui,"套用所選路線",!busy&&!self.chosen().is_empty()){self.confirm=Some(false);}
if ui.add_enabled(!busy,egui::Button::new("還原系統路線").min_size(Vec2::new(135.,42.))).clicked(){self.confirm=Some(true);}
if ui.button("重新整理狀態").clicked(){self.status=hosts::status();}});
        ui.add_space(8.);ui.label(RichText::new(&self.message).size(13.).color(if self.error{Color32::from_rgb(174,65,54)}else{MUTED}));small(ui,"測量 HTTPS 回應時間 · 套用需管理員權限 · 變更前自動備份");
     });
    });
        if let Some(restore) = self.confirm {
            egui::Modal::new(egui::Id::new("confirm_hosts")).show(ctx, |ui| {
                ui.heading(if restore {
                    "還原系統路線"
                } else {
                    "確認套用路線"
                });
                ui.label(if restore {
                    "將移除 Fastly Route 管理的 hosts 區塊。"
                } else {
                    "以下網域將寫入 hosts，其他設定會保留。"
                });
                if !restore {
                    for r in self.chosen() {
                        ui.monospace(format!("{}  {}", r.ip, SERVICES[r.service].domain));
                    }
                }
                small(ui, "請先確認程式具有系統管理員權限。");
                ui.horizontal(|ui| {
                    if primary(ui, "確認", true) {
                        self.confirm = None;
                        self.apply(restore, ctx);
                    }
                    if ui.button("取消").clicked() {
                        self.confirm = None;
                    }
                });
            });
        }
    }
}
fn main() -> eframe::Result<()> {
    if std::env::args().any(|a| a == "--smoke-test") {
        let nets = network::ranges().expect("official IP list");
        println!("Official ranges: {}", nets.len());
        let ip = network::candidates(&nets, 133)
            .into_iter()
            .find(|ip| ip.octets()[0] == 151)
            .unwrap();
        println!(
            "TLS probe {ip}: {:?}",
            network::probe(0, ip).expect("TLS probe failed")
        );
        return Ok(());
    }
    eframe::run_native(
        "Fastly Route",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1120., 800.])
                .with_min_inner_size([980., 720.]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
