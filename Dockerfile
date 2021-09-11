FROM rust:1.54.0 as build

# create a new empty shell project
RUN USER=root cargo new --bin commandservice
RUN USER=root mv -v commandservice/src/main.rs commandservice/src/server.rs
WORKDIR /commandservice

# copy over your manifests
COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml

# this build step will cache your dependencies
RUN rustup component add rustfmt
RUN cargo build --release
RUN rm src/*.rs

# copy your source tree and additional build files
COPY ./src ./src
COPY ./proto ./proto
COPY ./build.rs ./build.rs

# build for release
RUN rm ./target/release/deps/commandservice*
RUN cargo build --release

# our final base
FROM rust:1.54.0-slim-buster

RUN apt update && apt install -y wait-for-it libc6

# copy the build artifact from the build stage
COPY --from=build /commandservice/target/release/commandservice-server .

# set the startup command to run your binary
CMD ["./commandservice-server"]