use std::{collections::VecDeque, sync::mpsc, thread, time::Duration};

use eframe::egui::{self, Color32, FontId, RichText, Stroke, Vec2};
use sysinfo::System;

use crate::{
    protection::{self, BlockProfile},
    runtimes::{self, RuntimePackage},
    system::{self, StartupEntry},
};

const BG: Color32 = Color32::from_rgb(246, 248, 250);
const INK: Color32 = Color32::from_rgb(29, 35, 41);
const MUTED: Color32 = Color32::from_rgb(108, 118, 129);
const LINE: Color32 = Color32::from_rgb(226, 231, 235);
const ACCENT: Color32 = Color32::from_rgb(0, 123, 101);
const ORANGE: Color32 = Color32::from_rgb(187, 106, 21);

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Overview,
    Startup,
    Protection,
    Optimize,
    Runtimes,
    Tools,
    Activity,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Tổng quan",
            Self::Startup => "Ứng dụng khởi động",
            Self::Protection => "Chặn tải ứng dụng",
            Self::Optimize => "Dọn dẹp & hiệu năng",
            Self::Runtimes => "Thư viện & runtime",
            Self::Tools => "Công cụ",
            Self::Activity => "Nhật ký",
        }
    }
}

#[derive(Default)]
struct Snapshot {
    cpu: f32,
    used: u64,
    total: u64,
    uptime: u64,
    processes: usize,
    top: Vec<(String, u64)>,
}

impl Snapshot {
    fn collect(sys: &mut System) -> Self {
        sys.refresh_all();
        let mut top: Vec<_> = sys
            .processes()
            .values()
            .map(|p| (p.name().to_string_lossy().into_owned(), p.memory()))
            .collect();
        top.sort_unstable_by_key(|p| std::cmp::Reverse(p.1));
        top.truncate(6);
        Self {
            cpu: sys.global_cpu_usage(),
            used: sys.used_memory(),
            total: sys.total_memory(),
            uptime: System::uptime(),
            processes: sys.processes().len(),
            top,
        }
    }
}

enum Event {
    Snapshot(Snapshot),
    Startup(Result<Vec<StartupEntry>, String>),
    Job(Result<String, String>),
    Protection(Result<String, String>),
    Cleanup(Result<system::CleanupPreview, String>),
    Power(Result<String, String>),
    Runtimes(Result<(Vec<RuntimePackage>, bool), String>),
    RuntimeInstall(Result<String, String>),
}

#[derive(Clone, Copy)]
enum Confirm {
    Clean,
    RestoreHosts,
    InstallRuntimes,
}

pub struct Velo {
    smoke: Option<Smoke>,
    page: Page,
    admin: bool,
    os: String,
    snapshot: Snapshot,
    samples: VecDeque<f32>,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
    startup: Vec<StartupEntry>,
    startup_loaded: bool,
    search: String,
    profiles: Vec<BlockProfile>,
    profile_name: String,
    domains: String,
    profile_error: Option<String>,
    applied: bool,
    dirty: bool,
    busy: Option<String>,
    notice: Option<(bool, String)>,
    log: Vec<(bool, String)>,
    cleanup: Option<system::CleanupPreview>,
    power: String,
    confirm: Option<Confirm>,
    runtimes: Vec<RuntimePackage>,
    runtime_selected: std::collections::HashSet<String>,
    runtime_loaded: bool,
    runtime_error: Option<String>,
    winget: bool,
}

impl Velo {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure(&cc.egui_ctx);
        let (tx, rx) = mpsc::channel();
        let telemetry = tx.clone();
        thread::spawn(move || {
            let mut sys = System::new();
            sys.refresh_cpu_all();
            thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
            loop {
                if telemetry
                    .send(Event::Snapshot(Snapshot::collect(&mut sys)))
                    .is_err()
                {
                    break;
                }
                thread::sleep(Duration::from_secs(2));
            }
        });
        let (profiles, error) = match protection::load_profiles() {
            Ok(profiles) => (profiles, None),
            Err(e) => (vec![], Some((false, e))),
        };
        let applied = protection::protection_status().unwrap_or(false);
        let dirty = !protection::profiles_match_hosts(&profiles).unwrap_or(true);
        let mut app = Self {
            smoke: Smoke::from_args(),
            page: Page::Overview,
            admin: system::is_admin(),
            os: format!("Windows {}", System::os_version().unwrap_or_default()),
            snapshot: Snapshot::default(),
            samples: VecDeque::new(),
            tx,
            rx,
            startup: vec![],
            startup_loaded: false,
            search: String::new(),
            profiles,
            profile_name: String::new(),
            domains: String::new(),
            profile_error: None,
            applied,
            dirty,
            busy: None,
            notice: error,
            log: vec![],
            cleanup: None,
            power: "Đang đọc…".into(),
            confirm: None,
            runtimes: vec![],
            runtime_selected: Default::default(),
            runtime_loaded: false,
            runtime_error: None,
            winget: false,
        };
        app.refresh_startup();
        app.refresh_power();
        app.refresh_runtimes();
        app
    }

    fn refresh_startup(&self) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Event::Startup(system::startup_entries()));
        });
    }

    fn refresh_power(&self) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Event::Power(system::power_plan()));
        });
    }

    fn refresh_runtimes(&mut self) {
        self.runtime_loaded = false;
        self.runtime_error = None;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = runtimes::scan().map(|packages| (packages, runtimes::winget_available()));
            let _ = tx.send(Event::Runtimes(result));
        });
    }

    fn job(&mut self, label: &str, f: impl FnOnce() -> Result<String, String> + Send + 'static) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(label.into());
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Event::Job(f()));
        });
    }

    fn protection_job(
        &mut self,
        label: &str,
        f: impl FnOnce() -> Result<String, String> + Send + 'static,
    ) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(label.into());
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Event::Protection(f()));
        });
    }

    fn finish_job(&mut self, result: Result<String, String>) {
        self.busy = None;
        let entry = match result {
            Ok(s) => (true, s),
            Err(e) => (false, e),
        };
        self.notice = Some(entry.clone());
        self.log.insert(0, entry);
        self.log.truncate(100);
        self.refresh_startup();
        self.refresh_power();
        self.applied = protection::protection_status().unwrap_or(false);
    }

    fn drain(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Snapshot(s) => {
                    self.samples.push_back(s.cpu);
                    if self.samples.len() > 60 {
                        self.samples.pop_front();
                    }
                    self.snapshot = s;
                }
                Event::Startup(Ok(entries)) => {
                    self.startup = entries;
                    self.startup_loaded = true;
                }
                Event::Startup(Err(e)) => {
                    self.startup_loaded = true;
                    self.notice = Some((false, e));
                }
                Event::Job(result) => {
                    self.finish_job(result);
                }
                Event::Protection(mut result) => {
                    if result.is_ok() {
                        match protection::load_profiles() {
                            Ok(profiles) => {
                                self.profiles = profiles;
                            }
                            Err(e) => result = Err(e),
                        }
                    }
                    match protection::profiles_match_hosts(&self.profiles) {
                        Ok(matches) => self.dirty = !matches,
                        Err(e) => {
                            self.dirty = true;
                            if result.is_ok() {
                                result = Err(e);
                            }
                        }
                    }
                    self.finish_job(result);
                }
                Event::Cleanup(result) => {
                    self.busy = None;
                    match result {
                        Ok(p) => self.cleanup = Some(p),
                        Err(e) => self.notice = Some((false, e)),
                    }
                }
                Event::Power(result) => {
                    self.power = result.unwrap_or_else(|e| format!("Không đọc được: {e}"))
                }
                Event::Runtimes(result) => {
                    self.runtime_loaded = true;
                    match result {
                        Ok((packages, winget)) => {
                            self.runtime_selected = packages
                                .iter()
                                .filter(|p| !p.installed)
                                .map(|p| p.id.clone())
                                .collect();
                            self.runtimes = packages;
                            self.winget = winget;
                        }
                        Err(e) => self.runtime_error = Some(e),
                    }
                }
                Event::RuntimeInstall(result) => {
                    self.finish_job(result);
                    self.refresh_runtimes();
                }
            }
        }
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("navigation")
            .exact_width(210.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(INK).inner_margin(22))
            .show(ctx, |ui| {
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(30.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 6.0, ACCENT);
                    ui.painter().line_segment(
                        [
                            rect.left_top() + Vec2::new(8.0, 8.0),
                            rect.left_top() + Vec2::new(14.0, 23.0),
                        ],
                        Stroke::new(3.0_f32, Color32::WHITE),
                    );
                    ui.painter().line_segment(
                        [
                            rect.left_top() + Vec2::new(14.0, 23.0),
                            rect.left_top() + Vec2::new(23.0, 8.0),
                        ],
                        Stroke::new(3.0_f32, Color32::WHITE),
                    );
                    ui.label(
                        RichText::new("Velo")
                            .size(29.0)
                            .strong()
                            .color(Color32::WHITE),
                    );
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new("WINDOWS TOOLKIT")
                        .size(10.0)
                        .color(Color32::from_gray(149)),
                );
                ui.add_space(42.0);
                for (page, label) in [
                    (Page::Overview, "Tổng quan"),
                    (Page::Startup, "Khởi động"),
                    (Page::Protection, "Chặn tải"),
                    (Page::Optimize, "Hiệu năng"),
                    (Page::Runtimes, "Thư viện"),
                    (Page::Tools, "Công cụ"),
                    (Page::Activity, "Nhật ký"),
                ] {
                    let selected = self.page == page;
                    let text = RichText::new(label).size(15.0).color(if selected {
                        Color32::WHITE
                    } else {
                        Color32::from_gray(174)
                    });
                    let button = egui::Button::new(text)
                        .fill(if selected {
                            Color32::from_rgb(49, 61, 64)
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(Stroke::NONE)
                        .corner_radius(5.0);
                    if ui.add_sized([166.0, 43.0], button).clicked() {
                        self.page = page;
                    }
                    ui.add_space(5.0);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(
                        RichText::new(concat!("v", env!("CARGO_PKG_VERSION"), "  /  Rust native"))
                            .size(11.0)
                            .color(Color32::from_gray(132)),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(if self.admin {
                            "●  Quyền quản trị"
                        } else {
                            "●  Quyền người dùng"
                        })
                        .size(12.0)
                        .color(Color32::from_rgb(112, 204, 176)),
                    );
                });
            });
    }

    fn overview(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("TRẠNG THÁI HỆ THỐNG")
                    .size(11.0)
                    .color(MUTED)
                    .strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                pill(ui, "Đang cập nhật", ACCENT);
            });
        });
        ui.add_space(13.0);
        ui.columns(3, |cols| {
            metric(
                &mut cols[0],
                "CPU",
                &format!("{:.0}%", self.snapshot.cpu),
                "Mức sử dụng hiện tại",
                ACCENT,
                self.snapshot.cpu / 100.0,
            );
            let ratio = self.snapshot.used as f32 / self.snapshot.total.max(1) as f32;
            metric(
                &mut cols[1],
                "BỘ NHỚ RAM",
                &format!("{:.1} GB", self.snapshot.used as f64 / 1_073_741_824.0),
                &format!(
                    "/ {:.1} GB tổng bộ nhớ",
                    self.snapshot.total as f64 / 1_073_741_824.0
                ),
                Color32::from_rgb(103, 85, 173),
                ratio,
            );
            metric(
                &mut cols[2],
                "THỜI GIAN HOẠT ĐỘNG",
                &format!(
                    "{}h {}m",
                    self.snapshot.uptime / 3600,
                    (self.snapshot.uptime / 60) % 60
                ),
                &format!("{} tiến trình đang chạy", self.snapshot.processes),
                ORANGE,
                0.0,
            );
        });
        ui.add_space(26.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Hoạt động CPU").size(16.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new("120 giây gần nhất").size(12.0).color(MUTED));
            });
        });
        ui.add_space(10.0);
        graph(ui, &self.samples);
        ui.add_space(24.0);
        ui.label(RichText::new("Thao tác nhanh").size(17.0).strong());
        ui.add_space(8.0);
        action_row(
            ui,
            "Ứng dụng khởi động",
            &format!(
                "{} ứng dụng đang bật cùng Windows",
                self.startup.iter().filter(|s| s.enabled).count()
            ),
            "Quản lý",
            || {
                self.page = Page::Startup;
            },
        );
        action_row(
            ui,
            "Nguồn tải bị chặn",
            &format!(
                "{} cấu hình đã chọn · {}",
                self.profiles.iter().filter(|p| p.enabled).count(),
                if self.applied {
                    "Đã có quy tắc trong hosts"
                } else {
                    "Chưa áp dụng"
                }
            ),
            "Thiết lập",
            || {
                self.page = Page::Protection;
            },
        );
        action_row(
            ui,
            "Dọn dẹp hệ thống",
            "File tạm cũ và chế độ nguồn",
            "Mở",
            || {
                self.page = Page::Optimize;
            },
        );
        ui.add_space(22.0);
        ui.label(
            RichText::new("Tiến trình dùng nhiều RAM")
                .size(16.0)
                .strong(),
        );
        ui.add_space(8.0);
        for (name, bytes) in &self.snapshot.top {
            ui.horizontal(|ui| {
                ui.label(name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format_bytes(*bytes)).color(MUTED));
                });
            });
            ui.add_space(3.0);
        }
    }

    fn startup(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_sized(
                [300.0, 34.0],
                egui::TextEdit::singleline(&mut self.search).hint_text("Tìm ứng dụng…"),
            );
            if ui.button("Làm mới").clicked() {
                self.refresh_startup();
            }
        });
        ui.add_space(12.0);
        ui.label(RichText::new("Bật/tắt mục Run và thư mục Startup của Windows. Thay đổi có hiệu lực ở lần đăng nhập tiếp theo.").color(MUTED));
        ui.add_space(16.0);
        if !self.startup_loaded {
            ui.spinner();
            return;
        }
        let filter = self.search.to_lowercase();
        let entries: Vec<_> = self
            .startup
            .iter()
            .filter(|s| {
                format!("{} {}", s.name, s.command)
                    .to_lowercase()
                    .contains(&filter)
            })
            .cloned()
            .collect();
        if entries.is_empty() {
            empty_state(ui, "Không có mục khởi động phù hợp.");
        }
        for entry in entries {
            let text_width = (ui.available_width() - 100.0).max(150.0);
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(text_width, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        ui.label(RichText::new(&entry.name).strong());
                        ui.add(
                            egui::Label::new(RichText::new(&entry.command).size(11.0).color(MUTED))
                                .truncate(),
                        )
                        .on_hover_text(&entry.command);
                        ui.label(RichText::new(&entry.location).size(11.0).color(MUTED));
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut enabled = entry.enabled;
                    let can_change = self.busy.is_none() && (!entry.requires_admin || self.admin);
                    let response =
                        ui.add_enabled(can_change, egui::Checkbox::new(&mut enabled, "Bật"));
                    if response.changed() {
                        self.job("Đang cập nhật khởi động…", move || {
                            system::set_startup_enabled(&entry, enabled)?;
                            Ok(format!(
                                "{} khởi động cùng Windows: {}",
                                if enabled { "Đã bật" } else { "Đã tắt" },
                                entry.name
                            ))
                        });
                    }
                    if !can_change {
                        response.on_hover_text("Cần quyền quản trị hoặc chờ tác vụ hiện tại.");
                    }
                });
            });
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);
        }
        ui.add_space(12.0);
        ui.label(
            RichText::new("Các dịch vụ và tác vụ theo lịch được quản lý riêng trong Windows.")
                .size(12.0)
                .color(MUTED),
        );
    }

    fn protection(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            pill(
                ui,
                if self.applied {
                    "Quy tắc hosts đang có"
                } else {
                    "Chưa chặn"
                },
                if self.applied { ACCENT } else { MUTED },
            );
            if self.dirty {
                pill(ui, "Có thay đổi chưa áp dụng", ORANGE);
            }
        });
        ui.add_space(14.0);
        ui.label(RichText::new("Chặn truy cập chính xác các tên miền đã chọn, gồm nguồn tải và cập nhật của ứng dụng.").color(MUTED));
        ui.label(
            RichText::new("Nguồn tải khác, DNS riêng và ứng dụng đã cài có thể không bị chặn.")
                .size(12.0)
                .color(ORANGE),
        );
        ui.add_space(18.0);
        let mut remove = None;
        for (i, profile) in self.profiles.iter_mut().enumerate() {
            let text_width = (ui.available_width() - 90.0).max(100.0);
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(text_width, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        if ui
                            .checkbox(&mut profile.enabled, RichText::new(&profile.name).strong())
                            .changed()
                        {
                            self.dirty = true;
                        }
                        ui.label(
                            RichText::new(format!(
                                "{} tên miền · {}",
                                profile.domains.len(),
                                if profile.builtin {
                                    "Có sẵn"
                                } else {
                                    "Tùy chỉnh"
                                }
                            ))
                            .size(12.0)
                            .color(MUTED),
                        );
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("Xóa")
                        .on_hover_text("Xóa cấu hình khỏi danh sách; bấm Áp dụng để cập nhật hosts")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
            });
            egui::CollapsingHeader::new("Tên miền")
                .id_salt(&profile.id)
                .show(ui, |ui| {
                    for domain in &profile.domains {
                        ui.label(RichText::new(domain).monospace().size(12.0));
                    }
                });
            ui.separator();
            ui.add_space(8.0);
        }
        if let Some(i) = remove {
            self.profiles.remove(i);
            self.dirty = true;
        }
        ui.add_space(12.0);
        ui.label(RichText::new("Thêm ứng dụng").size(16.0).strong());
        ui.add_space(8.0);
        ui.add_sized(
            [ui.available_width().min(430.0), 32.0],
            egui::TextEdit::singleline(&mut self.profile_name).hint_text("Tên ứng dụng"),
        );
        ui.add_space(6.0);
        ui.add_sized(
            [ui.available_width().min(430.0), 76.0],
            egui::TextEdit::multiline(&mut self.domains)
                .hint_text("Tên miền tải, mỗi dòng một tên miền"),
        );
        if ui.button("+ Thêm vào danh sách").clicked() {
            match custom_profile(&self.profile_name, &self.domains, &self.profiles) {
                Ok(profile) => {
                    self.profiles.push(profile);
                    self.profile_name.clear();
                    self.domains.clear();
                    self.profile_error = None;
                    self.dirty = true;
                }
                Err(e) => self.profile_error = Some(e),
            }
        }
        if let Some(e) = &self.profile_error {
            ui.colored_label(ORANGE, e);
        }
        ui.add_space(18.0);
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(self.busy.is_none() && self.admin, primary("Áp dụng chặn tải")).clicked() {
                let profiles = self.profiles.clone();
                self.protection_job("Đang áp dụng quy tắc…", move || {
                    protection::apply_profiles(&profiles)?;
                    Ok("Đã áp dụng danh sách tên miền. Có thể cần mở lại trình duyệt hoặc ứng dụng.".into())
                });
            }
            if ui.add_enabled(self.busy.is_none(), egui::Button::new("Lưu danh sách")).clicked() {
                self.notice = Some(match protection::save_profiles(&self.profiles) { Ok(()) => (true, "Đã lưu danh sách. Quy tắc chặn chưa thay đổi.".into()), Err(e) => (false, e) });
            }
            if ui.add_enabled(self.busy.is_none() && self.admin, egui::Button::new("Gỡ toàn bộ chặn")).clicked() { self.confirm = Some(Confirm::RestoreHosts); }
        });
        if !self.admin {
            ui.label(
                RichText::new("Cần mở Velo bằng quyền quản trị để sửa hosts.")
                    .size(12.0)
                    .color(ORANGE),
            );
        }
    }

    fn optimize(&mut self, ui: &mut egui::Ui) {
        section(
            ui,
            "01",
            "File tạm",
            "Chỉ xét file trong thư mục Temp người dùng, cũ hơn 24 giờ.",
        );
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(self.busy.is_none(), egui::Button::new("Quét file tạm"))
                .clicked()
            {
                self.busy = Some("Đang quét file tạm…".into());
                let tx = self.tx.clone();
                thread::spawn(move || {
                    let _ = tx.send(Event::Cleanup(system::cleanup_preview()));
                });
            }
            if let Some(p) = &self.cleanup {
                ui.label(format!("{} file · {}", p.files, format_bytes(p.bytes)));
                if ui
                    .add_enabled(self.busy.is_none() && p.files > 0, primary("Dọn file tạm"))
                    .clicked()
                {
                    self.confirm = Some(Confirm::Clean);
                }
            }
        });
        ui.add_space(26.0);
        ui.separator();
        ui.add_space(24.0);
        section(
            ui,
            "02",
            "Bộ nhớ RAM",
            "Thu gọn working set của tiến trình thuộc tài khoản hiện tại. Ứng dụng vẫn tiếp tục chạy.",
        );
        ui.label(
            RichText::new(
                "Tác dụng tạm thời; RAM trống nhiều hơn không đồng nghĩa máy chạy nhanh hơn.",
            )
            .size(12.0)
            .color(ORANGE),
        );
        ui.add_space(12.0);
        if ui
            .add_enabled(self.busy.is_none(), egui::Button::new("Thu gọn RAM"))
            .clicked()
        {
            self.job("Đang thu gọn bộ nhớ…", || {
                let r = system::trim_memory()?;
                Ok(format!(
                    "Đã thu gọn {} tiến trình; bỏ qua {} tiến trình không có quyền truy cập.",
                    r.processes_trimmed, r.skipped
                ))
            });
        }
        ui.add_space(26.0);
        ui.separator();
        ui.add_space(24.0);
        section(
            ui,
            "03",
            "Chế độ nguồn",
            "Chọn cân bằng cho nhu cầu thường ngày, hiệu năng cao khi cần xử lý nặng.",
        );
        ui.label(RichText::new(&self.power).color(ACCENT));
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            for (high, label) in [(false, "Cân bằng"), (true, "Hiệu năng cao")] {
                if ui
                    .add_enabled(self.busy.is_none(), egui::Button::new(label))
                    .clicked()
                {
                    self.job("Đang đổi chế độ nguồn…", move || {
                        system::set_power_plan(high)?;
                        Ok(format!("Đã đổi chế độ nguồn: {label}."))
                    });
                }
            }
        });
        ui.add_space(10.0);
        ui.label(RichText::new("Hiệu năng cao có thể tăng nhiệt và hao pin. Một số máy Modern Standby chỉ hỗ trợ Cân bằng.").size(12.0).color(MUTED));
    }

    fn tools(&mut self, ui: &mut egui::Ui) {
        section(ui, "CTT", "Chris Titus Tech · WinUtil", "Nguồn chính thức");
        ui.horizontal_wrapped(|ui| {
            if ui.button("GitHub / WinUtil").clicked() {
                self.open_url("https://github.com/ChrisTitusTech/winutil");
            }
            if ui.button("Tài liệu WinUtil").clicked() {
                self.open_url("https://winutil.christitus.com/");
            }
        });
        ui.add_space(16.0);
        ui.label("Lệnh PowerShell của Chris Titus:");
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.admin,
                    egui::Button::new(RichText::new(system::WINUTIL_COMMAND).monospace()),
                )
                .on_hover_text("Mở PowerShell quản trị và chạy WinUtil")
                .clicked()
            {
                self.job("Đang mở WinUtil…", || {
                    system::launch_winutil()?;
                    Ok("Đã mở PowerShell để chạy WinUtil. Kết quả tải và chạy hiển thị trong cửa sổ PowerShell.".into())
                });
            }
            if ui.button("Sao chép").clicked() {
                ui.ctx().copy_text(system::WINUTIL_COMMAND.into());
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Lệnh tải và thực thi mã từ Internet. Đọc nguồn trước khi chạy bằng quyền quản trị.").size(12.0).color(ORANGE));
        ui.add_space(28.0);
        ui.separator();
        ui.add_space(24.0);
        section(ui, "WIN", "Cài đặt Windows", "");
        for (name, url) in [
            ("Ứng dụng đã cài", "ms-settings:appsfeatures"),
            ("Ứng dụng khởi động", "ms-settings:startupapps"),
            ("Windows Update", "ms-settings:windowsupdate"),
            ("Dung lượng lưu trữ", "ms-settings:storagesense"),
            ("Bảo mật Windows", "windowsdefender:"),
        ] {
            action_row(ui, name, "", "Mở", || {
                self.open_url(url);
            });
        }
    }

    fn runtimes(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Thư viện phổ biến cho ứng dụng và game Windows").color(MUTED));
            if ui.button("Kiểm tra lại").clicked() {
                self.refresh_runtimes();
            }
        });
        ui.add_space(18.0);
        if !self.runtime_loaded {
            ui.spinner();
            ui.label("Đang kiểm tra thư viện…");
            return;
        }
        if let Some(e) = &self.runtime_error {
            ui.colored_label(ORANGE, e);
            return;
        }
        for package in &self.runtimes {
            let text_width = (ui.available_width() - 100.0).max(100.0);
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(text_width, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        let mut selected = self.runtime_selected.contains(&package.id);
                        if ui
                            .add_enabled(
                                !package.installed,
                                egui::Checkbox::new(&mut selected, &package.name),
                            )
                            .changed()
                        {
                            if selected {
                                self.runtime_selected.insert(package.id.clone());
                            } else {
                                self.runtime_selected.remove(&package.id);
                            }
                        }
                        ui.label(RichText::new(&package.detail).size(12.0).color(MUTED));
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    pill(
                        ui,
                        if package.installed {
                            "Đã có"
                        } else {
                            "Chưa có"
                        },
                        if package.installed { ACCENT } else { ORANGE },
                    );
                });
            });
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);
        }
        ui.add_space(12.0);
        let count = self
            .runtimes
            .iter()
            .filter(|p| !p.installed && self.runtime_selected.contains(&p.id))
            .count();
        if self.winget {
            if ui
                .add_enabled(
                    self.busy.is_none() && self.admin && count > 0,
                    primary(&format!("Cài {count} thư viện còn thiếu")),
                )
                .clicked()
            {
                self.confirm = Some(Confirm::InstallRuntimes);
            }
            if !self.admin {
                ui.label(
                    RichText::new("Mở bằng Admin để cài thư viện cho máy.")
                        .size(12.0)
                        .color(ORANGE),
                );
            }
        } else {
            ui.colored_label(ORANGE, "Máy chưa có WinGet / App Installer.");
            if ui.button("Cài App Installer từ Microsoft").clicked() {
                self.open_url("https://aka.ms/getwinget");
            }
            ui.label(
                RichText::new("Cài App Installer xong, bấm Kiểm tra lại.")
                    .size(12.0)
                    .color(MUTED),
            );
        }
        ui.add_space(18.0);
        ui.label(RichText::new("WinGet tải gói từ nguồn winget chính thức và kiểm tra mã băm của bộ cài. Cần Internet; một số gói yêu cầu khởi động lại.").size(12.0).color(MUTED));
        ui.add_space(8.0);
        ui.label(RichText::new("Danh sách này không bao gồm mọi thư viện của từng game. Velo không cần cài thêm các runtime này để chạy.").size(12.0).color(MUTED));
    }

    fn open_url(&mut self, url: &str) {
        if let Err(e) = system::open_url(url) {
            self.notice = Some((false, e));
        }
    }

    fn activity(&self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Các thao tác trong phiên hiện tại").color(MUTED));
        ui.add_space(18.0);
        if self.log.is_empty() {
            empty_state(ui, "Chưa thực hiện thay đổi nào.");
        }
        for (ok, text) in &self.log {
            ui.horizontal_wrapped(|ui| {
                pill(
                    ui,
                    if *ok { "Xong" } else { "Lỗi" },
                    if *ok { ACCENT } else { ORANGE },
                );
                ui.label(text);
            });
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);
        }
    }

    fn confirmation(&mut self, ctx: &egui::Context) {
        let Some(confirm) = self.confirm else {
            return;
        };
        egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
            ui.set_width(430.0);
            ui.heading(match confirm { Confirm::Clean => "Dọn file tạm?", Confirm::RestoreHosts => "Gỡ quy tắc chặn tải?", Confirm::InstallRuntimes => "Cài các thư viện đã chọn?" });
            ui.add_space(12.0);
            ui.label(match confirm { Confirm::Clean => "Các file tạm đủ điều kiện sẽ bị xóa vĩnh viễn. File đang dùng sẽ được bỏ qua.",
                Confirm::RestoreHosts => "Xóa phần quy tắc do Velo quản lý trong hosts và tắt mọi cấu hình chặn đã lưu.",
                Confirm::InstallRuntimes => "WinGet sẽ tự tải và cài các gói dưới đây. Việc tiếp tục đồng ý điều khoản nguồn WinGet và giấy phép của từng gói." });
            if matches!(confirm, Confirm::InstallRuntimes) {
                for package in self.runtimes.iter().filter(|p| !p.installed && self.runtime_selected.contains(&p.id)) { ui.label(format!("• {}", package.name)); }
            }
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                if ui.button("Hủy").clicked() { self.confirm = None; }
                if ui.add(primary("Xác nhận")).clicked() {
                    self.confirm = None;
                    match confirm {
                        Confirm::Clean => {
                            self.cleanup = None;
                            self.job("Đang dọn file tạm…", || {
                                let r = system::cleanup_temp()?;
                                Ok(format!("Đã xóa {} file, giải phóng {}; bỏ qua {} mục.", r.files_removed, format_bytes(r.bytes_freed), r.skipped))
                            });
                        }
                        Confirm::RestoreHosts => {
                            self.protection_job("Đang gỡ quy tắc…", || { protection::restore_hosts()?; Ok("Đã gỡ quy tắc hosts của Velo.".into()) });
                        }
                        Confirm::InstallRuntimes => {
                            let packages: Vec<_> = self.runtimes.iter().filter(|p| !p.installed && self.runtime_selected.contains(&p.id)).cloned().collect();
                            self.busy = Some("Đang tải và cài thư viện, có thể mất vài phút…".into());
                            let tx = self.tx.clone();
                            thread::spawn(move || { let _ = tx.send(Event::RuntimeInstall(runtimes::install_missing(packages))); });
                        }
                    }
                }
            });
        });
    }
}

impl eframe::App for Velo {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        if let Some(smoke) = &mut self.smoke {
            smoke.update(
                ctx,
                &mut self.page,
                self.runtime_loaded && self.startup_loaded && self.snapshot.total > 0,
            );
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.busy.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.notice = Some((
                false,
                "Chờ tác vụ hiện tại hoàn tất trước khi đóng Velo.".into(),
            ));
        }
        self.sidebar(ctx);
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(30, 20)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(self.page.title()).size(23.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.admin && ui.button("Mở bằng Admin").clicked() {
                            let result = if self.dirty {
                                protection::save_profiles(&self.profiles)
                                    .and_then(|_| system::restart_as_admin())
                            } else {
                                system::restart_as_admin()
                            };
                            match result {
                                Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                                Err(e) => self.notice = Some((false, e)),
                            }
                        }
                        ui.label(RichText::new(&self.os).size(12.0).color(MUTED));
                    });
                });
            });
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(30, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(busy) = &self.busy {
                        ui.spinner();
                        ui.label(busy);
                    } else {
                        ui.label(RichText::new("Sẵn sàng").color(ACCENT));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("Windows 10 / 11").size(11.0).color(MUTED));
                    });
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(30))
            .show(ctx, |ui| {
                if let Some((ok, message)) = self.notice.clone() {
                    egui::Frame::new()
                        .fill(if ok {
                            Color32::from_rgb(225, 244, 236)
                        } else {
                            Color32::from_rgb(255, 239, 219)
                        })
                        .inner_margin(12)
                        .corner_radius(4.0)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(message).color(if ok {
                                    ACCENT
                                } else {
                                    ORANGE
                                }));
                                if ui
                                    .small_button("×")
                                    .on_hover_text("Đóng thông báo")
                                    .clicked()
                                {
                                    self.notice = None;
                                }
                            });
                        });
                    ui.add_space(14.0);
                }
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_enabled_ui(self.busy.is_none(), |ui| match self.page {
                            Page::Overview => self.overview(ui),
                            Page::Startup => self.startup(ui),
                            Page::Protection => self.protection(ui),
                            Page::Optimize => self.optimize(ui),
                            Page::Runtimes => self.runtimes(ui),
                            Page::Tools => self.tools(ui),
                            Page::Activity => self.activity(ui),
                        });
                    });
            });
        self.confirmation(ctx);
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

fn configure(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    let mut fonts = egui::FontDefinitions::default();
    let font = std::path::PathBuf::from(
        std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()),
    )
    .join("Fonts\\segoeui.ttf");
    if let Ok(bytes) = std::fs::read(font) {
        fonts
            .font_data
            .insert("segoe".into(), egui::FontData::from_owned(bytes).into());
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "segoe".into());
    }
    ctx.set_fonts(fonts);
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::light();
    style.visuals.override_text_color = Some(INK);
    style.visuals.panel_fill = BG;
    style.visuals.selection.bg_fill = ACCENT;
    style.visuals.widgets.inactive.bg_fill = Color32::WHITE;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, LINE);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(232, 242, 239);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(212, 236, 227);
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(14.0, 8.0);
    style.spacing.interact_size.y = 32.0;
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(13.0));
    ctx.set_style(style);
}

fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(Color32::WHITE))
        .fill(ACCENT)
        .stroke(Stroke::NONE)
        .corner_radius(4.0)
}

fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.label(RichText::new(text).size(11.0).strong().color(color));
}

fn metric(
    ui: &mut egui::Ui,
    label: &str,
    value: &str,
    detail: &str,
    color: Color32,
    progress: f32,
) {
    egui::Frame::new()
        .fill(Color32::WHITE)
        .stroke(Stroke::new(1.0_f32, LINE))
        .corner_radius(6.0)
        .inner_margin(18)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(119.0);
            ui.label(RichText::new(label).size(10.0).color(MUTED).strong());
            ui.add_space(5.0);
            ui.label(RichText::new(value).size(28.0).strong());
            ui.label(RichText::new(detail).size(12.0).color(MUTED));
            ui.add_space(9.0);
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), 3.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 1.0, BG);
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    rect.min,
                    Vec2::new(rect.width() * progress.clamp(0.0, 1.0), 3.0),
                ),
                1.0,
                color,
            );
        });
}

fn graph(ui: &mut egui::Ui, samples: &VecDeque<f32>) {
    let (rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 112.0), egui::Sense::hover());
    let painter = ui.painter();
    for i in 0..=4 {
        let y = rect.top() + rect.height() * i as f32 / 4.0;
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            Stroke::new(1.0_f32, LINE),
        );
    }
    if samples.len() > 1 {
        let points: Vec<_> = samples
            .iter()
            .enumerate()
            .map(|(i, v)| {
                egui::pos2(
                    rect.right() - (samples.len() - 1 - i) as f32 / 59.0 * rect.width(),
                    rect.bottom() - v.clamp(0.0, 100.0) / 100.0 * rect.height(),
                )
            })
            .collect();
        painter.add(egui::Shape::line(points, Stroke::new(2.0_f32, ACCENT)));
    }
}

fn action_row(ui: &mut egui::Ui, title: &str, detail: &str, command: &str, action: impl FnOnce()) {
    let mut clicked = false;
    let text_width = (ui.available_width() - 114.0).max(100.0);
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(text_width, 0.0),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                ui.label(RichText::new(title).strong());
                if !detail.is_empty() {
                    ui.label(RichText::new(detail).size(12.0).color(MUTED));
                }
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            clicked = ui
                .add_sized([94.0, 32.0], egui::Button::new(command))
                .clicked();
        });
    });
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(8.0);
    if clicked {
        action();
    }
}

fn section(ui: &mut egui::Ui, number: &str, title: &str, detail: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(number).size(12.0).color(ACCENT).strong());
        ui.label(RichText::new(title).size(18.0).strong());
    });
    if !detail.is_empty() {
        ui.label(RichText::new(detail).color(MUTED));
    }
    ui.add_space(12.0);
}

fn empty_state(ui: &mut egui::Ui, text: &str) {
    ui.add_space(30.0);
    ui.label(RichText::new(text).color(MUTED));
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.2} GB", bytes as f64 / 1_073_741_824.0)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    }
}

fn custom_profile(
    name: &str,
    domains: &str,
    existing: &[BlockProfile],
) -> Result<BlockProfile, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("Nhập tên ứng dụng từ 1 đến 80 ký tự.".into());
    }
    if existing
        .iter()
        .any(|p| p.name.to_lowercase() == name.to_lowercase())
    {
        return Err("Tên ứng dụng đã có trong danh sách.".into());
    }
    let mut normalized = Vec::new();
    for value in domains.split_whitespace() {
        let domain = protection::validate_domain(value)?;
        if !normalized.contains(&domain) {
            normalized.push(domain);
        }
    }
    if normalized.is_empty() {
        return Err("Thêm ít nhất một tên miền tải ứng dụng.".into());
    }
    let id = format!(
        "custom-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    Ok(BlockProfile {
        id,
        name: name.into(),
        domains: normalized,
        enabled: true,
        builtin: false,
    })
}

pub fn diagnose() {
    let mut sys = System::new();
    let s = Snapshot::collect(&mut sys);
    let result = serde_json::json!({
        "os": System::long_os_version(), "admin": system::is_admin(),
        "ram_bytes": s.total, "processes": s.processes,
        "startup": system::startup_entries().map(|s| s.len()),
        "profiles": protection::load_profiles().map(|s| s.len()),
        "hosts_managed": protection::protection_status(), "power_plan": system::power_plan(),
        "runtimes": runtimes::scan().map(|p| p.into_iter().map(|p| (p.id, p.installed)).collect::<Vec<_>>()),
        "read_only": true,
    });
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}

struct Smoke {
    directory: std::path::PathBuf,
    stage: usize,
    ticks: u8,
    pending: bool,
}

impl Smoke {
    fn from_args() -> Option<Self> {
        let args: Vec<_> = std::env::args_os().collect();
        let index = args.iter().position(|a| a == "--ui-smoke-dir")?;
        let directory = std::path::PathBuf::from(args.get(index + 1)?);
        std::fs::create_dir_all(&directory).ok()?;
        Some(Self {
            directory,
            stage: 0,
            ticks: 0,
            pending: false,
        })
    }

    fn update(&mut self, ctx: &egui::Context, page: &mut Page, ready: bool) {
        let pages = [
            Page::Overview,
            Page::Startup,
            Page::Protection,
            Page::Optimize,
            Page::Runtimes,
            Page::Tools,
            Page::Activity,
        ];
        let names = [
            "overview",
            "startup",
            "protection",
            "optimize",
            "runtimes",
            "tools",
            "activity",
        ];
        if let Some(image) = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        }) {
            let pixels: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            let path = self.directory.join(format!(
                "{}-{}.png",
                names[self.stage % 7],
                if self.stage < 7 { "wide" } else { "compact" }
            ));
            image::save_buffer(
                &path,
                &pixels,
                image.width() as u32,
                image.height() as u32,
                image::ColorType::Rgba8,
            )
            .expect("Save smoke screenshot");
            self.stage += 1;
            self.pending = false;
            self.ticks = 0;
        }
        if self.stage == 14 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        *page = pages[self.stage % 7];
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(if self.stage < 7 {
            Vec2::new(1180.0, 790.0)
        } else {
            Vec2::new(900.0, 640.0)
        }));
        if ready && !self.pending {
            self.ticks += 1;
            if self.ticks >= 4 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                self.pending = true;
            }
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_profile_rejects_empty_duplicate_and_unsafe_domain() {
        assert!(custom_profile("", "example.com", &[]).is_err());
        assert!(custom_profile("App", "example.com/path", &[]).is_err());
        let p = custom_profile("App", "EXAMPLE.com example.com\ncdn.example.com", &[]).unwrap();
        assert_eq!(p.domains, vec!["example.com", "cdn.example.com"]);
        assert!(custom_profile("app", "example.org", &[p]).is_err());
    }
}
