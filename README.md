# commandservice

Microservice responsible for handling commands sent by users.

## How it works

commandservice loads commands via dynamic libraries (on Windows these are .dll files, on Linux it's .so files and on macOS it's .dylib files) when it starts up. The commands are loaded via Rust's `libloading` crate, which loads a library and can extract function pointers and run them, effectively allowing the microservice to load and unload commands.

commandservice depends on both [youtubeservice](https://github.com/ByersPlusPlus/youtubeservice) and [userservice](https://github.com/ByersPlusPlus/userservice) to fetch messages and look up the user.
