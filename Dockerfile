FROM docker.io/lukemathwalker/cargo-chef:latest-rust-1.93-alpine AS chef
WORKDIR /app

# Stage: planner
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
RUN cargo chef prepare --recipe-path recipe.json

# Stage: builder
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --target x86_64-unknown-linux-musl --recipe-path recipe.json

COPY . .
RUN cargo build --release --target x86_64-unknown-linux-musl

# Stage: runner
FROM docker.io/alpine:latest
WORKDIR /app
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/factorio-server-dashboard .
ENTRYPOINT ["./factorio-server-dashboard"]
