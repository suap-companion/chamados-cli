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

A senha e os cookies ainda não são persistidos nesta etapa.

## Desenvolvimento

```bash
cargo check --workspace
cargo test --workspace
cargo run -p chamados-cli -- --help
cargo run -p chamados-cli -- status
```
