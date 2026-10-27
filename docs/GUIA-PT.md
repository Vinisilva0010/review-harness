# Guia em português: review-harness

Este guia explica o projeto em linguagem simples e mostra como apresentá-lo em uma entrevista,
andando pelo repositório no GitHub.

## 1. O que é, em uma frase

O **review-harness** olha para cada pull request (PR) e decide **qual verificação é a certa para aquele
risco**: primeiro as baratas e exatas (testes, scanners, regras), e só depois, se valer a pena, uma
revisão com IA, com um limite de gasto. No final, ele diz: **passou**, **precisa de um humano** ou
**falhou**.

A ideia vem do texto da Anthropic sobre como o time do Claude Code revisa código: a pergunta não é
"um humano leu?", e sim **"o que pode dar errado e quem pega isso melhor, máquina ou pessoa?"**.

O diferencial: **funciona sem nenhuma IA** e **gasta pouco**. Se não houver chave de API, a camada de
IA é pulada e o PR mostra "skipped: no provider configured". Quem quiser usa a própria chave.

## 2. As camadas, em palavras simples

1. **Classificar o risco (`classify`)**: lê o diff (o que mudou) e dá nota baixo, médio ou alto. Mexeu só
   em documentação? Baixo. Mexeu no workflow do GitHub, em autenticação ou colou algo parecido com
   senha? Alto. Não gasta nenhum token de IA.
2. **Verificar (`verify`)**: roda os comandos do projeto (formatação, clippy, testes) e guarda a
   **evidência**: código de saída, tempo e o final da saída. Se não há nenhum comando configurado,
   o resultado é "sem evidência", **nunca** "passou". Não dá para dizer "pronto" sem prova.
3. **Segurança (`security`)**: roda o `cargo-audit` (dependências com falhas conhecidas) e o `gitleaks`
   (segredos vazados). Se a ferramenta não está instalada, o resultado é "pulado", e **pulado nunca
   conta como aprovado**.
4. **Revisão com IA (`review`)**: só roda se o risco for médio ou alto, se houver chave configurada e se
   as camadas anteriores não falharam. Manda **só os trechos do diff**, nunca o repositório inteiro.
   Tem **orçamento de tokens** (recusa se passar), **no máximo 3 rodadas**, e a resposta precisa ser um
   JSON no formato certo. A IA é **só conselheira**: pode pedir mudanças, **nunca aprovar**.
5. **Validador (sem custo)**: se a IA aponta um erro numa linha que **não faz parte** das linhas
   adicionadas no PR, o apontamento é descartado e contado como "alucinação". Assim dá para medir
   quanto a IA inventa.
6. **Gate (a decisão)**: junta tudo. Risco alto **sempre** pede um humano. Falha de teste ou segredo
   achado = falhou. Qualquer coisa não provada (sem evidência, scanner pulado, IA falhou) = precisa de
   humano.
7. **Métricas (`metrics`) e aprendizado (`learn`)**: conta rodadas por PR, achados por gravidade, taxa de
   alucinação, tokens gastos e reverts no git. Quando um bug escapa, o `learn` escreve uma regra no
   `REVIEW.md` (que a IA passa a ler) e cria um esqueleto de teste de regressão.

## 3. Como rodar (para mostrar ao vivo)

```bash
cargo install --path crates/harness-cli
review-harness init
review-harness classify --base origin/main
review-harness run --base origin/main      # 0 passou, 1 falhou, 3 precisa de humano
review-harness metrics --records examples/run-records
```

## 4. Roteiro de demonstração pelo GitHub (5 a 7 minutos)

Abra o repositório e siga esta ordem.

1. **`README.md`** (começo): mostre o diagrama Mermaid das camadas. Diga: "primeiro as camadas baratas
   e exatas, depois a IA, só onde o risco justifica, e no fim um gate que decide."
2. **Aba Pull requests**: os PRs estão **empilhados** (cada fase aponta para a anterior), como numa
   empresa grande: PR #1 verify, #2 diff e risco, #3 segurança, #4 revisão com IA, #5 gate e Action,
   #6 métricas e learn, #7 documentação. Diga: "cada fase é um PR pequeno e revisável, com a evidência
   dos testes colada no `PROGRESS.md`."
3. **`crates/harness-core/src/gate.rs`**: mostre a função `decide` e o teste da tabela completa.
   Diga: "são 162 combinações testadas contra um oráculo escrito à parte. Risco alto nunca passa,
   e a IA só pode deixar o resultado mais rígido, nunca mais frouxo."
4. **`crates/harness-core/src/review/finding.rs`**: mostre `parse_findings` e `validate`.
   Diga: "o formato de saída não tem campo de veredito, então o modelo não consegue aprovar nada. Achado
   fora das linhas do diff é descartado e vira métrica de alucinação."
5. **`crates/harness-core/src/review/prompt.rs`**: mostre a defesa contra prompt injection.
   Diga: "o diff é dado não confiável, vai dentro de marcadores com um código aleatório derivado do
   conteúdo, e instruções dentro do diff viram um achado, não uma ordem."
6. **`SECURITY.md`**: mostre a tabela de ameaças. Destaque: chave só em variável de ambiente e
   mascarada em qualquer erro (há teste que prova), `pull_request` e não `pull_request_target`,
   actions fixadas por SHA, timeouts e limites de tamanho em tudo.
7. **`.github/workflows/review-harness.yml`** e **`action.yml`**: mostre as permissões mínimas, o
   `persist-credentials: false` e que a config é lida do **commit base**, para o PR não mudar as
   próprias regras. Mostre o comentário do bot em um PR: "AI review skipped: no provider configured".
8. **`docs/BRANCH_PROTECTION.md` e `.github/CODEOWNERS`**: "o humano é obrigatório onde importa;
   isso é configurado no GitHub, com passo a passo."
9. **`examples/run-records/` + `review-harness metrics`**: mostre as métricas geradas (taxa de
   alucinação, rodadas por PR, tokens).
10. **`ARCHITECTURE.md`**, seção "What Anthropic does with large budgets": explique a troca de
    agentes paralelos e verificadores por scanners, validador e humano.

## 5. Perguntas prováveis e respostas curtas

**Por que não usar só IA para revisar tudo?**
Custa caro, erra e não é reproduzível. Scanners e regras são exatos e grátis; a IA fica para o que eles
não enxergam, e só onde o risco justifica.

**E se a IA inventar um problema?**
O validador descarta achados fora das linhas adicionadas e conta isso como alucinação. Além disso a IA
é só conselheira: no máximo pede um humano.

**Como você controla o custo?**
Estimo os tokens antes de enviar e recuso se passar do orçamento do PR (somado entre rodadas). Mando só
os trechos do diff, máximo 3 rodadas, e a IA nem roda se uma camada barata já falhou.

**O que acontece sem chave de API?**
Tudo funciona. A camada de IA mostra "skipped: no provider configured" e o gate decide com o resto.

**Como evita prompt injection?**
O diff é tratado como dado, dentro de marcadores que o diff não consegue falsificar. O formato de saída
não tem campo de aprovação, então mesmo um modelo enganado não consegue passar o PR.

**Como protege a chave de API?**
Só vem de variável de ambiente, nunca do arquivo de config, e é mascarada em erros e logs. Nos PRs de
fork a chave nem chega ao workflow.

**Por que `pull_request` e não `pull_request_target`?**
`pull_request_target` roda com segredos mesmo para código de fork; combinado com checkout do PR vira
execução de código não confiável com segredos. Usei `pull_request`.

**Um scanner não instalado reprova ou aprova?**
Nenhum dos dois: fica "pulado" e o gate pede um humano. Pular nunca é aprovar.

**Por que Rust?**
Binário único, rápido, sem runtime, e `forbid(unsafe_code)`. Para uma ferramenta que processa entrada
hostil, a segurança de memória importa.

**Por que sem tokio/reqwest?**
Um processo curto por PR não precisa de async. O `ureq` bloqueante reduz dependências e memória
(a máquina de build tem 3,7 GB).

**Como testou sem gastar dinheiro com API?**
Um `MockProvider`, um transporte HTTP falso e fixtures JSON no formato real de cada provedor. Nenhum
teste usa rede.

**O que ficou de fora ou é limitação?**
O gitleaks e o workflow do GitHub só foram validados por stubs e execução local (veja `BLOCKERS.md`);
o primeiro run no GitHub é o teste real. A IA pode errar. Não há agentes paralelos nem verificadores por
causa do orçamento.

**O que você faria com mais orçamento?**
Revisores especializados em paralelo e uma segunda chamada que confirme cada achado antes de mantê-lo,
exatamente o que o texto da Anthropic descreve.

**Como o sistema aprende?**
O `learn` transforma um bug que escapou em uma regra no `REVIEW.md` (lida da branch base) e em um teste
de regressão. As métricas mostram se a revisão está melhorando.
