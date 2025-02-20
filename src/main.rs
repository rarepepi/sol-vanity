use axum::{routing::post, Json, Router};
use log::info;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use solana_sdk::signer::{keypair::Keypair, Signer};
use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::mpsc::{channel, Sender},
    time::Instant,
};
use tower_http::cors::{Any, CorsLayer};

#[derive(Deserialize)]
struct GenerateRequest {
    target: String,
    case_insensitive: Option<bool>,
}

#[derive(Serialize)]
struct GenerateResponse {
    public_key: String,
    private_key: String,
    attempts: u64,
    time_taken: f64,
}

static EXIT: AtomicBool = AtomicBool::new(false);
static TOTAL_ATTEMPTS: AtomicU64 = AtomicU64::new(0);

#[tokio::main]
async fn main() {
    env_logger::init();

    let num_threads = num_cpus::get();
    rayon::ThreadPoolBuilder::new()
        .num_threads(num_threads)
        .build_global()
        .unwrap();

    info!(
        "Starting vanity address generator with {} threads",
        num_threads
    );

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/generate", post(handle_generate))
        .layer(cors);

    info!("Server running on http://localhost:8080");
    axum::Server::bind(&"0.0.0.0:8080".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn handle_generate(Json(payload): Json<GenerateRequest>) -> Json<GenerateResponse> {
    let (tx, rx) = channel();
    let case_insensitive = payload.case_insensitive.unwrap_or(false);
    let target = validate_target(&payload.target, case_insensitive);

    info!(
        "Starting new search for target '{}' (case insensitive: {})",
        target, case_insensitive
    );

    EXIT.store(false, Ordering::SeqCst);
    TOTAL_ATTEMPTS.store(0, Ordering::SeqCst);

    std::thread::spawn(move || {
        grind_keypairs(target, case_insensitive, tx);
    });

    Json(rx.recv().unwrap())
}

fn grind_keypairs(target: String, case_insensitive: bool, tx: Sender<GenerateResponse>) {
    let timer = Instant::now();
    let num_cpus = rayon::current_num_threads();
    let log_interval = std::time::Duration::from_secs(5);
    let last_log = std::sync::Arc::new(std::sync::Mutex::new(Instant::now()));

    (0..num_cpus).into_par_iter().for_each(|thread_id| {
        let tx = tx.clone();
        let last_log = last_log.clone();

        info!("Thread {} started searching", thread_id);

        while !EXIT.load(Ordering::Acquire) {
            let keypair = Keypair::new();
            let pubkey = keypair.pubkey().to_string();
            let pubkey_check = if case_insensitive {
                pubkey.to_lowercase()
            } else {
                pubkey.clone()
            };

            TOTAL_ATTEMPTS.fetch_add(1, Ordering::Relaxed);

            // Log progress every 5 seconds
            if thread_id == 0 {
                let mut last_log = last_log.lock().unwrap();
                if last_log.elapsed() >= log_interval {
                    let attempts = TOTAL_ATTEMPTS.load(Ordering::Relaxed);
                    let rate = attempts as f64 / timer.elapsed().as_secs_f64();
                    info!("Progress: {} attempts, {:.2} addresses/sec", attempts, rate);
                    *last_log = Instant::now();
                }
            }

            if pubkey_check.ends_with(&target) {
                let attempts = TOTAL_ATTEMPTS.load(Ordering::Relaxed);
                let time_taken = timer.elapsed().as_secs_f64();
                let rate = attempts as f64 / time_taken;

                info!(
                    "Found matching address after {} attempts in {:.2}s ({:.2} addresses/sec)",
                    attempts, time_taken, rate
                );

                let response = GenerateResponse {
                    public_key: pubkey,
                    private_key: bs58::encode(keypair.to_bytes()).into_string(),
                    attempts,
                    time_taken,
                };

                tx.send(response).unwrap();
                EXIT.store(true, Ordering::Release);
                break;
            }
        }
    });
}

fn validate_target(target: &str, case_insensitive: bool) -> String {
    const BS58_CHARS: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

    assert!(
        target.chars().all(|c| BS58_CHARS.contains(c)),
        "Target contains invalid base58 characters"
    );

    if case_insensitive {
        target.to_lowercase()
    } else {
        target.to_string()
    }
}
