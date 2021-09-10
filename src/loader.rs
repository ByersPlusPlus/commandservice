use std::{borrow::{Borrow, BorrowMut}, cell::{RefCell, RefMut}, collections::HashMap, ffi::OsStr, path::PathBuf, sync::{Arc, Mutex}, time::Duration};
use async_trait::async_trait;
use chrono::NaiveDateTime;
use tonic::{Request, Status, transport::Channel};

use super::youtubeservice::you_tube_service_client::YouTubeServiceClient;
use bpp_command_api::userservice::user_service_client::UserServiceClient;
use bpp_command_api::userservice::BppUserById;
use bpp_command_api::{CommandDeclaration, CommandError, structs::Message, traits::{Command, YouTubeSendable}};
use libloading::Library;
use log::{debug, error, warn};

type Void = Result<(), Box<dyn std::error::Error>>;

custom_error::custom_error! { pub ProcessorError
    CommandNotFound { command: String } = "Command {} not found",
    CommandExecutionFailed { command: String, library: String, message: String } = "Command {} (from library {}) errored with the following message: {}"
}

#[derive(Clone)]
pub struct CommandProxy {
    command: Box<dyn Command<dyn YouTubeSendable>>,
    _lib: Arc<Library>,
    _lib_name: String,
}

#[async_trait]
impl Command<super::youtubeservice::you_tube_service_client::YouTubeServiceClient<tonic::transport::Channel>> for CommandProxy {
    async fn execute(&self, message: Message, sendable: &mut YouTubeServiceClient<Channel>, user_client: &mut UserServiceClient<Channel>) -> Result<(), CommandError> {
        self.command.execute(message, sendable, user_client).await
    }
}

struct CommandRegistrar {
    commands: HashMap<String, CommandProxy>,
    lib: Arc<Library>,
    library_name: String
}

impl CommandRegistrar {
    fn new(lib: Arc<Library>, library_name: String) -> Self {
        CommandRegistrar {
            commands: HashMap::new(),
            lib,
            library_name
        }
    }
}

impl bpp_command_api::traits::CommandRegistrar for CommandRegistrar {
    fn register_command(&mut self, name: &str, aliases: &[&str], command: Box<dyn Command<dyn YouTubeSendable>>) {
        let proxy = CommandProxy {
            command,
            _lib: Arc::clone(&self.lib),
            _lib_name: self.library_name.clone()
        };

        for alias in aliases {
            self.commands.insert(alias.to_string(), proxy.clone());
        }
        self.commands.insert(name.to_string(), proxy);
    }
}

fn from_prost_timestamp(prost_timestamp: &prost_types::Timestamp) -> NaiveDateTime {
    NaiveDateTime::from_timestamp(prost_timestamp.seconds, prost_timestamp.nanos as u32)
}

type YouTubeClient = RefCell<YouTubeServiceClient<tonic::transport::Channel>>;
type UserClient = RefCell<UserServiceClient<tonic::transport::Channel>>;

pub struct CommandProcessor {
    commands: HashMap<String, CommandProxy>,
    libraries: Vec<Arc<Library>>,
    // youtube_sender: Arc<Mutex<YouTubeServiceClient<tonic::transport::Channel>>>,
    youtube_sender: YouTubeClient,
    userservice_client: UserClient,
}

impl CommandProcessor {
    //pub fn new(youtube_sender: Arc<Mutex<YouTubeServiceClient<tonic::transport::Channel>>>) -> Self {
    pub fn new(youtube_sender: YouTubeServiceClient<tonic::transport::Channel>, userservice_client: UserServiceClient<tonic::transport::Channel>) -> Self {
        CommandProcessor {
            commands: HashMap::new(),
            libraries: Vec::new(),
            youtube_sender: RefCell::new(youtube_sender),
            userservice_client: RefCell::new(userservice_client)
        }
    }

    pub async fn call(&self, sender: &mut YouTubeServiceClient<Channel>, user_client: &mut UserServiceClient<Channel>, message: Message) -> Result<(), ProcessorError> {
        if !self.commands.contains_key(&message.command_name) {
            return Err(ProcessorError::CommandNotFound { command: message.command_name.clone() });
        }
        let command = self.commands.get(&message.command_name).unwrap();
        // let sender_mut = self.youtube_sender.lock();
        // if sender_mut.is_err() {
            // warn!("Previous lock of the YouTube client panicked! This might be from a command.");
        // }
        // let mut sender = sender_mut.unwrap();
        let command_name = message.command_name.clone();
        let raw_message = message.message.clone();
        let library_name = command._lib_name.clone();

        let command_result = command.execute(message, sender, user_client).await;

        if command_result.is_err() {
            error!("{:?}", command_result.err().unwrap());
            return Err(ProcessorError::CommandExecutionFailed {
                command: command_name,
                library: library_name,
                message: raw_message
            });
        }

        return Ok(());
    }

    pub async fn fetch_messages(&self) -> Void {
        let mut sender = self.youtube_sender.borrow_mut();
        let mut user_service = self.userservice_client.borrow_mut();

        let mut stream = sender
            .subscribe_messages(Request::new(()))
            .await?
            .into_inner();
        
        while let Some(message) = stream.message().await? {
            let channel_id = message.channel_id.clone();
            let mut user = user_service.get_user_by_id(Request::new(BppUserById { channel_id })).await;
            if user.is_err() {
                let err = user.as_ref().err().unwrap();
                if err.code() == tonic::Code::NotFound {
                    debug!("User doesn't exist in userservice yet, waiting for 0.1 seconds and then trying again");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let channel_id = message.channel_id.clone();
                    user = user_service.get_user_by_id(Request::new(BppUserById { channel_id })).await;
                    if user.is_err() {
                        warn!("User doesn't exist in userservice, even with waiting, skipping message (this could also indicate the userservice not properly fetching users)");
                        continue;
                    }
                }
            }
            let user = user.unwrap();
            let user = user.into_inner();

            let command_message = Message::new(user.into(), message.message.clone());
            if !command_message.has_command_info {
                continue;
            }
            let command_result = self.call(&mut sender, &mut user_service, command_message).await;
            if command_result.is_err() {
                let error = command_result.err().unwrap();
                // if error is CommandNotFound, we log in debug and continue
                if let ProcessorError::CommandNotFound { command } = error {
                    debug!("Command {} could not be found, skipping", command);
                    continue;
                } else {
                    error!("{:?}", error);
                }
            }
        }

        return Ok(());
    }

    /// Load a plugin library and add all contained functions to the internal
    /// function table.
    ///
    /// # Safety
    ///
    /// A plugin library **must** be implemented using the
    /// [`bpp_command_api::export_command!()`] macro. Trying manually implement
    /// a plugin without going through that macro will result in undefined
    /// behaviour.
    pub unsafe fn load<P: AsRef<OsStr>>(&mut self, library_path: P) -> std::io::Result<()> {
        let path: PathBuf = library_path.as_ref().into();
        let library = Arc::new(Library::new(library_path).unwrap());

        let decl = library
            .get::<*mut CommandDeclaration>(b"command_declaration\0").unwrap()
            .read();
        
        if decl.rustc_version != bpp_command_api::RUSTC_VERSION || decl.core_version != bpp_command_api::CORE_VERSION {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "Version mismatch"));
        }

        let file_name = path.file_name().unwrap().to_str().unwrap().to_string();
        let mut registrar = CommandRegistrar::new(Arc::clone(&library), file_name);
        (decl.register)(&mut registrar);
        self.commands.extend(registrar.commands);
        self.libraries.push(library);

        return Ok(());
    }
}