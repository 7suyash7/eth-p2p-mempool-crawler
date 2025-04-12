//! Configuration loading and setup helpers.

use anyhow::Result;
use clap::Parser;
use config::{Config as AppConfig, Environment, File as ConfigFile};
use reth_discv4::NodeRecord;
use reth_network::config::{rng_secret_key, SecretKey as RethSecretKey};
use secp256k1::Secp256k1; // Keep for key generation
use serde::Deserialize;
use std::{net::SocketAddr, path::PathBuf, str::FromStr};
use tracing::info; // Keep for logging within this module
use tracing_appender::rolling;
use tracing_subscriber::{
    fmt::{self, writer::MakeWriterExt},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

// --- CLI Arguments ---
// (Kept private to this module as it's only used within load_config)
#[derive(Parser, Debug)]
#[clap(
    name = "eth-p2p-crawler",
    author = "Suyash Nayan",
    version,
    about = "Ethereum P2P Network Crawler and Mempool Analyzer using Reth"
)]
struct CliArgs {
    #[clap(long, help = "Override node key file path from config")]
    node_key_file: Option<PathBuf>,
    #[clap(long, help = "Override P2P listener address from config")]
    p2p_listen_addr: Option<SocketAddr>,
    #[clap(long, help = "Override Discovery v4 listener address from config")]
    discv4_listen_addr: Option<SocketAddr>,
    #[clap(
        long,
        use_value_delimiter = true,
        value_delimiter = ',',
        help = "Override bootnodes from config"
    )]
    bootnodes: Option<Vec<String>>,
    #[clap(long, help = "Override debug logging setting from config")]
    debug: Option<bool>,
    #[clap(
        long,
        default_value = "config.toml",
        help = "Path to the configuration file"
    )]
    config_path: PathBuf,
}

// --- Configuration Struct ---
// (Make fields pub that need to be accessed from main.rs)
#[derive(Deserialize, Debug, Clone)] // Added Clone for simplicity if needed later
pub struct Config {
    pub node_key_file: Option<PathBuf>,
    pub p2p_listen_addr: SocketAddr,
    pub discv4_listen_addr: SocketAddr,
    pub bootnodes: Option<Vec<String>>,
    pub max_peers_outbound: usize,
    pub max_peers_inbound: usize,
    pub debug_logging: bool,
}

/// Loads configuration from file, environment variables, and CLI arguments.
pub fn load_config() -> Result<Config> {
    let cli_args = CliArgs::parse();

    let config_builder = AppConfig::builder()
        // Load from specified config file (or default "config.toml")
        // Make it optional so the app can run without a config file
        .add_source(ConfigFile::from(cli_args.config_path.clone()).required(false))
        // Load environment variables (e.g., CRAWLER_P2P_LISTEN_ADDR)
        .add_source(Environment::with_prefix("CRAWLER").separator("_"));

    // Apply CLI overrides - this takes precedence
    let config_builder = if let Some(addr) = cli_args.p2p_listen_addr {
        config_builder.set_override("p2p_listen_addr", addr.to_string())?
    } else {
        config_builder
    };
    let config_builder = if let Some(addr) = cli_args.discv4_listen_addr {
        config_builder.set_override("discv4_listen_addr", addr.to_string())?
    } else {
        config_builder
    };
    let config_builder = if let Some(debug) = cli_args.debug {
        config_builder.set_override("debug_logging", debug)?
    } else {
        config_builder
    };

    // Build the final configuration source
    let settings = config_builder
        // Add default values directly here for required fields if file doesn't exist
        .set_default("p2p_listen_addr", "0.0.0.0:30303")?
        .set_default("discv4_listen_addr", "0.0.0.0:30304")?
        .set_default("max_peers_outbound", 15)?
        .set_default("max_peers_inbound", 10)?
        .set_default("debug_logging", false)?
        // Optionals don't need defaults set here if handled later
        .build()?;

    // Deserialize into our Config struct
    let mut app_config: Config = settings.try_deserialize()?;

    // Handle optional overrides from CLI that weren't easy with set_override
    if cli_args.node_key_file.is_some() {
        app_config.node_key_file = cli_args.node_key_file;
    }
    if cli_args.bootnodes.is_some() {
        app_config.bootnodes = cli_args.bootnodes;
    }

    Ok(app_config)
}

/// Sets up logging to a daily rolling file.
pub fn setup_logging(debug_logging: bool) {
    let log_file = rolling::daily("./logs", "crawler.log"); // Log to ./logs/ dir

    let default_filter = if debug_logging {
        "eth_p2p_crawler=debug,reth_network=info,reth_discv4=info,reth_eth_wire=debug,reth_network::transactions=trace,crawler=trace"
    } else {
        "eth_p2p_crawler=info,reth_network=warn,reth_discv4=warn,reth_network::transactions=info,crawler=info"
    };
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_filter))
        .expect("Failed to parse log filter");

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(log_file)
                .with_ansi(false)
                .with_target(true),
        )
        .init();

    // This log goes to the file now
    info!(
        "Logging initialized (debug={}, outputting to ./logs/crawler.log)",
        debug_logging
    );
}

/// Loads a node key from file or generates a new one.
pub fn load_or_generate_key(file_path: Option<PathBuf>) -> Result<RethSecretKey> {
    if let Some(path) = file_path {
        info!("🔑 Loading node key from file: {:?}", path);
        let hex_content = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read key file {:?}: {}", path, e))?;
        let bytes = hex::decode(hex_content.trim())
            .map_err(|e| anyhow::anyhow!("Failed to decode hex in key file {:?}: {}", path, e))?;
        RethSecretKey::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("Invalid key data in file {:?}: {}", path, e))
    } else {
        info!("🔑 Node key file not provided, generating a new random key.");
        // Maybe save the generated key to a default file? Optional improvement.
        Ok(rng_secret_key())
    }
}

/// Parses bootnode strings or returns defaults.
pub fn parse_bootnodes(nodes_config: Option<Vec<String>>) -> Result<Vec<NodeRecord>> {
    let nodes_to_parse = match nodes_config {
        Some(nodes) if !nodes.is_empty() => {
            info!("Parsing bootnodes from config file or CLI.");
            nodes
        }
        _ => {
            info!("Using default mainnet bootnodes (config/CLI unspecified).");
            vec![
                "enode://d860a01f9722d78051619d1e2351aba3f43f943f6f00718d1b9baa4101932a1f5011f16bb2b1bb35db20d6fe28fa0bf09636d26a87d31de9ec6203eeedb1f666@18.138.108.67:30303".to_string(),
                "enode://22a8232c3abc76a16ae9d6c3b164f98775fe226f0917b0ca871128a74a8e9630b458460865bab457221f1d448dd9791d24c4e5d88786180ac185df813a68d4de@3.209.45.79:30303".to_string(),
                "enode://2b252ab6a1d0f971d9722cb839a42cb81db019ba44c08754628ab4a823487071b5695317c8ccd085219c3a03af063495b2f1da8d18218da2d6a82981b45e6ffc@65.108.70.101:30303".to_string(),
                "enode://4aeb4ab6c14b23e2c4cfdce879c04b0748a20d8e9b59e25ded2a08143e265c6c25936e74cbc8e641e3312ca288673d91f2f93f8e277de3cfa444ecdaaf982052@157.90.35.166:30303".to_string(),
                "enode://8499da03c47d637b20eee24eec3c356c9a2e6148d6fe25ca195c7949ab8ec2c03e3556126b0d7ed644675e78c4318b08691b7b57de10e5f0d40d05b09238fa0a@52.187.207.27:30303".to_string(),
                "enode://103858bdb88756c71f15e9b5e09b56dc1be52f0a5021d46301dbbfb7e130029cc9d0d6f73f693bc29b665770fff7da4d34f3c6379fe12721b5d7a0bcb5ca1fc1@191.234.162.198:30303".to_string(),
                "enode://715171f50508aba88aecd1250af392a45a330af91d7b90701c436b618c86aaa1589c9184561907bebbb56439b8f8787bc01f49a7c77276c58c1b09822d75e8e8@52.231.165.108:30303".to_string(),
                "enode://5d6d7cd20d6da4bb83a1d28cadb5d409b64edf314c0335df658c1a54e32c7c4a7ab7823d57c39b6a757556e68ff1df17c748b698544a55cb488b52479a92b60f@104.42.217.25:30303".to_string(),
            ]
        }
    };

    nodes_to_parse
        .iter()
        .map(|s| {
            NodeRecord::from_str(s)
                .map_err(|e| anyhow::anyhow!("Failed to parse bootnode '{}': {}", s, e))
        })
        .collect()
}