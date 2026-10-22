//! Teste rápido em escada: começa numa sensibilidade bem baixa e fixa (400 DPI equivalente,
//! onde a precisão está garantida para quase todo mundo), sobe um degrau por bloco até o
//! desempenho começar a cair e depois refina em volta do melhor degrau, com o melhor nível e
//! dois meios-degraus vizinhos em ordem misturada.
//!
//! Os níveis são absolutos (DPI equivalente), não relativos à configuração da pessoa, para o
//! resultado não ficar ancorado no que ela já usa.
//!
//! "Começar a errar" é medido pela pontuação de velocidade x precisão (`sweep_score`), não
//! só por erro: sensibilidade baixa demais também perde, porque fica lenta. O refinamento em
//! ordem misturada separa o efeito da sensibilidade do aquecimento e do cansaço, que numa
//! subida simples ficariam confundidos com ela.

use crate::{sweep_optimum, sweep_score, SweepPoint, ACCEPT_BAND};

/// Primeiro degrau, em DPI equivalente.
pub const START_DPI: f64 = 400.0;
/// Razão entre degraus da subida.
pub const STEP: f64 = 1.3;
/// Último degrau possível da subida.
pub const MAX_DPI: f64 = 3200.0;
/// Queda em relação ao melhor degrau que conta como "piorou".
const DROP: f64 = 0.92;
/// Erros de clique por alvo acima disso contam como "começou a errar".
const MAX_MISS_RATE: f64 = 0.35;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Climb,
    Refine,
    Done,
}

#[derive(Clone, Copy, Debug)]
pub struct Block {
    /// Índice em `Staircase::levels`.
    pub level: usize,
    pub point: SweepPoint,
    pub miss_rate: f64,
    pub refine: bool,
}

#[derive(Clone, Debug)]
pub struct Staircase {
    pub phase: Phase,
    /// Níveis em DPI equivalente: os da subida e, depois, os meios-degraus do refinamento.
    pub levels: Vec<f64>,
    climb: usize,
    strikes: usize,
    refine_queue: Vec<usize>,
    pub blocks: Vec<Block>,
    /// Parou de subir porque passou do ponto (e não por chegar ao topo).
    pub stopped_by_drop: bool,
}

impl Default for Staircase {
    fn default() -> Self {
        Self::new()
    }
}

impl Staircase {
    pub fn new() -> Self {
        let mut levels = vec![START_DPI];
        while levels.last().unwrap() * STEP <= MAX_DPI * 1.01 {
            let next = (levels.last().unwrap() * STEP / 10.0).round() * 10.0;
            levels.push(next);
        }
        Self { phase: Phase::Climb, levels, climb: 0, strikes: 0, refine_queue: Vec::new(), blocks: Vec::new(), stopped_by_drop: false }
    }

    /// DPI equivalente do próximo bloco, ou `None` quando acabou.
    pub fn next_dpi(&self) -> Option<f64> {
        self.next_level().map(|l| self.levels[l])
    }

    fn next_level(&self) -> Option<usize> {
        match self.phase {
            Phase::Climb => Some(self.climb),
            Phase::Refine => self.refine_queue.last().copied(),
            Phase::Done => None,
        }
    }

    /// Blocos que ainda faltam no refinamento.
    pub fn remaining_refine(&self) -> usize {
        self.refine_queue.len()
    }

    /// Média dos blocos de cada nível já medido, em ordem crescente de DPI:
    /// (índice do nível, ponto médio, erros por alvo, número de blocos).
    pub fn per_level(&self) -> Vec<(usize, SweepPoint, f64, usize)> {
        let mut idx: Vec<usize> = (0..self.levels.len()).collect();
        idx.sort_by(|a, b| self.levels[*a].total_cmp(&self.levels[*b]));
        let mut out = Vec::new();
        for l in idx {
            let bs: Vec<&Block> = self.blocks.iter().filter(|b| b.level == l && b.point.throughput.is_finite() && b.point.median_ratio.is_finite()).collect();
            if bs.is_empty() {
                continue;
            }
            let n = bs.len() as f64;
            let pt = SweepPoint {
                scale: bs.iter().map(|b| b.point.scale).sum::<f64>() / n,
                median_ratio: bs.iter().map(|b| b.point.median_ratio).sum::<f64>() / n,
                throughput: bs.iter().map(|b| b.point.throughput).sum::<f64>() / n,
            };
            let miss = bs.iter().map(|b| b.miss_rate).sum::<f64>() / n;
            out.push((l, pt, miss, bs.len()));
        }
        out
    }

    fn best_level(&self) -> Option<usize> {
        self.per_level().into_iter().max_by(|a, b| score(&a.1, a.2).total_cmp(&score(&b.1, b.2))).map(|p| p.0)
    }

    /// Registra o bloco que acabou de rodar no nível `next_dpi()`.
    /// `point.scale` é o DPI equivalente real usado.
    pub fn record(&mut self, point: SweepPoint, miss_rate: f64, shuffle_seed: u64) {
        let Some(level) = self.next_level() else { return };
        let refine = self.phase == Phase::Refine;
        self.blocks.push(Block { level, point, miss_rate, refine });
        match self.phase {
            Phase::Climb => {
                let best = self.per_level().iter().map(|p| score(&p.1, p.2)).fold(0.0, f64::max);
                let cur = if point.throughput.is_finite() { score(&point, miss_rate) } else { 0.0 };
                self.strikes = if best > 0.0 && cur < DROP * best { self.strikes + 1 } else { 0 };
                let overshoot = point.median_ratio > ACCEPT_BAND.1 || miss_rate > MAX_MISS_RATE;
                let past_best = self.best_level().is_some_and(|b| level > b);
                let dropped = self.strikes >= 2 || (overshoot && past_best);
                if dropped || level + 1 >= self.levels.len() {
                    self.stopped_by_drop = dropped;
                    self.start_refine(shuffle_seed);
                } else {
                    self.climb += 1;
                }
            }
            Phase::Refine => {
                self.refine_queue.pop();
                if self.refine_queue.is_empty() {
                    self.phase = Phase::Done;
                }
            }
            Phase::Done => {}
        }
    }

    fn start_refine(&mut self, seed: u64) {
        let Some(b) = self.best_level() else {
            self.phase = Phase::Done;
            return;
        };
        let best = self.levels[b];
        let half = STEP.sqrt();
        let mut q = vec![b];
        for dpi in [best / half, best * half] {
            if dpi >= START_DPI / half * 0.99 && dpi <= MAX_DPI * half * 1.01 {
                self.levels.push((dpi / 10.0).round() * 10.0);
                q.push(self.levels.len() - 1);
            }
        }
        // embaralha (Fisher-Yates com LCG simples; a ordem só precisa não ser a da subida)
        let mut x = seed | 1;
        for i in (1..q.len()).rev() {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            q.swap(i, (x >> 33) as usize % (i + 1));
        }
        self.refine_queue = q;
        self.phase = Phase::Refine;
    }

    /// DPI equivalente de equilíbrio, com a média de subida e refinamento em cada nível.
    /// A curva só usa os níveis a até um degrau do melhor: longe dele a penalidade de
    /// precisão despenca e entortaria a parábola.
    pub fn result(&self) -> Option<f64> {
        let best = self.levels[self.best_level()?];
        let pts: Vec<SweepPoint> = self
            .per_level()
            .into_iter()
            .filter(|(l, ..)| (self.levels[*l] / best).ln().abs() <= STEP.ln() * 1.01)
            .map(|(_, mut p, miss, _)| {
                // erros de clique entram como queda de throughput
                p.throughput /= 1.0 + miss;
                p
            })
            .collect();
        sweep_optimum(&pts)
    }
}

fn score(p: &SweepPoint, miss_rate: f64) -> f64 {
    sweep_score(p) / (1.0 + miss_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pessoa simulada: throughput com pico em `peak` DPI, passa do alvo acima dele.
    fn person(dpi: f64, peak: f64, fatigue: f64) -> (SweepPoint, f64) {
        let tp = 5.0 * (-1.5 * (dpi.ln() - peak.ln()).powi(2)).exp() - fatigue;
        let ratio = 0.95 + 0.5 * (dpi / peak - 1.0).max(0.0);
        let miss = if dpi > peak * 1.4 { 0.5 } else { 0.05 };
        (SweepPoint { scale: dpi, median_ratio: ratio, throughput: tp }, miss)
    }

    fn run(peak: f64) -> Staircase {
        let mut s = Staircase::new();
        let mut n = 0.0;
        while let Some(dpi) = s.next_dpi() {
            let (p, m) = person(dpi, peak, n * 0.02);
            s.record(p, m, 42);
            n += 1.0;
            assert!(n < 20.0);
        }
        s
    }

    #[test]
    fn niveis_comecam_em_400() {
        let s = Staircase::new();
        assert_eq!(s.levels[0], 400.0);
        assert_eq!(s.levels[1], 520.0);
        assert!(*s.levels.last().unwrap() <= MAX_DPI * 1.01);
    }

    #[test]
    fn sobe_ate_piorar_e_refina() {
        let s = run(800.0);
        assert_eq!(s.phase, Phase::Done);
        assert!(s.stopped_by_drop);
        let climb = s.blocks.iter().filter(|b| !b.refine).count();
        assert!(climb <= 7, "{climb}");
        assert_eq!(s.blocks.iter().filter(|b| b.refine).count(), 3);
        let r = s.result().unwrap();
        assert!((r / 800.0 - 1.0).abs() < 0.08, "{r}");
    }

    #[test]
    fn pico_baixo_e_alto() {
        let r = run(450.0).result().unwrap();
        assert!(r < 560.0, "{r}");
        let r = run(1800.0).result().unwrap();
        assert!((r / 1800.0 - 1.0).abs() < 0.12, "{r}");
    }

    #[test]
    fn chega_ao_topo_sem_piorar() {
        // quanto mais rápido, melhor, sem passar do alvo
        let mut s = Staircase::new();
        while let Some(dpi) = s.next_dpi() {
            s.record(SweepPoint { scale: dpi, median_ratio: 0.95, throughput: dpi / 1000.0 }, 0.05, 1);
        }
        assert!(!s.stopped_by_drop);
        assert_eq!(s.phase, Phase::Done);
    }
}
