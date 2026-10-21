//! Constantes de graus por count (yaw) por jogo. Verificar cada valor antes de confiar
//! (seção 9 da spec). Vão migrar para data/games.toml com fonte e data de verificação.

pub struct Game {
    pub name: &'static str,
    pub yaw: f64,
    pub fov: f64,
    /// Onde o jogo deixa ajustar o eixo vertical separado, se deixa.
    pub vertical: &'static str,
}

pub const GAMES: &[Game] = &[
    Game { name: "Só o mouse (sem jogo)", yaw: 0.0, fov: 0.0, vertical: "DPI X/Y separado no software do mouse" },
    Game { name: "Counter-Strike 2", yaw: 0.022, fov: 106.26, vertical: "m_pitch (padrão 0,022)" },
    Game { name: "Valorant", yaw: 0.07, fov: 103.0, vertical: "Multiplicador de sensibilidade vertical" },
    Game { name: "Apex Legends", yaw: 0.022, fov: 110.0, vertical: "proporção vertical no settings.cfg" },
    Game { name: "Overwatch 2", yaw: 0.0066, fov: 103.0, vertical: "Sensibilidade vertical (%)" },
    Game { name: "Outro (yaw manual)", yaw: 0.022, fov: 103.0, vertical: "ajuste vertical do jogo, se existir" },
];

/// Calibra só o mouse: a sugestão sai em DPI e polling rate, sem converter para jogo.
pub const MOUSE_ONLY: usize = 0;
pub const CUSTOM: usize = 5;
