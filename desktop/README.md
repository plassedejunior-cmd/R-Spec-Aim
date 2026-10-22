# R-Spec Aim (desktop)

Calibrador de sensibilidade para Windows 11, seguindo a spec "Primeira Parada" (`../claude.md`).

## Usar

1. Rode `R-Spec-Aim.exe` (pasta `../build`). O Windows pode avisar que o app não é assinado: "Mais informações" → "Executar assim mesmo".
2. **Captura:** mova o mouse em círculos rápidos e confira o polling rate medido.
3. **Configurar e testar:** escolha "Só o mouse" (sugestão em DPI e polling rate) ou um jogo (sugestão também na sensibilidade do jogo e em cm/360), e informe o DPI e a sensibilidade que você usa. Eles servem só para converter o resultado. Aperte F11 para tela cheia e clique em Começar.
4. **Resultado:** responda se dá para ajustar horizontal e vertical separados e veja a sugestão. Para testar a sugestão, mude você mesmo o DPI ou a sensibilidade, atualize os valores e rode outra rodada. Com duas rodadas ou mais, o app aponta a de melhor equilíbrio entre velocidade e precisão.

**Teste rápido (escada):** começa bem baixo, em 400 DPI equivalente, independente do que você usa. A cada bloco de 8 alvos, sobe uns 30% (400, 520, 680, 880, 1140...) até você começar a piorar, medindo velocidade e precisão juntas. Depois refina em volta do melhor nível: repete esse nível e dois meios-degraus vizinhos em ordem misturada, para separar o efeito da sensibilidade do aquecimento e do cansaço. Tem dois modos: "Escala só no app", em que só a mira do app muda, e "100% nativo", em que o app pede o DPI de cada bloco (em passos de 50) e você troca no software do mouse.

**Movimento nativo:** no teste a mira anda exatamente como o cursor do Windows (counts do sensor × velocidade do ponteiro, sem aceleração, em pixels físicos). O app nunca muda a sensibilidade nem o DPI durante o teste.

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
