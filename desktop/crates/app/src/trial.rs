//! Protocolo de trial do modo 2D (seção 7 da spec). Não desenha nada: recebe amostras
//! brutas na ordem em que chegaram e o relógio, e avança a máquina de estados.

use rspec_analysis::synthetic::XorShift;
use rspec_analysis::{analyze_trial, Params, TrialMetrics};
use rspec_core::{buttons, Point, RawSample, TrialInput, Vec2};

pub const TARGET_RADIUS: f64 = 11.0;
const HOLD_S: f64 = 0.2;
const TIMEOUT_S: f64 = 4.0;
const MARGIN: f64 = 40.0;
const DISTANCES: [f64; 4] = [150.0, 250.0, 350.0, 450.0];

#[derive(Clone, Debug)]
pub struct Plan {
    pub angle_deg: f64,
    pub distance: f64,
    pub warmup: bool,
}

#[derive(Clone, Debug)]
pub struct Active {
    pub plan: Plan,
    pub start: Vec2,
    pub target: Vec2,
    pub t_appear: f64,
    pub path: Vec<Point>,
    pub misses: u32,
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Done(Box<TrialMetrics>),
    Invalid(String),
    Timeout,
}

#[derive(Clone, Debug)]
pub struct Record {
    pub index: usize,
    pub plan: Plan,
    pub start: Vec2,
    pub target: Vec2,
    pub misses: u32,
    pub outcome: Outcome,
    /// Trajetória para o feedback pós-trial.
    pub path: Vec<Point>,
}

impl Record {
    pub fn metrics(&self) -> Option<&TrialMetrics> {
        match &self.outcome {
            Outcome::Done(m) if !self.plan.warmup && m.is_valid() => Some(m),
            _ => None,
        }
    }
}

pub enum Phase {
    /// Espera a mira ficar parada por 200 ms.
    Hold,
    /// Atraso aleatório antes do alvo aparecer. Mover aqui volta para Hold.
    Delay { until: f64 },
    Active(Active),
    /// Mostra o rastro do trial anterior.
    Feedback { until: f64 },
}

pub struct Run {
    pub pos: Vec2,
    pub arena: (f64, f64),
    /// px por count em cada eixo, já com o ganho de "testar com a sugestão".
    pub ppc: (f64, f64),
    pub device: Option<u64>,
    pub phase: Phase,
    pub queue: Vec<Plan>,
    pub records: Vec<Record>,
    pub raw: Vec<(usize, RawSample)>,
    pub feedback: bool,
    pub valid_goal: usize,
    last_move: f64,
    rng: XorShift,
    attempts_left: usize,
}

impl Run {
    pub fn new(arena: (f64, f64), ppc: (f64, f64), warmup: usize, valid_goal: usize, feedback: bool, seed: u64) -> Self {
        let mut rng = XorShift::new(seed);
        // 8 setores x 4 distâncias, balanceado para qualquer quantidade: percorre os setores
        // primeiro e depois as faixas de distância (32 trials = 8 horizontais e 8 verticais)
        let mut main: Vec<Plan> = (0..valid_goal)
            .map(|k| Plan { angle_deg: (k % 8) as f64 * 45.0, distance: DISTANCES[(k / 8 + k % 4) % 4], warmup: false })
            .collect();
        shuffle(&mut main, &mut rng);
        let mut queue: Vec<Plan> = (0..warmup)
            .map(|_| Plan { angle_deg: rng.below(8) as f64 * 45.0, distance: DISTANCES[rng.below(4)], warmup: true })
            .collect();
        queue.extend(main);
        queue.reverse(); // pop() tira do fim
        Self {
            pos: Vec2::new(arena.0 / 2.0, arena.1 / 2.0),
            arena,
            ppc,
            device: None,
            phase: Phase::Hold,
            attempts_left: queue.len() * 2,
            queue,
            records: Vec::new(),
            raw: Vec::new(),
            feedback,
            valid_goal,
            last_move: rspec_core::now(),
            rng,
        }
    }

    pub fn valid_count(&self) -> usize {
        self.records.iter().filter(|r| r.metrics().is_some()).count()
    }

    pub fn finished(&self) -> bool {
        self.queue.is_empty() && !matches!(self.phase, Phase::Active(_)) || self.attempts_left == 0
    }

    /// Processa amostras na ordem de chegada. Retorna o registro de um trial que terminou.
    pub fn feed(&mut self, samples: &[RawSample]) -> Option<usize> {
        let mut finished = None;
        for s in samples {
            // trava no mouse que gerou o primeiro movimento
            let dev = *self.device.get_or_insert(s.device);
            if s.device != dev {
                continue;
            }
            self.raw.push((self.records.len(), *s));
            if s.dx != 0 || s.dy != 0 {
                self.last_move = s.t;
                self.pos.x = (self.pos.x + s.dx as f64 * self.ppc.0).clamp(0.0, self.arena.0);
                self.pos.y = (self.pos.y + s.dy as f64 * self.ppc.1).clamp(0.0, self.arena.1);
                if matches!(self.phase, Phase::Delay { .. }) {
                    self.phase = Phase::Hold; // movimento antecipado: recomeça a espera
                }
            }
            if let Phase::Active(a) = &mut self.phase {
                a.path.push(Point { t: s.t, x: self.pos.x, y: self.pos.y });
                if s.buttons & buttons::LEFT_DOWN != 0 {
                    let d = (self.pos.x - a.target.x).hypot(self.pos.y - a.target.y);
                    if d <= TARGET_RADIUS {
                        finished = Some(self.finish(s.t));
                    } else {
                        a.misses += 1;
                    }
                }
            }
        }
        finished
    }

    /// Transições por tempo. Chamar a cada frame, depois de `feed`.
    pub fn tick(&mut self, now: f64) -> Option<usize> {
        match &self.phase {
            Phase::Hold if now - self.last_move >= HOLD_S && !self.queue.is_empty() => {
                let delay = 0.3 + 0.4 * self.rng.next_f64();
                self.phase = Phase::Delay { until: now + delay };
            }
            Phase::Delay { until } if now >= *until => self.spawn(now),
            Phase::Active(a) if now - a.t_appear > TIMEOUT_S => {
                let Phase::Active(a) = std::mem::replace(&mut self.phase, Phase::Hold) else { unreachable!() };
                self.queue.insert(0, a.plan.clone());
                return Some(self.push(a, Outcome::Timeout));
            }
            Phase::Feedback { until } if now >= *until => self.phase = Phase::Hold,
            _ => {}
        }
        None
    }

    fn spawn(&mut self, now: f64) {
        let Some(plan) = self.queue.pop() else {
            self.phase = Phase::Hold;
            return;
        };
        let target = self.place(&plan);
        self.phase = Phase::Active(Active {
            plan,
            start: self.pos,
            target,
            t_appear: now,
            path: vec![Point { t: now, x: self.pos.x, y: self.pos.y }],
            misses: 0,
        });
    }

    /// Posição do alvo a partir da mira atual. Se não couber, tenta a direção oposta
    /// (mantém o eixo) e depois encurta a distância.
    fn place(&self, plan: &Plan) -> Vec2 {
        let fits = |p: Vec2| p.x >= MARGIN && p.x <= self.arena.0 - MARGIN && p.y >= MARGIN && p.y <= self.arena.1 - MARGIN;
        let at = |deg: f64, d: f64| {
            let r = deg.to_radians();
            Vec2::new(self.pos.x + d * r.cos(), self.pos.y - d * r.sin())
        };
        let mut d = plan.distance;
        while d >= 100.0 {
            for deg in [plan.angle_deg, plan.angle_deg + 180.0] {
                let p = at(deg, d);
                if fits(p) {
                    return p;
                }
            }
            d -= 25.0;
        }
        Vec2::new(self.arena.0 / 2.0, self.arena.1 / 2.0)
    }

    fn finish(&mut self, t_click: f64) -> usize {
        let Phase::Active(a) = std::mem::replace(&mut self.phase, Phase::Hold) else { unreachable!() };
        let input = TrialInput { path: a.path.clone(), start: a.start, target: a.target, t_appear: a.t_appear, t_click };
        let outcome = match analyze_trial(&input, &Params::default()) {
            Ok(m) => Outcome::Done(Box::new(m)),
            Err(e) => Outcome::Invalid(format!("{e:?}")),
        };
        let bad = !matches!(&outcome, Outcome::Done(m) if m.is_valid());
        if bad && !a.plan.warmup {
            self.queue.insert(0, a.plan.clone());
        }
        if self.feedback {
            self.phase = Phase::Feedback { until: t_click + 0.8 };
        }
        self.push(a, outcome)
    }

    fn push(&mut self, a: Active, outcome: Outcome) -> usize {
        self.attempts_left = self.attempts_left.saturating_sub(1);
        self.records.push(Record {
            index: self.records.len(),
            plan: a.plan,
            start: a.start,
            target: a.target,
            misses: a.misses,
            outcome,
            path: a.path,
        });
        self.records.len() - 1
    }
}

fn shuffle<T>(v: &mut [T], rng: &mut XorShift) {
    for i in (1..v.len()).rev() {
        v.swap(i, rng.below(i + 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simula um jogador: espera o alvo, faz um movimento de mínimo jerk que para em
    /// `ratio` da distância, corrige e clica. Tudo em counts brutos a 1000 Hz.
    fn play(ratio: f64) -> Run {
        let mut run = Run::new((1920.0, 1080.0), (0.5, 0.5), 2, 16, false, 9);
        let mut t = 0.0;
        let mj = |s: f64| { let s = s.clamp(0.0, 1.0); 10.0 * s.powi(3) - 15.0 * s.powi(4) + 6.0 * s.powi(5) };
        let mut guard = 0;
        while !run.finished() && guard < 200 {
            guard += 1;
            // parado até o alvo aparecer
            while !matches!(run.phase, Phase::Active(_)) {
                t += 0.001;
                run.tick(t);
            }
            let Phase::Active(a) = &run.phase else { unreachable!() };
            let (p0, tg) = (a.start, a.target);
            let stop = Vec2::new(p0.x + (tg.x - p0.x) * ratio, p0.y + (tg.y - p0.y) * ratio);
            let t0 = t;
            let (mut cx, mut cy) = (0.0f64, 0.0f64); // counts acumulados
            for i in 1..=700 {
                let tt = i as f64 / 1000.0;
                let (a1, a2) = (mj((tt - 0.2) / 0.28), mj((tt - 0.55) / 0.15));
                let px = (stop.x - p0.x) * a1 + (tg.x - stop.x) * a2;
                let py = (stop.y - p0.y) * a1 + (tg.y - stop.y) * a2;
                let (nx, ny) = ((px / 0.5).round(), (py / 0.5).round());
                let (dx, dy) = ((nx - cx) as i32, (ny - cy) as i32);
                cx = nx;
                cy = ny;
                let click = i == 700;
                if dx != 0 || dy != 0 || click {
                    let s = RawSample { t: t0 + tt, dx, dy, device: 1, buttons: if click { buttons::LEFT_DOWN } else { 0 }, batched: false };
                    run.feed(&[s]);
                }
                run.tick(t0 + tt);
            }
            t = t0 + 0.7;
        }
        run
    }

    #[test]
    fn rodada_completa_mede_ratio() {
        let run = play(1.1);
        assert!(run.finished());
        let v: Vec<f64> = run.records.iter().filter_map(|r| r.metrics().map(|m| m.ratio)).collect();
        assert_eq!(v.len(), 16, "{:?}", run.records.iter().map(|r| &r.outcome).collect::<Vec<_>>());
        for r in v {
            assert!((r - 1.1).abs() < 0.03, "{r}");
        }
    }
}
