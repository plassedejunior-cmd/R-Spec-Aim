//! Agregação da sessão: mediana, IQR e intervalo de confiança da mediana por bootstrap.

use crate::synthetic::XorShift;
use crate::TrialMetrics;

pub fn quantile(v: &[f64], q: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let i = (s.len() - 1) as f64 * q;
    let lo = i.floor() as usize;
    let hi = (lo + 1).min(s.len() - 1);
    s[lo] + (s[hi] - s[lo]) * (i - lo as f64)
}

pub fn median(v: &[f64]) -> f64 {
    quantile(v, 0.5)
}

/// IC de 95% da mediana por bootstrap percentil. `None` com menos de 3 valores.
pub fn bootstrap_ci(v: &[f64], reps: usize, rng: &mut XorShift) -> Option<(f64, f64)> {
    if v.len() < 3 {
        return None;
    }
    let mut buf = vec![0.0; v.len()];
    let meds: Vec<f64> = (0..reps)
        .map(|_| {
            buf.iter_mut().for_each(|b| *b = v[rng.below(v.len())]);
            median(&buf)
        })
        .collect();
    Some((quantile(&meds, 0.025), quantile(&meds, 0.975)))
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub n: usize,
    pub median: f64,
    pub q1: f64,
    pub q3: f64,
    pub ci: Option<(f64, f64)>,
}

impl Summary {
    pub fn of(v: &[f64], rng: &mut XorShift) -> Self {
        if v.is_empty() {
            return Self::default();
        }
        Self { n: v.len(), median: median(v), q1: quantile(v, 0.25), q3: quantile(v, 0.75), ci: bootstrap_ci(v, 1000, rng) }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SessionStats {
    pub all: Summary,
    pub x: Summary,
    pub y: Summary,
    pub n_valid: usize,
    pub n_total: usize,
    /// Balanço velocidade x precisão da sessão.
    pub speed_accuracy: Option<SpeedAccuracy>,
}

/// Throughput efetivo no estilo ISO 9241-9 (Soukoreff & MacKenzie, 2004), adaptado:
/// a precisão vem da dispersão do ponto de primeira parada (o movimento balístico), e a
/// velocidade vem do tempo total até acertar o alvo (início do movimento até o clique),
/// que é o que decide uma troca de tiro. Mais alto = melhor equilíbrio.
#[derive(Clone, Copy, Debug)]
pub struct SpeedAccuracy {
    /// Desvio padrão do erro da primeira parada ao longo do eixo, em frações da distância.
    pub sigma: f64,
    /// Índice de dificuldade efetivo, bits: log2(1 / (4.133 * sigma) + 1).
    pub ide: f64,
    /// Tempo médio do início do movimento até o clique (s).
    pub mt_mean: f64,
    pub mt_median: f64,
    /// Tempo médio do início do movimento até a primeira parada (s).
    pub stop_mean: f64,
    /// bits/s
    pub throughput: f64,
}

pub fn speed_accuracy(trials: &[&TrialMetrics]) -> Option<SpeedAccuracy> {
    if trials.len() < 5 {
        return None;
    }
    let n = trials.len() as f64;
    let err: Vec<f64> = trials.iter().map(|t| t.ratio - 1.0).collect();
    let mean = err.iter().sum::<f64>() / n;
    let sigma = (err.iter().map(|e| (e - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt().max(1e-3);
    let ide = (1.0 / (4.133 * sigma) + 1.0).log2();
    let mt: Vec<f64> = trials.iter().map(|t| (t.click_ms - t.onset_ms) as f64 / 1000.0).collect();
    let mt_mean = mt.iter().sum::<f64>() / n;
    let stop_mean = trials.iter().map(|t| (t.stop_ms - t.onset_ms) as f64 / 1000.0).sum::<f64>() / n;
    Some(SpeedAccuracy { sigma, ide, mt_mean, mt_median: median(&mt), stop_mean, throughput: ide / mt_mean })
}

pub fn aggregate(trials: &[TrialMetrics], seed: u64) -> SessionStats {
    let mut rng = XorShift::new(seed);
    let v: Vec<&TrialMetrics> = trials.iter().filter(|t| t.is_valid()).collect();
    let all: Vec<f64> = v.iter().map(|t| t.ratio).collect();
    let xs: Vec<f64> = v.iter().filter_map(|t| t.ratio_x).collect();
    let ys: Vec<f64> = v.iter().filter_map(|t| t.ratio_y).collect();
    SessionStats {
        all: Summary::of(&all, &mut rng),
        x: Summary::of(&xs, &mut rng),
        y: Summary::of(&ys, &mut rng),
        n_valid: v.len(),
        n_total: trials.len(),
        speed_accuracy: speed_accuracy(&v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::Synth;
    use crate::{analyze_trial, Params};

    fn session(spread: f64, correction_ms: f64) -> SessionStats {
        let mut rng = XorShift::new(11);
        let m: Vec<TrialMetrics> = (0..24)
            .map(|i| {
                let r = 0.95 + spread * (rng.next_f64() - 0.5);
                let s = Synth { angle: i as f64 * 0.7, ratio_x: r, ratio_y: r, correction_ms, ..Default::default() };
                analyze_trial(&s.generate(&mut rng), &Params::default()).unwrap()
            })
            .collect();
        aggregate(&m, 1)
    }

    #[test]
    fn throughput_premia_consistencia_e_rapidez() {
        let base = session(0.2, 170.0).speed_accuracy.unwrap();
        let mais_preciso = session(0.08, 170.0).speed_accuracy.unwrap();
        let mais_lento = session(0.2, 400.0).speed_accuracy.unwrap();
        assert!(mais_preciso.sigma < base.sigma);
        assert!(mais_preciso.throughput > base.throughput);
        assert!(mais_lento.throughput < base.throughput);
    }
}
