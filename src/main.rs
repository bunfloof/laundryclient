use serde::Deserialize;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::str;
use std::time::{Duration, Instant};
use tokio;

#[derive(Deserialize, Debug)]
struct Config {
    location: String,
    room: String,
}

#[derive(Deserialize, Debug)]
struct Machine {
    ip: String,
    l: String,
}

#[tokio::main]
async fn main() {
    let config_path = "/usr/local/etc/bright/config.json";
    let config: Config = match read_config(config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            // eprintln!("Failed to read config: {}", e);
            return;
        }
    };

    let server_address = "private:25651";

    loop {
        match TcpStream::connect(server_address) {
            Ok(mut stream) => {
                // println!("Connected to the server at {}", server_address);

                let identifier =
                    format!(r#"{{"location":"{}","room":"{}"}}"#, config.location, config.room);

                if stream.write_all(identifier.as_bytes()).is_err() {
                    // eprintln!("Failed to send identifier, retrying...");
                    continue;
                }

                let mut buffer = [0; 512];
                let mut last_keep_alive = Instant::now();

                loop {
                    if last_keep_alive.elapsed() > Duration::from_secs(30) {
                        if let Err(e) = stream.write_all(b"KEEP_ALIVE") {
                            // eprintln!("Failed to send keep-alive message: {}. Reconnecting...", e);
                            break;
                        }
                        last_keep_alive = Instant::now();
                    }

                    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();

                    match stream.read(&mut buffer) {
                        Ok(size) => {
                            if size == 0 {
                                // println!("Connection closed by the server. Reconnecting...");
                                break;
                            }

                            let message = str::from_utf8(&buffer[..size]).unwrap().trim();
                            // println!("Received message: {}", message);

                            if let Some(machine_label) = message.strip_prefix("Machine: ") {
                                if let Some(machine_ip) = find_machine_ip(machine_label) {
                                    let url =
                                        format!("http://{}:8080/action/putInStartMode", machine_ip);
                                    // println!("Sending GET request to: {}", url);

                                    // Spawn a new task for the GET request
                                    tokio::spawn(async move {
                                        match reqwest::get(&url).await {
                                            Ok(_) => { /* println!("GET request sent successfully to {}", machine_ip) */
                                            }
                                            Err(_) => { /* eprintln!("Failed to send GET request to {}: {}", machine_ip, e) */
                                            }
                                        }
                                    });
                                } else {
                                    // eprintln!("Machine {} not found in machines.json", machine_label);
                                }
                            }
                        }
                        Err(ref e)
                            if e.kind() == ErrorKind::WouldBlock
                                || e.kind() == ErrorKind::TimedOut =>
                        {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                        Err(e) => {
                            // eprintln!("Failed to receive data: {}. Reconnecting...", e);
                            break;
                        }
                    }

                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            Err(e) => {
                // eprintln!("Failed to connect to the server: {}. Retrying in 5 seconds...", e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

fn read_config(path: &str) -> Result<Config, Box<dyn std::error::Error>> {
    let config_data = fs::read_to_string(path)?;
    let config: Config = serde_json::from_str(&config_data)?;
    Ok(config)
}

fn find_machine_ip(machine_label: &str) -> Option<String> {
    let machines_path = "/usr/local/etc/bright/machines.json";
    let machines_data = fs::read_to_string(machines_path).expect("Failed to read machines.json");
    let machines: Vec<Machine> =
        serde_json::from_str(&machines_data).expect("Failed to parse machines.json");

    machines.iter().find(|m| m.l == machine_label).map(|m| m.ip.clone())
}
