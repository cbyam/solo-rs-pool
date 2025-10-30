# solo-rs-pool (starter)

Single-binary Rust solo pool starter:
- bitcoind RPC + (stubbed) ZMQ watcher
- Stratum v1 (subscribe/authorize/submit) minimal
- Axum + Askama + HTMX web UI + SSE live updates
- Sqlx/SQLite storage

> Not production-ready. Use as a learning scaffold and build real job/coinbase/merkle/PoW validation.

## Run