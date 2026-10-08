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

> A partir da v0.5.0 a configuração deixou de ficar no diretório de configuração do sistema (ex.: `%APPDATA%` no Windows). Se você já tinha um `config.toml` lá, copie-o para `~/.config/suap/` ou rode `config-init` de novo.

Consulte os caminhos com:

```bash
cargo run -p chamados-cli -- paths
```

Crie a configuração inicial com:

```bash
cargo run -p chamados-cli -- config-init \
  --base-url https://suap.ifrn.edu.br/ \
  --username seu_usuario
```

Consulte a configuração sem exibir senha:

```bash
cargo run -p chamados-cli -- config-show
```

### Perfis (ambientes)

Use `--profile <nome>` para manter vários ambientes (por exemplo, produção e um SUAP local) com configuração e sessão separadas. Sem a flag, vale o perfil `default`, e o `config-init` sem perfil grava o `default`. A flag funciona antes ou depois do subcomando:

```bash
chamados config-init --profile local --base-url http://localhost:8000 --username 2080882
chamados --profile local login
chamados list --profile local
```

Um perfil diferente de `default` precisa ser criado com `config-init` antes de ser usado. A configuração fica em `[profiles.<nome>]` no `config.toml` (um `config.toml` antigo, sem perfis, vale como o perfil `default`), e a sessão do `default` continua em `session.cookies`, enquanto a dos demais fica em `session-<nome>.cookies`.

### Login

A senha nunca é persistida nem passada por argumento: o comando `login` a lê da variável de ambiente `SUAP_PASSWORD`. O usuário vem de `--username` ou, se omitido, do `username` da configuração local.

```bash
export SUAP_PASSWORD='sua_senha'   # PowerShell: $env:SUAP_PASSWORD = 'sua_senha'
cargo run -p chamados-cli -- login --username seu_usuario
```

Após o login, apenas os cookies de sessão são salvos em `session.cookies`.

> A partir da v0.8.1 a sessão é gravada no formato `cookie_store::serde` (JSON). Sessões salvas por versões anteriores não são compatíveis: são descartadas ao abrir e basta rodar `chamados login` de novo.

### Listar chamados

Com a sessão salva por `login`, liste os chamados (uma linha por chamado: `#id`, situação e assunto, separados por tabulação):

```bash
cargo run -p chamados-cli -- list           # fila de suporte (menu "Chamados")
cargo run -p chamados-cli -- list --meus    # "Meus chamados" ativos
```

Se a sessão estiver ausente ou expirada, o comando orienta a executar `chamados login`.

### Abrir um chamado

Guarde os padrões no perfil uma vez e abra chamados só com a descrição:

```bash
chamados config-init --service 53 --interested 1            # padrões do perfil (campus/centro também: --campus, --center)
chamados open -d "Não consigo acessar as bibliotecas virtuais"
```

- **Serviço e interessado:** `53` é o número do serviço no SUAP (o mesmo de `/centralservicos/abrir_chamado/53/`) e `--interested` é o id do vínculo da pessoa interessada, que o formulário do SUAP exige. Podem vir do perfil (`config-init --service/--interested`) ou ser passados no comando (`chamados open 53 --interested 1 ...`), e o que vier no comando prevalece.
- **Campus e centro de atendimento:** por padrão o do perfil; sem isso, o campus do usuário e o único centro disponível (se houver vários, o comando lista as opções e pede `--center`).
- **Texto de várias linhas e entrada padrão:** `-d` aceita texto com quebras de linha. Com `-d -`, ou sem `-d`, a descrição é lida da entrada padrão: `cat descricao.txt | chamados open` (no PowerShell: `Get-Content descricao.txt | chamados open`).
- **Anexos:** `-a arquivo.pdf` (repetível, no máximo 3). O SUAP só aceita `xlsx`, `xls`, `csv`, `docx`, `doc`, `pdf`, `jpg`, `jpeg` e `png`; o comando recusa outros tipos antes de enviar. O serviço precisa permitir anexos.
- **Cópia por e-mail:** a opção "Enviar cópia de abertura deste chamado para os interessados?" vai marcada por padrão; use `--no-email-copy` para desmarcar.
- **Assumir e atender:** `--assume` atribui o chamado a você logo após abrir; `--start` também o coloca em atendimento (implica `--assume`). Se um desses passos falhar, o chamado já foi aberto e o erro informa o número.
- **Outros campos:** `--field NOME=VALOR` (ex.: `--field patrimonio=123`) e `--campus`/`--center` sobrescrevem os padrões.

### Ver detalhes de um chamado

```bash
cargo run -p chamados-cli -- show 559298
```

Exibe título, situação, serviço, URL, dados do interessado, descrição e a linha do tempo completa do chamado. Também exige a sessão salva por `login`.

### SUAP local de desenvolvimento

Para testar `open`, `list` e `show` sem tocar na produção, suba um SUAP local e rode o seed da Central de Serviços (idempotente; cria campus, servidor de teste, catálogo e grupo de atendimento). **Nunca** rode contra homologação ou produção:

```bash
docker exec -i <container-web-do-suap> python manage.py shell < scripts/seed_central_servicos.py
```

O script imprime `servico_id`, `campus_id` e `centro_id`.

## Requisitos de software

- **RS-01 — Cobertura de testes de 100%.** Os testes automatizados do workspace devem cobrir 100% das linhas de código. A verificação roda no CI (`ci.yml`) e o build falha se a cobertura ficar abaixo disso. O ponto de entrada `main.rs` de cada binário deve conter apenas o encadeamento mínimo e é excluído da medição; toda a lógica fica em `lib.rs`, onde é testada.

Para verificar localmente (requer `cargo install cargo-llvm-cov` e o componente `llvm-tools-preview`):

```bash
cargo llvm-cov --workspace --all-targets --ignore-filename-regex 'main\.rs' --fail-under-lines 100
```

## Desenvolvimento

```bash
cargo check --workspace
cargo test --workspace
cargo run -p chamados-cli -- --help
cargo run -p chamados-cli -- status
```
