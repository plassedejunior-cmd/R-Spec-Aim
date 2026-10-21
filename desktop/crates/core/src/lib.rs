//! Tipos compartilhados do R-Spec Aim: amostras brutas, trials e unidades físicas.
//! Nada aqui depende de janela, GPU ou sistema operacional.

/// Versão do algoritmo de análise gravada em cada sessão, para reprocessar sessões antigas.
pub const ANALYSIS_VERSION: u32 = 1;

static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Relógio único do app, em segundos. `Instant` é QueryPerformanceCounter no Windows,
/// então captura, render e cliques ficam na mesma base de tempo com resolução sub-µs.
pub fn now() -> f64 {
    EPOCH.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

/// Um evento de movimento relativo vindo do sensor, como o Raw Input entrega.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawSample {
    /// Tempo em segundos, relógio monotônico de alta resolução (QPC no Windows).
    pub t: f64,
    /// Deslocamento em counts do sensor.
    pub dx: i32,
    pub dy: i32,
    /// Identificador opaco do dispositivo (hDevice no Windows).
    pub device: u64,
    /// Bits de botão (veja `buttons`).
    pub buttons: u16,
    /// O timestamp foi distribuído dentro de um lote lido de uma vez.
    pub batched: bool,
}

pub mod buttons {
    pub const LEFT_DOWN: u16 = 0x0001;
    pub const LEFT_UP: u16 = 0x0002;
}

/// Posição acumulada (em counts, ou em graus no modo 3D) num instante.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub t: f64,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vec2 {
    pub x: f64,
    pub y: f64,
}

impl Vec2 {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    pub fn len(self) -> f64 {
        self.x.hypot(self.y)
    }
}

/// Entrada da análise de um trial, já integrada para posição.
#[derive(Clone, Debug)]
pub struct TrialInput {
    /// Trajetória (posição acumulada), em ordem de tempo.
    pub path: Vec<Point>,
    pub start: Vec2,
    pub target: Vec2,
    /// Instante em que o alvo apareceu (s).
    pub t_appear: f64,
    /// Instante do clique que acertou o alvo (s).
    pub t_click: f64,
}

/// Integra counts em posição acumulada a partir da origem, multiplicando por `scale`.
pub fn integrate(samples: &[RawSample], scale_x: f64, scale_y: f64) -> Vec<Point> {
    let (mut x, mut y) = (0.0, 0.0);
    samples
        .iter()
        .map(|s| {
            x += s.dx as f64 * scale_x;
            y += s.dy as f64 * scale_y;
            Point { t: s.t, x, y }
        })
        .collect()
}

/// Estima o polling rate (Hz) pela mediana dos intervalos entre eventos de movimento contínuo.
/// Intervalos acima de `gap` segundos são tratados como pausa e ignorados.
pub fn estimate_polling_hz(samples: &[RawSample], gap: f64) -> Option<f64> {
    let mut dts: Vec<f64> = samples
        .windows(2)
        .filter(|w| !w[1].batched && (w[1].dx != 0 || w[1].dy != 0))
        .map(|w| w[1].t - w[0].t)
        .filter(|&dt| dt > 0.0 && dt < gap)
        .collect();
    if dts.len() < 20 {
        return None;
    }
    dts.sort_by(|a, b| a.total_cmp(b));
    Some(1.0 / dts[dts.len() / 2])
}

/// Unidades físicas (seção 5 da spec).
pub mod units {
    pub fn cm_per_count(dpi: f64) -> f64 {
        2.54 / dpi
    }
    pub fn cm_per_360(deg_per_count: f64, dpi: f64) -> f64 {
        360.0 / deg_per_count * cm_per_count(dpi)
    }
    /// DPI real a partir de counts medidos numa distância conhecida em cm.
    pub fn dpi_from_ruler(counts: f64, cm: f64) -> f64 {
        counts / (cm / 2.54)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cm360_cs2() {
        // CS2: sens 1.0, yaw 0.022, 800 DPI => ~51.95 cm/360
        let v = units::cm_per_360(1.0 * 0.022, 800.0);
        assert!((v - 51.95).abs() < 0.01, "{v}");
    }

    #[test]
    fn polling_1000hz() {
        let s: Vec<RawSample> = (0..200)
            .map(|i| RawSample { t: i as f64 * 0.001, dx: 1, dy: 0, device: 1, buttons: 0, batched: false })
            .collect();
        let hz = estimate_polling_hz(&s, 0.05).unwrap();
        assert!((hz - 1000.0).abs() < 1.0, "{hz}");
    }
}
