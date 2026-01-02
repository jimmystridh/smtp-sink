FROM rust:1.83-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY public ./public
COPY tests ./tests
COPY benches ./benches

RUN cargo build --release

FROM alpine:3.21

RUN apk add --no-cache ca-certificates

COPY --from=builder /app/target/release/smtp-sink /usr/local/bin/

EXPOSE 1025 1080

ENTRYPOINT ["smtp-sink"]
