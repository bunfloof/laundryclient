FROM rust:1.80 AS builder
WORKDIR /usr/src/laundryclient
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get -o Acquire::Check-Valid-Until=false -o Acquire::Check-Date=false update && \
    apt-get install -y libssl-dev && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/src/laundryclient/target/release/laundryclient /usr/local/bin/laundryclient
CMD ["laundryclient"]
