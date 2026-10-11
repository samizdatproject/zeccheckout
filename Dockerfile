# ZecCheckout + demo shop. Built for linux/amd64 (Render.com and most hosts).
#   docker build -t zeccheckout .
#   docker run -e ZC_UFVK=uviewtest1... -e ZC_API_KEY=... -p 10000:10000 zeccheckout
#   docker run ... zeccheckout /usr/local/bin/demo-shop          # the demo shop
FROM --platform=linux/amd64 rust:1.99.0-slim-bookworm@sha256:2c3a22f0a5533ea2dd5a16627bc841228151faa2d4de2644ac9987e4a2f1f2fa AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY static ./static
ENV CARGO_PROFILE_RELEASE_STRIP=true CARGO_INCREMENTAL=0
RUN cargo build --release --locked --bin zeccheckout --bin demo-shop

# Distroless: no shell, no package manager; glibc + libgcc only. All TLS is
# rustls with bundled webpki roots and SQLite is bundled, so nothing else is needed.
FROM --platform=linux/amd64 gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /src/target/release/zeccheckout /src/target/release/demo-shop /usr/local/bin/
WORKDIR /home/nonroot
USER nonroot
# Listen on all interfaces inside the container (Render's default PORT is 10000).
# The SQLite file lives in the container; mount a volume at /home/nonroot to keep it.
ENV ZC_BIND=0.0.0.0:10000 SHOP_BIND=0.0.0.0:10000 ZC_DB=/home/nonroot/zeccheckout.db
EXPOSE 10000
ENTRYPOINT []
CMD ["/usr/local/bin/zeccheckout", "serve"]
