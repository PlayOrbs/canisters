# Orbs ICP Canisters

Verifiable randomness and game state storage for Orbs on the Internet Computer.

## Deployed Canisters

| Environment | Canister ID | Network |
|-------------|-------------|---------|
| **Production** | `uy5s7-myaaa-aaaam-qfnua-cai` | IC Mainnet |
| **Development** | `2lvus-jqaaa-aaaam-qerkq-cai` | IC Mainnet |

## Features

- **Verifiable Random Seeds**: Cryptographically secure seed generation using ICP's threshold ECDSA
- **Merkle Proofs**: On-chain verification of seed authenticity
- **Round Snapshots**: Immutable storage of game results
- **Player Configs**: Spawn positions and skill allocations (revealed after settlement)
- **Engine Configs**: Versioned game configuration storage

## Development

```bash
# Start local replica
dfx start --background

# Deploy canisters
dfx deploy
```

## License

BSL-1.1 - See LICENSE file
