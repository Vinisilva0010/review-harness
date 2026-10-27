# Two-minute pitch

Each paragraph is followed by its Portuguese translation. Roughly 280 words in English, about two
minutes at a calm speaking pace.

---

Most teams ask the wrong question about code review. They ask, "Did a human read this?" The better
question is, "What could go wrong with this change, and which check, automated or human, is the right
one to catch it?" That is the idea behind review-harness, which I built after reading how the Claude Code
team does code review.

A maioria dos times faz a pergunta errada sobre revisão de código. Eles perguntam: "Um humano leu isso?".
A pergunta melhor é: "O que pode dar errado nesta mudança, e qual verificação, automática ou humana, é a
certa para pegar isso?". Essa é a ideia do review-harness, que construí depois de ler como o time do
Claude Code faz revisão de código.

---

It is a Rust command-line tool and a GitHub Action. Every pull request goes through layers, cheapest
first. A risk classifier reads the diff and rates it low, medium or high, with no tokens spent. Then it
runs your own checks and keeps the evidence, runs security scanners, and only then, if the risk justifies
the spend, asks a language model for a review.

É uma ferramenta de linha de comando em Rust e uma GitHub Action. Cada pull request passa por camadas, das
mais baratas para as mais caras. Um classificador de risco lê o diff e dá nota baixa, média ou alta, sem
gastar tokens. Depois roda as suas verificações e guarda a evidência, roda scanners de segurança e, só
então, se o risco justificar o gasto, pede uma revisão a um modelo de linguagem.

---

The AI is strictly a consultant. Its output must match a JSON schema with no verdict field, so it cannot
approve anything. A zero-cost validator drops any finding that points outside the lines the author added,
and counts it as a hallucination, so we can measure how much the model invents. There is a hard token
budget per pull request, a maximum of three rounds, and if the model's answer is invalid twice, the tool
fails closed and asks a human.

A IA é estritamente uma consultora. A saída dela precisa seguir um esquema JSON sem campo de veredito,
então ela não consegue aprovar nada. Um validador sem custo descarta qualquer achado que aponte para fora
das linhas adicionadas pelo autor e o conta como alucinação, para medirmos quanto o modelo inventa. Há um
orçamento rígido de tokens por pull request, no máximo três rodadas, e, se a resposta do modelo for
inválida duas vezes, a ferramenta falha de forma segura e chama um humano.

---

Security was a design input, not an afterthought. Keys only come from environment variables and are
redacted everywhere, with a test that proves it. The diff is treated as untrusted data against prompt
injection. The workflow uses pull_request, never pull_request_target, with minimal permissions and
actions pinned by commit hash. A scanner that is missing counts as skipped, never as passed, and high-risk
changes always require a person.

A segurança foi uma entrada do projeto, não um detalhe final. As chaves só vêm de variáveis de ambiente e
são mascaradas em todo lugar, com um teste que prova isso. O diff é tratado como dado não confiável contra
prompt injection. O workflow usa pull_request, nunca pull_request_target, com permissões mínimas e actions
fixadas por hash de commit. Um scanner ausente conta como pulado, nunca como aprovado, e mudanças de alto
risco sempre exigem uma pessoa.

---

The differentiator is cost. Anthropic can afford parallel reviewer agents plus verifier agents. I built for
a small budget instead: deterministic layers first, one focused model call where it matters, and metrics
on rounds, hallucination rate and tokens. With no API key at all, it still works. When a bug escapes, one
command turns it into a rule and a regression test. That is the part I would love to discuss with you.

O diferencial é o custo. A Anthropic pode pagar por vários agentes revisores em paralelo mais agentes
verificadores. Eu construí para um orçamento pequeno: camadas determinísticas primeiro, uma chamada focada
ao modelo onde importa, e métricas de rodadas, taxa de alucinação e tokens. Sem nenhuma chave de API, ele
ainda funciona. Quando um bug escapa, um comando o transforma em uma regra e em um teste de regressão. Essa
é a parte que eu adoraria discutir com vocês.
