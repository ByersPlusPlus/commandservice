use bpp_command_api::traits::YouTubeSendable;
use tonic::{transport::Server, Request, Response, Status};
use ::log::{debug, error, info};
use crate::{loader::CommandProcessor, log::setup_log};
use std::{env, sync::{Arc, Mutex}};
use async_trait::async_trait;

use commandservice::*;

pub mod log;
mod loader;

pub mod commandservice {
    tonic::include_proto!("commandservice");
}

pub mod youtubeservice {
    tonic::include_proto!("youtubeservice");
}

#[async_trait]
impl YouTubeSendable for youtubeservice::you_tube_service_client::YouTubeServiceClient<tonic::transport::Channel> {
    async fn send_message(&mut self, message: &str) {
        let response = youtubeservice::you_tube_service_client::YouTubeServiceClient::send_message(&mut self, message.to_string()).await;
        if response.is_err() {
            error!("Error sending message to YouTube: {}", response.err().unwrap());
        }
    }
}

// Implement your proto here
// https://github.com/hyperium/tonic/blob/master/examples/helloworld-tutorial.md
// https://github.com/hyperium/tonic/blob/master/examples/routeguide-tutorial.md

fn ensure_command_directory() {
    let mut path = env::current_dir().unwrap();
    path.push("commands");
    if !path.exists() {
        std::fs::create_dir(path).unwrap();
    }
}

fn load_commands(loader: &mut CommandProcessor) {
    // for each file in the commands directory, that is a shared library, load it
    for entry in std::fs::read_dir("commands").unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_file() {
            let file_name = path.file_name().unwrap().to_str().unwrap();
            #[cfg(target_os = "linux")]
            if file_name.ends_with(".so") {
                info!("Loading library: {}", file_name);
                unsafe {
                    let load_result = loader.load(file_name);
                    if load_result.is_err() {
                        error!("Error loading library: {}", load_result.err().unwrap());
                    }
                }
            }
            #[cfg(target_os = "windows")]
            if file_name.ends_with(".dll") {
                info!("Loading library: {}", file_name);
                unsafe {
                    let load_result = loader.load(file_name);
                    if load_result.is_err() {
                        error!("Error loading library: {}", load_result.err().unwrap());
                    }
                }
            }
            #[cfg(target_os = "macos")]
            if file_name.ends_with(".dylib") {
                info!("Loading library: {}", file_name);
                unsafe {
                    let load_result = loader.load(file_name);
                    if load_result.is_err() {
                        error!("Error loading library: {}", load_result.err().unwrap());
                    }
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    setup_log(env::var_os("DEBUG").is_some());
    debug!("Debug mode activated!");

    let youtube_address = env::var("YTS_GRPC_ADDRESS").expect("YTS_GRPC_ADDRESS must be set");
    let user_address = env::var("US_GRPC_ADDRESS").expect("US_GRPC_ADDRESS must be set");

    let youtube_client = youtubeservice::you_tube_service_client::YouTubeServiceClient::connect(youtube_address).await?;
    let user_client = bpp_command_api::userservice::user_service_client::UserServiceClient::connect(user_address).await?;

    info!("Loading commands");
    let mut loader = CommandProcessor::new(youtube_client, user_client);
    ensure_command_directory();
    load_commands(&mut loader);

    let (_) = tokio::join!(
        loader.fetch_messages()
    );

    return Ok(());
}
