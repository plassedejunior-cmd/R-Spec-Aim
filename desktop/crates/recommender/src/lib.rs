//! Sugestão de sensibilidade a partir das razões de primeira parada (seção 9 da spec).

pub mod staircase;

use rspec_analysis::stats::{SessionStats, Summary};

#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// Parar levemente antes do alvo é o padrão de quem mira bem.
    pub target_ratio: f64,
    /// Fração do erro corrigida por rodada.
    pub damping: f64,
    pub min_factor: f64,
    pub max_factor: f64,
    pub min_trials_per_axis: usize,
    /// Faixa considerada boa: se o IC da mediana cruza ela, mantém.
    pub keep_band: (f64, f64),
    /// Mudanças menores que isto viram "manter".
    pub min_change: f64,
    /// Diferença entre medianas X e Y que dispara o aviso de desequilíbrio.
    pub imbalance_warn: f64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            target_ratio: 0.95,
            damping: 0.6,
            min_factor: 0.75,
            max_factor: 1.25,
            min_trials_per_axis: 8,
            keep_band: (0.90, 1.00),
            min_change: 0.03,
            imbalance_warn: 0.10,
        }
    }
}

impl Params {
    pub fn factor(&self, ratio: f64) -> f64 {
        (1.0 + self.damping * (self.target_ratio / ratio - 1.0)).clamp(self.min_factor, self.max_factor)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepReason {
    NoData,
    CiInBand,
    SmallChange,
}

#[derive(Clone, Copy, Debug)]
pub struct AxisAdvice {
    /// Fator a aplicar (1.0 quando a recomendação é manter).
    pub factor: f64,
    /// Fator calculado antes da regra de manter.
    pub raw: f64,
    pub keep: Option<KeepReason>,
    /// Eixo com amostra insuficiente, usou a mediana geral.
    pub fallback: bool,
}

#[derive(Clone, Debug)]
pub enum Advice {
    /// Mouse ou jogo permite ajustar horizontal e vertical separadamente.
    Split { x: AxisAdvice, y: AxisAdvice },
    Single {
        all: AxisAdvice,
        /// median(ratio_x) - median(ratio_y), quando acima do limiar de aviso.
        imbalance: Option<f64>,
    },
}

fn decide(s: &Summary, p: &Params, fallback: bool) -> AxisAdvice {
    if s.n == 0 {
        return AxisAdvice { factor: 1.0, raw: 1.0, keep: Some(KeepReason::NoData), fallback };
    }
    let raw = p.factor(s.median);
    let keep = match s.ci {
        Some((lo, hi)) if hi >= p.keep_band.0 && lo <= p.keep_band.1 => Some(KeepReason::CiInBand),
        _ if (raw - 1.0).abs() < p.min_change => Some(KeepReason::SmallChange),
        _ => None,
    };
    AxisAdvice { factor: if keep.is_some() { 1.0 } else { raw }, raw, keep, fallback }
}

/// `split`: resposta à pergunta obrigatória "seu mouse ou jogo permite ajustar H e V separadamente?".
pub fn recommend(stats: &SessionStats, split: bool, p: &Params) -> Advice {
    if split {
        let axis = |s: &Summary| {
            let fb = s.n < p.min_trials_per_axis;
            decide(if fb { &stats.all } else { s }, p, fb)
        };
        Advice::Split { x: axis(&stats.x), y: axis(&stats.y) }
    } else {
        let imbalance = (stats.x.n > 0 && stats.y.n > 0)
            .then(|| stats.x.median - stats.y.median)
            .filter(|d| d.abs() > p.imbalance_warn);
        Advice::Single { all: decide(&stats.all, p, false), imbalance }
    }
}

/// Uma rodada já jogada, para escolher entre sensibilidades testadas.
#[derive(Clone, Copy, Debug)]
pub struct RoundScore {
    pub median_ratio: f64,
    /// Throughput (bits/s), veja `rspec_analysis::stats::SpeedAccuracy`.
    pub throughput: f64,
}

/// Faixa de primeira parada aceitável para uma rodada concorrer como "melhor".
pub const ACCEPT_BAND: (f64, f64) = (0.85, 1.08);

/// Escolhe a rodada com melhor equilíbrio entre velocidade e precisão.
/// A precisão continua sendo o filtro principal: só concorrem rodadas cuja mediana de
/// primeira parada está na faixa aceitável; entre elas vence o maior throughput.
/// Se nenhuma estiver na faixa, vence a mais próxima do alvo de precisão.
pub fn best_round(rounds: &[RoundScore], p: &Params) -> Option<usize> {
    let ok = |r: &RoundScore| (ACCEPT_BAND.0..=ACCEPT_BAND.1).contains(&r.median_ratio) && r.throughput.is_finite();
    let by_tp = rounds.iter().enumerate().filter(|(_, r)| ok(r)).max_by(|a, b| a.1.throughput.total_cmp(&b.1.throughput));
    by_tp.map(|(i, _)| i).or_else(|| {
        rounds
            .iter()
            .enumerate()
            .min_by(|a, b| (a.1.median_ratio - p.target_ratio).abs().total_cmp(&(b.1.median_ratio - p.target_ratio).abs()))
            .map(|(i, _)| i)
    })
}

/// Um bloco curto da varredura de sensibilidade.
#[derive(Clone, Copy, Debug)]
pub struct SweepPoint {
    /// Escala de sensibilidade do bloco em relação ao nativo (1.0 = nativo).
    pub scale: f64,
    pub median_ratio: f64,
    pub throughput: f64,
}

/// Pontuação de um bloco: throughput, com penalidade suave quando a primeira parada sai
/// da faixa aceitável. A precisão continua sendo o filtro principal.
pub fn sweep_score(pt: &SweepPoint) -> f64 {
    let (lo, hi) = ACCEPT_BAND;
    let d = if pt.median_ratio < lo { lo - pt.median_ratio } else if pt.median_ratio > hi { pt.median_ratio - hi } else { 0.0 };
    pt.throughput * (-(d / 0.08).powi(2)).exp()
}

/// Escala de equilíbrio entre velocidade e precisão a partir da varredura.
/// Ajusta uma parábola da pontuação contra ln(escala) por mínimos quadrados; se ela tiver
/// máximo dentro da faixa testada, usa o vértice, senão usa o melhor bloco medido.
pub fn sweep_optimum(points: &[SweepPoint]) -> Option<f64> {
    let pts: Vec<(f64, f64)> = points
        .iter()
        .filter(|p| p.scale > 0.0 && p.throughput.is_finite() && p.median_ratio.is_finite())
        .map(|p| (p.scale.ln(), sweep_score(p)))
        .collect();
    let best = pts.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1))?;
    if pts.len() < 3 {
        return Some(best.0.exp());
    }
    // normais de y = a + b x + c x²
    let mut m = [[0.0f64; 4]; 3];
    for &(x, y) in &pts {
        let v = [1.0, x, x * x];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += v[i] * v[j];
            }
            m[i][3] += v[i] * y;
        }
    }
    let Some([_, b, c]) = solve3(m) else { return Some(best.0.exp()) };
    let (xmin, xmax) = pts.iter().fold((f64::MAX, f64::MIN), |r, p| (r.0.min(p.0), r.1.max(p.0)));
    if c < 0.0 {
        let xv = -b / (2.0 * c);
        if (xmin..=xmax).contains(&xv) {
            return Some(xv.exp());
        }
    }
    Some(best.0.exp())
}

fn solve3(mut m: [[f64; 4]; 3]) -> Option<[f64; 3]> {
    for col in 0..3 {
        let piv = (col..3).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
        if m[piv][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, piv);
        for r in 0..3 {
            if r != col {
                let f = m[r][col] / m[col][col];
                for k in col..4 {
                    m[r][k] -= f * m[col][k];
                }
            }
        }
    }
    Some([m[0][3] / m[0][0], m[1][3] / m[1][1], m[2][3] / m[2][2]])
}

/// Conselho de polling rate a partir da taxa medida e do fps do PC (fundamentos do
/// projeto: 1000 Hz é o padrão; PC simples fica em 500-1000; acima disso só se o frame
/// time não piorar).
pub fn polling_advice(measured_hz: f64, fps: f64) -> String {
    let nominal = [125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0]
        .into_iter()
        .min_by(|a: &f64, b: &f64| (a - measured_hz).abs().total_cmp(&(b - measured_hz).abs()))
        .unwrap_or(1000.0);
    if nominal < 1000.0 {
        format!("Seu mouse está em ~{nominal:.0} Hz. Se o software dele permitir, use 1000 Hz: menos atraso e movimento mais liso.")
    } else if nominal > 1000.0 && fps < 2.0 * nominal / 4.0 {
        format!("Seu mouse está em ~{nominal:.0} Hz, mas o PC rodou a {fps:.0} fps. Em PC mais simples, 1000 Hz costuma ser melhor: taxas altas gastam CPU e podem travar o jogo.")
    } else {
        format!("~{nominal:.0} Hz está bom. Não precisa mudar.")
    }
}

/// Conversão de sensibilidade de jogo: `deg_per_count = sens * yaw`.
pub fn new_game_sens(sens: f64, factor: f64) -> f64 {
    sens * factor
}

/// Alternativa por DPI, arredondada para o passo que o software do mouse aceita.
pub fn new_dpi(dpi: f64, factor: f64, step: f64) -> f64 {
    ((dpi * factor) / step).round() * step
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summ(n: usize, med: f64, ci: (f64, f64)) -> Summary {
        Summary { n, median: med, q1: med, q3: med, ci: Some(ci) }
    }

    #[test]
    fn fatores_conhecidos() {
        let p = Params::default();
        assert!((p.factor(0.95) - 1.0).abs() < 1e-12);
        // overshoot 1.10 => 1 + 0.6*(0.95/1.10 - 1) = 0.91818...
        assert!((p.factor(1.10) - 0.918_181_8).abs() < 1e-6);
        // undershoot 0.80 => 1 + 0.6*(0.1875) = 1.1125
        assert!((p.factor(0.80) - 1.1125).abs() < 1e-9);
    }

    #[test]
    fn clamp() {
        let p = Params::default();
        assert_eq!(p.factor(0.3), 1.25);
        assert_eq!(p.factor(1.9), 0.75);
    }

    #[test]
    fn mantem_quando_ic_na_faixa() {
        let s = SessionStats { all: summ(30, 1.02, (0.97, 1.08)), ..Default::default() };
        match recommend(&s, false, &Params::default()) {
            Advice::Single { all, .. } => {
                assert_eq!(all.keep, Some(KeepReason::CiInBand));
                assert_eq!(all.factor, 1.0);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn muda_quando_ic_fora() {
        let s = SessionStats { all: summ(30, 1.15, (1.10, 1.20)), ..Default::default() };
        match recommend(&s, false, &Params::default()) {
            Advice::Single { all, .. } => {
                assert!(all.keep.is_none());
                assert!(all.factor < 0.92);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn eixos_separados_e_fallback() {
        let s = SessionStats {
            all: summ(30, 1.15, (1.10, 1.20)),
            x: summ(12, 1.20, (1.15, 1.25)),
            y: summ(4, 0.80, (0.75, 0.85)),
            ..Default::default()
        };
        match recommend(&s, true, &Params::default()) {
            Advice::Split { x, y } => {
                assert!(!x.fallback && x.factor < 0.9);
                // Y com 4 trials usa a mediana geral
                assert!(y.fallback);
                assert!((y.raw - Params::default().factor(1.15)).abs() < 1e-12);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn aviso_de_desequilibrio() {
        let s = SessionStats {
            all: summ(30, 1.0, (0.95, 1.05)),
            x: summ(15, 1.08, (1.0, 1.1)),
            y: summ(15, 0.92, (0.9, 1.0)),
            ..Default::default()
        };
        match recommend(&s, false, &Params::default()) {
            Advice::Single { imbalance, .. } => assert!((imbalance.unwrap() - 0.16).abs() < 1e-9),
            _ => unreachable!(),
        }
    }

    #[test]
    fn melhor_rodada_equilibra_velocidade() {
        let p = Params::default();
        let r = [
            RoundScore { median_ratio: 1.12, throughput: 6.0 }, // rápido, mas passa do alvo
            RoundScore { median_ratio: 0.97, throughput: 4.8 },
            RoundScore { median_ratio: 0.93, throughput: 4.1 }, // preciso, mas lento
        ];
        assert_eq!(best_round(&r, &p), Some(1));
        let fora = [RoundScore { median_ratio: 1.2, throughput: 6.0 }, RoundScore { median_ratio: 1.1, throughput: 5.0 }];
        assert_eq!(best_round(&fora, &p), Some(1));
    }

    #[test]
    fn varredura_acha_o_pico() {
        // throughput com pico em escala 0.92
        let pts: Vec<SweepPoint> = [0.85, 0.92, 1.0, 1.08, 1.15]
            .iter()
            .map(|&k: &f64| SweepPoint { scale: k, median_ratio: 0.95, throughput: 5.0 - 40.0 * (k.ln() - 0.92f64.ln()).powi(2) })
            .collect();
        let o = sweep_optimum(&pts).unwrap();
        assert!((o - 0.92).abs() < 0.005, "{o}");
    }

    #[test]
    fn varredura_penaliza_imprecisao() {
        // a escala mais alta é a mais rápida, mas passa muito do alvo
        let pts = [
            SweepPoint { scale: 0.9, median_ratio: 0.94, throughput: 4.0 },
            SweepPoint { scale: 1.0, median_ratio: 1.0, throughput: 4.2 },
            SweepPoint { scale: 1.1, median_ratio: 1.25, throughput: 5.0 },
        ];
        let o = sweep_optimum(&pts).unwrap();
        assert!(o < 1.06, "{o}");
    }

    #[test]
    fn polling() {
        assert!(polling_advice(498.0, 300.0).contains("1000 Hz"));
        assert!(polling_advice(1003.0, 300.0).contains("está bom"));
        assert!(polling_advice(7900.0, 240.0).contains("PC mais simples"));
        assert!(polling_advice(4010.0, 2400.0).contains("está bom"));
    }

    #[test]
    fn dpi_alternativo() {
        assert_eq!(new_dpi(800.0, 0.93, 1.0), 744.0);
        assert_eq!(new_dpi(800.0, 0.93, 50.0), 750.0);
    }
}
