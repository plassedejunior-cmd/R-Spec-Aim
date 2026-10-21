//! Gerador de trajetórias sintéticas para testes (seção 11 da spec).
//!
//! Submovimento primário com perfil de mínimo jerk (Flash & Hogan, 1985) até um ponto
//! de parada com amplitude controlada, pausa opcional e submovimento corretivo até o alvo.
//! Posições quantizadas em counts inteiros, como o sensor entrega.

use rspec_core::{Point, TrialInput, Vec2};

/// PRNG pequeno e determinístico, para não puxar dependência só por causa dos testes.
#[derive(Clone, Debug)]
pub struct XorShift(u64);

impl XorShift {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Uniforme em [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

fn min_jerk(s: f64) -> f64 {
    let s = s.clamp(0.0, 1.0);
    10.0 * s.powi(3) - 15.0 * s.powi(4) + 6.0 * s.powi(5)
}

#[derive(Clone, Debug)]
pub struct Synth {
    /// Distância até o alvo, em counts.
    pub distance: f64,
    /// Direção em radianos.
    pub angle: f64,
    /// Amplitude da primeira parada por eixo (1.0 = em cima do alvo).
    pub ratio_x: f64,
    pub ratio_y: f64,
    /// Polling rate simulado.
    pub hz: f64,
    pub reaction_ms: f64,
    pub primary_ms: f64,
    pub pause_ms: f64,
    /// Quanto a correção começa antes do fim do primário (submovimento emendado).
    pub overlap_ms: f64,
    pub correction_ms: f64,
    /// Ruído uniforme de posição, em counts (pico a pico).
    pub noise: f64,
    /// Erro perpendicular máximo na parada, fração da distância.
    pub perp_jitter: f64,
    /// Eventos por lote de leitura (1 = sem lote). Eventos do lote recebem o mesmo tempo.
    pub batch: usize,
}

impl Default for Synth {
    fn default() -> Self {
        Self {
            distance: 600.0,
            angle: 0.0,
            ratio_x: 1.0,
            ratio_y: 1.0,
            hz: 1000.0,
            reaction_ms: 220.0,
            primary_ms: 280.0,
            pause_ms: 70.0,
            overlap_ms: 0.0,
            correction_ms: 170.0,
            noise: 0.15,
            perp_jitter: 0.06,
            batch: 1,
        }
    }
}

impl Synth {
    pub fn generate(&self, rng: &mut XorShift) -> TrialInput {
        let tg = Vec2::new(self.distance * self.angle.cos(), self.distance * self.angle.sin());
        let pe = (rng.next_f64() - 0.5) * self.perp_jitter * self.distance;
        let stop = Vec2::new(tg.x * self.ratio_x - self.angle.sin() * pe, tg.y * self.ratio_y + self.angle.cos() * pe);
        let t_corr = self.reaction_ms + self.primary_ms + self.pause_ms - self.overlap_ms;
        let t_end = t_corr + self.correction_ms + 60.0;
        let dt = 1000.0 / self.hz;
        let mut path = Vec::new();
        let mut i = 1usize;
        loop {
            let t = i as f64 * dt;
            if t > t_end {
                break;
            }
            let a = min_jerk((t - self.reaction_ms) / self.primary_ms);
            let b = min_jerk((t - t_corr) / self.correction_ms);
            let x = stop.x * a + (tg.x - stop.x) * b + (rng.next_f64() - 0.5) * self.noise;
            let y = stop.y * a + (tg.y - stop.y) * b + (rng.next_f64() - 0.5) * self.noise;
            path.push(Point { t: t / 1000.0, x: x.round(), y: y.round() });
            i += 1;
        }
        if self.batch > 1 {
            for chunk in path.chunks_mut(self.batch) {
                let t = chunk.last().unwrap().t;
                chunk.iter_mut().for_each(|p| p.t = t);
            }
        }
        TrialInput { path, start: Vec2::new(0.0, 0.0), target: tg, t_appear: 0.0, t_click: t_end / 1000.0 }
    }
}
