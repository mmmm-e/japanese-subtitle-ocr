#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use eframe::egui;
use japanese_subtitle_ocr::guide::{self, Topic};
use japanese_subtitle_ocr::playback::{subtitle_text_lane, PlaybackEvent, Player};
use japanese_subtitle_ocr::*;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread,
};

enum Job {
    Noop,
    Open(PathBuf, Settings),
    Init(Settings),
    Test(Video, f64, Region, Settings),
    Run(Video, Vec<RegionTrack>, Settings),
    Export(PathBuf, Vec<Segment>),
    Import(PathBuf),
    SaveSettings(Settings),
    Merge(Vec<Segment>),
    MergeSelected(Vec<Segment>, Vec<usize>),
}
enum Event {
    Video(Video),
    Image(image::DynamicImage),
    Info(String),
    Device(String),
    Details(String),
    Progress(f32),
    Rows(Vec<Segment>),
    Error(String),
    Done,
}
fn service(rx: Receiver<Job>, tx: Sender<Event>, cancel: Arc<AtomicBool>) {
    let mut worker: Option<Worker> = None;
    let mut signature = String::new();
    for job in rx {
        let result = (|| -> anyhow::Result<()> {
            match job {
                Job::Merge(rows) => {
                    let report = temporal::merge_repeated_segments(&rows, &cancel)?;
                    tx.send(Event::Info(format!("연속 중복 정리 완료: {}개 → {}개. 후보가 다른 {}개 구간은 검토하세요. 이미지 근거를 확보하지 못한 경우는 자동 통합하지 않았습니다.",rows.len(),report.segments.len(),report.uncertain)))?;
                    tx.send(Event::Rows(report.segments))?;
                }
                Job::MergeSelected(rows, selected) => {
                    let result = temporal::merge_selected_segments(&rows, &selected)?;
                    tx.send(Event::Info("선택한 행을 합쳤습니다. 문구가 다르면 인식 후보에서 대표 문구를 고르거나 직접 수정하세요. 되돌리기도 가능합니다.".into()))?;
                    tx.send(Event::Rows(result))?;
                }
                Job::SaveSettings(s) => {
                    std::fs::write("settings.json", serde_json::to_vec_pretty(&s)?)?;
                    tx.send(Event::Info(
                        "설정을 저장했습니다. 다음 실행에도 유지됩니다.".into(),
                    ))?;
                }
                Job::Noop => {
                    tx.send(Event::Info("준비됨".into()))?;
                }
                Job::Open(path, s) => {
                    let v = probe(&path, &s, &cancel)?;
                    let img = preview(&v, 0., &s, &cancel)?;
                    tx.send(Event::Video(v))?;
                    tx.send(Event::Image(img))?;
                    tx.send(Event::Info(
                        "영상을 열었습니다. 재생을 일시정지하고 초록 자막 영역을 지정한 뒤 시험 인식하세요.".into(),
                    ))?;
                }
                Job::Import(path) => {
                    let rows = load_results(&path)?;
                    tx.send(Event::Rows(rows))?;
                    tx.send(Event::Info(
                        "결과를 불러왔습니다. 재생하면 추출 자막을 함께 확인할 수 있습니다.".into(),
                    ))?;
                }
                Job::Export(path, rows) => {
                    export(&path, &rows)?;
                    tx.send(Event::Info(format!("저장했습니다: {}", path.display())))?;
                }
                other => {
                    let s = match &other {
                        Job::Init(s) | Job::Test(_, _, _, s) | Job::Run(_, _, s) => s,
                        _ => unreachable!(),
                    };
                    let sig = s.worker_signature();
                    if worker.is_none() || signature != sig {
                        tx.send(Event::Info(
                            "모델 준비 중… 처음에는 다운로드에 몇 분 이상 걸릴 수 있습니다.".into(),
                        ))?;
                        worker = None;
                        worker = Some(Worker::start(s, &cancel)?);
                        signature = sig;
                    }
                    let w = worker.as_mut().unwrap();
                    tx.send(Event::Device(format!(
                        "실제 장치: {} | {}\nPyTorch {} / CUDA 빌드 {}{}",
                        w.info["device"].as_str().unwrap_or("확인 중"),
                        w.info["gpu"].as_str().unwrap_or("확인 중"),
                        w.info["torch_version"].as_str().unwrap_or("확인 중"),
                        w.info["torch_cuda"].as_str().unwrap_or("없음"),
                        if w.info["device"].as_str().unwrap_or("").starts_with("cpu") {
                            "\nCPU로 처리합니다. CUDA를 사용할 수 없거나 CPU를 선택한 상태입니다."
                        } else {
                            ""
                        }
                    )))?;
                    tx.send(Event::Details(serde_json::to_string_pretty(&w.info)?))?;
                    tx.send(Event::Info(format!(
                        "{} 준비 완료. 현재 영역을 먼저 시험 인식하세요.",
                        model_label(&s.model)
                    )))?;
                    match other {
                        Job::Init(_) => {}
                        Job::Test(v, t, r, s) => {
                            let img = preview(&v, t, &s, &cancel)?;
                            let crop = preprocess(r.crop(&img)?, &s);
                            let dir = video_cache(&v)?;
                            let p = dir.join("test-crop.png");
                            crop.save(&p)?;
                            let result = w.recognize(&p, &cancel)?;
                            tx.send(Event::Info(format!(
                                "시험 인식 결과\n{}\n한 장 처리: {:.0}ms | 장치 {} | 모델 커밋 {}",
                                result.text, result.elapsed_ms, result.device, result.revision
                            )))?;
                        }
                        Job::Run(v, r, s) => {
                            tx.send(Event::Info(
                                "검사할 프레임을 준비하는 중입니다. 취소할 수 있으며 아직 인식 진행률은 0%일 수 있습니다.".into(),
                            ))?;
                            let frames = sample(&v, &s, &cancel)?;
                            tx.send(Event::Info(format!(
                                "{}장 인식 중 | 장치 {} | GPU {}",
                                frames.len(),
                                w.info["device"].as_str().unwrap_or("확인 중"),
                                w.info["gpu"].as_str().unwrap_or("확인 중")
                            )))?;
                            let mut last = std::time::Instant::now();
                            let rows =
                                pipeline_tracks(&v, &frames, &r, &s, w, &cancel, |p, rows| {
                                    if last.elapsed().as_millis() > 300 || p >= 1. {
                                        let _ = tx.send(Event::Progress(p));
                                        let _ = tx.send(Event::Rows(rows.to_vec()));
                                        last = std::time::Instant::now();
                                    }
                                })?;
                            tx.send(Event::Info(format!(
                                "추출 완료: {}개 구간입니다. 재생하며 검토한 뒤 파일로 저장하세요.",
                                rows.len()
                            )))?;
                            tx.send(Event::Rows(rows))?;
                        }
                        _ => unreachable!(),
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            worker = None;
            let _ = tx.send(Event::Error(format!("{e:#}")));
        }
        let _ = tx.send(Event::Done);
    }
}
struct Studio {
    settings: Settings,
    video: Option<Video>,
    texture: Option<egui::TextureHandle>,
    region: Region,
    other_region: Region,
    dual_regions: bool,
    selected_region: usize,
    time: f64,
    rows: Vec<Segment>,
    status: String,
    device_info: String,
    busy: bool,
    progress: f32,
    tx: Sender<Job>,
    rx: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    drag: Option<(egui::Pos2, Region, u8)>,
    export_format: usize,
    diagnostic: Option<u32>,
    player: Player,
    pending_results: Option<PathBuf>,
    playing: bool,
    player_loaded: bool,
    scrubbing: bool,
    show_subtitles: bool,
    volume: f32,
    playback_status: String,
    guide_open: bool,
    guide_topic: Topic,
    show_setting_help: bool,
    diagnostic_details: String,
    selected_rows: BTreeSet<usize>,
    undo_rows: Option<Vec<Segment>>,
    pending_undo: Option<Vec<Segment>>,
}
impl Studio {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let mut fonts = egui::FontDefinitions::default();
        for path in [
            "C:/Windows/Fonts/YuGothR.ttc",
            "C:/Windows/Fonts/meiryo.ttc",
            "C:/Windows/Fonts/malgun.ttf",
        ] {
            if let Ok(data) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert(path.into(), egui::FontData::from_owned(data).into());
                fonts
                    .families
                    .get_mut(&egui::FontFamily::Proportional)
                    .unwrap()
                    .push(path.into());
            }
        }
        cc.egui_ctx.set_fonts(fonts);
        let settings = std::fs::read("settings.json")
            .ok()
            .and_then(|b| Settings::from_json_bytes(&b).ok())
            .unwrap_or_default();
        let (tx, jobs) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        thread::spawn(move || service(jobs, events, c));
        let mut app = Self {
            settings,
            video: None,
            texture: None,
            region: Region::default(),
            other_region: Region::default(),
            dual_regions: false,
            selected_region: 0,
            time: 0.,
            rows: vec![],
            device_info: "모델 준비 전 — 아래에서 모델 준비 / GPU 확인을 누르세요.".into(),
            status:
                "MP4 / MKV / MOV 영상을 열거나 끌어오세요. 처음이라면 상단 사용 안내를 확인하세요."
                    .into(),
            busy: false,
            progress: 0.,
            tx,
            rx,
            cancel,
            drag: None,
            export_format: 0,
            player: Player::new(),
            pending_results: None,
            playing: false,
            player_loaded: false,
            scrubbing: false,
            show_subtitles: true,
            volume: 0.8,
            playback_status: "준비됨".into(),
            guide_open: false,
            guide_topic: Topic::default(),
            show_setting_help: true,
            diagnostic_details: String::new(),
            selected_rows: BTreeSet::new(),
            undo_rows: None,
            pending_undo: None,
            diagnostic: std::env::args().any(|a| a == "--diagnostic").then_some(0),
        };
        let mut args = std::env::args_os().skip(1);
        let mut video = None;
        while let Some(arg) = args.next() {
            if arg == "--results" {
                app.pending_results = args.next().map(PathBuf::from);
            } else if arg == "--dual" {
                app.set_dual(true);
            } else if arg == "--guide-settings" {
                app.guide_open = true;
                app.guide_topic = Topic::Settings;
            } else if arg == "--guide" {
                app.guide_open = true;
            } else if arg != "--diagnostic" {
                video = Some(PathBuf::from(arg));
            }
        }
        if let Some(path) = video {
            app.submit(Job::Open(path, app.settings.clone()));
        }
        app
    }
    fn set_dual(&mut self, enabled: bool) {
        self.dual_regions = enabled;
        self.selected_region = 0;
        let tracks = RegionTrack::split();
        self.region = if enabled {
            tracks[0].region
        } else {
            Region::default()
        };
        self.other_region = tracks[1].region;
        self.drag = None;
        if enabled {
            self.export_format = 3;
        }
    }
    fn tracks(&self) -> Vec<RegionTrack> {
        if !self.dual_regions {
            return vec![RegionTrack {
                lane: SubtitleLane::Single,
                region: self.region,
            }];
        }
        let (left, right) = if self.selected_region == 0 {
            (self.region, self.other_region)
        } else {
            (self.other_region, self.region)
        };
        vec![
            RegionTrack {
                lane: SubtitleLane::Left,
                region: left,
            },
            RegionTrack {
                lane: SubtitleLane::Right,
                region: right,
            },
        ]
    }
    fn regions_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(!self.busy && !self.playing, |ui| {
            let mut dual = self.dual_regions;
            if ui
                .checkbox(&mut dual, "왼쪽·오른쪽 자막 따로 인식")
                .on_hover_text("켜면 두 영역을 각각 지정합니다. 영역 모드를 전환하면 기본 위치로 초기화됩니다.")
                .changed()
            {
                self.set_dual(dual);
            }
            if self.dual_regions {
                let mut selected = self.selected_region;
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut selected, 0, "왼쪽 영역 · 초록");
                    ui.selectable_value(&mut selected, 1, "오른쪽 영역 · 파랑");
                });
                if selected != self.selected_region {
                    std::mem::swap(&mut self.region, &mut self.other_region);
                    self.selected_region = selected;
                    self.drag = None;
                }
                ui.small(
                    "선택한 영역을 영상에서 드래그하세요. 시험 인식은 선택한 쪽만, 전체 추출은 양쪽을 처리합니다. 저장 위치: 왼쪽 아래 / 오른쪽 아래.",
                );
            }
        });
    }
    fn set_image(&mut self, ctx: &egui::Context, rgb: image::RgbaImage) {
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [rgb.width() as usize, rgb.height() as usize],
            rgb.as_raw(),
        );
        if let Some(texture) = self.texture.as_mut() {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture = Some(ctx.load_texture("video", image, egui::TextureOptions::LINEAR));
        }
    }
    fn seek_to(&mut self, time: f64, play: bool) {
        if let Some(video) = self.video.clone() {
            self.time = time.clamp(0., (video.duration - 0.001).max(0.));
            self.playing = play;
            self.player_loaded = true;
            self.player
                .load(video, self.settings.clone(), self.time, !play, self.volume);
            self.playback_status = "재생할 장면을 준비하는 중…".into();
        }
    }
    fn stop_playback(&mut self) {
        self.player.stop();
        self.player_loaded = false;
        self.playing = false;
        self.scrubbing = false;
    }
    fn submit(&mut self, job: Job) {
        if self.busy {
            return;
        }
        if matches!(&job, Job::Open(..)) {
            self.guide_open = false;
            self.stop_playback();
        }
        if matches!(&job, Job::Open(..) | Job::Run(..) | Job::Import(..)) {
            self.undo_rows = None;
            self.pending_undo = None;
            self.selected_rows.clear();
        }
        self.cancel.store(false, Ordering::Relaxed);
        self.busy = true;
        self.status = "작업 중… 오른쪽 상태 문구를 확인하세요.".into();
        if self.tx.send(job).is_err() {
            self.status = "작업 스레드에 연결하지 못했습니다. 프로그램을 다시 실행하세요.".into();
            self.busy = false;
        }
    }
    fn actions_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("인식 설정");
        self.regions_ui(ui);
        ui.label(&self.device_info);
        ui.add_enabled_ui(!self.busy,|ui|{
            ui.horizontal_wrapped(|ui|{
                if let Some(v)=self.video.clone(){
                    if ui.button("현재 영역 시험 인식").on_hover_text("현재 시각의 자막 영역 한 장만 읽습니다. 설정 변경 후 같은 장면으로 비교하세요.").clicked(){self.submit(Job::Test(v.clone(),self.time,self.region,self.settings.clone()));}
                    if ui.button("영상 전체 자막 추출").clicked(){self.rows.clear();self.progress=0.;self.submit(Job::Run(v,self.tracks(),self.settings.clone()));}
                }
                if ui.button("설정 저장").clicked(){self.submit(Job::SaveSettings(self.settings.clone()));}
            });
        });
        if self.video.is_some() {
            ui.small("다시 추출하면 현재 결과 표가 바뀝니다. 편집본은 먼저 저장하세요.");
        } else {
            ui.small("영상을 열면 시험 인식과 전체 추출을 사용할 수 있습니다.");
        }
        egui::ScrollArea::vertical()
            .id_salt("status-scroll")
            .max_height(110.)
            .show(ui, |ui| {
                ui.label(&self.status);
            });
        ui.separator();
    }
    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.checkbox(&mut self.show_setting_help, "설정 설명 함께 보기")
            .on_hover_text(
                "짧은 설명을 표시합니다. 각 항목의 자세한 설명은 항상 펼칠 수 있습니다.",
            );

        ui.separator();
        ui.add_enabled_ui(!self.busy,|ui| {
            egui::ComboBox::from_label("OCR 모델").selected_text(model_label(&self.settings.model)).show_ui(ui,|ui| {
                for id in [PADDLE_MODEL,MODEL] {
                    if ui.selectable_value(&mut self.settings.model,id.into(),model_label(id)).changed(){
                        self.settings.revision="main".into();
                        self.device_info="선택 모델을 바꿨습니다. 모델 준비 버튼으로 실제 장치를 확인하세요.".into();
                    }
                }
            });
            if self.show_setting_help {ui.small(if self.settings.model==PADDLE_MODEL {"컬러·여러 줄 영역을 시험할 수 있습니다. CPT보다 느릴 수 있으며 결과 검토가 필요합니다."}else{"가벼운 일본어 인식 모델입니다. 한 줄에 가까운 영역으로 시험하세요. 내부에서 회색조로 변환합니다."});}
            egui::ComboBox::from_label("처리 장치").selected_text(match self.settings.device.as_str(){"cuda"=>"NVIDIA GPU (CUDA)","cpu"=>"CPU",_=>"자동 · GPU 우선"}).show_ui(ui,|ui| {
                for (id,label) in [("auto","자동 · GPU 우선"),("cuda","NVIDIA GPU (CUDA)"),("cpu","CPU")] {ui.selectable_value(&mut self.settings.device,id.into(),label);}
            });
            if self.show_setting_help {ui.small("자동은 CUDA가 가능하면 GPU, 아니면 CPU를 사용합니다. CPU는 느릴 수 있습니다. 위의 실제 장치 표시를 확인하세요.");}
            if ui.button("모델 준비 / GPU 확인").clicked(){self.submit(Job::Init(self.settings.clone()));}
            ui.separator();
            ui.strong("검사할 장면 수");
            egui::ComboBox::from_label("샘플링 FPS").selected_text(format!("{}FPS · 1초에 약 {}장",self.settings.fps,self.settings.fps)).show_ui(ui,|ui| {
                for fps in [1,2,3,5] {ui.selectable_value(&mut self.settings.fps,fps,format!("{fps}FPS · 1초에 약 {fps}장"));}
            });
            guide::hint(ui,"fps",self.show_setting_help);
            if let Some(v)=&self.video {ui.small(format!("현재 영상 {:.1}초 → 약 {}장 검사 예정 (영상 시간 정보에 따라 달라짐)",v.duration,(v.duration*self.settings.fps as f64).ceil() as u64));}
            if self.settings.model==PADDLE_MODEL {
                ui.separator();ui.strong("Paddle 모델 세부 설정");
                ui.label(format!("입력 면적 상한 · 약 {:.2}MP",self.settings.vl_max_pixels as f64/1_000_000.));
                ui.add(egui::Slider::new(&mut self.settings.vl_max_pixels,65536..=1605632).text("픽셀 면적"));
                ui.horizontal_wrapped(|ui| {for (value,label) in [(501760,"0.5MP 절약"),(1003520,"1.0MP 기본"),(1605632,"1.6MP 세밀")]{if ui.small_button(label).clicked(){self.settings.vl_max_pixels=value;}}});
                guide::hint(ui,"pixels",self.show_setting_help);
                ui.add_space(6.);
                ui.add(egui::Slider::new(&mut self.settings.vl_max_tokens,16..=1024).text("출력 상한 (토큰)"));
                guide::hint(ui,"tokens",self.show_setting_help);
                if ui.button("Paddle 권장 시작값 적용").on_hover_text("1FPS / 입력 1.0MP / 출력 512토큰 / 전처리 끔. 모델·장치·실행 경로는 바꾸지 않습니다.").clicked(){
                    self.settings.fps=1;self.settings.vl_max_pixels=1003520;self.settings.vl_max_tokens=512;
                    self.settings.upscale=false;self.settings.gray=false;self.settings.contrast=0.;self.settings.sharpen=false;self.settings.fast=false;
                    self.status="권장 시작값을 적용했습니다. 같은 장면에서 시험 인식하고, 유지하려면 설정 저장을 누르세요.".into();
                }
            }
            ui.separator();ui.strong("이미지 전처리");
            ui.small("기본은 원본 컬러입니다. 한 항목씩 바꾸며 같은 장면을 시험하세요.");
            ui.checkbox(&mut self.settings.upscale,"이미지 2배 확대");guide::hint(ui,"upscale",self.show_setting_help);
            ui.checkbox(&mut self.settings.gray,"흑백(회색조) 변환");guide::hint(ui,"gray",self.show_setting_help);
            ui.checkbox(&mut self.settings.sharpen,"선명하게 처리");guide::hint(ui,"sharpen",self.show_setting_help);
            ui.add(egui::Slider::new(&mut self.settings.contrast,-50.0..=100.0).text("대비 · 0은 원본"));guide::hint(ui,"contrast",self.show_setting_help);
            if ui.small_button("전처리를 원본으로 되돌리기").clicked(){self.settings.upscale=false;self.settings.gray=false;self.settings.sharpen=false;self.settings.contrast=0.;}
            ui.separator();
            ui.checkbox(&mut self.settings.merge_similar,"연속된 같은 자막 통합");guide::hint(ui,"merge",self.show_setting_help);
            ui.checkbox(&mut self.settings.fast,"완전히 같은 이미지 바로 재사용");guide::hint(ui,"reuse",self.show_setting_help);
            ui.collapsing("고급 설정 · 모델 버전 / 실행 경로",|ui| {
                ui.label("모델 버전 (보통 main 유지)");ui.text_edit_singleline(&mut self.settings.revision);
                ui.small("특정 모델 커밋으로 고정할 때만 바꾸세요. 숫자가 크다고 정확도가 오르는 값이 아닙니다.");
                for (label,value) in [("Python · CPT용",&mut self.settings.python),("Python · Paddle용",&mut self.settings.vl_python),("OCR 워커 파일",&mut self.settings.worker),("FFmpeg 실행 파일",&mut self.settings.ffmpeg),("FFprobe 실행 파일",&mut self.settings.ffprobe)]{ui.label(label);ui.text_edit_singleline(value);}
                ui.small("CPT는 .venv, Paddle은 .venv-vl을 사용합니다. 경로에 따옴표나 실행 인수를 붙이지 마세요. 정상 실행된다면 바꾸지 않아도 됩니다.");
            });
        });
        ui.separator();

        if !self.diagnostic_details.is_empty() {
            ui.collapsing("오류·진단 원문", |ui| {
                ui.label("문제 해결을 위한 기술 정보입니다.");
                ui.label(&self.diagnostic_details);
            });
        }
        if ui.button("설정 설명 / 사용 안내 열기").clicked() {
            self.guide_open = true;
            self.guide_topic = Topic::Settings;
        }
    }
    fn results_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.heading(format!("추출 결과 {}개", self.rows.len()));
            if ui
                .add_enabled(
                    !self.busy && self.video.is_some(),
                    egui::Button::new("결과 JSON 불러오기"),
                )
                .on_hover_text("이 앱에서 저장한 JSON 결과를 불러와 현재 영상과 함께 검토합니다.")
                .clicked()
            {
                self.undo_rows = None;
                self.pending_undo = None;
                self.selected_rows.clear();
                let tx = self.tx.clone();
                self.busy = true;
                thread::spawn(move || {
                    let job = rfd::FileDialog::new()
                        .set_title("이 영상의 OCR 결과 JSON 선택")
                        .add_filter("OCR 결과 JSON", &["json"])
                        .pick_file()
                        .map_or(Job::Noop, Job::Import);
                    let _ = tx.send(job);
                });
            }
            egui::ComboBox::from_id_salt("format")
                .selected_text(
                    [
                        "SRT · 시간 포함 자막",
                        "TXT · 글만",
                        "JSON · 다시 검토할 결과",
                        "ASS · 좌우 위치 포함",
                    ][self.export_format],
                )
                .show_ui(ui, |ui| {
                    for (i, label) in [
                        "SRT · 시간 포함 자막",
                        "TXT · 글만",
                        "JSON · 다시 검토할 결과",
                        "ASS · 좌우 위치 포함",
                    ]
                    .iter()
                    .enumerate()
                    {
                        ui.selectable_value(&mut self.export_format, i, *label);
                    }
                });
            if ui
                .add_enabled(
                    !self.busy && !self.rows.is_empty(),
                    egui::Button::new("파일로 저장"),
                )
                .clicked()
            {
                let tx = self.tx.clone();
                let rows = self.rows.clone();
                let ext = ["srt", "txt", "json", "ass"][self.export_format];
                self.busy = true;
                self.cancel.store(false, Ordering::Relaxed);
                thread::spawn(move || {
                    let job = if let Some(p) = rfd::FileDialog::new()
                        .set_title("추출한 자막 저장")
                        .add_filter(ext, &[ext])
                        .set_file_name(format!("subtitles.{ext}"))
                        .save_file()
                    {
                        Job::Export(p, rows)
                    } else {
                        Job::Noop
                    };
                    let _ = tx.send(job);
                });
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(!self.busy&&self.rows.len()>1,egui::Button::new("현재 결과 중복 정리")).on_hover_text("시간이 이어지고 문구와 자막 이미지가 일치하는 근거가 충분한 항목을 보수적으로 정리합니다.").clicked(){
                self.pending_undo=Some(self.rows.clone());self.submit(Job::Merge(self.rows.clone()));self.status="캐시 이미지로 연속 중복을 정리하는 중…".into();
            }

            if ui.add_enabled(!self.busy&&self.selected_rows.len()>=2,egui::Button::new(format!("선택한 {}개 행 합치기",self.selected_rows.len()))).on_hover_text("같은 영역의 시간상 이어지는 행을 고르세요. 좌우 자막은 서로 합치지 않습니다. 가장 자주 나온 문구를 대표로 선택하고 다른 인식 후보도 보존합니다.").clicked(){
                self.pending_undo=Some(self.rows.clone());self.submit(Job::MergeSelected(self.rows.clone(),self.selected_rows.iter().copied().collect()));
            }
            if ui.add_enabled(!self.busy&&self.undo_rows.is_some(),egui::Button::new("중복 정리 되돌리기")).clicked(){
                self.rows=self.undo_rows.take().unwrap();self.selected_rows.clear();self.status="중복 정리 전 결과로 되돌렸습니다.".into();
            }
        });
        if self.rows.iter().any(|r| r.lane != SubtitleLane::Single) {
            ui.small("좌우 위치를 유지하려면 ASS로 저장하세요. JSON에는 영역과 인식 후보가 보존됩니다. SRT/TXT는 좌우 배치를 저장하지 않습니다.");
        }
        ui.small("문구·시간은 직접 수정할 수 있습니다. 검토 필요 체크는 저장을 막지 않습니다. 모델 이름 위에 마우스를 올리면 근거 정보를 볼 수 있습니다.");
        egui::ScrollArea::both().id_salt("results-scroll").auto_shrink([false,false]).show(ui,|ui|{
            ui.add_enabled_ui(!self.busy,|ui|{
                let mut inspect=None;
                egui::Grid::new("rows").num_columns(8).striped(true).spacing(egui::vec2(12.,8.)).show(ui,|ui|{
                    for h in ["선택","영역","시작(초)","종료(초)","일본어 자막 · 직접 수정","검토 필요","모델 / 근거 정보","원본 확인"]{ui.strong(h);}ui.end_row();
                    for (index,row) in self.rows.iter_mut().enumerate() {
                        let mut selected=self.selected_rows.contains(&index);
                        if ui.checkbox(&mut selected,"").changed(){if selected{self.selected_rows.insert(index);}else{self.selected_rows.remove(&index);}}
                        ui.label(row.lane.label());
                        ui.add(egui::DragValue::new(&mut row.start).speed(0.01).range(0.0..=864000.));
                        ui.add(egui::DragValue::new(&mut row.end).speed(0.01).range(0.0..=864000.));
                        ui.vertical(|ui| {
                            let before=row.text.clone();
                            if ui.add(egui::TextEdit::multiline(&mut row.text).desired_width(360.).min_size(egui::vec2(360.,36.)).desired_rows(2)).changed(){temporal::preserve_edit(row,&before);}
                            let missing=ui.fonts(|fonts|row.text.chars().filter(|c|!c.is_control()&&!fonts.has_glyph(&egui::FontId::proportional(14.),*c)).take(4).map(|c|format!("U+{:04X}",c as u32)).collect::<Vec<_>>());
                            if !missing.is_empty(){ui.colored_label(egui::Color32::YELLOW,format!("현재 폰트로 표시할 수 없는 문자: {}",missing.join(", "))).on_hover_text("네모 표시가 OCR 오류가 아니라 폰트 문제일 수 있습니다. 원본 문자열과 코드는 보존됩니다.");}
                            if row.variants.len()>1 {
                                ui.menu_button(format!("통합된 인식 후보 {}개",row.variants.len()),|ui|{
                                    ui.set_max_width(480.);
                                    ui.label("원본에서 나온 후보입니다. 선택하면 대표 문구가 바뀝니다.");
                                    for candidate in &row.variants {
                                        let source=if candidate.edited{"수정본".into()}else{format!("{}회 관찰",candidate.observations)};
                                        if ui.selectable_label(row.text==candidate.text,format!("{source} · {}",candidate.text)).clicked(){row.text=candidate.text.clone();row.edited=true;ui.close_menu();}
                                    }
                                });
                            }
                        });
                        ui.checkbox(&mut row.review,"");
                        ui.colored_label(if row.start<=self.time&&self.time<row.end{egui::Color32::LIGHT_GREEN}else{egui::Color32::GRAY},row.model.rsplit('/').next().unwrap_or(&row.model))
                            .on_hover_text(format!("모델: {}\n실제 커밋: {}\n최초 한 장 처리: {:.0}ms\n신뢰도: {}\n같은 자막 관찰: {}회\n잘린 이미지: {}",row.model,row.revision,row.elapsed_ms,row.confidence.map(|c|c.to_string()).unwrap_or_else(||"미제공".into()),row.observations,row.crop.display()));
                        if ui.button("해당 장면").clicked(){inspect=Some(row.start);}
                        ui.end_row();
                    }
                });
                if let Some(t)=inspect{self.seek_to(t,false);}
            });
        });
    }
    fn choose_open(&mut self) {
        self.stop_playback();
        let tx = self.tx.clone();
        let s = self.settings.clone();
        self.busy = true;
        self.cancel.store(false, Ordering::Relaxed);
        thread::spawn(move || {
            let path = rfd::FileDialog::new()
                .add_filter("영상", &["mp4", "mkv", "mov"])
                .pick_file();
            if let Some(p) = path {
                let _ = tx.send(Job::Open(p, s));
            } else {
                let _ = tx.send(Job::Noop);
            }
        });
    }
}
impl eframe::App for Studio {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(frames) = self
            .diagnostic
            .as_mut()
            .filter(|_| !self.busy && self.pending_results.is_none())
        {
            *frames += 1;
            if *frames == 20 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            let shot = ctx.input(|i| {
                i.events.iter().find_map(|e| {
                    if let egui::Event::Screenshot { image, .. } = e {
                        Some(image.clone())
                    } else {
                        None
                    }
                })
            });
            if let Some(shot) = shot {
                thread::spawn(move || {
                    let bytes: Vec<u8> = shot.pixels.iter().flat_map(|p| p.to_array()).collect();
                    if let Some(img) =
                        image::RgbaImage::from_raw(shot.size[0] as u32, shot.size[1] as u32, bytes)
                    {
                        let _ = std::fs::create_dir_all(".cache");
                        let _ = img.save(".cache/gui-diagnostic.png");
                    }
                });
                self.diagnostic = None;
            }
        }
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Video(v) => {
                    self.undo_rows = None;
                    self.pending_undo = None;
                    self.selected_rows.clear();
                    self.video = Some(v);
                    self.time = 0.;
                    self.rows.clear();
                    self.progress = 0.;
                    self.set_dual(self.dual_regions);
                }
                Event::Image(img) => self.set_image(ctx, img.to_rgba8()),
                Event::Info(s) => self.status = s,
                Event::Device(s) => self.device_info = s,
                Event::Details(s) => self.diagnostic_details = s,
                Event::Error(s) => {
                    self.pending_undo = None;
                    self.diagnostic_details = s;
                    self.status =
                        "작업을 완료하지 못했습니다. 아래 오류·진단 원문에서 원인을 확인하세요."
                            .into();
                    self.device_info =
                        "OCR 작업이 종료됐습니다. 모델을 다시 준비해 재시도할 수 있습니다.".into();
                }
                Event::Progress(p) => self.progress = p,
                Event::Rows(rows) => {
                    if let Some(before) = self.pending_undo.take() {
                        self.undo_rows = Some(before);
                    }
                    if rows.iter().any(|r| r.lane != SubtitleLane::Single) {
                        if !self.dual_regions {
                            self.set_dual(true);
                        }
                        self.export_format = 3;
                    }
                    self.rows = rows;
                    self.selected_rows.clear();
                }
                Event::Done => self.busy = false,
            }
        }
        if !self.busy {
            if let Some(path) = self.pending_results.take() {
                self.submit(Job::Import(path));
            }
        }
        let (frame, events) = self.player.poll();
        if let Some(frame) = frame {
            if !self.scrubbing {
                self.time = frame.time;
                self.set_image(ctx, frame.image);
            }
        }
        for event in events {
            match event {
                PlaybackEvent::Ready { audio } => {
                    self.playback_status = if audio {
                        "영상과 소리 재생 준비됨"
                    } else {
                        "무음 영상 재생 — 오디오 트랙이나 출력 장치가 없습니다."
                    }
                    .into()
                }
                PlaybackEvent::Notice(s) => {
                    self.playback_status =
                        "소리를 재생하지 못해 무음으로 진행합니다. 오류·진단 원문을 확인하세요."
                            .into();
                    self.diagnostic_details = s;
                }
                PlaybackEvent::Error(s) => {
                    self.player_loaded = false;
                    self.playing = false;
                    self.playback_status =
                        "영상을 재생하지 못했습니다. 오류·진단 원문을 확인하세요.".into();
                    self.diagnostic_details = s;
                }
                PlaybackEvent::Ended => {
                    self.player_loaded = false;
                    self.playing = false;
                    if let Some(v) = &self.video {
                        self.time = v.duration;
                    }
                    self.playback_status =
                        "영상이 끝났습니다. 재생을 누르면 처음부터 재생합니다.".into();
                }
            }
        }
        if !self.busy {
            if let Some(path) =
                ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()))
            {
                self.submit(Job::Open(path, self.settings.clone()));
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F1)) {
            self.guide_open = !self.guide_open;
        }
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("일본어 영상 자막 OCR Studio");
                ui.selectable_value(&mut self.guide_open, false, "작업 화면");
                ui.selectable_value(&mut self.guide_open, true, "사용 안내 · F1");
                ui.label(model_label(&self.settings.model));
                if ui
                    .add_enabled(!self.busy, egui::Button::new("영상 열기"))
                    .clicked()
                {
                    self.choose_open();
                }
                if self.busy && ui.button("작업 취소").clicked() {
                    self.cancel.store(true, Ordering::Relaxed);
                }
                ui.add(
                    egui::ProgressBar::new(self.progress)
                        .desired_width(180.)
                        .text(format!("자막 인식 {:.0}%", self.progress * 100.)),
                );
            });
        });
        if self.guide_open {
            guide::page(ctx, &mut self.guide_topic, &self.settings);
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        }
        egui::SidePanel::right("settings")
            .default_width(365.)
            .resizable(true)
            .show(ctx, |ui| {
                self.actions_ui(ui);
                egui::ScrollArea::vertical()
                    .id_salt("settings-scroll")
                    .show(ui, |ui| self.settings_ui(ui));
            });
        egui::TopBottomPanel::bottom("results")
            .default_height(260.)
            .resizable(true)
            .show(ctx, |ui| self.results_ui(ui));
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(v) = self.video.clone() {
                ui.label(format!(
                    "{} | {} × {} | {:.2}초",
                    v.path.display(),
                    v.width,
                    v.height,
                    v.duration
                ));
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button(if self.playing { "일시정지" } else { "재생" })
                        .clicked()
                    {
                        if self.playing {
                            self.player.pause(true);
                            self.playing = false;
                        } else {
                            let t = if self.time >= v.duration {
                                0.
                            } else {
                                self.time
                            };
                            if self.player_loaded {
                                self.player.pause(false);
                                self.playing = true;
                            } else {
                                self.seek_to(t, true);
                            }
                        }
                    }
                    if ui.button("정지").clicked() {
                        self.seek_to(0., false);
                    }
                    if ui.button("-5초").clicked() {
                        self.seek_to(self.time - 5., self.playing);
                    }
                    if ui.button("+5초").clicked() {
                        self.seek_to(self.time + 5., self.playing);
                    }
                    ui.label(format!(
                        "{} / {}",
                        timestamp(self.time).replace(',', "."),
                        timestamp(v.duration).replace(',', ".")
                    ));
                    ui.checkbox(&mut self.show_subtitles, "추출 자막 표시");
                    if ui
                        .add(egui::Slider::new(&mut self.volume, 0.0..=1.0).text("음량"))
                        .changed()
                    {
                        self.player.volume(self.volume);
                    }
                });
                let mut target = self.time;
                let timeline = ui
                    .scope(|ui| {
                        ui.spacing_mut().slider_width = (ui.available_width() - 180.).max(100.);
                        ui.add(
                            egui::Slider::new(&mut target, 0.0..=v.duration)
                                .text("재생 위치 (초)"),
                        )
                    })
                    .inner;
                if timeline.drag_started() {
                    self.scrubbing = true;
                    self.player.pause(true);
                }
                if timeline.changed() {
                    self.time = target;
                }
                if timeline.drag_stopped() || timeline.changed() && !timeline.dragged() {
                    self.scrubbing = false;
                    self.seek_to(target, self.playing);
                }
                ui.small(&self.playback_status);
                if let Some(tex) = &self.texture {
                    let available = ui.available_size();
                    let aspect = tex.size_vec2();
                    let scale = (available.x / aspect.x)
                        .min((available.y - 60.).max(1.) / aspect.y)
                        .max(0.001);
                    let size = aspect * scale;
                    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::drag());
                    ui.painter().image(
                        tex.id(),
                        rect,
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1., 1.)),
                        egui::Color32::WHITE,
                    );
                    if self.show_subtitles {
                        for lane in [SubtitleLane::Single,SubtitleLane::Left,SubtitleLane::Right] {
                            let text=subtitle_text_lane(&self.rows,self.time,lane);
                            if text.is_empty(){continue;}
                            let bounds=match lane {
                                SubtitleLane::Single=>rect,
                                SubtitleLane::Left=>egui::Rect::from_min_max(rect.min,egui::pos2(rect.center().x,rect.bottom())),
                                SubtitleLane::Right=>egui::Rect::from_min_max(egui::pos2(rect.center().x,rect.top()),rect.max),
                            };
                            let font=egui::FontId::proportional((rect.width()/45.).clamp(12.,26.));
                            let painter=ui.painter().with_clip_rect(bounds);
                            let galley=painter.layout(text,font,egui::Color32::WHITE,(bounds.width()-24.).max(1.));
                            let pos=egui::pos2(bounds.center().x-galley.size().x/2.,(rect.bottom()-galley.size().y-12.).max(rect.top()));
                            painter.rect_filled(egui::Rect::from_min_size(pos,galley.size()).expand(4.),4.,egui::Color32::from_black_alpha(210));
                            painter.galley(pos,galley,egui::Color32::WHITE);
                        }
                    }
                    if self.dual_regions {
                        let other=egui::Rect::from_min_size(egui::pos2(rect.left()+self.other_region.x*rect.width(),rect.top()+self.other_region.y*rect.height()),egui::vec2(self.other_region.w*rect.width(),self.other_region.h*rect.height()));
                        let color=if self.selected_region==0 {egui::Color32::LIGHT_BLUE}else{egui::Color32::LIGHT_GREEN};
                        ui.painter().rect_stroke(other,0.,egui::Stroke::new(1_f32,color.gamma_multiply(0.65)),egui::StrokeKind::Inside);
                        ui.painter().text(other.min,egui::Align2::LEFT_TOP,if self.selected_region==0 {"오른쪽"}else{"왼쪽"},egui::FontId::proportional(13.),color);
                    }
                    let active_color=if self.dual_regions && self.selected_region==1 {egui::Color32::LIGHT_BLUE}else{egui::Color32::LIGHT_GREEN};
                    let r = egui::Rect::from_min_size(
                        egui::pos2(
                            rect.left() + self.region.x * rect.width(),
                            rect.top() + self.region.y * rect.height(),
                        ),
                        egui::vec2(self.region.w * rect.width(), self.region.h * rect.height()),
                    );
                    ui.painter().rect_stroke(
                        r,
                        0.,
                        egui::Stroke::new(2_f32, active_color),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter()
                        .circle_filled(r.max, 5., active_color);
                    if self.dual_regions {ui.painter().text(r.min,egui::Align2::LEFT_TOP,if self.selected_region==0 {"왼쪽 · 선택됨"}else{"오른쪽 · 선택됨"},egui::FontId::proportional(13.),active_color);}
                    if !self.busy && !self.playing {
                        if response.drag_started() {
                            if let Some(p) = response.interact_pointer_pos() {
                                let mode = if p.distance(r.max) < 16. {
                                    2
                                } else if r.contains(p) {
                                    1
                                } else {
                                    0
                                };
                                self.drag = Some((p, self.region, mode));
                            }
                        }
                        if response.dragged() {
                            if let (Some((origin, old, mode)), Some(p)) =
                                (self.drag, response.interact_pointer_pos())
                            {
                                let p = p.clamp(rect.min, rect.max);
                                let dx = (p.x - origin.x) / rect.width();
                                let dy = (p.y - origin.y) / rect.height();
                                self.region = match mode {
                                    1 => Region {
                                        x: (old.x + dx).clamp(0., 1. - old.w),
                                        y: (old.y + dy).clamp(0., 1. - old.h),
                                        ..old
                                    },
                                    2 => Region {
                                        w: (old.w + dx).clamp(0.005, 1. - old.x),
                                        h: (old.h + dy).clamp(0.005, 1. - old.y),
                                        ..old
                                    },
                                    _ => Region {
                                        x: ((p.x.min(origin.x) - rect.left()) / rect.width())
                                            .clamp(0., 0.995),
                                        y: ((p.y.min(origin.y) - rect.top()) / rect.height())
                                            .clamp(0., 0.995),
                                        w: dx.abs().max(0.005),
                                        h: dy.abs().max(0.005),
                                    },
                                };
                            }
                        }
                        if response.drag_stopped() {
                            self.drag = None;
                        }
                    }
                    ui.small(
                        "일시정지 후 조정: 영역 밖 드래그 → 새 지정 / 안쪽 → 이동 / 우측 하단 손잡이 → 크기 변경",
                    );
                    ui.label(format!(
                        "원본 기준 자막 영역: x={} / y={} / 가로={} / 세로={} 픽셀",
                        (self.region.x * v.width as f32) as u32,
                        (self.region.y * v.height as f32) as u32,
                        (self.region.w * v.width as f32) as u32,
                        (self.region.h * v.height as f32) as u32
                    ));
                }
            } else {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui|{ui.heading("영상을 열거나 이곳으로 끌어오세요");ui.label("처음 사용한다면 상단 사용 안내에서 순서와 설정 설명을 확인하세요.");if ui.button("사용 안내 열기").clicked(){self.guide_open=true;}});
                });
            }
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(if self.playing {
            16
        } else {
            50
        }));
    }
}
impl Drop for Studio {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
fn main() -> eframe::Result {
    // Locate the development/runtime bundle when launched from Explorer or another cwd.
    if !std::path::Path::new("worker/ocr_worker.py").exists() {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(root) = exe
                .ancestors()
                .skip(1)
                .find(|p| p.join("worker/ocr_worker.py").exists())
            {
                let _ = std::env::set_current_dir(root);
            }
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1400., 900.]),
        ..Default::default()
    };
    eframe::run_native(
        "일본어 영상 자막 OCR Studio",
        options,
        Box::new(|cc| Ok(Box::new(Studio::new(cc)))),
    )
}
