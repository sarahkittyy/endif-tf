# endif.tf

## Running 

```bash
# create .env, create db 
cp .env.example .env
docker compose up -d

# init server
cargo run -p endif-server

# two clients to test
cargo run -p endif-client
cargo run -p endif-client
```

## Web build

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <version of wasm-bindgen in Cargo.lock>
./build-web.sh
python tools/serve.py
```
