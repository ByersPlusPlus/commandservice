use async_trait::async_trait;
use std::{ collections::HashMap, ffi::OsStr, path::PathBuf, sync::{Arc, Mutex}, time::Duration};
use tonic::{Request, transport::Channel};

use super::youtubeservice::you_tube_service_client::YouTubeServiceClient;
use bpp_command_api::{userservice::user_service_client::UserServiceClient};
use bpp_command_api::userservice::BppUserById;
use bpp_command_api::{
    structs::Message,
    traits::{Command, YouTubeSendable},
    CommandDeclaration, CommandError,
};
use libloading::Library;
use log::{debug, error, info, warn};

type Void = Result<(), Box<dyn std::error::Error>>;

custom_error::custom_error! { pub ProcessorError
    CommandNotFound { command: String } = "Command {} not found",
    CommandExecutionFailed { command: String, library: String, message: String } = "Command {} (from library {}) errored with the following message: {}",
    LoadError { library_name: String, message: String } = "Unable to load {}: {}",
    LibraryRustCVersionMismatch { library_name: String, rustc_version: String, actual_rustc_version: String } = "Library {} has a different rustc version than this core.\n\tExpected: {}\n\tActual: {}",
    LibraryCoreVersionMismatch { library_name: String, core_version: String, actual_core_version: String } = "Library {} has a different core version than this core.\n\tExpected: {}\n\tActual: {}"
}

#[derive(Clone)]
pub struct CommandProxy {
    pub command: Box<dyn Command<dyn YouTubeSendable>>,
    _lib: Arc<Library>,
    _lib_name: String,
    pub aliases: Vec<String>,
    pub is_alias: bool,
}

#[async_trait]
impl
    Command<
        super::youtubeservice::you_tube_service_client::YouTubeServiceClient<
            tonic::transport::Channel,
        >,
    > for CommandProxy
{
    async fn execute(
        &self,
        message: Message,
        sendable: &mut YouTubeServiceClient<Channel>,
        user_client: &mut UserServiceClient<Channel>,
    ) -> Result<(), CommandError> {
        self.command.execute(message, sendable, user_client).await
    }
}

struct CommandRegistrar {
    commands: HashMap<String, CommandProxy>,
    lib: Arc<Library>,
    library_name: String,
}

impl CommandRegistrar {
    fn new(lib: Arc<Library>, library_name: String) -> Self {
        CommandRegistrar {
            commands: HashMap::new(),
            lib,
            library_name,
        }
    }
}

impl bpp_command_api::traits::CommandRegistrar for CommandRegistrar {
    fn register_command(
        &mut self,
        name: &str,
        aliases: &[&str],
        command: Box<dyn Command<dyn YouTubeSendable>>,
    ) {
        let proxy = CommandProxy {
            command,
            _lib: Arc::clone(&self.lib),
            _lib_name: self.library_name.clone(),
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            is_alias: false,
        };

        for alias in aliases {
            let mut alias_proxy = proxy.clone();
            alias_proxy.is_alias = true;
            alias_proxy.aliases.clear();
            self.commands.insert(alias.to_string(), alias_proxy);
        }
        self.commands.insert(name.to_string(), proxy);
    }
}

type YouTubeClient = Arc<Mutex<YouTubeServiceClient<tonic::transport::Channel>>>;
type UserClient = Arc<Mutex<UserServiceClient<tonic::transport::Channel>>>;

pub struct CommandProcessor {
    libraries: Arc<Mutex<HashMap<String, Arc<CommandRegistrar>>>>,
    // youtube_sender: Arc<Mutex<YouTubeServiceClient<tonic::transport::Channel>>>,
    youtube_sender: YouTubeClient,
    userservice_client: UserClient,
}

impl CommandProcessor {
    //pub fn new(youtube_sender: Arc<Mutex<YouTubeServiceClient<tonic::transport::Channel>>>) -> Self {
    pub fn new(
        youtube_sender: YouTubeServiceClient<tonic::transport::Channel>,
        userservice_client: UserServiceClient<tonic::transport::Channel>,
    ) -> Self {
        CommandProcessor {
            libraries: Arc::new(Mutex::new(HashMap::new())),
            youtube_sender: Arc::new(Mutex::new(youtube_sender)),
            userservice_client: Arc::new(Mutex::new(userservice_client))
        }
    }

    pub async fn call(
        &self,
        sender: &mut YouTubeServiceClient<Channel>,
        user_client: &mut UserServiceClient<Channel>,
        message: Message,
    ) -> Result<(), ProcessorError> {
        let lib_clone = self.libraries.clone();
        let lib = lib_clone.lock().unwrap();
        let lookup = lib
            .values()
            .find_map(|lib| lib.commands.get(&message.command_name));

        if lookup.is_none() {
            return Err(ProcessorError::CommandNotFound {
                command: message.command_name.clone(),
            });
        }
        let command = lookup.unwrap();
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
                message: raw_message,
            });
        }

        return Ok(());
    }

    pub async fn fetch_messages(&self) -> Void {
        let sender = self.youtube_sender.lock();

        if sender.is_err() {
            warn!("Previous lock of the YouTube client panicked! This might be from a command.");
        }
        let mut sender = sender.unwrap();

        let user_service = self.userservice_client.lock();

        if user_service.is_err() {
            warn!("Previous lock of the User service client panicked! This might be from a command.");
        }
        let mut user_service = user_service.unwrap();

        let mut stream = sender
            .subscribe_messages(Request::new(()))
            .await?
            .into_inner();

        while let Some(message) = stream.message().await? {
            let channel_id = message.channel_id.clone();
            let mut user = user_service
                .get_user_by_id(Request::new(BppUserById { channel_id }))
                .await;
            if user.is_err() {
                let err = user.as_ref().err().unwrap();
                if err.code() == tonic::Code::NotFound {
                    debug!("User doesn't exist in userservice yet, waiting for 0.1 seconds and then trying again");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let channel_id = message.channel_id.clone();
                    user = user_service
                        .get_user_by_id(Request::new(BppUserById { channel_id }))
                        .await;
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
            let command_result = self
                .call(&mut sender, &mut user_service, command_message)
                .await;
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

    #[allow(dead_code)]
    pub fn unload<S: AsRef<str>>(&self, library_name: S) {
        let lib_clone = self.libraries.clone();
        let mut lib = lib_clone.lock().unwrap();

        let registrar = lib.remove(library_name.as_ref());
        if registrar.is_none() {
            warn!(
                "Library {} could not be found, skipping",
                library_name.as_ref()
            );
            return;
        }
        let registrar = registrar.unwrap();
        let registrar = Arc::<CommandRegistrar>::try_unwrap(registrar);

        if registrar.is_err() {
            error!("Error while trying to take ownership of command registrar {} (maybe it's still used somewhere?)", library_name.as_ref());
            lib
                .insert(library_name.as_ref().to_string(), registrar.err().unwrap());
            return;
        }
        let mut registrar = registrar.ok().unwrap();
        let commands: HashMap<String, CommandProxy> = registrar.commands.drain().collect();

        let library = Arc::<Library>::try_unwrap(registrar.lib);
        if library.is_err() {
            error!("Error while trying to take ownership of library {} (maybe it's still used somewhere?)", library_name.as_ref());
            let registrar = Arc::new(CommandRegistrar {
                lib: library.err().unwrap(),
                commands,
                library_name: library_name.as_ref().to_string(),
            });

            lib
                .insert(library_name.as_ref().to_string(), registrar);
            return;
        }
        let library = library.ok().unwrap();

        let success = library.close();
        if success.is_err() {
            let err = success.err().unwrap();
            error!(
                "An unrecoverable error occurred while unloading {}: {:?}",
                library_name.as_ref(),
                err
            );
            error!(
                "It might be a good idea to restart the service to prevent possible memory leaks!"
            );
            error!("Unlike all of the other errors that occur, this one will prevent the commands from this library to run in order to prevent the service from panicking!");
        }

        registrar.commands.clear();
    }

    /// Load a plugin library and add all contained functions to the internal
    /// function table.
    ///
    /// # Safety
    ///
    /// A plugin library **must** be implemented using the
    /// [`bpp_command_api::export_command!()`] macro. Trying manually implement
    /// a plugin without going through that macro will result in undefined
    /// behavior.
    pub unsafe fn load<P: AsRef<OsStr>>(&self, library_path: P) -> Result<(), ProcessorError> {
        let path: PathBuf = library_path.as_ref().into();
        let file_name = path.file_name().unwrap().to_str().unwrap().to_string();
        let library = Library::new(library_path);

        if library.is_err() {
            let err = library.err().unwrap();
            return Err(ProcessorError::LoadError {
                library_name: file_name,
                message: err.to_string(),
            });
        }
        let library = library.unwrap();
        let library_arc = Arc::new(library);

        let decl = library_arc
            .get::<*mut CommandDeclaration>(b"command_declaration\0")
            .unwrap()
            .read();

        if decl.rustc_version != bpp_command_api::RUSTC_VERSION {
            return Err(ProcessorError::LibraryRustCVersionMismatch {
                library_name: file_name,
                rustc_version: bpp_command_api::RUSTC_VERSION.to_string(),
                actual_rustc_version: decl.rustc_version.to_string(),
            });
        }

        if decl.core_version != bpp_command_api::CORE_VERSION {
            return Err(ProcessorError::LibraryCoreVersionMismatch {
                library_name: file_name,
                core_version: bpp_command_api::CORE_VERSION.to_string(),
                actual_core_version: decl.core_version.to_string(),
            });
        }

        let mut registrar = CommandRegistrar::new(Arc::clone(&library_arc), file_name.clone());
        (decl.register)(&mut registrar);
        let lib_clone = self.libraries.clone();
        let mut lib = lib_clone.lock().unwrap();
        lib
            .insert(file_name, Arc::new(registrar));

        Ok(())
    }
}

pub struct CommandServiceServer {
    pub processor: Arc<CommandProcessor>
}

#[async_trait]
impl super::command_service_server::CommandService for CommandServiceServer {
    async fn get_commands(
        &self,
        _: tonic::Request<()>,
    ) -> Result<tonic::Response<crate::commandservice::CommandList>, tonic::Status> {
        info!("Getting commands");
        let mut commands: Vec<super::commandservice::Command> = Vec::new();
        let lib_clone = self.processor.libraries.clone();
        info!("Acquiring mutex lock");
        let lib = lib_clone.lock().unwrap();
        info!("Iterating over libraries");
        for (library, registrar) in lib.iter() {
            for (name, command) in &registrar.commands {
                if command.is_alias {
                    continue;
                }
                let command_name = name.clone();
                let cmd = super::commandservice::Command {
                    name: command_name,
                    aliases: command.aliases.clone(),
                    description: "A command for ByersPlusPlus".to_string(),
                    library: library.clone(),
                };
                commands.push(cmd);
            }
        }

        info!("Sending response");
        let count = lib.len() as i32;
        let command_list = super::commandservice::CommandList { commands, count };
        return Ok(tonic::Response::new(command_list));
    }

    async fn get_command(
        &self,
        request: tonic::Request<prost::alloc::string::String>,
    ) -> Result<tonic::Response<crate::commandservice::Command>, tonic::Status> {
        let lib_clone = self.processor.libraries.clone();
        let lib = lib_clone.lock().unwrap();
        let command_name = request.into_inner();
        let mut found = false;
        let mut found_command = None;
        for (library, registrar) in lib.iter() {
            for (name, command) in &registrar.commands {
                if command.is_alias {
                    continue;
                }
                if name == &command_name {
                    found = true;
                    found_command = Some(super::commandservice::Command {
                        name: command_name.clone(),
                        aliases: command.aliases.clone(),
                        description: "A command for ByersPlusPlus".to_string(),
                        library: library.clone(),
                    });
                    break;
                }
            }
            if found {
                break;
            }
        }

        if !found || found_command.is_none() {
            return Err(tonic::Status::not_found(format!("")));
        }
        let found_command = found_command.unwrap();
        return Ok(tonic::Response::new(found_command));
    }
}
