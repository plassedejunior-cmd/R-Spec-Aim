# Primeira Parada — spec técnica (desktop)

Aim trainer desktop que mede **onde a mira para no primeiro movimento** (antes ou depois do alvo) e, com base nisso, sugere uma mudança de sensibilidade. Velocidade e acerto de clique são secundários. A métrica central é a precisão do movimento balístico inicial.

Este documento é o ponto de partida do projeto. Leia inteiro antes de criar código.

---

## 1. Objetivo técnico

1. Capturar movimento do mouse **bruto** (counts do sensor), sem aceleração do sistema, em todos os eventos do polling rate, com timestamp de alta resolução.
2. Apresentar alvos pequenos e registrar a trajetória completa de cada tentativa.
3. Detectar o **fim do submovimento primário** ("primeira parada") de forma robusta.
4. Calcular a razão `distância percorrida até a primeira parada / distância necessária`, no geral e por eixo (horizontal e vertical).
5. Gerar uma sugestão de sensibilidade:
   - com fatores **separados para X e Y** se o mouse/jogo permitir ajuste independente;
   - com **um fator único** caso contrário, avisando quando os eixos estiverem desequilibrados.
6. Expressar o resultado em unidades físicas (**cm/360**) e converter para o valor de sensibilidade de jogos específicos.

---

## 2. Plataforma e stack

**Alvo inicial:** Windows 10/11 (é onde está o público). macOS/Linux ficam fora do MVP, mas a camada de input deve ficar isolada atrás de um trait para permitir portar depois.

**Stack recomendada:**

| camada | escolha | motivo |
|---|---|---|
| linguagem | Rust (stable) | controle de threads e tempo, sem GC pausando a captura |
| input | `windows` crate, Raw Input (`RegisterRawInputDevices` + `GetRawInputBuffer`) em thread dedicada | único caminho que entrega todos os counts sem aceleração e por dispositivo |
| tempo | `QueryPerformanceCounter` | resolução sub-microssegundo |
| janela/render | `winit` + `wgpu` | precisa de 3D na fase 2 |
| UI (menus, resultados) | `egui` (via `egui-wgpu`) | rápido de iterar |
| análise | módulo Rust puro, sem dependência de UI | testável com trajetórias sintéticas |
| persistência | JSON (sessão) + CSV/binário (amostras brutas) | permite reanalisar sessões antigas quando o algoritmo mudar |

Se preferir C++: SDL3 + Dear ImGui funciona, mas mantenha a thread própria de Raw Input. O timestamp de evento do SDL marca o processamento, e a gente precisa do momento da leitura.

---

## 3. Arquitetura

```
┌────────────────────┐   ring buffer lock-free   ┌──────────────────────┐
│ input thread       │ ────────────────────────▶ │ sim loop (main)      │
│ - janela message-  │   RawSample {t, dx, dy,   │ - integra deltas     │
│   only             │   device, flags}          │ - estado do trial    │
│ - GetRawInputBuffer│                           │ - render (wgpu)      │
│ - QPC timestamp    │                           └──────────┬───────────┘
└────────────────────┘                                      │ trial concluído
                                                            ▼
                                               ┌──────────────────────┐
                                               │ analysis (puro)      │
                                               │ - reamostragem       │
                                               │ - filtro             │
                                               │ - primeira parada    │
                                               │ - métricas           │
                                               └──────────┬───────────┘
                                                          ▼
                                               ┌──────────────────────┐
                                               │ recommender (puro)   │
                                               └──────────────────────┘
```

Regras:
- A thread de input **nunca** depende do frame rate. O render pode travar e a captura continua.
- Todo dado bruto de cada trial é gravado. Análise é sempre recomputável a partir dele.
- `analysis` e `recommender` não sabem que existe janela, GPU ou Windows.

### Estrutura de pastas sugerida

```
/crates
  /input        # trait MouseSource + impl windows raw input
  /core         # tipos: RawSample, Trial, Session, unidades
  /analysis     # detecção de primeira parada, métricas
  /recommender  # sugestão de sensibilidade, conversão por jogo
  /app          # winit + wgpu + egui, protocolo de trials
/data
  games.toml    # constantes de yaw/pitch por jogo
/tests
  /synthetic    # gerador de trajetórias + casos de teste
```

---

## 4. Captura de input (Windows)

- Criar uma janela message-only (`HWND_MESSAGE`) na thread de input e registrar `RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x02, dwFlags: RIDEV_INPUTSINK, hwndTarget }`.
- Ler em lote com `GetRawInputBuffer` (evita perder eventos em mouses de 4–8 kHz). Fallback: `WM_INPUT` + `GetRawInputData`.
- Para cada `RAWMOUSE`:
  - aceitar só `MOUSE_MOVE_RELATIVE`; descartar `MOUSE_MOVE_ABSOLUTE` (tablet, RDP, VM) e avisar o usuário;
  - guardar `lLastX`, `lLastY` (counts), `hDevice`, flags de botão.
- **Timestamp:** QPC no momento da leitura. Em leitura em lote, os eventos chegam juntos; distribuir o timestamp linearmente dentro do lote usando o polling rate estimado e marcar a amostra com `batched: true`. Registrar essa limitação nos dados.
- **Polling rate:** estimar pela mediana do intervalo entre eventos durante movimento contínuo. Exibir para o usuário.
- **Clique:** o botão também vem do Raw Input, com o mesmo relógio. Nunca usar `WM_LBUTTONDOWN` para o tempo do clique.
- **Dispositivo:** se houver mais de um mouse, usar o `hDevice` que gerou o primeiro movimento do teste e ignorar os outros.
- **Configurações do sistema:** ler `SPI_GETMOUSE` (aceleração / "aumentar precisão do ponteiro") e `SPI_GETMOUSESPEED` só para informar. Raw Input já ignora os dois.
- Cursor: `ClipCursor` na janela + cursor escondido durante o teste. A posição exibida vem só da integração dos counts.

---

## 5. Unidades e calibração

Definições (todas em `core`):

- `count`: unidade bruta do sensor.
- `dpi_nominal`: informado pelo usuário.
- `dpi_real`: medido na calibração (sensores costumam errar alguns % do nominal).
- `cm_per_count = 2.54 / dpi_real`
- `deg_per_count`: sensibilidade efetiva no modo 3D.
- `cm_per_360 = (360 / deg_per_count) * cm_per_count`

**Calibração de DPI (opcional, recomendada):** usuário arrasta o mouse ao longo de uma régua por uma distância conhecida (ex.: 10 cm) três vezes. `dpi_real = counts_medidos / (cm / 2.54)`, mediana das três. Guardar por `hDevice`.

---

## 6. Modos de teste

### Fase 1 — modo 2D (MVP)
- Arena plana, mira integrada em pixels: `pos += counts * px_per_count`.
- `px_per_count` configurável (padrão derivado para que 1 cm de mão ≈ uma fração fixa da largura da tela).
- Serve para validar captura e algoritmo. A sugestão sai como **fator relativo**.

### Fase 2 — modo 3D (o que importa para jogos)
- Câmera em primeira pessoa, alvos esféricos num domo em volta do jogador.
- `yaw += dx * deg_per_count`, `pitch -= dy * deg_per_count * pitch_ratio`, pitch limitado a ±89°.
- FOV horizontal configurável. Projeção igual à dos jogos (FOV horizontal fixo, vertical derivado do aspect ratio).
- Distâncias e tamanhos de alvo definidos em **graus**, para independer de resolução.
- Sensibilidade inicial = a do jogo do usuário, convertida via `games.toml`, para o teste rodar com a mesma sensação.
- A sugestão sai em cm/360 e no valor do jogo.

---

## 7. Protocolo de trial

- Sessão padrão: 3 alvos de aquecimento + 30 válidos (configurável).
- **Direções:** 8 setores (0°, 45°, …, 315°), sorteados de forma balanceada. Garantir no mínimo 8 trials predominantemente horizontais e 8 predominantemente verticais.
- **Distâncias:**
  - 2D: 150–450 px.
  - 3D: 10°–60° de rotação, em 3 faixas (curta/média/longa), balanceadas.
- **Tamanho do alvo:** pequeno (2D: raio 10–12 px; 3D: ~0,6–1,0° de diâmetro angular). Alvo pequeno força o ajuste fino e separa bem a fase balística da corretiva.
- **Início do trial:** o alvo só aparece depois que a mira fica parada (< limiar de velocidade) por 200 ms, mais um atraso aleatório de 300–700 ms. Isso evita movimento antecipado.
- **Fim do trial:** clique dentro do alvo. Cliques fora contam como erro e o trial continua.
- **Timeout:** 4 s. Trial descartado.
- Feedback pós-trial (opcional, configurável): rastro com marcador da primeira parada. Deixar desligável, porque feedback pode alterar o comportamento que estamos medindo.

---

## 8. Algoritmo de análise

Entrada: lista de `RawSample` do trial, posição inicial, posição do alvo, tempo do clique.

1. **Integração:** somar counts para obter posição acumulada (x, y) em counts. No 3D, converter para (yaw, pitch) em graus.
2. **Reamostragem:** grade uniforme de 1 ms com interpolação linear da posição acumulada.
3. **Filtro:** passa-baixa Butterworth de 2ª ordem, corte ~15–20 Hz, aplicado ida e volta (zero-phase / filtfilt). Derivar velocidade e aceleração por diferenças centrais sobre o sinal filtrado.
4. **Projeção:** vetor unitário `u` do início ao alvo. Velocidade ao longo do eixo `v_par = v · u`; perpendicular `v_perp`.
5. **Início do movimento:** primeiro instante em que `|v|` passa de 5% do pico.
6. **Fim do submovimento primário (primeira parada)** = o primeiro que ocorrer, depois do pico de velocidade, entre:
   - a. `|v|` abaixo de 5–8% do pico por ≥ 20 ms;
   - b. `v_par` cruza zero (reversão: passou do alvo e voltou);
   - c. mínimo local de `|v|` seguido de nova subida acima de 20% do pico (submovimento corretivo emendado sem parar).
   Registrar qual critério disparou.
7. **Métricas do trial:**
   - `ratio = ((p_stop - p0) · u) / D` (1.0 = em cima do alvo; > 1 passou; < 1 parou antes);
   - `ratio_x = Δx_stop / Δx_alvo` se |Δx_alvo| ≥ 30% de D; idem `ratio_y`;
   - erro perpendicular normalizado;
   - número de submovimentos corretivos após a parada;
   - distância de correção (parada → clique);
   - tempo até a parada, tempo até o clique, erros de clique.
8. **Validade:** descartar trial se o pico de velocidade for muito baixo, se `ratio` estiver fora de [0.3, 1.9], se houver amostra absoluta, ou se o movimento começou antes do alvo aparecer.

Agregação da sessão: **mediana** e IQR de `ratio`, `ratio_x`, `ratio_y`. Intervalo de confiança da mediana por bootstrap (1000 reamostragens).

Referência de base: modelo de submovimentos otimizados (Meyer et al., 1988) e a literatura de "primary submovement endpoint". O código deve citar a referência nos comentários do módulo.

---

## 9. Recomendação de sensibilidade

Parâmetros (configuráveis, com estes padrões):
- `target_ratio = 0.95` (parar levemente antes é o padrão de quem mira bem; mirar em 1.0 empurra para overshoot)
- `damping = 0.6` (corrige 60% do erro por rodada)
- `clamp = ±25%`
- `min_trials_per_axis = 8`

Cálculo:
```
factor(r) = clamp(1 + damping * (target_ratio / r - 1), 0.75, 1.25)
```

- **Pergunta obrigatória antes da sugestão:** "seu mouse ou jogo permite ajustar horizontal e vertical separadamente?"
  - **Sim:** `fx = factor(median(ratio_x))`, `fy = factor(median(ratio_y))`. Eixo com amostra insuficiente usa a mediana geral e é sinalizado.
  - **Não:** `f = factor(median(ratio))`. Se `|median(ratio_x) - median(ratio_y)| > 0.10`, avisar o desequilíbrio e indicar se o jogo tem ajuste de proporção vertical.
- **Manter:** se o IC da mediana cruza a faixa [0.90, 1.00], ou se `|f - 1| < 0.03`, recomendar não mudar.
- **Saída:**
  - fator relativo (ex.: ×0.93, −7%);
  - cm/360 atual → sugerido (modo 3D ou com DPI calibrado);
  - valor no jogo escolhido, via `games.toml`;
  - alternativa por DPI, quando o software do mouse permitir (ex.: 800 → 744).
- Fluxo "testar com a sugestão": aplicar o fator dentro do teste e rodar outra sessão, mostrando a comparação entre rodadas.

### `data/games.toml`
Constante de graus por count de cada jogo (`yaw`, e `pitch` quando diferente). Exemplos a **verificar antes de usar**: Source/CS2 e Apex `0.022`, Valorant `0.07`, Overwatch 2 `0.0066`. Cada entrada precisa ter fonte e data de verificação.

```
deg_per_count = sens_jogo * yaw_jogo
sens_nova = (deg_per_count * f) / yaw_jogo
```

---

## 10. Persistência

Por sessão, em `%APPDATA%/PrimeiraParada/sessions/<timestamp>/`:
- `session.json`: config (modo, DPI, FOV, sens, ganho), dispositivo, polling rate, versão do algoritmo, métricas agregadas, recomendação.
- `trials.json`: metadados e métricas de cada trial.
- `raw.bin` (ou CSV): todas as `RawSample` com índice do trial.

Incluir `analysis_version` para reprocessar sessões antigas quando o algoritmo mudar.

---

## 11. Testes

- **Gerador sintético** em `/tests/synthetic`: perfil de mínimo jerk para o submovimento primário, com amplitude controlada (ex.: 0.85, 1.0, 1.12 da distância), mais submovimentos corretivos, ruído e quantização em counts. Gerar a 1000, 4000 e 8000 Hz, com e sem lote.
- O detector precisa recuperar o `ratio` sintético com erro < 2% nos casos limpos e < 5% com ruído.
- Testes dos três critérios de parada (a, b, c) isolados.
- Testes do recommender: valores conhecidos de ratio → fatores esperados, clamp, regra de "manter", eixos desequilibrados.
- Modo **replay**: reabrir uma sessão gravada e rodar a análise de novo, para depuração visual (plot de velocidade com marcadores).

---

## 12. Fases

1. **Captura:** thread de Raw Input, QPC, ring buffer, ferramenta de debug que mostra polling rate, counts e gráfico de velocidade em tempo real.
2. **Análise:** módulo `analysis` + gerador sintético + testes. Sem UI.
3. **MVP 2D:** arena, protocolo de trial, resultado com fator relativo, pergunta dos eixos, "testar com a sugestão".
4. **Calibração de DPI** e saída em cm.
5. **Modo 3D** com FOV, `games.toml`, saída em cm/360 e valor no jogo.
6. Comparação entre sessões e histórico.

---

## 13. Fora do escopo (por enquanto)

- macOS e Linux.
- Rastreamento de alvo em movimento (tracking). O teste mede flicks.
- Integração com jogos (ler/escrever config de jogo). A sugestão é sempre aplicada pelo usuário.
- Contas, nuvem, ranking.

---

## 14. Questões abertas

- Limiar exato de parada (5% vs 8%) e duração mínima: calibrar com dados reais.
- O feedback visual pós-trial altera o comportamento medido? Comparar sessões com e sem.
- O `target_ratio` ideal varia com a distância? Pode valer usar um alvo por faixa de distância.
- Vale ponderar trials longos mais que curtos na recomendação (flicks longos dependem mais da sensibilidade)?
- Latência de exibição (refresh rate, vsync) muda o comportamento: registrar refresh rate e modo de apresentação na sessão.
