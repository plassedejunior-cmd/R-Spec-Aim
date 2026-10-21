# R-Spec Aim (desktop)

Calibrador de sensibilidade para Windows 11, seguindo a spec "Primeira Parada" (`../claude.md`).

## Usar

1. Rode `R-Spec-Aim.exe` (pasta `../build`). O Windows pode avisar que o app não é assinado: "Mais informações" → "Executar assim mesmo".
2. **Captura:** mova o mouse em círculos rápidos e confira o polling rate medido.
3. **Configurar e testar:** escolha o jogo, sensibilidade, DPI e FOV. Aperte F11 para tela cheia e clique em Começar.
4. **Resultado:** responda se dá para ajustar horizontal e vertical separados, veja a sugestão e use "Testar com a sugestão" para rodar de novo. Com duas rodadas ou mais, o app aponta a de melhor equilíbrio entre velocidade e precisão.

Cada rodada é gravada em `%APPDATA%\RSpecAim\sessions\<timestamp>\` (session.json, trials.json, raw.csv).

## O que mede

- **Primeira parada:** onde o movimento balístico termina, em fração da distância até o alvo (1,00 = em cima; alvo 0,95). Geral e por eixo.
- **Velocidade x precisão:** throughput efetivo (bits/s), no estilo ISO 9241-9. A precisão vem da dispersão da primeira parada e a velocidade do tempo do início do movimento até o clique.
- A sugestão de fator vem da mediana da primeira parada (seção 9 da spec). Entre as rodadas testadas, só concorrem as com mediana entre 0,85 e 1,08, e vence a de maior throughput.

## Latência

A mira é desenhada direto dos counts do Raw Input (thread dedicada, prioridade crítica, relógio QPC), com vsync desligado e repaint contínuo. Não passa pelo cursor do Windows nem por aceleração.

## Desenvolver

```
cargo test                                         # core, analysis, recommender, protocolo
cargo build --release -p rspec-aim --target x86_64-pc-windows-gnu
```

Crates: `core` (tipos e unidades), `input` (trait + Raw Input Windows), `analysis` (primeira parada, stats, gerador sintético), `recommender` (fator e melhor rodada), `app` (eframe/egui).

Pendente: calibração de DPI real com régua, modo 3D com FOV, `data/games.toml` com fontes verificadas, histórico entre sessões.
