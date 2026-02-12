use std::{
    collections::HashSet,
    env,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
    sync::Arc,
    time::Duration,
};

use dotenv::dotenv;
use linemux::MuxedLines;
use reqwest::Client;
use serde::Serialize;
use tokio::{
    sync::{
        RwLock,
        broadcast::{Receiver, Sender},
    },
    time::sleep,
};

struct AppState {
    online_players: RwLock<HashSet<String>>,
    tx: Sender<GameEvent>,
}

impl AppState {
    fn new(tx: Sender<GameEvent>) -> Self {
        Self {
            online_players: RwLock::new(HashSet::new()),
            tx: tx,
        }
    }

    async fn clear_active_players(&self) {
        let mut players = self.online_players.write().await;
        players.clear();
        let _ = self.tx.send(GameEvent::SessionReset);
    }

    async fn add_player(&self, name: &str) {
        let mut players = self.online_players.write().await;
        if players.insert(name.to_string()) {
            println!("Detected join event for: {}", name);
            let _ = self.tx.send(GameEvent::PlayerJoined(name.to_string()));
        }
    }

    async fn remove_player(&self, name: &str) {
        let mut players = self.online_players.write().await;
        if players.remove(name) {
            println!("Detected leave event for: {}", name);
            let _ = self.tx.send(GameEvent::PlayerLeft(name.to_string()));
        }
    }
}

#[derive(Clone)]
enum GameEvent {
    PlayerJoined(String),
    PlayerLeft(String),
    SessionReset,
}

#[derive(Serialize)]
struct TelegramPayload {
    chat_id: String,
    text: String,
    parse_mode: String,
}

struct TelegramNotifier {
    token: String,
    chat_id: String,
    client: Client,
}

impl TelegramNotifier {
    fn new(token: String, chat_id: String) -> Self {
        Self {
            token,
            chat_id,
            client: Client::new(),
        }
    }

    async fn notify(&self, message: &str) {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.token);

        let payload = TelegramPayload {
            chat_id: self.chat_id.clone(),
            text: message.to_string(),
            parse_mode: "HTML".to_string(),
        };

        let response = self.client.post(url).json(&payload).send().await;
        match response {
            Ok(res) => {
                if !res.status().is_success() {
                    let err_body = res.text().await.unwrap_or_default();
                    eprintln!("Telegram API Error: {}", err_body);
                }
            }
            Err(e) => eprintln!("HTTP Request Error: {}", e),
        }
    }
}

async fn notification_worker(mut rx: Receiver<GameEvent>, notifier: TelegramNotifier) {
    println!("Notification worker is started");

    while let Ok(event) = rx.recv().await {
        let message = match event {
            GameEvent::PlayerJoined(name) => {
                format!("<b>{}</b> joined the game", name)
            }
            GameEvent::PlayerLeft(name) => {
                format!("<b>{}</b> left the game", name)
            }
            GameEvent::SessionReset => "Server session restarted".to_string(),
        };

        println!("Notification: {}", &message);
        notifier.notify(&message).await;
    }
}

async fn sync_historical_state(state: &Arc<AppState>, log_path: &str) {
    if !std::path::Path::new(log_path).exists() {
        return; // Nothing to sync yet
    }

    println!("Reading history from file: {}", log_path);

    let file = File::open(log_path).expect(&format!("Failed to read log file: {log_path}"));
    let reader = BufReader::new(file);

    let mut players = state.online_players.write().await;

    for line in reader.lines() {
        let content = line.expect("Failed to read content");

        if content.contains("Server Session Started") {
            players.clear();
            continue;
        }

        let parts: Vec<&str> = content.split('|').map(|s| s.trim()).collect();
        if parts.len() == 3 {
            match parts[0] {
                "JOIN" => {
                    players.insert(parts[2].to_string());
                }
                "LEAVE" => {
                    players.remove(parts[2]);
                }
                _ => {}
            }
        }
    }
}

async fn watch_log(
    app_state: Arc<AppState>,
    log_path: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    sync_historical_state(&app_state, log_path).await;

    let mut lines = MuxedLines::new()?;
    lines
        .add_file(log_path)
        .await
        .expect(&format!("Failed to read log file: {log_path}"));

    while !Path::new(log_path).exists() {
        println!("Waiting for Factorio to create the log file...");
        sleep(Duration::from_secs(2)).await;
    }
    println!("Log monitor started.");

    while let Ok(Some(line)) = lines.next_line().await {
        let content = line.line();

        if content.contains("Server Session Started") {
            app_state.clear_active_players().await;
            println!("Session reset detected. Cleared player list");
            continue;
        }

        let parts: Vec<&str> = content.split('|').map(|s| s.trim()).collect();

        if parts.len() == 3 {
            let action = parts[0];
            let username = parts[2];

            match action {
                "JOIN" => {
                    app_state.add_player(username).await;
                }
                "LEAVE" => {
                    app_state.remove_player(username).await;
                }
                _ => {}
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    dotenv().ok();

    let (tx, rx) = tokio::sync::broadcast::channel::<GameEvent>(100);
    let app_state = Arc::new(AppState::new(tx));

    let factorio_log_path =
        env::var("FACTORIO_LOG_PATH").expect("FACTORIO_LOG_PATH env var is required");
    let telegram_token = env::var("TELEGRAM_TOKEN").expect("TELEGRAM_TOKEN env var is required");
    let telegram_chat_id =
        env::var("TELEGRAM_CHAT_ID").expect("TELEGAM_CHAT_ID env var is required");

    tokio::spawn(async move {
        if let Err(e) = watch_log(Arc::clone(&app_state), &factorio_log_path).await {
            eprintln!("Log monitor error: {}", e);
        }
    });

    let notifier = TelegramNotifier::new(telegram_token, telegram_chat_id);
    tokio::spawn(notification_worker(rx, notifier));

    let result: Result<(), std::io::Error> = tokio::signal::ctrl_c().await;
    result.unwrap();

    println!("Shutting down log monitor");
}
