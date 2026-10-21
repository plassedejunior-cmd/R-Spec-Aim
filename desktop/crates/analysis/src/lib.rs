//! Detecção da primeira parada (fim do submovimento primário) e métricas por trial.
//!
//! Base teórica: modelo de submovimentos otimizados de Meyer, Abrams, Kornblum, Wright
//! e Smith (1988), "Optimality in human motor performance: ideal control of rapid aimed
//! movements", Psychological Review 95(3). O movimento até o alvo é tratado como um
//! submovimento balístico primário seguido de correções; medimos onde o primário termina.
//!
//! Segue a seção 8 da spec (R-Spec-Aim/claude.md). Mesma lógica do protótipo web.

use rspec_core::{Point, TrialInput, Vec2};

pub mod stats;
pub mod synthetic;

/// Frequência da grade de reamostragem (1 ms).
pub const FS: f64 = 1000.0;

#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// Corte do passa-baixa (Hz).
    pub cutoff_hz: f64,
    /// Limiar de início do movimento, fração do pico.
    pub onset_frac: f64,
    /// Critério (a): velocidade abaixo desta fração do pico...
    pub stop_frac: f64,
    /// ...por pelo menos isto (ms).
    pub stop_ms: usize,
    /// Critério (c): nova subida acima desta fração do pico.
    pub resurge_frac: f64,
    /// Pico mínimo de velocidade, em unidades de posição por segundo.
    pub min_peak: f64,
    /// Tempo mínimo entre a aparição do alvo e o início do movimento (ms).
    pub min_reaction_ms: usize,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            cutoff_hz: 18.0,
            onset_frac: 0.05,
            stop_frac: 0.07,
            stop_ms: 20,
            resurge_frac: 0.20,
            min_peak: 250.0,
            min_reaction_ms: 100,
        }
    }
}

/// Qual critério marcou a primeira parada.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopCriterion {
    /// (a) velocidade baixa sustentada.
    LowSpeed,
    /// (b) reversão da velocidade ao longo do eixo do alvo.
    Reversal,
    /// (c) mínimo local seguido de novo submovimento sem parar.
    Resurge,
    /// Nenhum critério disparou; usou o fim do trial.
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    TooFewSamples,
    TooShort,
    LowPeak,
    Anticipated,
    RatioOutOfRange,
}

#[derive(Clone, Debug)]
pub struct TrialMetrics {
    pub ratio: f64,
    pub ratio_x: Option<f64>,
    pub ratio_y: Option<f64>,
    /// Erro perpendicular na parada, normalizado pela distância.
    pub perp: f64,
    pub criterion: StopCriterion,
    pub peak_speed: f64,
    pub distance: f64,
    /// Índices na grade de 1 ms, a partir da aparição do alvo.
    pub onset_ms: usize,
    pub stop_ms: usize,
    pub click_ms: usize,
    pub corrections: usize,
    pub correction_dist: f64,
    /// Velocidade filtrada na grade de 1 ms, para gráficos e replay.
    pub speed: Vec<f64>,
    pub invalid: Option<Invalid>,
}

impl TrialMetrics {
    pub fn is_valid(&self) -> bool {
        self.invalid.is_none()
    }
}

/// Coeficientes de um Butterworth passa-baixa de 2ª ordem (transformada bilinear).
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    pub fn butter2(fc: f64, fs: f64) -> Self {
        let k = (std::f64::consts::PI * fc / fs).tan();
        let r2 = std::f64::consts::SQRT_2;
        let n = 1.0 / (1.0 + r2 * k + k * k);
        let b0 = k * k * n;
        Self { b0, b1: 2.0 * b0, b2: b0, a1: 2.0 * (k * k - 1.0) * n, a2: (1.0 - r2 * k + k * k) * n }
    }

    fn lfilter(&self, x: &[f64]) -> Vec<f64> {
        let (mut x1, mut x2, mut y1, mut y2) = (x[0], x[0], x[0], x[0]);
        x.iter()
            .map(|&xi| {
                let v = self.b0 * xi + self.b1 * x1 + self.b2 * x2 - self.a1 * y1 - self.a2 * y2;
                x2 = x1;
                x1 = xi;
                y2 = y1;
                y1 = v;
                v
            })
            .collect()
    }

    /// Filtro ida e volta (fase zero), com extensão por reflexão ímpar nas pontas.
    pub fn filtfilt(&self, x: &[f64]) -> Vec<f64> {
        let n = x.len();
        if n < 3 {
            return x.to_vec();
        }
        let pad = 120.min(n - 1);
        let mut ext = Vec::with_capacity(n + 2 * pad);
        ext.extend((0..pad).map(|i| 2.0 * x[0] - x[pad - i]));
        ext.extend_from_slice(x);
        ext.extend((0..pad).map(|i| 2.0 * x[n - 1] - x[n - 2 - i]));
        let mut y = self.lfilter(&ext);
        y.reverse();
        let mut y = self.lfilter(&y);
        y.reverse();
        y[pad..pad + n].to_vec()
    }
}

/// Reamostra a posição em grade uniforme de 1 ms entre `t0` e `t1` (segundos).
pub fn resample(path: &[Point], t0: f64, t1: f64) -> (Vec<f64>, Vec<f64>) {
    // amostras com o mesmo tempo (lote) colapsam para a última posição
    let mut pts: Vec<Point> = Vec::with_capacity(path.len());
    for p in path {
        match pts.last_mut() {
            Some(last) if p.t <= last.t => {
                last.x = p.x;
                last.y = p.y;
            }
            _ => pts.push(*p),
        }
    }
    let n = (((t1 - t0) * FS).floor() as usize + 1).max(2);
    let (mut xs, mut ys) = (Vec::with_capacity(n), Vec::with_capacity(n));
    let mut j = 0;
    for i in 0..n {
        let t = t0 + i as f64 / FS;
        while j + 1 < pts.len() && pts[j + 1].t <= t {
            j += 1;
        }
        let (x, y) = if t <= pts[0].t {
            (pts[0].x, pts[0].y)
        } else if j + 1 >= pts.len() {
            (pts[j].x, pts[j].y)
        } else {
            let (a, b) = (pts[j], pts[j + 1]);
            let f = (t - a.t) / (b.t - a.t);
            (a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f)
        };
        xs.push(x);
        ys.push(y);
    }
    (xs, ys)
}

/// Derivada por diferenças centrais, em unidades por segundo.
fn diff(a: &[f64]) -> Vec<f64> {
    let n = a.len();
    let mut d = vec![0.0; n];
    for i in 1..n - 1 {
        d[i] = (a[i + 1] - a[i - 1]) / 2.0 * FS;
    }
    d[0] = (a[1] - a[0]) * FS;
    d[n - 1] = (a[n - 1] - a[n - 2]) * FS;
    d
}

/// Encontra a primeira parada depois do pico. Retorna (índice, critério).
fn first_stop(speed: &[f64], vpar: &[f64], ip: usize, peak: f64, p: &Params) -> (usize, StopCriterion) {
    let n = speed.len();
    let (mut run, mut run_start) = (0usize, 0usize);
    let (mut min_v, mut min_i) = (f64::INFINITY, ip);
    for i in ip + 1..n {
        if speed[i] < p.stop_frac * peak {
            if run == 0 {
                run_start = i;
            }
            run += 1;
            if run >= p.stop_ms {
                return (run_start, StopCriterion::LowSpeed);
            }
        } else {
            run = 0;
        }
        if vpar[i] <= 0.0 && vpar[i - 1] > 0.0 {
            return (i, StopCriterion::Reversal);
        }
        if speed[i] < min_v {
            min_v = speed[i];
            min_i = i;
        }
        if min_v < 0.6 * peak && speed[i] > p.resurge_frac * peak && speed[i] > min_v + 0.12 * peak {
            return (min_i, StopCriterion::Resurge);
        }
    }
    (n - 1, StopCriterion::End)
}

pub fn analyze_trial(input: &TrialInput, p: &Params) -> Result<TrialMetrics, Invalid> {
    let (p0, tg) = (input.start, input.target);
    let delta = Vec2::new(tg.x - p0.x, tg.y - p0.y);
    let dist = delta.len();
    let u = Vec2::new(delta.x / dist, delta.y / dist);

    let mut path = vec![Point { t: input.t_appear, x: p0.x, y: p0.y }];
    path.extend(input.path.iter().filter(|s| s.t >= input.t_appear && s.t <= input.t_click + 0.005));
    if path.len() < 6 {
        return Err(Invalid::TooFewSamples);
    }
    let (xs, ys) = resample(&path, input.t_appear, input.t_click);
    let n = xs.len();
    if n < 60 {
        return Err(Invalid::TooShort);
    }
    let bq = Biquad::butter2(p.cutoff_hz, FS);
    let (xf, yf) = (bq.filtfilt(&xs), bq.filtfilt(&ys));
    let (vx, vy) = (diff(&xf), diff(&yf));
    let speed: Vec<f64> = vx.iter().zip(&vy).map(|(a, b)| a.hypot(*b)).collect();
    let vpar: Vec<f64> = vx.iter().zip(&vy).map(|(a, b)| a * u.x + b * u.y).collect();
    let (ip, peak) = speed.iter().copied().enumerate().fold((0, 0.0), |m, (i, v)| if v > m.1 { (i, v) } else { m });
    if peak < p.min_peak {
        return Err(Invalid::LowPeak);
    }
    let onset = (0..=ip).find(|&i| speed[i] > p.onset_frac * peak).unwrap_or(0);
    let (stop, criterion) = first_stop(&speed, &vpar, ip, peak, p);

    let ps = Vec2::new(xf[stop] - p0.x, yf[stop] - p0.y);
    let ratio = (ps.x * u.x + ps.y * u.y) / dist;
    let perp = (ps.x * -u.y + ps.y * u.x) / dist;
    let axis = |d: f64, s: f64| (d.abs() >= 0.3 * dist).then(|| s / d);

    // submovimentos corretivos: subidas acima de 10% do pico depois da parada
    let mut corrections = 0;
    let mut low = true;
    for &v in &speed[stop..] {
        if low && v > 0.1 * peak {
            corrections += 1;
            low = false;
        } else if !low && v < 0.05 * peak {
            low = true;
        }
    }

    let invalid = if onset < p.min_reaction_ms {
        Some(Invalid::Anticipated)
    } else if !(0.3..=1.9).contains(&ratio) {
        Some(Invalid::RatioOutOfRange)
    } else {
        None
    };

    Ok(TrialMetrics {
        ratio,
        ratio_x: axis(delta.x, ps.x),
        ratio_y: axis(delta.y, ps.y),
        perp,
        criterion,
        peak_speed: peak,
        distance: dist,
        onset_ms: onset,
        stop_ms: stop,
        click_ms: n - 1,
        corrections,
        correction_dist: (xs[n - 1] - xf[stop]).hypot(ys[n - 1] - yf[stop]),
        speed,
        invalid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use synthetic::{Synth, XorShift};

    fn check(ratio: f64, hz: f64, noise: f64, batch: usize, tol: f64) {
        let mut rng = XorShift::new(7);
        for k in 0..8 {
            let ang = k as f64 * std::f64::consts::FRAC_PI_4;
            let s = Synth { distance: 300.0, angle: ang, ratio_x: ratio, ratio_y: ratio, hz, noise, batch, ..Default::default() };
            let m = analyze_trial(&s.generate(&mut rng), &Params::default()).unwrap();
            let err = (m.ratio - ratio).abs() / ratio;
            assert!(err < tol, "ratio {ratio} hz {hz} ang {k}: medido {:.4} erro {:.3} ({:?})", m.ratio, err, m.criterion);
            assert!(m.is_valid(), "{:?}", m.invalid);
        }
    }

    #[test]
    fn limpo_recupera_ratio() {
        for hz in [1000.0, 4000.0, 8000.0] {
            for r in [0.85, 1.0, 1.12] {
                check(r, hz, 0.0, 1, 0.02);
            }
        }
    }

    #[test]
    fn com_ruido_e_lote() {
        for hz in [1000.0, 4000.0, 8000.0] {
            for r in [0.85, 1.0, 1.12] {
                check(r, hz, 0.15, 1, 0.05);
                check(r, hz, 0.15, 8, 0.05);
            }
        }
    }

    #[test]
    fn eixos_separados() {
        let mut rng = XorShift::new(3);
        let s = Synth { distance: 300.0, angle: 0.6, ratio_x: 1.10, ratio_y: 0.88, noise: 0.0, perp_jitter: 0.0, ..Default::default() };
        let m = analyze_trial(&s.generate(&mut rng), &Params::default()).unwrap();
        assert!((m.ratio_x.unwrap() - 1.10).abs() < 0.02, "{:?}", m.ratio_x);
        assert!((m.ratio_y.unwrap() - 0.88).abs() < 0.02, "{:?}", m.ratio_y);
    }

    #[test]
    fn criterio_a_parada_sustentada() {
        let mut rng = XorShift::new(1);
        let s = Synth { pause_ms: 90.0, noise: 0.0, ..Default::default() };
        let m = analyze_trial(&s.generate(&mut rng), &Params::default()).unwrap();
        assert_eq!(m.criterion, StopCriterion::LowSpeed);
    }

    #[test]
    fn criterio_b_reversao() {
        // passou do alvo e voltou sem pausa
        let mut rng = XorShift::new(1);
        let s = Synth { ratio_x: 1.25, ratio_y: 1.25, pause_ms: 0.0, noise: 0.0, perp_jitter: 0.0, ..Default::default() };
        let m = analyze_trial(&s.generate(&mut rng), &Params::default()).unwrap();
        assert_eq!(m.criterion, StopCriterion::Reversal);
        assert!((m.ratio - 1.25).abs() < 0.03, "{}", m.ratio);
    }

    #[test]
    fn criterio_c_submovimento_emendado() {
        // parou antes e emendou a correção na mesma direção sem zerar a velocidade
        let mut rng = XorShift::new(1);
        let s = Synth { ratio_x: 0.75, ratio_y: 0.75, pause_ms: 0.0, overlap_ms: 60.0, noise: 0.0, perp_jitter: 0.0, ..Default::default() };
        let m = analyze_trial(&s.generate(&mut rng), &Params::default()).unwrap();
        assert_eq!(m.criterion, StopCriterion::Resurge);
        assert!((m.ratio - 0.75).abs() < 0.08, "{}", m.ratio);
    }

    #[test]
    fn antecipado_invalido() {
        let mut rng = XorShift::new(1);
        let s = Synth { reaction_ms: 40.0, ..Default::default() };
        let m = analyze_trial(&s.generate(&mut rng), &Params::default()).unwrap();
        assert_eq!(m.invalid, Some(Invalid::Anticipated));
    }
}
