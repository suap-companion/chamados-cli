# chamados-cli

Cliente local para acompanhar chamados do SUAP, inicialmente como uma CLI em Rust e futuramente com aplicativo Android.

## Arquitetura

O projeto é um workspace Cargo com três crates:

- `suap-core`: configuração, diretórios, autenticação, sessão, cookies e transporte compartilhados.
- `chamados-core`: domínio de chamados, sincronização e recursos locais como títulos e adiamentos.
- `chamados-cli`: interface de linha de comando.

A separação permite reutilizar o núcleo Rust no Android sem colocar regras de chamados dentro da camada de autenticação do SUAP.

## Configuração local

O arquivo de configuração é `~/.config/suap/config.toml` em todos os sistemas (no Windows, `%USERPROFILE%\.config\suap\config.toml`). A sessão (`session.cookies`) fica no diretório de dados do sistema, obtido via `ProjectDirs`.

> A partir da v0.5.0 a configuração deixou de ficar no diretório de configuração do sistema (ex.: `%APPDATA%` no Windows). Se você já tinha um `config.toml` lá, copie-o para `~/.config/suap/` ou rode `chamados profile init` de novo.

Consulte os caminhos com:

```bash
cargo run -p chamados-cli -- paths
```

Crie o perfil inicial (`default`) com:

```bash
cargo run -p chamados-cli -- profile init \
  --base-url https://suap.ifrn.edu.br/ \
  --username seu_usuario
```

Consulte a configuração sem exibir senha:

```bash
cargo run -p chamados-cli -- profile show
```

> A partir da v0.9.0 os comandos `config-init` e `config-show` foram substituídos pelos comandos `profile` (veja abaixo).

### Perfis (ambientes)

Use `--profile <nome>` para manter vários ambientes (por exemplo, produção e um SUAP local) com configuração e sessão separadas. Sem a flag, vale o perfil `default`, e `profile init` sem nome grava o `default`. A flag funciona antes ou depois do subcomando:

```bash
chamados profile init local --base-url http://localhost:8000 --username 2080882
chamados --profile local login
chamados list --profile local
```

Gerencie os perfis com `chamados profile`:

| Comando | O que faz |
|---------|-----------|
| `profile init [nome] [--base-url ...] [--username ...] [--service ...] [--interested ...] [--campus ...] [--center ...]` | cria o perfil (erro se já existir); sem nome, usa o perfil selecionado por `--profile` (`default`) |
| `profile update [nome] <opções>` | altera campos de um perfil existente (exige ao menos uma opção) |
| `profile show [nome]` | exibe a configuração e se há sessão salva |
| `profile list` | lista os perfis (o `default` aparece marcado) |
| `profile remove <nome> --yes` | apaga a configuração e a sessão do perfil |

Um perfil diferente de `default` precisa ser criado com `profile init` antes de ser usado. A configuração fica em `[profiles.<nome>]` no `config.toml` (um `config.toml` antigo, sem perfis, vale como o perfil `default`), e a sessão do `default` continua em `session.cookies`, enquanto a dos demais fica em `session-<nome>.cookies`.

### Verificar a sessão

```bash
chamados session-status   # ou: chamados session-status --profile local
```

Informa se a sessão salva do perfil ainda é válida, sem pedir senha. Se não houver sessão ou ela tiver expirado, o comando termina com erro orientando a executar `chamados login` (a sessão salva nunca é apagada automaticamente).

### Login

A senha nunca é persistida nem passada por argumento: o comando `login` a lê da variável de ambiente `SUAP_PASSWORD`. O usuário vem de `--username` ou, se omitido, do `username` da configuração local.

```bash
export SUAP_PASSWORD='sua_senha'   # PowerShell: $env:SUAP_PASSWORD = 'sua_senha'
cargo run -p chamados-cli -- login --username seu_usuario
```

Após o login, apenas os cookies de sessão são salvos em `session.cookies`.

> A partir da v0.8.1 a sessão é gravada no formato `cookie_store::serde` (JSON). Sessões salvas por versões anteriores não são compatíveis: são descartadas ao abrir e basta rodar `chamados login` de novo.

### Listar chamados

Com a sessão salva por `login`, liste os chamados (uma linha por chamado: `#id`, situação, título local e assunto, separados por tabulação; `-` quando não há título):

```bash
cargo run -p chamados-cli -- list           # fila de suporte (menu "Chamados")
cargo run -p chamados-cli -- list --meus    # "Meus chamados" ativos
```

Se a sessão estiver ausente ou expirada, o comando orienta a executar `chamados login`.

#### Filtros, busca e páginas

Quem filtra é o próprio SUAP (os mesmos filtros do formulário da tela); só `--titulo` e `--limite` são aplicados aqui.

```bash
chamados list --busca moodle --status aberto --status atendimento
chamados list --desde 2026-09-01 --ate 2026-09-30 --todos          # inclui resolvidos, fechados e cancelados
chamados list --atribuidos mim --ordenar limite --sla-estourado
chamados list --id 559298
chamados list --titulo "plugin" --limite 10                          # filtra pelo título local
chamados list --todas-paginas                                        # o SUAP mostra 15 por página
chamados list --pagina 2
chamados list --meus --todos --relacao interessado --desde 2026-01-01
```

| Opção | Vale para | O que faz |
|-------|-----------|-----------|
| `--id N` | os dois | só este chamado |
| `--desde`/`--ate AAAA-MM-DD` | os dois | período de abertura |
| `--busca TEXTO` | suporte | procura em descrições, comentários e notas internas |
| `--status` (repetível), `--todos` | suporte; `--todos` também em `--meus` | situação: `aberto`, `atendimento`, `resolvido`, `fechado`, `reaberto`, `suspenso`, `cancelado`; sem elas o SUAP esconde os encerrados |
| `--atribuidos` | suporte | `mim`, `outros`, `ninguem`, `mim-ou-ninguem`, `alguem` |
| `--ordenar` (+ `--desc`) | suporte | `limite`, `interacao`, `abertura`, `situacao`, `servico`, `aberto-por`, `interessado` |
| `--sla-estourado` | suporte | só com SLA estourado |
| `--relacao` | `--meus` | `requisitante`, `interessado`, `outro`, `algum` |
| `--pagina N` / `--todas-paginas` | os dois | uma página, ou todas (para quando uma página não traz nada novo; no máximo 200) |
| `--titulo TEXTO` | os dois | só os que têm este texto no título local (sem diferenciar maiúsculas) |
| `--limite N` | os dois | no máximo N linhas |

Uma opção que não vale para a lista escolhida é recusada antes de qualquer requisição.

### Abrir um chamado

Guarde os padrões no perfil uma vez e abra chamados só com a descrição:

```bash
chamados profile update --service 53 --interested 1         # padrões do perfil (campus/centro também: --campus, --center)
chamados open -d "Não consigo acessar as bibliotecas virtuais"
```

- **Serviço e interessado:** `53` é o número do serviço no SUAP (o mesmo de `/centralservicos/abrir_chamado/53/`) e `--interested` é o id do vínculo da pessoa interessada, que o formulário do SUAP exige. Podem vir do perfil (`profile update --service/--interested`) ou ser passados no comando (`chamados open 53 --interested 1 ...`), e o que vier no comando prevalece.
- **Campus e centro de atendimento:** por padrão o do perfil; sem isso, o campus do usuário e o único centro disponível (se houver vários, o comando lista as opções e pede `--center`).
- **Texto de várias linhas e entrada padrão:** `-d` aceita texto com quebras de linha. Com `-d -`, ou sem `-d`, a descrição é lida da entrada padrão: `cat descricao.txt | chamados open` (no PowerShell: `Get-Content descricao.txt | chamados open`).
- **Anexos:** `-a arquivo.pdf` (repetível, no máximo 3). O SUAP só aceita `xlsx`, `xls`, `csv`, `docx`, `doc`, `pdf`, `jpg`, `jpeg` e `png`; o comando recusa outros tipos antes de enviar. O serviço precisa permitir anexos.
- **Cópia por e-mail:** a opção "Enviar cópia de abertura deste chamado para os interessados?" vai marcada por padrão; use `--no-email-copy` para desmarcar.
- **Assumir e atender:** `--assume` atribui o chamado a você logo após abrir; `--start` também o coloca em atendimento (implica `--assume`). Se um desses passos falhar, o chamado já foi aberto e o erro informa o número.
- **Outros campos:** `--field NOME=VALOR` (ex.: `--field patrimonio=123`) e `--campus`/`--center` sobrescrevem os padrões.

### Títulos locais

O SUAP não tem título para chamados, o que dificulta a manutenção. Dê um título ao chamado ao abrir ou depois; ele fica **só nesta máquina** e aparece no `list` e no `show`:

```bash
chamados open -d "Atualizar o Moodle para a 5.3.0" --title "Moodle 5.3.0"   # ao abrir (-t)
chamados title 559298 "Moodle 5.3.0"     # define ou altera
chamados title 559298                    # exibe o título atual
chamados title 559298 --remove           # remove
```

O título tem uma única linha, de até 120 caracteres. Fica em `titles.json` no diretório de dados (`titles-<perfil>.json` nos demais perfis), com a hora de cada alteração e as remoções registradas, para permitir sincronização futura. Esse arquivo é dado seu: se estiver corrompido, o comando informa o erro em vez de descartá-lo.

### Comentar e anotar um chamado

```bash
chamados comment 559298 -m "Já fiz o build da imagem base.
Agora estou testando os plugins."          # comentário (visível ao interessado)
chamados note 559298 -m "Senha do servidor está no cofre"   # nota interna (só a equipe de atendimento)
cat comentario.txt | chamados comment 559298                # texto pela entrada padrão
```

O texto pode ter várias linhas e vem de `-m`, de `-m -` ou, se `-m` for omitido, da entrada padrão (nunca se espera digitação no terminal). O comando abre a página do chamado, usa o formulário que o SUAP oferece (com o token CSRF) e envia o texto; se o SUAP recusar (sem permissão, chamado fechado, texto inválido), mostra a mensagem dele. Confira o resultado com `chamados show <id>`.

### Suspender e resolver um chamado

```bash
chamados suspend 559298 -m "Aguardando retorno do fornecedor."
chamados resolve 559298 -m "Atualizado para a versão 5.3.0.
Plugins validados."
cat resolucao.txt | chamados resolve 559298 --also 559300 --also 559301
```

A mensagem segue o mesmo padrão dos demais textos (várias linhas; `-m`, `-m -` ou entrada padrão) e vira um comentário na linha do tempo. O comando carrega o formulário que o SUAP oferece (`suspender_chamado`/`resolver_chamado`) e o envia de volta; se o SUAP recusar (situação inválida, sem permissão), mostra a mensagem dele.

No `resolve`:

- `--article <id>` (repetível) escolhe os artigos relacionados da base de conhecimento. O SUAP exige ao menos um **quando oferece algum**; sem a opção, o comando usa o **primeiro** da lista oferecida. Um id que o SUAP não ofereceu é recusado, com a lista de opções.
- `--also <id>` (repetível) resolve outros chamados junto com este.
- `--standard-reply <id>` usa uma resposta padrão.

### Assumir, colocar em atendimento, cancelar, reabrir e fechar

```bash
chamados assume 559298                  # atribui o chamado a você
chamados start 559298                   # situação "Em atendimento"
chamados start 559298 --assume          # assume e coloca em atendimento, de uma vez
chamados cancel 559298 --yes -m "Aberto por engano."
chamados reopen 559298 -m "O erro voltou."      # chamado Resolvido -> Reaberto
chamados close 559298 --nota 5 -m "Atendimento excelente."   # Resolvido -> Fechado, avaliando
chamados list --a-fechar                        # os resolvidos que você pode fechar
```

São as mesmas ações do `open --assume` / `open --start`, agora para chamados que já existem. O SUAP só deixa colocar em atendimento quem já assumiu o chamado; sem `--assume`, a recusa dele é mostrada. O `reopen` e o `close` só valem para chamados **Resolvidos**, a pedido do requisitante, do interessado ou do responsável pelo grupo de atendimento; o motivo do `reopen` é obrigatório (mesmo padrão dos demais textos). No `close`, `--nota` (1 a 5) e o comentário são a **avaliação do atendimento** e só valem quando quem fecha é o **interessado** (para os demais o SUAP ignora os dois); ambos são opcionais, e o comentário só lê a entrada padrão com `-m -` (omitido, fecha sem comentário). O `list --a-fechar` aceita só `--pagina`/`--todas-paginas`. O `cancel` **não pode ser desfeito**, por isso exige `--yes`; o motivo segue o padrão dos demais textos (várias linhas; `-m`, `-m -` ou entrada padrão) e o SUAP só cancela chamados abertos, reabertos, em atendimento ou suspensos, a pedido do requisitante, do interessado ou de um atendente.

### Baixar anexos

O `show` lista os anexos do chamado, numerados na ordem da linha do tempo. O `download` os grava no disco:

```bash
chamados download 559298                       # todos, na pasta atual
chamados download 559298 --anexo 2 -o baixados # só o anexo 2, na pasta "baixados" (criada se preciso)
chamados download 559298 --force               # sobrescreve arquivos que já existam
```

O nome do arquivo vem do SUAP, mas é saneado antes de gravar: pastas e `..` são descartados, caracteres que o sistema de arquivos recusa viram `_`, nomes reservados do Windows (`CON`, `NUL`...) ganham um `_` na frente e nomes repetidos viram `nome (2).ext`. Nada é sobrescrito sem `--force`, e a checagem acontece antes de baixar qualquer arquivo. Só são baixados os arquivos que o SUAP serve para chamados (`/djtools/arquivo/centralservicos/chamadoanexo/...`), nunca um endereço qualquer encontrado na página.

### Ver detalhes de um chamado

```bash
cargo run -p chamados-cli -- show 559298
```

Exibe título, situação, serviço, URL, dados do interessado, descrição e a linha do tempo completa do chamado. Também exige a sessão salva por `login`.

### SUAP local de desenvolvimento

Para testar `open`, `list`, `show`, `download` e os demais comandos sem tocar na produção, suba o SUAP local pelo [workspace](https://github.com/suap-companion/workspace) (seção "3. Inicie o Ambiente Docker do SUAP" do README dele) e crie os dados de teste da Central de Serviços (idempotente; cria campus, o usuário atendente 2080882, catálogo, grupo de atendimento e um artigo da base de conhecimento):

```bash
suap up -d db redis
suap create_db
suap launch
suap manage seed_central_servicos
```

O comando só roda com `DEBUG` ligado, ou seja, numa base local de desenvolvimento: **nunca** use homologação ou produção. Ele imprime `servico_id`, `campus_id` e `centro_id`.

## Sincronização em nuvem (cifrada)

Os **títulos locais** e as **configurações dos perfis** podem ser sincronizados entre dispositivos. Tudo é **cifrado no seu computador** (XChaCha20-Poly1305, chave de 256 bits) antes de sair: o armazenamento só enxerga bytes ilegíveis, e a chave nunca vai para a nuvem.

**Nunca sincronizam:** sessões e cookies (`session*.cookies`), a senha (`SUAP_PASSWORD`), credenciais da nuvem e a própria chave.

### Configurar

```bash
# 1. Onde guardar (por enquanto, uma pasta: sincronizada por outro programa, disco de rede ou, para testar, qualquer diretório)
chamados sync setup --path /caminho/da/pasta --key-source file    # key-source: keyring (padrão), file, protected-file ou env

# 2. A chave (uma só, para todos os seus dispositivos)
chamados sync key generate

# 3. Quais perfis sincronizam (começam desligados)
chamados profile update --sync true                 # perfil default
chamados profile update local --sync true

# 4. Sincronizar
chamados sync
```

Em outro dispositivo, repita o `setup` e **leve a chave por um canal seu** (nunca pela nuvem): `chamados sync key export` imprime a chave em hexadecimal e `chamados sync key import` a lê da entrada padrão (`chamados sync key export | ssh outro chamados sync key import`). `chamados sync key status` informa onde a chave está e se existe.

### Cloudflare R2 (ou outro S3-compatível)

Para o Cloudflare R2 use `--backend r2`; para qualquer outro S3-compatível, `--backend s3` (o protocolo é o mesmo, só muda o nome mostrado). Em vez de uma pasta, o `sync` pode usar um bucket S3-compatível (Cloudflare R2, AWS S3, MinIO, Backblaze B2...) por HTTPS, com requisições assinadas (SigV4). O provedor é escolha sua; nada no código o fixa. O `chamados` só faz `GET`, `PUT` e `DELETE` de um objeto: não usa ACLs, links pré-assinados nem listagens, então não há como tornar o objeto público.

No Cloudflare R2:

1. Crie um bucket **privado** (deixe desligados o acesso público `r2.dev` e qualquer domínio público).
2. Crie um token de API do R2 com permissão **Object Read & Write** restrita a esse bucket e anote o **Access Key ID** e o **Secret Access Key**.
3. O endpoint é `https://<ACCOUNT_ID>.r2.cloudflarestorage.com` (o `ACCOUNT_ID` aparece no painel do R2).

```bash
chamados sync setup --backend r2 \
  --endpoint https://<ACCOUNT_ID>.r2.cloudflarestorage.com \
  --bucket meu-bucket --prefix chamados --key-source file     # região padrão: auto (R2)

# credenciais: o comando pergunta o Access Key ID e o Secret Access Key (este sem aparecer na tela)
chamados sync credentials set

chamados sync key generate
chamados sync --check      # confere o acesso e se o bucket aceita escrita condicional
chamados sync
```

Em um script, `chamados sync credentials set` também aceita as duas linhas pela entrada padrão (Access Key ID, depois o Secret); segredos nunca são aceitos como argumento. As credenciais ficam no **chaveiro do sistema** ou, para automação, nas variáveis `CHAMADOS_S3_ACCESS_KEY_ID` e `CHAMADOS_S3_SECRET_ACCESS_KEY` (o `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` também valem); **nunca** vão para o `config.toml` nem para a nuvem. `chamados sync credentials status` informa se existem, sem mostrá-las. O endpoint precisa ser `https` (`http` só é aceito em `localhost`, para testar contra um servidor local).

`sync --check` faz duas escritas `If-None-Match: *` de um objeto temporário (que depois apaga) para descobrir se o bucket honra **escrita condicional**. Se não honrar (como alguns servidores), ajuste `chamados sync setup --conditional-writes false`: o `sync` passa a reler o que gravou para confirmar que ninguém gravou ao mesmo tempo.

### Onde fica a chave

| `--key-source` | Onde | Quando usar |
|----------------|------|-------------|
| `keyring` (padrão) | repositório de segredos do sistema (Windows Credential Manager, macOS Keychain, Linux Secret Service) | desktop com sessão aberta |
| `file` | arquivo `~/.config/suap/sync.key` (ou `--key-file`), permissão `0600` verificada no Linux e no macOS | servidores, SSH e **automação** |
| `protected-file` | o mesmo arquivo, mas **cifrado com uma frase-senha** (Argon2id + XChaCha20-Poly1305) | uso interativo em máquina sem chaveiro |
| `env` | variável de ambiente `CHAMADOS_SYNC_KEY` (somente leitura) | contêineres e CI |

O chaveiro de um desktop só abre numa sessão gráfica desbloqueada, então `cron` e timers devem usar `file` ou `env`.

**Frase-senha.** Com `--key-source protected-file`, `sync key generate` e `sync key import` pedem a frase duas vezes; `sync` e `sync key export` pedem uma. A frase também pode vir da variável `CHAMADOS_SYNC_PASSPHRASE`, mas guardá-la ao lado do arquivo anula a proteção, e um `cron` não consegue digitá-la: para automação, use `file` ou `env`. Quem esquecer a frase perde a chave (e, sem outra cópia dela, os dados da nuvem).

### Opções do `sync`

- `chamados sync --check`: mostra o backend, se ele suporta escrita condicional e se a chave existe, sem sincronizar.
- `chamados sync --dry-run`: mostra o que mudaria, sem gravar nada (nem local, nem na nuvem).
- `chamados sync --only <perfil>`: restringe a rodada a um perfil.
- `chamados sync --quiet`: não imprime nada em caso de sucesso (para automação). O código de saída é diferente de zero em falha.

Duas execuções ao mesmo tempo não se atrapalham: a segunda vê o bloqueio (`sync.lock`) e sai sem erro.

### Como as cópias se juntam

Cada título e cada bloco de configurações carrega o instante da última alteração (em milissegundos) e **o mais novo vence**, entrada por entrada; em empate de instante, vence o conteúdo maior (sempre o mesmo resultado, em qualquer ordem). Remover um título deixa uma marca de remoção, para a remoção também chegar aos outros dispositivos. Se alguém gravar na nuvem durante a sua rodada, ela recomeça do download (até 5 tentativas). Backends sem escrita condicional gravam e **releem para confirmar**.

Remover um perfil que sincroniza (`profile remove`) também deixa uma marca de remoção: no próximo `sync`, os outros dispositivos apagam o perfil (configuração, sessão e títulos) e ele não volta da nuvem. Recriar o perfil depois (`profile init`) o traz de volta, sem os títulos anteriores à remoção.

### Sincronizar a cada alteração

`chamados sync setup --auto true` faz o `title` e os comandos `profile init/update/remove` sincronizarem logo após gravar, para reduzir o intervalo entre dispositivos sem esperar o agendamento. Só dispara se houver algum perfil com `sync` ligado, e uma falha (sem rede, por exemplo) apenas mostra um aviso: o comando que alterou o dado continua valendo, e o agendamento ou um `chamados sync` posterior recuperam. Com chave em `protected-file`, cada alteração pede a frase-senha (use `file` se isso incomodar).

### Automatizar (a cada 5 minutos)

`chamados sync automation` mostra os modelos prontos para o seu sistema (`--platform systemd|cron|windows`, `--interval <minutos>`), já com o caminho do `chamados` em uso. No Linux, `chamados sync automation --install` grava os arquivos do timer em `~/.config/systemd/user` e mostra o `systemctl --user enable --now chamados-sync.timer` para ativar. Os modelos usam `chamados sync --quiet`.

Com a chave em `file` ou `env`. À mão, no Linux, um **timer do systemd de usuário** (`~/.config/systemd/user/chamados-sync.service` e `.timer`):

```ini
# chamados-sync.service
[Service]
Type=oneshot
ExecStart=%h/.local/bin/chamados sync --quiet

# chamados-sync.timer
[Timer]
OnBootSec=1min
OnUnitActiveSec=5min
Persistent=true
[Install]
WantedBy=timers.target
```

```bash
systemctl --user enable --now chamados-sync.timer
```

Com `cron`: `*/5 * * * * flock -n ~/.cache/chamados-sync.lock chamados sync --quiet`. No Windows, o Agendador de Tarefas (`schtasks /create /sc minute /mo 5 /tn chamados-sync /tr "chamados sync --quiet"`).

## Requisitos de software

- **RS-01 — Cobertura de testes de 100%.** Os testes automatizados do workspace devem cobrir 100% das linhas de código. A verificação roda no CI (`ci.yml`) e o build falha se a cobertura ficar abaixo disso. O ponto de entrada `main.rs` de cada binário deve conter apenas o encadeamento mínimo e é excluído da medição; toda a lógica fica em `lib.rs`, onde é testada.

- **RS-02 — Entradas de texto aceitam várias linhas e a entrada padrão.** Toda entrada de texto livre do `chamados` (descrição, comentário, nota interna e mensagens de resolvido e de suspenso, e qualquer campo de texto livre futuro) aceita **várias linhas** e pode ser lida da **entrada padrão**: a opção do comando (`-d`/`-m`) recebe o texto, `-` lê o texto do stdin, e omitir a opção também lê o stdin. Só a quebra de linha final é removida, texto vazio é recusado antes de qualquer envio e um terminal interativo nunca é aguardado. A exceção é o título local (`chamados title`), que tem uma única linha por decisão de projeto. O requisito é verificado por teste para cada comando de texto (`rs02_every_text_input_accepts_multiple_lines_and_standard_input`), e outro teste obriga a classificar cada comando novo como "com texto" ou "sem texto".

- **RS-03 — Os dados do usuário são só do usuário.** Os títulos e as configurações dos perfis que saem da máquina (sincronização em nuvem) **não podem ser compartilhados de forma alguma**: (1) tudo que sai vai **cifrado**, com uma chave que nunca sai dos dispositivos do usuário; (2) sessões e cookies, a senha (`SUAP_PASSWORD`), as credenciais da nuvem e a própria chave **nunca** entram no conjunto sincronizado; (3) o `chamados` não usa recurso de compartilhamento do provedor (ACLs, links públicos ou pré-assinados, colaboradores) e exige `https` para o endpoint; (4) sem chave (ou sem credenciais), o `sync` recusa operar e **não envia nenhuma requisição**; (5) mensagens de erro nunca repetem credenciais. O requisito é verificado por testes: o conteúdo enviado ao bucket é varrido atrás de títulos, nome de usuário, cookies, a chave e o segredo da nuvem; as requisições são conferidas (sem ACL, sem query, sempre assinadas); e um teste-guarda obriga a classificar cada configuração nova como "sincroniza" ou "só local" (`rs03_every_setting_is_classified_as_synced_or_local_only`).

Para verificar a cobertura localmente (requer `cargo install cargo-llvm-cov` e o componente `llvm-tools-preview`):

```bash
cargo llvm-cov --workspace --all-targets --ignore-filename-regex 'main\.rs' --fail-under-lines 100
```

## Desenvolvimento

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace
cargo test --workspace
cargo run -p chamados-cli -- --help
cargo run -p chamados-cli -- status
```
