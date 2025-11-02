# solo-rs-pool

A lightweight **Stratum-V1 solo pool** for **Bitcoin Core**, written in **Rust**.  
Built for home-lab miners, developers, and small setups who want direct block submission to their own node — **no middlemen, no fees.**

*** Experimental — use with care on mainnet. ***

---

### Features
- Stratum-V1 (`subscribe`, `authorize`, `notify`, `submit`)
- Auto-refresh via **ZMQ block events**
- Web UI with live job updates (SSE)
- Regtest / Testnet / Mainnet support
- Full-transaction submit via `getblocktemplate`
- SQLite persistence for shares and workers
- Optional miner password / LAN ACLs

---

### Quick Start (Regtest)

```bash
git clone https://github.com/cbyam/solo-rs-pool.git
cd solo-rs-pool
cp .env.example .env
cargo run
```
