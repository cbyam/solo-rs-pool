# Security Policy
- Keep Web UI bound to 127.0.0.1 or behind auth/HTTPS.
- Restrict Stratum to your miners (ACLs/firewall).
- Do not expose RPC creds publicly; use a dedicated bitcoind user.
- On NETWORK=mainnet, the pool refuses to start without PAYOUT_ADDRESS.
Report vulnerabilities privately via issues marked "security" or email (add contact).
