mod agent;

use core::future::pending;

use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use rand::RngCore;
use tokio::net::{TcpListener, UdpSocket};

use agent::AgentService;
use daemonbase::error::ExitError;
use daemonbase::logging::{self, Logger};
use daemonbase::process::{self, Process};
use domain::base::RelativeName;
use domain::net::server::buf::VecBufSource;
use domain::net::server::dgram::{self, DgramServer};
use domain::net::server::middleware::builder::MiddlewareBuilder;
use domain::net::server::middleware::chain::MiddlewareChain;
use domain::net::server::middleware::processors::cookies::CookiesMiddlewareProcessor;
use domain::net::server::stream::StreamServer;
use domain::net::server::{stream, ConnectionConfig};
use tracing::{error, info};

//----------- Args -----------------------------------------------------------

#[derive(clap::Parser)]
pub struct Args {
    /// Logging related settings
    #[command(flatten)]
    log: logging::Args,

    /// Detach from the terminal
    #[arg(short, long)]
    detach: bool,

    /// O/S process behaviour related settings
    #[command(flatten)]
    process: process::Args,

    /// The IP address to listen on
    #[arg(long = "addr", value_name = "LISTEN_ADDRESS", default_value = "[::]")]
    listen_address: String,

    /// The port to listen on
    #[arg(long = "port", value_name = "LISTEN_PORT", default_value = "53")]
    listen_port: u16,
}

//----------- init_middleware() ----------------------------------------------

fn init_middleware() -> MiddlewareChain<Vec<u8>, Vec<u8>> {
    let mut middleware = MiddlewareBuilder::<Vec<u8>, Vec<u8>>::standard();
    let mut server_secret = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut server_secret);
    let cookies = CookiesMiddlewareProcessor::new(server_secret);
    middleware.push(cookies.into());
    middleware.build()
}

//----------- init_service() -------------------------------------------------

fn init_service() -> Arc<AgentService<impl Fn(u16, u16, RelativeName<Vec<u8>>)>> {
    let svc = AgentService::new(|qtype, edns_err_code, qname| {
        println!("{qtype},{edns_err_code},{qname}")
    });
    Arc::new(svc)
}

//----------- main() ---------------------------------------------------------

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), ExitError> {
    Logger::init_logging()?;
    info!("Logging initialized");

    // Parse command line arguments
    let args = Args::parse();

    let log = Logger::from_config(&args.log.to_config())?;
    log.switch_logging(args.detach)?;

    let bind_address = format!("{}:{}", args.listen_address, args.listen_port);
    let bind_address = bind_address.parse::<SocketAddr>().unwrap();

    let mut process = Process::from_config(args.process.into_config());
    process.setup_daemon(args.detach)?;

    process.drop_privileges()?;

    // -----------------------------------------------------------------------
    // Create a service with accompanying middleware chain to answer incoming
    // requests.
    // https://www.rfc-editor.org/rfc/rfc9567#section-6.3-2 "The monitoring
    // agent SHOULD respond to queries received over UDP that have no DNS
    // Cookie set with a response that has the truncation bit (TC bit) set to
    // challenge the resolver to requery over TCP."
    let middleware = init_middleware();
    let svc = init_service();

    // -----------------------------------------------------------------------
    // Run a UDP DNS server.
    let Ok(udpsocket) = UdpSocket::bind(bind_address).await else {
        error!("Unable to bind to UDP address {bind_address}");
        std::process::exit(1);
    };

    let mut config = dgram::Config::default();
    config.set_middleware_chain(middleware.clone());
    let srv = DgramServer::with_config(udpsocket, VecBufSource, svc.clone(), config);
    tokio::spawn(async move { srv.run().await });

    // -----------------------------------------------------------------------
    // Run a TCP DNS server.
    let Ok(listener) = TcpListener::bind(bind_address).await else {
        error!("Unable to bind to UDP address {bind_address}");
        std::process::exit(1);
    };

    let mut conn_config = ConnectionConfig::default();
    conn_config.set_middleware_chain(middleware.clone());
    let mut config = stream::Config::default();
    config.set_connection_config(conn_config);
    let srv = StreamServer::with_config(listener, VecBufSource, svc, config);
    tokio::spawn(async move { srv.run().await });

    // Run until stopped.
    pending().await
}
