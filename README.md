# chamados-cli

Cliente local para acompanhar chamados do SUAP, inicialmente como uma CLI em Rust e futuramente com aplicativo Android.

## Arquitetura

O projeto é um workspace Cargo com três crates:

- `suap-core`: autenticação, sessão, cookies, configuração e transporte compartilhados.
- `chamados-core`: domínio de chamados, sincronização e recursos locais como títulos e adiamentos.
- `chamados-cli`: interface de linha de comando.

A separação permite reutilizar o núcleo Rust no Android sem colocar regras de chamados dentro da camada de autenticação do SUAP.

## Estado atual

Esta primeira versão cria somente a fundação compilável. Ainda não implementa login, persistência de cookies, requisições ao SUAP ou parsing HTML.

## Desenvolvimento

```bash
cargo check --workspace
cargo test --workspace
cargo run -p chamados-cli -- --help
cargo run -p chamados-cli -- status
```

## Próximos passos

1. Implementar configuração local.
2. Implementar cookie jar persistente em `suap-core`.
3. Implementar `session-status`.
4. Implementar login e reutilização de sessão.
5. Adicionar parser HTML do SUAP com fixtures testáveis.
