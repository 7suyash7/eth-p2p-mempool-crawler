# Ethereum P2P Mempool Crawler (Reth-based)

## Introduction

This project is a standalone Ethereum P2P node built entirely in Rust. It is designed specifically to connect directly to the Ethereum Mainnet peer-to-peer network and observe mempool activity in real-time. It leverages core networking components from [Reth (Rust Ethereum)](https://github.com/paradigmxyz/reth) to handle low-level P2P communication, discovery, and protocol handshakes.

Instead of relying on centralized RPC providers, this crawler talks directly to other peers and nodes on the network, listens for transaction announcements (both full transactions and hashes), requests full transaction data when needed, performs basic analysis (extracting sender, value, gas info, etc.), and displays the findings in a live TUI.

## Demo-TUI

https://github.com/user-attachments/assets/65ce162f-80d8-4b8a-addf-9d9a74eba181

A small note here, it says 0 peers because while testing I connected to a kind peer who keeps sending me transactions and I haven't yet implemented the logic to show data about peers who already connected to us. Also, it takes a bit for peers to connect to you, so be patient and let it run for a while, maybe half an hour!

## Things I was trying to learn/do while building this:

* **Deep Dive into Ethereum P2P and just P2P in general:** To understand and interact with Ethereum's network layer (Discv4, RLPx, ETH wire protocol) directly, going beyond typical RPC interactions.
* **Learn more about Rust & Asynchronous Programming:** To figure out Rust's async programming patterns.
* **Explore Reth Modularity:** It's pretty insane if you think about it, every Reth component is built to function as a standalone crate! So, I wanted to explore it a bit more in-depth.
* **Observe the Mempool:** To gain first-hand insight into mempool (transaction propagation, gas markets) without third-party dependencies (well, except Reth).
* **Build a Cool TUI:** To create an informative and visually engaging terminal dashboard for monitoring the network and mempool activity.

## Features

* **Direct P2P Connection:** Connects to Ethereum Mainnet peers using RLPx over TCP.
* **Peer Discovery:** Implements Discv4 UDP protocol for discovering peers, bootstrapped from known bootnodes.
* **ETH Handshake:** Handles the `Status` message exchange and basic compatibility checks.
* **Mempool Listening:**
    * Processes `NewPooledTransactionHashes` (hash broadcasts).
    * Sends `GetPooledTransactions` requests for specific hashes.
    * Processes `PooledTransactions` responses (full transactions requested).
    * Processes `Transactions` messages (full transactions broadcast directly).
* **Block Fetching:** A dedicated poller task actively requests headers and bodies for new blocks from peers, allowing the crawler to follow the chain head.
* **Transaction Processing:** Decodes received transaction data (`PooledTransaction`, `TransactionSigned`) into the standard `reth_primitives::TransactionSigned` format.
* **Basic Analysis:** Extracts key information like sender (via recovery), receiver, value, gas limit, gas price/fees, and transaction type.
* **Database Persistence:** All analyzed transactions (both public and private) are stored in a PostgreSQL database, enabling historical analysis and queries.
* **Real-time API & Gas Oracle**:
    * An axum-based web server provides a WebSocket (/ws) to stream new transactions and REST endpoints to query the database.

    * A simple gas oracle calculates fee estimates based on recent mempool activity.

* **Live TUI Dashboard:** The TUI displays:
    * Real-time statistics (Total Txs Seen, breakdown by type).
    * Connected peer list with client information.
    * A scrollable table showing details of the most recently observed transactions.
* **Configuration:** Uses a `config.toml` file for settings like listener addresses, peer limits, node key path, and logging verbosity (with CLI overrides).
* **File Logging:** Outputs detailed operational logs to `./logs/crawler.log` using `tracing`.

## 📸 Demo

*(Insert your awesome TUI screenshot or GIF here!)*

E.g.:
![Crawler TUI Screenshot](path/to/your/screenshot.png)

Or:
[![Crawler TUI Demo GIF](path/to/your/preview_static.png)](path/to/your/demo.gif)

## 🛠️ Technology Stack

* **Core Language:** [Rust](https://www.rust-lang.org/)
* **Asynchronous Runtime:** [Tokio](https://tokio.rs/)
* **Database**: `PostgreSQL` with `sqlx` for asynchronous queries and migrations.
* **API**: `axum` for the web server and WebSocket handling.
* **Ethereum P2P & Primitives:** [Reth](https://github.com/paradigmxyz/reth) Libraries:
    * `reth-network`: Core P2P session management, RLPx transport, `NetworkManager`.
    * `reth-discv4`: Kademlia-based Discovery v4 implementation.
    * `reth-eth-wire`: Encoding/Decoding of ETH sub-protocol messages (`Status`, `Transactions`, `GetPooledTransactions`, etc.).
    * `reth-primitives`: Core Ethereum data types (`TransactionSigned`, `Address`, `B256`, `Header`, `TxType`, etc.).
    * `reth-provider` (`NoopProvider`): Used to satisfy trait bounds for network components without needing a full database provider.
    * `reth-tasks`: Task management executor abstraction used by `NetworkManager`.
* **Consensus/Types:** [Alloy](https://github.com/alloy-rs) Crates (pulled in via Reth):
    * `alloy-consensus`: Ethereum transaction types (`TxEnvelope`, `TxLegacy`, `TxEip1559`, etc.) and signing logic.
* **Terminal UI (TUI):**
    * `ratatui`: Core TUI drawing and widget library.
    * `crossterm`: Backend for terminal manipulation (raw mode, events, screen switching).
* **Configuration:**
    * `config`: Loading configuration from files and environment variables.
    * `serde`: Serialization/Deserialization for config.
    * `toml`: TOML file parsing (often used by `config`).
* **CLI Arguments:** `clap`
* **Logging:**
    * `tracing`: Application-level logging framework.
    * `tracing-subscriber`: Configuring log collection (`EnvFilter`, formatting).
    * `tracing-appender`: File-based logging (rolling files).
* **Async Utilities:** `futures-util`
* **Error Handling:** `anyhow`
* **Concurrency:** `dashmap` (for thread-safe peer map in handler)
* **Number Formatting:** `num-format` (for TUI display)

## ⚙️ How It Works

This crawler operates as a standalone asynchronous application, leveraging several key components and concepts:

1.  **Modular Architecture:** The application is divided into several modules:
    * `config`: Handles loading settings from `config.toml`, env vars, and CLI args. Also includes helpers for keys and bootnodes.
    * `types`: Defines shared data structures passed between tasks via channels (`UiUpdate`, `PeerInfo`, etc.).
    * `network`: Contains the `EthP2PHandler` responsible for reacting to P2P network events and processing transaction-related messages.
    * `analysis`: Contains logic (`analyze_transaction`) to extract key details from `TransactionSigned` objects.
    * `ui`: Responsible for setting up the terminal, running the main TUI event loop (`run_ui`), managing UI state (`AppState`), and drawing the interface (`draw_frame`) using `ratatui`.
    * `main`: Orchestrates the setup, initializes components, creates channels, spawns all asynchronous tasks, and handles shutdown.

2.  **Task-Based Concurrency (Tokio):** The application runs several asynchronous tasks concurrently:
    * **Core Network Task:** Runs the `reth_network::NetworkManager` future, handling low-level connection management and protocol multiplexing.
    * **Peer Event Handler Task:** Listens for general network events (peer added/removed, session established/closed) emitted by `NetworkManager` and managed by our `EthP2PHandler`. Sends updates to the UI task.
    * **Transaction Event Handler Task:** Listens specifically for `NetworkTransactionEvent`s emitted by `NetworkManager` (forwarded via an MPSC channel). This task, managed by our `EthP2PHandler`, processes incoming transaction messages and sends successfully decoded/converted data to the Processor Task.
    * **Block Poller & Processor Tasks:** A dedicated task polls peers for new blocks. Upon receiving a block, another task processes its transactions, comparing them against the public mempool cache to identify and flag private transactions before sending them to the database writer.
    * **Database Writer Task**: A central task that receives transaction analysis results from both the mempool and block processors and writes them asynchronously to the PostgreSQL database.
    * **API Server & Gas Oracle Tasks**: Runs the axum web server and the background tasks for collecting gas fee data and calculating estimates.
    * **Decoded Tx Processor Task:** Receives `Arc<TransactionSigned>` objects from the Tx Event Handler, calls the `analysis::analyze_transaction` function, and sends the `TxAnalysisResult` to the UI task.
    * **UI Task:** Runs the main TUI loop (`ui::run_ui`), handling terminal drawing, user input ('q' to quit, Arrow keys for scrolling), and processing updates received from other tasks via the `ui_update` channel.

3.  **P2P Connection & Handshake:**
    * Uses `config.rs` helpers to load/generate a node key (`secp256k1::SecretKey`).
    * Loads bootnode information (`NodeRecord`) from config or defaults.
    * Initializes `Discv4ConfigBuilder` and `NetworkConfigBuilder` with listener addresses, peer limits, bootnodes, etc.
    * The `NetworkManager` uses Discv4 to find potential peers in the network.
    * It attempts outgoing TCP connections and accepts incoming ones.
    * Once TCP connects, the RLPx cryptographic handshake occurs.
    * If successful, the ETH sub-protocol handshake begins, exchanging `Status` messages containing protocol version, network ID, total difficulty, best block hash, genesis hash, and the EIP-2124 `ForkId`. (Note: ForkID validation is currently basic in this implementation).

4.  **Mempool Data Acquisition:** The `EthP2PHandler` (running in the Tx Event Handler task) listens for transaction data via two main paths:
    * **Hash -> Request -> Response:**
        * Receives `NetworkTransactionEvent::IncomingPooledTransactionHashes`.
        * Sends a `PeerRequest::GetPooledTransactions` request back to the peer via the `NetworkHandle`.
        * Awaits the response on a `tokio::sync::oneshot` channel.
        * If successful, receives a `PooledTransactions` message containing `Vec<Arc<PooledTransaction>>`.
        * Converts each `PooledTransaction` (the P2P enum format) to `TransactionSigned`.
        * Sends `Arc<TransactionSigned>` to the Processor task.
    * **Direct Broadcast:**
        * Receives `NetworkTransactionEvent::IncomingTransactions`.
        * Sends the `Arc<TransactionSigned>` directly to the Processor task.

5.  **Reth Integration:** This project *does not* run a full Reth node. Instead, it acts as a client library user:
    * It leverages `reth-network` for the complex machinery of managing multiple peer connections, RLPx encryption/framing, and handling the base P2P protocols.
    * It uses `reth-discv4` for peer discovery.
    * It uses `reth-eth-wire` for defining and understanding ETH sub-protocol message structures.
    * It uses `reth-primitives` extensively for core data types (`TransactionSigned`, `Address`, `B256`, etc.) and associated traits (`SignedTransaction`, `Transaction`).
    * It uses `alloy-consensus` types (via `reth-primitives`) for transaction structure definitions (`TxEnvelope`, `TxLegacy`, etc.).
    * Custom logic for handling events, analyzing data, and building the UI is added *on top* of these Reth libraries.

## 🚀 Setup & Installation

1.  **Prerequisites:** Ensure you have a recent Rust toolchain installed (https://rustup.rs/).
2.  **Clone the Repository and just run:
```bash
cargo build --release
```

## ⚙️ Configuration

The crawler is configured primarily via a `config.toml` file.

**`config.toml` Options:**

* `node_key_file` (Optional, String): Path to a file containing the node's secret key (hex-encoded). If omitted or the file doesn't exist, a new key will be generated in memory for the session.
* `p2p_listen_addr` (String, Required): The IP address and TCP port for listening for incoming RLPx peer connections (e.g., `"0.0.0.0:30303"`).
* `discv4_listen_addr` (String, Required): The IP address and UDP port for the Discv4 peer discovery protocol (e.g., `"0.0.0.0:30304"`).
* `bootnodes` (Optional, Array of Strings): A list of enode URLs used to bootstrap the peer discovery process. If omitted or empty, hardcoded Mainnet bootnodes will be used.
* `max_peers_outbound` (Integer, Required): The target maximum number of outbound peer connections to maintain.
* `max_peers_inbound` (Integer, Required): The maximum number of inbound peer connections to allow.
* `debug_logging` (Boolean, Required): Set to `true` for more verbose debug/trace logging in the log file, `false` for standard info/warn level.

**Overrides:**

Certain configuration values can be overridden using:

1.  **CLI Arguments:** Use flags like `--p2p-listen-addr`, `--discv4-listen-addr`, `--node-key-file`, `--bootnodes`, `--debug` when running the executable. See `--help` for details.
2.  **Environment Variables:** Set environment variables prefixed with `CRAWLER_` (e.g., `CRAWLER_P2P_LISTEN_ADDR="0.0.0.0:9999"`).

The order of precedence is: CLI Arguments > Environment Variables > Config File > Internal Defaults.

## ▶️ Running the Crawler

1.  Ensure you have built the project (`cargo build --release`).
2.  Make sure you have a `config.toml` file in the working directory (or specify one with `--config-path`).
3.  Run the executable:
    Using cargo:
    ```bash
    cargo run --release
    ```
4.  The TUI should launch, displaying the dashboard.
5.  Detailed logs will be written to files in the `./logs/` directory (e.g., `crawler.log.YYYY-MM-DD`).
6.  **To Quit:** Press `q` while the TUI is focused, or send a Ctrl+C signal to the process in the terminal where you launched it.

## Limitations

* **Not a Full Node:** This crawler does *not* sync the blockchain state or headers. It only observes the P2P network layer and mempool.
* **ForkID Validation:** Currently basic. Uses a default `Head` and doesn't validate against the live chain tip, potentially allowing connections to peers on incompatible forks (though they would likely disconnect quickly).
* **Peer Stability:** Connections can be transient, especially since this node doesn't participate in full block/state sync. The displayed peer list reflects the current active sessions.

## 🚀 Future Work

* [ ] **TUI Enhancements:**
    * Implement peer list scrolling/selection in the Peer Panel.
    * Display more peer details (address, latency, protocols).
    * Implement transaction selection in the table to show full details/analysis in a separate panel.
    * Add more detailed statistics (e.g., average gas prices over time, tx throughput).
* [ ] **Deeper Analysis:**
    * Identify contract creation transactions explicitly.
    * Basic MEV hints (flag high priority fees, known DEX router interactions, simple sandwich/arbitrage patterns).
    * Decode transaction input data for known contract ABIs.
* [ ] **Persistence:**
    * Save `TxAnalysisResult` data to a database.
    * Add more advanced database queries and analytics endpoints to the API.
* [ ] **Networking Improvements:**
    * Implement dynamic ForkID validation using periodically updated chain head info (potentially via light-client mechanisms or trusted RPC).
    * More robust peer management and scoring.
