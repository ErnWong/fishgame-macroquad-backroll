#[macroquad::main("Fish Lobby")]
async fn main() {
    server::lobby_main().await;
}
