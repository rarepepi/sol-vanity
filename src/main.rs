use axum::{routing::post, Json, Router};
use logfather::{Level, Logger};
use num_format::{Locale, ToFormattedString};
use rand::{distributions::Alphanumeric, Rng};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solana_pubkey::Pubkey;
use std::{
    array,
    str::FromStr,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::mpsc::{channel, Sender},
    time::Instant,
};
use tower_http::cors::{Any, CorsLayer};

// API Request/Response structures
#[derive(Deserialize)]
struct GenerateRequest {
    base: String,
    owner: String,
    target: String,
    case_insensitive: Option<bool>,
}

#[derive(Serialize)]
struct GenerateResponse {
    pubkey: String,
    seed: String,
    attempts: u64,
    time_taken: f64,
}

static EXIT: AtomicBool = AtomicBool::new(false);
static TOTAL_ATTEMPTS: AtomicU64 = AtomicU64::new(0);

#[tokio::main]
async fn main() {
    rayon::ThreadPoolBuilder::new().build_global().unwrap();

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/generate", post(handle_generate))
        .layer(cors);

    println!("Server running on http://localhost:3000");
    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn handle_generate(Json(payload): Json<GenerateRequest>) -> Json<GenerateResponse> {
    let (tx, rx) = channel();

    let program_id = parse_pubkey(&payload.owner).expect("Invalid program ID");
    let case_insensitive = payload.case_insensitive.unwrap_or(false);

    EXIT.store(false, Ordering::SeqCst);
    TOTAL_ATTEMPTS.store(0, Ordering::SeqCst);

    std::thread::spawn(move || {
        grind_pda_with_callback(program_id, &payload.target, case_insensitive, tx);
    });

    Json(rx.recv().unwrap())
}

fn grind_pda_with_callback(
    program_id: Pubkey,
    target: &str,
    case_insensitive: bool,
    tx: Sender<GenerateResponse>,
) {
    let target = get_validated_target(target, case_insensitive);
    let timer = Instant::now();

    #[cfg(feature = "gpu")]
    {
        println!("Starting GPU-based search for target: {}", target);
        println!("Case insensitive: {}", case_insensitive);
        
        let mut iteration = 0;
        let empty_owner = [0u8; 32];
        
        loop {
            if EXIT.load(Ordering::Acquire) {
                println!("Received exit signal, stopping GPU search");
                return;
            }

            if iteration % 100 == 0 {
                println!(
                    "GPU iteration {}, total attempts: {}, elapsed: {:.2}s",
                    iteration,
                    TOTAL_ATTEMPTS.load(Ordering::Relaxed),
                    timer.elapsed().as_secs_f64()
                );
            }

            let seed = new_gpu_seed(0, iteration);
            let mut out = [0u8; 24]; // 16 bytes for seed + 8 bytes for attempt count
            
            unsafe {
                vanity_round(
                    0,  // Use GPU 0
                    seed.as_ptr(),
                    program_id.as_ref().as_ptr(),
                    empty_owner.as_ptr(),
                    target.as_bytes().as_ptr(),
                    target.len() as u64,
                    out.as_mut_ptr(),
                    case_insensitive,
                );
            }
            
            // First 16 bytes contain the seed if found
            let found_seed = &out[..16];
            // Last 8 bytes contain the number of attempts
            let attempts = u64::from_le_bytes(out[16..24].try_into().unwrap());
            
            let previous_attempts = TOTAL_ATTEMPTS.fetch_add(attempts, Ordering::Relaxed);
            
            if attempts > 0 {
                println!(
                    "GPU batch completed: +{} attempts (total: {})",
                    attempts,
                    previous_attempts + attempts
                );
            }
            
            // If a result was found (non-zero seed)
            if found_seed.iter().any(|&x| x != 0) {
                let (pubkey, _bump) = Pubkey::find_program_address(&[found_seed], &program_id);
                println!("GPU found result!");
                println!("Seed: {}", String::from_utf8_lossy(found_seed));
                println!("Pubkey: {}", pubkey);
                
                tx.send(GenerateResponse {
                    pubkey: pubkey.to_string(),
                    seed: String::from_utf8(found_seed.to_vec()).unwrap(),
                    attempts: TOTAL_ATTEMPTS.load(Ordering::Relaxed),
                    time_taken: timer.elapsed().as_secs_f64(),
                })
                .unwrap();
                
                EXIT.store(true, Ordering::Release);
                println!("GPU search completed successfully");
                break;
            }
            
            iteration += 1;
        }
    }

    #[cfg(not(feature = "gpu"))]
    {
        // ... existing CPU implementation ...
        let num_cpus = rayon::current_num_threads() as u32;
        (0..num_cpus).into_par_iter().for_each(|_i| {
            let tx = tx.clone();

            loop {
                if EXIT.load(Ordering::Acquire) {
                    return;
                }

                let mut seed_iter = rand::thread_rng().sample_iter(&Alphanumeric).take(16);
                let seed: Vec<u8> = seed_iter.collect();
                
                let (pubkey, _bump) = Pubkey::find_program_address(&[&seed[..]], &program_id);
                let pubkey_str = pubkey.to_string();
                let out_str_target_check = maybe_bs58_aware_lowercase(&pubkey_str, case_insensitive);

                TOTAL_ATTEMPTS.fetch_add(1, Ordering::Relaxed);

                if out_str_target_check.ends_with(target) {
                    tx.send(GenerateResponse {
                        pubkey: pubkey_str,
                        seed: String::from_utf8(seed).unwrap(),
                        attempts: TOTAL_ATTEMPTS.load(Ordering::Relaxed),
                        time_taken: timer.elapsed().as_secs_f64(),
                    })
                    .unwrap();
                    EXIT.store(true, Ordering::Release);
                    break;
                }
            }
        });
    }
}

fn get_validated_target(target: &str, case_insensitive: bool) -> &'static str {
    const BS58_CHARS: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

    for c in target.chars() {
        assert!(
            BS58_CHARS.contains(c),
            "your target contains invalid bs58: {}",
            c
        );
    }

    let target = maybe_bs58_aware_lowercase(target, case_insensitive);
    target.leak()
}

fn maybe_bs58_aware_lowercase(target: &str, case_insensitive: bool) -> String {
    const LOWERCASE_EXCEPTIONS: &str = "L";

    if case_insensitive {
        target
            .chars()
            .map(|c| {
                if LOWERCASE_EXCEPTIONS.contains(c) {
                    c
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect::<String>()
    } else {
        target.to_string()
    }
}

extern "C" {
    pub fn vanity_round(
        gpus: u32,
        seed: *const u8,
        base: *const u8,
        owner: *const u8,
        target: *const u8,
        target_len: u64,
        out: *mut u8,
        case_insensitive: bool,
    );
}

#[cfg(feature = "gpu")]
fn new_gpu_seed(gpu_id: u32, iteration: u64) -> [u8; 32] {
    Sha256::new()
        .chain_update(rand::random::<[u8; 32]>())
        .chain_update(gpu_id.to_le_bytes())
        .chain_update(iteration.to_le_bytes())
        .finalize()
        .into()
}

fn parse_pubkey(input: &str) -> Result<Pubkey, String> {
    Pubkey::from_str(input).map_err(|e| e.to_string())
}
