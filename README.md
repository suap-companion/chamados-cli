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

### Listar chamados

Com a sessão salva por `login`, liste os chamados (uma linha por chamado: `#id`, situação e assunto, separados por tabulação):

```bash
cargo run -p chamados-cli -- list           # fila de suporte (menu "Chamados")
cargo run -p chamados-cli -- list --meus    # "Meus chamados" ativos
```

Se a sessão estiver ausente ou expirada, o comando orienta a executar `chamados login`.

### Abrir um chamado

```bash
cargo run -p chamados-cli -- open 53 --interested 1 --description "Não consigo acessar as bibliotecas virtuais"
```

`53` é o número do serviço no SUAP (o mesmo de `/centralservicos/abrir_chamado/53/`). O campus padrão é o do usuário e o centro de atendimento padrão é o único disponível para o campus; se houver vários, o comando lista as opções e pede `--center`. Outros campos do formulário podem ser enviados com `--field NOME=VALOR` (ex.: `--field patrimonio=123`), e `--campus` e `--center` sobrescrevem os padrões. `--interested` (id do vínculo da pessoa interessada, que o formulário do SUAP exige) é obrigatório.

### Ver detalhes de um chamado

```bash
cargo run -p chamados-cli -- show 559298
```

Exibe título, situação, serviço, URL, dados do interessado, descrição e a linha do tempo completa do chamado. Também exige a sessão salva por `login`.

### SUAP local de desenvolvimento

Para testar `open`, `list` e `show` sem tocar na produção, suba um SUAP local e rode o seed da Central de Serviços (idempotente; cria campus, servidor de teste, catálogo e grupo de atendimento). **Nunca** rode contra homologação ou produção:

```bash
docker exec -i docker-web-1 python manage.py shell < scripts/seed_central_servicos.py
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
