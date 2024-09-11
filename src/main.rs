use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{self, Value};
use std::fs;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::{
    connect_async,
    tungstenite::protocol::Message,
    MaybeTlsStream,
    WebSocketStream,
};
use url::Url;

#[derive(Deserialize, Debug)]
struct Config {
    location: String,
    room: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct ServerMessage {
    action: String,
    payload: serde_json::Value,
}

#[tokio::main]
async fn main() {
    let mut config: Option<Config> = None;
    let timeout_duration = Duration::from_secs(5);

    while config.is_none() {
        let start_time = Instant::now();
        let fetch_future = fetch_config();
        
        tokio::select! {
            result = fetch_future => {
                match result {
                    Ok(cfg) => {
                        config = Some(cfg);
                        //println!("Successfully fetched config.");
                    },
                    Err(e) => {
                        //eprintln!("Error fetching config: {}. Retrying in 5 seconds...", e);
                    }
                }
            }
            _ = sleep(timeout_duration) => {
                //eprintln!("Config fetch timed out after 5 seconds. Retrying...");
            }
        }

        if config.is_none() {
            let elapsed = start_time.elapsed();
            if elapsed < timeout_duration {
                sleep(timeout_duration - elapsed).await;
            }
        }
    }

    let config = config.unwrap();

    let server_url = Url::parse("ws://private").expect("Invalid WebSocket URL");
    
    loop {
        //println!("Attempting to connect to the server...");
        match connect_async(server_url.clone()).await {
            Ok((ws_stream, _)) => {
                //println!("Connected to the server at {}", server_url);
                if let Err(e) = handle_connection(ws_stream, &config).await {
                    //eprintln!("Connection error: {}. Reconnecting...", e);
                }
            }
            Err(e) => {
                //eprintln!("Failed to connect: {}. Retrying in 5 seconds...", e);
            }
        }
        sleep(Duration::from_secs(5)).await;
        //println!("Attempting to reconnect...");
    }
}

async fn fetch_config() -> Result<Config, Box<dyn std::error::Error>> {
    let response = reqwest::get("http://10.43.0.1:8080/local/status").await?;
    let status: serde_json::Value = response.json().await?;
    
    Ok(Config {
        location: status["location"].as_str().unwrap_or("").to_string(),
        room: status["room"].as_str().unwrap_or("").to_string(),
    })
}

async fn handle_connection(ws_stream: WebSocketStream<MaybeTlsStream<TcpStream>>, config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let (mut write, mut read) = ws_stream.split();

    let connect_message = ServerMessage {
        action: "CONNECT".to_string(),
        payload: serde_json::json!({
            "location": config.location,
            "room": config.room,
        }),
    };
    write.send(Message::Text(serde_json::to_string(&connect_message)?)).await?;
    //println!("Sent connect message: {}", serde_json::to_string(&connect_message)?);

    let (tx, mut rx) = mpsc::channel(32);

    let mut keep_alive_interval = tokio::time::interval(Duration::from_secs(20)); // was 5 secs
    let mut last_server_response = Instant::now();

    loop {
        tokio::select! {
            _ = keep_alive_interval.tick() => {
                if last_server_response.elapsed() > Duration::from_secs(60) {
                    return Err("Server unresponsive".into());
                }

                //println!("Sending KEEP_ALIVE message");
                let keep_alive_message = ServerMessage {
                    action: "KEEP_ALIVE".to_string(),
                    payload: serde_json::json!({
                        "location": config.location,
                        "room": config.room,
                    }),
                };
                write.send(Message::Text(serde_json::to_string(&keep_alive_message)?)).await?;
                //println!("Sent KEEP_ALIVE message");
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(message)) => {
                        //println!("Received: {message:?}");
                        last_server_response = Instant::now();
                        if let Message::Text(text) = message {
                            if let Ok(server_message) = serde_json::from_str::<ServerMessage>(&text) {
                                let tx_clone = tx.clone();
                                tokio::spawn(async move {
                                    handle_server_message(server_message, tx_clone).await;
                                });
                            }
                        }
                    }
                    Some(Err(e)) => {
                        //eprintln!("Error reading message: {}", e);
                        return Err(e.into());
                    }
                    None => {
                        //println!("WebSocket stream ended");
                        return Ok(());
                    }
                }
            }
            Some(response) = rx.recv() => {
                write.send(Message::Text(serde_json::to_string(&response)?)).await?;
            }
        }
    }
}

async fn handle_server_message(server_message: ServerMessage, tx: mpsc::Sender<ServerMessage>) {
    match server_message.action.as_str() {
        "START_MACHINE" => {
            if let (Some(machine_label), Some(request_id)) = (
                server_message.payload["machine"].as_str(),
                server_message.payload["request_id"].as_str()
            ) {
                match timeout(Duration::from_secs(5), find_machine_ip(machine_label)).await {
                    Ok(Some(machine_ip)) => {
                        let url = format!("http://{}:8080/action/putInStartMode", machine_ip);
                        //println!("Sending GET request to: {}", url);

                        let response_message = match timeout(Duration::from_secs(5), reqwest::get(&url)).await {
                            Ok(Ok(response)) => {
                                match timeout(Duration::from_secs(5), response.text()).await {
                                    Ok(Ok(response_text)) => {
                                        let payload = match serde_json::from_str::<serde_json::Value>(&response_text) {
                                            Ok(json_data) => json_data,
                                            Err(_) => serde_json::json!({ "message": response_text }),
                                        };

                                        ServerMessage {
                                            action: "START_MACHINE_RESPONSE".to_string(),
                                            payload: serde_json::json!({
                                                "data": payload,
                                                "request_id": request_id
                                            }),
                                        }
                                    }
                                    Ok(Err(e)) => {
                                        ServerMessage {
                                            action: "START_MACHINE_RESPONSE".to_string(),
                                            payload: serde_json::json!({
                                                "error": format!("Failed to read response: {}", e),
                                                "request_id": request_id 
                                            }),
                                        }
                                    }
                                    Err(_) => {
                                        ServerMessage {
                                            action: "START_MACHINE_RESPONSE".to_string(),
                                            payload: serde_json::json!({
                                                "error": "Timeout while reading response",
                                                "request_id": request_id 
                                            }),
                                        }
                                    }
                                }
                            }
                            Ok(Err(e)) => {
                                ServerMessage {
                                    action: "START_MACHINE_RESPONSE".to_string(),
                                    payload: serde_json::json!({
                                        "error": format!("Failed to send GET request to {}: {}", machine_ip, e),
                                        "request_id": request_id 
                                    }),
                                }
                            }
                            Err(_) => {
                                ServerMessage {
                                    action: "START_MACHINE_RESPONSE".to_string(),
                                    payload: serde_json::json!({ 
                                        "error": "Request timed out after 5 seconds",
                                        "request_id": request_id
                                    }),
                                }
                            }
                        };

                        let _ = tx.send(response_message).await;
                    }
                    Ok(None) => {
                        let response_message = ServerMessage {
                            action: "START_MACHINE_RESPONSE".to_string(),
                            payload: serde_json::json!({
                                "error": format!("Machine {} not found", machine_label),
                                "request_id": request_id
                            }),
                        };
                        let _ = tx.send(response_message).await;
                    }
                    Err(_) => {
                        let response_message = ServerMessage {
                            action: "START_MACHINE_RESPONSE".to_string(),
                            payload: serde_json::json!({
                                "error": "Timeout while finding machine IP",
                                "request_id": request_id
                            }),
                        };
                        let _ = tx.send(response_message).await;
                    }
                }
            }
        }
        "GET_MACHINES" => {
            if let Some(request_id) = server_message.payload["request_id"].as_str() {
                let machines_url = "http://10.43.0.1:8080/machines";
                let response_message = match timeout(Duration::from_secs(5), reqwest::get(machines_url)).await {
                    Ok(Ok(response)) => {
                        match timeout(Duration::from_secs(5), response.text()).await {
                            Ok(Ok(machines_data)) => {
                                let payload = match serde_json::from_str::<serde_json::Value>(&machines_data) {
                                    Ok(json_data) => json_data,
                                    Err(_) => serde_json::json!({ "data": machines_data }),
                                };

                                ServerMessage {
                                    action: "MACHINES_RESPONSE".to_string(),
                                    payload: serde_json::json!({
                                        "data": payload,
                                        "request_id": request_id
                                    }),
                                }
                            }
                            Ok(Err(e)) => {
                                ServerMessage {
                                    action: "MACHINES_RESPONSE".to_string(),
                                    payload: serde_json::json!({ 
                                        "error": format!("Failed to read machines data: {}", e) ,
                                        "request_id": request_id
                                    }),
                                }
                            }
                            Err(_) => {
                                ServerMessage {
                                    action: "MACHINES_RESPONSE".to_string(),
                                    payload: serde_json::json!({ 
                                        "error": "Timeout while reading response",
                                        "request_id": request_id 
                                    }),
                                }
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        ServerMessage {
                            action: "MACHINES_RESPONSE".to_string(),
                            payload: serde_json::json!({ 
                                "error": format!("Failed to get machines data: {}", e),
                                "request_id": request_id
                            }),
                        }
                    }
                    Err(_) => {
                        ServerMessage {
                            action: "MACHINES_RESPONSE".to_string(),
                            payload: serde_json::json!({
                                "error": "Request timed out after 5 seconds",
                                "request_id": request_id
                            }),
                        }
                    }
                };

                let _ = tx.send(response_message).await;
            }
        }
        "GET_MACHINE_STATUS" => {
            if let (Some(machine_label), Some(request_id)) = (
                server_message.payload["machine"].as_str(),
                server_message.payload["request_id"].as_str()
            ) {
                match timeout(Duration::from_secs(5), find_machine_ip(machine_label)).await {
                    Ok(Some(machine_ip)) => {
                        let url = format!("http://{}:8080/raw/status", machine_ip);
                        //println!("Sending GET request to: {}", url);

                        let response_message = match timeout(Duration::from_secs(5), reqwest::get(&url)).await {
                            Ok(Ok(response)) => {
                                match timeout(Duration::from_secs(5), response.text()).await {
                                    Ok(Ok(response_text)) => {
                                        let payload = match serde_json::from_str::<serde_json::Value>(&response_text) {
                                            Ok(json_data) => json_data,
                                            Err(_) => serde_json::json!({ "error": "Failed to parse machine status" }),
                                        };

                                        ServerMessage {
                                            action: "MACHINE_STATUS_RESPONSE".to_string(),
                                            payload: serde_json::json!({
                                                "data": payload,
                                                "request_id": request_id
                                            }),
                                        }
                                    }
                                    Ok(Err(e)) => {
                                        ServerMessage {
                                            action: "MACHINE_STATUS_RESPONSE".to_string(),
                                            payload: serde_json::json!({ 
                                                "error": format!("Failed to read response: {}", e),
                                                "request_id": request_id
                                            }),
                                        }
                                    }
                                    Err(_) => {
                                        ServerMessage {
                                            action: "MACHINE_STATUS_RESPONSE".to_string(),
                                            payload: serde_json::json!({ 
                                                "error": "Timeout while reading response",
                                                "request_id": request_id
                                            }),
                                        }
                                    }
                                }
                            }
                            Ok(Err(e)) => {
                                ServerMessage {
                                    action: "MACHINE_STATUS_RESPONSE".to_string(),
                                    payload: serde_json::json!({ 
                                        "error": format!("Failed to send GET request to {}: {}", machine_ip, e),
                                        "request_id": request_id
                                    }),
                                }
                            }
                            Err(_) => {
                                ServerMessage {
                                    action: "MACHINE_STATUS_RESPONSE".to_string(),
                                    payload: serde_json::json!({ 
                                        "error": "Request timed out after 5 seconds",
                                        "request_id": request_id
                                    }),
                                }
                            }
                        };

                        let _ = tx.send(response_message).await;
                    }
                    Ok(None) => {
                        let response_message = ServerMessage {
                            action: "MACHINE_STATUS_RESPONSE".to_string(),
                            payload: serde_json::json!({ 
                                "error": format!("Machine {} not found", machine_label),
                                "request_id": request_id
                            }),
                        };
                        let _ = tx.send(response_message).await;
                    }
                    Err(_) => {
                        let response_message = ServerMessage {
                            action: "MACHINE_STATUS_RESPONSE".to_string(),
                            payload: serde_json::json!({ 
                                "error": "Timeout while finding machine IP",
                                "request_id": request_id
                            }),
                        };
                        let _ = tx.send(response_message).await;
                    }
                }
            }
        }
        "GET_MACHINE_HEALTH" => {
            if let (Some(machine_label), Some(request_id)) = (
                server_message.payload["machine"].as_str(),
                server_message.payload["request_id"].as_str()
            ) {
                match timeout(Duration::from_secs(5), find_machine_ip(machine_label)).await {
                    Ok(Some(machine_ip)) => {
                        let url = format!("http://{}:8080/health", machine_ip);
                        //println!("Sending GET request to: {}", url);

                        let response_message = match timeout(Duration::from_secs(5), reqwest::get(&url)).await {
                            Ok(Ok(response)) => {
                                match timeout(Duration::from_secs(5), response.text()).await {
                                    Ok(Ok(response_text)) => {
                                        let payload = match serde_json::from_str::<serde_json::Value>(&response_text) {
                                            Ok(json_data) => json_data,
                                            Err(_) => serde_json::json!({ "message": response_text }),
                                        };

                                        ServerMessage {
                                            action: "MACHINE_HEALTH_RESPONSE".to_string(),
                                            payload: serde_json::json!({
                                                "data": payload,
                                                "request_id": request_id
                                            }),
                                        }
                                    }
                                    Ok(Err(e)) => {
                                        ServerMessage {
                                            action: "MACHINE_HEALTH_RESPONSE".to_string(),
                                            payload: serde_json::json!({ 
                                                "error": format!("Failed to read response: {}", e),
                                                "request_id": request_id
                                            }),
                                        }
                                    }
                                    Err(_) => {
                                        ServerMessage {
                                            action: "MACHINE_HEALTH_RESPONSE".to_string(),
                                            payload: serde_json::json!({ 
                                                "error": "Timeout while reading response",
                                                "request_id": request_id
                                            }),
                                        }
                                    }
                                }
                            }
                            Ok(Err(e)) => {
                                ServerMessage {
                                    action: "MACHINE_HEALTH_RESPONSE".to_string(),
                                    payload: serde_json::json!({ 
                                        "error": format!("Failed to send GET request to {}: {}", machine_ip, e),
                                        "request_id": request_id
                                    }),
                                }
                            }
                            Err(_) => {
                                ServerMessage {
                                    action: "MACHINE_HEALTH_RESPONSE".to_string(),
                                    payload: serde_json::json!({ 
                                        "error": "Request timed out after 5 seconds",
                                        "request_id": request_id
                                    }),
                                }
                            }
                        };

                        let _ = tx.send(response_message).await;
                    }
                    Ok(None) => {
                        let response_message = ServerMessage {
                            action: "MACHINE_HEALTH_RESPONSE".to_string(),
                            payload: serde_json::json!({ 
                                "error": format!("Machine {} not found", machine_label),
                                "request_id": request_id
                            }),
                        };
                        let _ = tx.send(response_message).await;
                    }
                    Err(_) => {
                        let response_message = ServerMessage {
                            action: "MACHINE_HEALTH_RESPONSE".to_string(),
                            payload: serde_json::json!({ 
                                "error": "Timeout while finding machine IP",
                                "request_id": request_id
                            }),
                        };
                        let _ = tx.send(response_message).await;
                    }
                }
            }
        }
        "GET_STATUS" => {
            if let Some(request_id) = server_message.payload["request_id"].as_str() {
                let status_url = "http://10.43.0.1:8080/local/status";
                let response_message = match timeout(Duration::from_secs(5), reqwest::get(status_url)).await {
                    Ok(Ok(response)) => {
                        match timeout(Duration::from_secs(5), response.text()).await {
                            Ok(Ok(status_data)) => {
                                let payload = match serde_json::from_str::<serde_json::Value>(&status_data) {
                                    Ok(json_data) => json_data,
                                    Err(_) => serde_json::json!({ "data": status_data }),
                                };

                                ServerMessage {
                                    action: "STATUS_RESPONSE".to_string(),
                                    payload: serde_json::json!({
                                        "data": payload,
                                        "request_id": request_id
                                    }),
                                }
                            }
                            Ok(Err(e)) => {
                                ServerMessage {
                                    action: "STATUS_RESPONSE".to_string(),
                                    payload: serde_json::json!({ 
                                        "error": format!("Failed to read status data: {}", e) ,
                                        "request_id": request_id
                                    }),
                                }
                            }
                            Err(_) => {
                                ServerMessage {
                                    action: "STATUS_RESPONSE".to_string(),
                                    payload: serde_json::json!({ 
                                        "error": "Timeout while reading response",
                                        "request_id": request_id 
                                    }),
                                }
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        ServerMessage {
                            action: "STATUS_RESPONSE".to_string(),
                            payload: serde_json::json!({ 
                                "error": format!("Failed to get status data: {}", e),
                                "request_id": request_id
                            }),
                        }
                    }
                    Err(_) => {
                        ServerMessage {
                            action: "STATUS_RESPONSE".to_string(),
                            payload: serde_json::json!({
                                "error": "Request timed out after 5 seconds",
                                "request_id": request_id
                            }),
                        }
                    }
                };

                let _ = tx.send(response_message).await;
            }
        }
        "GET_CLIENT_INFO" => {
            if let Some(request_id) = server_message.payload["request_id"].as_str() {
                let hardcoded_response = serde_json::json!({
                    "name": "Laundryclient",
                    "version": "2.0.3",
                    "motd": "I'm going to touch you.",
                });

                let response_message = ServerMessage {
                    action: "CLIENT_INFO_RESPONSE".to_string(),
                    payload: serde_json::json!({
                        "data": hardcoded_response,
                        "request_id": request_id
                    }),
                };

                let _ = tx.send(response_message).await;
            }
        }
        _ => println!("Unknown action: {}", server_message.action),
    }
}

async fn find_machine_ip(machine_label: &str) -> Option<String> {
    let machines_url = "http://10.43.0.1:8080/machines";
    
    match reqwest::get(machines_url).await {
        Ok(response) => {
            match response.json::<Vec<Value>>().await {
                Ok(machines) => {
                    machines.iter()
                        .find(|m| m["license"].as_str() == Some(machine_label))
                        .and_then(|m| m["ip"].as_str())
                        .map(String::from)
                }
                Err(e) => {
                    //eprintln!("Failed to parse machines data: {}", e);
                    None
                }
            }
        }
        Err(e) => {
            //eprintln!("Failed to fetch machines data: {}", e);
            None
        }
    }
}
