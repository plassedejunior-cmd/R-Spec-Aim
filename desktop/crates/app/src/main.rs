#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! R-Spec Aim: calibrador desktop de sensibilidade pela "primeira parada".
//!
//! Latência: a mira é desenhada direto dos counts brutos lidos pela thread de Raw Input,
//! com vsync desligado e repaint contínuo. Nada de cursor do sistema, compositor de
//! navegador ou suavização no caminho.

mod games;
mod session;
mod trial;

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke};
use games::{CUSTOM, GAMES, MOUSE_ONLY};
use rspec_analysis::stats::{aggregate, SessionStats, Summary};
use rspec_core::{units, RawSample};
use rspec_input::{CaptureStats, MouseSource, SystemMouseInfo};
use rspec_recommender::{best_round, recommend, Advice, AxisAdvice, KeepReason, Params as RecParams, RoundScore, SweepPoint};
use serde_json::json;
use trial::{Outcome, Phase, Run, TARGET_RADIUS};

const BG: Color32 = Color32::from_rgb(14, 16, 20);
const ACCENT: Color32 = Color32::from_rgb(255, 196, 0);
const COL_X: Color32 = Color32::from_rgb(90, 170, 255);
const COL_Y: Color32 = Color32::from_rgb(255, 120, 140);
const GOOD: Color32 = Color32::from_rgb(80, 210, 130);
const MUTED: Color32 = Color32::from_gray(140);

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("R-Spec Aim").with_inner_size([1280.0, 800.0]).with_maximized(true),
        vsync: false,
        ..Default::default()
    };
    eframe::run_native("R-Spec Aim", opts, Box::new(|_cc| Ok(Box::new(App::new()))))
}

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Monitor,
    Setup,
    Test,
    /// Entre blocos do teste rápido.
    SweepBreak,
    SweepResult,
    Results,
}

struct Config {
    game: usize,
    sens: f64,
    yaw: f64,
    dpi: f64,
    fov: f64,
    warmup: usize,
    trials: usize,
    feedback: bool,
    /// Resposta à pergunta obrigatória sobre ajuste separado de eixos.
    split: Option<bool>,
}

impl Config {
    fn yaw(&self) -> f64 {
        if self.game == CUSTOM {
            self.yaw
        } else {
            GAMES[self.game].yaw
        }
    }
    fn deg_per_count(&self) -> f64 {
        self.sens * self.yaw()
    }
    fn mouse_only(&self) -> bool {
        self.game == MOUSE_ONLY
    }
}

struct Round {
    /// Configuração que a pessoa usava nesta rodada (o app não muda nada durante o teste).
    dpi: f64,
    sens: f64,
    stats: SessionStats,
    saved: Option<String>,
}

impl Round {
    fn score(&self) -> RoundScore {
        RoundScore {
            median_ratio: if self.stats.all.n > 0 { self.stats.all.median } else { f64::NAN },
            throughput: self.stats.speed_accuracy.map_or(f64::NAN, |s| s.throughput),
        }
    }
}

/// Escalas do teste rápido, em relação ao movimento nativo.
const SWEEP_SCALES: [f64; 5] = [0.85, 0.92, 1.0, 1.08, 1.15];
const SWEEP_TRIALS: usize = 10;

/// Teste rápido: blocos curtos com sensibilidades diferentes para achar o ponto de
/// equilíbrio entre velocidade e precisão da pessoa, sem depender de histórico.
struct Sweep {
    /// true: a pessoa troca o DPI entre blocos e a mira fica 100% nativa.
    /// false: a escala é aplicada só dentro do app, em ordem embaralhada (às cegas).
    native: bool,
    /// Escala real de cada bloco, na ordem em que roda.
    order: Vec<f64>,
    idx: usize,
    blocks: Vec<(f64, SessionStats)>,
}

impl Sweep {
    fn new(native: bool, dpi: f64, seed: u64) -> Self {
        let mut order: Vec<f64> = if native {
            // DPI arredondado para passos de 10; a escala real é a do DPI que a pessoa vai usar
            SWEEP_SCALES.iter().map(|k| ((dpi * k) / 10.0).round() * 10.0 / dpi).collect()
        } else {
            SWEEP_SCALES.to_vec()
        };
        if !native {
            let mut rng = rspec_analysis::synthetic::XorShift::new(seed);
            for i in (1..order.len()).rev() {
                order.swap(i, rng.below(i + 1));
            }
        }
        Self { native, order, idx: 0, blocks: Vec::new() }
    }
    fn scale(&self) -> f64 {
        self.order[self.idx]
    }
}

struct App {
    src: Option<Box<dyn MouseSource>>,
    src_err: Option<String>,
    cap: Arc<CaptureStats>,
    sys: Option<SystemMouseInfo>,
    buf: Vec<RawSample>,
    recent: VecDeque<RawSample>,
    polling: Option<f64>,
    frame_ms: VecDeque<f64>,
    last_frame: f64,
    screen: Screen,
    cfg: Config,
    run: Option<Run>,
    sweep: Option<Sweep>,
    rounds: Vec<Round>,
    fullscreen: bool,
}

impl App {
    fn new() -> Self {
        let (src, src_err) = match rspec_input::open() {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some(e)),
        };
        let cap = src.as_ref().map(|s| s.stats()).unwrap_or_default();
        let sys = src.as_ref().and_then(|s| s.system_info());
        Self {
            src,
            src_err,
            cap,
            sys,
            buf: Vec::with_capacity(4096),
            recent: VecDeque::new(),
            polling: None,
            frame_ms: VecDeque::new(),
            last_frame: rspec_core::now(),
            screen: Screen::Monitor,
            cfg: Config { game: 0, sens: 1.2, yaw: 0.022, dpi: 800.0, fov: GAMES[0].fov, warmup: 3, trials: 32, feedback: false, split: None },
            run: None,
            sweep: None,
            rounds: Vec::new(),
            fullscreen: false,
        }
    }

    fn pump(&mut self, now: f64) {
        self.buf.clear();
        if let Some(src) = self.src.as_mut() {
            src.drain(&mut self.buf);
        }
        self.recent.extend(self.buf.iter().copied());
        while self.recent.front().is_some_and(|s| now - s.t > 2.0) {
            self.recent.pop_front();
        }
        let v: Vec<RawSample> = self.recent.iter().copied().collect();
        if let Some(hz) = rspec_core::estimate_polling_hz(&v, 0.02) {
            self.polling = Some(hz);
        }
        let dt = (now - self.last_frame) * 1000.0;
        self.last_frame = now;
        self.frame_ms.push_back(dt);
        if self.frame_ms.len() > 120 {
            self.frame_ms.pop_front();
        }
    }

    fn fps(&self) -> f64 {
        let avg = self.frame_ms.iter().sum::<f64>() / self.frame_ms.len().max(1) as f64;
        1000.0 / avg.max(0.01)
    }

    /// Movimento nativo: a mira anda exatamente como o cursor do Windows (counts x
    /// velocidade do ponteiro, sem aceleração), em pixels físicos. O teste nunca muda a
    /// sensibilidade da pessoa; a sugestão só aparece no resultado.
    /// Retorna pontos do egui por count (pixels físicos / pixels_per_point).
    fn ppc(&self, pixels_per_point: f64) -> (f64, f64) {
        let mult = self.sys.map_or(1.0, |s| rspec_input::pointer_multiplier(s.speed));
        let v = mult / pixels_per_point;
        (v, v)
    }

    fn start_round(&mut self, ctx: &egui::Context) {
        let r = ctx.screen_rect();
        let arena = (r.width() as f64, r.height() as f64);
        let seed = (rspec_core::now() * 1e6) as u64 ^ 0x5eed;
        let (ppc, warmup, trials) = match &self.sweep {
            // modo escala: só a mira do app muda; modo nativo: a pessoa já trocou o DPI
            Some(sw) => {
                let k = if sw.native { 1.0 } else { sw.scale() };
                let p = self.ppc(ctx.pixels_per_point() as f64);
                ((p.0 * k, p.1 * k), 1, SWEEP_TRIALS)
            }
            None => (self.ppc(ctx.pixels_per_point() as f64), self.cfg.warmup, self.cfg.trials),
        };
        self.run = Some(Run::new(arena, ppc, warmup, trials, self.cfg.feedback, seed));
        self.screen = Screen::Test;
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::Confined));
    }

    fn end_round(&mut self, ctx: &egui::Context, complete: bool) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::None));
        let Some(run) = self.run.take() else { return };
        if !complete {
            self.sweep = None;
            self.screen = Screen::Setup;
            return;
        }
        let metrics: Vec<_> = run.records.iter().filter_map(|r| r.metrics().cloned()).collect();
        let stats = aggregate(&metrics, 0xC0FFEE);
        let saved = self.save(&run, &stats).map(|p| p.display().to_string()).ok();
        if let Some(sw) = self.sweep.as_mut() {
            sw.blocks.push((sw.scale(), stats));
            sw.idx += 1;
            self.screen = if sw.idx < sw.order.len() { Screen::SweepBreak } else { Screen::SweepResult };
            return;
        }
        self.rounds.push(Round { dpi: self.cfg.dpi, sens: self.cfg.sens, stats, saved });
        self.screen = Screen::Results;
    }

    fn save(&self, run: &Run, stats: &SessionStats) -> std::io::Result<std::path::PathBuf> {
        let summ = |s: &Summary| json!({"n": s.n, "median": s.median, "q1": s.q1, "q3": s.q3, "ci": s.ci.map(|c| [c.0, c.1])});
        let sa = stats.speed_accuracy.map(|s| {
            json!({"sigma": s.sigma, "ide_bits": s.ide, "mt_mean_s": s.mt_mean, "mt_median_s": s.mt_median, "stop_mean_s": s.stop_mean, "throughput_bps": s.throughput})
        });
        let session = json!({
            "meta": session::meta(),
            "backend": self.src.as_ref().map(|s| s.backend()),
            "mode": "2d",
            "config": {
                "game": GAMES[self.cfg.game].name, "sens": self.cfg.sens, "yaw": self.cfg.yaw(),
                "dpi_nominal": self.cfg.dpi, "fov": self.cfg.fov, "mapping": "native-windows-pointer",
                "px_per_count": [run.ppc.0, run.ppc.1], "arena_px": [run.arena.0, run.arena.1],
                "feedback": run.feedback,
            },
            "device": run.device,
            "polling_hz": self.polling,
            "fps_avg": self.fps(),
            "vsync": false,
            "system_mouse": self.sys.map(|s| json!({"enhance_precision": s.enhance_precision, "speed": s.speed})),
            "absolute_events": self.cap.absolute.load(Ordering::Relaxed),
            "sweep": self.sweep.as_ref().map(|sw| json!({"native": sw.native, "scale": sw.scale(), "block": sw.idx + 1, "blocks": sw.order.len()})),
            "stats": {"all": summ(&stats.all), "x": summ(&stats.x), "y": summ(&stats.y), "n_valid": stats.n_valid, "speed_accuracy": sa},
        });
        let trials: Vec<_> = run
            .records
            .iter()
            .map(|r| {
                let m = match &r.outcome {
                    Outcome::Done(m) => json!({
                        "valid": m.is_valid(), "invalid": m.invalid.map(|i| format!("{i:?}")),
                        "ratio": m.ratio, "ratio_x": m.ratio_x, "ratio_y": m.ratio_y, "perp": m.perp,
                        "criterion": format!("{:?}", m.criterion), "peak_speed": m.peak_speed,
                        "onset_ms": m.onset_ms, "stop_ms": m.stop_ms, "click_ms": m.click_ms,
                        "corrections": m.corrections, "correction_dist": m.correction_dist,
                    }),
                    Outcome::Invalid(e) => json!({"valid": false, "invalid": e}),
                    Outcome::Timeout => json!({"valid": false, "invalid": "Timeout"}),
                };
                json!({
                    "index": r.index, "warmup": r.plan.warmup, "angle_deg": r.plan.angle_deg,
                    "distance_plan": r.plan.distance, "start": [r.start.x, r.start.y],
                    "target": [r.target.x, r.target.y], "misses": r.misses, "metrics": m,
                })
            })
            .collect();
        session::save(&session, &json!(trials), &run.raw)
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = rspec_core::now();
        self.pump(now);
        ctx.request_repaint();
        if ctx.input(|i| i.key_pressed(egui::Key::F11)) {
            self.fullscreen = !self.fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
        }
        if self.screen == Screen::Test {
            self.test_screen(ctx, now);
            return;
        }
        egui::TopBottomPanel::top("nav").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("R-Spec Aim");
                ui.separator();
                ui.selectable_value(&mut self.screen, Screen::Monitor, "1. Captura");
                ui.selectable_value(&mut self.screen, Screen::Setup, "2. Configurar e testar");
                if !self.rounds.is_empty() {
                    ui.selectable_value(&mut self.screen, Screen::Results, "3. Resultado");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("{:.0} fps · F11 tela cheia", self.fps()));
                });
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.screen {
                Screen::Monitor => self.monitor_ui(ui),
                Screen::Setup => self.setup_ui(ui, ctx),
                Screen::Results => self.results_ui(ui),
                Screen::SweepBreak => self.sweep_break_ui(ui, ctx),
                Screen::SweepResult => self.sweep_result_ui(ui),
                Screen::Test => {}
            });
        });
    }
}

impl App {
    fn monitor_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(e) = &self.src_err {
            ui.colored_label(COL_Y, format!("Sem captura bruta: {e}"));
            return;
        }
        ui.label("Mova o mouse em círculos rápidos por alguns segundos para medir a taxa real.");
        ui.add_space(8.0);
        let span = self.recent.back().zip(self.recent.front()).map(|(b, f)| b.t - f.t).unwrap_or(0.0);
        let (cx, cy): (i64, i64) = self.recent.iter().fold((0, 0), |a, s| (a.0 + s.dx.abs() as i64, a.1 + s.dy.abs() as i64));
        egui::Grid::new("mon").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
            ui.label("Polling rate medido");
            ui.label(egui::RichText::new(self.polling.map_or("mova o mouse".into(), |h| format!("{h:.0} Hz"))).size(28.0).color(ACCENT));
            ui.end_row();
            ui.label("Eventos nos últimos 2 s");
            ui.label(format!("{}", self.recent.len()));
            ui.end_row();
            ui.label("Counts/s (X · Y)");
            if span > 0.1 {
                ui.label(format!("{:.0} · {:.0}", cx as f64 / span, cy as f64 / span));
            } else {
                ui.label("–");
            }
            ui.end_row();
            ui.label("Mouses vistos");
            let mut devs: Vec<u64> = self.recent.iter().map(|s| s.device).collect();
            devs.sort_unstable();
            devs.dedup();
            ui.label(format!("{}", devs.len()));
            ui.end_row();
            if let Some(s) = self.sys {
                ui.label("Windows: aumentar precisão do ponteiro");
                ui.label(if s.enhance_precision { "ligado (não afeta este teste, mas afeta jogos sem raw input)" } else { "desligado" });
                ui.end_row();
                ui.label("Windows: velocidade do ponteiro");
                ui.label(format!("{} de 20{}", s.speed, if s.speed == 10 { " (neutro)" } else { "" }));
                ui.end_row();
            }
        });
        if let Some(hz) = self.polling {
            ui.label(rspec_recommender::polling_advice(hz, self.fps()));
        }
        if self.cap.absolute_seen() {
            ui.colored_label(COL_Y, "Recebi eventos absolutos (tablet, área de trabalho remota ou VM). Eles foram descartados; use um mouse comum direto no PC.");
        }
        ui.add_space(12.0);
        ui.label("Velocidade (counts/s), últimos 2 s");
        speed_plot(ui, &self.recent, rspec_core::now());
    }

    fn setup_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let c = &mut self.cfg;
        egui::Grid::new("cfg").num_columns(2).spacing([24.0, 8.0]).show(ui, |ui| {
            ui.label("Jogo");
            let prev = c.game;
            egui::ComboBox::from_id_salt("game").selected_text(GAMES[c.game].name).show_ui(ui, |ui| {
                for (i, g) in GAMES.iter().enumerate() {
                    ui.selectable_value(&mut c.game, i, g.name);
                }
            });
            if prev != c.game {
                c.fov = GAMES[c.game].fov;
            }
            ui.end_row();
            if c.game == CUSTOM {
                ui.label("Yaw (graus por count)");
                ui.add(egui::DragValue::new(&mut c.yaw).speed(0.0001).range(0.0001..=1.0).max_decimals(5));
                ui.end_row();
            }
            if c.game != MOUSE_ONLY {
                ui.label("Sensibilidade no jogo");
                ui.add(egui::DragValue::new(&mut c.sens).speed(0.01).range(0.01..=100.0).max_decimals(3));
                ui.end_row();
            }
            ui.label("DPI do mouse");
            ui.add(egui::DragValue::new(&mut c.dpi).speed(10.0).range(100.0..=32000.0));
            ui.end_row();
            ui.label("Trials válidos");
            ui.add(egui::DragValue::new(&mut c.trials).range(16..=96));
            ui.end_row();
            ui.label("Rastro depois de cada trial");
            ui.checkbox(&mut c.feedback, "mostrar (pode mudar o jeito que você mira)");
            ui.end_row();
        });
        ui.add_space(6.0);
        if c.mouse_only() {
            ui.label("Calibra só o mouse: a sugestão sai em DPI (e DPI X/Y, se o software do mouse deixar) e polling rate.");
        } else {
            let cm = units::cm_per_360(c.deg_per_count(), c.dpi);
            ui.label(format!("Sensibilidade atual: {cm:.1} cm/360 (com {:.0} DPI nominal)", c.dpi));
        }
        ui.label(
            egui::RichText::new("No teste a mira anda igual ao seu cursor do Windows, com o DPI e a velocidade que você já usa. O app não muda nada no seu mouse nem no jogo.")
                .color(MUTED),
        );
        if self.sys.is_some_and(|s| s.enhance_precision) {
            ui.colored_label(COL_Y, "\"Aumentar precisão do ponteiro\" está ligado no Windows. O teste ignora essa aceleração, como os jogos com raw input.");
        }
        ui.add_space(12.0);
        ui.label("Mire no alvo pequeno e clique nele o mais rápido que conseguir sem errar. Fique parado até o próximo alvo aparecer. Esc cancela.");
        ui.label("Use tela cheia (F11) para a sensação ficar igual à do jogo.");
        ui.add_space(8.0);
        let ok = self.src.is_some();
        ui.horizontal(|ui| {
            if ui.add_enabled(ok, egui::Button::new(egui::RichText::new("Teste completo").size(20.0))).clicked() {
                self.sweep = None;
                self.start_round(ctx);
            }
            ui.label(egui::RichText::new(format!("{} alvos, sugestão por eixo", self.cfg.trials)).color(MUTED));
        });
        ui.add_space(10.0);
        ui.label(egui::RichText::new("Teste rápido: acha a sensibilidade com melhor equilíbrio entre velocidade e precisão").strong());
        ui.label(
            egui::RichText::new(format!("{} blocos curtos de {SWEEP_TRIALS} alvos, de ×{:.2} a ×{:.2} da sua sensibilidade.", SWEEP_SCALES.len(), SWEEP_SCALES[0], SWEEP_SCALES[SWEEP_SCALES.len() - 1]))
                .color(MUTED),
        );
        ui.horizontal(|ui| {
            let seed = (rspec_core::now() * 1e6) as u64;
            if ui.add_enabled(ok, egui::Button::new("Escala só no app (às cegas)")).clicked() {
                self.sweep = Some(Sweep::new(false, self.cfg.dpi, seed));
                self.screen = Screen::SweepBreak;
            }
            if ui.add_enabled(ok, egui::Button::new("100% nativo (eu troco o DPI)")).clicked() {
                self.sweep = Some(Sweep::new(true, self.cfg.dpi, seed));
                self.screen = Screen::SweepBreak;
            }
        });
    }

    fn test_screen(&mut self, ctx: &egui::Context, now: f64) {
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let Some(run) = self.run.as_mut() else { return };
        if focused {
            run.feed(&self.buf);
            run.tick(now);
        }
        let done = run.finished();
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(BG)).show(ctx, |ui| {
            let rect = ui.max_rect();
            let p = ui.painter();
            let at = |x: f64, y: f64| pos2(rect.left() + x as f32, rect.top() + y as f32);
            let prog = format!("{}/{} válidos · Esc cancela", run.valid_count(), run.valid_goal);
            p.text(rect.left_top() + vec2(16.0, 12.0), Align2::LEFT_TOP, prog, FontId::proportional(15.0), MUTED);
            if !focused {
                p.text(rect.center(), Align2::CENTER_CENTER, "Clique na janela para continuar", FontId::proportional(22.0), MUTED);
            }
            match &run.phase {
                Phase::Active(a) => {
                    p.circle_filled(at(a.target.x, a.target.y), TARGET_RADIUS as f32, ACCENT);
                }
                Phase::Feedback { .. } => {
                    if let Some(r) = run.records.last() {
                        let pts: Vec<Pos2> = r.path.iter().map(|q| at(q.x, q.y)).collect();
                        p.add(Shape::line(pts, Stroke::new(1.5_f32, MUTED)));
                        p.circle_stroke(at(r.target.x, r.target.y), TARGET_RADIUS as f32, Stroke::new(1.5_f32, ACCENT));
                        if let Outcome::Done(m) = &r.outcome {
                            if let Some(q) = r.path.iter().find(|q| q.t >= r.path[0].t + m.stop_ms as f64 / 1000.0) {
                                p.circle_filled(at(q.x, q.y), 5.0, GOOD);
                            }
                            p.text(at(r.target.x, r.target.y) + vec2(0.0, -24.0), Align2::CENTER_BOTTOM, format!("{:.2}", m.ratio), FontId::proportional(16.0), GOOD);
                        }
                    }
                }
                _ => {}
            }
            // mira
            let c = at(run.pos.x, run.pos.y);
            let s = Stroke::new(2.0_f32, Color32::WHITE);
            p.line_segment([c - vec2(9.0, 0.0), c - vec2(3.0, 0.0)], s);
            p.line_segment([c + vec2(3.0, 0.0), c + vec2(9.0, 0.0)], s);
            p.line_segment([c - vec2(0.0, 9.0), c - vec2(0.0, 3.0)], s);
            p.line_segment([c + vec2(0.0, 3.0), c + vec2(0.0, 9.0)], s);
        });
        if esc {
            self.end_round(ctx, false);
        } else if done {
            self.end_round(ctx, true);
        }
    }

    fn results_ui(&mut self, ui: &mut egui::Ui) {
        let Some(round) = self.rounds.last() else { return };
        let st = &round.stats;
        ui.heading("Onde a mira para no primeiro movimento");
        ui.label("1,00 = parou em cima do alvo · acima de 1 passou · abaixo de 1 parou antes. Alvo ideal: 0,95.");
        ui.add_space(8.0);
        egui::Grid::new("res").num_columns(4).spacing([24.0, 6.0]).show(ui, |ui| {
            for h in ["", "mediana", "IC 95%", "trials"] {
                ui.label(egui::RichText::new(h).color(MUTED));
            }
            ui.end_row();
            for (name, s, col) in [("Geral", &st.all, ACCENT), ("Horizontal", &st.x, COL_X), ("Vertical", &st.y, COL_Y)] {
                ui.colored_label(col, name);
                ui.label(if s.n > 0 { format!("{:.3}", s.median) } else { "–".into() });
                ui.label(s.ci.map_or("–".into(), |c| format!("{:.3} – {:.3}", c.0, c.1)));
                ui.label(format!("{}", s.n));
                ui.end_row();
            }
        });
        if let Some(sa) = st.speed_accuracy {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Velocidade x precisão").strong());
            ui.label(format!(
                "Tempo até acertar: {:.0} ms (mediana) · até a primeira parada: {:.0} ms · consistência da parada: ±{:.1}% · throughput {:.2} bits/s",
                sa.mt_median * 1000.0,
                sa.stop_mean * 1000.0,
                sa.sigma * 100.0,
                sa.throughput
            ));
        }
        if let Some(p) = &round.saved {
            ui.label(egui::RichText::new(format!("Sessão gravada em {p}")).color(MUTED).small());
        }
        ui.add_space(14.0);

        let Some(split) = self.cfg.split else {
            let q = if self.cfg.mouse_only() {
                "O software do seu mouse permite DPI horizontal e vertical separados?"
            } else {
                "Seu mouse ou jogo permite ajustar horizontal e vertical separadamente?"
            };
            ui.label(egui::RichText::new(q).size(18.0));
            ui.horizontal(|ui| {
                if ui.button("Sim").clicked() {
                    self.cfg.split = Some(true);
                }
                if ui.button("Não").clicked() {
                    self.cfg.split = Some(false);
                }
            });
            return;
        };
        let rp = RecParams::default();
        let advice = recommend(st, split, &rp);
        let dpc = self.cfg.deg_per_count();
        let (fx, fy) = match &advice {
            Advice::Split { x, y } => {
                axis_line(ui, "Horizontal", COL_X, x, &self.cfg);
                axis_line(ui, "Vertical", COL_Y, y, &self.cfg);
                (x.factor, y.factor)
            }
            Advice::Single { all, imbalance } => {
                axis_line(ui, "Sensibilidade", ACCENT, all, &self.cfg);
                if let Some(d) = imbalance {
                    let which = if *d > 0.0 { "horizontal passa mais" } else { "vertical passa mais" };
                    ui.colored_label(COL_Y, format!("Eixos desequilibrados ({which}, diferença {:.2}). Onde dá para ajustar, se existir: {}.", d.abs(), GAMES[self.cfg.game].vertical));
                }
                (all.factor, all.factor)
            }
        };
        let total = (fx, fy);
        ui.add_space(6.0);
        if self.cfg.mouse_only() {
            if split && (total.0 - total.1).abs() > 1e-9 {
                ui.label(format!(
                    "DPI: {:.0} → X {:.0} · Y {:.0}",
                    self.cfg.dpi,
                    rspec_recommender::new_dpi(self.cfg.dpi, total.0, 1.0),
                    rspec_recommender::new_dpi(self.cfg.dpi, total.1, 1.0)
                ));
            }
        } else {
            ui.label(format!(
                "cm/360: {:.1} → {:.1}  (DPI nominal; calibrar o DPI real vem na próxima fase)",
                units::cm_per_360(dpc, self.cfg.dpi),
                units::cm_per_360(dpc * total.0, self.cfg.dpi)
            ));
            ui.label(format!("Alternativa pelo DPI do mouse: {:.0} → {:.0}", self.cfg.dpi, rspec_recommender::new_dpi(self.cfg.dpi, total.0, 1.0)));
        }
        if let Some(hz) = self.polling {
            ui.label(format!("Polling rate: {}", rspec_recommender::polling_advice(hz, self.fps())));
        }
        ui.label(egui::RichText::new("Para testar a sugestão, mude você mesmo o DPI ou a sensibilidade, atualize os valores na tela de configuração e rode de novo.").color(MUTED));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Nova rodada").clicked() {
                self.screen = Screen::Setup;
            }
            if ui.button("Mudar resposta dos eixos").clicked() {
                self.cfg.split = None;
            }
        });

        if self.rounds.len() > 1 {
            ui.add_space(14.0);
            ui.heading("Comparação entre rodadas");
            let scores: Vec<RoundScore> = self.rounds.iter().map(Round::score).collect();
            let best = best_round(&scores, &rp);
            egui::Grid::new("cmp").num_columns(7).spacing([24.0, 4.0]).show(ui, |ui| {
                for h in ["rodada", "DPI · sens", "geral", "horizontal", "vertical", "tempo até acertar", "throughput"] {
                    ui.label(egui::RichText::new(h).color(MUTED));
                }
                ui.end_row();
                for (i, r) in self.rounds.iter().enumerate() {
                    let label = format!("{}{}", i + 1, if best == Some(i) { " ★" } else { "" });
                    ui.colored_label(if best == Some(i) { GOOD } else { Color32::WHITE }, label);
                    ui.label(format!("{:.0} · {:.3}", r.dpi, r.sens));
                    for s in [&r.stats.all, &r.stats.x, &r.stats.y] {
                        ui.label(if s.n > 0 { format!("{:.3}", s.median) } else { "–".into() });
                    }
                    let sa = r.stats.speed_accuracy;
                    ui.label(sa.map_or("–".into(), |s| format!("{:.0} ms", s.mt_median * 1000.0)));
                    ui.label(sa.map_or("–".into(), |s| format!("{:.2}", s.throughput)));
                    ui.end_row();
                }
            });
            if let Some(b) = best {
                let r = &self.rounds[b];
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Melhor equilíbrio entre velocidade e precisão: rodada {} ({:.0} DPI{})",
                        b + 1,
                        r.dpi,
                        if self.cfg.mouse_only() { String::new() } else { format!(", sensibilidade {:.3} no {}", r.sens, GAMES[self.cfg.game].name) }
                    ))
                    .size(18.0)
                    .color(GOOD),
                );
                ui.label(egui::RichText::new("Só concorrem rodadas em que a primeira parada ficou entre 0,85 e 1,08; entre elas vence a de maior throughput.").color(MUTED).small());
            }
        }
    }
}

impl App {
    fn sweep_break_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(sw) = self.sweep.as_ref() else {
            self.screen = Screen::Setup;
            return;
        };
        ui.heading(format!("Teste rápido · bloco {} de {}", sw.idx + 1, sw.order.len()));
        ui.add_space(8.0);
        if sw.native {
            let dpi = self.cfg.dpi * sw.scale();
            ui.label(egui::RichText::new(format!("Mude o DPI do mouse para {dpi:.0} no software do mouse e clique em Continuar.")).size(20.0).color(ACCENT));
            ui.label(egui::RichText::new(format!("No fim, volte para {:.0} se não for adotar a sugestão.", self.cfg.dpi)).color(MUTED));
        } else {
            ui.label("A velocidade da mira muda a cada bloco, em ordem sorteada. Seu mouse e seu jogo não mudam.");
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button(egui::RichText::new("Continuar").size(20.0)).clicked() {
                self.start_round(ctx);
            }
            if ui.button("Cancelar").clicked() {
                self.sweep = None;
                self.screen = Screen::Setup;
            }
        });
    }

    fn sweep_result_ui(&mut self, ui: &mut egui::Ui) {
        let Some(sw) = self.sweep.as_ref() else { return };
        let mut blocks = sw.blocks.clone();
        blocks.sort_by(|a, b| a.0.total_cmp(&b.0));
        let pts: Vec<SweepPoint> = blocks
            .iter()
            .map(|(k, st)| SweepPoint {
                scale: *k,
                median_ratio: if st.all.n > 0 { st.all.median } else { f64::NAN },
                throughput: st.speed_accuracy.map_or(f64::NAN, |s| s.throughput),
            })
            .collect();
        ui.heading("Teste rápido: resultado");
        ui.add_space(6.0);
        egui::Grid::new("sweep").num_columns(6).spacing([24.0, 4.0]).show(ui, |ui| {
            for h in ["escala", if self.cfg.mouse_only() { "DPI" } else { "sensibilidade" }, "primeira parada", "tempo até acertar", "throughput", "pontuação"] {
                ui.label(egui::RichText::new(h).color(MUTED));
            }
            ui.end_row();
            for ((k, st), p) in blocks.iter().zip(&pts) {
                ui.label(format!("×{k:.2}"));
                ui.label(if self.cfg.mouse_only() { format!("{:.0}", self.cfg.dpi * k) } else { format!("{:.3}", self.cfg.sens * k) });
                ui.label(if st.all.n > 0 { format!("{:.3}", st.all.median) } else { "–".into() });
                ui.label(st.speed_accuracy.map_or("–".into(), |s| format!("{:.0} ms", s.mt_median * 1000.0)));
                ui.label(st.speed_accuracy.map_or("–".into(), |s| format!("{:.2}", s.throughput)));
                ui.label(format!("{:.2}", rspec_recommender::sweep_score(p)));
                ui.end_row();
            }
        });
        ui.add_space(10.0);
        match rspec_recommender::sweep_optimum(&pts) {
            Some(k) => {
                let txt = if self.cfg.mouse_only() {
                    format!("Seu ponto de equilíbrio: ×{k:.3} → DPI {:.0} (hoje {:.0})", rspec_recommender::new_dpi(self.cfg.dpi, k, 1.0), self.cfg.dpi)
                } else {
                    format!(
                        "Seu ponto de equilíbrio: ×{k:.3} → sensibilidade {:.3} no {} ({:.1} cm/360), ou DPI {:.0}",
                        self.cfg.sens * k,
                        GAMES[self.cfg.game].name,
                        units::cm_per_360(self.cfg.deg_per_count() * k, self.cfg.dpi),
                        rspec_recommender::new_dpi(self.cfg.dpi, k, 1.0)
                    )
                };
                ui.label(egui::RichText::new(txt).size(20.0).color(GOOD));
                if (k - 1.0).abs() < 0.03 {
                    ui.label("Fica a menos de 3% do que você já usa: pode manter.");
                }
            }
            None => {
                ui.colored_label(COL_Y, "Poucos alvos válidos para achar o ponto de equilíbrio. Rode de novo.");
            }
        }
        ui.label(
            egui::RichText::new("Pontuação = throughput, com penalidade quando a primeira parada sai de 0,85 a 1,08. O ponto de equilíbrio vem de uma curva ajustada aos blocos.")
                .color(MUTED)
                .small(),
        );
        if sw.native {
            ui.colored_label(ACCENT, format!("Lembre de voltar o DPI para {:.0} se não for adotar a sugestão.", self.cfg.dpi));
        }
        ui.add_space(8.0);
        if ui.button("Voltar").clicked() {
            self.sweep = None;
            self.screen = Screen::Setup;
        }
    }
}

fn axis_line(ui: &mut egui::Ui, name: &str, col: Color32, a: &AxisAdvice, cfg: &Config) {
    let txt = match a.keep {
        Some(KeepReason::NoData) => "sem dados suficientes".to_string(),
        Some(KeepReason::CiInBand) => "manter (já está na faixa boa)".to_string(),
        Some(KeepReason::SmallChange) => format!("manter (mudança de {:+.1}% é pequena demais)", (a.raw - 1.0) * 100.0),
        None => {
            let f = a.factor;
            if cfg.mouse_only() {
                format!("×{:.3} ({:+.0}%) → DPI {:.0}", f, (f - 1.0) * 100.0, rspec_recommender::new_dpi(cfg.dpi, f, 1.0))
            } else {
                format!("×{:.3} ({:+.0}%) → sensibilidade {:.3} no {}", f, (f - 1.0) * 100.0, cfg.sens * f, GAMES[cfg.game].name)
            }
        }
    };
    ui.horizontal(|ui| {
        ui.colored_label(col, egui::RichText::new(name).strong());
        ui.label(egui::RichText::new(txt).size(18.0));
        if a.fallback {
            ui.colored_label(MUTED, "(poucos trials neste eixo, usei a mediana geral)");
        }
    });
}

/// Gráfico de velocidade em bins de 5 ms.
fn speed_plot(ui: &mut egui::Ui, recent: &VecDeque<RawSample>, now: f64) {
    let w = ui.available_width();
    let (resp, p) = ui.allocate_painter(vec2(w, 220.0), Sense::hover());
    let r: Rect = resp.rect;
    p.rect_filled(r, 4.0, BG);
    const BIN: f64 = 0.005;
    let n = (2.0 / BIN) as usize;
    let mut bins = vec![(0.0f64, 0.0f64); n];
    for s in recent {
        let age = now - s.t;
        if (0.0..2.0).contains(&age) {
            let i = n - 1 - ((age / BIN) as usize).min(n - 1);
            bins[i].0 += s.dx as f64;
            bins[i].1 += s.dy as f64;
        }
    }
    let sp: Vec<f64> = bins.iter().map(|b| b.0.hypot(b.1) / BIN).collect();
    let max = sp.iter().copied().fold(1000.0, f64::max);
    let pts: Vec<Pos2> = sp
        .iter()
        .enumerate()
        .map(|(i, v)| pos2(r.left() + r.width() * i as f32 / (n - 1) as f32, r.bottom() - (v / max) as f32 * (r.height() - 8.0)))
        .collect();
    p.add(Shape::line(pts, Stroke::new(1.5_f32, ACCENT)));
    p.text(r.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, format!("{max:.0}"), FontId::monospace(11.0), MUTED);
}
