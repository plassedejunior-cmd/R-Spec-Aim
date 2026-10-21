//! Sugestão de sensibilidade a partir das razões de primeira parada (seção 9 da spec).

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
    /// Ganho aplicado na rodada (1.0 = sensibilidade original).
    pub gain: f64,
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
            RoundScore { gain: 1.0, median_ratio: 1.12, throughput: 6.0 }, // rápido, mas passa do alvo
            RoundScore { gain: 0.92, median_ratio: 0.97, throughput: 4.8 },
            RoundScore { gain: 0.85, median_ratio: 0.93, throughput: 4.1 }, // preciso, mas lento
        ];
        assert_eq!(best_round(&r, &p), Some(1));
        let fora = [RoundScore { gain: 1.0, median_ratio: 1.2, throughput: 6.0 }, RoundScore { gain: 0.9, median_ratio: 1.1, throughput: 5.0 }];
        assert_eq!(best_round(&fora, &p), Some(1));
    }

    #[test]
    fn dpi_alternativo() {
        assert_eq!(new_dpi(800.0, 0.93, 1.0), 744.0);
        assert_eq!(new_dpi(800.0, 0.93, 50.0), 750.0);
    }
}
