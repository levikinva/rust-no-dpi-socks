mod arguments;
mod context;
mod data;
mod request;
mod response;
mod transfer;

use std::error::Error;
use std::fmt::Display;

use arguments::Arguments;
use clap::Parser;
use context::Context;
use log::error;
use log::info;
use tokio::io::split;
use tokio::join;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::runtime::Builder;

use data::AuthMethod;
use request::AuthMethodsRequest;
use request::CommandRequest;
use transfer::TransferError;

use crate::data::Command;
use crate::response::AuthMethodResponse;
use crate::response::CommandResponse;
use crate::transfer::copy_data;

#[macro_use]
extern crate windows_service;

use std::ffi::OsString;
use windows_service::service_dispatcher;

define_windows_service!(ffi_service_main, my_service_main);

const SOCKS5_VERSION: u8 = 0x05;

macro_rules! log_error {
    ($e:expr, $message:expr) => {{
        match $e {
            Ok(value) => value,
            Err(error) => {
                error!("{}: {}", $message, error);

                return;
            }
        }
    }};
}

//sc create YoutubeDPI binPath= "C:\no-dpi-socks.exe" type=own start=auto

fn my_service_main(arguments: Vec<OsString>) {
    let arguments = Arguments::parse();
    let runtime = Builder::new_multi_thread().enable_io().build().expect("!!!!!");
    let context = Context::create(arguments, runtime);

    context.runtime().block_on(async {
        let listener = log_error!(
            TcpListener::bind((context.bind_address(), context.bind_port())).await,
            "Failed to bind address"
        );

        if let Ok(bind_address) = listener.local_addr() {
            info!("Listening on {}", bind_address);
        } else {
            info!("Listening...");
        }

        loop {
            match listener.accept().await {
                Ok((socket, _)) => {
                    let cntxt = context.clone();

                    context.runtime().spawn(async move {
                        match handle_client(cntxt, socket).await {
                            Ok(()) => {}
                            Err(error) => eprintln!("{}", error),
                        }
                    });
                }
                Err(error) => eprintln!("{}", error),
            }
        }
    });
}

fn main() -> Result<(), windows_service::Error> {
    env_logger::init();

    service_dispatcher::start("myservice", ffi_service_main)?;

    Ok(())
}

async fn handle_client(context: Context, mut stream: TcpStream) -> Result<(), Box<dyn Error>> {
    let source_address = stream.peer_addr()?;

    info!("Connection from {}", source_address);

    let auth_request = AuthMethodsRequest::read(&mut stream).await?;

    if !auth_request
        .methods()
        .contains(&AuthMethod::NoAuthenticationRequired)
    {
        return Ok(());
    }

    let response = AuthMethodResponse::create(SOCKS5_VERSION, AuthMethod::NoAuthenticationRequired);
    response.send(&mut stream).await?;

    let command_request = CommandRequest::read(&mut stream).await?;

    if command_request.command() != Command::Connect {
        let response = CommandResponse::command_not_supported(SOCKS5_VERSION);
        response.send(&mut stream).await?;
    } else if let Some(destination_address) = command_request.destination() {
        let destination = TcpStream::connect(destination_address).await?;
        destination.set_nodelay(true)?;
        stream.set_nodelay(true)?;

        let response = CommandResponse::success(SOCKS5_VERSION, &destination_address);
        response.send(&mut stream).await?;

        let (src_read, src_write) = split(stream);
        let (dst_read, dst_write) = split(destination);

        info!(
            "{} -> {}: start transfer",
            source_address, destination_address
        );

        let n_bytes = context.n_bytes();
        let copy_to = context
            .runtime()
            .spawn(async move { copy_data(src_read, dst_write, n_bytes).await });
        let copy_from = context
            .runtime()
            .spawn(async move { copy_data(dst_read, src_write, 0).await });

        let (in_result, out_result) = join!(copy_to, copy_from);

        info!(
            "{} -> {}: {}, {}",
            source_address,
            destination_address,
            bytes_string(in_result?, "in bytes: "),
            bytes_string(out_result?, "out bytes: ")
        );
    } else {
        let response = CommandResponse::host_unreachable(SOCKS5_VERSION);
        response.send(&mut stream).await?;
    }

    Ok(())
}

fn bytes_string<T>(result: Result<T, TransferError>, message: &str) -> String
where
    T: Display,
{
    match result {
        Ok(value) => format!("{}{}", message, value),
        Err(error) => format!("{}{} (error: {})", message, error.count(), error.message()),
    }
}
