mod backend;
mod protocol;

use anyhow::{Context, Result, bail};
use backend::Backend;
use clap::{Parser, ValueEnum};
use encoding_rs::{EUC_JP, Encoding, UTF_8};
use karukan_engine::{Dictionary, KanaKanjiConverter};
use karukan_im::config::Settings;
use std::io::BufReader;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

const VERSION: &str = concat!("karukan-skkserv.", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Copy, ValueEnum)]
enum Charset {
    EucJp,
    Utf8,
}

/// skkserv-compatible server backed by the karukan conversion engine
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Address to bind to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Port to listen on
    #[arg(short, long, default_value_t = 1178)]
    port: u16,
    /// Wire encoding
    #[arg(long, value_enum, default_value = "euc-jp")]
    encoding: Charset,
    /// Dictionary path [default: karukan's configured dict.bin]
    #[arg(long)]
    dict: Option<PathBuf>,
    /// Key in karukan's [models] table [default: conversion.model]
    #[arg(long)]
    model: Option<String>,
    /// Serve dictionary results only
    #[arg(long)]
    no_model: bool,
    /// Number of model candidates per lookup
    #[arg(long, default_value_t = 3)]
    model_candidates: usize,
    /// Maximum candidates returned per lookup
    #[arg(long, default_value_t = 30)]
    max_candidates: usize,
    /// Time budget for one model conversion, in milliseconds
    #[arg(long, default_value_t = 2000)]
    timeout_ms: u64,
    /// Inference threads [default: conversion.n_threads]
    #[arg(long)]
    threads: Option<u32>,
    /// Maximum simultaneous client connections
    #[arg(long, default_value_t = 32)]
    max_connections: usize,
}

fn load_dictionary(args: &Args, settings: &Settings) -> Option<Dictionary> {
    let path = args
        .dict
        .clone()
        .or_else(|| settings.conversion.dict_path.as_ref().map(PathBuf::from))
        .or_else(|| Settings::data_dir().map(|dir| dir.join("dict.bin")))?;
    match Dictionary::load_auto(&path) {
        Ok(dictionary) => {
            tracing::info!("loaded dictionary {}", path.display());
            Some(dictionary)
        }
        Err(e) => {
            tracing::warn!("no dictionary loaded from {}: {e}", path.display());
            None
        }
    }
}

fn spawn_model_loader(
    backend: Arc<Backend>,
    settings: &Settings,
    key: String,
    threads: u32,
) -> Result<()> {
    let source = settings.model_source(&key)?;
    thread::spawn(move || {
        tracing::info!("loading model {key}");
        match KanaKanjiConverter::from_source(&source, &key) {
            Ok(mut model) => {
                model.set_n_threads(threads);
                backend.install_model(model);
                tracing::info!("model {key} ready");
            }
            Err(e) => tracing::error!("model {key} unavailable, serving dictionary only: {e}"),
        }
    });
    Ok(())
}

struct ConnectionSlot(Arc<AtomicUsize>);

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn handle(
    stream: TcpStream,
    backend: &Backend,
    encoding: &'static Encoding,
    host: &str,
) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    protocol::serve(&mut reader, &mut writer, backend, encoding, VERSION, host)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    if args.model_candidates == 0 || args.max_candidates == 0 || args.max_connections == 0 {
        bail!("candidate and connection limits must be at least 1");
    }
    let encoding: &'static Encoding = match args.encoding {
        Charset::EucJp => EUC_JP,
        Charset::Utf8 => UTF_8,
    };

    let settings = Settings::load_or_default();
    let listener = TcpListener::bind((args.host.as_str(), args.port))
        .with_context(|| format!("cannot bind {}:{}", args.host, args.port))?;
    let local = listener.local_addr()?;
    let host_info = format!("karukan-skkserv:{}", local.ip());

    let backend = Arc::new(Backend::new(
        load_dictionary(&args, &settings),
        args.model_candidates,
        args.max_candidates,
        Duration::from_millis(args.timeout_ms),
    ));
    if !args.no_model {
        let key = args
            .model
            .clone()
            .unwrap_or_else(|| settings.conversion.model.clone());
        let threads = args.threads.unwrap_or(settings.conversion.n_threads);
        spawn_model_loader(Arc::clone(&backend), &settings, key, threads)?;
    }

    tracing::info!("listening on {local}");
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(e) => {
                tracing::warn!("accept failed: {e}");
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        if active.fetch_add(1, Ordering::SeqCst) >= args.max_connections {
            active.fetch_sub(1, Ordering::SeqCst);
            tracing::warn!("connection limit reached, rejecting client");
            continue;
        }
        let slot = ConnectionSlot(Arc::clone(&active));
        let backend = Arc::clone(&backend);
        let host_info = host_info.clone();
        thread::spawn(move || {
            let _slot = slot;
            if let Err(e) = handle(stream, &backend, encoding, &host_info) {
                tracing::debug!("connection ended: {e}");
            }
        });
    }
    Ok(())
}
