# chamados-cli

Cliente local para acompanhar chamados do SUAP, inicialmente como uma CLI em Rust e futuramente com aplicativo Android.

## Arquitetura

O projeto é um workspace Cargo com três crates:

- `suap-core`: configuração, diretórios, autenticação, sessão, cookies e transporte compartilhados.
- `chamados-core`: domínio de chamados, sincronização e recursos locais como títulos e adiamentos.
- `chamados-cli`: interface de linha de comando.

A separação permite reutilizar o núcleo Rust no Android sem colocar regras de chamados dentro da camada de autenticação do SUAP.

## Configuração local

A aplicação usa `ProjectDirs` para obter diretórios apropriados para cada sistema operacional. O arquivo de configuração é `config.toml`; o arquivo reservado para a sessão fica no diretório de dados como `session.cookies`.

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

A senha não é persistida; os cookies de sessão são salvos em `session.cookies` após o login.

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
